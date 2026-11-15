//! HTTP/1.1 的请求解析与响应写出，对应原版的 `httpserver/HttpServer.cpp`、
//! `Request.cpp`、`Response.cpp` 三件套。
//!
//! 只做 LANShare 网页版实际用到的那一小块：定长请求体、单段 `Range` 续传、
//! `multipart/form-data` 上传。请求行按字节读、读到空行为止——与原版
//! `readHttpLine()` 一致，因此也允许 TCP 把一行拆成好几个包。

use std::collections::HashMap;
use std::io::{Read, Seek};
use std::path::Path;
use std::sync::Arc;

use crate::lanshare::stream::Conn;
use crate::lanshare::Engine;

use super::multipart::Body;

/// 原版 `Response::HTTP_VERSION`。
const HTTP_VERSION: &str = "HTTP/1.1";
pub const TEXT_PLAIN: &str = "text/plain; charset=UTF-8";
pub const JSON: &str = "application/json";
pub const OCTET_STREAM: &str = "application/octet-stream";

/// 一次已解析完头部的请求。正文要等处理器自己来取，所以请求体只记录长度不预先读。
pub struct Request {
    pub method: String,
    /// 问号之前、已经百分号解码的路径。
    pub url: String,
    /// 问号之后的原始查询串。原版 `getRequestBody()` 在 GET 时就返回它。
    pub query: String,
    /// 查询串拆出来的键值，已经解码。
    pub params: HashMap<String, String>,
    headers: HashMap<String, String>,
    pub client_ip: String,
    pub content_length: u64,
    /// `Range: bytes=a-b`，`b` 缺省时是 `-1`，与原版 `getRangeLength()` 一致。
    pub range: Option<(i64, i64)>,
    pub conn: Conn,
}

impl Request {
    pub fn header(&self, key: &str) -> &str {
        self.headers.get(&key.to_ascii_lowercase()).map(String::as_str).unwrap_or("")
    }

    pub fn param(&self, key: &str) -> &str {
        self.params.get(key).map(String::as_str).unwrap_or("")
    }

    /// 正文的字节流视图，定长 `content_length`。
    pub fn body_source(&self) -> Body {
        Body::new(self.conn.clone(), self.content_length)
    }

    /// 把正文整个读进内存。`Content-Length` 为 0 时返回空，与原版一致。
    pub fn body(&mut self) -> std::io::Result<Vec<u8>> {
        self.body_source().read_all()
    }

    pub fn body_text(&mut self) -> std::io::Result<String> {
        Ok(String::from_utf8_lossy(&self.body()?).into_owned())
    }

    /// GET 时等价于查询串，其余方法是请求体——原版的 `getRequestBody()` 就是这么分叉的。
    pub fn request_text(&mut self) -> std::io::Result<String> {
        if self.method == "GET" {
            return Ok(self.query.clone());
        }
        self.body_text()
    }
}

/// 响应头部累加器。真正落到 socket 是在 `write*` 那一组方法里。
pub struct Response {
    status: u16,
    reason: String,
    headers: Vec<(String, String)>,
    /// 由请求的 `Range` 头带进来，`send_file` 据此决定发 200 还是 206。
    pub range: Option<(i64, i64)>,
}

impl Default for Response {
    fn default() -> Self {
        Self::new()
    }
}

impl Response {
    /// 200 + `Content-Type: text/html` + `Connection: close`，与原版构造函数一致。
    pub fn new() -> Self {
        Self {
            status: 200,
            reason: "OK".to_string(),
            headers: vec![("Content-Type".to_string(), "text/html; charset=utf-8".to_string()), ("Connection".to_string(), "close".to_string())],
            range: None,
        }
    }

    pub fn status(&mut self, code: u16, reason: &str) -> &mut Self {
        self.status = code;
        self.reason = reason.to_string();
        self
    }

    /// 同名头覆盖旧值（原版用 QMap::insert，行为相同）。
    pub fn header(&mut self, key: &str, value: &str) -> &mut Self {
        match self.headers.iter_mut().find(|(name, _)| name == key) {
            Some(slot) => slot.1 = value.to_string(),
            None => self.headers.push((key.to_string(), value.to_string())),
        }
        self
    }

    pub fn content_type(&mut self, value: &str) -> &mut Self {
        self.header("Content-Type", value)
    }

    fn head(&self, length: u64) -> Vec<u8> {
        let mut out = format!("{HTTP_VERSION} {} {}\r\n", self.status, self.reason);
        for (key, value) in &self.headers {
            out.push_str(&format!("{key}: {value}\r\n"));
        }
        out.push_str(&format!("Content-Length: {length}\r\n\r\n"));
        out.into_bytes()
    }

