//! 旧版（协议 V1~V3）文件收发的帧格式与接收实现，对应原版 `LegacyFileTransfer`。
//!
//! 这一路全程**不混淆**（原版只在 UDP 分支调 `decAllData()`），一帧 = 12 字节包头 + 包体：
//! - 描述帧：`[cmd:i32][count:i32][len:i32]` + `long 大小 + str 名称 + int 类型 + str 视频时长 [+ int 子文件数]`；
//! - 数据帧：首**字节**是命令字，`1=FS_DATA`、`2=FS_END`、`3=FS_CLOSE`，`len` 仍在偏移 8；
//! - 每收到一个数据帧回 1 字节：`5=FS_NEXT` 继续、`6=FS_BREAK` 取消；每个顶层文件收完回 `2`。

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use super::crypto::dec_data;
use super::device::Device;
use super::proto::{FS_BREAK, FS_CLOSE, FS_DATA, FS_END, FS_NEXT, HEADER_LEN, Packet, Payload};
use super::stream::Conn;
use super::walk::{avoid_duplication, join_relative, size_str};
use super::{Engine, TransferInfo};

/// 单个数据帧上限：原版发送侧用 2MB 缓冲区，收侧要能接住。
const CHUNK: usize = 1024 * 1024 * 2;

/// 一条旧版文件描述。`sub_count > 0` 表示文件夹。
#[derive(Debug, Clone)]
pub struct LegacyFile {
    /// 本机生成的展示键（旧版协议里没有 fileId，原版用 `Utils::getUUID()`）。
    pub id: String,
    pub name: String,
    pub length: i64,
    /// `FILE_IMAGE` / `FILE_VIDEO` / `FILE_FILE` / `FILE_FOLDER`。
    pub kind: i32,
    pub sub_count: i32,
    /// 媒体同步用的相册 ID，-1 表示没有。
    pub media_id: i64,
}

/// 读 `count` 条文件描述帧（`FS_SHARE_FILE` 的首帧之后）。
pub fn read_descriptors(conn: &Conn, count: i32) -> std::io::Result<Vec<LegacyFile>> {
    let mut files = Vec::new();
    for _ in 0..count.max(0) {
        let (_, mut payload) = read_frame(conn)?;
        let length = payload.i64();
        let name = payload.string();
        let kind = payload.i32();
        // 视频时长：旧版写的是字符串，这里读掉保持偏移一致
        let _ = payload.string();
        let mut sub_count = 0;
        let mut media_id = -1;
        if kind == super::proto::FILE_FOLDER {
            sub_count = payload.i32();
        } else if kind == super::proto::FILE_IMAGE || kind == super::proto::FILE_VIDEO {
            media_id = payload.i64_or(-1);
        }
        files.push(LegacyFile { id: super::crypto::new_id(), name, length, kind, sub_count, media_id });
    }
    Ok(files)
}

/// 读一帧裸包：返回 `(cmd, 包头+包体)`，包体外再无内容时 `Payload` 只能读到包头。
fn read_frame(conn: &Conn) -> std::io::Result<(i32, Payload)> {
    let mut header = [0u8; HEADER_LEN];
    conn.read_into(&mut header)?;
    let cmd = i32::from_be_bytes(header[..4].try_into().unwrap_or([0; 4]));
    let len = i32::from_be_bytes(header[8..12].try_into().unwrap_or([0; 4]));
    let mut raw = header.to_vec();
    if len > 0 {
        let mut body = vec![0u8; len as usize];
        conn.read_into(&mut body)?;
        raw.extend_from_slice(&body);
    }
    let (_, payload) = Payload::from_plain(&raw).ok_or_else(|| std::io::Error::other("包头不完整"))?;
    Ok((cmd, payload))
}

