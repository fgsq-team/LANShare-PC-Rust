//! windui 侧的视图模型与信号集。
//!
//! `ChatApp` 是不认识界面框架的纯状态机，`Ui` 是它的「投影」：一组建好就交给控件的信号。
//! 两者之间只有一条通道 —— [`Ui::sync`]，它把状态机里的数据换算成一份份**纯数据行**
//! （`ConvRow` / `ThreadRow`）写进信号。
//!
//! 为什么要 `sync` 而不是「控件直接读 ChatApp」：
//! - windui 的键控列表只在**数据本身**变化时重建那一行，行外读到的状态不会跟着刷；
//!   所以选中态、在线态、时间、预览这些都必须进数据。
//! - `Signal::set` 不做相等判断，每次写入都自增版本并重绘；因此写入前一律先比对，
//!   否则每 200ms 的心跳都会把整侧栏重建成新节点。

use std::collections::HashSet;
use std::path::PathBuf;

use windui::prelude::*;

use crate::app::{ChatApp, ChatMode, ThemePref};
use crate::lanshare::config::Config;
use crate::lanshare::nets::local_mode;
use crate::lanshare::ReceiveOffer;
use crate::model::{self, Conversation, Side, Status};
use crate::theme;

/// 没选中设备时标题栏下的说明。
pub const HEAD_UNSELECTED: &str =
    "先在左边选一台设备。看不到设备就确认下是否同一网段，以及对方是否开着 UDP 4573。";

/// 菜单项的动作类型。行数据里只放这个轻量枚举：标签与图标由它决定，真正发的消息由 `views`
/// 按行自身的字段现编——这样行数据保持 `PartialEq`（键控列表要求），也不把带网络类型的
/// `Msg` 塞进 UI 模型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAction {
    /// 取消进行中的传输。
    Cancel,
    /// 重新发送文件消息（原版「发送」：未传完的走 V5 续传，传完的全量重发）。
    Resend,
    /// 交给系统默认程序打开文件（原版「打开」）。
    Open,
    /// 在文件管理器里打开所在位置（原版「在文件夹打开」）。
    OpenPath,
    /// 把气泡的展示文本写进剪贴板（原版「复制文本」）。
    CopyText,
    /// 弹出可选中文本的对话框（原版「自由复制文本」）。
    FreeCopyText,
    /// 删除这条本地记录。
    Delete,
    /// 标为已读。
    MarkRead,
    /// 全部标为已读。
    MarkAll,
    /// 清空该会话的气泡。
    ClearChat,
    /// 打开会话（未读为 0 时的第一项）。
    OpenConversation,
    /// 重新枚举本机网卡。
    Refresh,
    /// 打开设置。
    Settings,
    /// 打开「关于」（原业主菜单栏那个「关于」菜单里的唯一一项）。
    About,
}

impl MenuAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Cancel => "取消传输",
            Self::Resend => "发送",
            Self::Open => "打开",
            Self::OpenPath => "在文件夹打开",
            Self::CopyText => "复制文本",
            Self::FreeCopyText => "自由复制文本",
            Self::Delete => "删除",
            Self::MarkRead => "标为已读",
            Self::MarkAll => "全部标为已读",
            Self::ClearChat => "清空消息",
            Self::OpenConversation => "打开会话",
            Self::Refresh => "重新枚举本机网卡",
            Self::Settings => "设置",
            Self::About => "关于",
        }
    }

    /// 菜单项前置图标的 Material Design 矢量图标名（交给 `assets::md_icon_image`）。
    pub fn icon(self) -> &'static str {
        match self {
            Self::Cancel => "close",
            Self::Resend => "refresh",
            Self::Open => "open_in_new",
            Self::OpenPath => "folder",
            Self::CopyText | Self::FreeCopyText => "content_copy",
            Self::Delete => "delete",
            Self::MarkRead => "done",
            Self::MarkAll => "done_all",
            Self::ClearChat => "delete",
            Self::OpenConversation => "chat_bubble",
            Self::Refresh => "refresh",
            Self::Settings => "settings",
            Self::About => "info",
        }
    }
}

/// 侧栏里的一行会话。字段就是这一行要显示的全部东西。
#[derive(Clone, Debug, PartialEq)]
pub struct ConvRow {
    pub id: u64,
    pub name: String,
    /// 设备类型，决定头像里画哪个图标（原版 `avatarPixmap(devMode)`）。
    pub mode: i32,
    pub subtitle: String,
    pub preview: String,
    pub time: String,
    pub online: bool,
    pub selected: bool,
    pub unread: usize,
    /// 设备是否在充电：在副标题前画一枚 MD 电量图标，不再用 emoji。
    pub charging: bool,
    pub actions: Vec<MenuAction>,
}

