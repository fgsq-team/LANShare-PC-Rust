//! 图片预览子窗：复刻 `LANShare-PC/src/ImagePreview.cpp`。
//!
//! 交互一律照原版：滚轮以指针为锚缩放、左键拖拽平移、双击在「适应窗口」与「原始尺寸」之间
//! 切换、`←→`/`AD` 翻图、`R`/`Shift+R` 旋转、`F`/`Esc` 全屏、`C` 复制、`S` 另存为、`O` 外部打开，
//! 底部一条悬浮工具条 + 左右两枚圆形侧箭头（只在多图时出现），顶部一条 1.6 秒自行收起的提示。
//!
//! 三处与 Qt 版做不到完全等价，都是 windui 的能力边界，取舍写在这里而不是藏在代码里：
//! - **全屏**用最大化/还原近似：windui 没有全屏 API，而原版的全屏本来就是「撑满可用区 + Esc 退出」，
//!   最大化在观感与键位上都对得上。
//! - **旋转是烘进像素的**：`Canvas::draw_image` 没有旋转变换（`save/restore` 只管裁剪），所以转过的
//!   角度交给后台线程重画一份位图，回包再换上来。代价是一次旋转有一帧延迟，换来的是零变换路径。
//! - **提示条自绘**而不是用 `ctx.toast_*`：另存为的结果回调拿不到 `EventCtx`，只有原子的
//!   `FnOnce(Option<PathBuf>)`；提示态挂在会话上、由自绘覆盖层显示，保存失败也才有地方说话。

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use windui::core::{ClickFn, EventCtx, Widget};
use windui::event::{Event, Key, KeyEvent, MouseButton, PointerEvent, PointerKind};
use windui::prelude::*;
use windui::render::{Canvas, Paint};
use windui::signal::SignalScope;
use windui::text::TextEngine;

use crate::assets;
use crate::theme;
use crate::utils::{clipboard, pick};

/// 轮询后台解码回包的间隔：一帧半的量级，够快又不至于每帧去锁一次通道。
const POLL: Duration = Duration::from_millis(80);
/// 顶部提示条的停留时长，与原版 `QTimer::singleShot(1600, …)` 一致。
const TOAST_HOLD: Duration = Duration::from_millis(1600);
/// 缩放读数那一格的宽度：内容形如 `100%  1200×800  ·  适应`，原版给的是 minWidth 120，
/// 但那句还带尺寸与模式，120 会一路截断，按实测放宽到 190。
const ZOOM_LABEL_WIDTH: i32 = 190;

/// 一个预览窗的全部可变状态，UI 线程独占（`Rc<RefCell<…>>`，全程不跨线程）。
type Shared = Rc<RefCell<Session>>;

/// 工具条与标题栏要跟着变的文本/开关。
///
/// 全部在 [`SignalScope`] 里创建：子窗的 chrome 是在**开窗之前**建好的，落在全局 arena 里
/// 就永远不会回收（关一次窗漏一批信号）。收进作用域、作用域又存进 [`Session`]，关窗即 dispose。
#[derive(Clone, Copy)]
struct Chrome {
    /// `文件名  [i/n]`，原版 `setWindowTitle(QString("%2  [%1/%3]"))`。
    title: Signal<String>,
    /// `NN%  宽×高  ·  适应/手动`。
    zoom: Signal<String>,
    prev_on: Signal<bool>,
    next_on: Signal<bool>,
    toast: Signal<String>,
    toast_visible: Signal<bool>,
    /// 每点一次工具条按钮自增：windui 的按钮按下会抢焦点，抢回来快捷键才失灵不了。
    focus: Signal<u64>,
}

struct Session {
    /// 本次可预览的图片列表，开窗时定下、中途不变（与原版一样不做动态刷新）。
    files: Vec<PathBuf>,
    index: usize,
    /// 已经烘进 `picture` 的角度，同时记住「从 `base` 转过去要转多少」。
    rotation: i32,
    fit_mode: bool,
    /// 手动缩放倍率。`fit_mode` 为真时它不参与显示，只是「切回手动」的起点。
    scale: f64,
    /// 平移量（相对居中位）。
    offset: (i32, i32),
    /// 画布当前的内容矩形，`paint` 每帧回写：适应比例、锚点、居中都靠它。
    view: Rect,
    dragging: bool,
    last: Point,
    /// 当前这张图的加载批次号：换图后旧回包按批次丢弃，不会把上一张糊回来。
    revision: u64,
    /// 未旋转的原图，旋转的取材。
    base: Option<Arc<image::RgbaImage>>,
    /// 上屏的那份（已旋转）。
    picture: Option<Rc<Image>>,
    loading: bool,
    failed: bool,
    /// 提示条的收起时刻，`None` = 没有待收的提示。
    toast_at: Option<Instant>,
    jobs: std::sync::mpsc::Sender<Job>,
    results: RefCell<std::sync::mpsc::Receiver<Outcome>>,
    chrome: Chrome,
    /// chrome 信号的归属地，随 Session 一起析构即回收。
    _scope: SignalScope,
}

