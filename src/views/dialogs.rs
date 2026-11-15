//! 对话框：接收确认、网页授权、二维码、自由复制、选设备发送、设置、关于与更新提示。
//!
//! 这些都挂在根元素的 `stack` 上：[`Element::dialog`] 的遮罩是撑满整窗的，
//! 放进线性容器会跟着挤一排的位置。面板必须自带底色，否则空白处会穿透遮罩。

use windui::prelude::*;

use crate::app::Msg;
use crate::assets;
use crate::theme;
use crate::theme::Skin;
use super::state::{PickRow, Ui};
use super::caption;
use super::device_avatar;

/// 接收确认：列出待发文件与总大小。拒绝时引擎会回 `1103`。
///
/// 只给「接收 / 拒绝」两个出口：按 ESC 顶多把遮罩收起，状态机里那条请求还在，
/// 下一次同步会把它重新摆回来，不会被误当成拒绝。
pub fn offer(ui: &Ui, tx: &Sender<Msg>) -> Element {
    let key = ui.offer_key;
    Element::dialog(
        ui.offer_open,
        Element::col()
            .width(460)
            .padding(24)
            .spacing(14)
            .bg_role(Role::Surface)
            .corner(theme::RADIUS_CARD * 2.0)
            .child(
                Element::label(ui.offer_title)
                    .font_size(theme::FONT_TITLE)
                    .font_weight(600)
                    .fg_role(Role::Text)
                    .width_match(),
            )
            .child(
                Element::scroll()
                    .width_match()
                    .max_height(240)
                    .child(Element::label(ui.offer_body).font_size(theme::FONT_META).fg_role(Role::TextSubtle).line_height(1.6)),
            )
            .child(
                Element::row()
                    .width_match()
                    .spacing(8)
                    .child(Element::flex_spacer())
                    .child(press("拒绝", false, false, key, tx))
                    .child(press("接收", true, true, key, tx)),
            ),
    )
}

/// 对话框底部的「接收 / 拒绝」按钮。`key` 在点击时现取：对话框节点只建一次，
/// 而待确认请求的号是一条一条换的。
fn press(label: &str, accept: bool, primary: bool, key: Signal<u64>, tx: &Sender<Msg>) -> Element {
    let sender = tx.clone();
    let mut button = Element::button(label.to_owned()).height(38);
    if !primary {
        button = button.intent(Intent::Neutral);
    }
    button.on_click(move |_| {
        let _ = sender.send(Msg::Resolve { key: key.get(), accept });
    })
}

/// 网页客户端授权：对应原版 `LANShareWindow::newWebClient()`。默认「不同意」，
/// 而且按 ESC 把遮罩收起来不算答案：那条请求还在队列里，下一次同步会重新摆回来。
pub fn grant(ui: &Ui, tx: &Sender<Msg>) -> Element {
    let token = ui.grant_token;
    Element::dialog(
        ui.grant_open,
        Element::col()
            .width(460)
            .padding(24)
            .spacing(14)
            .bg_role(Role::Surface)
            .corner(theme::RADIUS_CARD * 2.0)
            .child(
                Element::label("有新的客户端访问")
                    .font_size(theme::FONT_TITLE)
                    .font_weight(600)
                    .fg_role(Role::Text)
                    .width_match(),
            )
            .child(
                Element::label(ui.grant_body)
                    .font_size(theme::FONT_META)
                    .fg_role(Role::TextSubtle)
                    .line_height(1.6)
                    .width_match(),
            )
            .child(
                Element::row()
                    .width_match()
                    .spacing(8)
                    .child(Element::flex_spacer())
                    .child(grant_press("不同意", false, token, tx))
                    .child(grant_press("同意", true, token, tx)),
            ),
    )
}

/// 授权框底下的「同意 / 不同意」。令牌在点击时现取：对话框节点只建一次，
/// 而排队的问题是一条一条换的。
fn grant_press(label: &str, accept: bool, token: Signal<String>, tx: &Sender<Msg>) -> Element {
    let sender = tx.clone();
    let mut button = Element::button(label.to_owned()).height(38);
    if !accept {
        button = button.intent(Intent::Neutral);
    }
    button.on_click(move |_| {
        let _ = sender.send(Msg::ResolveWebGrant { token: token.get(), accept });
    })
}

