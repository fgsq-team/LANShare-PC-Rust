//! 网页版服务：把原版的 `LHttpServer` + `HttpServer` + `LWebSocketServer` 三层
//! 合成这一个模块。
//!
//! 没有独立的监听端口：网页流量与文件传输共用同一个 TCP 端口，由
//! [`crate::lanshare::tcp`] 在读到首 4 字节时分流进来（见 [`looks_like_http`]）。
//! 每条连接起一个工作线程，走完 [`serve`] 的一次「读请求 → 找路由 → 写响应 → 关闭」，
//! 只有升级成 WebSocket 的连接会在处理器里持续循环。

pub mod api;
pub mod media;
pub mod qr;
pub mod sessions;
pub mod tokens;
pub mod zip;

// HTTP / WebSocket 框架层
pub mod server;
// 兼容旧扁平路径：api.rs 与本文件仍以 embed/http/multipart/ws 直呼其名
pub use server::{embed, http, multipart, ws};

use std::sync::Arc;

use crate::lanshare::stream::Conn;
use crate::lanshare::Engine;

/// 分流判据：原版取首 4 字节转大写后判 `startsWith("GET") || startsWith("POST")`，
/// 其余（含 `HEAD`/`PUT`/`DELETE`）一律按非法连接关掉，宁可不服务也不误判魔数。
pub fn looks_like_http(head: &[u8; 4]) -> bool {
    let upper: Vec<u8> = head.iter().map(u8::to_ascii_uppercase).collect();
    upper.starts_with(b"GET") || upper.starts_with(b"POST")
}

/// 路由表：进程内只建一次，处理器都是无状态函数指针。
fn routes() -> &'static [http::Route] {
    static ROUTES: std::sync::OnceLock<Vec<http::Route>> = std::sync::OnceLock::new();
    ROUTES.get_or_init(api::routes)
}

/// 处理一条已被判定为 HTTP 的连接。`head` 是分流时读到的那 4 字节，要补回请求行开头。
///
/// 收尾分两种：普通响应走优雅关闭（可能还有没读完的请求体，硬关会触发 RST 让浏览器
/// 读不到已写出的响应）；已升级的 WebSocket 只能直接关——排干会把会话自己读死。
pub fn serve(engine: Arc<Engine>, conn: Conn, head: [u8; 4]) -> std::io::Result<()> {
    let mut request = match http::read_request(&conn, &head) {
        Ok(request) => request,
        Err(error) => {
            conn.shutdown();
            return Err(error);
        }
    };
    // 头部读完了。后面可能是正文、文件流，或者一整条 WebSocket 会话，不能带超时。
    let _ = conn.set_read_timeout(None);
    let upgraded = request.header("upgrade").eq_ignore_ascii_case("websocket");
    let mut response = http::Response::new();
    response.range = request.range;
    let outcome = dispatch(&engine, &mut request, &mut response);
    if upgraded {
        conn.shutdown();
    } else {
        conn.graceful_close();
    }
    outcome
}

/// 先过令牌守卫，再按路由表分发。守卫拦下时已经把 302 写出去了，返回 `Ok` 即可。
fn dispatch(engine: &Arc<Engine>, request: &mut http::Request, response: &mut http::Response) -> std::io::Result<()> {
    if !api::guard(engine, request, response)? {
        return Ok(());
    }
    match http::find(routes(), &request.method, &request.url) {
        Some(Ok(entry)) => (entry.handler)(engine, request, response),
        Some(Err(())) => response.write_405(&request.conn),
        None => response.write_404(&request.conn),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 只认 `GET`/`POST`：`HEAD` 落到文件传输分支会被当成坏魔数，与原版一致。
    #[test]
    fn only_get_and_post_are_treated_as_http() {
        assert!(looks_like_http(b"GET "));
        assert!(looks_like_http(b"get "));
        assert!(looks_like_http(b"POST"));
        assert!(looks_like_http(b"post"));
        assert!(!looks_like_http(b"HEAD"), "原版不认 HEAD");
        assert!(!looks_like_http(b"PUT "), "原版不认 PUT");
        assert!(!looks_like_http(b"DELE"), "原版不认 DELETE");
        assert!(!looks_like_http(&[0, 0, 0x12, 0x34]), "LANShare 魔数不能被误判成 HTTP");
    }

    /// 路由表要在第一次用到时建好，并且方法受限的端点不能被别的方法撞进来。
    #[test]
    fn the_route_table_covers_every_original_endpoint() {
        let table = routes();
        for pattern in ["/", "/hello", "/favicon.ico", "/initConfig", "/checkPass", "/files", "/file/*", "/media", "/imageload/*", "/drawable", "/images/*", "/css/*", "/js/*", "/wss", "/compressMedias", "/compressFiles", "/downloadZipFile", "/uploadFile", "/chatUploadFile"] {
            assert!(table.iter().any(|entry| entry.pattern == pattern), "缺端点 {pattern}");
        }
        assert!(http::find(table, "GET", "/").unwrap().is_ok());
        assert!(http::find(table, "POST", "/checkPass").unwrap().is_ok());
        assert!(http::find(table, "GET", "/checkPass").unwrap().is_err(), "checkPass 只收 POST");
        assert!(http::find(table, "GET", "/nope").is_none());
    }
}