/// 交给后台线程的活：解码原图，或把已有的原图转个角度。两条都只搬裸像素——`Image` 内含 `Rc`，
/// `!Send`，构造必须留在 UI 线程。
enum Job {
    Load { path: PathBuf, revision: u64 },
    Rotate { pixels: Arc<image::RgbaImage>, degrees: i32, revision: u64 },
}

/// 后台回包。`is_base` 为真时这份像素是新加载的原图，要同时当作旋转的取材。
struct Outcome {
    revision: u64,
    pixels: Option<Arc<image::RgbaImage>>,
    is_base: bool,
}

/// 借用结束后才交给 `EventCtx` 的事：剪贴板、另存为、外部程序、窗口动作。
/// 攒成枚举再执行，是为了不把 `RefMut` 拖过 `ctx` 调用。
enum Task {
    Copy { rgba: Vec<u8>, width: u32, height: u32 },
    Save { picture: Rc<Image>, name: String },
    Open(PathBuf),
    Maximize,
    Close,
}

/// 工具条按钮与画布快捷键共用的一套动作。
#[derive(Clone, Copy)]
enum Action {
    Prev,
    Next,
    ZoomIn,
    ZoomOut,
    Fit,
    Actual,
    RotateLeft,
    RotateRight,
    Copy,
    Save,
    Open,
    ToggleFull,
    Close,
}

/// 开一个预览窗。`files` 是聚合出来的可预览图片，`index` 是被点的那张在其中的位置。
pub fn open(ctx: &mut EventCtx, files: Vec<PathBuf>, index: usize) {
    let shared = Session::spawn(files, index);
    let title = shared.borrow().chrome.title;
    let icon = assets::window_icon();
    let interval_shared = Rc::clone(&shared);
    let content_shared = Rc::clone(&shared);
    let mut builder = Window::new("图片预览", theme::PREVIEW_WIDTH, theme::PREVIEW_HEIGHT)
        .title(title)
        .owned(true)
        // 与原版一样居中在发起窗口上（`owned` + `centered`）
        .centered(true)
        .min_size(theme::PREVIEW_MIN_WIDTH, theme::PREVIEW_MIN_HEIGHT)
        .bg(theme::PREVIEW_BG)
        .on_interval(POLL, move |ctx| drain(&interval_shared, ctx));
    if let Some(icon) = icon {
        builder = builder.icon(icon);
    }
    ctx.open_window(builder.content(move || window(content_shared)));
}

impl Session {
    /// 建会话、起后台线程、投第一张。只能在 UI 线程调（`signal()` 是线程局部的）。
    fn spawn(files: Vec<PathBuf>, index: usize) -> Shared {
        let (jobs_tx, jobs_rx) = std::sync::mpsc::channel::<Job>();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<Outcome>();
        std::thread::spawn(move || worker(jobs_rx, done_tx));

        let mut scope = SignalScope::new();
        let chrome = scope.collect(|| Chrome {
            title: signal(String::new()),
            zoom: signal(String::new()),
            prev_on: signal(false),
            next_on: signal(false),
            toast: signal(String::new()),
            toast_visible: signal(false),
            focus: signal(0u64),
        });
        let shared = Rc::new(RefCell::new(Self {
            files,
            index,
            rotation: 0,
            fit_mode: true,
            scale: 1.0,
            offset: (0, 0),
            view: Rect::new(0, 0, 0, 0),
            dragging: false,
            last: Point::new(0, 0),
            revision: 0,
            base: None,
            picture: None,
            loading: false,
            failed: false,
            toast_at: None,
            jobs: jobs_tx,
            results: RefCell::new(done_rx),
            chrome,
            _scope: scope,
        }));
        shared.borrow_mut().load_current();
        shared
    }

    fn file(&self) -> Option<PathBuf> {
        self.files.get(self.index).cloned()
    }

