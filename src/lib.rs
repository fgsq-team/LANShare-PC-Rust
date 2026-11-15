//! 一个用 WindUI（跨平台自绘 UI）写的局域网聊天/文件传输客户端。
//!
//! 拆成库 + 可执行两个目标：`lanshare` 引擎与 UI 都在库里，方便 `cargo test`
//! 直接跑协议/加密的单元测试，`main.rs` 只负责拉起窗口。

pub mod app;
pub mod assets;
pub mod lanshare;
pub mod model;
pub mod theme;
pub mod utils;
pub mod views;
pub mod web;
