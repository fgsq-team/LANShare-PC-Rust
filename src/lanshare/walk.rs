//! 文件扫描与传输元数据。
//!
//! 对应原版 `LFile::findFileNew`（递归扫描）与 `fileTransferJson` 的结构：
//! - 子文件名是**相对基准目录的 POSIX 风格路径**（Qt 的 `absoluteFilePath()` 恒用 `/`），
//!   接收端据此建子目录，所以这里也必须用 `/` 而不是 `\`；
//! - `QDir::setFilter(QDir::Dirs | QDir::Files)` 不含 `QDir::Hidden`，隐藏项不参与扫描。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::proto::{FILE_TYPE_APK, FILE_TYPE_AUDIO, FILE_TYPE_FILE, FILE_TYPE_FOLDER, FILE_TYPE_IMAGE, FILE_TYPE_VIDEO};

/// 一个待收发条目：顶层是文件或文件夹，文件夹带子文件列表。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// 协议里的 `fileId`，发送前由 V4/V5 逻辑填。
    pub file_id: String,
    /// 顶层用显示名；子文件用相对基准目录的 POSIX 路径。
    pub name: String,
    /// 本机绝对路径。
    pub path: PathBuf,
    pub length: i64,
    pub is_dir: bool,
    pub children: Vec<Item>,
}

impl Item {
    /// 协议里的 `fileType`：文件夹固定 `FOLDER(6)`，普通文件按扩展名归类
    ///（镜像 Java 版「相册选中的发 IMAGE/VIDEO、其余发 FILE」的效果，手机才能出媒体气泡）。
    pub fn kind(&self) -> i32 {
        if self.is_dir { FILE_TYPE_FOLDER } else { kind_by_name(&self.name) }
    }

    /// 文件夹的总长度等于子文件之和（与原版一致：扫描后用累加值覆盖）。
    pub fn total_length(&self) -> i64 {
        if self.is_dir {
            self.children.iter().map(|child| child.length).sum()
        } else {
            self.length
        }
    }
}

/// 按名字（含扩展名）归类线格式 `fileType`，无匹配扩展名时落回 `FILE(5)`。
pub fn kind_by_name(name: &str) -> i32 {
    let suffix = match name.rfind('.') {
        Some(dot) if dot > 0 => name[dot + 1..].to_ascii_lowercase(),
        _ => return FILE_TYPE_FILE,
    };
    const IMAGE_EXTS: [&str; 15] = ["jpg", "jpeg", "png", "gif", "bmp", "webp", "ico", "tif", "tiff", "svg", "heic", "heif", "avif", "raw", "psd"];
    const VIDEO_EXTS: [&str; 11] = ["mp4", "avi", "mkv", "mov", "wmv", "flv", "webm", "3gp", "mpeg", "mpg", "ts"];
    const AUDIO_EXTS: [&str; 9] = ["mp3", "wav", "flac", "aac", "ogg", "m4a", "wma", "opus", "amr"];
    if IMAGE_EXTS.contains(&suffix.as_str()) {
        FILE_TYPE_IMAGE
    } else if VIDEO_EXTS.contains(&suffix.as_str()) {
        FILE_TYPE_VIDEO
    } else if AUDIO_EXTS.contains(&suffix.as_str()) {
        FILE_TYPE_AUDIO
    } else if suffix == "apk" {
        FILE_TYPE_APK
    } else {
        FILE_TYPE_FILE
    }
}

/// 扫描一个目录，返回所有普通文件（含深层），`name` 为相对 `base` 的 POSIX 路径。
/// 返回值为（子文件列表，字节总数），与 `findFileNew(children, 0, path, path)` 等价。
pub fn scan_folder(base: &Path) -> (Vec<Item>, i64) {
    let mut files = Vec::new();
    let mut total = 0i64;
    walk_into(base, base, &mut files, &mut total);
    (files, total)
}