/// 同意接收后把文件收完。出错时返回 `Err`，由引擎统一按顶层 id 收尾。
pub fn receive(engine: &Engine, conn: Conn, from: Device, enc_data: bool, files: Vec<LegacyFile>) -> std::io::Result<()> {
    conn.write_raw(&Packet::new(super::proto::FS_AGREE, 0).plain())?;
    // 与原版一致：留一点时间让对方把描述帧发完
    std::thread::sleep(Duration::from_millis(100));
    let root = engine.config().save_root();
    let mut queue = Vec::new();
    for file in &files {
        let info = TransferInfo {
            id: file.id.clone(),
            device: from.clone(),
            name: file.name.clone(),
            length: file.length,
            is_dir: file.sub_count > 0,
            sending: false,
        };
        engine.begin_receive(&info);
        queue.push((info, file.clone()));
    }
    let mut index = 0;
    while index < queue.len() {
        let (info, file) = &queue[index];
        let result = if file.sub_count > 0 {
            receive_folder(engine, &conn, &root, info, file, enc_data)
        } else {
            receive_single(engine, &conn, &root, info, file, enc_data)
        };
        match result {
            Ok((ok, path)) => {
                // 每个顶层文件收完都回一个 0x02，让对方继续下一个
                conn.write_u8(FS_END)?;
                engine.finish(info, ok, &path);
            }
            // 流已经断了：本条与后面的都按失败收尾，对方不会再发数据
            Err(_) => {
                for (info, _) in &queue[index..] {
                    engine.finish(info, false, "");
                }
                break;
            }
        }
        index += 1;
    }
    conn.shutdown();
    Ok(())
}

/// 收一个文件夹：子文件的相对路径由描述帧给出，落盘前逐个查重重命名。
fn receive_folder(engine: &Engine, conn: &Conn, root: &Path, info: &TransferInfo, file: &LegacyFile, enc_data: bool) -> std::io::Result<(bool, String)> {
    let mut total = 0i64;
    for _ in 0..file.sub_count {
        let (_, mut payload) = read_frame(conn)?;
        let child_length = payload.i64();
        // 原版发送的是「去掉父目录前缀」的相对路径，里面已经带了顶层文件夹名
        let relative = payload.string();
        let target = avoid_duplication(&join_relative(root, &relative));
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = std::fs::File::create(&target)?;
        let received = recv_stream(engine, conn, info, &mut out, enc_data, total, file.length, child_length)?;
        if received == ABORTED {
            return Ok((false, String::new()));
        }
        if received > 0 {
            total += received;
        }
    }
    Ok((total == file.length, root.to_string_lossy().into_owned()))
}

/// 收一个单文件。
fn receive_single(engine: &Engine, conn: &Conn, root: &Path, info: &TransferInfo, file: &LegacyFile, enc_data: bool) -> std::io::Result<(bool, String)> {
    std::fs::create_dir_all(root)?;
    let target = avoid_duplication(&root.join(&file.name));
    let mut out = std::fs::File::create(&target)?;
    let received = recv_stream(engine, conn, info, &mut out, enc_data, 0, file.length, file.length)?;
    drop(out);
    let ok = received == file.length;
    // 收失败的文件不留半成品（与原版 `fileIO.deleteFile()` 一致）
    if !ok {
        let _ = std::fs::remove_file(&target);
    }
    Ok((ok, if ok { target.to_string_lossy().into_owned() } else { String::new() }))
}

/// `recv_stream` 的对端明确终止（`FS_CLOSE`）标记。
const ABORTED: i64 = -3;

/// 读数据帧写盘，返回本文件实际收到的字节数。
///
/// - `progress_base`：文件夹场景下已累加的字节数，用于算总进度；
/// - `final_size`：进度分母（单文件是自身长度，文件夹是文件夹总长）；
/// - `length`：本帧声明的文件长度，收满就收尾，因为下一个子文件的描述帧紧接着就来；
/// - 解密偏移用**本文件内**的绝对偏移，与发送端 `encData(..., thatSend)` 对齐。
fn recv_stream(
    engine: &Engine,
    conn: &Conn,
    info: &TransferInfo,
    out: &mut std::fs::File,
    enc_data: bool,
    mut progress: i64,
    final_size: i64,
    length: i64,
) -> std::io::Result<i64> {
    let mut buf = vec![0u8; CHUNK];
    let mut that_total = 0i64;
    let mut last = 0i32;
    loop {
        let mut header = [0u8; HEADER_LEN];
        conn.read_into(&mut header)?;
        let cmd = header[0];
        let len = i32::from_be_bytes(header[8..12].try_into().unwrap_or([0; 4])).max(0) as usize;
        match cmd {
            FS_DATA => {
                if len > buf.len() {
                    buf.resize(len, 0);
                }
                conn.read_into(&mut buf[..len])?;
                if enc_data {
                    dec_data(&mut buf[..len], that_total);
                }
                out.write_all(&buf[..len])?;
                that_total += len as i64;
                progress += len as i64;
                let percent = if final_size > 0 { (progress * 100 / final_size) as i32 } else { 100 };
                if percent > last {
                    last = percent;
                    engine.publish_progress(&info.id, percent);
                }
                // 用户取消时改发 FS_BREAK，让对方停手
                let reply = if engine.cancelled(&info.id) { FS_BREAK } else { FS_NEXT };
                conn.write_u8(reply)?;
                if reply == FS_BREAK {
                    return Ok(that_total);
                }
                // 声明长度收齐了：后面的帧是兄弟文件的描述或 FS_END，不能当数据读
                if length > 0 && that_total >= length {
                    break;
                }
            }
            FS_END => break,
            // FS_CLOSE 或非法命令字：本文件作废
            _ => {
                if cmd == FS_CLOSE {
                    return Ok(ABORTED);
                }
                return Ok(ABORTED);
            }
        }
    }
    Ok(that_total)
}

