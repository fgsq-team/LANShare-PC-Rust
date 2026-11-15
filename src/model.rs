//! 界面侧的数据模型：一台设备一个会话，气泡元素要么是文本，要么是文件。
//!
//! 这里只做展示层的换算，网络细节全在 `crate::lanshare` 里：
//! - 时间戳沿用引擎的 epoch 毫秒，展示时再换算成本地墙上时间；
//! - 文件气泡的字段对齐原版 `ContentElement`（名称、大小、路径、progress、completed、received），
//!   进度与取消都以协议里的 `fileId` 为键，这样事件到达时能准确找回那条气泡。

use std::path::PathBuf;

use crate::lanshare::db::{Entry, local_offset_ms};
use crate::lanshare::device::{Device, MODE_ANDROID, MODE_IOS, MODE_LINUX, MODE_MAC, MODE_WIN};
use crate::lanshare::walk::size_str;
use crate::lanshare::TransferInfo;

/// 本地时区相对 UTC 的偏移（秒），用来把时间戳格式化成墙上时间。
///
/// 不再硬编码东八区：哪个平台、哪个时区都能拿到本地偏移，具体算法见 `db::local_offset_ms`。
fn local_offset_seconds() -> i64 {
    local_offset_ms() / 1000
}

/// 图片扩展名，与原版 `Utils::isImage` 判断的那一组一致。
const IMAGE_SUFFIXES: [&str; 8] = ["png", "jpg", "jpeg", "gif", "bmp", "webp", "ico", "tif"];

/// 消息的发送方。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Side {
    /// 本机发出，气泡靠右。
    Mine,
    /// 对方发来，气泡靠左。
    Peer,
}

/// 一条传输的进展，文本消息恒为 `Finished`。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    /// 正在发送或接收。
    Ongoing,
    Finished,
    Failed,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ongoing => "传输中",
            Self::Finished => "已完成",
            Self::Failed => "失败",
        }
    }
}

/// 文件类气泡的元素，字段与原版 `ContentElement` 对齐。
#[derive(Clone, Debug)]
pub struct FileElement {
    /// 协议里的 `fileId`：进度、完成、取消事件都按它定位。
    pub id: String,
    pub name: String,
    /// 字节数。
    pub size: i64,
    /// 发送端是源文件路径，接收端是落盘路径。
    pub path: PathBuf,
    /// 0~100。
    pub progress: i32,
    pub completed: bool,
    /// true = 这是收到的文件。
    pub received: bool,
    pub is_dir: bool,
    /// 落盘文件在不在的缓存：`None` = 未查，`Some(false)` = 已被删。
    /// 渲染热路径只读它、绝不 stat；刷新只在切换会话/收尾/翻页时做一次。
    pub exists: Option<bool>,
}

impl FileElement {
    pub fn size_text(&self) -> String {
        size_str(self.size)
    }

    /// 菜单与提示里的一行状态。
    pub fn state_text(&self) -> String {
        if self.completed {
            format!("已完成 · {}", self.size_text())
        } else {
            format!("{}% · {}", self.progress, self.size_text())
        }
    }
}

/// 气泡内容：文本、文件或图片。
#[derive(Clone, Debug)]
pub enum Content {
    Text(String),
    File(FileElement),
    Image(FileElement),
}

impl Content {
    /// 文件类内容共用的字段。
    pub fn file(&self) -> Option<&FileElement> {
        match self {
            Self::Text(_) => None,
            Self::File(element) | Self::Image(element) => Some(element),
        }
    }

    /// 文件类内容的可变引用，进度与收尾事件按它定位。
    pub fn file_mut(&mut self) -> Option<&mut FileElement> {
        match self {
            Self::Text(_) => None,
            Self::File(element) | Self::Image(element) => Some(element),
        }
    }

    pub fn is_image(&self) -> bool {
        matches!(self, Self::Image(_))
    }
}

/// 一条聊天消息。
#[derive(Clone, Debug)]
pub struct Message {
    /// 界面内的自增序号，只用于 keyed 列表的稳定标识。
    pub id: u64,
    pub side: Side,
    pub content: Content,
    /// epoch 毫秒。
    pub at: i64,
    pub status: Status,
    /// 对方要求写进剪贴板（原版「复制到对方剪贴板」）。
    pub clipboard: bool,
}

impl Message {
    pub fn text(side: Side, text: impl Into<String>, at: i64, clipboard: bool) -> Self {
        Self {
            id: 0,
            side,
            content: Content::Text(text.into()),
            at,
            status: Status::Finished,
            clipboard,
        }
    }

