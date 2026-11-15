//! 右侧主区：设备标题栏、提示条、消息（气泡）列表。
//!
//! 气泡的三种形态照抄 `ChatItemWidget`：文本（`TextBubble`）、文件（`FileBubble`）、
//! 图片（`ImageBubble`）。一行的骨架也是抄的：外边距 12/8、头像与内容间距 10、
//! 头像 40 顶对齐、头像上方那一列先昵称后气泡，宽度上限 500。
//!
//! 三处与原版「不一样」的地方是刻意的：
//! - 原版每条消息都画头像和昵称，我们照做，所以没有「连续同向合并」的逻辑；
//! - 原版气泡上不画时间（`createTime` 只进库），我们把日期时刻挂到 tooltip。
//! - 合并（全部设备）流里收到的那几条，头像与昵称点得：点一下就跳到那台的分设备会话。
//!   原版只有一个会话列表，没有「跳过去」这一步。
//!
//! 列表同样键控：[`ThreadRow`] 带着「是不是最后一行」，新消息只重建那一行。
//! 颜色是在建树时烘进元素的，主题切换由根节点的 `host_signal(ui.skin, …)` 整棵重烘。

use std::path::PathBuf;
use std::rc::Rc;

use windui::prelude::*;

use crate::app::{ChatMode, Msg};
use crate::theme::Skin;
use crate::{assets, theme};
use super::state::{Bubble, FileCard, ImageCard, Shape, ThreadRow, Ui};
use super::{
    bubble_menu, caption, device_avatar, file_tile, header_menu, icon_button, menu_button, pill_progress, status_chip,
};

/// 主区整体：标题栏 + 提示条 + 消息列表 + 输入区。
pub fn build(ui: &Ui, tx: &Sender<Msg>, skin: Skin) -> Element {
    Element::col()
        .fill()
        .child(header(ui, tx, skin))
        .child(notice(ui, tx, skin))
        .child(body(ui, tx, skin))
        .child(super::composer::build(ui, tx, skin))
}

/// 会话标题栏：头像、设备名与地址电量、打开接收目录、会话菜单。
fn header(ui: &Ui, tx: &Sender<Msg>, skin: Skin) -> Element {
    // 当前选中的那一行，取 0..1 条：头像跟着它重建，菜单项点击时也从这里现取会话号。
    let head = ui.convs.map(|rows| rows.iter().filter(|row| row.selected).cloned().take(1).collect::<Vec<_>>());
    // 合并态下没有任何行是选中的，靠这个区分“有设备头像”与“多设备占位块”。
    let has_sel = ui.convs.map(|rows| rows.iter().any(|row| row.selected));
    let merged = ui.chat_mode.map(|index| *index == ChatMode::Merged.index());
    let path = ui.save_path;
    let open_tx = tx.clone();
    let mode_sig = ui.chat_mode;
    Element::row()
        .width_match()
        .height(theme::HEADER_HEIGHT)
        .padding_xy(16, 0)
        .spacing(10)
        .cross(Align::Center)
        .border_role(Role::Border, 1)
        .border_edges(Edges::BOTTOM)
        // 头像位：分设备显选中设备头像，合并显一枚多设备占位块，两者叠在同一格互斥可见。
        .child(
            Element::stack()
                .width(40)
                .height(40)
                .child(
                    Element::host_signal(head, move |row| device_avatar(row.mode, 40, true, row.online, skin))
                        .width(40)
                        .height(40)
                        .visible_signal(has_sel),
                )
                .child(
                    assets::md_icon("groups", skin.icon_tint(), 24)
                        .align_xy(Align::Center, Align::Center)
                        .visible_signal(merged),
                ),
        )
        .child(
            Element::col()
                .weight(1.0)
                .spacing(2)
                .child(
                    Element::label(ui.head_title)
                        .font_size(theme::FONT_TITLE)
                        .font_weight(600)
                        .fg_role(Role::Text)
                        .max_lines(1)
                        .truncate(Truncate::End),
                )
                .child(
                    Element::label(ui.head_detail)
                        .font_size(theme::FONT_META)
                        .fg_role(Role::TextMuted)
                        .max_lines(2)
                        .line_height(1.4),
                ),
        )
        .child(
            Element::icon_button_content(ImageContent::from_bytes(assets::ICON_OPEN))
                .tooltip("打开接收目录")
                .on_click(move |_| {
                    let _ = open_tx.send(Msg::OpenPath(PathBuf::from(path.get())));
                }),
        )
        .child(menu_button("more_vert", "会话操作", tx, move |tx| {
            header_menu(head_id(&head.get()), mode_sig.get() == ChatMode::Merged.index(), tx)
        }, skin))
}