impl ConvRow {
    fn of(app: &ChatApp, conversation: &Conversation) -> Self {
        let id = conversation.id;
        let online = app.is_online(&conversation.key());
        let time = conversation.last_at().map(model::list_time).unwrap_or_else(|| {
            if online { "在线".to_owned() } else { "离线".to_owned() }
        });
        // 有未读时第一项是「标为已读」，没有就退化成「打开会话」
        let lead = if conversation.unread > 0 { MenuAction::MarkRead } else { MenuAction::OpenConversation };
        Self {
            id,
            name: conversation.name().to_owned(),
            mode: conversation.device.mode,
            subtitle: format!("{} · {}", conversation.subtitle(), conversation.battery_text()),
            preview: conversation.preview(),
            time,
            online,
            selected: app.mode() == ChatMode::PerDevice && app.selected() == Some(id),
            unread: conversation.unread,
            charging: conversation.charging(),
            actions: vec![lead, MenuAction::ClearChat],
        }
    }
}

/// 「选择设备发送」里的一行。`target` 就是点它要带回的目标，兼当行键。
#[derive(Clone, Debug, PartialEq)]
pub struct PickRow {
    /// `None` = 全部设备（群发），`Some(会话号)` = 只发这一台。
    pub target: Option<u64>,
    pub name: String,
    /// 设备类型，决定头像里画哪个图标；`None` 是首行「全部设备」，改画 `groups` 矢量图。
    pub mode: Option<i32>,
    pub subtitle: String,
}

impl PickRow {
    /// 首行：一次发给全部在线设备。
    fn all(count: usize) -> Self {
        Self {
            target: None,
            name: "全部设备".to_owned(),
            mode: None,
            subtitle: format!("同时发给 {count} 台在线设备"),
        }
    }

    fn of(conversation: &Conversation) -> Self {
        Self {
            target: Some(conversation.id),
            name: conversation.name().to_owned(),
            mode: Some(conversation.device.mode),
            // 与侧栏同一句副标题：地址加版本 · 电量。能进这个列表的都是在线设备，不带在线离线状态。
            subtitle: format!("{} · {}", conversation.subtitle(), conversation.battery_text()),
        }
    }
}

/// 气泡的三种形态，与原版 `TextBubble` / `FileBubble` / `ImageBubble` 一一对应。
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    Text(String),
    File(FileCard),
    Image(ImageCard),
}

/// 状态胶囊：圆形徽标 + 文字。`ok` 决定绿底白勾还是红底白感叹号。
#[derive(Clone, Debug, PartialEq)]
pub struct Chip {
    pub text: String,
    pub ok: bool,
}

/// 文件（文件夹）卡片：图块 + 名称 + 大小 + 进度条 + 状态胶囊。
#[derive(Clone, Debug, PartialEq)]
pub struct FileCard {
    pub name: String,
    pub size: String,
    pub is_dir: bool,
    /// 0..1，自绘胶囊进度条的取值域。
    pub progress: f32,
    /// 原版只在 `0 < progress < 100` 且还没结果时画进度条。
    pub show_bar: bool,
    /// 状态胶囊，`None` 就是不画（传输中、刚重发）。
    pub chip: Option<Chip>,
}

impl FileCard {
    fn of(element: &model::FileElement, mine: bool, status: Status) -> Self {
        let progress = element.progress.clamp(0, 100);
        // 原版 `refreshStatusStyle`：收完还要确认文件在不在，不在就报「文件已被删除」。
        // 存在性由 `element.exists` 缓存供给（切换/收尾/翻页时刷），渲染期绝不 stat。
        let deleted = element.exists == Some(false);
        let chip = if element.completed {
            if !mine && deleted {
                Some(Chip { text: "文件已被删除".to_owned(), ok: false })
            } else {
                Some(Chip { text: if mine { "发送成功" } else { "接收成功" }.to_owned(), ok: true })
            }
        } else if status == Status::Failed {
            // 左侧中断统一叫「未完成」，右侧仍是「发送失败」，与原版文案一致
            Some(Chip { text: if mine { "发送失败" } else { "未完成" }.to_owned(), ok: false })
        } else {
            None
        };
        Self {
            name: element.name.clone(),
            size: element.size_text(),
            is_dir: element.is_dir,
            progress: progress as f32 / 100.0,
            // 有结果（胶囊）就不再画条：原版的 `setVisible(p > 0 && p < 100 && result.isEmpty())`
            show_bar: !element.completed && progress > 0 && progress < 100 && chip.is_none(),
            chip,
        }
    }
}

/// 图片气泡：完成前是深色占位块，完成后等缩略图解码就绪才换图。
#[derive(Clone, Debug, PartialEq)]
pub struct ImageCard {
    pub path: PathBuf,
    /// 占位块上的文字：`名称\nNN%`，失败时是失败文案。
    pub placeholder: String,
    /// 该不该画缩略图（完成且文件还在）。
    pub thumb: bool,
    /// 后台解码是否已就绪。建行时顺手投任务（`queue_thumb`），回包落库后 `sync` 重建
    /// 把这个布尔翻真——键控列表只重建**数据变了**的行，就绪那一帧该行才会换上图。
    pub ready: bool,
    /// 占位块的配色档：失败用更淡的底 + 橙红字。
    pub failed: bool,
}