    /// 头部 + 定长正文一次写完。
    pub fn write(&mut self, conn: &Conn, body: &[u8]) -> std::io::Result<()> {
        let mut out = self.head(body.len() as u64);
        out.extend_from_slice(body);
        conn.write_raw(&out)
    }

    pub fn write_bytes(&mut self, conn: &Conn, body: &[u8], content_type: &str) -> std::io::Result<()> {
        self.content_type(content_type);
        self.write(conn, body)
    }

    pub fn write_text(&mut self, conn: &Conn, body: &str) -> std::io::Result<()> {
        self.content_type(TEXT_PLAIN);
        self.write(conn, body.as_bytes())
    }

    pub fn write_json(&mut self, conn: &Conn, body: &str) -> std::io::Result<()> {
        self.content_type(JSON);
        self.write(conn, body.as_bytes())
    }

    pub fn write_404(&mut self, conn: &Conn) -> std::io::Result<()> {
        self.status(404, "Not Found");
        self.write_text(conn, "404 Not Found")
    }

    pub fn write_405(&mut self, conn: &Conn) -> std::io::Result<()> {
        // 原版 `write405()` 的状态行写的是 404，正文才是 405；这是它的既有行为，照抄
        self.status(404, "Method Not Allowed");
        self.write_text(conn, "405 Method Not Allowed")
    }

    pub fn write_500(&mut self, conn: &Conn, message: &str) -> std::io::Result<()> {
        self.status(500, "error");
        self.write_text(conn, if message.is_empty() { "500 error" } else { message })
    }

    pub fn write_302(&mut self, conn: &Conn, message: &str, location: &str) -> std::io::Result<()> {
        self.status(302, "Found");
        self.header("Location", location);
        self.write_text(conn, message)
    }

    /// 流式发送磁盘文件，支持单段 `Range`。文件打不开就回 404，与原版相同。
    ///
    /// 目录一律算「打不开」：Windows 上 `File::open` 本来就会失败（原版正是这样回的 404），
    /// 而 Unix 上能打开成功但读不动，不挡掉就会回一个空正文的 200。
    pub fn send_file(&mut self, conn: &Conn, path: &str) -> std::io::Result<()> {
        let mut file = match std::fs::File::open(path) {
            Ok(file) if file.metadata().map(|meta| meta.is_file()).unwrap_or(false) => file,
            _ => return self.write_404(conn),
        };
        let size = file.metadata().map(|meta| meta.len() as i64).unwrap_or(0).max(0);
        let (start, length, partial) = match self.range {
            Some((raw_start, raw_end)) => {
                let start = raw_start.clamp(0, size);
                let end = if raw_end < 0 { size } else { raw_end.clamp(start, size) + 1 };
                (start, (end - start).max(0), true)
            }
            None => (0, size, false),
        };
        self.header("Accept-Ranges", "bytes");
        if partial {
            self.status(206, "Partial Content");
            // 原版第三段填的是 `len + start`，浏览器要的却是文件总长；这里按规范写
            self.header("Content-Range", &format!("bytes {start}-{}-{size}", (start + length - 1).max(start)));
        }
        self.content_type(&content_type_of(path));
        conn.write_raw(&self.head(length as u64))?;
        let mut left = length as u64;
        let mut buf = vec![0u8; 8192];
        let _ = file.seek(std::io::SeekFrom::Start(start as u64));
        while left > 0 {
            let take = std::cmp::min(left, buf.len() as u64) as usize;
            match file.read(&mut buf[..take]) {
                Ok(0) => break,
                Ok(read) => {
                    conn.write_raw(&buf[..read])?;
                    left -= read as u64;
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        Ok(())
    }
}

/// 按扩展名给 MIME 类型。这一表取自原版 `ContentTypes.cpp` 里与网页、文件分享相关的子集，
/// 命中不到就退回 `application/octet-stream`（原版 `writeFile` 的兜底相同）。
pub fn content_type_of(path: &str) -> String {
    let name = Path::new(path).file_name().map(|os| os.to_string_lossy().into_owned()).unwrap_or_default();
    let suffix = name.rsplit_once('.').map(|(_, tail)| tail.to_ascii_lowercase()).unwrap_or_default();
    let known = match suffix.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "json" => "application/json",
        "txt" | "md" | "log" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "svg" => "image/svg+xml",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "wav" => "audio/x-wav",
        "zip" => "application/zip",
        "rar" => "application/vnd.rar",
        "7z" => "application/x-7z-compressed",
        "gz" => "application/gzip",
        "pdf" => "application/pdf",
        "apk" => "application/vnd.android.package-archive",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "",
    };
    if known.is_empty() { OCTET_STREAM.to_string() } else { known.to_string() }
}

/// 百分号解码，对应原版的 `formatUrl()`。非法转义按原样保留。
pub fn percent_decode(text: &str) -> String {
    let raw = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(raw.len());
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'%' && index + 2 < raw.len() {
            match u8::from_str_radix(&text[index + 1..index + 3], 16) {
                Ok(value) => {
                    out.push(value);
                    index += 3;
                    continue;
                }
                Err(_) => {}
            }
        }
        out.push(if raw[index] == b'+' { b' ' } else { raw[index] });
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 百分号编码，只保留 RFC 3986 的非保留字符。`filename*=UTF-8''` 那里要用。
pub fn percent_encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => {
                out.push(*byte as char);
            }
            value => out.push_str(&format!("%{value:02X}")),
        }
    }
    out
}

