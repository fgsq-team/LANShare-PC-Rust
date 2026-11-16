//! V4 接收：`fileTransferJson` 描述的新式文件流（无续传）。
//!
//! 首帧分发（`handle_version`）与文件接收（`receive`）都走原版的 `CustomDataStream` 语义：
//! 全程裸大端、不混淆。流程是 `readInt(cmd)` → `1101` 时读 `bool encData` + `str json`，
//! 同意后按文件逐个收块，每块形如 `int 长度 + 字节`，长度非正表示本文件结束。

use std::io::Write;
use std::sync::Arc;

use super::crypto::dec_data;
use super::device::Device;
use super::proto::{FILE_TYPE_FOLDER, FS_AGREE, FS_MESSAGE, FS_SHARE_FILE};
use super::stream::Conn;
use super::walk::{TransferMeta, WireFile, avoid_duplication, join_relative};
use super::{Engine, TransferInfo, receive_info};

/// 单个数据块的缓冲区，与原版一致取 1MB。
const CHUNK: usize = 1024 * 1024;

/// V4 连接在握手之后的分发。`resume` 由首帧魔数给出（`-3` 为 V5）。
pub fn handle_version(engine: &Arc<Engine>, conn: Conn, from: Device, resume: bool) -> std::io::Result<()> {
    match conn.read_i32()? {
        FS_MESSAGE => {
            // 新式消息：一个 isClip 标志位 + 密文
            let clipboard = conn.read_bool()?;
            let text = conn.read_str()?;
            engine.on_message(from, &text, clipboard);
            conn.shutdown();
        }
        FS_SHARE_FILE => {
            let enc_data = conn.read_bool()?;
            let json = conn.read_str()?;
            let meta = TransferMeta::parse(&json).ok_or_else(|| std::io::Error::other("fileTransferJson 解析失败"))?;
            engine.offer_receive(conn, from, enc_data, meta, resume);
        }
        // FS_GET_NO_SYNC_MEDIA（媒体同步）属于阶段二
        _ => conn.shutdown(),
    }
    Ok(())
}

/// 用户同意后把这条流上的文件收完。
///
/// 出错时按剩余条目逐个收尾并返回 `Ok(())`：流已经断了，上层再广播一次失败会是重复事件。
pub fn receive(engine: &Engine, conn: Conn, from: Device, enc_data: bool, meta: TransferMeta) -> std::io::Result<()> {
    conn.write_i32(FS_AGREE)?;
    let infos: Vec<TransferInfo> = meta.files.iter().map(|file| receive_info(file, &file.file_id, &from)).collect();
    // 与原版 `onStart` 一致：先把所有顶层气泡建出来，再逐个收
    for info in infos.iter() {
        engine.begin_receive(info);
    }
    let mut index = 0;
    while index < meta.files.len() {
        let file = &meta.files[index];
        let info = &infos[index];
        let result = if file.file_type == FILE_TYPE_FOLDER {
            handle_folder(engine, &conn, file, info, enc_data)
        } else {
            handle_file(engine, &conn, file, info, enc_data)
        };
        match result {
            Ok((ok, path)) => engine.finish(info, ok, &path),
            Err(_) => {
                for rest in &infos[index..] {
                    engine.finish(rest, false, "");
                }
                break;
            }
        }
        index += 1;
    }
    conn.shutdown();
    Ok(())
}

/// 收一个单文件：落在保存根目录下，重名自动加序号。
fn handle_file(engine: &Engine, conn: &Conn, file: &WireFile, info: &TransferInfo, enc_data: bool) -> std::io::Result<(bool, String)> {
    let target = avoid_duplication(&join_relative(&engine.config().save_root(), &file.name));
    if file.length <= 0 {
        std::fs::create_dir_all(parent_of(&target))?;
        std::fs::File::create(&target)?;
        return Ok((true, target.to_string_lossy().into_owned()));
    }
    let mut out = std::fs::File::create(&target)?;
    let total = recv_stream(engine, conn, &mut out, info, enc_data, 0, file.length, file.length)?;
    drop(out);
    let ok = total == file.length;
    Ok((ok, if ok { target.to_string_lossy().into_owned() } else { String::new() }))
}