/// 选中会话的号，一台会话都没有时给 `None`（会话号从 1 起编，`0` 只会是「无选中」）。
/// 标题栏菜单据此不收「标记已读 / 清空消息」——那两个动作按号在状态机里找不到会话。
fn head_id(rows: &[super::state::ConvRow]) -> Option<u64> {
    rows.first().map(|row| row.id)
}

/// 顶部提示条：发送失败、参数非法等一次性提示。
fn notice(ui: &Ui, tx: &Sender<Msg>, skin: Skin) -> Element {
    Element::row()
        .width_match()
        .spacing(8)
        .cross(Align::Center)
        .padding_xy(12, 8)
        .margin_edges(Insets::new(16, 8, 16, 0))
        .bg_role_alpha(Role::Warning, 0.16)
        .corner(theme::RADIUS_CARD)
        .child(assets::md_icon("warning", skin.icon_tint(), 18))
        .child(Element::label(ui.notice).font_size(theme::FONT_META).fg_role(Role::Text).weight(1.0).line_height(1.4))
        .child(icon_button("close", "关闭提示", Msg::DismissNotice, tx, skin))
        .visible_signal(ui.notice_visible)
}

/// 消息区：滚动列表，没有消息时压一层占位说明。
///
/// 空态做成覆盖层而不是列表里的一行：[`Element::list_signal`] 的滚动容器按无限高度
/// 测子元素，往里塞 `weight` 的撑满内容会塌成 0。
fn body(ui: &Ui, tx: &Sender<Msg>, skin: Skin) -> Element {
    let sender = tx.clone();
    let empty = ui.items.map(|rows| rows.is_empty());
    // 行闭包要求 `'static`，所以只把需要的信号按拷贝取出来，不带走 `&Ui`。
    let reveal = ui.reveal;
    // 探针发 `Msg::LoadOlder` 要带当前会话号。整个 `body` 不随切换会话重建（根只在 `skin`
    // 变时重建），但切换会同时换 `ui.convs` 的选中行与 `ui.items` → 行重建 → 这里按行重建时
    // 现取选中会话号，始终是当前那份。
    let convs = ui.convs;
    // 合并流用哨兵会话号，与任何真实会话（从 1 起编号）在共享的 `PinState`/`ProbeState` 里区分开。
    let chat_mode = ui.chat_mode;
    // 「滚到底」探针的共享状态：记住上次钉过的是哪个会话（`Rc` 包 `PinState`），各行共享。
    // 切会话（含来回切）都重新钉一次到底；同一会话内的后续重排不再跟用户上滚较劲。
    let pin = Rc::new(super::bottom_probe::PinState::new());
    // 顶部探针的共享装填态：翻页前插会把首行连 widget 一起重建，装填态只存在 widget 里
    // 就会被每次重建重新装填，一次触顶连环翻到库底，所以跨重建挂在这枚 `Rc` 上。
    let probe = Rc::new(super::scroll_probe::ProbeState::new());
    Element::stack()
        .width_match()
        .weight(1.0)
        .bg(skin.chat_bg())
        .child(
            Element::list_signal(ui.items, |row| row.key(), move |row| row_of(row, reveal, skin, &sender, convs, chat_mode, pin.clone(), probe.clone()))
                .padding_edges(Insets::new(0, 2, 0, 10)),
        )
        .child(empty_state(ui, skin).visible_signal(empty))
}

/// 当前选中的会话号，没选中时给 0——与探针发射时同源，行重建（切会话）时现取。
fn selected_conv(convs: &Signal<Vec<super::state::ConvRow>>) -> u64 {
    convs.get().iter().find(|item| item.selected).map(|item| item.id).unwrap_or_default()
}

/// 合并流的探针会话号。真实会话从 1 起编号，`0` 是「无选中」，都不够拉开距离，
/// 故取一个不可能碰上的极大值，使合并流与分设备在共享的 `PinState`/`ProbeState` 里各自独立。
///
/// 它转成 `i64` 就是 `-1`，所以两处共享状态的「未初始化」哨兵统一用 `i64::MIN`，
/// 否则合并流会被当成「已经处理过」，顶不顶底、翻不翻页都差一帧。
const MERGED_CONV: u64 = u64::MAX;

/// 探针会话号：合并模式返哨兵 `MERGED_CONV`（翻页带它回，dispatch 按模式走合并翻页），
/// 分设备模式返当前选中会话。切会话/切模式时行重建，这里现取。
fn probe_conv(convs: &Signal<Vec<super::state::ConvRow>>, chat_mode: Signal<usize>) -> u64 {
    if chat_mode.get() == ChatMode::Merged.index() {
        MERGED_CONV
    } else {
        selected_conv(convs)
    }
}