    /// 由传输信息建气泡：发送与接收共用，方向取 `info.sending`。
    pub fn transfer(id: u64, info: &TransferInfo, at: i64) -> Self {
        let element = FileElement {
            id: info.id.clone(),
            name: info.name.clone(),
            size: info.length,
            path: PathBuf::new(),
            progress: 0,
            completed: false,
            received: !info.sending,
            is_dir: info.is_dir,
            exists: None,
        };
        let content = if !element.is_dir && looks_like_image(&element.name) {
            Content::Image(element)
        } else {
            Content::File(element)
        };
        Self { id, side: if info.sending { Side::Mine } else { Side::Peer }, content, at, status: Status::Ongoing, clipboard: false }
    }

    /// 历史库里的一行。
    pub fn from_entry(entry: &Entry) -> Self {
        let side = if entry.is_left { Side::Peer } else { Side::Mine };
        let content = if entry.is_file {
            let element = FileElement {
                id: entry.id.clone(),
                name: entry.message.clone(),
                size: entry.file_size,
                path: PathBuf::from(&entry.file_path),
                progress: entry.progress.clamp(0, 100) as i32,
                completed: entry.completed,
                received: entry.received,
                is_dir: false,
                exists: None,
            };
            if looks_like_image(&element.name) {
                Content::Image(element)
            } else {
                Content::File(element)
            }
        } else {
            Content::Text(entry.message.clone())
        };
        let status = if !entry.completed {
            Status::Failed
        } else if entry.is_file && entry.progress < 100 {
            Status::Ongoing
        } else {
            Status::Finished
        };
        Self { id: 0, side, content, at: entry.time_stamp, status, clipboard: false }
    }

    /// 会话列表里的一行摘要。
    pub fn preview(&self) -> String {
        match &self.content {
            Content::Text(text) => {
                let flat = text.replace('\n', " ");
                match (self.side, self.clipboard) {
                    (Side::Mine, false) => format!("我: {flat}"),
                    (Side::Mine, true) => format!("我（发到剪贴板）: {flat}"),
                    (Side::Peer, true) => format!("对方剪贴板: {flat}"),
                    (Side::Peer, false) => flat,
                }
            }
            Content::File(element) | Content::Image(element) => {
                let kind = if element.is_dir { "文件夹" } else if self.is_image() { "图片" } else { "文件" };
                let mark = match self.status {
                    Status::Ongoing => format!("{}%", element.progress),
                    Status::Finished => "完成".to_owned(),
                    Status::Failed => "失败".to_owned(),
                };
                format!("[{kind}] {} · {} · {}", element.name, element.size_text(), mark)
            }
        }
    }

    pub fn is_image(&self) -> bool {
        self.content.is_image()
    }

    /// 气泡上的文件字段（文本消息返回 `None`）。
    pub fn file(&self) -> Option<&FileElement> {
        self.content.file()
    }

    pub fn file_mut(&mut self) -> Option<&mut FileElement> {
        self.content.file_mut()
    }

    /// 气泡上的时刻。
    pub fn clock(&self) -> String {
        clock(self.at)
    }
}

/// 合并（全部设备）流里的一行：跨设备的全局分页不依附任何单个会话窗口，
/// 而 `Message` 本身不带设备展示信息，故每行额外挂上显示用的设备名与类型。
#[derive(Clone, Debug)]
pub struct MergedItem {
    pub message: Message,
    /// 展示用的设备名（气泡上方的归属标签）。
    pub name: String,
    /// 展示用的设备类型（`Device::mode`）。
    pub mode: i32,
    /// 这一行属于哪台设备（`Device::uuid`，旧库里无主的行是空串）。
    ///
    /// 光有 `name` 不够：会话侧「清空消息」要把这台设备在合并流里的副本一并撤掉，
    /// 而设备名可以重名、可以变，只有 `uuid` 与历史库的归属键对得上。
    pub device_id: String,
}

/// 一个设备会话。`key` 用 `ip:port`，与引擎的设备表一致。
#[derive(Clone, Debug)]
pub struct Conversation {
    pub id: u64,
    pub device: Device,
    pub unread: usize,
    pub messages: Vec<Message>,
    /// 历史库里是否还有更早的消息没拉进窗口（驱动顶部探针与分页行）。
    pub has_more_history: bool,
    /// 已加载窗口里最旧一条的 `timeStamp`（0 = 未初始化）；`older` 按它往前取下一页。
    pub history_cursor: i64,
}

impl Conversation {
    pub fn new(id: u64, device: Device) -> Self {
        Self { id, device, unread: 0, messages: Vec::new(), has_more_history: false, history_cursor: 0 }
    }

    /// 会话标识：`ip:port`。
    pub fn key(&self) -> String {
        self.device.key()
    }

    pub fn name(&self) -> &str {
        if self.device.name.is_empty() { &self.device.ip } else { &self.device.name }
    }