/// 网页二维码：对应原版 `WebUrlQRDialog`。网卡下拉、那张码、底下的地址行，
/// 加「刷新 / 关闭」两个按钮。
pub fn qr(ui: &Ui, tx: &Sender<Msg>) -> Element {
    let close_tx = tx.clone();
    Element::dialog_panel(
        ui.qr_open,
        "网页二维码",
        360,
        move |_| {
            let _ = close_tx.send(Msg::CloseQr);
        },
        qr_body(ui),
        qr_footer(tx),
    )
}

/// 二维码对话框正文：一句说明 → 网卡选择（多网卡才显）→ 居中的白底二维码与地址 → 未开启时的提醒。
///
/// 重点放在那张码上：把图与地址居中成一块，周围用留白拉开；单一网卡时不再摆一行下拉。
fn qr_body(ui: &Ui) -> Element {
    let side = crate::web::qr::DIALOG_SIDE as i32;
    // 图与那句失败文案叠在同一格里，换网卡时不会跳一下
    let shown = ui.qr_error.map(|error| error.is_empty());
    let hidden = ui.qr_error.map(|error| !error.is_empty());
    let image = ui.qr_svg.map(|svg| vec![svg.clone()]);
    // 只有一块网卡时那行下拉属于噪音，藏掉；多网卡才需要选。
    let multi = ui.qr_ips.map(|ips| ips.len() > 1);
    // 网页服务未勾选时码照样出，但在地址底下提醒一句（不进地址，地址才能整段复制）。
    let web_off = ui.web_service.map(|on| !on);
    // 地址用富文本：拖选高亮 + Ctrl+C，与侧栏那两块一致。
    let url = ui.qr_url.map(|text| RichDoc::new().para(text.clone()));
    Element::col()
        .width_match()
        .spacing(14)
        .child(
            Element::label("用手机相机或浏览器扫描下方二维码，即可打开电脑上的 LANShare 网页版。")
                .font_size(theme::FONT_META)
                .fg_role(Role::TextMuted)
                .line_height(1.55)
                .text_align(Align::Center)
                .width_match(),
        )
        .child(
            Element::field("网卡", Element::dropdown_signal(ui.qr_ips, ui.qr_index).weight(1.0))
                .visible_signal(multi),
        )
        // 居中一块：白底卡片托二维码（深浅主题都保证码落在亮底上、扫得出），底下是地址与提醒。
        .child(
            Element::col()
                .width_match()
                .cross(Align::Center)
                .spacing(10)
                .child(
                    Element::col()
                        .padding(12)
                        .bg(Color::WHITE)
                        .border_role(Role::Border, 1)
                        .corner(theme::RADIUS_CARD)
                        .child(
                            Element::stack()
                                .width(side)
                                .height(side)
                                // `target_width=None`：拿 SVG 自己的 240 当逻辑尺寸，按实际物理尺寸光栅，
                                // 高 DPI 下不会多一次重采样——二维码的模块边不直就会扫不出来
                                .child(
                                    Element::host_signal(image, move |svg: String| {
                                        Element::image_svg(svg.as_bytes(), None).size(side, side)
                                    })
                                    .visible_signal(shown),
                                )
                                .child(
                                    Element::label(ui.qr_error)
                                        .font_size(theme::FONT_META)
                                        .fg_role(Role::TextMuted)
                                        .text_align(Align::Center)
                                        .width(side)
                                        .height(side)
                                        .visible_signal(hidden),
                                ),
                        ),
                )
                .child(
                    Element::rich_signal(url)
                        .font_size(theme::FONT_SMALL)
                        .fg_role(Role::TextSubtle),
                )
                .child(
                    Element::label("未开启网页功能，请先在主页勾选“开启网页功能”后再扫码访问。")
                        .font_size(theme::FONT_META)
                        .fg_role(Role::Warning)
                        .text_align(Align::Center)
                        .line_height(1.5)
                        .width_match()
                        .visible_signal(web_off),
                ),
        )
}