/// 收一个文件夹：子文件名是相对基准目录的 POSIX 路径，逐个建目录后连续收流。
fn handle_folder(engine: &Engine, conn: &Conn, file: &WireFile, info: &TransferInfo, enc_data: bool) -> std::io::Result<(bool, String)> {
    let folder = avoid_duplication(&join_relative(&engine.config().save_root(), &file.name));
    std::fs::create_dir_all(&folder)?;
    let mut total = 0i64;
    for child in &file.children {
        let path = join_relative(&folder, &child.name);
        std::fs::create_dir_all(parent_of(&path))?;
        if child.length <= 0 {
            std::fs::File::create(&path)?;
            continue;
        }
        let mut out = std::fs::File::create(&path)?;
        let received = recv_stream(engine, conn, &mut out, info, enc_data, total, file.length, child.length)?;
        drop(out);
        if received < 0 {
            total = 0;
            break;
        }
        total += received;
    }
    Ok((total == file.length, folder.to_string_lossy().into_owned()))
}

/// 读「int 长度 + 字节」的数据块直到本文件结束，返回本文件收到的字节数（不足则 -1）。
///
/// - `progress_base`：文件夹场景下已经累加的字节数，用来算整体进度；
/// - `final_size`：进度分母；
/// - 解混淆索引用**本文件内**的绝对偏移，与发送端 `encData(buffer, len, 0, subTotal)` 对齐。
fn recv_stream(
    engine: &Engine,
    conn: &Conn,
    out: &mut std::fs::File,
    info: &TransferInfo,
    enc_data: bool,
    mut progress: i64,
    final_size: i64,
    length: i64,
) -> std::io::Result<i64> {
    let mut buf = vec![0u8; CHUNK];
    let mut sub_total = 0i64;
    let mut target = length;
    let mut last = 0i32;
    let mut stopped = false;
    loop {
        let len = conn.read_i32()?;
        // V4 用非正长度表示本文件发完了（V5 视为异常）
        if len <= 0 {
            break;
        }
        let size = len as usize;
        if size > buf.len() {
            buf.resize(size, 0);
        }
        let read = conn.read_into(&mut buf[..size])?;
        if read == 0 {
            break;
        }
        if enc_data {
            dec_data(&mut buf[..read], sub_total);
        }
        out.write_all(&buf[..read])?;
        sub_total += read as i64;
        progress += read as i64;
        target -= read as i64;
        let percent = if final_size > 0 { (progress * 100 / final_size) as i32 } else { 100 };
        if percent > last {
            last = percent;
            engine.publish_progress(&info.id, percent);
        }
        if engine.cancelled(&info.id) {
            // 原版只在发送端有这个动作，接收端取消时同样把 fileId 写回去让对方停手
            let _ = conn.write_str(&info.id);
            stopped = true;
            break;
        }
        if target <= 0 {
            break;
        }
    }
    // 用户取消即使字节凑齐了也算作废
    if stopped || sub_total != length { Ok(-1) } else { Ok(sub_total) }
}

