//! `multipart/form-data` 的定长请求体读取与解析。
//!
//! 原版是 `UploadInputStream`：只认第一个部件、按「总长减去头部」硬算正文长度，
//! 于是拖拽多个文件时后面那几个会被并进第一个文件里。这里改成按分隔符切分，
//! 每个部件各落一个文件，其余语义（只取 `filename`、写到指定目录）保持不变。

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::lanshare::stream::Conn;

/// 一次已落盘的上传部件。
pub struct Upload {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
}

/// 定长请求体的分块读取器。`Conn` 内部是 `Arc<Mutex<TcpStream>>`，可以按块取。
pub struct Body {
    conn: Conn,
    left: u64,
    buf: Vec<u8>,
    start: usize,
    end: usize,
}

/// 一次从 socket 取多少字节。上传动辄几百兆，逐字节读会把锁开销放大到不可接受。
const CHUNK: usize = 8192;

impl Body {
    pub fn new(conn: Conn, length: u64) -> Self {
        Self { conn, left: length, buf: vec![0u8; CHUNK], start: 0, end: 0 }
    }

    /// 补一块数据。返回 `false` 表示已经读完 `content_length` 声明的全部字节。
    fn fill(&mut self) -> std::io::Result<bool> {
        if self.start < self.end {
            return Ok(true);
        }
        if self.left == 0 {
            return Ok(false);
        }
        self.start = 0;
        self.end = 0;
        let take = std::cmp::min(self.left, CHUNK as u64) as usize;
        self.conn.read_into(&mut self.buf[..take])?;
        self.end = take;
        self.left -= take as u64;
        Ok(true)
    }

    pub fn exhausted(&self) -> bool {
        self.left == 0 && self.start >= self.end
    }

    /// 读一个字节。到末尾返回 `None`。
    fn byte(&mut self) -> std::io::Result<Option<u8>> {
        if !self.fill()? {
            return Ok(None);
        }
        let value = self.buf[self.start];
        self.start += 1;
        Ok(Some(value))
    }

    /// 读到行尾，去掉 CRLF/LF。
    pub fn read_line(&mut self) -> std::io::Result<Vec<u8>> {
        let mut line = Vec::new();
        while let Some(value) = self.byte()? {
            if value == b'\n' {
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(line);
            }
            line.push(value);
        }
        Ok(line)
    }

    pub fn read_exact(&mut self, n: usize) -> std::io::Result<Vec<u8>> {
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            match self.byte()? {
                Some(value) => out.push(value),
                None => break,
            }
        }
        Ok(out)
    }

    /// 整个正文读进内存（JSON 请求体这种小体才这么用）。
    pub fn read_all(&mut self) -> std::io::Result<Vec<u8>> {
        let mut out = Vec::with_capacity(std::cmp::min(self.left as usize + (self.end - self.start), 1 << 20));
        while let Some(value) = self.byte()? {
            out.push(value);
        }
        Ok(out)
    }

    /// 一直写到 `needle` 出现为止，`needle` 本身不写出去。
    /// 返回 `false` 表示正文结束也没遇到（体被截断）。
    ///
    /// 只保留最近 `needle.len()-1` 个字节做候选：任何匹配都必然以「刚读到的这个字节」
    /// 结尾结束，否则在更早的某一步就已经命中了。
    fn copy_until(&mut self, writer: &mut dyn Write, needle: &[u8]) -> std::io::Result<bool> {
        let keep = needle.len().saturating_sub(1);
        let mut window: Vec<u8> = Vec::with_capacity(needle.len());
        while let Some(value) = self.byte()? {
            window.push(value);
            if window.len() >= needle.len() && window[window.len() - needle.len()..] == *needle {
                writer.write_all(&window[..window.len() - needle.len()])?;
                return Ok(true);
            }
            if window.len() > keep {
                let flush_to = window.len() - keep;
                writer.write_all(&window[..flush_to])?;
                window.drain(..flush_to);
            }
        }
        writer.write_all(&window)?;
        Ok(false)
    }

    /// 把剩余全部字节写出去（非 multipart 的裸体用）。
    pub fn copy_rest(&mut self, writer: &mut dyn Write) -> std::io::Result<u64> {
        let mut total = 0u64;
        while let Some(value) = self.byte()? {
            writer.write_all(&[value])?;
            total += 1;
        }
        Ok(total)
    }
}