    /// 被点的那张的文件名，另存为的默认名。
    fn name(&self) -> String {
        self.files
            .get(self.index)
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "图片.png".to_owned())
    }

    /// 换到当前下标的图：姿态全部复位，与原版 `loadImage` 一致
    /// （`rotation=0; fitMode=true; offset=0; scale=1`）。
    fn load_current(&mut self) {
        self.revision += 1;
        self.rotation = 0;
        self.fit_mode = true;
        self.scale = 1.0;
        self.offset = (0, 0);
        self.dragging = false;
        self.base = None;
        self.picture = None;
        self.failed = false;
        match self.file() {
            Some(path) => {
                self.loading = true;
                if self.jobs.send(Job::Load { path, revision: self.revision }).is_err() {
                    // 后台线程已经没了：直接判失败，不能让画布永远停在「转圈」。
                    self.loading = false;
                    self.failed = true;
                }
            }
            None => {
                self.loading = false;
                self.failed = true;
            }
        }
        self.sync_chrome();
    }

    /// 上一张 / 下一张。多图才允许翻，越界就原地不动（原版的 `setEnabled` 也是这个意思）。
    fn step(&mut self, delta: i32) {
        if self.files.len() < 2 {
            return;
        }
        let last = self.files.len() as i32 - 1;
        let next = (self.index as i32 + delta).clamp(0, last);
        if next as usize == self.index {
            return;
        }
        self.index = next as usize;
        self.load_current();
    }

    /// 显示尺寸就是 `picture` 自身的尺寸——旋转已经烘进像素，不再像原版那样按角度互换宽高。
    fn display_size(&self) -> (u32, u32) {
        match &self.picture {
            Some(image) => (image.width(), image.height()),
            None => (0, 0),
        }
    }

    /// 视口中心（画布内容矩形的中心），工具条上的放大/缩小以它为锚。
    fn center(&self) -> Point {
        Point::new(self.view.x + self.view.w / 2, self.view.y + self.view.h / 2)
    }

    /// 适应窗口时的倍率：把图完整塞进可视区，取两轴里更小的那个。
    fn fit_scale(&self) -> f64 {
        let (width, height) = self.display_size();
        if width == 0 || height == 0 || self.view.w <= 0 || self.view.h <= 0 {
            return 1.0;
        }
        (self.view.w as f64 / width as f64).min(self.view.h as f64 / height as f64)
    }

    fn effective_scale(&self) -> f64 {
        if self.fit_mode { self.fit_scale() } else { self.scale }
    }

    /// 缩放。`anchor` 是屏幕上的锚点，缩放后它底下的图像点保持不动——原版的
    /// `m_offset = pivot - (pivot - m_offset) * (new / cur)`，抄的是同一个式子。
    fn apply_scale(&mut self, target: f64, anchor: Point) {
        // 没图就不动：原版 `applyScale` 开头那句 `if (!m_loaded) return;`
        if self.picture.is_none() {
            return;
        }
        let current = self.effective_scale();
        if current <= 0.0 || !current.is_finite() {
            return;
        }
        let next = target.clamp(theme::ZOOM_MIN, theme::ZOOM_MAX);
        if (next - current).abs() < 1e-9 {
            return;
        }
        if self.fit_mode {
            // 从「适应」转手动：先把当下这份适应倍率固化成自由缩放，画面才不会跳一下
            self.scale = current;
            self.fit_mode = false;
        }
        let center = self.center();
        let ratio = next / current;
        let pivot = (anchor.x - center.x, anchor.y - center.y);
        self.offset = (
            (pivot.0 as f64 - (pivot.0 - self.offset.0) as f64 * ratio).round() as i32,
            (pivot.1 as f64 - (pivot.1 - self.offset.1) as f64 * ratio).round() as i32,
        );
        self.scale = next;
        self.refresh_zoom();
    }

    /// 旋转：改角度并把原图重烘一份交给后台。旧图先留着，新回包到了再换，中间不闪白。
    fn rotate(&mut self, clockwise: bool) {
        if self.base.is_none() {
            return;
        }
        self.rotation = if clockwise { (self.rotation + 90) % 360 } else { (self.rotation + 270) % 360 };
        let degrees = self.rotation;
        let revision = self.revision;
        if let Some(base) = self.base.clone() {
            let _ = self.jobs.send(Job::Rotate { pixels: base, degrees, revision });
        }
    }

    fn fit_window(&mut self) {
        self.fit_mode = true;
        self.offset = (0, 0);
        self.refresh_zoom();
    }

    fn actual_size(&mut self) {
        self.fit_mode = false;
        self.scale = 1.0;
        self.offset = (0, 0);
        self.refresh_zoom();
    }

    fn flash(&mut self, text: &str) {
        self.chrome.toast.set(text.to_owned());
        self.chrome.toast_visible.set(true);
        self.toast_at = Some(Instant::now() + TOAST_HOLD);
    }

    fn refresh_zoom(&self) {
        let text = match &self.picture {
            None => "0%".to_owned(),
            Some(_) => {
                let percent = (self.effective_scale() * 100.0).round() as i64;
                let (width, height) = self.display_size();
                format!("{}%  {}×{}  ·  {}", percent, width, height, if self.fit_mode { "适应" } else { "手动" })
            }
        };
        // 先比一次再写：`resize` 每帧都会走到这里，无谓 bump 就是每帧重建一次那一格
        if self.chrome.zoom.get() != text {
            self.chrome.zoom.set(text);
        }
    }

    fn sync_chrome(&self) {
        self.chrome.title.set(format!("{}  [{}/{}]", self.name(), self.index + 1, self.files.len()));
        let multi = self.files.len() > 1;
        self.chrome.prev_on.set(multi && self.index > 0);
        self.chrome.next_on.set(multi && self.index + 1 < self.files.len());
        self.refresh_zoom();
    }
}