impl ImageCard {
    fn of(element: &model::FileElement, mine: bool, status: Status) -> Self {
        let failed = !element.completed && status == Status::Failed;
        let placeholder = if failed {
            if mine { "发送失败" } else { "接收失败" }.to_owned()
        } else if element.completed {
            // 已完成、缩略图还在解：只显名字，避免“100%”误导
            element.name.clone()
        } else {
            format!("{}\n{}%", element.name, element.progress.clamp(0, 100))
        };
        let thumb = element.completed && element.exists != Some(false);
        // 最早的朴素方案：建行即投任务（`queue_thumb` 内部按缓存/在途去重，不会重复解），
        // 解完回包把 `ready` 翻真。不淘汰、不判视口。
        let ready = thumb && crate::assets::thumb_ready(&element.path);
        if thumb && !ready {
            crate::assets::queue_thumb(&element.path, crate::theme::THUMB_SIZE);
        }
        Self { path: element.path.clone(), placeholder, thumb, ready, failed }
    }
}

/// 一个气泡。
#[derive(Clone, Debug, PartialEq)]
pub struct Bubble {
    pub id: u64,
    /// 协议里的 `fileId`，文本气泡为空串。
    pub file_id: String,
    pub mine: bool,
    /// 气泡上方的昵称。原版每条消息都画，连续消息也不合并。
    pub device: String,
    /// 头像里的设备图标。
    pub mode: i32,
    /// 头像/昵称点下去要跳到哪个会话（`Msg::Select` 的号）。
    ///
    /// 只在合并（全部设备）流里给值：那面把好几台的消息汇成一排，得能一眼认出这条是谁的
    /// 并点回它的单独会话。分设备模式恒为 `None`——已经在那台的会话里，再跳是原地打转。
    pub target: Option<u64>,
    pub shape: Shape,
    /// 悬停提示：日期加时刻。原版把时间存进库却不画在气泡上，这里也不画，
    /// 只挂在 tooltip 上，免得想看时拿不到。
    pub tip: String,
    pub actions: Vec<MenuAction>,
    /// 「打开 / 在文件夹打开」要用的路径，与行数据同生命周期。
    pub path: PathBuf,
    /// 「复制文本 / 自由复制文本」的内容（原版 `displayText()`）：文本气泡是正文，文件气泡是文件名。
    pub text: String,
}

impl Bubble {
    fn of(message: &model::Message, device: &str, mode: i32) -> Self {
        let mine = message.side == Side::Mine;
        let file = message.file();
        // 菜单项与顺序对齐原版 `on_chatListWidget_customContextMenuRequested`：
        // 打开 / 在文件夹打开（仅传完的文件）→ 发送（文件消息都可，收到的回传也行）→
        // 复制文本 / 自由复制文本 → 删除。进行中的传输额外给「取消传输」顶在最前。
        let actions = match file {
            Some(element) => {
                let mut list = Vec::new();
                if message.status == Status::Ongoing {
                    list.push(MenuAction::Cancel);
                } else {
                    if element.completed && !element.path.as_os_str().is_empty() {
                        list.push(MenuAction::Open);
                        list.push(MenuAction::OpenPath);
                    }
                    list.push(MenuAction::Resend);
                }
                list.push(MenuAction::CopyText);
                list.push(MenuAction::FreeCopyText);
                list.push(MenuAction::Delete);
                list
            }
            None => vec![MenuAction::CopyText, MenuAction::FreeCopyText, MenuAction::Delete],
        };
        let text = match &message.content {
            model::Content::Text(text) => text.clone(),
            _ => file.map(|element| element.name.clone()).unwrap_or_default(),
        };
        let shape = match &message.content {
            model::Content::Text(text) => Shape::Text(text.clone()),
            model::Content::File(element) => Shape::File(FileCard::of(element, mine, message.status)),
            model::Content::Image(element) => Shape::Image(ImageCard::of(element, mine, message.status)),
        };
        Self {
            id: message.id,
            file_id: file.map(|element| element.id.clone()).unwrap_or_default(),
            mine,
            device: device.to_owned(),
            mode,
            // 能不能跳转由建行的一方补上（只有合并流拿得到行对应的会话）。
            target: None,
            shape,
            tip: format!("{} {}", model::day_label(message.at), message.clock()),
            actions,
            path: file.map(|element| element.path.clone()).unwrap_or_default(),
            text,
        }
    }
}

/// 消息区里的一行。只有最后一行要挂 `.reveal_when`，新消息到来时把视口滚到底。
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadRow {
    pub bubble: Bubble,
    pub last: bool,
    /// 首行且还有更早历史时置真：行上挂顶部滚动探针，滑到顶自动翻页。
    pub probe: bool,
}

impl ThreadRow {
    /// 键控列表的稳定键。
    pub fn key(&self) -> String {
        format!("msg-{}", self.bubble.id)
    }
}