/// 二维码对话框底栏。
fn qr_footer(tx: &Sender<Msg>) -> Element {
    let refresh = tx.clone();
    let close = tx.clone();
    Element::row()
        .width_match()
        .spacing(8)
        .child(Element::flex_spacer())
        .child(
            Element::button("刷新")
                .height(38)
                .intent(Intent::Neutral)
                .on_click(move |_| {
                    let _ = refresh.send(Msg::RefreshQr);
                }),
        )
        .child(
            Element::button("关闭")
                .height(38)
                .on_click(move |_| {
                    let _ = close.send(Msg::CloseQr);
                }),
        )
}

/// 自由复制文本：对应原版 `CopyableTextDialog`。只读多行输入框（可拖选、Ctrl+A/Ctrl+C），
/// 底下一枚「复制全部文本」，复制后提示并关掉。
pub fn free_copy(ui: &Ui, tx: &Sender<Msg>) -> Element {
    let close_tx = tx.clone();
    Element::dialog_panel(
        ui.free_copy_open,
        "自由复制文本",
        460,
        move |_| {
            let _ = close_tx.send(Msg::CloseFreeCopy);
        },
        Element::text_input(ui.free_copy_text, "")
            .multiline()
            .wrap(true)
            .read_only()
            .width_match()
            .height(240),
        free_copy_footer(tx),
    )
}

/// 自由复制对话框底栏：只有一枚「复制全部文本」，与原版一致；关闭走标题栏的 ×。
fn free_copy_footer(tx: &Sender<Msg>) -> Element {
    let copy = tx.clone();
    Element::row()
        .width_match()
        .spacing(8)
        .child(Element::flex_spacer())
        .child(
            Element::button("复制全部文本")
                .height(38)
                .on_click(move |_| {
                    let _ = copy.send(Msg::CopyFreeCopy);
                }),
        )
}

/// 合并（全部设备）模式下选完文件后弹出的「选择设备发送」：首行「全部设备」= 群发，
/// 其余一台在线设备一行。点一行就发出去并关掉，与原版 `DeviceSelecter` 的
/// 「itemClicked -> callback + close」一致；按 × 或「取消」则这批文件一个都不发。
pub fn picker(ui: &Ui, tx: &Sender<Msg>, skin: Skin) -> Element {
    let close_tx = tx.clone();
    Element::dialog_panel(
        ui.pick_open,
        "选择设备发送",
        380,
        move |_| {
            let _ = close_tx.send(Msg::ClosePicker);
        },
        picker_body(ui, tx, skin),
        picker_footer(tx),
    )
}

/// 选择器正文：顶上那句待发清单，底下一排可点的设备行。
///
/// 行表用键控列表：设备上下线会改行集，但开着对话框期间大多数同步一行都没变，
/// `put` 比过值就不会重建。
fn picker_body(ui: &Ui, tx: &Sender<Msg>, skin: Skin) -> Element {
    let sender = tx.clone();
    Element::col()
        .width_match()
        .spacing(10)
        .child(
            Element::label(ui.pick_files)
                .font_size(theme::FONT_META)
                .fg_role(Role::TextMuted)
                .line_height(1.5)
                .width_match(),
        )
        // 行键就是点击要带回的目标，「全部设备」那行是 `None`，与任何会话号都不重号。
        // `list_signal` 自带 `fill`，再罩一层 `scroll` 会是双滚动条，直接给它定个上限。
        .child(
            Element::list_signal(ui.pick_rows, |row| row.target, move |row| pick_row(row, &sender, skin))
                .width_match()
                .max_height(300),
        )
}

