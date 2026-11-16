//! V5 发送：带断点续传的新式文件流。
//!
//! 帧结构与 V4 完全一致，只有两处不同：首帧写 `-3`（`NEW_VERSION_5`）、`fileTransferJson` 顶层
//! 多一个 `resume: 1`；对方同意之后**紧接着读一张偏移量表**（`str {fileId: 已完成字节}`），
//! 然后照 V4 的方式推块，已完成的子文件直接跳过，没完成的从断点 seek 后继续。
//!
//! `fileId` 由发送端生成、接收端只当不透明键用，所以这里把它做成 **稳定 ID**（原版顶层文件用的是
//! 随机 UUID，`.part` 永远命中不到）。种子里额外拼了接收端标识，避免同一文件同时发往两台设备时
//! 两条传输共用一个气泡与取消标记。

use std::collections::BTreeMap;
use std::time::Duration;

use super::device::Device;
use super::proto::NEW_VERSION_5;
use super::resume::ResumeStore;
use super::stream::Conn;
use super::walk::{Item, TransferMeta};
use super::{Engine, crypto, fsend4, tcp};

/// 等待偏移量表的上限：它紧跟在同意之后，正常是毫秒级。
const MAP_WAIT: Duration = Duration::from_secs(30);

/// 给待发送条目填 `fileId`。`resume` 为假时全部用随机 ID（等价 V4 行为）。
pub fn prepare_ids(items: &mut [Item], from: &Device, to: &Device, resume: bool) {
    for item in items {
        for child in item.children.iter_mut() {
            child.file_id = if resume && child.length > 0 && !child.path.as_os_str().is_empty() {
                ResumeStore::stable_id(&ResumeStore::child_seed(&from.uuid, &child.path, child.length))
            } else {
                crypto::new_id()
            };
        }
        item.file_id = if resume && !item.path.as_os_str().is_empty() {
            ResumeStore::stable_id(&top_seed(from, to, item))
        } else {
            crypto::new_id()
        };
    }
}

/// 顶层条目的续传种子：`接收端标识|发送端 uuid|路径|大小`（文件夹多一段 `folder`）。
///
/// 路径统一成 POSIX 风格，同一台机器上换网口、换 IP 写法不会导致 ID 漂移。
fn top_seed(from: &Device, to: &Device, item: &Item) -> String {
    let peer = if to.uuid.is_empty() { to.ip.as_str() } else { to.uuid.as_str() };
    let path = item.path.to_string_lossy().replace('\\', "/");
    let tag = if item.is_dir { "folder|" } else { "" };
    format!("{peer}|{}|{tag}{path}|{}", from.uuid, item.length)
}

/// 把 `items` 发给 `to`（V5，支持续传）。
pub fn send(engine: &Engine, from: &Device, to: &Device, items: &[Item], meta: &TransferMeta) -> std::io::Result<()> {
    let conn = tcp::connect(to)?;
    let encrypted = engine.config().enc_data;
    fsend4::handshake(&conn, from, NEW_VERSION_5, meta, encrypted)?;
    let map = read_resume_map(&conn)?;
    fsend4::transmit(engine, &conn, items, encrypted, Some(&map))
}

/// 读接收端回传的偏移量表：读完就把读超时清掉，后面的阻塞读靠连接关闭唤醒。
fn read_resume_map(conn: &Conn) -> std::io::Result<BTreeMap<String, i64>> {
    let text = {
        conn.set_read_timeout(Some(MAP_WAIT))?;
        let text = conn.read_str();
        let _ = conn.set_read_timeout(None);
        text?
    };
    Ok(ResumeStore::parse_resume_map(&text))
}

