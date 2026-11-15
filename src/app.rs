//! 应用根状态：界面状态、消息处理，以及与 `lanshare::Engine` 之间的「泵」桥接。
//!
//! 引擎跑在自己的线程上，只能通过事件队列汇报。windui 的 `App::channel` 发送端是
//! `Send` 的，闭包却不需要 `Send`，所以泵线程把事件打包成消息发回 UI 线程，
//! 分发器则可以直接捕获 `Rc<RefCell<ChatApp>>`。出站动作（发消息、发文件、取消、
//! 接受/拒绝）反过来直接调 `Arc<Engine>`，不绕队列。
//!
//! 状态的「真相」分两处：会随界面走的数据（搜索词、草稿、表单）活在 `Signal` 里，
//! 业务数据（会话、气泡、设备）活在 `ChatApp` 里。分发器负责每次消息处理后把两边对齐。
//!
//! 本模块按功能摊开成若干子模块，`impl ChatApp` 与 `impl Pump` 各按主题分片：
//!
//! - [`msg`]：分发给应用的全部消息 `Msg`。
//! - [`prefs`]：`ChatMode`（合并 / 分设备）与 `ThemePref`（主题偏好）。
//! - [`state`]：`ChatApp` 的构造与只读访问器。
//! - [`feed`]：会话装载、历史分页、气泡追加，以及引擎事件落到状态机。
//! - [`actions`]：出站动作——发文本、发文件、重发、取消、切会话。
//! - [`panels`]：接收确认、网页授权、二维码、自由复制、选设备发送这些对话框的状态。
//! - [`settings`]：配置落库与那些「控件直接写信号」的 setter。
//! - [`update`]：把后台查回的版本信息判定成一条提示，与「不更新」时的记录落库。
//! - [`messages`]：气泡的本地操作——删除、标已读、清空会话。
//! - [`pump`]：`Pump` 的字段、同步门控 `Gate`，以及要拿 `EventCtx` 的辅助动作。
//! - [`dispatch`]：`Msg` 的完整分发。
//! - [`tray`]：系统托盘构建，与隐藏期间累加的「待通知」计数。
//! - [`run`]：启动装配——建窗、装托盘、拉起网络泵与缩略图泵、挂事件回调。
//!
//! 子模块只放 `impl` 块与各自的私有项，**`ChatApp` 的字段全部留在本文件**：Rust 的私有项
//! 对「定义它的模块及其所有后代」可见，字段锚在模块根，各分片就都能直接读写，不必把
//! 字段放开到 `pub`。跨分片用到的方法统一标 `pub(super)`，即只在 `app` 这棵树里可见。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::lanshare::config::Config;
use crate::lanshare::device::Device;
use crate::lanshare::{Engine, ReceiveOffer};
use crate::model::{self, Conversation};

mod actions;
mod dispatch;
mod feed;
mod messages;
mod msg;
mod panels;
mod prefs;
mod pump;
mod run;
mod settings;
mod state;
mod tray;
mod update;

pub use msg::Msg;
pub use prefs::{ChatMode, ThemePref};
pub use run::run;

/// UI 心跳：搜索框过滤与跨天后的日期分隔条都靠它兜底，空闲时一次比较就返回。
const TICK: Duration = Duration::from_millis(200);
/// 首次打开会话时从历史库拉多少条（只拉最近这一页，不再一次灌满窗口）。
const PAGE_FIRST: i64 = 10;
/// 顶部探针触发的「往上翻一页」每次拉多少条。
const PAGE_STEP: i64 = 10;

/// 输入框可用的表情，点击后插入草稿。
pub const EMOJIS: [char; 12] = ['😀', '😂', '😅', '😊', '😍', '🤔', '😎', '🙏', '👍', '🎉', '☕', '🌙'];

