//! 主题与尺寸 token：色值与几何一律对齐 LANShare-PC（Qt 版）的 Material You 观感。
//!
//! 三份配色档案抄自 `style_light.qss` / `style_dark.qss` / `style_emerald.qss` 的实测值，
//! 气泡几何抄自 `ChatItemWidget` 的布局常量（头像 40、文件图块 52、卡片上限 360、圆角 16 / 尾巴 6）。
//! 颜色优先直接给 `Color` 而不是借 `Role`：原版每一档都有确切十六进制值，
//! 走角色反而会被 windui 的默认调色板稀释掉。字号不走死数字：由配置里的磅值
//! 换算成 `Metrics` 的五档字号，换主题或改字号时整棵树自动重排。

use windui::prelude::{Color, Theme};

// ————————————————————— 布局尺寸（与主题无关） —————————————————————

/// 侧栏宽度（逻辑像素）。
pub const SIDEBAR_WIDTH: i32 = 320;
/// 顶部标题栏高度。
pub const HEADER_HEIGHT: i32 = 68;

pub const FONT_TITLE: f32 = 20.0;
pub const FONT_BODY: f32 = 14.0;
pub const FONT_META: f32 = 12.0;
pub const FONT_SMALL: f32 = 11.0;

pub const RADIUS_CARD: f32 = 8.0;

/// 工具栏 / 操作按钮里 MD 矢量图标的边长。
pub const ICON: i32 = 20;

/// 气泡主体圆角，抄 `ChatTheme::bubbleRadius()`。
pub const BUBBLE_RADIUS: f32 = 16.0;
/// 靠头像一侧的「尾巴」小圆角，抄 `ChatTheme::bubbleTailRadius()`。
pub const BUBBLE_TAIL: f32 = 6.0;
/// 气泡宽度上限，抄 `availableBubbleWidth()` 的 `qBound` 上界。
pub const BUBBLE_MAX_WIDTH: i32 = 500;

/// 消息行的外边距与列间距，抄 `ChatItemWidget::build()`。
pub const ROW_MARGIN_X: i32 = 12;
pub const ROW_MARGIN_Y: i32 = 8;
pub const ROW_SPACING: i32 = 10;
/// 昵称与气泡之间的距离（`m_column->setSpacing(4)`）。
pub const COLUMN_SPACING: i32 = 4;

/// 头像圆形容器与其中的设备图标（`kAvatarSize` / `kIconSize`）。
pub const AVATAR_SIZE: i32 = 40;
pub const AVATAR_ICON: i32 = 24;
/// 昵称字号：Qt 用 8pt，换算到逻辑像素约 11px。
pub const NICKNAME_FONT: f32 = 11.0;

/// 文本气泡内边距，抄 `TextBubble` 的 `(kPadding, 12, kPadding, 12)`。
pub const TEXT_PAD_X: i32 = 20;
pub const TEXT_PAD_Y: i32 = 12;

/// 文件卡片：宽度上限、图标边长、两列间距与内边距，抄 `FileBubble`。
pub const FILE_CARD_MAX_WIDTH: i32 = 360;
/// 文件卡片宽度下限（`setMinimumWidth(240)`），太窄时名称会挤成一列竖排。
pub const FILE_CARD_MIN_WIDTH: i32 = 240;
pub const FILE_ICON: i32 = 52;
pub const FILE_GAP: i32 = 12;
pub const FILE_PAD_X: i32 = 12;
pub const FILE_PAD_Y: i32 = 10;
pub const FILE_INFO_SPACING: i32 = 4;
/// 文件名与大小都用 13px，文件名加粗——原版的层级对比靠字重而非字号。
pub const FILE_TEXT_FONT: f32 = 13.0;

/// 进度条宽度上限（`m_progress->setMaximumWidth(160)`）与条高（`setFixedHeight(8)`）。
pub const PROGRESS_MAX_WIDTH: i32 = 160;
pub const PROGRESS_HEIGHT: i32 = 8;
/// 状态胶囊：圆角 12、高 24、徽标 16、左右内边距 7/10、徽标与文字间距 5。
pub const CHIP_RADIUS: f32 = 12.0;
pub const CHIP_HEIGHT: i32 = 24;
pub const CHIP_ICON: i32 = 16;
pub const CHIP_PAD_LEFT: i32 = 7;
pub const CHIP_PAD_RIGHT: i32 = 10;
pub const CHIP_GAP: i32 = 5;

