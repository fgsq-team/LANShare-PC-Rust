//! 从 LANShare-PC 复制过来的图片资源，以及运行期的着色与缩略图解码。
//!
//! 原版靠 Qt 资源系统把图打进可执行文件（`:/img/ic_phone.png`），这里用
//! `include_bytes!` / `include_str!` 达到同样效果：运行目录旁不必再摆一份
//! assets，拷走 exe 就能跑。
//!
//! 两处着色照抄原版的字符串替换：文件图块的 `__BG__` / `__ICON__`
//! （`recolorSvgPixmap`）、状态徽标与 MD 图标那一处 `__FILL__`——反正 resvg 与
//! QPainter 都是抗锯齿矢量光栅。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};

use windui::prelude::*;

use crate::lanshare::device::{MODE_ANDROID, MODE_IOS, MODE_WIN};
use crate::theme;

/// 手机头像（原版 `ic_phone.png`）。
const ICON_PHONE: &[u8] = include_bytes!("../assets/img/ic_phone.png");
/// Windows 电脑头像（原版 `ic_win.png`）。
const ICON_WIN: &[u8] = include_bytes!("../assets/img/ic_win.png");
/// 其它设备（含未知）走 LANShare 自带图标，与原版 `else` 分支一致。
const ICON_OTHER: &[u8] = include_bytes!("../assets/img/ic_launcher_32.png");

/// 底部工具条的「选择文件」（原版 `:/img/file.png`）。
pub const ICON_FILE: &[u8] = include_bytes!("../assets/img/file.png");
/// 底部工具条的「选择文件夹」（原版 `:/img/folder.png`）。
pub const ICON_FOLDER: &[u8] = include_bytes!("../assets/img/folder.png");
/// 发送按钮的图标（原版 `:/img/send.png`）。
pub const ICON_SEND: &[u8] = include_bytes!("../assets/img/send.png");
/// 「打开接收目录」（原版 `:/img/openfile.png`）。
pub const ICON_OPEN: &[u8] = include_bytes!("../assets/img/openfile.png");

/// 应用图标（原版 `ic_launcher.png`，256×256），供窗口标题栏、任务栏、Alt-Tab 与「关于」页顶部那枚图用。
pub const APP_ICON: &[u8] = include_bytes!("../assets/img/app_icon.png");

/// 「关于」页那两枚捐赠码：微信（原版 `:/img/wx.jpg`）与支付宝（原版 `:/img/alipay.jpg`）。
/// 图是从 LANShare-PC 拷来的原始照片，但**存成了 `.png`**：windui 只内置 PNG / SVG 解码器，
/// JPEG 得由使用方 `register_decoder` 注进去（本项目的 JPEG 一律自己用 `image` 解完再喂
/// `Image::from_rgba`，见预览器与缩略图），而这两枚是直接交给 `image_bytes` 的。
/// 两枚都不正好是方形（216×216 与 224×223），原版也是直接 `scaledContents` 拉成 100×100，
/// 所以这里同样按正方框画，不另裁图。
pub const DONATE_WECHAT: &[u8] = include_bytes!("../assets/img/wx.png");
/// 捐赠码：支付宝。
pub const DONATE_ALIPAY: &[u8] = include_bytes!("../assets/img/alipay.png");

/// 文件图块模板：底板与图形两处占位，固有尺寸 48。
const FILE_SVG: &str = include_str!("../assets/svg/rc_file_icon_file.svg");
/// 文件夹图块模板：同一组占位符。
const DIR_SVG: &str = include_str!("../assets/svg/rc_dir_blue_icon.svg");

/// 状态徽标：实心圆 + 白色对号，几何抄 `statusBadgePixmap` 的 success 分支。
const BADGE_CHECK: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" viewBox="0 0 32 32"><circle cx="16" cy="16" r="14" fill="__FILL__"/><path d="M9.6 16.6 L14.2 21.2 L22.6 11.8" fill="none" stroke="#FFFFFF" stroke-width="3.4" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;
/// 状态徽标：实心圆 + 白色感叹号（竖线加一个圆点），对应同一个函数的失败分支。
const BADGE_ALERT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32" viewBox="0 0 32 32"><circle cx="16" cy="16" r="14" fill="__FILL__"/><path d="M16 9.4 V18.2" fill="none" stroke="#FFFFFF" stroke-width="3.4" stroke-linecap="round"/><circle cx="16" cy="23.2" r="1.9" fill="#FFFFFF"/></svg>"##;