/// 把会话的气泡换算成键控列表的行序。
///
/// 「是不是最后一行」必须进数据：键控列表只比较行本身，行外读到的东西不会跟着刷。
pub fn thread_rows(app: &ChatApp, conversation: &Conversation) -> Vec<ThreadRow> {
    let peer = (conversation.name().to_owned(), conversation.device.mode);
    // 本机发出的那几条署名是本机，与原版一样拿自己的设备名与设备类型
    let mine = app
        .local()
        .first()
        .map(|device| (device.name.clone(), device.mode))
        .unwrap_or_else(|| (app.settings().client_name.clone(), local_mode()));
    let last_index = conversation.messages.len().saturating_sub(1);
    // 缩略图的解码任务在 `ImageCard::of` 里随建行直接投后台队列：会话里的每张图都会
    // 排队解码，解完一张显一张；后台 4 条线程走 `image` 纯 Rust 全解，不占用 UI。
    let mut rows: Vec<ThreadRow> = conversation
        .messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let (who, mode) = if message.side == Side::Mine { &mine } else { &peer };
            ThreadRow { bubble: Bubble::of(message, who, *mode), last: index == last_index, probe: false }
        })
        .collect();
    // 还有更早历史时，首行挂上顶部探针：用户滑到顶即自动取上一页。
    if conversation.has_more_history {
        if let Some(first) = rows.first_mut() {
            first.probe = true;
        }
    }
    rows
}

/// 合并模式的行序：直接读 `ChatApp` 的独立全局流（跨设备有界分页，非从会话窗口拼），
/// 逐条气泡取自己携带的设备名与类型。
///
/// 群发一条文本时，引擎会为每台设备各回一条「本机发出」事件、各落库一行；全局流里
/// 它们是同刻同文的副本，视图按（秒级时刻 + 文本）去重成一根，与原版「一次群发 = 一个
/// 气泡」一致。文件群发是每台一条真实传输，各自有进度，不去重。
///
/// 顶部翻页探针挂在首行（`index == 0`）：滑到就走合并流的独立全局分页（`history_before_all`），
/// 与分设备翻页各走各的。`last` 按去重后的真实行数算，最后一行才挂滞底探针。
///
/// 收到的那几条还要把 `target` 标上会话号：合并流里点设备头像/昵称即跳到那台的分设备会话。
pub fn merged_rows(app: &ChatApp) -> Vec<ThreadRow> {
    let mine = app
        .local()
        .first()
        .map(|device| (device.name.clone(), device.mode))
        .unwrap_or_else(|| (app.settings().client_name.clone(), local_mode()));
    let feed = app.merged_feed();
    let owners = app.conversation_ids_by_uuid();
    let mut seen: HashSet<(i64, String)> = HashSet::new();
    let mut bubbles: Vec<Bubble> = Vec::with_capacity(feed.len());
    for item in feed {
        let message = &item.message;
        // 只对「本机发出的文本」去重：群发同一条会在全局流里各存一份同刻同文的副本。
        if message.side == Side::Mine {
            if let model::Content::Text(text) = &message.content {
                if !seen.insert((message.at / 1000, text.clone())) {
                    continue;
                }
            }
        }
        let (who, mode): (&str, i32) = if message.side == Side::Mine {
            (mine.0.as_str(), mine.1)
        } else {
            (item.name.as_str(), item.mode)
        };
        let mut bubble = Bubble::of(message, who, mode);
        // 本机发出的那几条不标跳转：那是自己，没有「对方的会话」可去。
        if message.side != Side::Mine {
            bubble.target = owners.get(item.device_id.as_str()).copied();
        }
        bubbles.push(bubble);
    }
    // 只有真正最后一条挂「滚到底」；顶部探针看全局流是否还有更早的一页（`has_more_merged`）。
    let last_index = bubbles.len().saturating_sub(1);
    let more = app.has_more_merged();
    bubbles
        .into_iter()
        .enumerate()
        .map(|(index, bubble)| ThreadRow { bubble, last: index == last_index, probe: more && index == 0 })
        .collect()
}

/// 只在真的变化时写入：`Signal::set` 无条件自增版本，省掉这次比较就要付出整棵子树重建。
fn put<T: PartialEq + Clone + 'static>(sig: Signal<T>, value: T) {
    if sig.get() != value {
        sig.set(value);
    }
}

