//! 查询本机的公网 IPv6 地址，仅供「网页二维码」对话框在下拉里多列一项用。
//!
//! 为什么走外部服务而不是直接枚举网卡：`nics()` 现在只产 IPv4，且链路本地
//! (`fe80::…`) 地址对扫码访问没有意义。`ipv6.lanshares.com` 这个服务靠观察来源地址
//! 回吐调用方的公网 IPv6——响应体就是那个地址。只有**用 IPv6 连过去**才拿得到 v6，
//! 所以下面刻意从 DNS 结果里挑 IPv6 地址建连。
//!
//! 这条路径完全阻塞、且要出网，绝不能放在 UI 线程上：调用方把它丢到后台线程，
//! 拿到的结果再发回界面。任何一步失败（无 IPv6、无网络、响应看不懂）都返回 `None`，
//! 界面上就是不多这一项，不弹错、不卡住。

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// 查询服务：主机名、路径、端口。响应体预期是一个裸 IPv6 文本。
const HOST: &str = "ipv6.lanshares.com";
const PATH: &str = "/ip";
const PORT: u16 = 80;
/// 建连后单次读写的兜底超时，避免后台线程被一条半开的连接永久挂住。
const TIMEOUT: Duration = Duration::from_secs(5);

/// 取本机公网 IPv6；拿不到就 `None`。
pub fn query_ipv6() -> Option<String> {
    // 只挑 IPv6 解析结果：走 IPv4 连过去，服务只会回来源 v4，不是我们要的。
    let address = (HOST, PORT)
        .to_socket_addrs()
        .ok()?
        .find(|addr| addr.is_ipv6())?;
    let mut stream = TcpStream::connect(address).ok()?;
    let _ = stream.set_read_timeout(Some(TIMEOUT));
    let _ = stream.set_write_timeout(Some(TIMEOUT));
    let request = format!("GET {PATH} HTTP/1.1\r\nHost: {HOST}\r\nConnection: close\r\nAccept: text/plain\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    stream.flush().ok()?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).ok()?;
    parse_ipv6(String::from_utf8_lossy(&raw).as_ref())
}

/// 从一段完整的 HTTP 响应文本里抠出 IPv6：跳过头部、按 token 扫描，取第一个能解析成
/// `Ipv6Addr` 的（先剔掉一圈引号/逗号）。这样能容下裸文本与被引号包裹的响应体，也能
/// 跳过 chunked 传输时混进来的十六进制长度行（那些都解析不成 IPv6）。拆成纯函数便于单测。
fn parse_ipv6(response: &str) -> Option<String> {
    // 头部与正文以空行分隔；个别代理漏掉空行时退化成整段扫描。
    let body = response.split_once("\r\n\r\n").map(|(_, rest)| rest).unwrap_or(response);
    for token in body.split_whitespace() {
        let clean = token.trim_matches(|c| c == '"' || c == '\'' || c == ',' || c == ';');
        if clean.parse::<std::net::Ipv6Addr>().is_ok() {
            return Some(clean.to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::parse_ipv6;

    /// 纯文本响应体：直接是地址。
    #[test]
    fn plain_body_is_parsed() {
        let text = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\n2408:8000:1:2::5\n";
        assert_eq!(parse_ipv6(text).as_deref(), Some("2408:8000:1:2::5"));
    }

    /// 响应体把地址包成 JSON 字符串（`"..."`）时，一圈引号要能剔掉。
    #[test]
    fn quoted_body_is_parsed() {
        let text = "HTTP/1.1 200 OK\r\n\r\n\"2408:8000::1\"";
        assert_eq!(parse_ipv6(text).as_deref(), Some("2408:8000::1"));
    }

    /// 拿不到 IPv6（只有 v4 / 空体 / 非地址文本）时返回 `None`，界面上就是不多那一行。
    #[test]
    fn non_ipv6_body_yields_none() {
        assert_eq!(parse_ipv6("HTTP/1.1 200 OK\r\n\r\n1.2.3.4"), None);
        assert_eq!(parse_ipv6("HTTP/1.1 200 OK\r\n\r\n"), None);
    }
}