/// 一行目标设备。
fn pick_row(row: PickRow, tx: &Sender<Msg>, skin: Skin) -> Element {
    // 目标在建行时就算好了，闭包只带这个值：对话框节点只建一次，而行表会换。
    let target = row.target;
    let click_tx = tx.clone();
    Element::row()
        .width_match()
        .padding_edges(Insets::new(8, 6, 8, 6))
        .spacing(10)
        .cross(Align::Center)
        .corner(theme::RADIUS_CARD)
        .clickable()
        .on_click(move |_| {
            let _ = click_tx.send(Msg::PickTarget(target));
        })
        .child(match row.mode {
            Some(mode) => device_avatar(mode, theme::AVATAR_SIZE, false, true, skin),
            None => all_avatar(skin),
        })
        .child(
            Element::col()
                .weight(1.0)
                .spacing(2)
                .child(
                    Element::label(row.name)
                        .font_size(theme::FONT_BODY)
                        .font_weight(600)
                        .fg_role(Role::Text)
                        .max_lines(1)
                        .truncate(Truncate::End),
                )
                .child(caption(row.subtitle)),
        )
}

/// 首行「全部设备」的行首图块：与设备头像同尺寸的淡主色圆底 + `groups`（多人）矢量图标。
fn all_avatar(skin: Skin) -> Element {
    let size = theme::AVATAR_SIZE;
    let icon = size * theme::AVATAR_ICON / theme::AVATAR_SIZE;
    Element::stack()
        .size(size, size)
        .child(Element::leaf().fill().bg(skin.avatar_bg()).corner(size as f32 / 2.0))
        .child(assets::md_icon("groups", skin.icon_tint(), icon).align(Align::Center))
}

/// 选择器底栏：只有「取消」。发与不发全看有没有点中一行，与原版一样不设「确定」。
fn picker_footer(tx: &Sender<Msg>) -> Element {
    let cancel = tx.clone();
    Element::row()
        .width_match()
        .spacing(8)
        .child(Element::flex_spacer())
        .child(
            Element::button("取消")
                .height(38)
                .intent(Intent::Neutral)
                .on_click(move |_| {
                    let _ = cancel.send(Msg::ClosePicker);
                }),
        )
}

/// 设置对话框：保存目录、设备名、端口、传输开关、外观、消息密钥。
pub fn settings(ui: &Ui, tx: &Sender<Msg>) -> Element {
    let close_tx = tx.clone();
    Element::dialog_panel(
        ui.settings_open,
        "设置",
        560,
        move |_| {
            let _ = close_tx.send(Msg::CloseSettings);
        },
        settings_body(ui, tx),
        settings_footer(tx),
    )
}

/// 对话框正文：按组分节，整段可滚动。
fn settings_body(ui: &Ui, tx: &Sender<Msg>) -> Element {
    Element::scroll()
        .width_match()
        // 给右侧滑动条让出位置，否则正文输入框的右边框会被它盖住
        .padding_edges(Insets::new(0, 0, 12, 0))
        .max_height(460)
        .child(
            Element::col()
                .width_match()
                .spacing(8)
                .children([
                    section("常规"),
                    save_path_row(ui, tx),
                    field_text("设备名", ui.f_client_name, "局域网上显示的名字", false),
                    Element::checkbox("关闭主窗口时收进系统托盘（后台运行）", ui.f_hide_on_close).width_match(),
                    caption("后台运行时仍能正常收发消息与文件，双击托盘图标可重新打开；关掉则点 × 直接退出，托盘里也不会再挂图标。这一项要重启程序才生效。")
                        .width_match(),
                    section("网络"),
                    field_text("TCP 端口", ui.f_tcp, "5856", true),
                    field_text("UDP 端口", ui.f_udp, "4573", true),
                    Element::checkbox("开启网页服务（浏览器局域网直接访问本机文件）", ui.f_web_service).width_match(),
                    Element::checkbox("免授权开放网页服务（不再弹确认框）", ui.f_open_web_service).width_match(),
                    caption("改端口要重启程序才生效；网页服务复用 TCP 端口，勾上立刻生效；消息密钥换了要和对方一致，否则读不出正文。").width_match(),
                    section("传输"),
                    Element::checkbox("给文件数据块逐字节混淆", ui.f_enc_data).width_match(),
                    Element::checkbox("收到文件时不再询问，直接接收", ui.f_accept_recv).width_match(),
                    Element::checkbox("保存聊天记录", ui.f_save_message).width_match(),
                    Element::checkbox("允许与本机重复测试（不跳过自己发的包）", ui.f_same_host).width_match(),
                    Element::checkbox("使用内置预览器打开图片", ui.f_internal_preview).width_match(),
                    section("外观"),
                    field_text("字体", ui.f_font_family, "留空用内置字体", false),
                    field_text("字号", ui.f_font_size, "9", true),
                    // 四档与 `ThemePref::index` 一一对应：祖母绿是显式选的配色，不进「跟随系统」。
                    Element::field("主题", Element::segmented(vec!["跟随系统", "浅色", "深色", "祖母绿"], ui.f_theme)),
                    section("安全"),
                    field_text("消息密钥", ui.f_message_key, "用于消息文本的 AES 密钥", false),
                ]),
        )
}