/// 界面状态的全部信号。控件在建树时拿走它们的副本（`Signal` 是 `Copy` 的句柄），
/// 之后应用只通过 [`Ui::sync`] 改动它们。
#[derive(Clone, Copy)]
pub struct Ui {
    // —— 窗口与侧栏 ——
    pub title: Signal<String>,
    pub convs: Signal<Vec<ConvRow>>,
    /// 侧栏顶部「全部设备 / 分设备」分段。双向：分段控件写它、`sync_sidebar` 又按 `app.mode()` 回写。
    pub chat_mode: Signal<usize>,
    pub banner: Signal<String>,
    pub status_line: Signal<String>,
    pub query: Signal<String>,
    pub focus_search: Signal<u64>,
    // —— 会话标题栏与消息区 ——
    pub head_title: Signal<String>,
    pub head_detail: Signal<String>,
    pub head_visible: Signal<bool>,
    /// 当前接收目录，「打开接收目录」按钮点击时现取。
    pub save_path: Signal<String>,
    pub notice: Signal<String>,
    pub notice_visible: Signal<bool>,
    pub items: Signal<Vec<ThreadRow>>,
    pub empty_title: Signal<String>,
    pub empty_hint: Signal<String>,
    pub empty_visible: Signal<bool>,
    pub reveal: Signal<u64>,
    // —— 输入区 ——
    pub draft: Signal<String>,
    pub placeholder: Signal<String>,
    pub counter: Signal<String>,
    pub can_send: Signal<bool>,
    pub clipboard: Signal<bool>,
    pub emoji_open: Signal<bool>,
    pub focus_draft: Signal<u64>,
    // —— 接收确认与设置 ——
    pub offer_open: Signal<bool>,
    pub offer_title: Signal<String>,
    pub offer_body: Signal<String>,
    /// 待确认请求的键，「接收 / 拒绝」按钮要带回去。
    pub offer_key: Signal<u64>,
    // —— 网页服务 ——
    /// 侧栏那个「开启网页功能」勾选框。改了立即落盘，不等保存设置。
    pub web_service: Signal<bool>,
    /// 网页入口地址，空串就整行不画（没开服务或没网卡）。
    pub web_url: Signal<String>,
    // —— 网页授权 ——
    pub grant_open: Signal<bool>,
    pub grant_body: Signal<String>,
    /// 正在问的那张令牌，「同意 / 不同意」按钮要带回去。
    pub grant_token: Signal<String>,
    // —— 网页二维码 ——
    pub qr_open: Signal<bool>,
    pub qr_ips: Signal<Vec<String>>,
    pub qr_index: Signal<usize>,
    /// 已经烘进 `qr_svg` 的那个地址：只用来判断要不要重新编码。
    pub qr_ip: Signal<String>,
    pub qr_svg: Signal<String>,
    /// 非空就不画图而显这句（「未获取到本机 IP」/「二维码生成失败」）。
    pub qr_error: Signal<String>,
    /// 图片下面那行地址，包了未开服务时的提醒后缀。
    pub qr_url: Signal<String>,
    /// 「自由复制文本」对话框：开关与正文（只读多选输入框绑它）。
    pub free_copy_open: Signal<bool>,
    pub free_copy_text: Signal<String>,
    // —— 选设备发送 ——
    pub pick_open: Signal<bool>,
    /// 顶部那句「待发 N 项：…」。
    pub pick_files: Signal<String>,
    pub pick_rows: Signal<Vec<PickRow>>,
    pub settings_open: Signal<bool>,
    /// 「关于」对话框的开关。与设置同样只活在界面层：内容全是静态的，状态机没得记。
    pub about_open: Signal<bool>,
    // —— 更新提示 ——
    /// 开关跟的是 `ChatApp::update` 有没有值：这条得进状态机，因为它带内容、还要落盘。
    pub update_open: Signal<bool>,
    /// 标题那句「有新版本更新: 2.0」，对应原版 `QMessageBox` 的标题。
    pub update_title: Signal<String>,
    /// 更新说明正文。服务端给的是整页 HTML，进到状态机之前已经折成纯文本。
    pub update_body: Signal<String>,
    /// 「打开下载链接」那一下要交给系统浏览器的地址。点击时现取：对话框节点只建一次。
    pub update_url: Signal<String>,
    /// 强更档：左边的「不更新」换成「退出」，而且点了不记「已拒过」。
    pub update_force: Signal<bool>,
    pub f_save_path: Signal<String>,
    pub f_client_name: Signal<String>,
    pub f_message_key: Signal<String>,
    pub f_tcp: Signal<String>,
    pub f_udp: Signal<String>,
    pub f_web_service: Signal<bool>,
    pub f_open_web_service: Signal<bool>,
    pub f_font_family: Signal<String>,
    pub f_font_size: Signal<String>,
    pub f_enc_data: Signal<bool>,
    pub f_accept_recv: Signal<bool>,
    pub f_save_message: Signal<bool>,
    pub f_same_host: Signal<bool>,
    /// 内置图片预览器开关（原版 `useInternalImagePreview`）。
    pub f_internal_preview: Signal<bool>,
    /// 关闭主窗口时收进系统托盘（后台常驻）。图标装不装、关窗转不转隐藏都是构建期
    /// 声明，这一项改完要重启程序才生效。
    pub f_hide_on_close: Signal<bool>,
    pub f_theme: Signal<usize>,
    /// 当前配色档案。气泡里那些按档烘进去的定色不随 `ThemeHandle` 走，主题一换
    /// 就必须整棵重烘，所以把它也做成信号，交给根节点 `host_signal` 重建。
    pub skin: Signal<theme::Skin>,
}

