//! 更新检测：查版本服务器上属于 Rust 线的那一行，纯阻塞 + 纯函数，不认识界面。
//!
//! 协议与原版 `UpdateChecker` 逐字对齐：`GET /get_version?type=<通道>&versionCode=<数值码>
//! &versionName=<名>`，回一段 JSON，用到的字段是 `versionCode` / `version` / `fileUrl` /
//! `updateContent` / `isForceUpdate` / `forceUpdateMinVersion`。服务端已经给这条线开了
//! `release_windows_rust` 那一行，所以通道名与数值码的编排都必须和那一行对齐——对不上不会
//! 报错，只会「每次启动都提示有新版」或「永远提示没更新」。
//!
//! 与原版两处不同，判据一条没改：
//! - `updateContent` 在服务端存的是整页 HTML（原版丢给 `QMessageBox`，Qt 认 HTML 顺带排了版）。
//!   windui 没有 HTML 引擎，这里先折成纯文本（[`html_to_text`]）。
//! - 结果走程序自己的遮罩面板与轻提示，不再起系统模态框。
//!
//! 整条路径都要出网且完全阻塞，绝不能放在 UI 线程上：调用方把它丢后台线程，结果再发回界面
//! （见 `app::pump::Pump::spawn_update_check`）。

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use serde::Deserialize;

/// 版本服务器，与原版 `LANSHARE_SERVER` 同一个。明文 HTTP：下面手写的客户端不做 TLS，
/// 这个常量要改成 `https` 得连带换实现，不是换个串就完事。
pub const SERVER: &str = "http://lanshares.com";
/// 主机与端口：从 `SERVER` 里拆出来写死，省掉一个 URL 解析器。改上面那个就得改这里。
const HOST: &str = "lanshares.com";
const PORT: u16 = 80;
/// 建连与单次读写的兜底超时。后台线程也不该被一条半开的连接永久挂住。
const TIMEOUT: Duration = Duration::from_secs(6);
/// 响应体量级只有 1~2 KB，多给一档上限：坏服务回吐超大 body 时不至于把内存吃穿。
const MAX_BODY: usize = 4 << 20;

/// 本程序的数值版本号，对应原版 `LANSHARE_VERSION`（2601008）与安卓 `versionCode`（2601009）：
/// **前四位 `YYMM` + 末三位流水**，发一版往前推一位。
///
/// 值不在这里写：它在 `Cargo.toml` 的 `[package.metadata.lanshare] version-code`，由 build.rs
/// 读出来注成 `LANSHARE_VERSION_CODE`，发版时和那里的 `version` 一起改。之所以另立一个数而不是
/// 从 `2.0.0-Rust-dev` 换算：原版那两个数（`LANSHARE_VERSION` 与 `LANSHARE_VERSION_NAME`）本来就
/// 各写各的，服务端那一行填的也是这个编排。要是换成 `主*100万 + 次*1000 + 修订`，本程序只剩
/// 2000000，跟服务端那一行的 2601008 一比就成了「每次启动都提示有新版」。
pub const VERSION_CODE: i64 = digits(env!("LANSHARE_VERSION_CODE"));

/// 编译期把注入的那串数字转成 `i64`：`str::parse` 不是 `const fn`，只能自己按位累加。
/// 任何一位不是数字（包括空串）都 panic 在编译期——宁可构建失败，也不要带着 0 跑起来：
/// 后者的现象只是「每次启动都提示有新版」，在界面上根本查不出原因。
const fn digits(raw: &str) -> i64 {
    let bytes = raw.as_bytes();
    if bytes.is_empty() {
        panic!("注入的 LANSHARE_VERSION_CODE 是空的");
    }
    let mut out: i64 = 0;
    let mut i = 0;
    while i < bytes.len() {
        // 先 `wrapping_sub` 再比大小：非数字的字节（引号、符号、空格）会落到 0..=9 之外
        let digit = bytes[i].wrapping_sub(b'0');
        if digit > 9 {
            panic!("注入的 LANSHARE_VERSION_CODE 不是纯数字");
        }
        out = out * 10 + digit as i64;
        i += 1;
    }
    out
}