/// 底栏：保存与取消。
fn settings_footer(tx: &Sender<Msg>) -> Element {
    let save = tx.clone();
    let cancel = tx.clone();
    Element::row()
        .width_match()
        .spacing(8)
        .child(Element::flex_spacer())
        .child(
            Element::button("取消")
                .height(38)
                .intent(Intent::Neutral)
                .on_click(move |_| {
                    let _ = cancel.send(Msg::CloseSettings);
                }),
        )
        .child(
            Element::button("保存")
                .height(38)
                .on_click(move |_| {
                    let _ = save.send(Msg::SaveSettings);
                }),
        )
}

/// 分组小标题。
fn section(title: &str) -> Element {
    Element::label(title.to_owned())
        .font_size(theme::FONT_META)
        .font_weight(600)
        .fg_role(Role::TextSubtle)
        .width_match()
        .margin_edges(Insets::new(0, 8, 0, 0))
}

/// 一行「标签 + 文本框」。`numeric` 只放行数字，端口与字号用。
///
/// 控件用 `weight` 而不是 `width_match`：`field` 那一行里标签已经占定一列，
/// 剩下的宽度得按权重分配，`width_match` 会拿整个父宽去量，把同行的其他控件顶出去。
fn field_text(label: &str, text: Signal<String>, placeholder: &str, numeric: bool) -> Element {
    let mut input = Element::text_input(text, placeholder.to_owned()).weight(1.0);
    if numeric {
        input = input.numeric();
    }
    Element::field(label.to_owned(), input)
}

/// 保存目录一行：输入框 + 原生文件夹选择按钮。
///
/// 那一排用 `weight(1.0)` 而非 `width_match()`：`field` 已经把标签定成一列，
/// 按父宽去量会让输入框吃掉整列，把「浏览…」顶到面板外面去。
fn save_path_row(ui: &Ui, tx: &Sender<Msg>) -> Element {
    let sender = tx.clone();
    Element::field(
        "保存目录",
        Element::row()
            .weight(1.0)
            .spacing(8)
            .child(Element::text_input(ui.f_save_path, "收到的文件存到这里").weight(1.0))
            .child(
                Element::button("📂 浏览…")
                    .intent(Intent::Neutral)
                    .on_click(move |_| {
                        let _ = sender.send(Msg::PickSavePath);
                    }),
            ),
    )
}

// ————————————————————— 关于 —————————————————————

/// 原版 `about.ui` 底部那一排链接的地址。GitHub 指到 Rust 线自己的仓库
/// （`LANShare-PC-Rust`），而不是原版那套 Qt 代码。
const SITE_URL: &str = "http://lanshares.com";
/// QQ 群入群链接，与原版同一个（`qm.qq.com` 的二维码邀请串）。
const QQ_GROUP_URL: &str = "https://qm.qq.com/cgi-bin/qm/qr?k=J1R2CxT5897sZbGKAkpFB1HhDSkJl_Me&jump_from=webapi&authKey=+Z8FGpnK5z+NTeAIVrtvki9vgQ58VO07bHtDpo6ahWG9Wu4WqY7m2mae516k3HIH";
/// 本仓库主页。
const GITHUB_URL: &str = "https://github.com/fgsq-team/LANShare-PC-Rust";

