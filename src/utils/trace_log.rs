//! 轻量运行期诊断：把关键路径的调用序列追加写进数据目录下的日志文件。
//!
//! 为什么走文件而不是 `eprintln!`：release 在 Windows 是 `windows_subsystem = "windows"`，
//! 没有控制台，`println!` 全丢——正是用户报「卡死且没日志」时最需要的东西。这里用
//! `Config::dir()`（与 `message_v2.db` 同目录）落一个 `lanshare_trace.log`。
//!
//! 默认**关闭**、零开销：只有设了环境变量 `LANSHARE_TRACE`（任意非空值）才初始化并写盘。
//! 没开启时 [`trace`] 第一次判断就短路返回。同时 [`install_panic_hook`] 会把 panic 的
//! message + backtrace 也写进同一个文件——即便界面在 release 下无声崩溃也能留下现场。

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// 是否开启 trace（进程启动时读一次环境变量，之后只读这个位）。
static ENABLED: AtomicBool = AtomicBool::new(false);
/// 单调递增的序号行，便于在多线程交错日志里对齐先后。
static SEQ: AtomicU64 = AtomicU64::new(0);
/// 进程启动时刻，所有时间戳相对它，读数小、好看。
static START: Mutex<Option<Instant>> = Mutex::new(None);
/// 共享的日志文件句柄；未开启时为 `None`。
static LOG: Mutex<Option<std::fs::File>> = Mutex::new(None);

/// 日志文件路径：与历史库同目录。
pub fn path() -> std::path::PathBuf {
    crate::lanshare::config::Config::dir().join("lanshare_trace.log")
}

/// 若设了 `LANSHARE_TRACE` 就打开日志文件并记下开启位。重复调用无副作用。
pub fn init() {
    if ENABLED.load(Ordering::Relaxed) {
        return;
    }
    if std::env::var_os("LANSHARE_TRACE").filter(|v| !v.is_empty()).is_none() {
        return;
    }
    let path = path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let opened = std::fs::OpenOptions::new().create(true).append(true).open(&path);
    match opened {
        Ok(mut file) => {
            let stamp = epoch_ms();
            let _ = writeln!(file, "==== trace on {stamp} path={} ====", path.display());
            let _ = file.flush();
            *START.lock().expect("trace 起点锁未被污染") = Some(Instant::now());
            *LOG.lock().expect("trace 日志锁未被污染") = Some(file);
            ENABLED.store(true, Ordering::Relaxed);
        }
        Err(error) => eprintln!("LANSHARE_TRACE 已设但日志打开失败：{error}"),
    }
}

/// 写一行 trace。未开启时短路返回（几乎零开销）。
pub fn trace(args: &str) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let elapsed = START
        .lock()
        .expect("trace 起点锁未被污染")
        .as_ref()
        .map(|start| start.elapsed().as_millis())
        .unwrap_or(0);
    let thread = thread_tag();
    let mut guard = LOG.lock().expect("trace 日志锁未被污染");
    if let Some(file) = guard.as_mut() {
        let _ = writeln!(file, "[{elapsed:>7}ms #{n:>6} t{thread}] {args}");
        let _ = file.flush();
    }
}

/// 安装 panic 钩子：把 panic message 与 backtrace 写进日志。
///
/// 只在 [`init`] 真正开启后才装，避免关掉 trace 时干扰正常 panic 输出。
pub fn install_panic_hook() {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    std::panic::set_hook(Box::new(|info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        trace(&format!("PANIC: {info}\nBACKTRACE:\n{backtrace}"));
    }));
}

fn thread_tag() -> String {
    // `ThreadId` 的 Debug 形如 `ThreadId(3)`，去掉包装只留数字，日志更窄。
    let text = format!("{:?}", std::thread::current().id());
    text.trim_start_matches("ThreadId(").trim_end_matches(')').to_string()
}

fn epoch_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}
