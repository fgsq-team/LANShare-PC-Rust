//! 界面构建函数：把状态机投影成 windui 元素树。
//!
//! 每个构建函数只收两样东西：`&Ui`（一组建好就交给控件的信号）与 `&Sender<Msg>`
//! （点击之类要发的消息）。这里不读 `ChatApp`，也不做任何业务决定——
//! 行的内容早在 [`state`] 里换算成纯数据了。

pub mod composer;
pub mod bottom_probe;
pub mod dialogs;
pub mod preview;
pub mod scroll_probe;
pub mod sidebar;
pub mod state;
pub mod thread;

use std::rc::Rc;

use windui::prelude::*;

use crate::app::Msg;
use crate::assets;
use crate::lanshare::nets::local_mode;
use crate::theme;
use crate::theme::Skin;
use state::{Bubble, Chip, ConvRow, MenuAction, Ui};

/// 根元素：左列表 + 右消息区，上面压八个对话框（接收确认、网页授权、网页二维码、自由复制、选设备发送、设置、关于、更新提示），
/// 整窗接收文件拖放。
///
/// 对话框必须挂在 `stack` 里而不是 `row` 的子节点：`Element::dialog` 自带撑满的遮罩，
/// 放进线性容器会跟着挤一排的位置。
///
/// `skin` 由根节点的 `host_signal(ui.skin, …)` 传进来：消息区里那些按配色档案烘进去的
/// 定色（气泡底、正文、进度条、状态胶囊）只能靠重建整棵树来刷新。
pub fn root(ui: &Ui, tx: &Sender<Msg>, skin: Skin) -> Element {
    let sender = tx.clone();
    Element::stack()
        .fill()
        .child(
            Element::row()
                .fill()
                .cross(Align::Stretch)
                .child(sidebar::build(ui, tx, skin))
                .child(thread::build(ui, tx, skin).weight(1.0)),
        )
        .child(dialogs::offer(ui, tx))
        .child(dialogs::grant(ui, tx))
        .child(dialogs::qr(ui, tx))
        .child(dialogs::free_copy(ui, tx))
        .child(dialogs::picker(ui, tx, skin))
        .child(dialogs::settings(ui, tx))
        .child(dialogs::about(ui, tx))
        .child(dialogs::update(ui, tx))
        .on_drop_files(move |ctx, paths| {
            if paths.is_empty() {
                return;
            }
            let _ = sender.send(Msg::SendPaths(paths.to_vec()));
            let _ = ctx;
        })
}

/// 次要说明文字：单行、截断、走主题的弱化色。
pub fn caption(text: impl Into<TextContent>) -> Element {
    Element::label(text)
        .font_size(theme::FONT_META)
        .fg_role(Role::TextMuted)
        .max_lines(1)
}

/// 两行都能折的说明文字，用于横幅与空状态。
pub fn paragraph(text: impl Into<TextContent>) -> Element {
    Element::label(text).font_size(theme::FONT_META).fg_role(Role::TextMuted).line_height(1.5)
}

/// 工具条上的 MD 矢量图标按钮：`name` 是图标名，颜色随配色档案。
pub fn icon_button(name: &str, tooltip: &str, message: Msg, tx: &Sender<Msg>, skin: Skin) -> Element {
    let sender = tx.clone();
    Element::icon_button_content(assets::md_icon_content(name, skin.icon_tint()))
        .tooltip(tooltip.to_owned())
        .on_click(move |_| {
            let _ = sender.send(message.clone());
        })
}

/// 一枚弹出菜单的入口按钮：左击在按钮正下方弹，右击在指针处弹。
///
/// 菜单项每次弹之前现取（`on_context_menu` 的构建器是 `Fn`，且要求可重入），
/// 所以把生成器包成 `Rc` 给两条路径各挂一份引用。
pub fn menu_button(
    name: &str,
    tooltip: &str,
    tx: &Sender<Msg>,
    make: impl Fn(&Sender<Msg>) -> Vec<MenuItem> + 'static,
    skin: Skin,
) -> Element {
    let make = Rc::new(make);
    let for_click = Rc::clone(&make);
    let for_menu = Rc::clone(&make);
    let click_tx = tx.clone();
    let menu_tx = tx.clone();
    Element::icon_button_content(assets::md_icon_content(name, skin.icon_tint()))
        .tooltip(tooltip.to_owned())
        .on_click(move |ctx| {
            let items = for_click(&click_tx);
            ctx.show_menu_request(MenuRequest::below(ctx.bounds(), items));
        })
        .on_context_menu(move || for_menu(&menu_tx))
}

