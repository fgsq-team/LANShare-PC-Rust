//! 网页前端的内嵌副本，一一对应 Qt 版的 `res.qrc`（`prefix="/web"`）。
//!
//! 原版把这些文件打进 Qt 资源系统，运行时按 `:/web/web/css/bootstrap.min.css`
//! 这样的键取；这里用 `include_bytes!` 达到同样效果——拷走 exe 就能起网页版，
//! 运行目录旁不需要再摆一份 `web/`。
//!
//! 键名保持与 Qt 资源路径的最后一段一致（`/css/xxx.css`、`/js/xxx.js`、
//! `/images/xxx.png`、`/drawable/xxx.png`），路由层直接拿请求 URL 来查表。

/// 主页（`:/web/web/lanshare.html`）。
pub const INDEX_HTML: &[u8] = include_bytes!("../../../assets/web/lanshare.html");
/// 应用图标，同时充当 `/favicon.ico`。
pub const FAVICON: &[u8] = include_bytes!("../../../assets/web/images/lanshare.png");

/// 一块内嵌资源：Qt 资源里的相对路径 + 原始字节。
pub struct Entry {
    pub path: &'static str,
    pub bytes: &'static [u8],
}

/// `/images/*`、`/css/*`、`/js/*`、`/drawable/*` 四组前缀背后的全部文件。
///
/// 表里有 `nav-item-media-*` 那几个图标类名用到的样式，但没有 Qt 侧未打包的
/// `web/drawable/*.svg`（原版 PC 端也没有）。
pub static FILES: &[Entry] = &[
    Entry { path: "/css/animate.min.css", bytes: include_bytes!("../../../assets/web/css/animate.min.css") },
    Entry { path: "/css/bootstrap.min.css", bytes: include_bytes!("../../../assets/web/css/bootstrap.min.css") },
    Entry { path: "/css/lanshare.css", bytes: include_bytes!("../../../assets/web/css/lanshare.css") },
    Entry { path: "/css/style.min.css", bytes: include_bytes!("../../../assets/web/css/style.min.css") },
    Entry { path: "/css/theme.css", bytes: include_bytes!("../../../assets/web/css/theme.css") },
    Entry { path: "/js/bootstrap-notify.min.js", bytes: include_bytes!("../../../assets/web/js/bootstrap-notify.min.js") },
    Entry { path: "/js/bootstrap.min.js", bytes: include_bytes!("../../../assets/web/js/bootstrap.min.js") },
    Entry { path: "/js/jquery.min.js", bytes: include_bytes!("../../../assets/web/js/jquery.min.js") },
    Entry { path: "/js/lanshare.js", bytes: include_bytes!("../../../assets/web/js/lanshare.js") },
    Entry { path: "/js/lanshareChat.js", bytes: include_bytes!("../../../assets/web/js/lanshareChat.js") },
    Entry { path: "/js/lightyear.min.js", bytes: include_bytes!("../../../assets/web/js/lightyear.min.js") },
    Entry { path: "/js/main.min.js", bytes: include_bytes!("../../../assets/web/js/main.min.js") },
    Entry { path: "/js/perfect-scrollbar.min.js", bytes: include_bytes!("../../../assets/web/js/perfect-scrollbar.min.js") },
    Entry { path: "/js/request.min.js", bytes: include_bytes!("../../../assets/web/js/request.min.js") },
    Entry { path: "/js/WSCmd.js", bytes: include_bytes!("../../../assets/web/js/WSCmd.js") },
    Entry { path: "/images/apps.png", bytes: include_bytes!("../../../assets/web/images/apps.png") },
    Entry { path: "/images/exit.png", bytes: include_bytes!("../../../assets/web/images/exit.png") },
    Entry { path: "/images/folder.png", bytes: include_bytes!("../../../assets/web/images/folder.png") },
    Entry { path: "/images/img.png", bytes: include_bytes!("../../../assets/web/images/img.png") },
    Entry { path: "/images/lanshare.png", bytes: include_bytes!("../../../assets/web/images/lanshare.png") },
    Entry { path: "/images/no_network.png", bytes: include_bytes!("../../../assets/web/images/no_network.png") },
    Entry { path: "/images/player_play.png", bytes: include_bytes!("../../../assets/web/images/player_play.png") },
    Entry { path: "/images/record.png", bytes: include_bytes!("../../../assets/web/images/record.png") },
    Entry { path: "/images/wendang.png", bytes: include_bytes!("../../../assets/web/images/wendang.png") },
    Entry { path: "/images/wenjianjia_20.png", bytes: include_bytes!("../../../assets/web/images/wenjianjia_20.png") },
    Entry { path: "/images/wenjianjia_60.png", bytes: include_bytes!("../../../assets/web/images/wenjianjia_60.png") },
    Entry { path: "/drawable/ic_phone.png", bytes: include_bytes!("../../../assets/web/drawable/ic_phone.png") },
    Entry { path: "/drawable/ic_win.png", bytes: include_bytes!("../../../assets/web/drawable/ic_win.png") },
    Entry { path: "/drawable/rc_file_blue_icon.png", bytes: include_bytes!("../../../assets/web/drawable/rc_file_blue_icon.png") },
    Entry { path: "/drawable/rc_file_icon_file.png", bytes: include_bytes!("../../../assets/web/drawable/rc_file_icon_file.png") },
];

/// 按 Qt 资源路径取文件；`/drawable` 那一组的键不带 `.png`，调用方已经补好。
pub fn lookup(path: &str) -> Option<&'static [u8]> {
    FILES.iter().find(|entry| entry.path == path).map(|entry| entry.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内嵌表里不能出现空文件：`include_bytes!` 对不存在的路径会编译失败，
    /// 但一个 0 字节的 js/css 只会在浏览器里表现为「页面坏了」，很难往回查。
    #[test]
    fn every_embedded_asset_has_content() {
        assert!(!INDEX_HTML.is_empty() && !FAVICON.is_empty());
        assert!(FILES.iter().all(|entry| !entry.bytes.is_empty()), "{}", "存在空资源");
        for entry in FILES.iter() {
            assert!(entry.path.starts_with('/'), "键必须带前导斜杠: {}", entry.path);
        }
    }

    /// 主页引用的每个相对资源都要能在表里找到，漏一个就是白屏的一块。
    #[test]
    fn the_index_page_resolves_against_the_table() {
        let html = String::from_utf8_lossy(INDEX_HTML);
        for needle in ["css/bootstrap.min.css", "js/jquery.min.js", "images/lanshare.png"] {
            assert!(html.contains(needle), "主页应当引用 {needle}");
            let key = format!("/{needle}");
            assert!(lookup(&key).is_some(), "表里应当有 {key}");
        }
    }

    /// 四个菜单图标对应的类名来自 `lanshare.css`，样式文件缺席的话侧栏会是空白块。
    #[test]
    fn the_sidebar_icons_live_in_the_embedded_stylesheet() {
        let css = String::from_utf8_lossy(lookup("/css/lanshare.css").expect("lanshare.css 必须在表里"));
        for class in ["nav-item-media-img", "nav-item-media-folder", "nav-item-media-record"] {
            assert!(css.contains(class), "样式里缺 {class}");
        }
    }
}