/// 目录条目本身没有父目录时兜底到当前目录。
fn parent_of(path: &std::path::Path) -> std::path::PathBuf {
    path.parent().map(|parent| parent.to_path_buf()).unwrap_or_else(|| std::path::PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::super::config::Config;
    use super::super::crypto::{enc_data, enc_message};
    use super::super::device::MODE_ANDROID;
    use super::super::proto::NEW_VERSION_4;
    use super::*;
    use crate::lanshare::NetEvent;
    use std::net::TcpListener;
    use std::path::Path;
    use std::time::Duration;

    fn engine(tag: &str) -> Arc<Engine> {
        let mut config = Config::default();
        config.save_path = std::env::temp_dir().join(format!("lanshare-fsrv4-{tag}")).to_string_lossy().into_owned();
        let _ = std::fs::remove_dir_all(&config.save_path);
        config.unique_uuid = "uuid-v4".to_string();
        Arc::new(Engine::for_test(config))
    }

    fn pair() -> (Conn, Conn) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = std::net::TcpStream::connect(address).unwrap();
        let (server, _) = listener.accept().unwrap();
        (Conn::new(client).unwrap(), Conn::new(server).unwrap())
    }

    fn peer() -> Device {
        Device { name: "手机".into(), ip: "127.0.0.1".into(), port: 5856, mode: MODE_ANDROID, uuid: "remote".into(), ..Default::default() }
    }

    fn meta(json: &str) -> TransferMeta {
        TransferMeta::parse(json).expect("测试里的 JSON 必须可解")
    }

    /// 发送一块数据：`int 长度 + 字节`
    fn chunk(conn: &Conn, bytes: &[u8]) {
        conn.write_i32(bytes.len() as i32).unwrap();
        conn.write_raw(bytes).unwrap();
    }

    #[test]
    fn single_file_lands_and_reports_progress() {
        let engine = engine("single");
        let (client, server) = pair();
        let files = meta(r#"{"files":[{"fileId":"f1","name":"笔记.txt","length":5,"fileType":5}]}"#);
        let worker = Arc::clone(&engine);
        std::thread::spawn(move || receive(&worker, server, peer(), false, files));
        assert_eq!(client.read_i32().unwrap(), FS_AGREE);
        chunk(&client, b"123");
        chunk(&client, b"45");
        client.write_i32(0).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let saved = Path::new(&engine.config().save_path).join("笔记.txt");
        assert_eq!(std::fs::read(&saved).unwrap(), b"12345");
        let events = engine.take_events(Duration::from_millis(50));
        assert!(events.iter().any(|event| matches!(event, NetEvent::Start(info) if info.id == "f1")));
        assert!(events.iter().any(|event| matches!(event, NetEvent::Finish { id, ok: true, path } if id == "f1" && path == &saved.to_string_lossy())));
        let _ = std::fs::remove_dir_all(&engine.config().save_path);
    }

    #[test]
    fn existing_name_is_duplicated_with_a_counter() {
        let engine = engine("dup");
        std::fs::create_dir_all(&engine.config().save_path).unwrap();
        std::fs::write(Path::new(&engine.config().save_path).join("a.txt"), b"old").unwrap();
        let (client, server) = pair();
        let files = meta(r#"{"files":[{"fileId":"f2","name":"a.txt","length":3,"fileType":5}]}"#);
        let worker = Arc::clone(&engine);
        std::thread::spawn(move || receive(&worker, server, peer(), false, files));
        assert_eq!(client.read_i32().unwrap(), FS_AGREE);
        chunk(&client, b"new");
        client.write_i32(-2).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let saved = engine.config().save_path;
        let root = Path::new(&saved);
        assert_eq!(std::fs::read(root.join("a.txt")).unwrap(), b"old", "同名文件不能被覆盖");
        assert_eq!(std::fs::read(root.join("a(1).txt")).unwrap(), b"new");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn encrypted_chunks_use_the_offset_inside_each_child() {
        let engine = engine("enc");
        let (client, server) = pair();
        let files = meta(
            r#"{"files":[{"fileId":"d1","name":"照片","length":6,"fileType":6,
                "children":[{"fileId":"c1","name":"a/1.txt","length":3,"fileType":5},{"fileId":"c2","name":"2.txt","length":3,"fileType":5}]}]}"#,
        );
        let worker = Arc::clone(&engine);
        std::thread::spawn(move || receive(&worker, server, peer(), true, files));
        assert_eq!(client.read_i32().unwrap(), FS_AGREE);
        let mut first = b"abc".to_vec();
        // 每个子文件的偏移各自从 0 计
        enc_data(&mut first, 0);
        chunk(&client, &first);
        let mut second = b"def".to_vec();
        enc_data(&mut second, 0);
        chunk(&client, &second);
        client.write_i32(0).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let saved = engine.config().save_path;
        let root = Path::new(&saved);
        assert_eq!(std::fs::read(root.join("照片/a/1.txt")).unwrap(), b"abc");
        assert_eq!(std::fs::read(root.join("照片/2.txt")).unwrap(), b"def");
        let events = engine.take_events(Duration::from_millis(50));
        assert!(events.iter().any(|event| matches!(event, NetEvent::Finish { id, ok: true, .. } if id == "d1")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn zero_length_children_become_empty_files() {
        let engine = engine("empty");
        let (client, server) = pair();
        let files = meta(
            r#"{"files":[{"fileId":"d2","name":"box","length":0,"fileType":6,
                "children":[{"fileId":"c1","name":"占位","length":0,"fileType":5}]}]}"#,
        );
        let worker = Arc::clone(&engine);
        std::thread::spawn(move || receive(&worker, server, peer(), false, files));
        assert_eq!(client.read_i32().unwrap(), FS_AGREE);
        std::thread::sleep(Duration::from_millis(200));
        let placeholder = Path::new(&engine.config().save_path).join("box/占位");
        assert!(placeholder.exists(), "空文件也要落盘");
        assert_eq!(std::fs::metadata(&placeholder).unwrap().len(), 0);
        let _ = std::fs::remove_dir_all(&engine.config().save_path);
    }

    #[test]
    fn short_stream_marks_the_file_failed() {
        let engine = engine("short");
        let (client, server) = pair();
        let files = meta(r#"{"files":[{"fileId":"f3","name":"cut.bin","length":9,"fileType":5}]}"#);
        let worker = Arc::clone(&engine);
        std::thread::spawn(move || receive(&worker, server, peer(), false, files));
        assert_eq!(client.read_i32().unwrap(), FS_AGREE);
        chunk(&client, b"123");
        // 提前结束：字节数对不上就是失败
        client.write_i32(0).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let events = engine.take_events(Duration::from_millis(50));
        assert!(events.iter().any(|event| matches!(event, NetEvent::Finish { id, ok: false, .. } if id == "f3")));
        let _ = std::fs::remove_dir_all(&engine.config().save_path);
    }

    #[test]
    fn user_cancel_writes_the_file_id_back() {
        let engine = engine("cancel");
        let (client, server) = pair();
        let files = meta(r#"{"files":[{"fileId":"f4","name":"big.bin","length":6,"fileType":5}]}"#);
        let worker = Arc::clone(&engine);
        std::thread::spawn(move || receive(&worker, server, peer(), false, files));
        assert_eq!(client.read_i32().unwrap(), FS_AGREE);
        chunk(&client, b"123");
        // 先置取消位，下一块读完就会把 fileId 写回并收尾
        engine.cancel("f4");
        chunk(&client, b"456");
        let id = client.read_str().unwrap();
        assert_eq!(id, "f4");
        std::thread::sleep(Duration::from_millis(100));
        let events = engine.take_events(Duration::from_millis(50));
        assert!(events.iter().any(|event| matches!(event, NetEvent::Finish { id, ok: false, .. } if id == "f4")));
        let _ = std::fs::remove_dir_all(&engine.config().save_path);
    }

    #[test]
    fn dispatch_handles_both_versions_and_the_message_branch() {
        let engine = engine("dispatch");
        let key = engine.config().message_key;
        let (client, server) = pair();
        // 新式消息：cmd + isClip + 密文
        server.write_i32(FS_MESSAGE).unwrap();
        server.write_bool(false).unwrap();
        server.write_str(&enc_message("分发消息", &key)).unwrap();
        handle_version(&engine, client, peer(), false).unwrap();
        let events = engine.take_events(Duration::from_millis(50));
        let text = events.iter().find_map(|event| match event {
            NetEvent::Message { text, clipboard, .. } => Some((text.clone(), *clipboard)),
            _ => None,
        });
        assert_eq!(text, Some(("分发消息".to_string(), false)));
        // 未知命令字直接断开，不产生事件
        let (client, server) = pair();
        server.write_i32(NEW_VERSION_4).unwrap();
        assert!(handle_version(&engine, client, peer(), false).is_ok());
        assert!(engine.take_events(Duration::from_millis(50)).is_empty());
    }
}