/// 一行菜单项：标签由动作决定，前置位图图标用同一套 MD 矢量（windui 按文字色着色），
/// 动作本身发一条消息。
fn menu_item(action: MenuAction, message: Msg, tx: &Sender<Msg>) -> MenuItem {
    let sender = tx.clone();
    let item = MenuItem::run(action.label(), move |_| {
        let _ = sender.send(message.clone());
    }, false);
    let item = match assets::md_icon_image(action.icon()) {
        Some(icon) => item.icon_image(icon),
        None => item,
    };
    match action {
        MenuAction::Delete | MenuAction::ClearChat => item.danger(),
        _ => item,
    }
}

/// 侧栏会话行的右键菜单。
pub fn conv_menu(row: &ConvRow, tx: &Sender<Msg>) -> Vec<MenuItem> {
    build_menu(&row.actions, tx, |action| match action {
        MenuAction::MarkRead => Msg::MarkRead(row.id),
        MenuAction::OpenConversation => Msg::Select(row.id),
        MenuAction::ClearChat => Msg::ClearChat(row.id),
        other => Msg::Notice(format!("不支持的操作：{}", other.label())),
    })
}

/// 会话标题栏的菜单。`merged` 为合并态时没有「当前会话」可言，改给批量已读 / 刷新 / 设置 / 关于；
/// `conv` 为 `None`（一台会话都没有）时也不收那两个按号找会话的动作。
///
/// 收掉而不是留个点了没反应的项：菜单项亮着却落空只会让人以为按钮坏了。
pub fn header_menu(conv: Option<u64>, merged: bool, tx: &Sender<Msg>) -> Vec<MenuItem> {
    if merged {
        return [MenuAction::MarkAll, MenuAction::Refresh, MenuAction::Settings, MenuAction::About]
            .into_iter()
            .map(|action| {
                let message = match action {
                    MenuAction::MarkAll => Msg::MarkAll,
                    MenuAction::Refresh => Msg::Refresh,
                    MenuAction::About => Msg::OpenAbout,
                    _ => Msg::OpenSettings,
                };
                menu_item(action, message, tx)
            })
            .collect();
    }
    let items: Vec<(MenuAction, Msg)> = match conv {
        Some(id) => vec![
            (MenuAction::MarkRead, Msg::MarkRead(id)),
            (MenuAction::Refresh, Msg::Refresh),
            (MenuAction::ClearChat, Msg::ClearChat(id)),
            (MenuAction::Settings, Msg::OpenSettings),
            (MenuAction::About, Msg::OpenAbout),
        ],
        None => vec![
            (MenuAction::Refresh, Msg::Refresh),
            (MenuAction::Settings, Msg::OpenSettings),
            (MenuAction::About, Msg::OpenAbout),
        ],
    };
    items.into_iter().map(|(action, message)| menu_item(action, message, tx)).collect()
}

/// 侧栏底部的批量操作菜单。
pub fn footer_menu(tx: &Sender<Msg>) -> Vec<MenuItem> {
    build_menu(
        &[MenuAction::MarkAll, MenuAction::Refresh, MenuAction::Settings, MenuAction::About],
        tx,
        |action| match action {
            MenuAction::MarkAll => Msg::MarkAll,
            MenuAction::Refresh => Msg::Refresh,
            MenuAction::About => Msg::OpenAbout,
            _ => Msg::OpenSettings,
        },
    )
}

/// 气泡的菜单。动作列表在 `state` 里按方向与进展算好了，这里只负责翻成消息。
pub fn bubble_menu(bubble: &Bubble, tx: &Sender<Msg>) -> Vec<MenuItem> {
    build_menu(&bubble.actions, tx, |action| {
        let id = bubble.file_id.clone();
        match action {
            MenuAction::Cancel => Msg::Cancel(id),
            MenuAction::Resend => Msg::Resend(id),
            MenuAction::Open => Msg::OpenFile(bubble.path.clone()),
            MenuAction::OpenPath => Msg::OpenPath(bubble.path.clone()),
            MenuAction::CopyText => Msg::Copy(bubble.text.clone()),
            MenuAction::FreeCopyText => Msg::FreeCopyText(bubble.text.clone()),
            MenuAction::Delete => Msg::DeleteMessage(bubble.id),
            other => Msg::Notice(format!("不支持的操作：{}", other.label())),
        }
    })
}