/// 图片气泡的缩略图边长与圆角（`kThumbCorner`）。
pub const THUMB_SIZE: i32 = 160;
pub const THUMB_RADIUS: f32 = 12.0;
/// 缩略图没到位时的占位块：半透黑底 + 白字（`updatePlaceholder` 那档样式）。
pub const IMAGE_HOLD_BG: Color = Color::rgba(0, 0, 0, 90);
/// 图片收发失败时的占位块：更淡的底 + 橙红字。
pub const IMAGE_FAIL_BG: Color = Color::rgba(0, 0, 0, 40);
pub const IMAGE_FAIL_FG: Color = Color::hex(0xFF7A6E);

// ————————————————————— 图片预览窗 —————————————————————
//
// 这一段全是定色，不随深浅主题走：原版 `ImagePreview` 是个独立顶层窗口，
// `paintEvent` 直接 `fillRect(QColor(24,24,26))`，工具条与侧箭头也写死了半透黑底白字——
// 看图的底色必须恒定，否则同一张图在两套主题下明暗观感会不一样。

/// 画面底色（原版 `paintEvent` 的 `QColor(24,24,26)`）。
pub const PREVIEW_BG: Color = Color::hex(0x18181A);
/// 悬浮工具条底色（`rgba(28,28,30,215)`）。
pub const PREVIEW_BAR_BG: Color = Color::rgba(28, 28, 30, 215);
/// 侧悬浮箭头底色（`rgba(28,28,30,150)`）。
pub const PREVIEW_ARROW_BG: Color = Color::rgba(28, 28, 30, 150);
/// 按钮上的字与图标着色（`#F2F2F2`）。
pub const PREVIEW_FG: Color = Color::hex(0xF2F2F2);
/// 缩放百分比那行的字色（`#DDDDDD`）。
pub const PREVIEW_ZOOM_FG: Color = Color::hex(0xDDDDDD);
/// 禁用态字色（原版的 `rgba(255,255,255,70)` 太暗，这里按 windui 的置灰档取）。
pub const PREVIEW_DISABLED_FG: Color = Color::rgba(255, 255, 255, 88);
/// 工具条 / 侧箭头的按钮底。文字按钮拿它当 `Intent::Custom` 的基色（悬停与按下由 windui
/// 派生）；图标按钮只能当节点底色直绘，原因见 [`PREVIEW_BUTTON_CORNER`]。
pub const PREVIEW_BUTTON: Color = Color::hex(0x3A3A3D);
/// 图标按钮那层底的圆角。
///
/// windui 的 `intent()` 只认 `Button` / `CheckBox`（`icon_button_content` 背地里是
/// `IconButton`，挂上去 debug 构建直接 panic），所以图标按钮的底色只能画在节点上；
/// 而节点底色只读节点自己的 `corner`，不像 `Button` 会从主题取——这里报同一档
/// （windui `Metrics::corner_md` 的默认值），才跟同一排文字按钮的圆角对得上。
pub const PREVIEW_BUTTON_CORNER: f32 = 6.0;

/// 预览窗初始与最小尺寸（原版 `resize(960, 680)` / `setMinimumSize(360, 260)`）。
pub const PREVIEW_WIDTH: i32 = 960;
pub const PREVIEW_HEIGHT: i32 = 680;
pub const PREVIEW_MIN_WIDTH: i32 = 360;
pub const PREVIEW_MIN_HEIGHT: i32 = 260;
/// 缩放上下限与单次步进因子（原版 `kMinScale` / `kMaxScale` / `pow(1.25, steps)`）。
pub const ZOOM_MIN: f64 = 0.02;
pub const ZOOM_MAX: f64 = 40.0;
pub const ZOOM_STEP: f64 = 1.25;
/// 侧悬浮箭头的边长与离边距离（原版 44×44、`x = 12`）。
pub const ARROW_SIZE: i32 = 44;
pub const ARROW_MARGIN: i32 = 12;
/// 工具条离底的间距（原版 `max(6, h - 工具条高 - 14)` 里的那个 14）。
pub const BAR_BOTTOM_MARGIN: i32 = 14;
/// 预览用的最长边：超过就等比缩小。4096 足够屏幕上看清细节，又能把超大图的解码耗时
/// 与内存压在可接受的范围（原版的 `QPixmap` 也受 Qt 的图像尺寸上限约束）。
pub const PREVIEW_MAX_EDGE: u32 = 4096;

