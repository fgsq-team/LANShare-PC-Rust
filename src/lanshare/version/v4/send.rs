//! V4 发送：新版 TCP 文件流（无续传）。
//!
//! 握手是「魔数（由 `tcp::connect` 写好）→ `int -2` → `str 设备 JSON` → `int 1101` →
//! `bool encData` → `str fileTransferJson`」，然后读一个 `int` 作为答复。同意后按文件逐个
//! 推块：`int 块长 + 字节`，块长 1MB；中途要取消就先写回 `str fileId`，再写 `int -2` 中止整条流。
//! 原版还会另起一个线程读对端发来的 fileId，这里用作用域线程复刻；那个线程按读超时轮询，
//! 收尾时置完停止标记再关连接，避免 Windows 上阻塞的 recv 唤不醒。

use std::collections::BTreeMap;
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::crypto::enc_data;
use super::device::Device;
use super::proto::{NEW_FS_BREAK, NEW_VERSION_4, FS_AGREE, FS_SHARE_FILE};
use super::stream::Conn;
use super::walk::{Item, TransferMeta};
use super::{Engine, tcp};

/// 单块字节数，与原版一致取 1MB。
const CHUNK: usize = 1024 * 1024;
/// 等待对方同意/拒绝的上限：接收确认对话框需要人手点，不能太短。
const RESPONSE: Duration = Duration::from_secs(120);
/// 取消监听的轮询周期：Windows 上 `shutdown` 唤不醒阻塞中的 `recv`，只能靠读超时轮询。
const POLL: Duration = Duration::from_millis(300);

/// 把 `items` 发给 `to`（V4，无续传）。
pub fn send(engine: &Engine, from: &Device, to: &Device, items: &[Item], meta: &TransferMeta) -> std::io::Result<()> {
    let conn = tcp::connect(to)?;
    let encrypted = engine.config().enc_data;
    handshake(&conn, from, NEW_VERSION_4, meta, encrypted)?;
    transmit(engine, &conn, items, encrypted, None)
}

/// 写好文件流头部并等对方同意。
pub(crate) fn handshake(conn: &Conn, from: &Device, flag: i32, meta: &TransferMeta, encrypted: bool) -> std::io::Result<()> {
    conn.write_i32(flag)?;
    conn.write_str(&from.handshake_json())?;
    conn.write_i32(FS_SHARE_FILE)?;
    conn.write_bool(encrypted)?;
    conn.write_str(&meta.to_json())?;
    conn.set_read_timeout(Some(RESPONSE))?;
    let response = conn.read_i32()?;
    conn.set_read_timeout(None)?;
    if response != FS_AGREE {
        return Err(std::io::Error::other("对方拒绝接收文件"));
    }
    Ok(())
}

/// 推完全部条目并关掉连接。`resume` 是对方回传的偏移量表，`None` 表示不支持续传。
pub(crate) fn transmit(
    engine: &Engine,
    conn: &Conn,
    items: &[Item],
    encrypted: bool,
    resume: Option<&BTreeMap<String, i64>>,
) -> std::io::Result<()> {
    // 监听线程改成读超时轮询：收尾时先置 done 再关连接，作用域线程才能在 300ms 内退出
    let _ = conn.set_read_timeout(Some(POLL));
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| listen_cancel(engine, conn, &done));
        let result = send_items(engine, conn, items, encrypted, resume);
        done.store(true, Ordering::SeqCst);
        // 关掉连接，让对方立刻看到流已断
        conn.shutdown();
        result
    })
}

/// 读对端写回的 fileId 并置取消位（原版在 `handleFileTransfer` 里起的监听线程）。
fn listen_cancel(engine: &Engine, conn: &Conn, done: &AtomicBool) {
    while !done.load(Ordering::Relaxed) {
        match conn.read_str() {
            Ok(id) if !id.is_empty() => engine.cancel(&id),
            // 空串：对端不再关心这条流
            Ok(_) => break,
            // 读超时只说明这会儿没有取消指令，接着等
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            // 连接断了或锁被污染，没什么可监听的了
            Err(_) => break,
        }
    }
}