/// 设备类型对应的头像位图，分支与原版 `avatarPixmap` 一致。
pub fn device_icon(mode: i32) -> &'static [u8] {
    match mode {
        MODE_ANDROID | MODE_IOS => ICON_PHONE,
        MODE_WIN => ICON_WIN,
        _ => ICON_OTHER,
    }
}

/// 文件（文件夹）图块：白底 + 主色 glyph，两侧气泡同款——原版的注释说得很清楚，
/// 「左右不再区分」，靠底板白与气泡底色拉开层次。`tint` 由配色档案给，换主题时 glyph 跟着换色。
pub fn file_tile(is_dir: bool, size: i32, tint: Color) -> Element {
    let template = if is_dir { DIR_SVG } else { FILE_SVG };
    let svg = template
        .replace("__BG__", "#FFFFFF")
        .replace("__ICON__", &theme::hex_string(tint));
    // `target_width=None` 让 windui 按绘制框的实际物理尺寸光栅，任何 DPI 都 1:1 落像素
    Element::image_svg(svg.as_bytes(), None).size(size, size)
}

/// 状态徽标：成功是绿底白勾，失败是红底白感叹号。
pub fn status_badge(ok: bool, fill: Color, size: i32) -> Element {
    let svg = (if ok { BADGE_CHECK } else { BADGE_ALERT }).replace("__FILL__", &theme::hex_string(fill));
    Element::image_svg(svg.as_bytes(), None).size(size, size)
}

// ————————————————————— Material Design 矢量图标 —————————————————————
//
// 一律 24×24 视框、单色 `__FILL__` 占位，path 数据取 Material 官方图标。**每个图标一个 `.svg`**
// 摆在 `assets/md/`，这里只按名字 `include_str!` 进来：path 属于资源而不是代码，改图标就是改
// 那张图，能直接拿编辑器 / 浏览器预览，也不必在 .rs 里维护一堆上千字的坐标串。
// 与 `assets/svg/` 里那两张文件图块同一套做法，编译期内嵌、运行目录旁不需再摆一份。
//
// 原先用 emoji / 符号字符画的图标（搜索、复制、刷新、深/浅色、设置、二维码、更多、关闭、警告、
// 群聊、空态、电量、菜单项）全换成这套矢量：随主题着色、任何 DPI 都 1:1 落像素，观感与文件图块 /
// 状态徽标一致。工具栏那几张 PNG（文件 / 文件夹 / 发送 / 打开）不在本轮替换范围内，保持原样。

/// 取 MD 图标的 svg 模板。名字写错返回 `unknown.svg` 那枚实心圆点，画出来是一枚小圆斑，
/// 便于肉眼定位而不是整块空白。
fn md_template(name: &str) -> &'static str {
    match name {
        "search" => include_str!("../assets/md/search.svg"),
        "content_copy" => include_str!("../assets/md/content_copy.svg"),
        "refresh" => include_str!("../assets/md/refresh.svg"),
        "dark_mode" => include_str!("../assets/md/dark_mode.svg"),
        "settings" => include_str!("../assets/md/settings.svg"),
        "info" => include_str!("../assets/md/info.svg"),
        "qr_code" => include_str!("../assets/md/qr_code.svg"),
        "more_vert" => include_str!("../assets/md/more_vert.svg"),
        "mood" => include_str!("../assets/md/mood.svg"),
        "warning" => include_str!("../assets/md/warning.svg"),
        "close" => include_str!("../assets/md/close.svg"),
        "chat_bubble" => include_str!("../assets/md/chat_bubble.svg"),
        "groups" => include_str!("../assets/md/groups.svg"),
        "done" => include_str!("../assets/md/done.svg"),
        "done_all" => include_str!("../assets/md/done_all.svg"),
        "delete" => include_str!("../assets/md/delete.svg"),
        "folder" => include_str!("../assets/md/folder.svg"),
        "battery_charging_full" => include_str!("../assets/md/battery_charging_full.svg"),
        // 图片预览工具条那一组。旋转两枚取 MD 的 undo / redo：windui 没有旋转变换，预览器是
        // 把角度烘进像素的，图标要的只是「往回转 / 往后转」这层语义，箭头弧线正好对得上。
        "arrow_back" => include_str!("../assets/md/arrow_back.svg"),
        "arrow_forward" => include_str!("../assets/md/arrow_forward.svg"),
        "rotate_left" => include_str!("../assets/md/rotate_left.svg"),
        "rotate_right" => include_str!("../assets/md/rotate_right.svg"),
        "save" => include_str!("../assets/md/save.svg"),
        "open_in_new" => include_str!("../assets/md/open_in_new.svg"),
        "fullscreen" => include_str!("../assets/md/fullscreen.svg"),
        // 兜底：一个小实心圆点，让写错的图标名在画面上露个斑，而不是彻底看不见。
        _ => include_str!("../assets/md/unknown.svg"),
    }
}