/// 底部输入框的高度。原版 `textEdit` 是 150（连工具条共 ~190），那是在更大的窗口里；
/// 本移植默认 760 高，取 120 给消息区留够可视行数，观感上仍是「一整块大输入区」。
pub const COMPOSER_HEIGHT: i32 = 120;

// ————————————————————— 配色档案 —————————————————————

/// 一套完整配色，对应 C++ 版的一份 `.qss`。
///
/// 界面原先只问「深不深」，一个 `bool` 就够；第三套主题进来后每档颜色都要三选，
/// 于是把每档收进这个结构：视图层拿 `Skin` 而不是 `dark`，以后加「海蓝」「暖橙」只要
/// 多写一份常量，控件与气泡的取色代码一行都不用动。`Copy` 是为了沿用原先按值传参的写法，
/// 不必到处改成 `&Skin`。
///
/// 字段分两段：上半是 windui 控件调色板的原料（[`Skin::palette`] 逐字段搬进 `Palette`），
/// 下半是消息区那些 windui 管不着的定色（气泡底、聊天区底、右气泡上的次要文字）。
/// 语义重叠的档故意合一个字段：`text` 同时是左气泡正文，`text_muted` 同时是 MD 图标着色，
/// `danger` / `success` 同时是原来的 `error()` / `success()`——两边在 C++ 版里本就同值。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Skin {
    /// 深色基座：决定 windui 的 `Theme` 从哪套起步，也用于少数「往亮还是往暗」的判断。
    pub dark: bool,
    // —— windui 控件调色板 ——
    pub accent: Color,
    pub accent_hover: Color,
    pub accent_active: Color,
    pub on_accent: Color,
    pub accent_text: Option<Color>,
    pub bg: Color,
    pub surface: Color,
    pub surface_alt: Color,
    pub surface_inverse: Color,
    pub on_surface_inverse: Color,
    pub text: Color,
    pub text_muted: Color,
    pub text_subtle: Color,
    pub text_disabled: Color,
    pub border: Color,
    pub track: Color,
    pub placeholder: Color,
    pub divider: Color,
    pub danger: Color,
    pub success: Color,
    pub warning: Color,
    // —— 消息区定色 ——
    /// 主色：右气泡底、头像淡底、文件图块上那枚 glyph。
    pub primary: Color,
    /// 聊天区底色（C++ 版 `chatListWidget` 的背景）。
    pub thread_bg: Color,
    /// 左（对方）气泡底。
    pub bubble_peer: Color,
    /// 右气泡上的次要信息色（文件大小），原版写死淡灰。
    pub sub_on_mine: Color,
}

impl Skin {
    /// 浅色：抄 `style_light.qss`。
    pub const LIGHT: Self = Self {
        dark: false,
        accent: Color::hex(0x5B8DEF),
        accent_hover: Color::hex(0x4A7DD8),
        accent_active: Color::hex(0x3E6BC0),
        on_accent: Color::hex(0xFFFFFF),
        accent_text: Some(Color::hex(0x3B6FCC)),
        bg: Color::hex(0xF8F9FE),
        surface: Color::hex(0xFFFFFF),
        surface_alt: Color::hex(0xF0F2F8),
        surface_inverse: Color::hex(0x1A1C1E),
        on_surface_inverse: Color::hex(0xF8F9FE),
        text: Color::hex(0x1A1C1E),
        text_muted: Color::hex(0x5F6368),
        text_subtle: Color::hex(0x80868B),
        text_disabled: Color::hex(0x9AA0A6),
        border: Color::hex(0xD5DAE8),
        track: Color::hex(0xC7CDD8),
        placeholder: Color::hex(0x9AA0A6),
        divider: Color::hex(0xE2E6F0),
        danger: Color::hex(0xC62828),
        success: Color::hex(0x388E3C),
        warning: Color::hex(0xF9A825),
        primary: Color::hex(0x5B8DEF),
        thread_bg: Color::hex(0xF0F2F8),
        bubble_peer: Color::hex(0xE2E6F0),
        sub_on_mine: Color::hex(0xB0B6BF),
    };