impl Ui {
    /// 用当前状态建一整套信号。只能在 UI 线程上调（`signal()` 是线程局部的）。
    pub fn new(app: &ChatApp) -> Self {
        let ui = Self {
            title: signal(String::new()),
            convs: signal(Vec::new()),
            chat_mode: signal(0),
            banner: signal(String::new()),
            status_line: signal(String::new()),
            query: signal(String::new()),
            focus_search: signal(0u64),
            head_title: signal(String::new()),
            head_detail: signal(String::new()),
            head_visible: signal(false),
            save_path: signal(String::new()),
            notice: signal(String::new()),
            notice_visible: signal(false),
            items: signal(Vec::new()),
            empty_title: signal(String::new()),
            empty_hint: signal(String::new()),
            empty_visible: signal(false),
            reveal: signal(0u64),
            draft: signal(String::new()),
            placeholder: signal(String::new()),
            counter: signal(String::new()),
            can_send: signal(false),
            clipboard: signal(false),
            emoji_open: signal(false),
            focus_draft: signal(0u64),
            offer_open: signal(false),
            offer_title: signal(String::new()),
            offer_body: signal(String::new()),
            offer_key: signal(0u64),
            web_service: signal(false),
            web_url: signal(String::new()),
            grant_open: signal(false),
            grant_body: signal(String::new()),
            grant_token: signal(String::new()),
            qr_open: signal(false),
            qr_ips: signal(Vec::new()),
            qr_index: signal(0usize),
            qr_ip: signal(String::new()),
            qr_svg: signal(String::new()),
            qr_error: signal(String::new()),
            qr_url: signal(String::new()),
            free_copy_open: signal(false),
            free_copy_text: signal(String::new()),
            pick_open: signal(false),
            pick_files: signal(String::new()),
            pick_rows: signal(Vec::new()),
            settings_open: signal(false),
            about_open: signal(false),
            update_open: signal(false),
            update_title: signal(String::new()),
            update_body: signal(String::new()),
            update_url: signal(String::new()),
            update_force: signal(false),
            f_save_path: signal(String::new()),
            f_client_name: signal(String::new()),
            f_message_key: signal(String::new()),
            f_tcp: signal(String::new()),
            f_udp: signal(String::new()),
            f_web_service: signal(false),
            f_open_web_service: signal(false),
            f_font_family: signal(String::new()),
            f_font_size: signal(String::new()),
            f_enc_data: signal(false),
            f_accept_recv: signal(false),
            f_save_message: signal(false),
            f_same_host: signal(false),
            f_internal_preview: signal(true),
            f_hide_on_close: signal(true),
            f_theme: signal(0usize),
            skin: signal(app.skin()),
        };
        ui.sync(app);
        // 启动时把配置抄进表单，设置对话框第一次打开就有内容
        ui.fill_form(app);
        ui
    }

    /// 把状态机投影到界面上。消息处理后与心跳各调一次，幂等。
    pub fn sync(&self, app: &ChatApp) {
        put(self.title, app.title_text());
        put(self.banner, app.banner_text());
        put(self.status_line, app.status_text());
        put(self.save_path, app.settings().save_path.clone());
        put(self.skin, app.skin());
        self.sync_sidebar(app);
        self.sync_thread(app);
        self.sync_composer(app);
        self.sync_offer(app);
        self.sync_web(app);
        self.sync_grant(app);
        self.sync_qr(app);
        self.sync_free_copy(app);
        self.sync_pick(app);
        self.sync_update(app);
    }

    /// 更新提示：四样内容只在提示挂出来的那一刻一齐换，开着期间不会被动到其他人的值。
    /// 没挂提示时只需把开关掩下去，正文那些旧值留在信号里也无害。
    fn sync_update(&self, app: &ChatApp) {
        match app.update_prompt() {
            Some(prompt) => {
                put(self.update_title, format!("有新版本更新: {}", prompt.version));
                put(self.update_body, prompt.body.clone());
                put(self.update_url, prompt.url.clone());
                put(self.update_force, prompt.force);
                put(self.update_open, true);
            }
            None => put(self.update_open, false),
        }
    }

    /// 「选择设备发送」：行与说明只在开着时算，关着就只把开关按下去。
    /// 开着期间每台设备都真在线才有行，设备增减时要跟着刷，所以每次同步都重建行表；
    /// `put` 先比过值，列表没变就不会推动控件重建。
    fn sync_pick(&self, app: &ChatApp) {
        if !app.picker_open() {
            put(self.pick_open, false);
            return;
        }
        let devices = app.picker_rows();
        let mut rows = vec![PickRow::all(devices.len())];
        rows.extend(devices.into_iter().map(PickRow::of));
        put(self.pick_rows, rows);
        put(self.pick_files, app.picker_files());
        put(self.pick_open, true);
    }

    /// 自由复制对话框：正文只在打开时换，开着期间不覆盖（用户可能正在里面拖选）。
    fn sync_free_copy(&self, app: &ChatApp) {
        match app.free_copy() {
            Some(text) => {
                put(self.free_copy_text, text.to_owned());
                put(self.free_copy_open, true);
            }
            None => put(self.free_copy_open, false),
        }
    }

    /// 侧栏：会话行。搜索词由 `Ui::query` 信号自己拿着，`ChatApp` 在同步前把它拉过去。
    pub fn sync_sidebar(&self, app: &ChatApp) {
        // 回写分段：点设备行会顺带切到分设备，这里让分段跟着反映。`put` 先比过值，不无谓 bump。
        put(self.chat_mode, app.mode().index());
        let rows: Vec<ConvRow> = app.visible().into_iter().map(|item| ConvRow::of(app, item)).collect();
        put(self.convs, rows);
    }