#[cfg(test)]
mod tests {
    use super::super::config::Config;
    use super::super::proto::{FS_AGREE, FS_SHARE_FILE, MAGIC};
    use super::super::walk::to_items;
    use super::*;
    use crate::lanshare::NetEvent;
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn engine(tag: &str) -> Arc<Engine> {
        let mut config = Config::default();
        config.save_path = std::env::temp_dir().join(format!("lanshare-fsend5-{tag}")).to_string_lossy().into_owned();
        config.unique_uuid = "uuid-sender".to_string();
        Arc::new(Engine::for_test(config))
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lanshare-fsend5-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn from() -> Device {
        Device { name: "本机".into(), ip: "127.0.0.1".into(), port: 5856, uuid: "sender".into(), data_version: 5, ..Default::default() }
    }

    fn to(port: u16) -> Device {
        Device { name: "手机".into(), ip: "127.0.0.1".into(), port: port as i32, uuid: "phone".into(), ..Default::default() }
    }

    /// 另一台接收设备（只换 uuid，验证种子把接收端算进去了）。
    fn other() -> Device {
        Device { uuid: "pc".into(), ..to(5856) }
    }

    /// 假接收端：走完握手，回 `FS_AGREE` 和一张偏移量表。
    fn accept(listener: TcpListener, map: &str) -> (Conn, i32, String) {
        let conn = Conn::new(listener.accept().unwrap().0).unwrap();
        assert_eq!(conn.read_i32().unwrap(), MAGIC);
        let flag = conn.read_i32().unwrap();
        let _ = conn.read_str().unwrap();
        assert_eq!(conn.read_i32().unwrap(), FS_SHARE_FILE);
        assert!(!conn.read_bool().unwrap());
        let json = conn.read_str().unwrap();
        conn.write_i32(FS_AGREE).unwrap();
        conn.write_str(map).unwrap();
        (conn, flag, json)
    }

    #[test]
    fn resume_ids_are_stable_and_scoped_to_the_receiver() {
        let dir = temp_dir("ids");
        std::fs::create_dir_all(dir.join("box")).unwrap();
        std::fs::write(dir.join("box/a.txt"), b"abcde").unwrap();
        let paths = [dir.join("box/a.txt"), dir.join("box")];
        let mut once = to_items(&paths);
        let mut twice = to_items(&paths);
        let mut elsewhere = to_items(&paths);
        prepare_ids(&mut once, &from(), &to(5856), true);
        prepare_ids(&mut twice, &from(), &to(5856), true);
        prepare_ids(&mut elsewhere, &from(), &other(), true);
        // 同一发送端、同一接收端、同一内容：ID 必须完全一致，否则 .part 命中不了
        assert_eq!(once[0].file_id, twice[0].file_id);
        assert_eq!(once[1].children[0].file_id, twice[1].children[0].file_id);
        assert_eq!(once[0].file_id.len(), 32);
        assert_ne!(once[1].file_id, elsewhere[1].file_id, "换接收端就是另一条传输");
        // 子文件沿用原版种子形态：senderUuid|绝对路径|大小
        let seed = ResumeStore::child_seed("sender", &dir.join("box/a.txt"), 5);
        assert_eq!(once[1].children[0].file_id, ResumeStore::stable_id(&seed));
        assert_ne!(once[1].file_id, once[1].children[0].file_id, "文件夹不能和子文件撞号");
        assert_ne!(once[0].file_id, once[1].children[0].file_id, "单发的同一文件不能和文件夹里的它撞号");
        prepare_ids(&mut once, &from(), &to(5856), false);
        assert_ne!(once[0].file_id, twice[0].file_id, "不续传时用随机 ID");
        assert!(once[1].children.iter().all(|child| !child.file_id.is_empty()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_peer_offset_table_resumes_from_the_break_point() {
        let engine = engine("resume");
        let dir = temp_dir("resume");
        std::fs::write(dir.join("a.txt"), b"12345").unwrap();
        let mut items = to_items(&[dir.join("a.txt")]);
        prepare_ids(&mut items, &from(), &to(0), true);
        let id = items[0].file_id.clone();
        let meta = TransferMeta::build(&items, &from(), &to(5856), true);
        assert_eq!(meta.resume, Some(1), "V5 必须声明 resume");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let peer = to(listener.local_addr().unwrap().port());
        let worker = Arc::clone(&engine);
        let sender = std::thread::spawn(move || send(&worker, &from(), &peer, &items, &meta));
        // 对方已经收了 3 个字节，那就只该再发 "45"
        let (conn, flag, json) = accept(listener, &format!(r#"{{"{id}":3}}"#));
        assert_eq!(flag, NEW_VERSION_5);
        assert!(json.contains(&format!(r#""fileId":"{id}""#)), "{json}");
        assert_eq!(conn.read_i32().unwrap(), 2);
        let mut body = [0u8; 2];
        conn.read_into(&mut body).unwrap();
        assert_eq!(&body, b"45");
        sender.join().unwrap().expect("发送流程不应报错");
        let events = engine.take_events(Duration::from_millis(50));
        assert!(events.iter().any(|event| matches!(event, NetEvent::Finish { id: done, ok: true, .. } if done == &id)), "{events:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_complete_file_is_skipped_without_sending_bytes() {
        let engine = engine("skip");
        let dir = temp_dir("skip");
        std::fs::write(dir.join("a.txt"), b"12345").unwrap();
        let mut items = to_items(&[dir.join("a.txt")]);
        prepare_ids(&mut items, &from(), &to(0), true);
        let id = items[0].file_id.clone();
        let meta = TransferMeta::build(&items, &from(), &to(5856), true);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let peer = to(listener.local_addr().unwrap().port());
        let worker = Arc::clone(&engine);
        let sender = std::thread::spawn(move || send(&worker, &from(), &peer, &items, &meta));
        let (conn, _, _) = accept(listener, &format!(r#"{{"{id}":5}}"#));
        sender.join().unwrap().expect("发送流程不应报错");
        // 一个字节都没多发：对方一关连接也读不到任何东西
        conn.shutdown();
        let events = engine.take_events(Duration::from_millis(50));
        assert!(events.iter().any(|event| matches!(event, NetEvent::Finish { id: done, ok: true, .. } if done == &id)), "{events:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