/// 生成着色后的 MD 图标 SVG 字符串（供内联元素与位图光栅共用）。
///
/// 只把模板里那一处 `__FILL__` 换成主题色，与文件图块、状态徽标走同一套字符串替换。
fn md_svg(name: &str, fill: Color) -> String {
    md_template(name).replace("__FILL__", &theme::hex_string(fill))
}

/// 内联的 MD 图标元素：按绘制框 1:1 光栅，随主题着色。用于原来画 emoji / 符号字符的地方。
pub fn md_icon(name: &str, fill: Color, size: i32) -> Element {
    Element::image_svg(md_svg(name, fill).as_bytes(), None).size(size, size)
}

/// 图标按钮的内容：MD 矢量随主题着色，交给 `Element::icon_button_content`。
/// 与工具栏那张 PNG 走同一条 `icon_button_content` 通路，尺寸由按钮自定。
pub fn md_icon_content(name: &str, fill: Color) -> ImageContent {
    ImageContent::from_svg_bytes(md_svg(name, fill).as_bytes(), None)
}

/// 菜单项的前置位图图标：单色模板，windui 只取其 alpha、按该项文字色着色（删除项即危险红），
/// 所以填色取黑即可。解不出来（不该发生）就返回 `None`，调用方退回无图标文字项。
pub fn md_icon_image(name: &str) -> Option<Rc<Image>> {
    Image::from_svg_bytes(md_svg(name, Color::BLACK).as_bytes(), Some(24)).ok().map(Rc::new)
}

/// 解出窗口图标：把内嵌的 `app_icon.png` 解码成 RGBA 位图交给 windui。
///
/// 只给一张高分辨率（256）方形图，标题栏 / 任务栏 / Alt-Tab 各档 DPI 由 windui 自行缩放，
/// 与 Qt 版靠同一份 `ic_launcher` 出图一致。解不出（资源缺失或格式坏）就返回 `None`，
/// 调用方据此跳过 `.icon(..)`，退回 windui 内置图标而不是崩在启动路径上。
pub fn window_icon() -> Option<WindowIcon> {
    let picture = image::load_from_memory(APP_ICON).ok()?.into_rgba8();
    let (width, height) = (picture.width(), picture.height());
    WindowIcon::from_rgba(width, height, picture.into_raw())
}

/// 解出托盘图标：把内嵌的 `ic_launcher_32.png` 解码成**非预乘 RGBA8** 三元组，交给
/// `Tray::icon_rgba`。与 [`window_icon`] 同源不同用——那边给 `WindowIcon` 让 windui 按各档
/// DPI 自行缩放，这边是托盘要裸像素自建 HICON，尺寸就得贴系统托盘那一档（16/32），故用
/// 现成的 32×32 图。解不出（资源缺失或格式坏）就返回 `None`，调用方据此不设自定义图标，
/// 退回系统默认应用图标而不是崩在启动路径上。
pub fn tray_icon() -> Option<(u32, u32, Vec<u8>)> {
    let picture = image::load_from_memory(ICON_OTHER).ok()?.into_rgba8();
    let (width, height) = (picture.width(), picture.height());
    Some((width, height, picture.into_raw()))
}

