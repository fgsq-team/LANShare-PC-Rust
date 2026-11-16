//! V5 断点续传协议收发，对应 C++ 版 `src/version/five/`（FileSendV5 / FileServerV5 / ResumeStore）。

pub mod resume;
pub mod send;
pub mod server;

// 同 `manager/mod.rs`：为叶子文件既有的 `super::` 引用补上引擎根的符号。
#[allow(unused_imports)]
use crate::lanshare::{Engine, TransferInfo, config, crypto, device, fsend4, fsrv4, proto, receive_info, stream, tcp, walk};