/// 供 UI 展示的条目大小文本。
pub fn describe(file: &LegacyFile) -> String {
    size_str(file.length)
}

#[cfg(test)]
mod tests {
    use super::super::config::Config;
    use super::super::crypto::enc_data;
    use super::super::proto::{FILE_FILE, FILE_FOLDER, FS_AGREE, FS_MESSAGE};
    use super::*;
    use crate::lanshare::NetEvent;
    use std::net::TcpListener;
    use std::sync::Arc;

    fn engine(tag: &str) -> Arc<Engine> {
        let mut config = Config::default();
        config.save_path = std::env::temp_dir().join(format!("lanshare-legacy-{tag}")).to_string_lossy().into_owned();
        let _ = std::fs::remove_dir_all(&config.save_path);
        config.unique_uuid = "uuid-legacy".to_string();
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
        Device { name: "旧版设备".into(), ip: "127.0.0.1".into(), port: 5856, ..Default::default() }
    }

    /// 一帧「描述」：cmd + count + len + long 大小 + str 名称 + int 类型 + str 视频时长
    fn descriptor(length: i64, name: &str, kind: i32, extra: Option<i32>) -> Vec<u8> {
        let mut packet = Packet::new(FS_MESSAGE, 0);
        packet.i64(length).string(name).i32(kind).string("");
        if let Some(count) = extra {
            packet.i32(count);
        }
        packet.plain()
    }