/// 路由表里的一条：路径模式、可选的方法限制、处理器。
pub type Handler = dyn Fn(&Arc<Engine>, &mut Request, &mut Response) -> std::io::Result<()> + Send + Sync;

pub struct Route {
    pub pattern: &'static str,
    pub method: Option<&'static str>,
    pub handler: Box<Handler>,
}

/// 注册路由。顺序无关——分发前会按模式长度升序排，与原版 `sortPath()` 相同。
pub fn route(
    pattern: &'static str,
    method: Option<&'static str>,
    handler: impl Fn(&Arc<Engine>, &mut Request, &mut Response) -> std::io::Result<()> + Send + Sync + 'static,
) -> Route {
    Route { pattern, method, handler: Box::new(handler) }
}

/// 通配符路径匹配：`*` 代表「不含斜杠的一段」。
///
/// 原版把它换成 `[^/]*` 再补 `$` 交给 QRegularExpression，但**没有补 `^`**，
/// 于是 `/x/file/a` 也会命中 `/file/*`。这里两头都对齐，免掉那种意外。
pub fn path_matches(pattern: &str, path: &str) -> bool {
    let mut pattern = pattern.as_bytes();
    let mut rest = path.as_bytes();
    while let Some((&head, tail)) = pattern.split_first() {
        if head != b'*' {
            if rest.first() != Some(&head) {
                return false;
            }
            pattern = tail;
            rest = &rest[1..];
            continue;
        }
        pattern = tail;
        if pattern.is_empty() {
            // 结尾的 `*` 吃掉剩下整段，但不能跨过斜杠
            return !rest.contains(&b'/');
        }
        match rest.iter().position(|byte| *byte == b'/') {
            Some(stop) => rest = &rest[stop..],
            None => return false,
        }
    }
    rest.is_empty()
}

/// 从 socket 读一行（不含 CRLF）。`prefix` 用来把分流时多读的字节补回行首。
fn read_line(conn: &Conn, prefix: Option<&[u8]>) -> std::io::Result<Vec<u8>> {
    let mut line = prefix.map(<[u8]>::to_vec).unwrap_or_default();
    let mut byte = [0u8; 1];
    loop {
        if conn.read_into(&mut byte).is_err() {
            return Ok(line);
        }
        if byte[0] == b'\n' {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(line);
        }
        line.push(byte[0]);
    }
}

/// 解析请求行与头部。`head` 是 TCP 分流时已经读过的那 4 字节，要补回请求行开头。
pub fn read_request(conn: &Conn, head: &[u8; 4]) -> std::io::Result<Request> {
    let client_ip = conn.remote_addr().map(|addr| addr.ip().to_string()).unwrap_or_default();
    let first = read_line(conn, Some(head))?;
    let mut method = String::new();
    let mut url = String::new();
    let mut query = String::new();
    let mut params = HashMap::new();
    if !first.is_empty() {
        let line = String::from_utf8_lossy(&first).into_owned();
        let mut parts = line.splitn(3, ' ');
        method = parts.next().unwrap_or_default().to_ascii_uppercase();
        let target = parts.next().unwrap_or_default();
        let (raw_path, raw_query) = match target.find('?') {
            Some(at) => (&target[..at], &target[at + 1..]),
            None => (target, ""),
        };
        url = percent_decode(raw_path);
        query = raw_query.to_string();
        for piece in query.split('&').filter(|piece| !piece.is_empty()) {
            match piece.split_once('=') {
                Some((key, value)) => params.insert(percent_decode(key), percent_decode(value)),
                None => params.insert(percent_decode(piece), String::new()),
            };
        }
    }
    let mut headers = HashMap::new();
    loop {
        let line = read_line(conn, None)?;
        if line.is_empty() {
            break;
        }
        let text = String::from_utf8_lossy(&line).into_owned();
        let Some((key, value)) = text.split_once(':') else { continue };
        headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
    }
    let content_length = headers.get("content-length").and_then(|value| value.trim().parse::<u64>().ok()).unwrap_or(0);
    let range = headers.get("range").and_then(|value| parse_range(value));
    Ok(Request { method, url, query, params, headers, client_ip, content_length, range, conn: conn.clone() })
}