fn build_menu(actions: &[MenuAction], tx: &Sender<Msg>, mut make: impl FnMut(MenuAction) -> Msg) -> Vec<MenuItem> {
    actions.iter().map(|action| menu_item(*action, make(*action), tx)).collect()
}

/// 圆形头像：MD3 的淡主色圆底 + 居中的设备图标，右下角可叠一个在线状态点。
///
/// 图标尺寸按 `AVATAR_ICON / AVATAR_SIZE` 的比例随头像一起缩放，与原版
/// `avatarPixmap` 的 40 / 24 一致；底图不再是「按会话取色 + 首字母」，因为原版
/// 根本不走字母那套——它靠设备类型图标一眼区分 Windows 与手机。
pub fn device_avatar(mode: i32, size: i32, with_status: bool, online: bool, skin: Skin) -> Element {
    let icon = size * theme::AVATAR_ICON / theme::AVATAR_SIZE;
    let mut stack = Element::stack()
        .size(size, size)
        .child(Element::leaf().fill().bg(skin.avatar_bg()).corner(size as f32 / 2.0))
        .child(Element::image_bytes(assets::device_icon(mode)).size(icon, icon).align(Align::Center));
    if with_status {
        let dot = (size / 3).max(8);
        stack = stack.child(
            Element::leaf()
                .size(dot, dot)
                .align_xy(Align::End, Align::End)
                .corner(dot as f32 / 2.0)
                .bg(if online { theme::online() } else { theme::offline() }),
        );
    }
    stack
}

/// 「我」的头像：本机设备类型决定图标，与原版一样不给自己单独配色。
pub fn self_avatar(size: i32, skin: Skin) -> Element {
    device_avatar(local_mode(), size, false, true, skin)
}

/// 文件（文件夹）图块：白底 + 主色 glyph，尺寸取 `FILE_ICON`。
pub fn file_tile(is_dir: bool, skin: Skin) -> Element {
    assets::file_tile(is_dir, theme::FILE_ICON, skin.primary)
}

/// 状态胶囊：半透明底 + 圆形徽标 + 文字，几何抄 `FileBubble` 的 `kChip*`。
pub fn status_chip(chip: &Chip, skin: Skin, mine: bool) -> Element {
    let fill = if chip.ok { skin.success } else { skin.danger };
    Element::row()
        .height(theme::CHIP_HEIGHT)
        .padding_edges(Insets { left: theme::CHIP_PAD_LEFT, top: 0, right: theme::CHIP_PAD_RIGHT, bottom: 0 })
        .spacing(theme::CHIP_GAP)
        .cross(Align::Center)
        .bg(skin.chip_bg(mine))
        .corner(theme::CHIP_RADIUS)
        .child(assets::status_badge(chip.ok, fill, theme::CHIP_ICON))
        .child(
            Element::label(chip.text.clone())
                .font_size(theme::FILE_TEXT_FONT)
                .font_weight(500)
                .fg(skin.bubble_text(mine)),
        )
}

/// 自绘胶囊进度条。
///
/// 不用 `Element::progress`：它的 `measure` 写死 160x10，颜色只能从主题的
/// `progress` 档取、没法按气泡方向分白/绿，而原版要的是高 8、上限宽 160、
/// 右侧白色左侧成功绿。两条 `leaf` 叠起来就能逐元素控色，代价是不再有动画。
pub fn pill_progress(progress: f32, skin: Skin, mine: bool) -> Element {
    let height = theme::PROGRESS_HEIGHT;
    let radius = height as f32 / 2.0;
    // 填充短于高度时兜底到一个圆点，否则圆角会被压成扁方块
    let filled = ((theme::PROGRESS_MAX_WIDTH as f32 * progress.clamp(0.0, 1.0)).round() as i32).max(height);
    Element::stack()
        .size(theme::PROGRESS_MAX_WIDTH, height)
        .child(Element::leaf().fill().bg(theme::progress_track(mine)).corner(radius))
        .child(Element::leaf().size(filled, height).bg(skin.progress_chunk(mine)).corner(radius))
}