// 解码好的缩略图缓存。键是落盘路径，值 `None` 表示「解过、解不出来」——免得对着同一张
// 坏图反复尝试。不淘汰、不记序：一个会话里图片至多几百张，一张降采样缩略图约 0.4MB，
// 常驻内存完全可以接受；解过的图永远不再重解，这是最简单也最不会出反复的形态。
thread_local! {
    static THUMBS: RefCell<HashMap<PathBuf, Option<Rc<Image>>>> = RefCell::new(HashMap::new());
    // 已投递、还没回包的解码任务：同一个路径在解码期间不重复投。
    static PENDING: RefCell<HashSet<PathBuf>> = RefCell::new(HashSet::new());
}

/// 交给后台解码线程的一个任务：`(路径, 缩略图逻辑边长)`。
pub type ThumbJob = (PathBuf, i32);

/// 一条解码回包：`(路径, Option<(宽, 高, 非预乘 RGBA8)>)`；`None` = 解不出来，这个结论
/// 也进缓存，不对同一张坏图反复重请。**只带裸数据跨线程**：`Image` 内含 `Rc`，`!Send`，
/// 构造留在 UI 线程（`store_thumbs` 里现构）。
pub type DecodedThumb = (PathBuf, Option<(u32, u32, Vec<u8>)>);

// 解码队列的进程级两端：UI 线程建行的时候往 `JOBS_TX` 投任务，`RESULTS_RX` 由投递线程
// 取件后回投 `Msg::ThumbsReady`。
static JOBS_TX: OnceLock<std::sync::mpsc::Sender<ThumbJob>> = OnceLock::new();
static RESULTS_RX: OnceLock<Mutex<std::sync::mpsc::Receiver<Vec<DecodedThumb>>>> = OnceLock::new();

/// 后台并行解码线程数。JPEG 解码 CPU 密集、各图独立，几条就能把「一整批图全解完」
/// 的墙钟时间近乎线性砍下来；再多会与 UI 线程抢核，4 是稳的折中。
const DECODE_THREADS: usize = 4;

/// 取件口：首次调用时拉起解码线程池（懒启动——不开图片会话的进程不该白起线程）。
pub fn thumb_queue() -> &'static Mutex<std::sync::mpsc::Receiver<Vec<DecodedThumb>>> {
    RESULTS_RX.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<Vec<DecodedThumb>>();
        let (jobs_tx, jobs_rx) = std::sync::mpsc::channel::<ThumbJob>();
        let _ = JOBS_TX.set(jobs_tx);
        // 任务队列由几条线程共享一个 `Receiver`（`Receiver` 不 `Clone`，套 `Mutex` 抢取），
        // 回包 `Sender` 则各克隆一份：mpsc 允许多生产者。
        let jobs = Arc::new(Mutex::new(jobs_rx));
        for _ in 0..DECODE_THREADS {
            let jobs = Arc::clone(&jobs);
            let tx = tx.clone();
            std::thread::spawn(move || thumb_worker(jobs, tx));
        }
        // 丢掉建池时用的那个 `tx`：只要还有线程持克隆，接收端就不会误判断开。
        drop(tx);
        Mutex::new(rx)
    })
}

/// 解码线程主体：从共享队列取一个任务、在锁外解码、解完立刻回包（一张一发）。
///
/// 「一张一发」是关键：攒齐整批再一次性回，一批图要等全部解完才「哗」地一起冒出来，
/// 中间全是占位块——看着就像卡死。逐张回 + 投递线程即时转发，缩略图按完成顺序陆续显影。
fn thumb_worker(jobs: Arc<Mutex<std::sync::mpsc::Receiver<ThumbJob>>>, tx: std::sync::mpsc::Sender<Vec<DecodedThumb>>) {
    loop {
        // 只在取任务的一瞬持锁，解码并行跑在锁外。
        let job = match jobs.lock() {
            Ok(rx) => match rx.recv() {
                Ok(job) => job,
                // 任务通道关闭 = 进程在收尾，退出线程。
                Err(_) => return,
            },
            Err(_) => return,
        };
        let (path, size) = job;
        let decoded = decode_rgba(&path, size);
        // 发送失败 = UI 已关，退出线程。
        if tx.send(vec![(path, decoded)]).is_err() {
            return;
        }
    }
}

