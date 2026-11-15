//! 分发给应用的全部消息：界面操作与后台泵带回的引擎事件。

use std::path::PathBuf;

use crate::lanshare::NetEvent;

/// 分发给应用的全部消息：界面操作与后台泵带回的引擎事件。
///
/// 文本类输入（搜索词、草稿、设置表单）不在这里——控件直接写自己的信号，
/// 省掉每敲一个字跑一趟消息循环。
#[derive(Clone, Debug)]
pub enum Msg {
    /// 后台泵带回的事件。
    Events(Vec<NetEvent>),
    /// 切换会话。
    Select(u64),
    /// 往上翻一页更早的历史（消息区顶部探针触发）。
    LoadOlder(u64),
    /// 后台缩略图解码完成的一批回包（懒加载：像素在解码线程产，`Image` 在本线程构）。
    ThumbsReady(Vec<crate::assets::DecodedThumb>),
    /// 发送当前草稿。
    Send,
    /// 在草稿末尾插入一段文本（表情面板）。
    InsertText(String),
    /// 「发到对方剪贴板」开关。
    Clipboard(bool),
    /// 打开文件选择框。
    PickFiles,
    /// 打开文件夹选择框。
    PickFolder,
    /// 发送这些路径（选择框与拖放共用）。
    SendPaths(Vec<PathBuf>),
    /// 「选择设备发送」里点了一项：`None` = 全部设备（群发），`Some(会话号)` = 只发这台。
    PickTarget(Option<u64>),
    /// 关掉「选择设备发送」：那批文件一个都不发，直接丢掉。
    ClosePicker,
    /// 取消一条传输。
    Cancel(String),
    /// 重发一条文件消息（原版「发送」）。
    Resend(String),
    /// 交给系统默认程序打开这个文件（原版「打开」）。
    OpenFile(PathBuf),
    /// 在文件管理器里打开所在位置（原版「在文件夹打开」）。
    OpenPath(PathBuf),
    /// 打开「自由复制文本」对话框（原版 `CopyableTextDialog`）。
    FreeCopyText(String),
    /// 自由复制对话框的「复制全部文本」：复制并关掉。
    CopyFreeCopy,
    CloseFreeCopy,
    /// 用图片预览器打开这张图（消息列表左键点击缩略图）。
    PreviewImage(PathBuf),
    /// 删除一条本地气泡。
    DeleteMessage(u64),
    /// 某个会话标为已读。
    MarkRead(u64),
    /// 全部会话标为已读。
    MarkAll,
    /// 清空某个会话的气泡（不动历史库）。
    ClearChat(u64),
    /// 接受 / 拒绝对方的文件发送。
    Resolve { key: u64, accept: bool },
    /// 接受 / 拒绝网页客户端的访问请求。带令牌而不是带下标：队列会边弹边缩，
    /// 对话框节点又是早就建好的，按下标回去会答错人。
    ResolveWebGrant { token: String, accept: bool },
    /// 打开「网页二维码」对话框。
    OpenQr,
    CloseQr,
    /// 二维码对话框里的「刷新」：重新枚举本机网卡。
    RefreshQr,
    /// 后台查回的公网 IPv6（`None` = 没查到），仅供二维码下拉多列一项展示，
    /// 不参与实际收发。由后台线程发回，故单列一条消息而不是在打开时同步取。
    QrV6(Option<String>),
    OpenSettings,
    CloseSettings,
    SaveSettings,
    /// 打开「关于」：对应原版菜单栏「关于」里的 `actionAbout`。
    OpenAbout,
    /// 关掉「关于」。按 ESC 收起遮罩也算关：这页不改任何状态。
    CloseAbout,
    /// 查一次更新。`manual` = 用户在「关于」里点的那一下，要说实话（没新版也报一句、
    /// 失败也报）；启动时那一次是自动的，安静到只在真有新版时出声。对应原版
    /// `UpdateChecker::check(parent, manual)`。
    CheckUpdate { manual: bool },
    /// 后台线程查回的版本信息，`Err` 里那句直接拼进「检测更新失败：」。`manual` 跟着结果
    /// 一起回来：从发出到回包中间可能又处理了一堆事件，不能去读当时的界面状态。
    UpdateReport { result: Box<Result<crate::utils::update::Report, String>>, manual: bool },
    /// 把更新提示收起来（点 ×、按 ESC、或已经把下载链接开出去）：不记「已拒过」。
    CloseUpdate,
    /// 「不更新」：记下这一版，之后的自动检测不再提它。
    DeclineUpdate,
    /// 强更那一档点「退出」。
    QuitForUpdate,
    /// 选保存目录。
    PickSavePath,
    /// 文件夹选择框回填的保存目录。回调拿不到 `EventCtx`，只能走一条消息回来写表单。
    SavePathPicked(String),
    /// 快速切换主题。
    CycleTheme,
    /// 系统深浅色变了，「跟随系统」时要用。
    SystemTheme(bool),
    /// 重新枚举本机网卡。
    Refresh,
    /// 把一段文本写进系统剪贴板（本机地址 / 网页地址的「复制」按钮）。
    Copy(String),
    /// 顶部提示条。
    Notice(String),
    DismissNotice,
}