    pub fn sync_thread(&self, app: &ChatApp) {
        match app.mode() {
            // 合并：不分设备，主区汇一条流，标题栏固定「全部设备」。
            ChatMode::Merged => {
                let rows = merged_rows(app);
                put(self.head_visible, true);
                put(self.head_title, "全部设备".to_owned());
                put(self.head_detail, app.merged_head_detail());
                put(self.empty_visible, rows.is_empty());
                put(self.empty_title, "还没有消息".to_owned());
                put(
                    self.empty_hint,
                    "发给所有在线设备的消息、以及任何设备发来的消息都会汇到这里；按 Ctrl+Enter 发送。".to_owned(),
                );
                put(self.items, rows);
                put(self.reveal, app.reveal_key());
            }
            ChatMode::PerDevice => match app.current() {
                None => {
                    put(self.head_visible, false);
                    put(self.head_title, "局域网文件传输".to_owned());
                    put(self.head_detail, HEAD_UNSELECTED.to_owned());
                    put(self.items, Vec::new());
                    put(self.empty_visible, true);
                    put(self.empty_title, "还没有选择设备".to_owned());
                    put(self.empty_hint, "在左边点一台设备，也可以直接把文件或文件夹拖进窗口。".to_owned());
                }
                Some(conversation) => {
                    put(self.head_visible, true);
                    put(self.head_title, conversation.name().to_owned());
                    put(self.head_detail, app.head_detail(conversation));
                    put(self.items, thread_rows(app, conversation));
                    put(self.empty_visible, conversation.messages.is_empty());
                    put(self.empty_title, "还没有消息".to_owned());
                    put(
                        self.empty_hint,
                        format!("给 {} 发一条消息或传一个文件试试，按 Ctrl+Enter 发送。", conversation.name()),
                    );
                    put(self.reveal, app.reveal_key());
                }
            },
        }
        match app.notice() {
            Some(text) => {
                put(self.notice, text.to_owned());
                put(self.notice_visible, true);
            }
            None => put(self.notice_visible, false),
        }
    }

    pub fn sync_composer(&self, app: &ChatApp) {
        put(self.placeholder, app.placeholder());
        let text = self.draft.get();
        put(self.counter, format!("{} 字", text.chars().count()));
        // 合并模式要至少有一台在线设备才能群发；分设备模式要有选中。
        let has_target = match app.mode() {
            ChatMode::Merged => app.online_count() > 0,
            ChatMode::PerDevice => app.selected().is_some(),
        };
        put(self.can_send, has_target && !text.trim().is_empty());
        put(self.clipboard, app.clipboard_on());
    }

    pub fn sync_offer(&self, app: &ChatApp) {
        match app.pending_offer() {
            Some(offer) => {
                put(self.offer_title, format!("{} 想发送文件", offer_name(offer)));
                put(self.offer_body, app.offer_body());
                put(self.offer_key, offer.key);
                put(self.offer_open, true);
            }
            None => put(self.offer_open, false),
        }
    }

    /// 侧栏那行网页入口：勾选框与地址。地址空串就是不显示，判断在 `ChatApp::web_url()` 里。
    pub fn sync_web(&self, app: &ChatApp) {
        put(self.web_service, app.settings().web_service);
        put(self.web_url, app.web_url());
    }

    /// 网页授权框。文件接收确认还排着时先不弹：原版两个都是 `exec()` 级模态，
    /// 只会一个一个地问，两个遮罩叠在一起只会压得下面的按钮点不到。
    pub fn sync_grant(&self, app: &ChatApp) {
        match app.pending_grant() {
            Some((token, ip)) if app.pending_offer().is_none() => {
                put(self.grant_token, token.to_owned());
                put(self.grant_body, format!("是否同意网页客户端：【{ip}】的访问请求？"));
                put(self.grant_open, true);
            }
            _ => put(self.grant_open, false),
        }
    }

    /// 二维码对话框：网卡列表、选中项、那张 SVG，与地址行。
    pub fn sync_qr(&self, app: &ChatApp) {
        let open = app.qr_open();
        put(self.qr_open, open);
        if !open {
            return;
        }
        let ips = app.qr_ips();
        let index = app.qr_index().min(ips.len().saturating_sub(1));
        put(self.qr_ips, ips.clone());
        put(self.qr_index, index);
        let address = ips.get(index).cloned().unwrap_or_default();
        // 编码不便宜，而同步在每次用户动作与每 200ms 心跳都要跑一趟：
        // 只在地址真的换了时才重生成
        if address == self.qr_ip.get() {
            return;
        }
        put(self.qr_ip, address.clone());
        if address.is_empty() {
            put(self.qr_svg, String::new());
            put(self.qr_error, "未获取到本机 IP".to_owned());
            put(self.qr_url, String::new());
            return;
        }
        let url = crate::web::qr::web_url(&address, app.settings().tcp_port);
        if let Some(svg) = crate::web::qr::svg_text(&url) {
            put(self.qr_svg, svg);
            put(self.qr_error, String::new());
        } else {
            put(self.qr_svg, String::new());
            put(self.qr_error, "二维码生成失败".to_owned());
        }
        // 地址保持纯净（整段选中即复制，不夹带提醒文案）；“未开启网页功能”的提醒改由对话框
        // 按 `web_service` 单独画一行，与原版同一条文案。
        put(self.qr_url, url);
    }

