//! 左侧栏：账号区、本机地址横幅、搜索框、设备（会话）列表、状态条。
//!
//! 列表是键控的（[`Element::list_signal`]）：一行的全部外观都来自那一条 [`ConvRow`]，
//! 不在线读别的状态。这样数据没变的行原样留着节点，只有真的变了的行才重建。

use windui::prelude::*;

use crate::app::Msg;
use crate::assets;
use crate::theme;
use crate::theme::Skin;
use super::state::{ConvRow, Ui};
use super::{caption, conv_menu, device_avatar, footer_menu, icon_button, menu_button, self_avatar};

/// 侧栏整体。宽度在这里定，根容器只负责把它排到最左边。
pub fn build(ui: &Ui, tx: &Sender<Msg>, skin: Skin) -> Element {
    Element::col()
        .fill()
        .width(theme::SIDEBAR_WIDTH)
        .padding(12)
        .spacing(10)
        .border_role(Role::Border, 1)
        .border_edges(Edges::RIGHT)
        .child(account(ui, tx, skin))
        .child(mode_switch(ui))
        .child(search(ui, skin))
        .child(list(ui, tx, skin))
        .child(footer(ui, tx, skin))
}

/// 聊天列表模式切换：「全部设备」（合并流 + 群发）/「分设备」（按设备收发）。
///
/// 分段控件没有回调、只写 `ui.chat_mode` 信号，泵按版本轮询抄回状态机（与网页服务勾选框同一条约定）。
fn mode_switch(ui: &Ui) -> Element {
    Element::segmented(vec!["全部设备", "分设备"], ui.chat_mode).width_match()
}

/// 侧栏顶部信息卡：本机头像 + 标题 + 动作图标，下面压「本机活动地址」与「网页版地址」两行。
///
/// 只显活动那块网卡的 `ip:tcp端口`（不再罗列全部、不带掩码与协议版本）；两条地址都可拖选
/// 复制、也各带一个一键复制按钮。多网卡全集仍可在二维码对话框与设置里查看。
fn account(ui: &Ui, tx: &Sender<Msg>, skin: Skin) -> Element {
    // 地址文本用 `rich` 而不是 `label`：为的是「拖选 + Ctrl+C 复制」。`_signal` 版随信号更新，
    // 换网卡 / 开关网页服务时这一行跟着变，不会被建成时就烘进去的旧值拖住。
    let addr = ui.banner.map(|text| RichDoc::new().para(text.clone()));
    let url = ui.web_url.map(|text| RichDoc::new().para(text.clone()));
    let web_on = ui.web_url.map(|url| !url.is_empty());
    Element::col()
        .width_match()
        .padding_xy(10, 8)
        .spacing(8)
        .bg_role(Role::Surface)
        .border_role(Role::Border, 1)
        .corner(theme::RADIUS_CARD)
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(8)
                .child(self_avatar(36, skin))
                .child(
                    Element::label("局域网设备")
                        .font_size(theme::FONT_TITLE)
                        .font_weight(700)
                        .fg_role(Role::Text)
                        .max_lines(1)
                        .truncate(Truncate::End)
                        .weight(1.0),
                )
                .child(icon_button("refresh", "重新枚举本机网卡", Msg::Refresh, tx, skin))
                .child(icon_button("dark_mode", "循环切换主题（跟随系统 / 浅色 / 深色 / 祖母绿）", Msg::CycleTheme, tx, skin))
                .child(icon_button("settings", "设置（主题与网络参数）", Msg::OpenSettings, tx, skin)),
        )
        .child(Element::leaf().width_match().height(1).bg_role(Role::Border))
        // 本机活动地址：`ip:tcp端口`，对端连进来用的就是它。
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(6)
                .child(caption("本机"))
                .child(
                    Element::rich_signal(addr)
                        .font_size(theme::FONT_BODY)
                        .fg_role(Role::Text)
                        .line_height(1.2)
                        .weight(1.0),
                )
                .child(copy_button("复制本机地址", ui.banner, tx, skin)),
        )
        // 网页服务开关 + 二维码入口（关了网页服务时也要有个进得去对话框的入口）。
        .child(
            Element::row()
                .width_match()
                .spacing(6)
                .cross(Align::Center)
                .child(Element::checkbox("开启网页功能", ui.web_service).weight(1.0))
                .child(icon_button("qr_code", "网页二维码（手机扫码打开网页版）", Msg::OpenQr, tx, skin)),
        )
        // 网页版地址：开了网页服务才显；同样可拖选 + 一键复制。
        .child(
            Element::row()
                .width_match()
                .cross(Align::Center)
                .spacing(6)
                .child(
                    Element::rich_signal(url)
                        .font_size(theme::FONT_SMALL)
                        .fg_role(Role::TextMuted)
                        .line_height(1.2)
                        .weight(1.0),
                )
                .child(copy_button("复制网页地址", ui.web_url, tx, skin))
                .visible_signal(web_on),
        )
}

/// 一键复制按钮：按下时才从信号现取文本，避免把建成时的旧地址烘死；空文本不发消息。
fn copy_button(tip: &str, src: Signal<String>, tx: &Sender<Msg>, skin: Skin) -> Element {
    let sender = tx.clone();
    Element::icon_button_content(assets::md_icon_content("content_copy", skin.icon_tint()))
        .tooltip(tip.to_owned())
        .on_click(move |_| {
            let text = src.get();
            if !text.is_empty() {
                let _ = sender.send(Msg::Copy(text));
            }
        })
}

