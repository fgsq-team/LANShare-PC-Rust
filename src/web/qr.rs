//! 二维码生成，对应原版 `src/QrCode.cpp` 里的 `makeImage()`。
//!
//! 原版产出的是 `Grayscale8` 位图（`(模块数 + 4*2) * scale` 像素），因为 Qt 的
//! `QLabel` 只认 `QPixmap`。这里改成 SVG 字符串：windui 的 `image_svg` 会按绘制框
//! 的实际物理尺寸光栅，任何 DPI 都是 1:1 落像素，比预先放大 6 倍的位图清楚得多。
//!
//! 留白（quiet zone）沿用规范推荐的 4 个模块，与原版 `border = 4` 一致——扫码 App
//! 在留白不足时识别率会明显下降，这一圈不能省。

use qrcode::render::svg;
use qrcode::QrCode;

/// 原版对话框里那张图的边长（`WebUrlQRDialog` 的 `resize(240, 240)`）。
pub const DIALOG_SIDE: u32 = 240;

/// 把内容编码成 SVG 文本。内容太长超出 QR 上限（版本 40、纠错等级 M）时返回 `None`，
/// 与原版 `makeImage()` 返回空 `QImage` 的行为等价。
pub fn svg_text(content: &str) -> Option<String> {
    if content.is_empty() {
        return None;
    }
    // 等级 M：原版 `QrCode` 用的也是 Medium，容量与容错的折中点
    let code = QrCode::new(content.as_bytes()).ok()?;
    Some(
        code.render::<svg::Color>()
            .dark_color(svg::Color("#000000"))
            .light_color(svg::Color("#FFFFFF"))
            .quiet_zone(true)
            .build(),
    )
}

/// 编码成可以直接交给 `Element::image_svg` 的字节，并限定最小边长（像素）。
///
/// `min_dimensions` 只写 SVG 的 `width`/`height` 属性，路径本身仍是矢量的，
/// 放大缩小都不糊。
pub fn svg_bytes(content: &str, side: u32) -> Option<Vec<u8>> {
    let code = QrCode::new(content.as_bytes()).ok()?;
    let text = code
        .render::<svg::Color>()
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#FFFFFF"))
        .quiet_zone(true)
        .min_dimensions(side, side)
        .build();
    Some(text.into_bytes())
}

/// 局域网网页版的入口地址。原版在 `WebUrlQRDialog` 里拼的是同一个形式。
///
/// IPv6 必须用方括号包裹（`http://[fe80::1]:5856`），否则地址里的冒号会和端口
/// 糊在一起、浏览器无从分辨端口。已带方括号的输入先剥掉再统一包裹，避免重复嵌套；
/// IPv4 不含冒号，原样拼接。
pub fn web_url(ip: &str, port: i32) -> String {
    if ip.contains(':') {
        let body = ip.trim_start_matches('[').trim_end_matches(']');
        format!("http://[{body}]:{port}")
    } else {
        format!("http://{ip}:{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 二维码画出来必须是能扫码的黑白方块：SVG 根节点、纯黑前景、纯白背景，一个都不能少。
    #[test]
    fn a_short_url_renders_as_black_on_white_svg() {
        let text = svg_text(&web_url("192.168.1.20", 5856)).expect("这么短的地址不可能编不出码");
        // 渲染器会在 `<svg` 前面带一行 XML 声明，只能找子串而不能断言前缀
        assert!(text.contains("<svg "), "{text}");
        assert!(text.ends_with("</svg>"));
        assert!(text.contains("#000000") && text.contains("#FFFFFF"), "颜色被改写就不会显影");
        assert!(text.contains("crispEdges"), "边缘不做抗锯齿，否则模块边界会糊成灰边");
    }

    /// 空内容没有意义，也不能让调用方拿到一张全白的「二维码」去扫。
    #[test]
    fn empty_content_is_refused() {
        assert!(svg_text("").is_none());
    }

    /// 超出 QR 版本 40 的容量要安静地失败，让对话框显示「二维码生成失败」。
    #[test]
    fn content_too_long_for_any_version_is_refused() {
        let huge = "https://example.com/".repeat(400);
        assert!(svg_text(&huge).is_none(), "{} 字节的内容本就该编不下", huge.len());
    }

    /// 界面按 SVG 的固有边长占位才不会把二维码挤成一条线，而 `min_dimensions`
    /// 只是**下限**：渲染器最终按「（模块数 + 两侧留白） 乘 每模块 8px」向上取整，
    /// 所以这里只能断言“不小于请求的边长”。
    #[test]
    fn the_requested_side_becomes_the_svg_size() {
        let bytes = svg_bytes(&web_url("10.0.0.1", 5856), DIALOG_SIDE).expect("应当能出图");
        let text = String::from_utf8(bytes).unwrap();
        let width = declared_size(&text, "width").expect("SVG 要带 width 属性");
        let height = declared_size(&text, "height").expect("SVG 要带 height 属性");
        assert!(width >= DIALOG_SIDE && height == width, "请求 {DIALOG_SIDE}，实际 {width}x{height}: {text}");
        // 不写 min_dimensions 时只能拿到最小默认边长，对话框里会小到扫不出来
        let plain = svg_text(&web_url("10.0.0.1", 5856)).unwrap();
        assert!(declared_size(&plain, "width").unwrap_or(0) > 0, "默认渲染也要有边长");
    }

    /// 从 SVG 开头那一个标签里抠出 `key="N"` 的数字。
    fn declared_size(text: &str, key: &str) -> Option<u32> {
        let head = text.split("<svg").nth(1)?.split('>').next()?;
        let marker = format!("{key}=\"");
        let rest = head.split(marker.as_str()).nth(1)?;
        rest.split('"').next()?.parse().ok()
    }

    #[test]
    fn the_url_matches_the_qt_form() {
        assert_eq!(web_url("192.168.1.5", 5856), "http://192.168.1.5:5856");
    }

    /// IPv6 得包进方括号，端口才不会被地址里的冒号吞掉；已带括号的也不重复套。
    #[test]
    fn the_url_brackets_ipv6_hosts() {
        assert_eq!(web_url("2408:8000:1:2::5", 5856), "http://[2408:8000:1:2::5]:5856");
        assert_eq!(web_url("[2408:8000:1:2::5]", 5856), "http://[2408:8000:1:2::5]:5856");
    }
}