    /// 深色：抄 `style_dark.qss`。按钮底用提亮的 `#8AB4F8` 配深字，正文是 `#E3E3E3`。
    pub const DARK: Self = Self {
        dark: true,
        accent: Color::hex(0x8AB4F8),
        accent_hover: Color::hex(0xA6C8FF),
        accent_active: Color::hex(0x7BA6EA),
        on_accent: Color::hex(0x1A1C1E),
        accent_text: Some(Color::hex(0x8AB4F8)),
        bg: Color::hex(0x1A1C1E),
        surface: Color::hex(0x2D2F31),
        surface_alt: Color::hex(0x3A3D42),
        surface_inverse: Color::hex(0xE3E3E3),
        on_surface_inverse: Color::hex(0x1A1C1E),
        text: Color::hex(0xE3E3E3),
        text_muted: Color::hex(0xA8ACB0),
        text_subtle: Color::hex(0x8E9296),
        text_disabled: Color::hex(0x6E7276),
        border: Color::hex(0x4A4D52),
        track: Color::hex(0x4A4D52),
        placeholder: Color::hex(0x7A7E82),
        divider: Color::hex(0x33363A),
        danger: Color::hex(0xEF5350),
        success: Color::hex(0x66BB6A),
        warning: Color::hex(0xFFB74D),
        // 深色下右气泡仍是那枚蓝：原版 `Config::primaryColor` 不随深浅变。
        primary: Color::hex(0x5B8DEF),
        thread_bg: Color::hex(0x2D2F31),
        bubble_peer: Color::hex(0x3A3D42),
        sub_on_mine: Color::hex(0xB0B6BF),
    };

    /// 祖母绿：抄 `style_emerald.qss`，浅底基座 + 整组绿调。
    ///
    /// 原版把 `chatListWidget` 与左气泡都写成 `#EDF7ED`，靠 1px `#C8E6C9` 描边分层；
    /// 本移植的气泡不画描边，于是让列表底退回窗口通用的 `#F6FBF6`、气泡上移到 `#E8F5E9`
    /// （qss 里的「主色亮」档），层次仍靠同两组色值拉开。
    pub const EMERALD: Self = Self {
        dark: false,
        accent: Color::hex(0x2E7D32),
        accent_hover: Color::hex(0x1B5E20),
        accent_active: Color::hex(0x1B5E20),
        on_accent: Color::hex(0xFFFFFF),
        accent_text: Some(Color::hex(0x1B5E20)),
        bg: Color::hex(0xF6FBF6),
        surface: Color::hex(0xFFFFFF),
        surface_alt: Color::hex(0xEDF7ED),
        surface_inverse: Color::hex(0x1A1C1E),
        on_surface_inverse: Color::hex(0xF6FBF6),
        text: Color::hex(0x1A1C1E),
        // 弱化色带一点绿灰，免得在绿底上显得是从蓝主题抄来的。
        text_muted: Color::hex(0x4B5A4E),
        text_subtle: Color::hex(0x6B7D6F),
        text_disabled: Color::hex(0x9E9E9E),
        border: Color::hex(0xC8E6C9),
        track: Color::hex(0xC8E6C9),
        placeholder: Color::hex(0x9E9E9E),
        divider: Color::hex(0xEDF7ED),
        danger: Color::hex(0xC62828),
        // 主色已是绿，成功档故意留更深一档，别和「发送出去」的气泡撞成一个色。
        success: Color::hex(0x388E3C),
        warning: Color::hex(0xF9A825),
        primary: Color::hex(0x2E7D32),
        thread_bg: Color::hex(0xF6FBF6),
        bubble_peer: Color::hex(0xE8F5E9),
        sub_on_mine: Color::hex(0xB0B6BF),
    };

    /// 交给 windui 的控件调色板。
    pub fn palette(self) -> windui::theme::Palette {
        windui::theme::Palette {
            accent: self.accent,
            accent_hover: self.accent_hover,
            accent_active: self.accent_active,
            on_accent: self.on_accent,
            accent_text: self.accent_text,
            bg: self.bg,
            surface: self.surface,
            surface_alt: self.surface_alt,
            surface_inverse: self.surface_inverse,
            on_surface_inverse: self.on_surface_inverse,
            text: self.text,
            text_muted: self.text_muted,
            text_subtle: self.text_subtle,
            text_disabled: self.text_disabled,
            border: self.border,
            track: self.track,
            placeholder: self.placeholder,
            divider: self.divider,
            danger: self.danger,
            success: self.success,
            warning: self.warning,
        }
    }

    /// 聊天区底色。
    pub fn chat_bg(self) -> Color {
        self.thread_bg
    }

    /// 气泡填充色。右侧恒为主色，左侧用比列表底深（浅）一档的表面变体。
    pub fn bubble_fill(self, mine: bool) -> Color {
        if mine { self.primary } else { self.bubble_peer }
    }