/// 版本名：直接取包版本，「关于」页显示的、查询串里报的、exe 版本资源里的都是它。
pub fn version_name() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// 更新通道，对应原版 `APP_TYPE`。`_rust` 后缀把这条线和 C++ 那套代码隔开，服务端两行各自
/// 维护，互不干扰。
///
/// 不学原版按 debug 分叉出 `debug_windows_rust`：那条线没人登记过，`cargo run` 出来的调试版
/// 去查只会得到「没更新」，看着像这功能坏了。
pub fn channel() -> &'static str {
    if cfg!(windows) {
        "release_windows_rust"
    } else if cfg!(target_os = "macos") {
        "release_macos_rust"
    } else {
        "release_linux_rust"
    }
}

/// 服务端那一行的样子。`content` 已经折成纯文本，`deleted` 那行为 `isdel`。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// 服务端登记的数值版本号，和本程序的 [`VERSION_CODE`] 比大小。
    pub version_code: i64,
    /// 给人看的版本名，进标题「有新版本更新: …」。
    pub version: String,
    /// 下载地址；服务端偶尔写相对路径，见 [`download_url`]。
    pub file_url: String,
    /// 更新说明正文（HTML 折出来的纯文本）。
    pub content: String,
    /// 服务端标的「必须更新」开关。单看它不够，见 [`decide`]。
    pub force: bool,
    /// 低于这个数值版本才真的强更，对应原版 `forceUpdateMinVersion`。
    pub force_min: i64,
    /// 这一行已经下架。原版压根没读 `isdel`，那是它会拿服务器返回的那一行——服务端挑行时
    /// 已经滤过；我们宁可自己再挡一道。
    pub deleted: bool,
}

/// 要弹给用户的提示。`code` 留着给「不更新」写进配置，`force` 为真时不给退路。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    /// 服务端那一行的数值版本号。
    pub code: i64,
    pub version: String,
    pub body: String,
    pub url: String,
    pub force: bool,
}

/// 查一次更新。阻塞，只准在后台线程上调；`Err` 里那句直接拼进「检测更新失败：」。
pub fn query() -> Result<Report, String> {
    let path = request_path();
    let body = get(path.as_str())?;
    parse(body.as_str())
}

/// 请求路径。拆出来是为了能单测：通道名、参数名、数值码只要有一位写错，服务端就挑不到
/// 属于这条线的行，而现象只是「没更新」，不看请求串根本查不出来。
fn request_path() -> String {
    format!("/get_version?type={}&versionCode={version_code}&versionName={}", channel(), encode(version_name()), version_code = VERSION_CODE)
}

/// 手写一次 HTTP/1.1 取回：`Connection: close` 读完即断，本项目没有 HTTP client 依赖，
/// 与 `utils::public_ip` 同一套做法。
fn get(path: &str) -> Result<String, String> {
    let address = (HOST, PORT)
        .to_socket_addrs()
        .map_err(|error| format!("域名解析失败：{error}"))?
        .next()
        .ok_or_else(|| format!("{HOST} 解析不到地址"))?;
    let mut stream = TcpStream::connect_timeout(&address, TIMEOUT).map_err(|error| format!("连不上 {HOST}：{error}"))?;
    let _ = stream.set_read_timeout(Some(TIMEOUT));
    let _ = stream.set_write_timeout(Some(TIMEOUT));
    let request = format!("GET {path} HTTP/1.1\r\nHost: {HOST}\r\nUser-Agent: LANShare/{}\r\nAccept: application/json\r\nConnection: close\r\n\r\n", version_name());
    stream.write_all(request.as_bytes()).map_err(|error| format!("请求发不出去：{error}"))?;
    stream.flush().map_err(|error| format!("请求发不出去：{error}"))?;
    let mut raw = Vec::new();
    // 读满为止，但带上限：`read_to_end` 本身不设限，坏服务可以一直吐。这里不用 `by_ref`，
    // 那个名字在 `Read` 与 `Write` 上各有一个，直接写会撞出歧义。
    (&mut stream).take(MAX_BODY as u64).read_to_end(&mut raw).map_err(|error| format!("响应读不完：{error}"))?;
    if raw.len() >= MAX_BODY {
        return Err("响应超过 4MB，判定为异常".to_owned());
    }
    take_body(&raw)
}

