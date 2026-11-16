//! V4 协议收发，对应 C++ 版 `src/version/four/`（FileSend / FileServer）。

pub mod send;
pub mod server;

// 同 `manager/mod.rs`：为叶子文件既有的 `super::` 引用补上引擎根的符号。
#[allow(unused_imports)]
use crate::lanshare::{Engine, TransferInfo, config, crypto, device, proto, receive_info, stream, tcp, walk};