/// 后台线程主体：`recv` 到任务就干活、回包。`Sender` 随 Session 析构 → `recv` 报错 → 线程收摊；
/// 反过来 `Receiver` 先没了 → `send` 报错 → 同样收摊。两条退出路径都不需要额外标志。
fn worker(jobs: std::sync::mpsc::Receiver<Job>, out: std::sync::mpsc::Sender<Outcome>) {
    while let Ok(job) = jobs.recv() {
        let (revision, pixels, is_base) = match job {
            Job::Load { path, revision } => (revision, decode(&path), true),
            Job::Rotate { pixels, degrees, revision } => (revision, Some(rotate(&pixels, degrees)), false),
        };
        if out.send(Outcome { revision, pixels, is_base }).is_err() {
            return;
        }
    }
}

/// 读文件 -> 全解 -> 超长边就等比缩到 `PREVIEW_MAX_EDGE` 以内。解不出（坏图、没落盘）返回 `None`。
fn decode(path: &PathBuf) -> Option<Arc<image::RgbaImage>> {
    let bytes = std::fs::read(path).ok()?;
    let picture = image::load_from_memory(&bytes).ok()?.into_rgba8();
    let (width, height) = (picture.width(), picture.height());
    if width == 0 || height == 0 {
        return None;
    }
    let longest = width.max(height);
    if longest <= theme::PREVIEW_MAX_EDGE {
        return Some(Arc::new(picture));
    }
    let ratio = theme::PREVIEW_MAX_EDGE as f64 / longest as f64;
    let target = (
        (width as f64 * ratio).round().max(1.0) as u32,
        (height as f64 * ratio).round().max(1.0) as u32,
    );
    Some(Arc::new(image::imageops::resize(&picture, target.0, target.1, image::imageops::FilterType::Triangle)))
}

/// 按角度转成方向一致的位图。`rotation` 恒是 90 的倍数，走 `imageops` 的整角快路径。
fn rotate(pixels: &image::RgbaImage, degrees: i32) -> Arc<image::RgbaImage> {
    let turned = match degrees.rem_euclid(360) {
        90 => image::imageops::rotate90(pixels),
        180 => image::imageops::rotate180(pixels),
        270 => image::imageops::rotate270(pixels),
        _ => pixels.clone(),
    };
    Arc::new(turned)
}

/// 周期排空回包 + 收提示条。
fn drain(shared: &Shared, ctx: &mut EventCtx) {
    let repaint = {
        let mut session = shared.borrow_mut();
        let mut repaint = false;
        if let Some(deadline) = session.toast_at {
            if Instant::now() >= deadline {
                session.toast_at = None;
                session.chrome.toast_visible.set(false);
                repaint = true;
            }
        }
        loop {
            let Ok(outcome) = session.results.borrow_mut().try_recv() else { break };
            // 批次不对 = 这张图已经被换掉了（翻得比解得快），回包直接丢
            if outcome.revision != session.revision {
                continue;
            }
            session.loading = false;
            match outcome.pixels {
                Some(pixels) => {
                    if outcome.is_base {
                        session.base = Some(Arc::clone(&pixels));
                    }
                    match Image::from_rgba(pixels.width(), pixels.height(), pixels.as_raw()) {
                        Ok(image) => {
                            session.picture = Some(Rc::new(image));
                            session.failed = false;
                        }
                        Err(_) => {
                            session.picture = None;
                            session.failed = true;
                        }
                    }
                }
                None => {
                    session.picture = None;
                    session.base = None;
                    session.failed = true;
                }
            }
            session.sync_chrome();
            repaint = true;
        }
        repaint
    };
    if repaint {
        ctx.mark_dirty_all();
    }
}