/// 根状态。不认识 windui，只认识数据——界面要的一切都由 `views::state::Ui` 从这里换算。
pub struct ChatApp {
    engine: Arc<Engine>,
    /// 一台设备一个会话。
    conversations: Vec<Conversation>,
    /// 设备键（`ip:port`）-> 会话 id。
    index: HashMap<String, u64>,
    /// 在线设备快照，来自引擎的 `Devices` 事件。
    devices: Vec<Device>,
    /// 本机网卡快照，顶部横幅要用。
    local: Vec<Device>,
    /// 等待用户确认的接收请求。
    offers: Vec<ReceiveOffer>,
    /// 等待用户授权的网页客户端：`(令牌, 来源 IP)`。同一个浏览器会话每 2 秒问一次
    /// `/checkPass`，但授权请求只在 `/initConfig` 时发一条，这里按令牌去重。
    grants: Vec<(String, String)>,
    /// `fileId` -> 源文件路径，供「重新发送」找回原件。
    sources: HashMap<String, PathBuf>,
    selected: Option<u64>,
    /// 每个会话各自的草稿。当前会话的那份同时活在 `Ui::draft` 里。
    drafts: HashMap<u64, String>,
    /// 搜索框内容的副本，`Ui::query` 才是真值源，分发器负责抄过来。
    query: String,
    /// 下一条文本消息是否发到对方剪贴板。
    clipboard: bool,
    /// 聊天列表模式：合并（群发）或按设备。
    mode: ChatMode,
    /// 合并（全部设备）模式的独立全局流：跨设备有界分页，不依附任何单会话窗口。
    merged: Vec<model::MergedItem>,
    /// 全局流首屏是否已从库拉过（只拉一次，之后靠实时追加）。
    merged_loaded: bool,
    /// 全局流库里是否还有更早的一页（驱动顶部探针）。
    merged_has_more: bool,
    /// 全局流翻页游标：当前已加载最旧一条的 `at` 时间戳，`history_before_all` 按它往前取。
    merged_cursor: i64,
    /// 设置对话框正在编辑的那份配置。
    settings: Config,
    /// 「网页二维码」对话框是否开着。
    qr_open: bool,
    /// 对话框里按**文字**记住的那块网卡。网卡增减后下标会错位，原版 `WebUrlQRDialog::refresh()`
    /// 也是拿 `currentText()` 去新列表里找位置，这里照做。
    qr_pick: Option<String>,
    /// 后台查回的公网 IPv6，只在二维码下拉里多列一项展示；`None` = 未查到或不支持。
    qr_v6: Option<String>,
    /// 「自由复制文本」对话框要显示的正文；`None` = 对话框关着。
    free_copy: Option<String>,
    /// 「选择设备发送」对话框压着的那批待发路径；`None` = 对话框关着。
    /// 只在合并（全部设备）模式下用：选了文件先问发给谁，不当场群发。
    pick_paths: Option<Vec<PathBuf>>,
    /// 挂在屏幕上的更新提示；`None` = 不弹。由 [`update`] 把后台查回的版本信息判定出来。
    update: Option<crate::utils::update::Prompt>,
    notice: Option<String>,
    theme_pref: ThemePref,
    /// 系统报告的深浅色，供「跟随系统」解析。
    system_dark: bool,
    /// 状态改动过几次。分发器拿它当「要不要重新投影」的依据。
    revision: u64,
    /// 消息区该不该把视口滚到底。
    reveal: u64,
    next_conv: u64,
    next_msg: u64,
}

/// IPv4 的排序键：按数值升序，避免字典序把 `10.0.0.9` 排到 `10.0.0.10` 后面。
/// 解不出（非 IPv4 / 空串）给 `u32::MAX` 排到末尾，同值的靠端口与会话插入序（`sort_by` 稳定）兜底。
fn ip_sort_key(ip: &str) -> u32 {
    ip.parse::<std::net::Ipv4Addr>().map(u32::from).unwrap_or(u32::MAX)
}

/// 端口文本 -> `i32`。
fn parse_port(text: &str, label: &str) -> Result<i32, String> {
    let value = text.trim().parse::<i32>().map_err(|_| format!("{label}必须是数字"))?;
    if (1..65_536).contains(&value) {
        Ok(value)
    } else {
        Err(format!("{label}要在 1~65535 之间"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_sort_key_orders_by_value_not_lexically() {
        // 字典序会把 `.10` 排到 `.9` 前面（字符 '1' < '9'）；数值键必须让 `.9` 更靠前排。
        assert!(ip_sort_key("10.0.0.9") < ip_sort_key("10.0.0.10"));
        assert!(ip_sort_key("10.0.0.255") < ip_sort_key("192.168.1.1"));
        // 四段都参与比较：第三段大一位就靠后，哪怕末段更小。
        assert!(ip_sort_key("10.0.0.255") < ip_sort_key("10.0.1.0"));
    }

    #[test]
    fn ip_sort_key_puts_unparsable_addresses_last() {
        assert_eq!(ip_sort_key(""), u32::MAX);
        assert_eq!(ip_sort_key("lanshare.local"), u32::MAX);
        // v6 不在本版排序里，同样归到末尾。
        assert_eq!(ip_sort_key("::1"), u32::MAX);
        assert!(ip_sort_key("255.255.255.254") < u32::MAX);
    }
}