/// 把响应切成正文：先看状态行，非 2xx 直接报错（301 那种跳转会回一段 HTML，硬喂 JSON
/// 解析只会得到一句看不懂的话）；再按空行剥头，最后按 `Transfer-Encoding` 决定要不要拆 chunk。
fn take_body(raw: &[u8]) -> Result<String, String> {
    // 头部与正文之间一定有一个空行；没有就是响应被截断了，正文一段都取不到
    let Some(at) = index_of(raw, b"\r\n\r\n") else {
        return Err("响应不完整：找不到头部与正文的分隔空行".to_owned());
    };
    let head = String::from_utf8_lossy(&raw[..at]).into_owned();
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default();
    let code: u16 = status.parse().map_err(|_| format!("响应没有状态码（开头是 {:?}）", &head[..head.len().min(40)]))?;
    if !(200..300).contains(&code) {
        return Err(format!("服务器回了 {code}"));
    }
    let lower = head.to_ascii_lowercase();
    let rest = if lower.contains("transfer-encoding: chunked") || lower.contains("transfer-encoding:chunked") {
        // 长度行与 chunk 之间的边界都是 ASCII，按字节切完再合起来解码正文
        String::from_utf8_lossy(&dechunk(&raw[at + 4..])).into_owned()
    } else {
        String::from_utf8_lossy(&raw[at + 4..]).into_owned()
    };
    if rest.trim().is_empty() {
        return Err("服务器回了空正文".to_owned());
    }
    Ok(rest)
}

/// 解 chunked 传输：`<十六进制长度>\r\n<那么多字节>\r\n` 一段一段接，遇到 0 长度即止。
/// 半包（截断）就拿已齐的部分，后面的长度行解不出来就停。
fn dechunk(mut data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    // 读到没有长度行为止（截断的尾巴、结束 chunk 之后的余数），下面每一处 `break` 都是提前收手
    while let Some(line_end) = index_of(data, b"\r\n") {
        let head = String::from_utf8_lossy(&data[..line_end]).into_owned();
        // 长度行可以带 `;扩展`
        let digits = head.split(';').next().unwrap_or_default().trim();
        let Ok(size) = usize::from_str_radix(digits, 16) else { break };
        if size == 0 {
            break;
        }
        let start = line_end + 2;
        let Some(end) = start.checked_add(size) else { break };
        if data.len() < end {
            out.extend_from_slice(&data[start..]);
            break;
        }
        out.extend_from_slice(&data[start..end]);
        // 跳过这个 chunk 尾上的 `\r\n`，接着读下一个长度行
        data = &data[end..];
        data = data.strip_prefix(b"\r\n".as_slice()).unwrap_or(data);
    }
    out
}

/// 在字节串里找子串的第一处位置。为上面这两个切分动作专门写的：`str::find` 只吃字符串，
/// 而正文按字节切才不会在多字节汉字的中间劈开。
fn index_of(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|window| window == needle)
}

/// 查询串取值的最小百分号编码：只留 unreserved 那几个字符，其余按 UTF-8 逐字节转 `%XX`。
/// 版本号里出现 `+` 时不编码会被服务端当成空格。
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' | b'~' => out.push(*byte as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// 服务端那一行的原始形状。每一个 `rename` 都是契约的一部分：字段名写成蛇形就全默认成
/// 零值，而现象只是「一直已是最新」。
#[derive(Deserialize)]
struct Payload {
    #[serde(rename = "versionCode", default)]
    version_code: i64,
    #[serde(rename = "version", default)]
    version: String,
    #[serde(rename = "fileUrl", default)]
    file_url: String,
    #[serde(rename = "updateContent", default)]
    content: String,
    #[serde(rename = "isForceUpdate", default)]
    force: bool,
    #[serde(rename = "forceUpdateMinVersion", default)]
    force_min: i64,
    #[serde(rename = "isdel", default)]
    deleted: bool,
}