/// 动作落实。借用会话算完状态、攒出要交给 `ctx` 的事，退出借用后再做。
fn run(action: Action, shared: &Shared, ctx: &mut EventCtx) {
    let task = {
        let mut session = shared.borrow_mut();
        match action {
            Action::Prev => {
                session.step(-1);
                None
            }
            Action::Next => {
                session.step(1);
                None
            }
            Action::ZoomIn => {
                let (center, target) = (session.center(), session.effective_scale() * theme::ZOOM_STEP);
                session.apply_scale(target, center);
                None
            }
            Action::ZoomOut => {
                let (center, target) = (session.center(), session.effective_scale() / theme::ZOOM_STEP);
                session.apply_scale(target, center);
                None
            }
            Action::Fit => {
                session.fit_window();
                None
            }
            Action::Actual => {
                session.actual_size();
                None
            }
            Action::RotateLeft => {
                session.rotate(false);
                None
            }
            Action::RotateRight => {
                session.rotate(true);
                None
            }
            Action::Copy => match session.picture.clone() {
                Some(picture) => Some(Task::Copy {
                    rgba: picture.to_rgba(),
                    width: picture.width(),
                    height: picture.height(),
                }),
                None => {
                    session.flash("还没有可以复制的图片");
                    None
                }
            },
            Action::Save => match session.picture.clone() {
                Some(picture) => Some(Task::Save { picture, name: session.name() }),
                None => {
                    session.flash("还没有可以保存的图片");
                    None
                }
            },
            Action::Open => session.file().map(Task::Open),
            Action::ToggleFull => Some(Task::Maximize),
            Action::Close => Some(Task::Close),
        }
    };
    match task {
        None => ctx.mark_dirty(),
        Some(Task::Copy { rgba, width, height }) => {
            // 失败原因由 `clipboard` 给到「哪一步、什么错」：这里以前只收一个 `bool`，
            // 于是 Win32 任何一步出错都念成「该平台暂不支持复制图片」，把排查方向带偏。
            let text = match clipboard::set_image(&rgba, width, height) {
                Ok(()) => "已复制到剪贴板".to_owned(),
                Err(error) => error.to_string(),
            };
            shared.borrow_mut().flash(&text);
            ctx.mark_dirty_all();
        }
        Some(Task::Save { picture, name }) => {
            let shared = Rc::clone(shared);
            let dialog = PickDialog::new()
                .title("另存为")
                .file_name(name)
                .filter("图片", &["png", "jpg", "jpeg", "bmp", "gif", "webp"]);
            ctx.request_save_file(dialog, move |chosen| {
                // 回调在 UI 线程、但没有 `EventCtx`：提示只能写回会话，让自绘覆盖层去显
                let Some(chosen) = chosen else { return };
                let chosen = with_extension(chosen);
                let rgba = picture.to_rgba();
                let failed = match image::RgbaImage::from_raw(picture.width(), picture.height(), rgba) {
                    Some(buffer) => image::DynamicImage::ImageRgba8(buffer).save(&chosen).is_err(),
                    // 宽高与像素数对不上只可能是位图本身坏了，与「写盘失败」同一句提示
                    None => true,
                };
                if failed {
                    shared.borrow_mut().flash("保存失败");
                }
            });
        }
        Some(Task::Open(path)) => ctx.defer_blocking(move || pick::open(&path)),
        Some(Task::Maximize) => ctx.toggle_maximize(),
        Some(Task::Close) => ctx.request_close(),
    }
}

/// 另存为没写后缀就补一个 `.png`：`image` 是按扩展名决定编码格式的，没扩展名直接报「不支持的格式」。
fn with_extension(path: PathBuf) -> PathBuf {
    if path.extension().is_some() {
        return path;
    }
    path.with_extension("png")
}

// ————————————————————————— 元素树 —————————————————————————

/// 预览窗根：画布铺满，侧箭头、工具条、提示条依次压在它上面（后挂的先命中）。
fn window(shared: Shared) -> Element {
    let chrome = shared.borrow().chrome;
    let multi = shared.borrow().files.len() > 1;
    let canvas = Element::leaf()
        .fill()
        .bg(theme::PREVIEW_BG)
        .widget(ImageCanvas { shared: Rc::clone(&shared) })
        .autofocus()
        .focus_when(chrome.focus);
    let mut stack = Element::stack().fill().bg(theme::PREVIEW_BG).child(canvas).child(toast(&chrome));
    if multi {
        stack = stack
            .child(bar_button(side_arrow("arrow_back", true, "上一张 (←)"), Action::Prev, &shared, &chrome).enabled_signal(chrome.prev_on))
            .child(bar_button(side_arrow("arrow_forward", false, "下一张 (→)"), Action::Next, &shared, &chrome).enabled_signal(chrome.next_on));
    }
    stack.child(toolbar(&shared, &chrome))
}

