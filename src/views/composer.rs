//! 底部输入区：照抄原版 `LANShareWindow.ui` 的下三段——图标按钮排、大输入框、右下发送。
//!
//! 原版从上到下是「开启网页功能」复选行、`selectFiles` / `selectDirectories` 按钮排
//! （高 35~40）、150 高的 `textEdit`、浮在输入区右下的 81×23 `sendButton`。前两项
//! （网页服务、按设备群发）在 Rust 引擎里没有对应通路，所以那一行换成了表情与
//! 「发到剪贴板」开关——都是本移植真能做的事。
//!
//! 发送快捷键是 **Ctrl+Enter**：多行输入框的裸 Enter 归「换行」，Ctrl+Enter 才走
//! `on_submit`。挂了 `on_submit` 之后这组按键被控件消费，不会再冒到 `App::on_shortcut`，
//! 所以不必（也不能）在那边再兜一遍底。

use windui::prelude::*;

use crate::app::{EMOJIS, Msg};
use crate::assets;
use crate::theme;
use crate::theme::Skin;
use super::state::Ui;
use super::caption;

/// 输入区整体：按钮排 + 输入框 + 提示与发送按钮。
pub fn build(ui: &Ui, tx: &Sender<Msg>, skin: Skin) -> Element {
    Element::col()
        .width_match()
        .spacing(6)
        .padding_edges(Insets::new(12, 8, 12, 12))
        .border_role(Role::Border, 1)
        .border_edges(Edges::TOP)
        .child(toolbar(ui, tx, skin))
        .child(input(ui, tx))
        .child(footer(ui, tx))
}

/// 按钮排：左边表情与两个选择按钮，右边「发到剪贴板」开关。
fn toolbar(ui: &Ui, tx: &Sender<Msg>, skin: Skin) -> Element {
    Element::row()
        .width_match()
        .spacing(6)
        .cross(Align::Center)
        .child(emoji_button(ui, tx, skin))
        .child(tool_button("选择文件", assets::ICON_FILE, Msg::PickFiles, tx))
        .child(tool_button("选择文件夹", assets::ICON_FOLDER, Msg::PickFolder, tx))
        .child(Element::flex_spacer())
        .child(clipboard_toggle(ui, tx))
}

/// 带前置图标的文字按钮——图标就是原版 `:/img/*.png` 那几张，直接内嵌复用。
fn tool_button(label: &str, icon: &'static [u8], message: Msg, tx: &Sender<Msg>) -> Element {
    let sender = tx.clone();
    Element::button(label.to_owned())
        .icon_bytes(icon)
        .height(34)
        .on_click(move |_| {
            let _ = sender.send(message.clone());
        })
}

/// 多行输入框。原版给的是 150 的固定高，本窗口矮一些，取 `COMPOSER_HEIGHT`。
fn input(ui: &Ui, tx: &Sender<Msg>) -> Element {
    let sender = tx.clone();
    Element::text_input(ui.draft, ui.placeholder)
        .multiline()
        .wrap(true)
        .width_match()
        .height(theme::COMPOSER_HEIGHT)
        .focus_when(ui.focus_draft)
        .on_submit(move |_| {
            let _ = sender.send(Msg::Send);
        })
}

/// 输入框下面一行：左边提示与字数，右边发送按钮（原版 `sendButton` 的位置）。
fn footer(ui: &Ui, tx: &Sender<Msg>) -> Element {
    Element::row()
        .width_match()
        .spacing(8)
        .cross(Align::Center)
        .child(
            caption("Ctrl+Enter 发送 · Enter 换行 · 把文件或文件夹拖到窗口即可发送 · 气泡里的文字可以选中复制")
                .max_lines(2)
                .weight(1.0),
        )
        .child(
            Element::label_signal(ui.counter)
                .font_size(theme::FONT_SMALL)
                .fg_role(Role::TextMuted),
        )
        .child(send_button(ui, tx))
}

/// 发送按钮：草稿为空或没选设备就禁用。
fn send_button(ui: &Ui, tx: &Sender<Msg>) -> Element {
    let sender = tx.clone();
    Element::button("发送")
        .icon_bytes(assets::ICON_SEND)
        .tooltip("发送（Ctrl+Enter）")
        .min_width(81)
        .height(30)
        .enabled_signal(ui.can_send)
        .on_click(move |_| {
            let _ = sender.send(Msg::Send);
        })
}

/// 表情按钮：点了开，再点关；面板是锚在按钮上方的浮层，选完自动收。
fn emoji_button(ui: &Ui, tx: &Sender<Msg>, skin: Skin) -> Element {
    let open = ui.emoji_open;
    Element::icon_button_content(assets::md_icon_content("mood", skin.icon_tint()))
        .tooltip("插入表情")
        .height(34)
        .on_click(move |_| open.update(|on| *on = !*on))
        .popup(open, emoji_panel(ui, tx))
}

/// 表情面板：12 个常用表情，排成 4 列。
fn emoji_panel(ui: &Ui, tx: &Sender<Msg>) -> Element {
    let open = ui.emoji_open;
    let emojis = EMOJIS;
    let cells: Vec<Element> = emojis
        .iter()
        .map(|emoji| {
            let sender = tx.clone();
            let text = emoji.to_string();
            let insert = text.clone();
            Element::button(text)
                .font_size(18.0)
                .height(36)
                .intent(Intent::Neutral)
                .on_click(move |_| {
                    let _ = sender.send(Msg::InsertText(insert.clone()));
                    // 选完就走，面板收起
                    open.set(false);
                })
        })
        .collect();
    Element::col()
        // 浮层不限定宽就会被 `grid` 的等分列撑到整窗宽，这里钉住一列 ~64px
        .width(296)
        .padding(8)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(theme::RADIUS_CARD)
        .child(Element::grid(4, 2, cells))
}

/// 「发到对方剪贴板」开关：打开后下一条文本走 UDP 1005。
fn clipboard_toggle(ui: &Ui, tx: &Sender<Msg>) -> Element {
    let sender = tx.clone();
    Element::row()
        .spacing(6)
        .cross(Align::Center)
        .child(
            Element::switch(ui.clipboard).on_switch_change(move |_, on| {
                let _ = sender.send(Msg::Clipboard(on));
            }),
        )
        .child(caption("发到剪贴板"))
}
