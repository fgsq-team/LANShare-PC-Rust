//! 网页端「图片」面板背后的图库，对应原版 `LHttpServer.cpp` 顶上的
//! `scanImages()` / `mediaFolders` / `allMedias`。
//!
//! 结构保持与原版一致：一张「目录 → 该目录内的图片」表，按目录路径字典序编号，
//! 图片再按顺序拿一个全局下标。`/media` 用目录号或图片号取数据，
//! `/imageload?index=` 用全局图片号取缩略图，`/compressMedias` 也认这两个号。

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use image::ImageEncoder as _;

/// 参与扫描的扩展名，与原版 `scanMovies()` 的白名单逐字一致（不含 gif）。
const IMAGE_SUFFIX: [&str; 4] = ["jpg", "jpeg", "png", "bmp"];

/// 一个图片目录。`files` 是该目录内图片在 `all` 里的下标，顺序即编号顺序。
#[derive(Debug, Clone, Default)]
pub struct Folder {
    pub path: String,
    pub files: Vec<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct Store {
    pub folders: Vec<Folder>,
    pub all: Vec<String>,
    /// 扫描是否已经跑完。没跑完时 `/media` 返回空表，网页端表现为「暂无图片」。
    pub scanned: bool,
}

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();

fn store() -> &'static Mutex<Store> {
    STORE.get_or_init(|| Mutex::new(Store::default()))
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|tail| tail.to_str())
        .map(|tail| IMAGE_SUFFIX.iter().any(|suffix| suffix.eq_ignore_ascii_case(tail)))
        .unwrap_or(false)
}

/// 递归收集 `root` 下的图片，按「目录路径 -> 目录内文件名序」并进 `acc`。
fn walk(root: &Path, acc: &mut std::collections::BTreeMap<String, Vec<PathBuf>>) {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    let mut dirs = Vec::new();
    let mut here: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path);
        } else if path.is_file() && is_image(&path) {
            here.push(path);
        }
    }
    if !here.is_empty() {
        here.sort();
        acc.entry(root.to_string_lossy().replace('\\', "/")).or_default().extend(here);
    }
    for dir in dirs {
        walk(&dir, acc);
    }
}

/// 扫一批根目录，重建整张表。单元测试里直接喂临时目录用。
pub fn scan_into(roots: &[PathBuf]) -> Store {
    let mut acc = std::collections::BTreeMap::new();
    for root in roots {
        walk(root, &mut acc);
    }
    let mut store = Store { scanned: true, ..Default::default() };
    for (dir, files) in acc {
        if files.is_empty() {
            continue;
        }
        let start = store.all.len();
        store.all.extend(files.into_iter().map(|path| path.to_string_lossy().replace('\\', "/")));
        store.folders.push(Folder { path: dir, files: (start..store.all.len()).collect() });
    }
    store
}

/// 全局表的一份快照。
pub fn snapshot() -> Store {
    store().lock().map(|guard| guard.clone()).unwrap_or_default()
}

/// 后台线程里扫一次图库。原版也是 `std::thread(...).detach()`，
/// 因为「我的图片」目录动辄几万张，不能挡在启动路径上。
pub fn spawn_scan(roots: Vec<PathBuf>) {
    std::thread::spawn(move || {
        let scanned = scan_into(&roots);
        if let Ok(mut guard) = store().lock() {
            *guard = scanned;
        }
    });
}

/// 默认扫描根：系统的「图片」与「视频」目录，对应原版
/// `QStandardPaths::standardLocations(PicturesLocation / MoviesLocation)`。
///
/// 不为此引 `dirs` crate：先吃 XDG 变量，然后拼家目录下的常见名字，
/// Windows 再补一个 `%PUBLIC%`。
pub fn default_roots() -> Vec<PathBuf> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from);
    let mut out = Vec::new();
    // XDG 的变量本身就是完整目录路径，不能再拼一层
    let xdg = |key: &str| std::env::var_os(key).map(PathBuf::from).filter(|path| path.is_dir());
    for name in ["Pictures", "图片"] {
        match xdg("XDG_PICTURES_DIR").or_else(|| home.as_ref().map(|dir| dir.join(name))) {
            Some(dir) => push_dir(&mut out, dir),
            None => {}
        }
    }
    for name in ["Videos", "视频"] {
        match xdg("XDG_VIDEOS_DIR").or_else(|| home.as_ref().map(|dir| dir.join(name))) {
            Some(dir) => push_dir(&mut out, dir),
            None => {}
        }
    }
    // 公共图库（Windows 上 `%PUBLIC%\Pictures`）
    if let Some(public) = std::env::var_os("PUBLIC").map(PathBuf::from) {
        for name in ["Pictures", "Videos"] {
            push_dir(&mut out, public.join(name));
        }
    }
    out
}

fn push_dir(list: &mut Vec<PathBuf>, path: PathBuf) {
    if path.is_dir() && !list.contains(&path) {
        list.push(path);
    }
}