/// 底部悬浮工具条：半透深底 + 圆角，居中贴在窗口下沿。
///
/// 定位不硬算宽度（原版是 `((width - tw) / 2, h - th - 14)`）：`align_xy` + `margin_edges`
/// 让 windui 自己居中收底，窗口比工具条窄时两头等量裁掉——和 Qt 那个负数坐标的结果一致。
fn toolbar(shared: &Shared, chrome: &Chrome) -> Element {
    let row = Element::row()
        .cross(Align::Center)
        .spacing(2)
        .padding_edges(Insets::new(8, 4, 8, 4))
        .bg(theme::PREVIEW_BAR_BG)
        .corner(10.0)
        .align_xy(Align::Center, Align::End)
        .margin_edges(Insets::new(0, 0, 0, theme::BAR_BOTTOM_MARGIN))
        .child(icon_button("arrow_back", "上一张 (←)", Action::Prev, shared, chrome).enabled_signal(chrome.prev_on))
        .child(icon_button("arrow_forward", "下一张 (→)", Action::Next, shared, chrome).enabled_signal(chrome.next_on))
        .child(spacer())
        .child(text_button("−", "缩小 (-)", Action::ZoomOut, shared, chrome))
        .child(
            Element::label(chrome.zoom)
                .width(ZOOM_LABEL_WIDTH)
                .text_align(Align::Center)
                .max_lines(1)
                .truncate(Truncate::End)
                .font_size(13.0)
                .fg(theme::PREVIEW_ZOOM_FG),
        )
        .child(text_button("+", "放大 (+)", Action::ZoomIn, shared, chrome))
        .child(spacer())
        .child(text_button("适应", "适应窗口 (F)", Action::Fit, shared, chrome))
        .child(text_button("原始", "原始尺寸 (1)", Action::Actual, shared, chrome))
        .child(spacer())
        .child(icon_button("rotate_left", "向左旋转 (R)", Action::RotateLeft, shared, chrome))
        .child(icon_button("rotate_right", "向右旋转 (Shift+R)", Action::RotateRight, shared, chrome))
        .child(spacer())
        .child(text_button("复制", "复制到剪贴板 (C)", Action::Copy, shared, chrome))
        .child(icon_button("save", "另存为 (S)", Action::Save, shared, chrome))
        .child(icon_button("open_in_new", "用外部程序打开 (O)", Action::Open, shared, chrome))
        .child(icon_button("fullscreen", "全屏 (F / Esc)", Action::ToggleFull, shared, chrome));
    row
}

/// 组间那 6 像素的空档，与原版 `addSpacing(6)` 对应。
fn spacer() -> Element {
    Element::leaf().width(6).height(1)
}

/// 工具条上的矢量图标按钮。
///
/// 底色走 `.bg()` 而不是 `.intent(..)`：windui 的 `intent()` 只认 `Button` / `CheckBox`，
/// `icon_button_content` 用的是 `IconButton`，挂上去在 debug 构建里直接 panic。
/// 节点底色 + `IconButton` 自带的 hover/press 文字色叠层，正是这一排按钮要的效果。
fn icon_button(name: &str, tip: &str, action: Action, shared: &Shared, chrome: &Chrome) -> Element {
    let element = Element::icon_button_content(assets::md_icon_content(name, theme::PREVIEW_FG))
        .height(30)
        .bg(theme::PREVIEW_BUTTON)
        .corner(theme::PREVIEW_BUTTON_CORNER)
        .tooltip(tip.to_owned());
    bar_button(element, action, shared, chrome)
}

/// 工具条上的文字按钮，原文案照抄（`−` `+` `适应` `原始` `复制`）。
fn text_button(label: &str, tip: &str, action: Action, shared: &Shared, chrome: &Chrome) -> Element {
    let element = Element::button(label)
        .height(30)
        .intent(Intent::Custom(theme::PREVIEW_BUTTON))
        .tooltip(tip.to_owned());
    bar_button(element, action, shared, chrome)
}

/// 给按钮挂上「执行动作 + 把焦点夺回画布」。windui 的按钮按下会 `request_focus`，
/// 不夺回来的话，点过一次工具条之后所有快捷键就没人接了。
fn bar_button(element: Element, action: Action, shared: &Shared, chrome: &Chrome) -> Element {
    let session = Rc::clone(shared);
    let focus = chrome.focus;
    element.on_click(move |ctx| {
        run(action, &session, ctx);
        focus.update(|count| *count += 1);
    })
}