/// 一行：气泡行。最后一行同时挂 `.reveal_when`（新消息到来时滚到底）与一枚「滚到底」
/// 探针 [`BottomProbe`]（按会话钉底，首屏与切会话都靠它）；首行若还要更早历史，则在其上方
/// 再挂一枚顶部滚动探针（滑到顶自动翻页）。
fn row_of(row: ThreadRow, reveal: Signal<u64>, skin: Skin, tx: &Sender<Msg>, convs: Signal<Vec<super::state::ConvRow>>, chat_mode: Signal<usize>, pin: Rc<super::bottom_probe::PinState>, probe: Rc<super::scroll_probe::ProbeState>) -> Element {
    // 探针是一枚 `1×1` 响应式叶子：`on_update` 沿父链找到滚动祖先，按会话号一次性把
    // `scroll_y` 钉到底部。它压在头像那一格上（见 `bubble_row`），不能当成行的又一个孩子挂。
    let bottom = row.last.then(|| {
        Element::leaf()
            .size(1, 1)
            .widget(super::bottom_probe::BottomProbe::new(pin, probe_conv(&convs, chat_mode)))
            .reactive()
    });
    let bubble = bubble_row(row.bubble, skin, tx, bottom);
    let bubble = if row.last { bubble.reveal_when(reveal) } else { bubble };
    if !row.probe {
        return bubble;
    }
    let conv_id = probe_conv(&convs, chat_mode);
    let probe_leaf = Element::leaf()
        .size(1, 1)
        .widget(super::scroll_probe::ScrollProbe::new(tx.clone(), conv_id, probe))
        .reactive();
    Element::col().width_match().child(probe_leaf).child(bubble)
}

/// 一行消息：昵称列 + 头像（本机发出的靠右），两头各留一个撑开位保证贴边。
///
/// 头像顶对齐（原版 `m_avatar` 是 `Qt::AlignTop`），列与气泡都按方向收边，
/// 于是短消息的气泡跟着内容收窄，不会被宽度上限撑成一块板。
///
/// `bottom` 是最后一行才有的「滚到底」探针，与头像叠在同一格：行是 `spacing(ROW_SPACING)`
/// 的 row，多一个孩子就多一份间距加它自己的宽度，而本机发出那一排的头像是末尾的实心孩子——
/// 直接挂在行尾会把最新那条的头像朝左挪出 11px，与其他条对不齐。头像这格尺寸钉死，压个孩子
/// 进去既不改布局也不显形。
fn bubble_row(bubble: Bubble, skin: Skin, tx: &Sender<Msg>, bottom: Option<Element>) -> Element {
    let mine = bubble.mine;
    let avatar = match bottom {
        Some(probe) => Element::stack()
            .size(theme::AVATAR_SIZE, theme::AVATAR_SIZE)
            .child(device_avatar(bubble.mode, theme::AVATAR_SIZE, false, true, skin))
            .child(probe),
        None => device_avatar(bubble.mode, theme::AVATAR_SIZE, false, true, skin),
    };
    let nickname = Element::label(bubble.device.clone())
        .font_size(theme::NICKNAME_FONT)
        .fg(skin.nickname())
        .max_lines(1)
        .truncate(Truncate::End);
    // 合并（全部设备）流里每根气泡都知道自己属于哪台会话（`target`）：头像与昵称都点得，
    // 点一下就跳到那台的分设备会话（`select` 顺带把列表模式也切过去）。`target` 为 `None`
    // 时（分设备模式、本机发出的那几条、旧库里没归属设备的行）不挂点击，行还是原来的行。
    let jump = bubble.target.map(|id| (id, bubble.device.clone()));
    let avatar = match &jump {
        Some((id, name)) => jump_to_conv(avatar, *id, name, tx),
        None => avatar,
    };
    let nickname = match &jump {
        // 昵称是个 `label`，一个节点只有一个 widget 槽，`.widget()` 会把标签本身顶掉，
        // 所以套一层 `row` 把点击挂在容器上；上限宽度照抄列的，长名字该截断还截断。
        Some((id, name)) => jump_to_conv(Element::row().max_width(theme::BUBBLE_MAX_WIDTH).child(nickname), *id, name, tx),
        None => nickname,
    };
    let card = bubble_card(bubble, skin, tx);
    let column = Element::col()
        .max_width(theme::BUBBLE_MAX_WIDTH)
        .spacing(theme::COLUMN_SPACING)
        .cross(if mine { Align::End } else { Align::Start })
        .child(nickname)
        .child(card);

    let line = Element::row()
        .width_match()
        .margin_edges(Insets::new(theme::ROW_MARGIN_X, theme::ROW_MARGIN_Y, theme::ROW_MARGIN_X, theme::ROW_MARGIN_Y))
        .spacing(theme::ROW_SPACING)
        .cross(Align::Start);
    if mine {
        line.child(Element::flex_spacer()).child(column).child(avatar)
    } else {
        line.child(avatar).child(column).child(Element::flex_spacer())
    }
}

