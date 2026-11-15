//! 极简 ZIP 写出器（只存不压，method = 0）。
//!
//! 原版用 `zip/` 目录里的 miniz，压缩级别是默认的 6。这里改成「存储」方式有两个理由：
//! 网页端打包下载的几乎都是图片、视频、已经压过的文档，deflate 再压一遍收益接近零，
//! 却要额外背一个压缩依赖；而 ZIP 的存储格式是完全合法的，Windows/macOS/手机
//! 自带解压器都能直接打开。
//!
//! 布局照 ZIP 规范：若干组「本地头 + 数据」，然后中央目录，最后 EOCD。
//! 本地头里的 CRC 与长度要等数据写完才知道，所以先写占位、再回 seeks 补丁。

use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::lanshare::crypto::new_id;

/// 原版 `CACHE_DIR`：压缩包都落在 `保存目录/.cache/` 里。
pub const CACHE_DIR: &str = "/.cache";

const LOCAL_SIGNATURE: u32 = 0x04034b50;
const CENTRAL_SIGNATURE: u32 = 0x02014b50;
const END_SIGNATURE: u32 = 0x06054b50;
/// bit 11：文件名是 UTF-8。
const FLAG_UTF8: u16 = 0x0800;
/// 本地头里 CRC 字段的偏移（签名 4 + 版本 2 + 标志 2 + 方法 2 + 时间 2 + 日期 2）。
const CRC_OFFSET: u64 = 14;
/// 固定长度的本地头部分，名字从第 30 字节开始。
const LOCAL_HEADER: u64 = 30;

/// CRC-32（IEEE），ZIP 要求的校验算法。
fn crc32_table() -> &'static [u32; 256] {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (index, slot) in table.iter_mut().enumerate() {
            let mut value = index as u32;
            for _ in 0..8 {
                value = if value & 1 != 0 { 0xEDB8_8320 ^ (value >> 1) } else { value >> 1 };
            }
            *slot = value;
        }
        table
    })
}

fn crc32_update(crc: u32, bytes: &[u8]) -> u32 {
    let mut value = crc ^ 0xFFFF_FFFF;
    for byte in bytes {
        value = crc32_table()[((value ^ *byte as u32) & 0xFF) as usize] ^ (value >> 8);
    }
    value ^ 0xFFFF_FFFF
}