/// JSON -> [`Report`]，顺带把那段 HTML 说明折成纯文本。
fn parse(body: &str) -> Result<Report, String> {
    let payload: Payload = serde_json::from_str(body.trim()).map_err(|error| format!("响应不是预期的 JSON：{error}"))?;
    Ok(Report {
        version_code: payload.version_code,
        version: payload.version,
        file_url: payload.file_url,
        content: html_to_text(&payload.content),
        force: payload.force,
        force_min: payload.force_min,
        deleted: payload.deleted,
    })
}

/// 该不该提示、提示成什么样。逐条对原版 `UpdateChecker::handleReply`：
/// - 强更的判据是「开关开着 **且** 本地码低于 `forceUpdateMinVersion`」，单看那个布尔不够；
/// - 「服务端码更大」还要过一道闸：这一版是不是用户之前点过「不更新」的（`skipped`）。
///   **手动检测不看这道闸**，否则用户在「关于」里点检查会得到一句「已是最新」，而他明明拒过；
/// - 下架的行不提。
pub fn decide(report: &Report, current: i64, skipped: i64, manual: bool) -> Option<Prompt> {
    if report.deleted {
        return None;
    }
    let force = report.force && current < report.force_min;
    let newer = report.version_code > current && (manual || report.version_code > skipped);
    if !(newer || force) {
        return None;
    }
    Some(Prompt {
        code: report.version_code,
        version: report.version.clone(),
        body: report.content.clone(),
        url: download_url(&report.file_url),
        force,
    })
}

/// 下载地址：原版给不是 `http` 开头的值会前拼上服务器地址，照做（服务端偶尔真写相对路径）。
fn download_url(file_url: &str) -> String {
    let trimmed = file_url.trim();
    if trimmed.is_empty() || trimmed.starts_with("http") {
        return trimmed.to_owned();
    }
    if trimmed.starts_with('/') {
        format!("{SERVER}{trimmed}")
    } else {
        format!("{SERVER}/{trimmed}")
    }
}

/// 标签里在这一行之后要断行的那几个。`<li>` 另外补一个圆点，剩下的只换行——原版那份说明
/// 就是标题 + 列表 + 一段下载渠道，压平到这种程度够用。
const BLOCKS: [&str; 14] = ["p", "div", "li", "ul", "ol", "br", "hr", "tr", "table", "pre", "h1", "h2", "h3", "h4"];

/// 把服务端那段 HTML 折成纯文本：剥掉所有标签，块级标签处换行，`<head>`/`<style>`/`<script>`
/// 里的文字整段跳过，常见实体还原。`<a>` 的 `href` 属性丢掉，只留可见文字——那份通告里
/// 可见文字本身就是下载链接。
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2 + 16);
    let mut rest = html;
    // 正在 `<head>`/`<style>`/`<script>` 里头：那些文字不是给用户看的
    let mut hidden = false;
    while let Some(start) = rest.find('<') {
        let (text, after) = rest.split_at(start);
        if !hidden {
            out.push_str(text);
        }
        let Some(end) = after.find('>') else {
            // 没有闭合的尖括号：剩下的当正文尾巴处理
            if !hidden {
                out.push_str(&after[1..]);
            }
            return tidy(out.as_str());
        };
        let tag = &after[1..end];
        rest = &after[end + 1..];
        let closing = tag.starts_with('/');
        let name = tag.trim_start_matches('/').trim_start().split([' ', '\t', '\r', '\n', '/', '>']).next().unwrap_or_default();
        match name {
            "head" | "style" | "script" | "title" => hidden = !closing,
            "br" => out.push('\n'),
            _ if hidden => {}
            _ if BLOCKS.contains(&name) => {
                out.push('\n');
                if name == "li" && !closing {
                    out.push_str("· ");
                }
            }
            _ => {}
        }
    }
    if !hidden {
        out.push_str(rest);
    }
    tidy(out.as_str())
}