/// 给头像、昵称挂上「跳到那台的会话」。用 `BubbleClick` 而不是 `clickable()`：
/// 后者自带 hover/press 叠层，会把气泡上的头像洗成一块按钮底色，原版没这个东西。
fn jump_to_conv(element: Element, id: u64, name: &str, tx: &Sender<Msg>) -> Element {
    let sender = tx.clone();
    element
        .widget(super::preview::BubbleClick::new())
        .tooltip(format!("打开与 {name} 的会话"))
        .on_click(move |_| {
            let _ = sender.send(Msg::Select(id));
        })
}

/// 单个气泡。靠头像那一角收小成「尾巴」，其余三角 16，与 `BubbleFrame::paintEvent` 一致。
///
/// 三种形态的底色不同：文本用纯气泡色，文件用预合成过 MD3 叠加色的文件色，
/// 图片**没有底**——原版 `ImageBubble` 调了 `setFillEnabled(false)`，缩略图自己就是形状。
fn bubble_card(bubble: Bubble, skin: Skin, tx: &Sender<Msg>) -> Element {
    let mine = bubble.mine;
    let (radius, tail) = (theme::BUBBLE_RADIUS, theme::BUBBLE_TAIL);
    let corners = if mine {
        Corners::new(radius, tail, radius, radius)
    } else {
        Corners::new(tail, radius, radius, radius)
    };
    let card = match &bubble.shape {
        Shape::Text(text) => Element::col()
            .padding_xy(theme::TEXT_PAD_X, theme::TEXT_PAD_Y)
            .bg(skin.bubble_fill(mine))
            .child(text_body(text, skin, mine)),
        Shape::File(card) => Element::col()
            .padding(5)
            .bg(skin.bubble_file_fill(mine))
            .child(file_body(card, skin, mine)),
        Shape::Image(card) => Element::col().child(image_body(card, tx)),
    }
    .corners(corners);
    // 右键菜单与悬停提示挂在气泡上：有底的两种形态本来就吞命中，图片形态的缩略图
    // 与占位块也是实心方块，所以不必额外 `clickable()`（那会带来按压叠层，原版没有）。
    // 左键预览另说：图片那一支整块挂了 `BubbleClick`（同样不画叠层），缩略图与占位块都能点。
    let menu_bubble = bubble.clone();
    let tip = bubble.tip.clone();
    let menu_tx = tx.clone();
    card.tooltip(tip).on_context_menu(move || bubble_menu(&menu_bubble, &menu_tx))
}

/// 文本气泡的正文。用富文本控件而非 label：为的是「拖选 + Ctrl+C 复制」，
/// 这也正是原版给文本气泡开 `IsTextSelectionEnabled` 的意图。
fn text_body(text: &str, skin: Skin, mine: bool) -> Element {
    Element::rich(RichDoc::new().para(text.to_owned()))
        .font_size(theme::FONT_BODY)
        .line_height(1.45)
        // 折行宽度得显式给：控件不会替我们消化「气泡上限减去内边距」
        .max_width(theme::BUBBLE_MAX_WIDTH - 2 * theme::TEXT_PAD_X)
        .fg(skin.bubble_text(mine))
}