/// 把一个解码任务投进后台队列（按 `PENDING` 去重，已在缓存里的直接跳过）。行构建时
/// 直接调用：会话里的每张图都会排队，解码全在后台线程——不判视口、不排优先级、不撤销，
/// 最早的朴素方案；解码走 `image` 纯 Rust 全解。
pub fn queue_thumb(path: &Path, size: i32) {
    if path.as_os_str().is_empty() {
        return;
    }
    if THUMBS.with(|cache| cache.borrow().contains_key(path)) {
        return;
    }
    if !PENDING.with(|pending| pending.borrow_mut().insert(path.to_path_buf())) {
        return;
    }
    // 顺带保证解码线程池已起（`thumb_queue` 懒启动会把 `JOBS_TX` 填上）。
    let _ = thumb_queue();
    if let Some(tx) = JOBS_TX.get() {
        let _ = tx.send((path.to_path_buf(), size));
    }
}

/// 回包落地：在 UI 线程把裸像素构造成 `Image`、写缓存、销 `PENDING`。
pub fn store_thumbs(results: Vec<DecodedThumb>) {
    THUMBS.with(|cache| {
        let mut cache = cache.borrow_mut();
        for (path, decoded) in results {
            PENDING.with(|pending| pending.borrow_mut().remove(&path));
            let built = decoded.and_then(|(w, h, rgba)| Image::from_rgba(w, h, &rgba).ok().map(Rc::new));
            cache.insert(path, built);
        }
    });
}

/// 该路径的缩略图是否已在缓存里（含「解不出来」这个确定结论）。
///
/// 行数据必须带上这个布尔：键控列表只重建**数据变了**的行，若 `ImageCard` 不记录就绪态，
/// 回包到了那一行也不会重画。任务由行构建时的 [`queue_thumb`] 投，这里只读。
pub fn thumb_ready(path: &Path) -> bool {
    if path.as_os_str().is_empty() {
        return false;
    }
    THUMBS.with(|cache| cache.borrow().contains_key(path))
}

/// 图片气泡的缩略图：命中缓存就给图，否则给 `None`（行画占位块）。
///
/// 回包经 `Msg::ThumbsReady` 落库、行重建后才换上图，解码从不在渲染线程发生。
/// 内层 `Option` 为 `None` 是「解过的坏图」这个确定结论，取出来仍是 `None`。
pub fn thumbnail(path: &Path, _size: i32) -> Option<Rc<Image>> {
    THUMBS.with(|cache| cache.borrow().get(path).and_then(|entry| entry.clone()))
}

/// 读图 -> 居中裁成正方形 -> 缩放到 `side` 的 2 倍 -> 非预乘 RGBA。
///
/// 只产出裸像素：这一步在后台解码线程上跑，返回类型必须 `Send`。
fn decode_rgba(path: &Path, side: i32) -> Option<(u32, u32, Vec<u8>)> {
    // 缩略图按 2× 出图（高 DPI 屏 1 逻辑像素 = 2 物理像素）。
    let target = (side as u32).saturating_mul(2).max(1);
    // 纯 Rust 全解：`image` 没有公开的 JPEG 降采样解码 API，只能整张解出后再裁方缩小。
    let bytes = std::fs::read(path).ok()?;
    let picture = image::load_from_memory(&bytes).ok()?.to_rgb8();
    let (width, height) = (picture.width(), picture.height());
    if width == 0 || height == 0 {
        return None;
    }
    let edge = width.min(height);
    let cropped = image::imageops::crop_imm(&picture, (width - edge) / 2, (height - edge) / 2, edge, edge).to_image();
    let side_px = target.min(edge);
    let small = image::imageops::resize(&cropped, side_px, side_px, image::imageops::FilterType::Lanczos3);
    let rgba = image::DynamicImage::ImageRgb8(small).to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    Some((w, h, rgba.into_raw()))
}