    /// 打开设置时把配置抄进表单信号。平时不再覆盖，免得把用户正在打的字冲掉。
    pub fn fill_form(&self, app: &ChatApp) {
        let config = app.settings();
        put(self.f_save_path, config.save_path.clone());
        put(self.f_client_name, config.client_name.clone());
        put(self.f_message_key, config.message_key.clone());
        put(self.f_tcp, config.tcp_port.to_string());
        put(self.f_udp, config.udp_port.to_string());
        put(self.f_web_service, config.web_service);
        put(self.f_open_web_service, config.open_web_service);
        put(self.f_font_family, config.font_family.clone());
        put(self.f_font_size, config.font_size.to_string());
        put(self.f_enc_data, config.enc_data);
        put(self.f_accept_recv, config.accept_recv_files);
        put(self.f_save_message, config.save_message);
        put(self.f_same_host, config.allow_same_host_test);
        put(self.f_internal_preview, config.use_internal_image_preview);
        put(self.f_hide_on_close, config.hide_on_close);
        put(self.f_theme, ThemePref::from_theme(&config.theme).index());
    }

    /// 读表单信号，拼出用户改过的配置草稿。端口与字号仍是文本，交给 `ChatApp` 解析校验。
    pub fn read_form(&self, app: &ChatApp) -> FormDraft {
        let mut config = app.settings().clone();
        config.save_path = self.f_save_path.get();
        config.client_name = self.f_client_name.get();
        config.message_key = self.f_message_key.get();
        config.font_family = self.f_font_family.get();
        config.enc_data = self.f_enc_data.get();
        config.accept_recv_files = self.f_accept_recv.get();
        config.save_message = self.f_save_message.get();
        config.allow_same_host_test = self.f_same_host.get();
        config.use_internal_image_preview = self.f_internal_preview.get();
        config.hide_on_close = self.f_hide_on_close.get();
        config.web_service = self.f_web_service.get();
        config.open_web_service = self.f_open_web_service.get();
        config.theme = ThemePref::from_index(self.f_theme.get()).as_theme().to_owned();
        FormDraft { config, tcp: self.f_tcp.get(), udp: self.f_udp.get(), font_size: self.f_font_size.get() }
    }
}

/// 设置表单里按文本编辑的数值项。
pub struct FormDraft {
    pub config: Config,
    pub tcp: String,
    pub udp: String,
    pub font_size: String,
}

fn offer_name(offer: &ReceiveOffer) -> String {
    if offer.device.name.is_empty() { offer.device.ip.clone() } else { offer.device.name.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_message(completed: bool, status: Status, path: &str) -> model::Message {
        let element = model::FileElement {
            id: "f1".to_owned(),
            name: "报告.zip".to_owned(),
            size: 1024,
            path: PathBuf::from(path),
            progress: if completed { 100 } else { 40 },
            completed,
            received: true,
            is_dir: false,
            exists: None,
        };
        model::Message { id: 1, side: Side::Peer, content: model::Content::File(element), at: 0, status, clipboard: false }
    }

    /// 菜单项与顺序对齐 C++ 版 `on_chatListWidget_customContextMenuRequested`：
    /// 打开/在文件夹打开（仅传完且有路径）→ 发送 → 复制文本/自由复制文本 → 删除；
    /// 进行中的传输用「取消传输」顶替打开与发送。
    #[test]
    fn bubble_menu_matches_cpp_version() {
        let text = Bubble::of(&model::Message::text(Side::Peer, "你好", 0, false), "peer", 0);
        assert_eq!(text.actions, vec![MenuAction::CopyText, MenuAction::FreeCopyText, MenuAction::Delete]);
        assert_eq!(text.text, "你好");

        let done = Bubble::of(&file_message(true, Status::Finished, "C:/LANShare/报告.zip"), "peer", 0);
        assert_eq!(
            done.actions,
            vec![MenuAction::Open, MenuAction::OpenPath, MenuAction::Resend, MenuAction::CopyText, MenuAction::FreeCopyText, MenuAction::Delete]
        );
        assert_eq!(done.text, "报告.zip", "文件气泡的复制文本取文件名（原版 displayText）");

        let ongoing = Bubble::of(&file_message(false, Status::Ongoing, ""), "peer", 0);
        assert_eq!(ongoing.actions, vec![MenuAction::Cancel, MenuAction::CopyText, MenuAction::FreeCopyText, MenuAction::Delete]);

        let failed = Bubble::of(&file_message(false, Status::Failed, ""), "peer", 0);
        assert_eq!(failed.actions, vec![MenuAction::Resend, MenuAction::CopyText, MenuAction::FreeCopyText, MenuAction::Delete]);

        let no_path = Bubble::of(&file_message(true, Status::Finished, ""), "peer", 0);
        assert_eq!(
            no_path.actions,
            vec![MenuAction::Resend, MenuAction::CopyText, MenuAction::FreeCopyText, MenuAction::Delete],
            "没有落盘路径时不给打开项"
        );
    }
}