/// 文件（文件夹）形态：白底图块 + 名称/大小/进度/状态胶囊。
///
/// 尺寸抄 `FileBubble`：外层气泡还有一圈 5 的内衬，所以这里的上下限各自减去 10，
/// 让成品正好落在 240~360。图标 52、两列间距 12、内边距 12/10、行距 4。
fn file_body(card: &FileCard, skin: Skin, mine: bool) -> Element {
    // 信息列不 `weight`：原版卡片的宽度是「内容决定、上下限 240~360」，
    // 挂上权重就变成「总是撑到上限」，一句短文件名也能占满一排。
    let mut info = Element::col()
        .spacing(theme::FILE_INFO_SPACING)
        .child(
            Element::label(card.name.clone())
                .font_size(theme::FILE_TEXT_FONT)
                .font_weight(600)
                .fg(skin.bubble_text(mine))
                .line_height(1.35)
                .max_lines(2)
                .truncate(Truncate::End),
        )
        .child(
            Element::label(card.size.clone())
                .font_size(theme::FILE_TEXT_FONT)
                .fg(skin.sub_text(mine))
                .max_lines(1),
        );
    if card.show_bar {
        info = info.child(pill_progress(card.progress, skin, mine));
    }
    if let Some(chip) = &card.chip {
        info = info.child(status_chip(chip, skin, mine));
    }
    Element::row()
        .min_width(theme::FILE_CARD_MIN_WIDTH - 10)
        .max_width(theme::FILE_CARD_MAX_WIDTH - 10)
        .padding_edges(Insets { left: theme::FILE_PAD_X, top: theme::FILE_PAD_Y, right: theme::FILE_PAD_X, bottom: theme::FILE_PAD_Y })
        .spacing(theme::FILE_GAP)
        .cross(Align::Center)
        .child(file_tile(card.is_dir, skin))
        .child(info)
}

/// 图片形态：160×160 圆角 12 的缩略图。图还没解码完（或解不出来、未落盘）时是深色占位块。
///
/// 占位块不看深浅主题：原版给的是半透黑底 + 白字（`updatePlaceholder`），
/// 因为它本来就盖在图上，两套主题都一样。
///
/// 解码任务由行构建时直接投后台队列（`ImageCard::of` → `assets::queue_thumb`），解完
/// 回包把那一行的 `ready` 翻真就换上图；没有视口探针、没有撤销，最早的朴素方案。
///
/// 缩略图到位后整块可点，占位块也能点：原版 `ImageBubble` 在构造里就把 `m_path` 定下来了，
/// `mousePressEvent` 只问「左键 + 路径非空」，于是传输中点一下也能预览（图还没落盘就是「无法加载图片」）。
/// 点击只发一条 `Msg::PreviewImage`，聚合当前列表的图片与开子窗都由状态机去做，
/// 对标 `imageClicked` → `openImagePreview`。路径为空（还没落盘的接收）就不接点击。
fn image_body(card: &ImageCard, tx: &Sender<Msg>) -> Element {
    let body = Element::stack()
        .size(theme::THUMB_SIZE, theme::THUMB_SIZE)
        .corner(theme::THUMB_RADIUS)
        .bg(if card.failed { theme::IMAGE_FAIL_BG } else { theme::IMAGE_HOLD_BG });
    let thumb = if card.thumb && card.ready {
        assets::thumbnail_element(&card.path, theme::THUMB_SIZE)
    } else {
        None
    };
    let body = match thumb {
        Some(thumb) => body.child(thumb),
        None => body.child(placeholder_text(card)),
    };
    if card.path.as_os_str().is_empty() {
        return body;
    }
    let sender = tx.clone();
    let path = card.path.clone();
    // 用 `BubbleClick` 而不是 `clickable()`：后者带 hover/press 叠层，原版按下不变色
    body.widget(super::preview::BubbleClick::new()).on_click(move |_| {
        let _ = sender.send(Msg::PreviewImage(path.clone()));
    })
}

/// 占位块上的那行字：传输中是「名称 + 百分比」，完成后是名称，失败时是失败文案。
fn placeholder_text(card: &ImageCard) -> Element {
    Element::label(card.placeholder.clone())
        .align_xy(Align::Center, Align::Center)
        .text_align(Align::Center)
        .max_width(theme::THUMB_SIZE - 16)
        .font_size(theme::FILE_TEXT_FONT)
        .line_height(1.4)
        .fg(if card.failed { theme::IMAGE_FAIL_FG } else { Color::WHITE })
}

/// 没有消息时的占位内容。上下各一个撑开位，让它落在可视区正中。
fn empty_state(ui: &Ui, skin: Skin) -> Element {
    Element::col()
        .fill()
        .cross(Align::Center)
        .spacing(8)
        .child(Element::flex_spacer())
        .child(assets::md_icon("chat_bubble", skin.icon_tint(), 38))
        .child(
            Element::label(ui.empty_title)
                .font_size(theme::FONT_TITLE)
                .font_weight(600)
                .fg_role(Role::Text),
        )
        .child(
            Element::label(ui.empty_hint)
                .font_size(theme::FONT_BODY)
                .fg_role(Role::TextMuted)
                .line_height(1.5)
                .text_align(Align::Center)
                .max_width(420),
        )
        .child(caption("把文件或文件夹拖进窗口也能直接发送"))
        .child(Element::flex_spacer())
}