    /// 侧栏副标题：地址加协议版本。
    pub fn subtitle(&self) -> String {
        format!("{}:{} · V{}", self.device.ip, self.device.port, self.device.data_version.max(1))
    }

    /// 电量百分比文本（不带充电符号）；台式机（拿不到电池）显示“电源”。
    /// 充电时由视图层在副标题前另画一枚 MD 电量矢量图标。
    pub fn battery_text(&self) -> String {
        if self.device.battery < 0 {
            return "电源".to_owned();
        }
        format!("{}%", self.device.battery)
    }

    /// 设备是否正在充电（`charge > 0`）。
    pub fn charging(&self) -> bool {
        self.device.charge > 0
    }

    pub fn preview(&self) -> String {
        self.messages.last().map(Message::preview).unwrap_or_else(|| "还没有消息".to_owned())
    }

    pub fn last_at(&self) -> Option<i64> {
        self.messages.last().map(|message| message.at)
    }

    /// 刷新有界窗口内“已完成的接收文件”的落盘存在性。
    ///
    /// 只在切换会话 / 翻页 / 收尾时低频调用：把 `path.exists()` 这种系统调用
    /// 从每帧渲染热路径里拿掉，换回“同步不卡”。发送方自己发的文件不查
    /// （原版也只在接收侧报“文件已被删除”）。
    pub fn refresh_existence(&mut self) {
        for message in &mut self.messages {
            if let Some(element) = message.file_mut() {
                if element.completed && element.received && !element.path.as_os_str().is_empty() {
                    element.exists = Some(element.path.exists());
                }
            }
        }
    }
}

/// 设备类型的中文名。设备类型在头像上由 `assets::device_icon` 选的位图表达，
/// 这里只负责文案。
pub fn mode_label(mode: i32) -> &'static str {
    match mode {
        MODE_ANDROID => "安卓手机",
        MODE_IOS => "iPhone",
        MODE_WIN => "Windows 电脑",
        MODE_LINUX => "Linux 电脑",
        MODE_MAC => "macOS 电脑",
        _ => "未知设备",
    }
}

fn looks_like_image(name: &str) -> bool {
    match name.rfind('.') {
        Some(dot) => IMAGE_SUFFIXES.contains(&name[dot + 1..].to_ascii_lowercase().as_str()),
        None => false,
    }
}

/// 当前时间的 epoch 毫秒，与引擎的 `now_ms` 同一量纲。
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

/// 把时间戳拆成 (年, 月, 日, 时, 分)，Howard Hinnant 的 civil-from-days 算法。
///
/// 偏移由外部给定：单测靠它保证结果不跟着跑测试的机器时区跑。
fn break_down_at(at: i64, offset: i64) -> (i64, i64, i64, i64, i64) {
    let local = at / 1000 + offset;
    let days = local.div_euclid(86_400);
    let seconds = local.rem_euclid(86_400);
    let (hour, minute) = (seconds / 3600, seconds % 3600 / 60);

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 { month_prime + 3 } else { month_prime - 9 };
    let year = if month <= 2 { year + 1 } else { year };

    (year, month, day, hour, minute)
}

/// 气泡上的时刻，例如 `14:23`。
pub fn clock(at: i64) -> String {
    clock_at(at, local_offset_seconds())
}

fn clock_at(at: i64, offset: i64) -> String {
    let (_, _, _, hour, minute) = break_down_at(at, offset);
    format!("{hour:02}:{minute:02}")
}

/// 日期分组键，同一天的消息共用一个分隔条。
pub fn day_key(at: i64) -> i64 {
    day_key_at(at, local_offset_seconds())
}

fn day_key_at(at: i64, offset: i64) -> i64 {
    (at / 1000 + offset).div_euclid(86_400)
}

/// 日期分隔条文字：今天 / 昨天 / MM月DD日 / YYYY年MM月DD日。
pub fn day_label(at: i64) -> String {
    day_label_at(at, local_offset_seconds())
}

fn day_label_at(at: i64, offset: i64) -> String {
    let today = day_key_at(now_ms(), offset);
    let key = day_key_at(at, offset);
    let (year, month, day, _, _) = break_down_at(at, offset);
    match today - key {
        0 => "今天".to_owned(),
        1 => "昨天".to_owned(),
        _ if year == break_down_at(now_ms(), offset).0 => format!("{month:02}月{day:02}日"),
        _ => format!("{year}年{month:02}月{day:02}日"),
    }
}