fn walk_into(base: &Path, dir: &Path, files: &mut Vec<Item>, total: &mut i64) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut names = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_name) = entry.file_name().into_string() else { continue };
        if file_name == "." || file_name == ".." {
            continue;
        }
        names.push((file_name, entry.path()));
    }
    // 与 Qt 一致：目录在前、同名条目按名字稳定排序，保证两端扫描顺序相同
    names.sort_by(|(a, a_path), (b, b_path)| {
        let dir_a = a_path.is_dir();
        let dir_b = b_path.is_dir();
        dir_b.cmp(&dir_a).then_with(|| a.cmp(b))
    });
    for (_, path) in names {
        if is_hidden(&path) {
            continue;
        }
        if path.is_dir() {
            walk_into(base, &path, files, total);
        } else {
            let length = path.metadata().map(|meta| meta.len() as i64).unwrap_or(0);
            *total += length;
            files.push(Item {
                file_id: String::new(),
                name: relative_posix(base, &path),
                path: path.clone(),
                length,
                is_dir: false,
                children: Vec::new(),
            });
        }
    }
}

fn is_hidden(path: &Path) -> bool {
    #[cfg(windows)]
    let hidden = {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x0000_0002;
        path.metadata().map(|meta| meta.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0).unwrap_or(false)
    };
    #[cfg(not(windows))]
    let hidden = {
        // Qt 在非 Windows 上也是按属性判，这里不拦任何项
        let _ = path;
        false
    };
    hidden
}

/// 相对路径，永远用 `/` 分隔（协议规定）。
pub fn relative_posix(base: &Path, path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let parent = base.to_string_lossy().replace('\\', "/");
    let parent = parent.trim_end_matches('/');
    if !parent.is_empty() && text.starts_with(parent) {
        let rest = &text[parent.len()..];
        return rest.trim_start_matches('/').to_string();
    }
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or(text)
}

/// 把用户选中的路径整理成发送条目：目录展开成子文件列表并累加大小。
pub fn to_items(paths: &[PathBuf]) -> Vec<Item> {
    let mut items = Vec::new();
    for path in paths {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        if path.is_dir() {
            let (children, total) = scan_folder(path);
            items.push(Item {
                file_id: String::new(),
                name,
                path: path.clone(),
                length: total,
                is_dir: true,
                children,
            });
        } else {
            let length = path.metadata().map(|meta| meta.len() as i64).unwrap_or(0);
            items.push(Item { file_id: String::new(), name, path: path.clone(), length, is_dir: false, children: Vec::new() });
        }
    }
    items
}

/// 目标已存在时在最后一个小数点前插入 `(n)`，与原版 `mUtils::avoidDuplication` 一致。
pub fn avoid_duplication(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let parent = path.parent().map(|parent| parent.to_path_buf()).unwrap_or_default();
    for index in 1..65535i32 {
        let candidate = match name.rfind('.') {
            Some(dot) if dot > 0 => format!("{}({}){}", &name[..dot], index, &name[dot..]),
            _ => format!("{name}({index})"),
        };
        let path = parent.join(candidate);
        if !path.exists() {
            return path;
        }
    }
    path.to_path_buf()
}

/// 把协议里的相对路径落到根目录下：分隔符归一化，并丢掉 `.`/`..` 片段。
///
/// 对端发来的路径不可信，`..` 一律忽略，避免写到保存目录外面。
pub fn join_relative(root: &Path, relative: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for part in relative.replace('\\', "/").split('/') {
        if part.is_empty() || part == "." || part == ".." {
            continue;
        }
        path.push(part);
    }
    path
}

/// 人类可读大小，与原版 `Utils::computeSize` 同格式（一位小数，上限 TB）。
pub fn size_str(bytes: i64) -> String {
    if bytes <= 0 {
        return "0B".to_string();
    }
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let value = bytes as f64;
    let mut group = (value.log10() / 1024f64.log10()) as usize;
    if group >= UNITS.len() {
        group = UNITS.len() - 1;
    }
    format!("{:.1}{}", value / 1024f64.powi(group as i32), UNITS[group])
}

/// 子文件的线格式。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireFile {
    #[serde(rename = "fileId", default)]
    pub file_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub length: i64,
    #[serde(rename = "fileType", default)]
    pub file_type: i32,
    #[serde(rename = "toUser", default, skip_serializing_if = "String::is_empty")]
    pub to_user: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<WireFile>,
}

/// `fileTransferJson` 整体结构。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferMeta {
    #[serde(rename = "fromDevice", default)]
    pub from_device: serde_json::Value,
    #[serde(rename = "toDevice", default)]
    pub to_device: serde_json::Value,
    #[serde(default)]
    pub files: Vec<WireFile>,
    /// V5 才写，值固定 1。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<i32>,
}