// 检查更新走真接口：`GET http://lanshares.com/get_version`，参数 `type=release_windows_rust`、
// `versionCode=<数值码>`、`versionName=<名>`，请求与判定都在 `utils::update` 里。服务端已经给
// 这条线开了那一行、数值码的编排也对得上，所以这里不再凑一个假地址。

/// 版本号那一行。原版是 `"V" + LANSHARE_VERSION_NAME` 那个另写的常量，这里直接取包版本，
/// 只留 `Cargo.toml` 一处真值（exe 的版本资源也是它）。
fn version_text() -> String {
    format!("V{}", env!("CARGO_PKG_VERSION"))
}

/// 关于：对应原版 `About`（`about.ui`）。图标、版本号、捐赠一句、两枚捐赠码、版权两行，
/// 底栏一排链接。宽度取原版 `setFixedSize(384, 350)` 的那个 384。
///
/// 与原版两处不一样：原版是模态子窗口、这里是遮罩面板（本程序只有一棵 windui 元素树）；
/// 原版那个「检查更新」是个 `QPushButton`，这里跟其他三项一样是链接样式，发一条消息出去查。
pub fn about(ui: &Ui, tx: &Sender<Msg>) -> Element {
    let close_tx = tx.clone();
    Element::dialog_panel(
        ui.about_open,
        "关于",
        384,
        move |_| {
            let _ = close_tx.send(Msg::CloseAbout);
        },
        about_body(),
        about_footer(tx),
    )
}

/// 正文：自上而下与 `about.ui` 里那几个 `horizontalLayoutWidget` 同序，整列居中。
fn about_body() -> Element {
    Element::col()
        .width_match()
        .spacing(12)
        .cross(Align::Center)
        .child(Element::image_bytes(assets::APP_ICON).size(48, 48).corner(10.0))
        .child(
            Element::label(version_text())
                .font_size(theme::FONT_BODY)
                .font_weight(600)
                .fg_role(Role::Text),
        )
        // 原版给这行硬刷了 `#FF5722` 加粗，走主题的警示档而不是定色：深色下照样看得清。
        .child(
            Element::label("感谢您的捐赠支持！")
                .font_size(theme::FONT_META)
                .font_weight(600)
                .fg_role(Role::Warning),
        )
        .children([donate_column("微信", assets::DONATE_WECHAT), donate_column("支付宝", assets::DONATE_ALIPAY)])
        .child(copyright_line("Copyright © 2021-2025 By FGSQ"))
        .child(copyright_line("All Rights Reserved"))
}

/// 一枚捐赠码：图 + 底下那行名字，对应原版 `wechatQRCode` / `wechatLabel` 那一组。
///
/// 两枚并排靠外层 `col` 的 `cross(Align::Center)` 居中，尺寸照原版的 100×100。
fn donate_column(name: &str, picture: &'static [u8]) -> Element {
    Element::col()
        .spacing(4)
        .cross(Align::Center)
        .child(
            Element::image_bytes(picture)
                .size(100, 100)
                .corner(theme::RADIUS_CARD)
                .border_role(Role::Border, 1),
        )
        .child(Element::label(name.to_owned()).font_size(theme::FONT_META).fg_role(Role::TextSubtle))
}

/// 版权那两行，与原版同样是次要文字。
fn copyright_line(text: &str) -> Element {
    Element::label(text.to_owned()).font_size(theme::FONT_META).fg_role(Role::TextMuted)
}

/// 底栏的链接行：`官网 | QQ群 | GitHub | 检查更新`。两头各垫一个 `flex_spacer` 让它居中，
/// 而不是像设置那样把按钮推到右边。
fn about_footer(tx: &Sender<Msg>) -> Element {
    Element::row()
        .width_match()
        .spacing(6)
        .cross(Align::Center)
        .child(Element::flex_spacer())
        .child(about_link("官网", SITE_URL))
        .child(link_separator())
        .child(about_link("QQ群", QQ_GROUP_URL))
        .child(link_separator())
        .child(about_link("GitHub", GITHUB_URL))
        .child(link_separator())
        .child(check_update_link(tx))
        .child(Element::flex_spacer())
}