/// 从 `Content-Disposition` 里取 `name="…"` / `filename="…"`。
fn field(text: &str, key: &str) -> Option<String> {
    let tail = text.get(text.find(&format!("{key}="))? + key.len() + 1..)?;
    let tail = tail.trim_start();
    if let Some(rest) = tail.strip_prefix('"') {
        return Some(rest.split('"').next()?.to_string());
    }
    // 不带引号的写法：到分号或行尾为止
    Some(tail.split([';', '\r', '\n']).next()?.trim().to_string())
}

/// 只保留文件名本体，挡掉 `..\..\x` 与绝对路径这种穿越写法。
fn safe_name(raw: &str) -> String {
    let tail = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    if tail.is_empty() || tail == "." || tail == ".." { String::new() } else { tail.to_string() }
}

/// 解析 multipart 请求体，把每个带 `filename` 的部件落到 `dir`。
///
/// 分隔符前后的换行处理与 RFC 2046 一致：部件正文以 `\r\n--boundary` 结束，
/// 最后一个分隔符后面紧跟 `--`。
pub fn read_upload(body: &mut Body, boundary: &str, dir: &Path) -> std::io::Result<Vec<Upload>> {
    if boundary.is_empty() {
        return Err(std::io::Error::other("请求不是 multipart/form-data"));
    }
    std::fs::create_dir_all(dir)?;
    let delimiter = format!("--{boundary}");
    // 跳过 preamble，定位到第一个分隔行
    let mut first = None;
    loop {
        let line = body.read_line()?;
        if line.starts_with(delimiter.as_bytes()) {
            first = Some(line);
            break;
        }
        if body.exhausted() {
            break;
        }
    }
    let mut out = Vec::new();
    let terminator = format!("\r\n{delimiter}");
    let Some(line) = first else { return Err(std::io::Error::other("multipart 体里找不到分隔符")) };
    if line[delimiter.len()..].starts_with(b"--") {
        // 分隔行直接就是结束标记：体里一个部件都没有
        return Ok(out);
    }
    loop {
        // 部件头部：读到空行为止
        let mut filename: Option<String> = None;
        loop {
            let line = body.read_line()?;
            if line.is_empty() {
                break;
            }
            let text = String::from_utf8_lossy(&line).into_owned();
            if text.contains("filename=") {
                if let Some(found) = field(&text, "filename") {
                    let clean = safe_name(&found);
                    if !clean.is_empty() {
                        filename = Some(clean);
                    }
                }
            }
        }
        let name = filename.unwrap_or_else(|| format!("upload_{}", out.len()));
        let path = unique_path(dir, &name);
        let mut file = std::fs::File::create(&path)?;
        // 分隔符之后的 `tail` 若已是 CRLF，说明正文紧接着开始；若是 `--`，说明整体结束
        let found = body.copy_until(&mut file, terminator.as_bytes())?;
        file.flush()?;
        let size = file.metadata().map(|meta| meta.len()).unwrap_or(0);
        out.push(Upload { name: path.file_name().map(|os| os.to_string_lossy().into_owned()).unwrap_or(name), path, size });
        if !found {
            break;
        }
        // `copy_until` 吃掉了 `\r\n--boundary`，接下来两个字节决定还有没有下一个部件
        let mark = body.read_exact(2)?;
        if mark.starts_with(b"--") || body.exhausted() {
            break;
        }
    }
    Ok(out)
}