/// 缩略图控件：圆角 12 的方块，取不到图就交给调用方画占位。
pub fn thumbnail_element(path: &Path, size: i32) -> Option<Element> {
    let image = thumbnail(path, size)?;
    let content = ImageContent::new(Some((*image).clone())).fit(Fit::Cover);
    Some(Element::image_content(content).size(size, size).corner(theme::THUMB_RADIUS))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 模板里的占位符名字写错，替换就会静默失效：图块会画成模板里残留的字面量，
    /// resvg 直接把非法颜色当黑色，看上去像「图标坏了」。
    #[test]
    fn svg_templates_carry_the_placeholders_the_replacement_expects() {
        assert!(FILE_SVG.contains("__BG__") && FILE_SVG.contains("__ICON__"));
        assert!(DIR_SVG.contains("__BG__") && DIR_SVG.contains("__ICON__"));
        assert!(BADGE_CHECK.contains("__FILL__") && BADGE_ALERT.contains("__FILL__"));
        // 兜底那枚也算一个模板：写错图标名时它得跟着主题变色，不能是个定色。
        assert!(md_template("__no_such_icon__").contains("__FILL__"));
    }

    /// 替换后不该再有任何占位符残留，且颜色要写成 SVG 认得的形式。
    #[test]
    fn recolored_svg_leaves_no_placeholder_behind() {
        let svg = FILE_SVG
            .replace("__BG__", "#FFFFFF")
            .replace("__ICON__", &theme::hex_string(theme::Skin::LIGHT.primary));
        assert!(!svg.contains("__"));
        assert!(svg.contains("#5B8DEF"));
        assert!(svg.contains("#FFFFFF"));
    }

    /// 徽标按成功/失败取不同形状，填充色由主题给，两者不能串。
    #[test]
    fn badge_shapes_are_distinct_between_success_and_failure() {
        let ok = BADGE_CHECK.replace("__FILL__", "#388E3C");
        let bad = BADGE_ALERT.replace("__FILL__", "#C62828");
        assert!(ok.contains("stroke-linejoin") && !bad.contains("stroke-linejoin"));
        assert!(bad.contains("cy=\"23.2\"") && !ok.contains("cy=\"23.2\""));
    }

    /// 未知设备类型也要有图标可画，绝不能空着。
    #[test]
    fn every_device_mode_falls_back_to_an_icon() {
        assert!(device_icon(MODE_ANDROID).starts_with(&[0x89, b'P', b'N', b'G']));
        assert_eq!(device_icon(MODE_WIN), ICON_WIN);
        assert_eq!(device_icon(-1), ICON_OTHER);
    }

    /// 工具条那几个图标都是 PNG，写错路径会在编译期就炸，但尺寸不对是运行期才看得出的。
    #[test]
    fn toolbar_icons_are_real_pngs() {
        for icon in [ICON_FILE, ICON_FOLDER, ICON_SEND, ICON_OPEN] {
            assert!(icon.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]), "不是完整的 PNG 魔数");
        }
    }

    /// 缩略图走的是「居中裁方再缩小」：手机发来的原图几乎都是长方形，
    /// 短边决定裁窗口，长边被切掉。取不到图也必须安静地返回 `None`，让调用方画占位。
    ///
    /// 加载测试不碰后台线程：直接拿裸像素 → `store_thumbs` 入库 → 再读，
    /// 验证的就是「回包落库后行能拿到图」这条链。
    #[test]
    fn thumbnail_is_lazy_and_lands_through_the_ready_channel() {
        let path = std::env::temp_dir().join(format!("lanshare-thumb-{}.png", std::process::id()));
        let mut picture = image::RgbImage::new(900, 600);
        for (x, y, pixel) in picture.enumerate_pixels_mut() {
            *pixel = image::Rgb([x as u8, y as u8, 128]);
        }
        image::DynamicImage::ImageRgb8(picture).save(&path).unwrap();

        let raw = decode_rgba(&path, 160).expect("长方形原图应该能出缩略图");
        assert_eq!((raw.0, raw.1), (320, 320), "按 2 倍高 DPI 出图");
        store_thumbs(vec![(path.clone(), Some(raw))]);

        let thumb = thumbnail(&path, 160).expect("入库后就绪");
        assert!(thumb_ready(&path));
        // 同一张图再取一次走缓存，结果必须是同一个对象
        assert!(Rc::ptr_eq(&thumb, &thumbnail(&path, 160).unwrap()));

        // 「解不出来」也是确定结论：入库后算就绪，不对同一张坏图反复重请。
        let broken = path.join("missing.png");
        store_thumbs(vec![(broken.clone(), None)]);
        assert!(thumbnail(&broken, 160).is_none());
        assert!(thumb_ready(&broken), "失败结论也该算就绪");
        let _ = std::fs::remove_file(&path);
    }

    /// 缓存只增不汰：灌一批回包，每一张都必须永远可读——解过的图绝不二次解码。
    /// 旧版按张数/LRU 淘汰正在显示的图，引发行重建时 `ready` 翻转、占位重解的
    /// 「加载出来又重载」风暴；这个回归就靠这条守住。
    #[test]
    fn thumb_cache_never_evicts() {
        let keyed = |n: u32| PathBuf::from(format!("lanshare-keep-{}-{n}.png", std::process::id()));
        THUMBS.with(|cache| cache.borrow_mut().clear());

        store_thumbs((0..50u32).map(|n| (keyed(n), Some((1u32, 1u32, vec![0, 0, 0, 255])))).collect());

        let (len, all_ready) = THUMBS.with(|cache| {
            let cache = cache.borrow();
            (cache.len(), (0..50u32).all(|n| cache.contains_key(&keyed(n))))
        });
        assert_eq!(len, 50, "一批 50 张该全部常驻，实测 {len}");
        assert!(all_ready, "任何一张都不该被淘汰后要求重解");

        THUMBS.with(|cache| cache.borrow_mut().clear());
    }

    /// 视图引用的每个 MD 图标名都要有自己的 `.svg`，不能落到兜底圆点（名字写错会静默画成小
    /// 圆斑，肉眼当成「图标坏了」）；文件要真能当 svg 解析，着色后占位符也要替换干净。
    #[test]
    fn every_referenced_material_icon_resolves_to_a_real_path() {
        const NAMES: [&str; 25] = [
            "search", "content_copy", "refresh", "dark_mode", "settings", "info", "qr_code",
            "more_vert", "mood", "warning", "close", "chat_bubble", "groups", "done", "done_all",
            "delete", "folder", "battery_charging_full", "arrow_back", "arrow_forward",
            "rotate_left", "rotate_right", "save", "open_in_new", "fullscreen",
        ];
        let fallback = md_template("__not_an_icon__");
        for name in NAMES {
            let template = md_template(name);
            assert_ne!(template, fallback, "{name} 落到了兜底圆点，图标名写错了");
            // 图标名与文件名同名的那条约定：写错路径会在编译期就炸，这里钉的是名字写漏。
            assert!(template.contains("viewBox=\"0 0 24 24\""), "{name} 不是 24×24 视框");
            assert!(template.contains("fill=\"__FILL__\""), "{name} 的模板没有填充色占位符");
            let svg = md_svg(name, theme::Skin::LIGHT.primary);
            assert!(svg.starts_with("<svg") && svg.contains("<path fill=\"#5B8DEF\""), "{name} 着色后的 SVG 结构不对");
            assert!(!svg.contains("__"), "{name} 还残留没替换掉的占位符");
            // 最硬的一条：真拿去光栅。文件开头多一个 BOM、路径写坏、标签不闭合，到这一步
            // 都会解不出图——那是运行期只会表现为「图标静静消失」的错。
            assert!(md_icon_image(name).is_some(), "{name} 解不出位图，svg 文件坏了");
        }
        // 兜底那枚也要能画：它是唯一允许可见的「名字写错」信号。
        assert!(md_svg("__not_an_icon__", Color::BLACK).contains("<path fill=\"#000000\""));
    }

    /// 应用图标必须能解成方形 RGBA 位图，否则窗口/任务栏会退回 windui 默认图标。
    #[test]
    fn app_icon_decodes_to_a_square_bitmap() {
        let icon = window_icon().expect("app_icon.png 应能解成窗口图标");
        assert_eq!(icon.width(), icon.height(), "必须是方形");
        assert!(icon.width() >= 32, "分辨率过低会糊");
        assert_eq!(icon.rgba().len(), (icon.width() * icon.height() * 4) as usize, "RGBA 每像素 4 字节");
    }
}