/// 全局下标 -> 图片路径。
pub fn path_at(index: usize) -> Option<String> {
    snapshot().all.get(index).cloned()
}

/// `/media` 的两个分支：`-1` 列目录，否则列该目录里的图片。
pub fn folder_at(index: usize) -> Option<Folder> {
    snapshot().folders.get(index).cloned()
}

/// 缩略图：按 `KeepAspectRatio` 缩进 `max` 方块再转 JPEG，与原版
/// `ThumbnailUtil::getThumnail(path, 200, 200)` + `image.save("jpg", 100)` 等价。
///
/// 桌面端气泡用的是「居中裁方」，网页端要的是「等比缩到框内」，两条路不能共用。
pub fn thumbnail_jpeg(path: &str, max: u32) -> Option<Vec<u8>> {
    let picture = image::open(path).ok()?.into_rgb8();
    let (width, height) = (picture.width(), picture.height());
    if width == 0 || height == 0 {
        return None;
    }
    // 与原版的 `KeepAspectRatio` 一致：小图也会放大到框满
    let scale = (max as f64 / width as f64).min(max as f64 / height as f64);
    let target_w = ((width as f64 * scale).round() as u32).max(1);
    let target_h = ((height as f64 * scale).round() as u32).max(1);
    let small = image::imageops::resize(&picture, target_w, target_h, image::imageops::FilterType::Triangle);
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 100)
        .write_image(small.as_raw(), target_w, target_h, image::ExtendedColorType::Rgb8)
        .ok()?;
    Some(out)
}

/// 图片扩展名判定，聊天里按扩展名决定气泡是缩略图还是文件卡片。
pub fn is_image_path(path: &str) -> bool {
    is_image(Path::new(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_picture(dir: &Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        let picture = image::RgbImage::from_fn(40, 20, |x, y| image::Rgb([(x * 3) as u8, (y * 9) as u8, 90]));
        image::DynamicImage::ImageRgb8(picture).save(&path).unwrap();
        path
    }

    fn temp_tree(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("lanshare-media-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// 目录按路径字典序编号，同一目录内按文件名排序——编号会直接暴露给网页，
    /// 乱了就会把 A 目录的图打包成 B 目录。
    #[test]
    fn folders_are_indexed_in_path_order() {
        let root = temp_tree("order");
        write_picture(&root.join("Camera"), "b.jpg");
        write_picture(&root.join("Camera"), "a.png");
        write_picture(&root.join("Screenshots"), "c.bmp");
        std::fs::write(root.join("note.txt"), b"not an image").unwrap();
        let store = scan_into(&[root.clone()]);
        assert_eq!(store.folders.len(), 2);
        assert_eq!(store.all.len(), 3, "txt 不该进表");
        assert!(store.folders[0].path.ends_with("Camera"));
        assert_eq!(store.folders[0].files.len(), 2);
        // 这里的 `files` 是在 `all` 里的下标；`path_at` 读的是全局单例，不能用来验
        // 这份刚扫出来的局部表
        assert!(store.all[store.folders[0].files[0]].ends_with("a.png"), "{:?}", store.all);
        assert!(store.all[store.folders[0].files[1]].ends_with("b.jpg"));
        assert!(store.folders[1].path.ends_with("Screenshots"));
        assert_eq!(store.folders[1].files, vec![2], "第二个目录的下标要接着往后排");
        std::fs::remove_dir_all(&root).ok();
    }

    /// 空目录不进表：原版也是 `images.isEmpty()` 就跳过。
    #[test]
    fn empty_directories_are_left_out() {
        let root = temp_tree("empty");
        std::fs::create_dir_all(root.join("Nothing")).unwrap();
        write_picture(&root.join("There"), "x.jpg");
        let store = scan_into(&[root]);
        assert_eq!(store.folders.len(), 1);
        assert!(store.scanned);
    }

    /// 等比缩放进 200 的框里，长边吃掉 200、短边按比例，绝不裁方。
    #[test]
    fn thumbnails_keep_the_aspect_ratio() {
        let root = temp_tree("thumb");
        let path = write_picture(&root, "wide.jpg");
        let bytes = thumbnail_jpeg(&path.to_string_lossy(), 200).expect("应当能出缩略图");
        let back = image::load_from_memory(&bytes).unwrap();
        assert!(back.width() <= 200 && back.height() <= 200, "{}x{}", back.width(), back.height());
        assert_eq!((back.width(), back.height()), (200, 100), "40x20 的图要等比放大到框满");
        assert!(thumbnail_jpeg("/没有这个文件.jpg", 200).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_image_filter_matches_the_original_whitelist() {
        assert!(is_image_path("a.JPG"));
        assert!(is_image_path("b.jpeg"));
        assert!(!is_image_path("c.gif"), "原版扫描白名单里没有 gif");
        assert!(!is_image_path("d.txt"));
    }
}
