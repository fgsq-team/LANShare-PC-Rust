//! 设备发现与传输协议管理，对应 C++ 版 `src/manager/`（DeviceManager、Tcp/UdpProtocol、
//! LegacyFileTransfer）与 `NetWorldUtils`。

pub mod device;
pub mod legacy;
pub mod nets;
pub mod tcp;
pub mod udp;

// 让子模块内部既有的 `super::` / `super::super::` 引用继续解析到引擎根，
// 迁移目录时无需逐个改写叶子文件的 import。
#[allow(unused_imports)]
use crate::lanshare::{Engine, NetEvent, TransferInfo, config, crypto, fsrv4, fsrv5, proto, stream, walk};
