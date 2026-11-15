//! HTTP / WebSocket 框架层，对应 C++ 版 `httpserver/`（HttpServer、Request/Response、
//! UploadInputStream、LWebSocketServer）。

pub mod embed;
pub mod http;
pub mod multipart;
pub mod ws;