fn civil_from_days(days: i64) -> (i64, u64, u64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_097 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// MS-DOS 日期与时间，ZIP 的时间字段就长这样。
///
/// ZIP 存的是本地时间，这里没有可靠的时区来源，按 UTC 写：解压工具显示的时间戳
/// 会偏几个小时，不影响任何内容。
fn dos_time(seconds: u64) -> (u16, u16) {
    let (mut year, mut month, mut day) = civil_from_days((seconds / 86400) as i64);
    if year < 1980 {
        (year, month, day) = (1980, 1, 1);
    }
    // 字段布局是 小时(5) | 分钟(6) | 秒/2(5)：秒只存偶数，奇数秒直接截掉
    let secs = seconds % 86400;
    let time = ((secs / 3600) << 11 | (secs % 3600) / 60 << 5 | secs % 60 / 2) as u16;
    let date = (((year - 1980) as u16) << 9 | (month as u16) << 5 | day as u16).max(1);
    (time, date)
}

/// 中央目录里的一条。
struct Record {
    name: Vec<u8>,
    crc: u32,
    size: u64,
    offset: u64,
    time: u16,
    date: u16,
}

/// 流式 ZIP 写出器，直接落盘。
pub struct ZipWriter {
    out: BufWriter<std::fs::File>,
    records: Vec<Record>,
    offset: u64,
}

impl ZipWriter {
    pub fn new(file: std::fs::File) -> Self {
        Self { out: BufWriter::new(file), records: Vec::new(), offset: 0 }
    }

    /// 把一个磁盘文件写成一条 ZIP 条目。`name` 是包内的相对路径，用 `/` 分隔。
    pub fn push_file(&mut self, name: &str, source: &Path) -> std::io::Result<()> {
        let name = normalize(name);
        if name.is_empty() {
            return Ok(());
        }
        let mut file = std::fs::File::open(source)?;
        let metadata = file.metadata()?;
        if metadata.len() > u32::MAX as u64 {
            return Err(std::io::Error::other(format!("单个文件超过 4GB，ZIP32 存不下：{}", source.display())));
        }
        let stamp = metadata
            .modified()
            .ok()
            .and_then(|when| when.duration_since(UNIX_EPOCH).ok())
            .map(|span| span.as_secs())
            .unwrap_or(0);
        let (time, date) = dos_time(stamp);
        let name_bytes = name.into_bytes();
        let offset = self.offset;

        // 占位本地头：CRC 与长度先写 0，数据落完再回填
        let mut head = Vec::with_capacity(LOCAL_HEADER as usize + name_bytes.len());
        head.extend_from_slice(&LOCAL_SIGNATURE.to_le_bytes());
        head.extend_from_slice(&20u16.to_le_bytes());
        head.extend_from_slice(&FLAG_UTF8.to_le_bytes());
        head.extend_from_slice(&0u16.to_le_bytes()); // method = store
        head.extend_from_slice(&time.to_le_bytes());
        head.extend_from_slice(&date.to_le_bytes());
        head.extend_from_slice(&0u32.to_le_bytes());
        head.extend_from_slice(&0u32.to_le_bytes());
        head.extend_from_slice(&0u32.to_le_bytes());
        head.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        head.extend_from_slice(&0u16.to_le_bytes());
        head.extend_from_slice(&name_bytes);
        self.out.write_all(&head)?;

        let mut crc = 0u32;
        let mut total = 0u64;
        let mut buf = vec![0u8; 8192];
        loop {
            let read = file.read(&mut buf)?;
            if read == 0 {
                break;
            }
            crc = crc32_update(crc, &buf[..read]);
            self.out.write_all(&buf[..read])?;
            total += read as u64;
        }

        self.offset = offset + head.len() as u64 + total;
        self.out.flush()?;
        let inner = &mut self.out;
        inner.seek(SeekFrom::Start(offset + CRC_OFFSET))?;
        inner.write_all(&crc.to_le_bytes())?;
        inner.write_all(&(total as u32).to_le_bytes())?;
        inner.write_all(&(total as u32).to_le_bytes())?;
        inner.seek(SeekFrom::End(0))?;
        self.records.push(Record { name: name_bytes, crc, size: total, offset, time, date });
        Ok(())
    }

    /// 写完中央目录与 EOCD，返回整包大小。
    pub fn finish(mut self) -> std::io::Result<u64> {
        let directory_start = self.offset;
        for record in &self.records {
            let mut entry = Vec::with_capacity(46 + record.name.len());
            entry.extend_from_slice(&CENTRAL_SIGNATURE.to_le_bytes());
            entry.extend_from_slice(&20u16.to_le_bytes()); // version made by
            entry.extend_from_slice(&20u16.to_le_bytes()); // version needed
            entry.extend_from_slice(&FLAG_UTF8.to_le_bytes());
            entry.extend_from_slice(&0u16.to_le_bytes()); // method = store
            entry.extend_from_slice(&record.time.to_le_bytes());
            entry.extend_from_slice(&record.date.to_le_bytes());
            entry.extend_from_slice(&record.crc.to_le_bytes());
            entry.extend_from_slice(&(record.size as u32).to_le_bytes());
            entry.extend_from_slice(&(record.size as u32).to_le_bytes());
            entry.extend_from_slice(&(record.name.len() as u16).to_le_bytes());
            entry.extend_from_slice(&[0u8; 8]); // extra len + comment len + disk start
            entry.extend_from_slice(&0u16.to_le_bytes()); // disk number
            entry.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            entry.extend_from_slice(&0u32.to_le_bytes()); // external attrs
            entry.extend_from_slice(&(record.offset as u32).to_le_bytes());
            entry.extend_from_slice(&record.name);
            self.out.write_all(&entry)?;
            self.offset += entry.len() as u64;
        }
        let count = self.records.len() as u16;
        let directory_size = self.offset - directory_start;
        let mut end = Vec::with_capacity(22);
        end.extend_from_slice(&END_SIGNATURE.to_le_bytes());
        end.extend_from_slice(&[0u8; 4]); // 两个 disk 号
        end.extend_from_slice(&count.to_le_bytes());
        end.extend_from_slice(&count.to_le_bytes());
        end.extend_from_slice(&(directory_size as u32).to_le_bytes());
        end.extend_from_slice(&(directory_start as u32).to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes());
        self.out.write_all(&end)?;
        self.offset += end.len() as u64;
        self.out.flush()?;
        Ok(self.offset)
    }
}

/// 条目名规范化：反斜杠换正斜杠、去掉开头的 `/`
/// （原版是按字符长度切片的，切出来的目录条目会带一个前导斜杠）。
fn normalize(name: &str) -> String {
    name.replace('\\', "/").trim_matches('/').to_string()
}

/// 收集 `path` 要写进包里的全部条目：`(包内名字, 源文件)`。
///
/// `prefix` 是「要削掉的绝对目录」，整棵子树都用同一个值——原版是按字符数切片的
/// （`path.mid(pathPrefix)`），传的就是父目录的长度，递归下去不再变。
/// 与原版 `composeZip()` 一样：目录本身不进包，只把里面的文件按相对路径摊平，
/// 空目录会被丢掉。
pub fn plan(prefix: &str, path: &Path, out: &mut Vec<(String, PathBuf)>) {
    if path.is_file() {
        out.push((path_strip(prefix, path), path.to_path_buf()));
        return;
    }
    if !path.is_dir() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(path) else { return };
    let mut children: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    children.sort();
    for child in children {
        plan(prefix, &child, out);
    }
}

/// 把绝对路径换成相对 `prefix` 的条目名；`prefix` 不匹配时退回文件名。
fn path_strip(prefix: &str, path: &Path) -> String {
    let full = path.to_string_lossy().replace('\\', "/");
    let prefix = prefix.replace('\\', "/");
    if !prefix.is_empty() && full.starts_with(prefix.as_str()) {
        normalize(&full[prefix.len()..])
    } else {
        normalize(path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or(full).as_str())
    }
}

/// 在 `target_dir`（就是 `.cache`）里生成一个包，返回文件名与完整路径。
pub fn create(target_dir: &Path, items: &[(String, PathBuf)]) -> std::io::Result<(String, PathBuf)> {
    std::fs::create_dir_all(target_dir)?;
    let name = format!("{}.zip", new_id());
    let path = target_dir.join(&name);
    let file = std::fs::File::create(&path)?;
    let mut zip = ZipWriter::new(file);
    for (entry, source) in items {
        zip.push_file(entry, source)?;
    }
    zip.finish()?;
    Ok((name, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("lanshare-zip-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// 本地头里回填的 CRC/长度要和中央目录一致，正文按「存储」原样躺在包里。
    #[test]
    fn a_single_file_archive_is_structurally_valid() {
        let root = temp_dir("one");
        let source = root.join("a.txt");
        std::fs::write(&source, b"hello zip").unwrap();
        let (name, path) = create(&root, &[("a.txt".to_string(), source.clone())]).unwrap();
        assert!(name.ends_with(".zip"));
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(&LOCAL_SIGNATURE.to_le_bytes()));
        assert!(bytes.windows(9).any(|window| window == b"hello zip"), "正文要原样可寻");

        // 本地头：CRC、压缩长、原始长（偏移 14/18/22）
        let local_crc = u32::from_le_bytes(bytes[14..18].try_into().unwrap());
        let local_size = u32::from_le_bytes(bytes[18..22].try_into().unwrap());
        assert_eq!(local_size, 9);
        assert_eq!(local_crc, crc32_update(0, b"hello zip"));

        let tail = &bytes[bytes.len() - 22..];
        assert_eq!(&tail[0..4], &END_SIGNATURE.to_le_bytes());
        assert_eq!(u16::from_le_bytes(tail[8..10].try_into().unwrap()), 1, "条目数");
        let directory_at = u32::from_le_bytes(tail[16..20].try_into().unwrap()) as usize;
        assert!(bytes[directory_at..].starts_with(&CENTRAL_SIGNATURE.to_le_bytes()), "中央目录偏移要指对");
        // 中央目录里记的 CRC 与偏移也要一致
        assert_eq!(u32::from_le_bytes(bytes[directory_at + 16..directory_at + 20].try_into().unwrap()), local_crc);
        assert_eq!(u32::from_le_bytes(bytes[directory_at + 42..directory_at + 46].try_into().unwrap()), 0);
        std::fs::remove_dir_all(&root).ok();
    }

    /// 目录要摊平成 `子目录/文件`，且前导斜杠必须被吃掉。
    #[test]
    fn directories_are_flattened_with_relative_names() {
        let root = temp_dir("tree");
        std::fs::create_dir_all(root.join("box/inner")).unwrap();
        std::fs::write(root.join("box/inner/x.bin"), b"12345").unwrap();
        std::fs::write(root.join("box/top.txt"), b"t").unwrap();
        let mut items = Vec::new();
        plan(&root.to_string_lossy(), &root.join("box"), &mut items);
        let names: Vec<String> = items.iter().map(|(name, _)| name.clone()).collect();
        assert!(names.contains(&"box/top.txt".to_string()), "{names:?}");
        assert!(names.contains(&"box/inner/x.bin".to_string()), "{names:?}");
        assert!(names.iter().all(|name| !name.starts_with('/')), "不能有前导斜杠: {names:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// 单个文件的 `prefix` 是它的父目录，切出来会带一个前导斜杠（原版的
    /// `/compressFiles` 就是这么把名字写进包的），`normalize` 负责吃掉。
    #[test]
    fn a_single_file_keeps_only_its_name() {
        let root = temp_dir("single");
        std::fs::write(root.join("x.txt"), b"x").unwrap();
        let mut items = Vec::new();
        plan(&root.to_string_lossy(), &root.join("x.txt"), &mut items);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].0, "x.txt", "父目录长度不带头绪，切出来必然是 /x.txt");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn crc32_matches_the_known_vector() {
        // “123456789” 的 CRC-32 是 0xCBF43926，这是校验表写错时最先炸的地方
        let mut crc = 0u32;
        for chunk in [b"1234".as_slice(), b"56789".as_slice()] {
            crc = crc32_update(crc, chunk);
        }
        assert_eq!(crc, 0xCBF4_3926);
    }

    /// 1981-01-02 01:01:01：三个字段各占一段，最容易看出移位写错。
    #[test]
    fn dos_time_encodes_the_epoch_day_one() {
        // 1980 是闰年，所以 1981-01-02 距 epoch 是 4019 天
        let (time, date) = dos_time(4019 * 86400 + 3600 + 60 + 1);
        assert_eq!(time, (1 << 11) | (1 << 5), "奇数秒存不下，被截成偶数秒");
        assert_eq!(time & 0x1F, 0);
        assert_eq!(date, (1 << 9) | (1 << 5) | 2, "年偏移 1、1 月 2 日");
        assert_eq!(date >> 9, 1, "1981 年偏移 1");
        assert_eq!((date >> 5) & 0x0F, 1);
        assert_eq!(date & 0x1F, 2);
        // 1980 年之前的时间戳会写进非法日期，钳到 1980-01-01
        let (time, date) = dos_time(0);
        assert_eq!(date >> 9, 0);
        assert_eq!(date & 0x1F, 1);
        assert_eq!(time, 0, "钳位后仍按原时刻的 00:00:00 写");
    }

    /// 反斜杠要换成正斜杠、前后多余的 `/` 要吃掉，不然某些解压工具会把条目当成绝对路径。
    #[test]
    fn normalization_keeps_entry_names_portable() {
        assert_eq!(normalize(r"\a\b\c.txt"), "a/b/c.txt");
        assert_eq!(normalize("/lead/"), "lead");
        assert_eq!(normalize(""), "");
    }
}