impl TransferMeta {
    /// 由本地条目构造发送元数据。`from`/`to` 直接取设备的 JSON 值。
    pub fn build(items: &[Item], from: &super::device::Device, to: &super::device::Device, resume: bool) -> Self {
        let files = items
            .iter()
            .map(|item| WireFile {
                file_id: item.file_id.clone(),
                name: item.name.clone(),
                length: item.length,
                file_type: item.kind(),
                to_user: to.name.clone(),
                children: item
                    .children
                    .iter()
                    .map(|child| WireFile {
                        file_id: child.file_id.clone(),
                        name: child.name.clone(),
                        length: child.length,
                        file_type: kind_by_name(&child.name),
                        to_user: String::new(),
                        children: Vec::new(),
                    })
                    .collect(),
            })
            .collect();
        let value = |device: &super::device::Device| {
            serde_json::from_str::<serde_json::Value>(&device.handshake_json()).unwrap_or_else(|_| serde_json::json!({}))
        };
        Self { from_device: value(from), to_device: value(to), files, resume: if resume { Some(1) } else { None } }
    }

    pub fn parse(text: &str) -> Option<Self> {
        serde_json::from_str(text).ok()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| String::from("{}"))
    }

    /// 顶层条目数（旧版协议用它当 `count`）。
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn resume_enabled(&self) -> bool {
        self.resume.is_some_and(|value| value != 0)
    }

    /// 从 `fromDevice`/`toDevice` 里还原设备信息。
    pub fn peer(&self, field: &serde_json::Value) -> super::device::Device {
        super::device::Device::from_json_value(field)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lanshare-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn folder_scan_uses_posix_relative_names() {
        let root = temp_dir("scan");
        std::fs::create_dir_all(root.join("sub/deep")).unwrap();
        std::fs::write(root.join("a.txt"), b"12345").unwrap();
        std::fs::write(root.join("sub/b.txt"), b"123").unwrap();
        std::fs::write(root.join("sub/deep/c.bin"), vec![7u8; 1024]).unwrap();
        let (files, total) = scan_folder(&root);
        let names: Vec<String> = files.iter().map(|file| file.name.clone()).collect();
        assert!(names.contains(&"a.txt".to_string()), "{names:?}");
        assert!(names.iter().any(|name| name.replace('\\', "/") == "sub/b.txt"), "{names:?}");
        assert!(names.iter().any(|name| name.replace('\\', "/") == "sub/deep/c.bin"), "{names:?}");
        assert!(names.iter().all(|name| !name.contains('\\')), "相对路径不能出现反斜杠: {names:?}");
        assert_eq!(total, 5 + 3 + 1024);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn to_items_fills_folder_length_from_children() {
        let root = temp_dir("items");
        std::fs::create_dir_all(root.join("box")).unwrap();
        std::fs::write(root.join("box/one.txt"), b"aa").unwrap();
        std::fs::write(root.join("box/two.txt"), b"bbb").unwrap();
        let items = to_items(&[root.join("box")]);
        assert_eq!(items.len(), 1);
        let folder = &items[0];
        assert!(folder.is_dir);
        assert_eq!(folder.kind(), FILE_TYPE_FOLDER);
        assert_eq!(folder.length, 5);
        assert_eq!(folder.total_length(), folder.length);
        assert_eq!(folder.children.len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn relative_posix_falls_back_to_file_name_outside_base() {
        assert_eq!(relative_posix(Path::new("C:/a"), Path::new("C:/a/b/c.txt")), "b/c.txt");
        assert_eq!(relative_posix(Path::new("C:/x"), Path::new("C:/y/z.txt")), "z.txt");
    }

    #[test]
    fn duplication_counter_goes_before_the_extension() {
        let root = temp_dir("dup");
        let target = root.join("report.pdf");
        std::fs::write(&target, b"x").unwrap();
        let first = avoid_duplication(&target);
        assert_eq!(first.file_name().unwrap(), "report(1).pdf");
        std::fs::write(&first, b"x").unwrap();
        assert_eq!(avoid_duplication(&target).file_name().unwrap(), "report(2).pdf");
        let folder = root.join("没有扩展名");
        std::fs::create_dir_all(&folder).unwrap();
        assert_eq!(avoid_duplication(&folder).file_name().unwrap(), "没有扩展名(1)");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn relative_paths_cannot_escape_the_root() {
        let root = Path::new("C:/LANShare");
        assert_eq!(join_relative(root, "sub/deep/a.png"), PathBuf::from("C:/LANShare").join("sub").join("deep").join("a.png"));
        assert_eq!(join_relative(root, "a\\b.txt"), PathBuf::from("C:/LANShare").join("a").join("b.txt"));
        assert_eq!(join_relative(root, "../../windows/x"), PathBuf::from("C:/LANShare").join("windows").join("x"));
        assert_eq!(join_relative(root, "/"), PathBuf::from("C:/LANShare"));
    }

    #[test]
    fn size_format_matches_compute_size() {
        assert_eq!(size_str(0), "0B");
        assert_eq!(size_str(-5), "0B");
        assert_eq!(size_str(1), "1.0B");
        assert_eq!(size_str(1024), "1.0KB");
        assert_eq!(size_str(1536), "1.5KB");
        assert_eq!(size_str(1024 * 1024), "1.0MB");
        assert_eq!(size_str(3 * 1024 * 1024 * 1024), "3.0GB");
        // 超过 TB 也不能越界
        let huge = size_str(i64::MAX);
        assert!(huge.ends_with("TB"), "{huge}");
    }

    #[test]
    fn kind_follows_the_java_extension_classification() {
        // 对端把 1/2 当图片/视频、6 当文件夹：普通文件绝不能发 1
        let root = temp_dir("kind");
        std::fs::create_dir_all(&root).unwrap();
        for name in ["pic.PNG", "clip.mp4", "song.mp3", "app.apk", "doc.bin"] {
            std::fs::write(root.join(name), b"x").unwrap();
        }
        let items = to_items(&[root.join("pic.PNG"), root.join("clip.mp4"), root.join("song.mp3"), root.join("app.apk"), root.join("doc.bin"), root.clone()]);
        let kinds: Vec<i32> = items.iter().map(|item| item.kind()).collect();
        assert_eq!(kinds, vec![FILE_TYPE_IMAGE, FILE_TYPE_VIDEO, FILE_TYPE_AUDIO, FILE_TYPE_APK, FILE_TYPE_FILE, FILE_TYPE_FOLDER]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn transfer_json_layout_matches_the_protocol() {
        let root = temp_dir("json");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("note.txt"), b"123").unwrap();
        let mut items = to_items(&[root.join("note.txt")]);
        items[0].file_id = "abc".into();
        let from = super::super::device::Device {
            name: "本机".into(),
            ip: "192.168.1.2".into(),
            port: 5856,
            mode: super::super::device::MODE_WIN,
            uuid: "me".into(),
            data_version: 5,
            ..Default::default()
        };
        let to = super::super::device::Device { name: "手机".into(), ip: "192.168.1.3".into(), port: 5856, ..Default::default() };
        let meta = TransferMeta::build(&items, &from, &to, true);
        let text = meta.to_json();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["resume"].as_i64(), Some(1));
        assert_eq!(value["files"][0]["fileId"].as_str(), Some("abc"));
        assert_eq!(value["files"][0]["fileType"].as_i64(), Some(5), "普通文件必须是 FILE(5)，写成 1 会被手机当图片");
        assert_eq!(value["files"][0]["length"].as_i64(), Some(3));
        assert_eq!(value["files"][0]["name"].as_str(), Some("note.txt"));
        assert_eq!(value["files"][0]["toUser"].as_str(), Some("手机"));
        assert_eq!(value["fromDevice"]["devMode"].as_i64(), Some(2));
        let back = TransferMeta::parse(&text).expect("自己写的 JSON 必须能读回");
        assert_eq!(back.files.len(), 1);
        assert!(back.resume_enabled());
        assert_eq!(back.peer(&back.from_device).ip, "192.168.1.2");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn transfer_json_tolerates_missing_fields() {
        let meta = TransferMeta::parse(r#"{"files":[{"fileId":"x","name":"n","length":9,"fileType":5,"children":[]}]}"#)
            .expect("缺 fromDevice 也要能解");
        assert_eq!(meta.file_count(), 1);
        assert!(!meta.resume_enabled());
        assert_eq!(meta.files[0].children.len(), 0);
    }
}
