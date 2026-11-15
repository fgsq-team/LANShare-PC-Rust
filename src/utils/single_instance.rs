//! 单实例守卫：对应 C++ 版 `main.cpp` 用 `QLocalServer`（名字 `lanshare_server`）占坑的效果——
//! 第二次启动不再起一套新界面，而是把已在跑的那个实例的窗口唤到前台，本进程随即退出。
//!
//! Windows 走命名互斥体（`CreateMutexW`）：句柄一旦拿到就**永不 `CloseHandle`**，进程退出时
//! 由系统自动回收，因此不留残留、也不需要清理。第二次调用拿到 `ERROR_ALREADY_EXISTS` 就说明
//! 已有实例在跑，于是 best-effort 枚举顶层窗口、找到标题以「LANShare」开头的那台唤起来。
//!
//! 其余平台（macOS/Linux）没有互斥体这条 API，改用数据目录下一把 `try_lock` 的文件锁：锁得到
//! 就是第一个实例，锁被别进程占着就是已有实例（此时只退出、不做跨进程唤起——没有可靠且零依赖
//! 的通路）。文件句柄同样**故意不关**，让内核记录的文件锁活到进程结束。
//!
//! 拿不到锁 / 调 API 失败的极端情况一律当「第一个实例」放行：守卫是为了挡住重复启动，
//! 不该因为自己出错反倒把程序关在门外。

/// 守卫的判定结果。
pub enum Outcome {
    /// 本进程是第一个实例：继续正常启动。OS 层的占坑资源（互斥体句柄 / 锁文件）已在
    /// [`acquire`] 内部故意泄漏，活到进程结束，调用方无需持有或释放任何东西。
    First,
    /// 已有实例在跑：本进程应立刻退出（Windows 上 [`acquire`] 已尽力唤起已有窗口）。
    AlreadyRunning,
}

/// 尝试占坑。见 [`Outcome`]。只能在 `main` 线程、建窗之前调一次。
pub fn acquire() -> Outcome {
    #[cfg(windows)]
    {
        win_acquire()
    }
    #[cfg(not(windows))]
    {
        file_acquire()
    }
}

/// 跨平台兜底：拿数据目录（`Config::dir()`）下的 `single_instance.lock` 做一把排他文件锁。
/// 目录还没建时先建；任何 IO 异常都当「第一个实例」放行，不拦住正常启动。
#[cfg(not(windows))]
fn file_acquire() -> Outcome {
    use std::fs::OpenOptions;

    let path = crate::lanshare::config::Config::dir().join("single_instance.lock");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(file) = OpenOptions::new().create(true).read(true).write(true).truncate(false).open(&path) else {
        return Outcome::First;
    };
    // `try_lock` 独占：拿不到 = 别的实例正持有这把锁。成功就把句柄故意留着（`mem::forget`），
    // 一旦 drop/close，内核记录锁就没了，第二个实例反而能进来。
    if file.try_lock().is_ok() {
        std::mem::forget(file);
        Outcome::First
    } else {
        Outcome::AlreadyRunning
    }
}

/// Windows：命名互斥体占坑 + 命中已有实例时唤起它的窗口。
#[cfg(windows)]
fn win_acquire() -> Outcome {
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;

    // 名字带够辨识度，避免撞上别的程序；`Local\` 命名空间 = 同一登录会话内互斥。
    let name: PCWSTR = w!("Local\\LANShare-RS.SingleInstance.v1");
    let handle = unsafe { CreateMutexW(None, false, name) };
    let Ok(handle) = handle else {
        return Outcome::First;
    };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        // 已有实例：这个句柄对应的是同一个内核对象，关掉它无妨（真正那份在对方进程里）。
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(handle);
        }
        activate_existing_window();
        Outcome::AlreadyRunning
    } else {
        // 第一个实例：句柄不关即活到进程结束（见文件头注释）。`HANDLE` 无 `Drop`，函数返回
        // 后它就搁在那里——不会被回收，也就不需要 `mem::forget`（那对 `Copy` 类型是空操作）。
        Outcome::First
    }
}

/// 找到已在跑的那个实例的主窗口并唤到前台。best-effort：找不到就算了。
///
/// 此刻本进程还没建任何窗口（守卫在 `App::new` 之前跑），所以枚举到的「LANShare…」窗口
/// 必属对方实例。命中后：最小化态先 `SW_RESTORE` 还原，其余 `SW_SHOW`（能掀开被隐到托盘
/// 的窗口），再 `SetForegroundWindow` 置前。
#[cfg(windows)]
fn activate_existing_window() {
    use windows::Win32::Foundation::LPARAM;
    use windows::Win32::UI::WindowsAndMessaging::EnumWindows;

    FOUND.with(|slot| slot.set(None));
    unsafe {
        // `EnumWindows` 在**本线程**同步回调 `proc`，故上面的 thread_local 就是这条线程的，
        // `proc` 里写入的 HWND 紧接着在这里读得到。
        let _ = EnumWindows(Some(enumerate), LPARAM(0));
    }
    let Some(hwnd) = FOUND.with(|slot| slot.get()) else {
        return;
    };
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{
            IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW,
        };
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        } else {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

/// `EnumWindows` 的回调：命中第一个标题以「LANShare」开头的顶层窗口就记下并停枚举。
#[cfg(windows)]
unsafe extern "system" fn enumerate(hwnd: windows::Win32::Foundation::HWND, _lparam: windows::Win32::Foundation::LPARAM) -> windows::core::BOOL {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextLengthW, GetWindowTextW};

    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return CONTINUE;
    }
    // 多留一格给终止符：`GetWindowTextW` 按 u16 元素数写入并补 `\0`。
    let mut buffer = vec![0u16; len as usize + 1];
    let written = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    if written <= 0 {
        return CONTINUE;
    }
    let title = String::from_utf16_lossy(&buffer[..written as usize]);
    if title.starts_with("LANShare") {
        FOUND.with(|slot| slot.set(Some(hwnd)));
        return STOP;
    }
    CONTINUE
}

// 枚举回调用的线程局部结果位。`EnumWindows` 与读它的 `activate_existing_window` 同线程。
#[cfg(windows)]
thread_local! {
    static FOUND: std::cell::Cell<Option<windows::Win32::Foundation::HWND>> = const { std::cell::Cell::new(None) };
}

/// `BOOL`：继续枚举。
#[cfg(windows)]
const CONTINUE: windows::core::BOOL = windows::core::BOOL(1);
/// `BOOL`：停止枚举。
#[cfg(windows)]
const STOP: windows::core::BOOL = windows::core::BOOL(0);