/// 搜索框：一枚 MD 放大镜矢量图标 + 输入框。windui 的 `leading_icon` 只收字符，
/// 不内嵌矢量，所以改成行布局把图标排在输入框左侧。Ctrl+L 由 `App::on_shortcut` 送焦点到这里。
fn search(ui: &Ui, skin: Skin) -> Element {
    Element::row()
        .width_match()
        .spacing(6)
        .cross(Align::Center)
        .child(assets::md_icon("search", skin.icon_tint(), 18))
        .child(
            Element::text_input(ui.query, "按名称、地址或内容过滤设备")
                .width_match()
                .focus_select_all_when(ui.focus_search),
        )
}

/// 设备列表：在线优先，同在线按设备 IP 数值升序。空列表时压一段说明。
fn list(ui: &Ui, tx: &Sender<Msg>, skin: Skin) -> Element {
    let sender = tx.clone();
    let rows = Element::list_signal(ui.convs, |row| row.id, move |row| row_of(row, &sender, skin));
    let empty = ui.convs.map(|rows| rows.is_empty());
    Element::stack()
        .width_match()
        .weight(1.0)
        .child(rows)
        .child(empty_hint().visible_signal(empty))
}

/// 一台设备行。选中底色的深浅交给主题的强调色，不写死颜色。
fn row_of(row: ConvRow, tx: &Sender<Msg>, skin: Skin) -> Element {
    let id = row.id;
    let click_tx = tx.clone();
    let menu_tx = tx.clone();
    let menu = row.clone();
    let button_menu = menu.clone();
    let online = row.online;
    let selected = row.selected;
    let subtitle_role = if online { Role::TextMuted } else { Role::TextDisabled };

    Element::col()
        .width_match()
        .padding(8)
        .spacing(2)
        .corner(theme::RADIUS_CARD)
        .bg_role_alpha(Role::Accent, if selected { 0.16 } else { 0.0 })
        .clickable()
        .on_click(move |_| {
            let _ = click_tx.send(Msg::Select(id));
        })
        .on_context_menu(move || conv_menu(&menu, &menu_tx))
        .child(
            Element::row()
                .width_match()
                .spacing(10)
                .cross(Align::Center)
                .child(device_avatar(row.mode, theme::AVATAR_SIZE, true, online, skin))
                .child(
                    Element::col()
                        .weight(1.0)
                        .spacing(2)
                        .child(title_line(&row))
                        .child(subtitle_line(&row, skin, subtitle_role))
                        .child(preview(&row)),
                )
                .child(menu_button("more_vert", "设备操作", tx, move |tx| conv_menu(&button_menu, tx), skin)),
        )
}

/// 第一行：名字 + 未读角标 + 时间。设备类型不写在这里——头像里的图标已经说了。
fn title_line(row: &ConvRow) -> Element {
    Element::row()
        .width_match()
        .spacing(6)
        .cross(Align::Center)
        .child(
            Element::label(row.name.clone())
                .font_size(theme::FONT_BODY)
                .font_weight(600)
                .fg_role(Role::Text)
                .max_lines(1)
                .truncate(Truncate::End)
                .weight(1.0),
        )
        .child(Element::badge(row.unread.to_string()).visible(row.unread > 0))
        .child(caption(row.time.clone()))
}

/// 预览行：最后一条消息的一句话摘要。
fn preview(row: &ConvRow) -> Element {
    Element::label(row.preview.clone())
        .font_size(theme::FONT_META)
        .fg_role(Role::TextMuted)
        .max_lines(1)
        .truncate(Truncate::End)
}

/// 副标题行：充电时前置一枚 MD 电量矢量图标，后接地址 · 电量百分比文本。
fn subtitle_line(row: &ConvRow, skin: Skin, role: Role) -> Element {
    Element::row()
        .width_match()
        .spacing(4)
        .cross(Align::Center)
        .child(assets::md_icon("battery_charging_full", skin.icon_tint(), 14).visible(row.charging))
        .child(
            Element::label(row.subtitle.clone())
                .font_size(theme::FONT_SMALL)
                .fg_role(role)
                .max_lines(1)
                .truncate(Truncate::End)
                .weight(1.0),
        )
}

/// 底部状态条与批量操作入口。
fn footer(ui: &Ui, tx: &Sender<Msg>, skin: Skin) -> Element {
    Element::row()
        .width_match()
        .spacing(6)
        .cross(Align::Center)
        .child(
            Element::label_signal(ui.status_line)
                .font_size(theme::FONT_SMALL)
                .fg_role(Role::TextMuted)
                .weight(1.0)
                .line_height(1.4),
        )
        .child(menu_button("more_vert", "批量操作", tx, footer_menu, skin))
}

/// 一个设备都没发现时的说明。
fn empty_hint() -> Element {
    Element::col()
        .fill()
        .cross(Align::Center)
        .spacing(8)
        .padding(8)
        .child(
            Element::label("还没有发现设备")
                .font_size(theme::FONT_BODY)
                .font_weight(600)
                .fg_role(Role::Text),
        )
        .child(
            Element::label("确认对方与本机等在同一网段，并开着 UDP 4573 / TCP 5856。")
                .font_size(theme::FONT_META)
                .fg_role(Role::TextMuted)
                .line_height(1.5)
                .text_align(Align::Center)
                .width_match(),
        )
}
