//! 各协议版本的收发实现，对应 C++ 版 `src/version/`（`four/` 与 `five/`）。
//!
//! 同时以旧扁平名（`fsend4`、`fsrv4`……）再导出，保证引擎根与外部对这些路径的引用不变。

pub mod v4;
pub mod v5;

pub use v4::send as fsend4;
pub use v4::server as fsrv4;
pub use v5::resume;
pub use v5::send as fsend5;
pub use v5::server as fsrv5;