/// 一枚链接：`Element::link` 自带链接色、下划线与手型光标，点击由 windui 交系统默认
/// 浏览器（`EventCtx::open_url`），不需要这里起进程、也不发一条消息。
fn about_link(text: &str, url: &str) -> Element {
    Element::link(text.to_owned()).font_size(theme::FONT_META).url(url.to_owned())
}

/// 「检查更新」长得和前三个一样，行为不一样：它不开浏览器，而是发一条消息去后台查
/// （查回什么由 `dialogs::update` 那个面板说）。`Element::link` 的 `on_click` 优先于 `url`，
/// 所以这里不写 `.url()`，点了不会出现「既开浏览器又发消息」的双重动作。
///
/// `manual: true`：用户主动问的，要实话——没新版也得报一句。
fn check_update_link(tx: &Sender<Msg>) -> Element {
    let sender = tx.clone();
    Element::link("检查更新".to_owned())
        .font_size(theme::FONT_META)
        .on_click(move |_| {
            let _ = sender.send(Msg::CheckUpdate { manual: true });
        })
}

/// 链接之间那道竖线。原版是普通 `QLabel` 写的 `" | "`，这里压成弱化色免得抢链接的分量。
fn link_separator() -> Element {
    Element::label(" | ").font_size(theme::FONT_META).fg_role(Role::TextMuted)
}

// ————————————————————— 更新提示 —————————————————————

/// 更新提示：对应原版 `UpdateChecker::handleReply` 里那个 `QMessageBox`。标题是
/// 「有新版本更新: <版本名>」，正文是服务端那段更新说明（可拖选，也能整段拷走），底栏
/// 「不更新 / 复制全文 / 打开下载链接」；强更时左边那一枚换成「退出」——原版那条路也没有
/// 退路，记下「已拒过」同样没用。
///
/// 与原版三处不一样：原版是系统模态框、这里是遮罩面板（本程序只有一棵 windui 元素树）；
/// 那段说明在 Qt 里是 HTML 直接排版，这里显的是折出来的纯文本；「已是最新 / 检测失败」
/// 那两句走轻提示而不是这个面板。
pub fn update(ui: &Ui, tx: &Sender<Msg>) -> Element {
    let close_tx = tx.clone();
    Element::dialog_panel(
        ui.update_open,
        ui.update_title,
        460,
        move |_| {
            // 按 × 与按 ESC 都只算「先不看」，不等于「不更新」：不记那一版，下次照样提。
            let _ = close_tx.send(Msg::CloseUpdate);
        },
        update_body(ui),
        update_footer(ui, tx),
    )
}

/// 正文：那段说明可以很长（服务端存的是整页 HTML，折过来还有几十行），给一个定高的框，
/// 多的内容由控件自己滚，面板不至于被顶到窗外。
///
/// 用只读多行输入框而不是 `rich`，是为了拖选能住：富文本控件只要布局缓存 miss 就连带
/// 清选区（windui `rich::ensure_layout`），而 `width_match` 套在 `scroll` 里恰好是它最容易
/// miss 的形状——竖向滚动条让 measure 与 paint 拿到不同的具体宽度，于是每帧交替重排；
/// 拖动靠 pointer 事件逐帧重新高亮所以看得见，一松手下一帧就没了（实际反馈如此）。
/// 只读输入框的选区不依赖那个缓存键，自带滚动、`wrap`、拖选与 Ctrl+A/Ctrl+C，与
/// 「自由复制文本」同一款控件、同一口径。
fn update_body(ui: &Ui) -> Element {
    Element::text_input(ui.update_body, "")
        .multiline()
        .wrap(true)
        .read_only()
        .width_match()
        .height(300)
}