/// 会话列表右侧的时间：当天只显示时刻，昨天显示「昨天」，其余显示日期。
pub fn list_time(at: i64) -> String {
    let offset = local_offset_seconds();
    let delta = day_key_at(now_ms(), offset) - day_key_at(at, offset);
    if delta == 0 {
        clock_at(at, offset)
    } else if delta == 1 {
        "昨天".to_owned()
    } else {
        let (_, month, day, _, _) = break_down_at(at, offset);
        format!("{month:02}/{day:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(sending: bool, name: &str, length: i64, is_dir: bool) -> TransferInfo {
        TransferInfo {
            id: "f1".to_string(),
            device: Device { name: "手机".into(), ip: "10.0.0.9".into(), port: 5856, ..Default::default() },
            name: name.to_string(),
            length,
            is_dir,
            sending,
        }
    }

    #[test]
    fn a_transfer_message_keeps_the_protocol_id() {
        let mine = Message::transfer(1, &info(true, "a.png", 12, false), 1_700_000_000_000);
        assert_eq!(mine.side, Side::Mine);
        assert_eq!(mine.content.file().expect("文件气泡").id, "f1");
        assert!(mine.is_image(), "按扩展名认出图片");
        assert_eq!(mine.status, Status::Ongoing);
        let theirs = Message::transfer(2, &info(false, "相册", 12, true), 1_700_000_000_000);
        assert_eq!(theirs.side, Side::Peer);
        assert!(theirs.content.file().expect("文件夹气泡").received);
        assert!(!theirs.is_image(), "文件夹永远不是图片");
    }

    #[test]
    fn previews_show_size_and_progress() {
        let mut message = Message::transfer(1, &info(true, "报告.docx", 2048, false), 0);
        assert!(!message.preview().contains("50%"));
        if let Content::File(element) = &mut message.content {
            element.progress = 50;
        }
        assert_eq!(message.preview(), "[文件] 报告.docx · 2.0KB · 50%");
        let folder = Message::transfer(2, &info(true, "相册", 0, true), 0);
        assert!(folder.preview().starts_with("[文件夹] 相册"));
        let text = Message::text(Side::Mine, "第一行\n第二行", 0, false);
        assert_eq!(text.preview(), "我: 第一行 第二行");
    }

    #[test]
    fn times_are_grouped_by_local_day() {
        // 固定成东八区偏移，免得结果跟着跑测试的机器时区跑
        let offset = 8 * 3600;
        // 2024-01-01 00:00:00（东八区）
        let morning = 1_704_038_400_000;
        assert_eq!(clock_at(morning, offset), "00:00");
        assert_eq!(break_down_at(morning, offset), (2024, 1, 1, 0, 0));
        assert_eq!(day_key_at(morning, offset), day_key_at(morning + 3_600_000, offset));
        assert_ne!(day_key_at(morning, offset), day_key_at(morning + 90_000_000, offset));
        // 不是今天也不是昨天，就带上完整年份
        assert!(day_label_at(morning, offset).starts_with("2024年"), "{}", day_label_at(morning, offset));
    }

    #[test]
    fn day_grouping_follows_the_given_offset() {
        // 同一个时间戳，UTC 与东八区应当分到不同的天，证明墙上时间真的跟着偏移走
        let stamp = 1_704_038_400_000;
        assert_eq!(clock_at(stamp, 0), "16:00");
        // UTC 下这一刻是 12-31 16:00：再过 7 小时还在同一天，过 8 小时就跨天了
        assert_eq!(day_key_at(stamp, 0), day_key_at(stamp + 7 * 3_600_000, 0));
        assert_ne!(day_key_at(stamp, 0), day_key_at(stamp + 8 * 3_600_000, 0));
        assert_ne!(day_key_at(stamp, 0), day_key_at(stamp, 8 * 3600));
        // 不传偏移的对外版本必须用本机器时区，不能直接复上 8 小时
        assert_eq!(day_key(stamp), day_key_at(stamp, local_offset_seconds()));
    }

    #[test]
    fn conversation_labels_carry_address_and_battery() {
        let device = Device {
            name: "小米 14".into(),
            ip: "192.168.1.9".into(),
            port: 5856,
            mode: MODE_ANDROID,
            battery: 82,
            charge: 1,
            data_version: 5,
            ..Default::default()
        };
        let conversation = Conversation::new(1, device);
        assert_eq!(conversation.key(), "192.168.1.9:5856");
        assert_eq!(conversation.subtitle(), "192.168.1.9:5856 · V5");
        assert_eq!(conversation.battery_text(), "82%");
        assert!(conversation.charging(), "charge > 0 即充电中");
        assert_eq!(mode_label(MODE_ANDROID), "安卓手机");
        // 台式机读不到电池，只显示电源
        let desktop = Conversation::new(2, Device { name: String::new(), ip: "10.0.0.1".into(), ..Default::default() });
        assert_eq!(desktop.battery_text(), "电源");
        assert_eq!(desktop.name(), "10.0.0.1", "没名字就用 IP");
    }
}