    /// 文件气泡填充：原版 `paintEvent` 把 MD3 叠加色预合成进底色，
    /// 免得「底色 + 内缩卡片」两层露出色差。右叠白 28/255，左叠黑 16/255。
    pub fn bubble_file_fill(self, mine: bool) -> Color {
        let base = self.bubble_fill(mine);
        if mine {
            overlay(base, Color::rgba(255, 255, 255, 28))
        } else {
            overlay(base, Color::rgba(0, 0, 0, 16))
        }
    }

    /// 气泡正文色：右侧纯白，左侧用正文色。
    pub fn bubble_text(self, mine: bool) -> Color {
        if mine { Color::WHITE } else { self.text }
    }

    /// 次要信息（文件大小）色：右侧固定淡灰，左侧正文色淡化到 alpha 150。
    pub fn sub_text(self, mine: bool) -> Color {
        if mine { self.sub_on_mine } else { with_alpha(self.text, 150) }
    }

    /// 昵称（设备名）色：正文色淡化到 alpha 170。
    pub fn nickname(self) -> Color {
        with_alpha(self.text, 170)
    }

    /// 头像圆底：主色 alpha 50 的淡底（MD3 头像容器）。
    pub fn avatar_bg(self) -> Color {
        with_alpha(self.primary, 50)
    }

    /// 状态胶囊底：右侧半透明黑，左侧在气泡底色上加深一档（tone-on-tone）。
    pub fn chip_bg(self, mine: bool) -> Color {
        if mine {
            Color::rgba(0, 0, 0, 55)
        } else {
            darken(self.bubble_peer, 115)
        }
    }

    /// 进度条已完成段：右侧白色压在主题主色上，左侧用成功绿。
    pub fn progress_chunk(self, mine: bool) -> Color {
        if mine { Color::WHITE } else { self.success }
    }

    /// MD 矢量图标的着色：取正文弱化色，每套主题下都够醒目又不抢。
    pub fn icon_tint(self) -> Color {
        self.text_muted
    }
}

/// 把不透明色改写 alpha 通道（原版 Qt 的 `QColor(r,g,b,a)` 定色写法）。
fn with_alpha(c: Color, a: u8) -> Color {
    Color::rgba(c.r, c.g, c.b, a)
}

// ————————————————————— 与主题无关的定色 —————————————————————

/// 进度条轨道：右侧半透明黑，左侧更淡的半透明黑。每套主题都是这个值。
pub fn progress_track(mine: bool) -> Color {
    if mine { Color::rgba(0, 0, 0, 60) } else { Color::rgba(0, 0, 0, 35) }
}

/// 在线状态点。
pub fn online() -> Color {
    Color::rgb(0x35, 0xC8, 0x6F)
}

/// 离线状态点。
pub fn offline() -> Color {
    Color::rgb(0x9A, 0x9A, 0x9A)
}

/// 半透明色 `over` 预合成到不透明底色 `base` 上（SrcOver，结果仍不透明）。
fn overlay(base: Color, over: Color) -> Color {
    let a = over.a as u32;
    Color {
        r: ((over.r as u32 * a + base.r as u32 * (255 - a)) / 255) as u8,
        g: ((over.g as u32 * a + base.g as u32 * (255 - a)) / 255) as u8,
        b: ((over.b as u32 * a + base.b as u32 * (255 - a)) / 255) as u8,
        a: 255,
    }
}

/// 等价于 Qt 的 `QColor::darker(factor)`：每通道乘 `100 / factor`。
pub fn darken(c: Color, factor: u32) -> Color {
    let f = 100u32.max(factor);
    Color {
        r: (c.r as u32 * 100 / f) as u8,
        g: (c.g as u32 * 100 / f) as u8,
        b: (c.b as u32 * 100 / f) as u8,
        a: c.a,
    }
}

/// 颜色的 `#RRGGBB` 写法，供 SVG 模板着色用。
pub fn hex_string(c: Color) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
}

// ————————————————————— 组装 Theme —————————————————————

/// 配置里的磅值 -> 逻辑像素字号。原版 `DEFAULT_FONT_SIZE = 9`（磅）换算后约 14px，
/// 与界面正文字号一致；上下限挡住离谱输入。
fn body_px(font_size: i32) -> f32 {
    (font_size.max(6) as f32 * 1.6).clamp(12.0, 32.0)
}