/// 逐条发送；某一条写不下去时把剩下的全部作废，避免上层再广播一次重复失败。
fn send_items(engine: &Engine, conn: &Conn, items: &[Item], encrypted: bool, resume: Option<&BTreeMap<String, i64>>) -> std::io::Result<()> {
    let mut index = 0;
    while index < items.len() {
        let item = &items[index];
        let result = if item.is_dir {
            send_folder(engine, conn, item, encrypted, resume)
        } else {
            send_file(engine, conn, item, encrypted, resume)
        };
        match result {
            Ok((ok, path)) => engine.publish_finish(&item.file_id, ok, &path, ""),
            Err(_) => {
                for rest in &items[index..] {
                    engine.publish_finish(&rest.file_id, false, "", "");
                }
                break;
            }
        }
        index += 1;
    }
    Ok(())
}

/// 发一个单文件。续传时进度基准就是续传偏移。
fn send_file(engine: &Engine, conn: &Conn, item: &Item, encrypted: bool, resume: Option<&BTreeMap<String, i64>>) -> std::io::Result<(bool, String)> {
    let length = item.length;
    let mut total = 0;
    if length > 0 {
        let offset = offset_of(resume, &item.file_id, length);
        total = if offset >= length {
            length
        } else {
            send_stream(engine, conn, &item.file_id, item, encrypted, offset, length, length, offset)?
        };
    }
    Ok((total == length, item.path.to_string_lossy().into_owned()))
}

/// 发一个文件夹：子文件的字节首尾相接连续发送，已完成的直接跳过。
fn send_folder(engine: &Engine, conn: &Conn, item: &Item, encrypted: bool, resume: Option<&BTreeMap<String, i64>>) -> std::io::Result<(bool, String)> {
    let mut sub_total = 0i64;
    for child in &item.children {
        if child.length <= 0 {
            continue;
        }
        let offset = offset_of(resume, &child.file_id, child.length);
        if offset >= child.length {
            sub_total += child.length;
            continue;
        }
        let sent = send_stream(engine, conn, &item.file_id, child, encrypted, sub_total + offset, item.length, child.length, offset)?;
        if sent < 0 {
            break;
        }
        sub_total += sent;
    }
    Ok((sub_total == item.length, item.path.to_string_lossy().into_owned()))
}

/// 推一个文件（或子文件）的数据块，返回该文件的绝对完成字节数（异常返回 -1）。
///
/// 混淆索引与接收端 `decData` 对齐，用的是**该文件内的绝对偏移**（含续传起点）。
#[allow(clippy::too_many_arguments)]
fn send_stream(
    engine: &Engine,
    conn: &Conn,
    top_id: &str,
    file: &Item,
    encrypted: bool,
    progress_base: i64,
    final_size: i64,
    length: i64,
    start_offset: i64,
) -> std::io::Result<i64> {
    let mut input = std::fs::File::open(&file.path)?;
    if start_offset > 0 {
        input.seek(SeekFrom::Start(start_offset as u64))?;
    }
    let mut buf = vec![0u8; CHUNK];
    let mut file_index = start_offset;
    let mut sent = 0i64;
    let mut target = length - start_offset;
    let mut last = 0i32;
    loop {
        if engine.cancelled(top_id) {
            // 先写 fileId 让对方作废这个文件，再用 -2 中止整条流
            let _ = conn.write_str(top_id);
            let _ = conn.write_i32(NEW_FS_BREAK);
            return Ok(-1);
        }
        let read = fill(&mut input, &mut buf)?;
        if read == 0 {
            break;
        }
        if encrypted {
            enc_data(&mut buf[..read], file_index);
        }
        conn.write_i32(read as i32)?;
        conn.write_raw(&buf[..read])?;
        file_index += read as i64;
        sent += read as i64;
        target -= read as i64;
        let percent = if final_size > 0 { (progress_base + sent) * 100 / final_size } else { 100 };
        let percent = percent.clamp(0, i64::from(i32::MAX)) as i32;
        if percent > last {
            last = percent;
            engine.publish_progress(top_id, percent);
        }
        if target <= 0 {
            break;
        }
    }
    if file_index != length { Ok(-1) } else { Ok(file_index) }
}