/// 左右两枚 44×44 的圆形翻图箭头。原版只在多图时出现，首/尾各自禁用。
fn side_arrow(name: &str, left: bool, tip: &str) -> Element {
    let align = if left { Align::Start } else { Align::End };
    let margin = Insets::new(
        if left { theme::ARROW_MARGIN } else { 0 },
        0,
        if left { 0 } else { theme::ARROW_MARGIN },
        0,
    );
    Element::stack()
        .size(theme::ARROW_SIZE, theme::ARROW_SIZE)
        .bg(theme::PREVIEW_ARROW_BG)
        .corner(theme::ARROW_SIZE as f32 / 2.0)
        .align_xy(align, Align::Center)
        .margin_edges(margin)
        .clickable()
        .tooltip(tip.to_owned())
        .child(assets::md_icon(name, theme::PREVIEW_FG, 22).align_xy(Align::Center, Align::Center))
}

/// 顶部居中、1.6 秒自行收起的提示条。原版 `statusBarFlash` 落的是状态栏，这里画成覆盖层。
fn toast(chrome: &Chrome) -> Element {
    Element::row()
        .cross(Align::Center)
        .padding_edges(Insets::new(14, 6, 14, 6))
        .bg(theme::PREVIEW_BAR_BG)
        .corner(8.0)
        .align_xy(Align::Center, Align::Start)
        .margin_edges(Insets::new(0, 16, 0, 0))
        .child(
            Element::label(chrome.toast)
                .font_size(13.0)
                .max_lines(1)
                .fg(theme::PREVIEW_FG),
        )
        .visible_signal(chrome.toast_visible)
}

// ————————————————————————— 画布 —————————————————————————

/// 图片画布：自己吞命中、自己接键盘，缩放/平移/旋转/翻图全在这里认事件。
struct ImageCanvas {
    shared: Shared,
}

impl Widget for ImageCanvas {
    fn measure(&self, avail: Size, _style: &Style, _text: &mut dyn TextEngine) -> Size {
        avail
    }

    fn paint(
        &self,
        bounds: Rect,
        content: Rect,
        _focused: bool,
        _enabled: bool,
        canvas: &mut dyn Canvas,
        _style: &Style,
    ) {
        let mut session = self.shared.borrow_mut();
        // 每帧回写视口：适应比例、缩放锚点、居中都按它算，窗口一 resize 就跟着变。
        // 尺寸真变过还得补一次读数：适应档下倍率跟着视口走，原版也是在 `resizeEvent` 里刷的。
        let resized = session.view != content;
        session.view = content;
        canvas.fill_rect(bounds.x as f32, bounds.y as f32, bounds.w as f32, bounds.h as f32, &Paint::fill(theme::PREVIEW_BG));
        let scale = session.effective_scale();
        match &session.picture {
            Some(picture) => {
                let width = ((picture.width() as f64 * scale).round() as i32).max(1);
                let height = ((picture.height() as f64 * scale).round() as i32).max(1);
                let cx = content.x + content.w / 2 + session.offset.0;
                let cy = content.y + content.h / 2 + session.offset.1;
                // 目标框与图同比例，`Contain` 在这里等价于 1:1 落位，只是保证绝不溢出 dst
                canvas.draw_image(picture, Rect::new(cx - width / 2, cy - height / 2, width, height), Fit::Contain, 0.0, 1.0);
            }
            None => {
                if !session.loading {
                    // 原版的失败文案：白字居中「无法加载图片」
                    let text = if session.failed { "无法加载图片" } else { "加载中…" };
                    canvas.draw_text(text, content, Color::WHITE, Align::Center, &windui::text::TextStyle::new(14.0));
                }
            }
        }
        if resized {
            session.refresh_zoom();
        }
    }

    fn on_event(&mut self, ctx: &mut EventCtx, event: &Event) -> bool {
        match event {
            Event::Pointer(pointer) => self.pointer(ctx, pointer),
            Event::Key(key) => self.key(ctx, key),
        }
    }

    fn focusable(&self) -> bool {
        true
    }

    /// 吞下命中：画布是整窗最底一层，不该让指针穿到它下面去。
    fn hit_opaque(&self) -> bool {
        true
    }

    fn cursor(&self) -> CursorShape {
        // 原版是 OpenHand / ClosedHand，windui 只有手型与四向箭头，取语义最近的一对
        if self.shared.borrow().dragging { CursorShape::SizeAll } else { CursorShape::Hand }
    }
}