    /// 一帧数据：首字节命令字 + 偏移 8 的长度 + 裸载荷
    fn frame(cmd: u8, payload: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; HEADER_LEN];
        out[0] = cmd;
        out[8..12].copy_from_slice(&(payload.len() as i32).to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    #[test]
    fn descriptors_are_read_in_wire_order() {
        let (client, server) = pair();
        client.write_raw(&descriptor(3, "a.txt", FILE_FILE, None)).unwrap();
        client.write_raw(&descriptor(0, "box", FILE_FOLDER, Some(2))).unwrap();
        let files = read_descriptors(&server, 2).expect("两帧描述应当可解");
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].name, "a.txt");
        assert_eq!(files[0].length, 3);
        assert_eq!(files[0].sub_count, 0);
        assert_eq!(files[0].media_id, -1);
        assert_eq!(files[1].kind, FILE_FOLDER);
        assert_eq!(files[1].sub_count, 2);
        assert!(!files[0].id.is_empty(), "旧版没有 fileId，本机要补一个");
    }

    #[test]
    fn single_file_lands_with_agree_handshake() {
        let engine = engine("single");
        let (client, server) = pair();
        let files = vec![LegacyFile { id: "x1".into(), name: "笔记.txt".into(), length: 5, kind: FILE_FILE, sub_count: 0, media_id: -1 }];
        let worker = Arc::clone(&engine);
        std::thread::spawn(move || receive(&worker, server, peer(), false, files));
        // 同意帧：cmd=int32(1102)，len=0
        let mut agree = [0u8; HEADER_LEN];
        client.read_into(&mut agree).unwrap();
        assert_eq!(i32::from_be_bytes(agree[..4].try_into().unwrap()), FS_AGREE);
        client.write_raw(&frame(FS_DATA, b"12345")).unwrap();
        assert_eq!(client.read_u8().unwrap(), FS_NEXT);
        client.write_raw(&frame(FS_END, &[])).unwrap();
        assert_eq!(client.read_u8().unwrap(), FS_END, "每个顶层文件收完要回 0x02");
        std::thread::sleep(Duration::from_millis(300));
        let saved = Path::new(&engine.config().save_path).join("笔记.txt");
        assert_eq!(std::fs::read(&saved).unwrap(), b"12345");
        let events = engine.take_events(Duration::from_millis(50));
        assert!(events.iter().any(|event| matches!(event, NetEvent::Finish { ok: true, .. })), "{events:?}");
        let _ = std::fs::remove_dir_all(&engine.config().save_path);
    }

    #[test]
    fn encrypted_chunks_are_decoded_with_the_file_offset() {
        let engine = engine("enc");
        let (client, server) = pair();
        let files = vec![LegacyFile { id: "x2".into(), name: "bin.dat".into(), length: 6, kind: FILE_FILE, sub_count: 0, media_id: -1 }];
        let worker = Arc::clone(&engine);
        std::thread::spawn(move || receive(&worker, server, peer(), true, files));
        let mut agree = [0u8; HEADER_LEN];
        client.read_into(&mut agree).unwrap();
        let mut first = b"123".to_vec();
        // 发送端开的就是 encData，首段也要混淆（索引从 0 起）
        enc_data(&mut first, 0);
        client.write_raw(&frame(FS_DATA, &first)).unwrap();
        client.read_u8().unwrap();
        let mut encrypted = b"456".to_vec();
        // 发送端用「文件内绝对偏移」当索引，第二段必须从 3 开始
        enc_data(&mut encrypted, 3);
        client.write_raw(&frame(FS_DATA, &encrypted)).unwrap();
        client.read_u8().unwrap();
        client.write_raw(&frame(FS_END, &[])).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let saved = Path::new(&engine.config().save_path).join("bin.dat");
        assert_eq!(std::fs::read(&saved).unwrap(), b"123456");
        let _ = std::fs::remove_dir_all(&engine.config().save_path);
    }

    #[test]
    fn folder_children_keep_their_relative_path() {
        let engine = engine("folder");
        let (client, server) = pair();
        let files = vec![LegacyFile {
            id: "x3".into(),
            name: "box".into(),
            length: 5,
            kind: FILE_FOLDER,
            sub_count: 2,
            media_id: -1,
        }];
        let worker = Arc::clone(&engine);
        std::thread::spawn(move || receive(&worker, server, peer(), false, files));
        let mut agree = [0u8; HEADER_LEN];
        client.read_into(&mut agree).unwrap();
        client.write_raw(&descriptor(2, "box/sub/a.txt", FILE_FILE, None)).unwrap();
        client.write_raw(&frame(FS_DATA, b"ab")).unwrap();
        client.read_u8().unwrap();
        client.write_raw(&descriptor(3, "box/b.txt", FILE_FILE, None)).unwrap();
        client.write_raw(&frame(FS_DATA, b"cde")).unwrap();
        client.read_u8().unwrap();
        client.write_raw(&frame(FS_END, &[])).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let saved = engine.config().save_path;
        let root = Path::new(&saved);
        assert_eq!(std::fs::read(root.join("box/sub/a.txt")).unwrap(), b"ab");
        assert_eq!(std::fs::read(root.join("box/b.txt")).unwrap(), b"cde");
        let events = engine.take_events(Duration::from_millis(50));
        assert!(events.iter().any(|event| matches!(event, NetEvent::Finish { ok: true, .. })), "{events:?}");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn user_cancel_makes_the_receiver_send_fs_break() {
        let engine = engine("cancel");
        let (client, server) = pair();
        let files = vec![LegacyFile { id: "x4".into(), name: "big.bin".into(), length: 1024, kind: FILE_FILE, sub_count: 0, media_id: -1 }];
        let worker = Arc::clone(&engine);
        std::thread::spawn(move || receive(&worker, server, peer(), false, files));
        let mut agree = [0u8; HEADER_LEN];
        client.read_into(&mut agree).unwrap();
        // 等 begin_receive 把取消标记登记好再点取消
        std::thread::sleep(Duration::from_millis(300));
        engine.cancel("x4");
        client.write_raw(&frame(FS_DATA, b"part")).unwrap();
        assert_eq!(client.read_u8().unwrap(), FS_BREAK);
        std::thread::sleep(Duration::from_millis(200));
        let events = engine.take_events(Duration::from_millis(50));
        assert!(events.iter().any(|event| matches!(event, NetEvent::Finish { ok: false, .. })), "{events:?}");
        let _ = std::fs::remove_dir_all(&engine.config().save_path);
    }
}