/// 续传起点：偏移表里没有就是 0，越界的一律裁到 0。
fn offset_of(resume: Option<&BTreeMap<String, i64>>, file_id: &str, length: i64) -> i64 {
    let offset = resume.and_then(|map| map.get(file_id)).copied().unwrap_or(0);
    if offset < 0 || offset > length { 0 } else { offset }
}

/// 读满一个缓冲区（`File::read` 允许短读，这里循环补到 EOF 或满）。
fn fill(input: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        match input.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(read) => total += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::super::config::Config;
    use super::super::proto::{FS_NOT_AGREE, MAGIC};
    use super::super::walk::to_items;
    use super::*;
    use crate::lanshare::NetEvent;
    use std::net::{TcpListener, TcpStream};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn engine(tag: &str) -> Arc<Engine> {
        let mut config = Config::default();
        config.save_path = std::env::temp_dir().join(format!("lanshare-fsend4-{tag}")).to_string_lossy().into_owned();
        config.unique_uuid = "uuid-send".to_string();
        Arc::new(Engine::for_test(config))
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lanshare-fsend4-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn pair() -> (Conn, Conn) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = TcpStream::connect(address).unwrap();
        let (server, _) = listener.accept().unwrap();
        (Conn::new(client).unwrap(), Conn::new(server).unwrap())
    }

    fn me() -> Device {
        Device { name: "本机".into(), ip: "127.0.0.1".into(), port: 5856, uuid: "me".into(), ..Default::default() }
    }

    fn peer(port: u16) -> Device {
        Device { name: "手机".into(), ip: "127.0.0.1".into(), port: port as i32, ..Default::default() }
    }

    /// 假接收端：读完握手并按 `agree` 答复，返回（连接、设备 JSON、fileTransferJson）。
    fn accept_handshake(listener: TcpListener, agree: bool) -> (Conn, String, String) {
        let conn = Conn::new(listener.accept().unwrap().0).unwrap();
        assert_eq!(conn.read_i32().unwrap(), MAGIC, "首 4 字节必须是魔数");
        assert_eq!(conn.read_i32().unwrap(), NEW_VERSION_4);
        let device = conn.read_str().unwrap();
        assert_eq!(conn.read_i32().unwrap(), FS_SHARE_FILE);
        assert!(!conn.read_bool().unwrap());
        let json = conn.read_str().unwrap();
        conn.write_i32(if agree { FS_AGREE } else { FS_NOT_AGREE }).unwrap();
        (conn, device, json)
    }

    #[test]
    fn handshake_and_chunks_match_the_wire_format() {
        let engine = engine("wire");
        let dir = temp_dir("wire");
        std::fs::write(dir.join("a.txt"), b"12345").unwrap();
        let mut items = to_items(&[dir.join("a.txt")]);
        items[0].file_id = "top1".into();
        let meta = TransferMeta::build(&items, &me(), &peer(5856), false);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let to = peer(listener.local_addr().unwrap().port());
        let worker = Arc::clone(&engine);
        let sender = std::thread::spawn(move || send(&worker, &me(), &to, &items, &meta));
        let (conn, device, json) = accept_handshake(listener, true);
        assert!(device.contains("\"devName\":\"本机\""), "{device}");
        assert!(json.contains("\"fileId\":\"top1\""), "{json}");
        assert_eq!(conn.read_i32().unwrap(), 5);
        let mut body = vec![0u8; 5];
        conn.read_into(&mut body).unwrap();
        assert_eq!(body, b"12345");
        sender.join().unwrap().expect("发送流程不应报错");
        let events = engine.take_events(Duration::from_millis(50));
        assert!(events.iter().any(|event| matches!(event, NetEvent::Finish { id, ok: true, .. } if id == "top1")), "{events:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refusal_is_reported_as_an_error() {
        let engine = engine("refuse");
        let dir = temp_dir("refuse");
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        let items = to_items(&[dir.join("a.txt")]);
        let meta = TransferMeta::build(&items, &me(), &peer(5856), false);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let to = peer(listener.local_addr().unwrap().port());
        let worker = Arc::clone(&engine);
        let sender = std::thread::spawn(move || send(&worker, &me(), &to, &items, &meta));
        let (_, _, _) = accept_handshake(listener, false);
        let error = sender.join().unwrap().expect_err("拒绝必须报错");
        assert!(error.to_string().contains("拒绝"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cancelled_item_writes_the_file_id_and_breaks_the_stream() {
        let engine = engine("cancel");
        let dir = temp_dir("cancel");
        std::fs::write(dir.join("a.txt"), b"12345").unwrap();
        let mut items = to_items(&[dir.join("a.txt")]);
        items[0].file_id = "top2".into();
        engine.register_cancel("top2");
        engine.cancel("top2");
        let (client, server) = pair();
        // 跳过握手直接推条目：取消位已置位，只会写下 fileId 与 -2
        send_items(&engine, &server, &items, false, None).expect("取消本身不算连接错误");
        assert_eq!(client.read_str().unwrap(), "top2");
        assert_eq!(client.read_i32().unwrap(), NEW_FS_BREAK);
        let events = engine.take_events(Duration::from_millis(50));
        assert!(events.iter().any(|event| matches!(event, NetEvent::Finish { id, ok: false, .. } if id == "top2")), "{events:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_id_from_the_peer_sets_the_cancel_flag() {
        let engine = engine("listen");
        let (client, server) = pair();
        engine.register_cancel("top5");
        client.write_str("top5").unwrap();
        // 空串让监听线程正常退出
        client.write_str("").unwrap();
        let done = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| listen_cancel(&engine, &server, &done));
        });
        assert!(engine.cancelled("top5"));
    }

    #[test]
    fn folder_children_are_streamed_back_to_back() {
        let engine = engine("folder");
        let dir = temp_dir("folder");
        std::fs::create_dir_all(dir.join("box/sub")).unwrap();
        std::fs::write(dir.join("box/sub/a.txt"), b"ab").unwrap();
        std::fs::write(dir.join("box/b.txt"), b"cde").unwrap();
        let mut items = to_items(&[dir.join("box")]);
        items[0].file_id = "top3".into();
        let meta = TransferMeta::build(&items, &me(), &peer(5856), false);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let to = peer(listener.local_addr().unwrap().port());
        let worker = Arc::clone(&engine);
        let sender = std::thread::spawn(move || send(&worker, &me(), &to, &items, &meta));
        let (conn, _, json) = accept_handshake(listener, true);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["files"][0]["fileType"].as_i64(), Some(6));
        assert_eq!(value["files"][0]["length"].as_i64(), Some(5));
        // 子文件按扫描顺序连续发：先 sub/a.txt 再 b.txt
        let mut got = Vec::new();
        for _ in 0..2 {
            let len = conn.read_i32().unwrap();
            let mut body = vec![0u8; len as usize];
            conn.read_into(&mut body).unwrap();
            got.extend_from_slice(&body);
        }
        assert_eq!(got, b"abcde".to_vec());
        sender.join().unwrap().expect("发送流程不应报错");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_offset_skips_completed_bytes() {
        let mut map = BTreeMap::new();
        map.insert("f".to_string(), 3i64);
        map.insert("over".to_string(), 99i64);
        assert_eq!(offset_of(Some(&map), "f", 10), 3);
        // 越界与未知都要退回 0，否则 seek 会跑出文件外
        assert_eq!(offset_of(Some(&map), "over", 10), 0);
        assert_eq!(offset_of(Some(&map), "unknown", 10), 0);
        assert_eq!(offset_of(None, "f", 10), 0);
    }
}