/// 同名文件不覆盖：收目录里已经有 `a.txt` 时就写 `a (1).txt`。
/// 原版直接用 `path + fileName` 覆盖旧文件，网页端连续传两次同名文件会丢一份。
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let stem = Path::new(name).file_stem().map(|os| os.to_string_lossy().into_owned()).unwrap_or_default();
    let suffix = Path::new(name).extension().map(|os| format!(".{}", os.to_string_lossy())).unwrap_or_default();
    for index in 1..1000 {
        let candidate = dir.join(format!("{stem} ({index}){suffix}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{stem}-{}{suffix}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_of(text: &[u8]) -> Body {
        // 用一对环回套接字喂数据，这样解析器走的是真实的「分块到达」路径
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mut client = std::net::TcpStream::connect(address).unwrap();
        let (server, _) = listener.accept().unwrap();
        std::io::Write::write_all(&mut client, text).unwrap();
        Body::new(Conn::new(server).unwrap(), text.len() as u64)
    }

    const BOUNDARY: &str = "----WebKitFormBoundaryABCD";

    fn sample(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut raw = Vec::new();
        for (name, data) in files {
            raw.extend_from_slice(format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes());
            raw.extend_from_slice(data);
            raw.extend_from_slice(b"\r\n");
        }
        raw.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        raw
    }

    #[test]
    fn a_single_file_lands_on_disk_with_the_exact_bytes() {
        let dir = std::env::temp_dir().join(format!("lanshare-upload-{}", std::process::id()));
        let payload = sample(&[("周报.txt", "前三行\n正文".as_bytes())]);
        let uploads = read_upload(&mut body_of(&payload), BOUNDARY, &dir).expect("应当解析出一个部件");
        assert_eq!(uploads.len(), 1);
        assert_eq!(uploads[0].name, "周报.txt");
        let text = std::fs::read_to_string(&uploads[0].path).unwrap();
        assert_eq!(text, "前三行\n正文", "结尾的 CRLF 不能被算进正文");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 拖拽上传一次可以带好几个文件，原版会把它们粘成一个——这里必须拆开。
    #[test]
    fn several_parts_become_several_files() {
        let dir = std::env::temp_dir().join(format!("lanshare-upload-multi-{}", std::process::id()));
        let payload = sample(&[("a.txt", b"aaa".as_slice()), ("b.txt", b"bbbb".as_slice())]);
        let uploads = read_upload(&mut body_of(&payload), BOUNDARY, &dir).expect("应当解析出两个部件");
        assert_eq!(uploads.len(), 2);
        assert_eq!(std::fs::read(&uploads[0].path).unwrap(), b"aaa");
        assert_eq!(std::fs::read(&uploads[1].path).unwrap(), b"bbbb");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 文件名里带路径是本机的常见坑：浏览器的 IE 内核会给完整路径。
    #[test]
    fn path_components_are_stripped_from_the_filename() {
        assert_eq!(safe_name(r"C:\Users\a\b.txt"), "b.txt");
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name(".."), "");
    }

    #[test]
    fn same_name_uploads_do_not_overwrite_each_other() {
        let dir = std::env::temp_dir().join(format!("lanshare-upload-clash-{}", std::process::id()));
        let payload = sample(&[("x.bin", b"1".as_slice()), ("x.bin", b"2".as_slice())]);
        let uploads = read_upload(&mut body_of(&payload), BOUNDARY, &dir).expect("同名也要各存一份");
        assert_eq!(uploads.len(), 2);
        assert_ne!(uploads[0].path, uploads[1].path);
        assert_eq!(uploads[1].name, "x (1).bin");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fields_without_a_filename_are_named_by_index() {
        let dir = std::env::temp_dir().join(format!("lanshare-upload-field-{}", std::process::id()));
        let raw = format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"note\"\r\n\r\nhi\r\n--{BOUNDARY}--\r\n");
        let uploads = read_upload(&mut body_of(raw.as_bytes()), BOUNDARY, &dir).expect("没有 filename 也要有落点");
        assert_eq!(uploads[0].name, "upload_0");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_boundary_is_an_error() {
        let dir = std::env::temp_dir().join(format!("lanshare-upload-none-{}", std::process::id()));
        assert!(read_upload(&mut body_of(b"{}"), "", &dir).is_err());
    }
}