/// 收拾成能看的文本：行内连续空白压成一个空格，连续空行压成一个，整段去首尾空行。
fn tidy(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let flat = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if flat.is_empty() && lines.last().is_some_and(|last| last.is_empty()) {
            continue;
        }
        lines.push(flat);
    }
    decode_entities(lines.join("\n").as_str()).trim_matches('\n').trim().to_owned()
}

/// 只还原这份通告里真会出现的那几个实体，外加 `&#十进制;` / `&#x十六进制;`。认不出的原样留着，
/// 好过把一个乱码当数字吃掉。
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let Some(offset) = after.find(';') else {
            out.push_str(after);
            return out;
        };
        // 下面切的全是 `rest`，而 `offset` 是从 `after` 算的：两处都得带上 `start`，
        // 否则正文里有汉字时这一步会切到多字节字符中间去（直接 panic）。
        let end = start + offset;
        let entity = &rest[start + 1..end];
        let decoded: Option<&str> = match entity {
            "amp" => Some("&"),
            "lt" => Some("<"),
            "gt" => Some(">"),
            "quot" => Some("\""),
            "apos" => Some("'"),
            "nbsp" => Some(" "),
            _ => None,
        };
        match decoded {
            Some(text) => out.push_str(text),
            None => match entity.strip_prefix('#') {
                Some(digits) => {
                    let code = digits
                        .strip_prefix(['x', 'X'])
                        .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                        .or_else(|| digits.parse::<u32>().ok());
                    match code.and_then(char::from_u32) {
                        Some(ch) => out.push(ch),
                        None => out.push_str(&rest[start..=end]),
                    }
                }
                None => out.push_str(&rest[start..=end]),
            },
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 服务端那一行的原文形状（字段名与真实响应一致，正文截取过）。
    const ROW: &str = r#"{"createTs":1713669866000,"fileUrl":"http://fgsqw.com/release/LANShare_windows_x64_1.4.zip?device=web&type=windows","forceUpdateMinVersion":2400212,"id":38,"isForceUpdate":false,"isdel":false,"type":"release_windows_rust","updateContent":"<h1>更新：V2.0</h1><ol><li>修复一个问题</li></ol>","updateTs":1791466298000,"version":"2.0","versionCode":2601008}"#;

    /// 通道名与数值码必须落在服务端那一行的编排里：`YYMM` + 三位流水。写成一个看起来
    /// 像「2.0.0 换算出来的数」就全错了——本程序的码只剩两百万，比服务端任何一行都小。
    #[test]
    fn version_code_keeps_the_original_yymm_sequence_encoding() {
        let year_month = VERSION_CODE / 1000;
        let sequence = VERSION_CODE % 1000;
        assert!((24..=99).contains(&(year_month / 100)), "{VERSION_CODE} 的前两位不是年份：{year_month}");
        assert!((1..=12).contains(&(year_month % 100)), "第 3~4 位是月份，要落在 1~12：{year_month}");
        assert!((0..1000).contains(&sequence), "末三位是流水号：{sequence}");
    }

    /// 查询串的三个参数名一个都不能错，且 `type` 要带上 Rust 线的后缀。
    #[test]
    fn request_path_names_the_rust_channel() {
        let path = request_path();
        assert!(path.starts_with("/get_version?"), "{path}");
        assert!(path.contains("type=release_windows_rust") || path.contains("type=release_macos_rust") || path.contains("type=release_linux_rust"), "通道名少了 _rust 后缀：{path}");
        assert!(path.contains(&format!("versionCode={VERSION_CODE}")), "{path}");
        assert!(path.contains("versionName="), "{path}");
    }

    /// `+` 在查询串里是空格，得编码；`-` 和点号本来就好，别多此一举变成 `%2D`。
    #[test]
    fn plus_signs_in_a_version_are_escaped_but_safe_characters_are_not() {
        assert_eq!(encode("2.0.0-dev"), "2.0.0-dev");
        assert_eq!(encode("2.0.0+dev"), "2.0.0%2Bdev");
    }

    /// 字段名全是驼峰外加一个 `isdel`：serde 的 `rename` 就是契约本身，这一条直接喂真响应
    /// 的形状，改名漏一个会立刻在这里露出来（界面上只是「永远已是最新」，查不出来）。
    #[test]
    fn a_real_row_parses_into_every_field_we_read() {
        let report = parse(ROW).expect("这一行应当解得开");
        assert_eq!(report.version_code, 2601008);
        assert_eq!(report.version, "2.0");
        assert_eq!(report.file_url, "http://fgsqw.com/release/LANShare_windows_x64_1.4.zip?device=web&type=windows");
        assert_eq!(report.force_min, 2400212);
        assert!(!report.force);
        assert!(!report.deleted);
        assert_eq!(report.content, "更新：V2.0\n\n· 修复一个问题");
    }

    /// 缺字段的行不能炸：全都退回零值/空串，顶多不提。
    #[test]
    fn a_partial_row_still_parses() {
        let report = parse(r#"{"versionCode":2612001}"#).expect("缺字段应当走默认值");
        assert_eq!(report.version_code, 2612001);
        assert_eq!(report.version, "");
    }

    /// 非 JSON（错误页、网关页）要回一句人话，而不是 serde 那种长篇内部描述。
    #[test]
    fn a_non_json_body_says_so() {
        let error = parse("<html><body>502 Bad Gateway</body></html>").unwrap_err();
        assert!(error.starts_with("响应不是预期的 JSON"), "{error}");
    }

    #[test]
    fn headers_are_stripped_and_only_2xx_gets_through() {
        let ok = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{}".to_vec();
        assert_eq!(take_body(&ok).unwrap(), "{}");
        // 301 回的是跳转页：不能拿去喂 JSON 解析，要报「服务器回了 301」
        let redirect = b"HTTP/1.1 301 Moved Permanently\r\nLocation: https://x\r\n\r\n<html>".to_vec();
        assert_eq!(take_body(&redirect).unwrap_err(), "服务器回了 301");
        let empty = b"HTTP/1.1 200 OK\r\n\r\n".to_vec();
        assert!(take_body(&empty).unwrap_err().contains("空正文"));
        // 只有头没有空行的截断响应：不能去切越界的正文
        assert!(take_body(b"HTTP/1.1 200 OK\r").unwrap_err().contains("找不到头部"));
    }

    /// chunked 传输里混着十六进制长度行和 chunk 之间的 CRLF，不拆干净就喂不进 JSON 解析。
    /// 三段长度 6/7/4 拼起来恰好是那个 17 字节的对象。
    #[test]
    fn chunked_body_is_assembled_from_its_pieces() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n6\r\n{\"vers\r\n7\r\nion\":\"2\r\n4\r\n.0\"}\r\n0\r\n\r\n".to_vec();
        assert_eq!(take_body(&raw).unwrap(), r#"{"version":"2.0"}"#);
    }

    /// 下载链接是给用户点出去的，相对路径必须补全成绝对地址。
    #[test]
    fn relative_download_urls_get_the_server_prefix() {
        assert_eq!(download_url("http://a/b.zip"), "http://a/b.zip");
        assert_eq!(download_url("https://a/b.zip"), "https://a/b.zip");
        assert_eq!(download_url("/release/a.zip"), "http://lanshares.com/release/a.zip");
        assert_eq!(download_url("release/a.zip"), "http://lanshares.com/release/a.zip");
    }

    /// 提不提，取决于「服务端码更大」+「不是刚拒过的那一版」；手动检测无视拒绝记录。
    #[test]
    fn a_strictly_newer_code_prompts_unless_that_version_was_declined() {
        let report = Report { version_code: 2612001, ..Default::default() };
        assert!(decide(&report, 2611001, -1, false).is_some());
        // 同号、更小，都不提
        assert!(decide(&Report { version_code: 2611001, ..Default::default() }, 2611001, -1, true).is_none());
        assert!(decide(&Report { version_code: 2610001, ..Default::default() }, 2611001, -1, true).is_none());
        // 自动检测遇到用户拒过的那一版：闭嘴
        assert!(decide(&report, 2611001, 2612001, false).is_none());
        // 手动检测同一个版本照样提——用户主动问的，得给实话
        assert!(decide(&report, 2611001, 2612001, true).is_some());
    }

    /// 强更是「开关 + 本地码低于那一道线」两个条件一起：只有开关不够，
    /// 否则一台已经够新的机器也会被拦下来。
    #[test]
    fn force_needs_the_flag_and_the_local_code_under_the_minimum() {
        let report = Report { version_code: 2611001, force: true, force_min: 2612001, ..Default::default() };
        let prompt = decide(&report, 2611001, -1, false).expect("强更时即使同号也要提");
        assert!(prompt.force);
        // 已经高过那道线：这次不算强更
        let fresh = Report { version_code: 2611001, force: true, force_min: 2600000, ..Default::default() };
        assert!(decide(&fresh, 2611001, -1, false).is_none());
        // 只有开关没有那条线（`force_min` 为 0）也不算
        assert!(decide(&Report { version_code: 2611001, force: true, ..Default::default() }, 2611001, -1, false).is_none());
    }

    /// 拒过的那条记录由「不更新」写回配置，得带上服务端那一行的码。
    #[test]
    fn the_prompt_carries_the_code_to_record() {
        let prompt = decide(&Report { version_code: 2612001, version: "2.1".to_owned(), ..Default::default() }, 2611001, -1, false).unwrap();
        assert_eq!(prompt.code, 2612001);
        assert_eq!(prompt.version, "2.1");
    }

    /// 下架的行不提——那是运维已经撤回的通告。
    #[test]
    fn a_retired_row_is_never_shown() {
        let report = Report { version_code: 2612001, deleted: true, ..Default::default() };
        assert!(decide(&report, 2611001, -1, true).is_none());
    }

    /// 服务端那份 HTML 说明折成文本：标签、`<head>` 里的 meta 都不该留在正文里，
    /// 标题与列表项各占一行，实体还原。
    #[test]
    fn a_real_html_note_folds_to_plain_text() {
        let html = r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
    <meta charset="UTF-8">
    <title>更新通知</title>
</head>
<body>
<h1>版本：V1.4.0</h1>

<h2>更新内容</h2>
<ol>
    <li>优化主题配色，增加绿色主题</li>
    <li>修复网页端文件传输 &gt;50MB 失败的问题（A&amp;B）</li>
</ol>

<h2>下载渠道</h2>
<ul>
    <li>蓝奏云：<a href="https://wwazz.lanzouw.com/iqky84b6cl6b">https://wwazz.lanzouw.com/iqky84b6cl6b</a></li>
</ul>
</body>
</html>"#;
        let text = html_to_text(html);
        assert!(!text.contains('<'), "标签没剥干净：{text}");
        assert!(!text.contains("charset"), "head 里的内容混进正文了：{text}");
        assert!(!text.contains("href="), "属性混进正文了：{text}");
        assert!(text.contains("版本：V1.4.0\n"), "标题要单独一行：{text:?}");
        assert!(text.contains("· 优化主题配色，增加绿色主题"), "列表项要带圆点：{text:?}");
        assert!(text.contains(">50MB"), "`&gt;` 没还原：{text:?}");
        assert!(text.contains("A&B"), "`&amp;` 没还原：{text:?}");
        assert!(text.contains("https://wwazz.lanzouw.com/iqky84b6cl6b"), "链接文字要留着：{text:?}");
        assert!(!text.contains("\n\n\n"), "空行没压掉：{text:?}");
    }

    /// 数字实体与不认识的串：前者还原，后者原样留着，不能吃掉半句。
    #[test]
    fn entities_decode_when_recognised_and_survive_when_not() {
        assert_eq!(html_to_text("&#20184; &#xZZ; &unknown; &amp;"), "付 &#xZZ; &unknown; &");
    }

    /// 没有闭合尖括号的残破 HTML 也不能 panic，剩下的当正文。
    #[test]
    fn an_unclosed_tag_does_not_panic() {
        assert_eq!(html_to_text("<p>前半段<b>后半段"), "前半段后半段");
        assert_eq!(html_to_text(""), "");
    }
}