/// 底栏：「不更新」与「打开下载链接」，强更时前者换成「退出」。两枚都常挂在树上、按
/// `update_force` 决定谁可见——省得为一档开关来回重建子树。中间那枚「复制全文」不带开关，
/// 因为正文里那些链接能不能拷走与强不强更无关。
fn update_footer(ui: &Ui, tx: &Sender<Msg>) -> Element {
    let force = ui.update_force;
    let url = ui.update_url;
    let body = ui.update_body;
    let download_tx = tx.clone();
    let copy_tx = tx.clone();
    Element::row()
        .width_match()
        .spacing(8)
        .child(Element::flex_spacer())
        .child(update_press("不更新", Msg::DeclineUpdate, tx).visible_signal(force.map(|force| !*force)))
        .child(update_press("退出", Msg::QuitForUpdate, tx).visible_signal(force.map(|force| *force)))
        .child(
            Element::button("复制全文")
                .height(38)
                .intent(Intent::Neutral)
                .on_click(move |_| {
                    // 同样按下时现取：对话框节点只建一次，每次提示的是不同版本。空正文不发消息，
                    // 否则会得到一句「已复制到剪贴板」而剪贴板里其实没东西。
                    let text = body.get();
                    if !text.is_empty() {
                        let _ = copy_tx.send(Msg::Copy(text));
                    }
                }),
        )
        .child(
            Element::button("打开下载链接")
                .height(38)
                .on_click(move |ctx| {
                    // 地址点击时现取：对话框节点只建一次，而每次提示的是不同版本。
                    // 空地址不去叫浏览器（服务端那一行偶尔真没填下载地址），但提示照样收掉——
                    // 说明正文里通常还列着镜像地址。
                    let link = url.get();
                    if !link.is_empty() {
                        ctx.open_url(&link);
                    }
                    let _ = download_tx.send(Msg::CloseUpdate);
                }),
        )
}

/// 底栏那两枚只需发一条消息的按钮：面板由状态机的下一次同步收掉。都是次要档，
/// 主操作让给右边的「打开下载链接」，与原版的默认按钮一致。
fn update_press(label: &str, action: Msg, tx: &Sender<Msg>) -> Element {
    let sender = tx.clone();
    Element::button(label.to_owned()).height(38).intent(Intent::Neutral).on_click(move |_| {
        let _ = sender.send(action.clone());
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 版本行只能跟着包版本：原版那种另写一份常量的做法，迟早和 `Cargo.toml`、
    /// 以及 exe 里的版本资源对不上。
    #[test]
    fn version_text_follows_the_package_version() {
        assert_eq!(version_text(), format!("V{}", env!("CARGO_PKG_VERSION")));
    }

    /// 三处链接得是完整绝对地址：QQ 群那条一长串参数，复制时漏个 `&` 后面的就断了，
    /// 而断掉的链接点开会落到一个报错页，肉眼在界面上看不出来。
    #[test]
    fn about_links_are_complete_absolute_urls() {
        for url in [SITE_URL, QQ_GROUP_URL, GITHUB_URL] {
            // 割出主机名那一段：没有它就只能断定「开头像个协议」，测不出域名写坏了。
            let (scheme, rest) = url.split_once("://").unwrap_or_else(|| panic!("链接不是绝对地址：{url}"));
            assert!(matches!(scheme, "http" | "https"), "链接协议不对：{url}");
            let host = rest.split('/').next().unwrap_or_default();
            assert!(!host.is_empty() && host.contains('.'), "链接少了域名：{url}");
        }
        assert!(QQ_GROUP_URL.contains("&jump_from=webapi&authKey="), "QQ 群链接的参数字符串被截断了");
    }

    /// 两枚捐赠码要能走通运行期那条真正的路：`Element::image_bytes` → windui 自己的解码器
    /// 注册表。那里只内置 PNG 与 SVG，递 JPEG 进去会得到 `UnsupportedFormat`、画面上就是一块
    /// 空白——所以故意不用 `image` crate 自己解：那条路解得开 JPEG，恰好把问题掩盖掉。
    #[test]
    fn donation_pictures_decode_through_windui() {
        for (name, bytes) in [("wx.png", assets::DONATE_WECHAT), ("alipay.png", assets::DONATE_ALIPAY)] {
            let picture =
                Image::from_bytes(bytes).unwrap_or_else(|error| panic!("{name} 过不了 windui 解码：{error}"));
            assert!(picture.width() >= 100 && picture.height() >= 100, "{name} 分辨率太低，拉到 100×100 会糊");
        }
    }
}
