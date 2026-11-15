//! 「在文件管理器里定位某个文件」与「交给系统默认程序打开」的跨平台外壳调用。
//!
//! 选择文件/文件夹不再需要这里：windui 自带 `EventCtx::request_pick_files` /
//! `request_pick_folder`，由平台层（Windows 走 COM，macOS/Linux 走各自原生对话框）实现。
//! 这里只剩两件「起外部进程」的事，而且都拿不到「成功」这种可靠回执，一律 best-effort。

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 在文件管理器里打开并选中该文件，失败时静默忽略（文件可能已被移走）。
pub fn reveal(path: &PathBuf) {
    let _ = spawn_reveal(path);
}

/// 交给系统默认程序打开这个文件（预览器关掉时的退路，对应原版的 `QDesktopServices::openUrl`）。
pub fn open(path: &PathBuf) {
    let _ = spawn_open(path);
}

/// 真正拼命令的地方，返回平台层的错误，方便单元测试与日志。
///
/// Windows 的 `explorer` 即使路径不存在也会返回 0，所以只能信它「起来了」；
/// macOS 用 `open -R`，Linux 没有通用的「选中」语义，退化成打开所在目录。
fn spawn_reveal(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        // explorer 两种情况都会静默落进「文档」：路径带正斜杠（默认保存目录 C:/LANShare/ 就带），
        // 以及含空格时整个 "/select,路径" 被 Rust 当成一个参数整体引起来。与 C++ 版一致：
        // 分隔符归一化成反斜杠，/select, 与路径分两个参数传（引号只包路径）。
        let path = path.to_string_lossy().replace('/', "\\");
        Command::new("explorer")
            .arg("/select,")
            .arg(path)
            .spawn()
            .map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg("-R").arg(path).spawn().map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = path.parent().unwrap_or(path).to_path_buf();
        Command::new("xdg-open").arg(dir).spawn().map(|_| ())
    }
    #[cfg(not(any(windows, target_os = "macos", unix)))]
    {
        let _ = path;
        Err(io::Error::new(io::ErrorKind::Unsupported, "该平台没有可用的文件管理器"))
    }
}

/// 真正拼「打开」命令的地方。Windows 用 `cmd /c start`（空标题串不能省，否则第一个
/// 参数会被当成窗口标题），macOS 用 `open`，Linux 用 `xdg-open`。
fn spawn_open(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        Command::new("cmd")
            .args(["/c", "start", ""])
            .arg(path)
            .spawn()
            .map(|_| ())
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(path).spawn().map(|_| ())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open").arg(path).spawn().map(|_| ())
    }
    #[cfg(not(any(windows, target_os = "macos", unix)))]
    {
        let _ = path;
        Err(io::Error::new(io::ErrorKind::Unsupported, "该平台没有可用的默认程序入口"))
    }
}