/// 按偏好组装主题。`skin` 已经把「深不深」解算进去了，由调用方决定。
pub fn build_theme(skin: Skin, font_family: &str, font_size: i32) -> Theme {
    let mut theme = if skin.dark { Theme::dark() } else { Theme::default() };
    theme.palette = skin.palette();
    let family = font_family.trim();
    if !family.is_empty() {
        theme.font_family = Some(family.to_owned());
    }
    let md = body_px(font_size);
    let metrics = &mut theme.metrics;
    metrics.font_xs = (md - 2.0).max(9.0);
    metrics.font_sm = (md - 1.0).max(10.0);
    metrics.font_md = md;
    metrics.font_lg = md + 2.0;
    metrics.font_xl = md + 6.0;
    theme
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 带名字的全量档案：下面的不变量都要逐套跑一遍，报错时得说清是哪套垮了。
    const NAMED: [(&str, Skin); 3] = [
        ("浅色", Skin::LIGHT),
        ("深色", Skin::DARK),
        ("祖母绿", Skin::EMERALD),
    ];

    /// 文件气泡的底色必须比同侧气泡「右更亮、左更暗」——预合成算反了方向
    /// 就会露出来：右侧混白应变亮，左侧混黑应变暗。
    #[test]
    fn file_bubble_overlay_moves_in_the_expected_direction() {
        for (name, skin) in NAMED {
            let base_right = skin.bubble_fill(true);
            let mixed_right = skin.bubble_file_fill(true);
            assert!(luma(mixed_right) > luma(base_right), "{name} 右侧混白后应更亮");

            let base_left = skin.bubble_fill(false);
            let mixed_left = skin.bubble_file_fill(false);
            assert!(luma(mixed_left) < luma(base_left), "{name} 左侧混黑后应更暗");
        }
    }

    /// 左侧气泡与聊天区底色必须分得开，否则气泡像浮在空气里。每套主题各验一次。
    ///
    /// 祖母绿尤其盯着：原版把它的气泡与列表底都写成 `#EDF7ED`、靠描边分层，
    /// 本移植不画描边，两组色值挪错了位置就会在这里失败。
    #[test]
    fn left_bubble_stands_out_from_the_thread_background() {
        for (name, skin) in NAMED {
            let gap = (luma(skin.bubble_fill(false)) - luma(skin.chat_bg())).abs();
            assert!(gap >= 6.0, "{name} 左气泡与列表底亮度差只有 {gap}，看不出是气泡");
        }
    }

    /// 主题间必须靠「真的换了色」区分，而不是整份常量抄过来忘改值：强调色、左气泡底、
    /// 聊天区底三项两两互异，任何一档原样照抄另一套都会在这里暴露，而不是等用户切了
    /// 主题发现「画面没变」。
    ///
    /// 偏偏不能拿右气泡（`primary`）来区分：原版 `Config::primaryColor` 不随深浅变，
    /// 浅色与深色共用同一枚 `#5B8DEF` 是照抄，不是漏改。
    #[test]
    fn every_skin_is_a_distinct_palette() {
        for (left_name, left) in NAMED {
            for (right_name, right) in NAMED {
                if left_name == right_name {
                    continue;
                }
                assert_ne!(left, right, "{left_name} 与 {right_name} 整份撞色");
                assert_ne!(left.accent, right.accent, "{left_name} 与 {right_name} 的强调色撞色");
                assert_ne!(left.bubble_peer, right.bubble_peer, "{left_name} 与 {right_name} 的左气泡撞色");
                assert_ne!(left.thread_bg, right.thread_bg, "{left_name} 与 {right_name} 的聊天区撞色");
            }
        }
        // 浅/深共用右气泡蓝是原版行为，绿主题则整组换成绿。
        assert_eq!(Skin::LIGHT.primary, Skin::DARK.primary, "浅与深的右气泡本该同色");
        assert_ne!(Skin::EMERALD.primary, Skin::LIGHT.primary, "祖母绿的右气泡该是绿");
        // 深浅基座决定 windui 起步那套，抄错会让控件整体跑错方向。
        assert!(!Skin::EMERALD.dark, "祖母绿是浅底主题");
        assert!(Skin::DARK.dark, "深色该走深色基座");
    }

    /// SVG 模板靠字符串替换着色，格式错了 resvg 会静默画不出来。
    #[test]
    fn svg_placeholder_color_is_written_the_way_svg_expects() {
        assert_eq!(hex_string(Skin::LIGHT.primary), "#5B8DEF");
        assert_eq!(hex_string(Color::BLACK), "#000000");
    }

    fn luma(c: Color) -> f32 {
        0.299 * c.r as f32 + 0.587 * c.g as f32 + 0.114 * c.b as f32
    }
}