impl ImageCanvas {
    fn pointer(&mut self, ctx: &mut EventCtx, event: &PointerEvent) -> bool {
        match event.kind {
            PointerKind::Wheel(steps) => {
                let mut session = self.shared.borrow_mut();
                let target = session.effective_scale() * theme::ZOOM_STEP.powf(wheel_notches(steps));
                session.apply_scale(target, event.pos);
                true
            }
            PointerKind::Down => {
                if event.button != MouseButton::Left {
                    return false;
                }
                if event.click_count >= 2 {
                    // 双击在「适应」与「原始尺寸」之间切换
                    let mut session = self.shared.borrow_mut();
                    if session.fit_mode {
                        session.actual_size();
                    } else {
                        session.fit_window();
                    }
                    ctx.mark_dirty();
                    return true;
                }
                let mut session = self.shared.borrow_mut();
                session.dragging = true;
                session.last = event.pos;
                ctx.capture();
                ctx.mark_dirty();
                true
            }
            PointerKind::Up => {
                if event.button != MouseButton::Left {
                    return false;
                }
                let mut session = self.shared.borrow_mut();
                session.dragging = false;
                ctx.release_capture();
                ctx.mark_dirty();
                true
            }
            PointerKind::Move => {
                let mut session = self.shared.borrow_mut();
                if !session.dragging {
                    return false;
                }
                session.offset = (
                    session.offset.0 + (event.pos.x - session.last.x),
                    session.offset.1 + (event.pos.y - session.last.y),
                );
                session.last = event.pos;
                ctx.mark_dirty();
                true
            }
            _ => false,
        }
    }

    fn key(&mut self, ctx: &mut EventCtx, event: &KeyEvent) -> bool {
        // 与原版一致地不拦 Ctrl：Qt 的 `event->key()` 根本不看修饰键，`Ctrl+C` / `Ctrl+S` 在原版
        // 里就是复制到剪贴板与另存为，而用户复制图片时本能就是按 Ctrl+C。Alt 与 Meta 仍要让开，
        // 那是系统与应用级热键的领地。
        if !event.pressed || event.alt || event.meta {
            return false;
        }
        let action = match event.key {
            Key::NumpadAdd | Key::Char('+') | Key::Char('=') => Some(Action::ZoomIn),
            Key::NumpadSubtract | Key::Char('-') | Key::Char('_') => Some(Action::ZoomOut),
            Key::Char('0') => Some(Action::Fit),
            Key::Char('1') => Some(Action::Actual),
            Key::Left => Some(Action::Prev),
            Key::Right => Some(Action::Next),
            Key::Escape => {
                Some(if ctx.window_state().maximized { Action::ToggleFull } else { Action::Close })
            }
            Key::Char(letter) => match letter.to_ascii_lowercase() {
                'r' => Some(if event.shift { Action::RotateLeft } else { Action::RotateRight }),
                'a' => Some(Action::Prev),
                'd' => Some(Action::Next),
                'f' => Some(Action::ToggleFull),
                'c' => Some(Action::Copy),
                's' => Some(Action::Save),
                'o' => Some(Action::Open),
                _ => None,
            },
            _ => None,
        };
        match action {
            Some(action) => {
                run(action, &self.shared, ctx);
                true
            }
            None => false,
        }
    }
}

/// 滚轮一格折成多少「档」。Windows 一格是 ±120，触控板与 macOS 送的是像素级小增量，
/// 直接除以 120 会得到 0.08 这种几乎看不见的缩放，所以小于半格就按一整档算。
fn wheel_notches(steps: i32) -> f64 {
    if steps.abs() < 60 {
        steps.signum() as f64
    } else {
        steps as f64 / 120.0
    }
}

/// 图片气泡上的点击捕获：按下再松开落在同一块里算一次「点了」，除此之外什么都不画。
///
/// 不用 `Element::clickable()`：它自带 hover/press 叠层，而原版 `ImageBubble` 按下不变色。
/// 挂在本节点上的右键不受影响——非左键一律返回 `false`，照常冒泡给气泡的上下文菜单。
pub struct BubbleClick {
    press: Option<Point>,
    click: Option<ClickFn>,
}

impl BubbleClick {
    pub fn new() -> Self {
        Self { press: None, click: None }
    }
}

impl Widget for BubbleClick {
    fn on_event(&mut self, ctx: &mut EventCtx, event: &Event) -> bool {
        let Event::Pointer(event) = event else { return false };
        if event.button != MouseButton::Left {
            return false;
        }
        match event.kind {
            PointerKind::Down => {
                self.press = Some(event.pos);
                ctx.capture();
                true
            }
            PointerKind::Up => {
                let inside = ctx.bounds().contains(event.pos);
                let pressed_here = self.press.take().is_some_and(|at| ctx.bounds().contains(at));
                ctx.release_capture();
                if inside && pressed_here {
                    if let Some(click) = self.click.as_mut() {
                        click(ctx);
                    }
                }
                true
            }
            _ => false,
        }
    }

    fn take_click(&mut self, click: ClickFn) {
        self.click = Some(click);
    }

    fn hit_opaque(&self) -> bool {
        true
    }

    fn cursor(&self) -> CursorShape {
        CursorShape::Hand
    }
}
