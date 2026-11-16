//! 断点续传的分段存储，对应原版 `ResumeStore`。
//!
//! 布局（根目录 `<保存路径>/.lanshare_resume/`）：
//! - 单文件：`<fileId>.part`
//! - 文件夹：`<folderFileId>/<childFileId>.part`，同目录下一个 `.target` 记录最终目录名
//! - `fileId` 由 `stable_id(senderUuid + "|" + 绝对路径 + "|" + 大小)` 得到，所以重发同一文件能命中旧分段
//! - 分段超过 7 天（按修改时间）在启动时清理

use std::path::{Path, PathBuf};

use super::crypto::stable_id;
use super::device::now_ms;
use super::walk::avoid_duplication;

/// 续传根目录名。
pub const RESUME_DIR: &str = ".lanshare_resume";
/// 分段后缀。
pub const PART_SUFFIX: &str = ".part";
/// 文件夹最终目录名标记。
pub const TARGET_MARKER: &str = ".target";
/// 分段有效期：7 天。
pub const PART_EXPIRE_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// 一个保存目录对应一个存储实例。
#[derive(Debug, Clone)]
pub struct ResumeStore {
    root: PathBuf,
}

impl ResumeStore {
    /// 以收到的文件保存目录为基准，建好续传根目录。
    pub fn new(save_path: &str) -> Self {
        let root = PathBuf::from(save_path).join(RESUME_DIR);
        let _ = std::fs::create_dir_all(&root);
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 稳定 ID：同样的发送方 + 路径 + 大小永远得到同一个值。
    pub fn stable_id(seed: &str) -> String {
        stable_id(seed)
    }

    /// 子文件的续传种子：`senderUuid|绝对路径|大小`。
    pub fn child_seed(sender_uuid: &str, path: &Path, length: i64) -> String {
        format!("{sender_uuid}|{}|{length}", path.to_string_lossy().replace('\\', "/"))
    }

    pub fn file_part(&self, file_id: &str) -> PathBuf {
        self.root.join(format!("{file_id}{PART_SUFFIX}"))
    }

    /// 文件夹的暂存目录（不存在则创建）。
    pub fn folder_stage(&self, folder_file_id: &str) -> PathBuf {
        let dir = self.root.join(folder_file_id);
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    pub fn child_part(&self, folder_file_id: &str, child_file_id: &str) -> PathBuf {
        self.root.join(folder_file_id).join(format!("{child_file_id}{PART_SUFFIX}"))
    }

    /// 已完成的字节数。分段比总长大、或总长未知时删掉分段并从 0 开始（与原版一致）。
    pub fn read_offset(&self, part: &Path, total: i64) -> i64 {
        match std::fs::metadata(part) {
            Ok(meta) => {
                let len = meta.len() as i64;
                if len > total || total <= 0 {
                    let _ = std::fs::remove_file(part);
                    0
                } else {
                    len
                }
            }
            Err(_) => 0,
        }
    }

    /// 记下文件夹最终要落到哪个目录名。
    pub fn write_target(&self, stage: &Path, final_dir: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(stage)?;
        std::fs::write(stage.join(TARGET_MARKER), final_dir.as_bytes())
    }

    pub fn read_target(&self, stage: &Path) -> Option<String> {
        let text = std::fs::read(stage.join(TARGET_MARKER)).ok()?;
        if text.is_empty() {
            return None;
        }
        Some(String::from_utf8_lossy(&text).into_owned())
    }

    /// 分段完成 -> 正式文件（重名时自动加序号），返回最终路径。
    pub fn finalize_part(&self, part: &Path, target_file: &Path) -> PathBuf {
        if let Some(parent) = target_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let final_file = avoid_duplication(target_file);
        if part.exists() {
            move_file(part, &final_file);
        }
        final_file
    }

    /// 把某个子文件的分段搬到目标目录里（保留相对路径），已存在且同大小就丢弃分段。
    pub fn move_part_to_final(&self, part: &Path, final_dir: &Path, relative_name: &str) {
        if relative_name.is_empty() || !part.exists() {
            return;
        }
        let target = final_dir.join(relative_name.replace('/', &format!("{}", std::path::MAIN_SEPARATOR)));
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let same = std::fs::metadata(&target)
            .ok()
            .zip(std::fs::metadata(part).ok())
            .is_some_and(|(target_meta, part_meta)| target_meta.len() == part_meta.len());
        if same {
            let _ = std::fs::remove_file(part);
            return;
        }
        move_file(part, &target);
    }

    pub fn delete(&self, path: &Path) {
        let _ = std::fs::remove_file(path);
    }

    pub fn delete_dir(&self, path: &Path) {
        let _ = std::fs::remove_dir_all(path);
    }

    /// 清掉超过 7 天的分段（启动时调用）。
    pub fn cleanup_expired(&self) {
        self.cleanup_expired_before(now_ms());
    }

    /// 可注入当前时间的版本，方便测试。
    pub fn cleanup_expired_before(&self, now: i64) {
        let Ok(entries) = std::fs::read_dir(&self.root) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            let age = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|elapsed| now - elapsed.as_millis() as i64)
                .unwrap_or(0);
            if age <= PART_EXPIRE_MS {
                continue;
            }
            if meta.is_dir() {
                let _ = std::fs::remove_dir_all(&path);
            } else {
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    /// 续传映射表：`{fileId: 已完成字节}`，JSON 形态（无记录时是 `{}`）。
    pub fn resume_map_json(map: &std::collections::BTreeMap<String, i64>) -> String {
        serde_json::to_string(map).unwrap_or_else(|_| String::from("{}"))
    }

    pub fn parse_resume_map(text: &str) -> std::collections::BTreeMap<String, i64> {
        if text.trim().is_empty() {
            return std::collections::BTreeMap::new();
        }
        serde_json::from_str(text).unwrap_or_default()
    }
}

/// 先试重命名，跨卷或不允许时复制后删除。
fn move_file(from: &Path, to: &Path) {
    if std::fs::rename(from, to).is_ok() {
        return;
    }
    if std::fs::copy(from, to).is_ok() {
        let _ = std::fs::remove_file(from);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(tag: &str) -> (ResumeStore, PathBuf) {
        let root = std::env::temp_dir().join(format!("lanshare-resume-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        (ResumeStore::new(&root.to_string_lossy()), root)
    }

    #[test]
    fn layout_matches_the_original_store() {
        let (store, root) = temp_store("layout");
        assert_eq!(store.root(), root.join(RESUME_DIR).as_path());
        assert!(store.root().exists());
        assert_eq!(store.file_part("f1"), root.join(RESUME_DIR).join("f1.part"));
        assert_eq!(store.child_part("d1", "c1"), root.join(RESUME_DIR).join("d1").join("c1.part"));
        assert_eq!(ResumeStore::stable_id("s|C:/a.txt|10"), stable_id("s|C:/a.txt|10"));
        assert_eq!(ResumeStore::child_seed("s", Path::new("C:\\a\\b.txt"), 10), "s|C:/a/b.txt|10");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_offset_reuses_part_and_rejects_nonsense_totals() {
        let (store, root) = temp_store("offset");
        let part = store.file_part("x");
        std::fs::write(&part, vec![0u8; 4096]).unwrap();
        assert_eq!(store.read_offset(&part, 8192), 4096);
        // 分段比总长大：说明对不上，删掉从头来
        assert_eq!(store.read_offset(&part, 100), 0);
        assert!(!part.exists());
        // 总长未知同样不用分段
        std::fs::write(&part, vec![0u8; 8]).unwrap();
        assert_eq!(store.read_offset(&part, 0), 0);
        assert_eq!(store.read_offset(&root.join("不存在.part"), 10), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn target_marker_round_trip() {
        let (store, root) = temp_store("target");
        let stage = store.folder_stage("folder1");
        assert!(stage.exists());
        assert_eq!(store.read_target(&stage), None);
        store.write_target(&stage, "C:/LANShare/照片").unwrap();
        assert_eq!(store.read_target(&stage).as_deref(), Some("C:/LANShare/照片"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn finalize_renames_part_and_avoids_overwrite() {
        let (store, root) = temp_store("finalize");
        let out = root.join("out");
        std::fs::create_dir_all(&out).unwrap();
        let part = store.file_part("z");
        std::fs::write(&part, b"content").unwrap();
        let target = out.join("note.txt");
        let first = store.finalize_part(&part, &target);
        assert_eq!(first, target);
        assert!(!part.exists(), "分段用完后必须被搬走");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "content");
        // 已有同名文件时加序号
        let part2 = store.file_part("z2");
        std::fs::write(&part2, b"more").unwrap();
        let second = store.finalize_part(&part2, &target);
        assert_eq!(second.file_name().unwrap(), "note(1).txt");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "more");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn child_part_moves_into_relative_path_and_skips_duplicates() {
        let (store, root) = temp_store("child");
        let final_dir = root.join("final");
        let stage = store.folder_stage("d");
        let part = store.child_part("d", "c");
        std::fs::create_dir_all(&stage).unwrap();
        std::fs::write(&part, b"12345").unwrap();
        store.move_part_to_final(&part, &final_dir, "sub/a.txt");
        let target = final_dir.join("sub").join("a.txt");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "12345");
        assert!(!part.exists());
        // 同大小的目标已存在：丢弃分段，不动目标
        std::fs::write(store.child_part("d", "c2"), b"xxxxx").unwrap();
        store.move_part_to_final(&store.child_part("d", "c2"), &final_dir, "sub/a.txt");
        assert!(!store.child_part("d", "c2").exists());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "12345");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn expired_entries_are_removed_by_age() {
        let (store, root) = temp_store("expire");
        std::fs::write(store.file_part("old"), b"x").unwrap();
        std::fs::create_dir_all(store.folder_stage("olddir")).unwrap();
        let fresh = store.file_part("new");
        std::fs::write(&fresh, b"x").unwrap();
        // 刚写的分段不在 7 天窗口外
        store.cleanup_expired();
        assert!(store.file_part("old").exists());
        assert!(store.root().join("olddir").exists());
        assert!(fresh.exists());
        // 把“现在”往前推 8 天，所有条目都算过期
        let future = now_ms() + 8 * 24 * 60 * 60 * 1000;
        store.cleanup_expired_before(future);
        assert!(!store.file_part("old").exists());
        assert!(!store.root().join("olddir").exists(), "过期目录要整个删掉");
        assert!(!fresh.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resume_map_json_round_trip() {
        let mut map = std::collections::BTreeMap::new();
        assert_eq!(ResumeStore::resume_map_json(&map), "{}");
        map.insert("a".to_string(), 123);
        map.insert("b".to_string(), 0);
        let text = ResumeStore::resume_map_json(&map);
        let back = ResumeStore::parse_resume_map(&text);
        assert_eq!(back.get("a"), Some(&123));
        assert_eq!(back.get("b"), Some(&0));
        assert!(ResumeStore::parse_resume_map("").is_empty());
        assert!(ResumeStore::parse_resume_map("不是 JSON").is_empty());
    }
}