fn parse_range(value: &str) -> Option<(i64, i64)> {
    let tail = value.strip_prefix("bytes=")?;
    let (start, end) = tail.split_once('-')?;
    let start = start.trim().parse::<i64>().ok()?;
    let end = if end.trim().is_empty() { -1 } else { end.trim().parse::<i64>().ok()? };
    Some((start, end))
}

/// 按原版 `sortPath()` 的口径排好序：模式越长越靠后，短的先匹配。
pub fn sorted(mut routes: Vec<Route>) -> Vec<Route> {
    routes.sort_by_key(|entry| entry.pattern.chars().count());
    routes
}

/// 找出第一条命中请求路径的路由。返回值：`None` = 没有任何模式命中（404）；
/// `Some(Err(()))` = 命中了但方法不对（405）。
pub fn find<'a>(routes: &'a [Route], method: &str, url: &str) -> Option<Result<&'a Route, ()>> {
    for entry in routes {
        if !path_matches(entry.pattern, url) {
            continue;
        }
        if let Some(want) = entry.method {
            if method != want {
                return Some(Err(()));
            }
        }
        return Some(Ok(entry));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_pattern_matches_one_segment() {
        assert!(path_matches("/file/*", "/file/a.txt"));
        assert!(path_matches("/images/*", "/images/lanshare.png"));
        assert!(path_matches("/file/*", "/file/"), "空段也要能命中，原版 `[^/]*` 允许零个字符");
        assert!(!path_matches("/file/*", "/file/a/b.txt"), "`*` 不该跨过斜杠");
        assert!(path_matches("/files", "/files"));
        assert!(!path_matches("/files", "/filesx"));
        // 原版少了 `^` 时 `/x/file/a` 会误命中，这里必须不命中
        assert!(!path_matches("/file/*", "/x/file/a"));
    }

    #[test]
    fn percent_codec_round_trips_utf8() {
        let raw = "图片 100%.txt";
        let encoded = percent_encode(raw);
        assert!(encoded.contains('%'), "{encoded}");
        assert_eq!(percent_decode(&encoded), raw);
        assert_eq!(percent_decode("not%zzvalid"), "not%zzvalid");
        assert_eq!(percent_decode("a+b"), "a b");
    }

    #[test]
    fn range_header_is_parsed_like_the_original() {
        assert_eq!(parse_range("bytes=0-1023"), Some((0, 1023)));
        assert_eq!(parse_range("bytes=512-"), Some((512, -1)));
        assert_eq!(parse_range("none"), None);
    }

    #[test]
    fn content_types_cover_the_web_assets_and_common_media() {
        assert_eq!(content_type_of("/a/lanshare.html"), "text/html; charset=utf-8");
        assert_eq!(content_type_of("/a/bootstrap.min.css"), "text/css; charset=utf-8");
        assert_eq!(content_type_of("/a/lanshare.js"), "application/javascript; charset=utf-8");
        assert_eq!(content_type_of("x.PNG"), "image/png");
        assert_eq!(content_type_of("no-suffix"), OCTET_STREAM);
    }

    #[test]
    fn routes_are_sorted_by_pattern_length_and_method_mismatch_is_reported() {
        let routes = sorted(vec![
            route("/file/*", None, |_, _, _| Ok(())),
            route("/", None, |_, _, _| Ok(())),
            route("/checkPass", Some("POST"), |_, _, _| Ok(())),
        ]);
        assert_eq!(routes[0].pattern, "/", "最短的先匹配");
        assert!(find(&routes, "GET", "/checkPass").unwrap().is_err(), "方法不匹配要当 405 处理");
        assert!(find(&routes, "POST", "/checkPass").unwrap().is_ok());
        assert!(find(&routes, "GET", "/nope").is_none(), "没命中任何模式要回 404");
        assert!(find(&routes, "GET", "/file/a.txt").unwrap().is_ok(), "带通配的路径不检查方法");
    }

    /// 响应头必须是「状态行 + 头部 + 空行」，`Content-Length` 与正文一一对齐，
    /// 否则浏览器会一直等到超时才报错。
    #[test]
    fn header_block_ends_with_a_blank_line_and_carries_the_length() {
        let mut response = Response::new();
        response.status(201, "Created").header("X-Kind", "test");
        let head = response.head(7);
        let text = String::from_utf8(head).unwrap();
        assert!(text.starts_with("HTTP/1.1 201 Created\r\n"), "{text}");
        assert!(text.contains("X-Kind: test\r\n"));
        assert!(text.contains("Content-Length: 7\r\n"));
        assert!(text.ends_with("\r\n\r\n"));
    }
}
