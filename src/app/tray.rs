//! 系统托盘与后台通知：托盘怎么构建，以及隐藏期间累加的「待通知」计数。

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use windui::prelude::*;

use crate::assets;
use crate::lanshare::{Engine, NetEvent};

/// 后台运行期间累加的「待通知」计数。
///
/// 为什么不直接在 `dispatch` 里发气泡：本应用不是 resident 模式，窗口一 `SW_HIDE` 就
/// 不再有 `WM_PAINT`，而 windui 的 `drain_channels`（跑 `on_message`，即 `dispatch`）只挂在
/// render 帧路径上——隐藏时 channel 根本不排空，事件全积压到重新显示才处理。于是把
/// 「谁来了几条」的统计挪到**网络泵线程**（它不依赖 UI 帧，一直在跑）累加进这块共享状态，
/// UI 侧只在心跳 `on_interval`（绑 `WM_TIMER`，隐藏时照样投递）里排空、发气泡。
#[derive(Default)]
pub(super) struct PendingNotice {
    /// 收到的聊天消息条数（不含对端写入剪贴板的静默消息）。
    messages: usize,
    /// 收到的文件项数（确认模式的接收请求，或自动模式的传输开始）。
    files: usize,
    /// 最后一个来源设备名，用来把气泡正文写得像人话。
    last: String,
}

impl PendingNotice {
    pub(super) fn is_empty(&self) -> bool {
        self.messages == 0 && self.files == 0
    }

    /// 汇总成 `(标题, 正文)`；调用方随即清空本结构。
    pub(super) fn summarize(&self) -> (String, String) {
        let mut parts = Vec::new();
        if self.messages > 0 {
            parts.push(format!("{} 条消息", self.messages));
        }
        if self.files > 0 {
            parts.push(format!("{} 个文件", self.files));
        }
        let who = if self.last.is_empty() {
            String::new()
        } else {
            format!("来自 {}：", self.last)
        };
        ("LANShare · 收到新内容".to_string(), format!("{who}{}", parts.join("、")))
    }
}

/// 网络泵线程每收到一批引擎事件就调一次：把「值得通知的」累加进共享计数。
///
/// 只统计**入向**：`Message{incoming:true, clipboard:false}`（对端发来的聊天，剪贴板静默
/// 注入的不算）算消息；`Receive`（确认模式的来件请求）与 `Start{sending:false}`（自动模式
/// 的实际传输）算文件。这两种文件事件分属两种接收模式、彼此不会同时到达，故不会重复计数。
pub(super) fn accumulate_notice(events: &[NetEvent], pending: &Arc<Mutex<PendingNotice>>) {
    let mut notice = match pending.lock() {
        Ok(guard) => guard,
        // 锁被别处 panic 打脏也不该拖垮网络泵：本次统计丢弃即可。
        Err(_) => return,
    };
    for event in events {
        match event {
            NetEvent::Message { device, clipboard, incoming, .. } => {
                if *incoming && !*clipboard {
                    notice.messages += 1;
                    notice.last = device.name.clone();
                }
            }
            NetEvent::Receive(offer) => {
                notice.files += 1;
                notice.last = offer.device.name.clone();
            }
            NetEvent::Start(info) if !info.sending => {
                notice.files += 1;
                notice.last = info.device.name.clone();
            }
            _ => {}
        }
    }
}

/// 构建系统托盘：图标、悬停提示、单击切换显隐、双击唤出、右键菜单。
///
/// **只在启动那一刻判定要收进托盘时才调用**（判定见 `run`）：`App::tray` 与
/// `App::hide_on_close` 都是构建期声明，windui 运行期两样都改不动（`TrayOp` 没有增删图标
/// 的口子，`NIM_DELETE` 只随平台宿主 drop；`hide_on_close` 没有 setter），所以这一整轮的行
/// 为从头到尾是固定的，改设置要重启才生效。
///
/// `in_tray` 是应用**自维护**的「已收进托盘」标志——框架的 `window_state().visible` 在
/// `SW_HIDE` 后不刷新（读到的仍是陈旧的 `true`），不足以判「当前是否隐藏」，故显隐由这个
/// 标志说了算：关窗转隐藏 / 菜单隐藏时置位，`on_show` 里复位。左键据此在显、隐之间切换。
///
/// 「退出」闭包必须自己 `engine.shutdown()`：`ctx.quit()` 走 `TrayAction::Quit`→`quit_app()`
/// →`DestroyWindow`，**绕过** `on_close_request`（那是给标题栏关闭用的），不手动关引擎，
/// 网络线程与套接字就收不干净。
pub(super) fn build_tray(engine: Arc<Engine>, in_tray: Rc<Cell<bool>>) -> Tray {
    let mut tray = Tray::new()
        .tooltip("LANShare · 局域网文件传输")
        .on_double_click(|ctx| ctx.show_window());
    // 解不出图标就不设，退回系统默认应用图标，而不是崩在启动路径上。
    if let Some((w, h, rgba)) = assets::tray_icon() {
        tray = tray.icon_rgba(w, h, &rgba);
    }
    let left_tray = Rc::clone(&in_tray);
    tray = tray.on_left_click(move |ctx| {
        if left_tray.get() {
            ctx.show_window();
            // 复位交给 `on_show`：那里才是窗口真显出来的时刻，避免此处与框架各写一份。
        } else {
            left_tray.set(true);
            ctx.hide_window();
        }
    });
    let hide_flag = Rc::clone(&in_tray);
    tray.menu(vec![
        TrayMenuItem::item("显示主窗口", |ctx| ctx.show_window()),
        TrayMenuItem::item("隐藏到托盘", move |ctx| {
            hide_flag.set(true);
            ctx.hide_window();
        }),
        TrayMenuItem::separator(),
        TrayMenuItem::item("退出", move |ctx| {
            engine.shutdown();
            ctx.quit();
        }),
    ])
}
