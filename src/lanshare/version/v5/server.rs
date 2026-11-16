//! V5 接收：带断点续传的新式文件流。
//!
//! 与 V4 的唯一协议差异是同意之后接收端要**立刻回一张偏移量表**（`{fileId: 已完成字节}`），
//! 发送端据此跳过已完成的子文件、并从断点继续。落盘细节：
//! - 单文件先写 `<fileId>.part`，长度对上再改名成正式文件；
//! - 文件夹的子文件写 `<folderFileId>/<childFileId>.part`，全部齐了再搬进最终目录；
//! - 最终目录名记在同级 `.target` 里，进程被杀后重发才知道落到哪；
//! - 数据块解混淆的索引是**文件内绝对偏移**（含续传起点），与发送端 `encData(..., fileIndex)` 对齐。

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::crypto::dec_data;
use super::device::Device;
use super::proto::{FILE_TYPE_FOLDER, FS_AGREE};
use super::resume::ResumeStore;
use super::stream::Conn;
use super::walk::{TransferMeta, WireFile, avoid_duplication, join_relative};
use super::{Engine, TransferInfo, receive_info};

/// 单个数据块的缓冲区，与原版一致取 1MB。
const CHUNK: usize = 1024 * 1024;

/// V5 连接在握手之后的分发：首帧结构与 V4 完全相同，直接复用，
/// 但分流标志写死为续传：Java 手机发的 V5 JSON 里没有 `resume` 字段，
/// 能不能收偏移量表只能由首帧魔数（`-3`）决定。
pub fn handle_version(engine: &Arc<Engine>, conn: Conn, from: Device) -> std::io::Result<()> {
    super::fsrv4::handle_version(engine, conn, from, true)
}

/// 用户同意后把这条流上的文件收完（带续传）。
pub fn receive(engine: &Engine, conn: Conn, from: Device, enc_data: bool, meta: TransferMeta) -> std::io::Result<()> {
    conn.write_i32(FS_AGREE)?;
    let store = ResumeStore::new(&engine.config().save_path);
    // 偏移量表必须在任何数据之前回传，否则发送端已经从头开始写了
    let map = build_resume_map(&store, &meta.files);
    conn.write_str(&ResumeStore::resume_map_json(&map))?;
    let infos: Vec<TransferInfo> = meta.files.iter().map(|file| receive_info(file, &file.file_id, &from)).collect();
    for info in infos.iter() {
        engine.begin_receive(info);
    }
    let mut index = 0;
    while index < meta.files.len() {
        let file = &meta.files[index];
        let info = &infos[index];
        let result = if file.file_type == FILE_TYPE_FOLDER {
            handle_folder(engine, &store, &conn, file, info, enc_data)
        } else {
            handle_file(engine, &store, &conn, file, info, enc_data)
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

/// 续传偏移量表：只登记还没收完的分段，键取发送端给的 `fileId`。
pub(crate) fn build_resume_map(store: &ResumeStore, files: &[WireFile]) -> BTreeMap<String, i64> {
    let mut map = BTreeMap::new();
    for file in files {
        if file.file_id.is_empty() {
            continue;
        }
        if file.file_type == FILE_TYPE_FOLDER {
            // 暂存目录不存在就说明一条分段都没有，不必再翻子文件
            let stage = store.root().join(&file.file_id);
            if !stage.is_dir() {
                continue;
            }
            let final_dir = store.read_target(&stage).unwrap_or_default();
            for child in &file.children {
                if child.length <= 0 || child.file_id.is_empty() {
                    continue;
                }
                let part = store.child_part(&file.file_id, &child.file_id);
                let done = if final_dir.is_empty() {
                    None
                } else {
                    Some(join_relative(&PathBuf::from(&final_dir), &child.name))
                };
                let offset = match done.and_then(|path| std::fs::metadata(path).ok()) {
                    Some(meta) if meta.len() as i64 == child.length => child.length,
                    _ => store.read_offset(&part, child.length),
                };
                if offset > 0 {
                    map.insert(child.file_id.clone(), offset);
                }
            }
        } else {
            if file.length <= 0 {
                continue;
            }
            let offset = store.read_offset(&store.file_part(&file.file_id), file.length);
            if offset > 0 {
                map.insert(file.file_id.clone(), offset);
            }
        }
    }
    map
}

/// 收一个单文件：有 `fileId` 才走分段，否则直接落盘（等价原版 `receiveDirect`）。
fn handle_file(engine: &Engine, store: &ResumeStore, conn: &Conn, file: &WireFile, info: &TransferInfo, enc_data: bool) -> std::io::Result<(bool, String)> {
    let base = base_target(engine, &file.name);
    if file.length <= 0 {
        let target = landing(&base)?;
        std::fs::File::create(&target)?;
        return Ok((true, target.to_string_lossy().into_owned()));
    }
    let part = store.file_part(&file.file_id);
    let offset = store.read_offset(&part, file.length);
    let received = if offset >= file.length {
        file.length
    } else {
        let mut out = open_part(&part, offset)?;
        let received = recv_stream(engine, conn, &mut out, info, enc_data, offset, file.length, file.length, offset)?;
        drop(out);
        received
    };
    if received >= file.length {
        let final_file = store.finalize_part(&part, &base);
        Ok((true, final_file.to_string_lossy().into_owned()))
    } else {
        // 没收完：分段留着，下次重发接着用
        Ok((false, String::new()))
    }
}

/// 收一个文件夹：先把最终目录名记进 `.target`，子文件逐个走分段。
fn handle_folder(engine: &Engine, store: &ResumeStore, conn: &Conn, file: &WireFile, info: &TransferInfo, enc_data: bool) -> std::io::Result<(bool, String)> {
    let stage = store.folder_stage(&file.file_id);
    let final_dir = match store.read_target(&stage) {
        Some(name) if !name.is_empty() => PathBuf::from(name),
        _ => {
            let dir = avoid_duplication(&base_target(engine, &file.name));
            std::fs::create_dir_all(&dir)?;
            store.write_target(&stage, &dir.to_string_lossy())?;
            dir
        }
    };
    let mut total = 0i64;
    let mut failed = false;
    for child in &file.children {
        let done = join_relative(&final_dir, &child.name);
        if let Some(parent) = done.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if child.length <= 0 {
            std::fs::File::create(&done)?;
            continue;
        }
        // 子文件没带 fileId 就没法续传：直接往最终位置写
        let resumable = !child.file_id.is_empty();
        let part = store.child_part(&file.file_id, &child.file_id);
        let offset = if !resumable {
            0
        } else if std::fs::metadata(&done).ok().is_some_and(|meta| meta.len() as i64 == child.length) {
            child.length
        } else {
            store.read_offset(&part, child.length)
        };
        let received = if offset >= child.length {
            child.length
        } else {
            let target = if resumable { part.clone() } else { done.clone() };
            let mut out = open_part(&target, offset)?;
            let received = recv_stream(engine, conn, &mut out, info, enc_data, total + offset, file.length, child.length, offset)?;
            drop(out);
            received
        };
        if received < 0 {
            failed = true;
            break;
        }
        if received >= child.length && resumable {
            store.move_part_to_final(&part, &final_dir, &child.name);
        }
        total += received;
    }
    let success = !failed && total == file.length;
    if success {
        store.delete_dir(&stage);
    }
    Ok((success, final_dir.to_string_lossy().into_owned()))
}

/// 读「int 长度 + 字节」的数据块，返回本文件的绝对完成字节数（异常返回 -1）。
///
/// V5 与 V4 的差别：长度非正一律当异常（原版 `qDebug("cmd error")` 后 `return -1`），
/// 因为续传流的每个文件长度都是已知的，提前结束说明对方没发完。
fn recv_stream(
    engine: &Engine,
    conn: &Conn,
    out: &mut std::fs::File,
    info: &TransferInfo,
    enc_data: bool,
    progress_base: i64,
    final_size: i64,
    length: i64,
    start_offset: i64,
) -> std::io::Result<i64> {
    let mut buf = vec![0u8; CHUNK];
    let mut file_index = start_offset;
    let mut sent = 0i64;
    let mut target = length - start_offset;
    let mut last = 0i32;
    loop {
        let len = conn.read_i32()?;
        if len <= 0 {
            return Ok(-1);
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
            // 索引是文件内绝对偏移，续传起点也要算进去
            dec_data(&mut buf[..read], file_index);
        }
        out.write_all(&buf[..read])?;
        file_index += read as i64;
        sent += read as i64;
        target -= read as i64;
        let percent = if final_size > 0 { ((progress_base + sent) * 100 / final_size) as i32 } else { 100 };
        if percent != last {
            last = percent;
            engine.publish_progress(&info.id, percent);
        }
        if engine.cancelled(&info.id) {
            let _ = conn.write_str(&info.id);
            return Ok(-1);
        }
        if target <= 0 {
            break;
        }
    }
    if file_index != length { Ok(-1) } else { Ok(file_index) }
}

/// 保存目录下的落点，路径片段经过清洗。
fn base_target(engine: &Engine, name: &str) -> PathBuf {
    join_relative(&engine.config().save_root(), name)
}

/// 落盘前先重名查重（原版 `landingPathOf`）。
fn landing(base: &Path) -> std::io::Result<PathBuf> {
    if let Some(parent) = base.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(avoid_duplication(base))
}

/// 打开分段：有断点就追加，否则新建。
fn open_part(part: &Path, offset: i64) -> std::io::Result<std::fs::File> {
    if let Some(parent) = part.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if offset > 0 {
        std::fs::OpenOptions::new().append(true).open(part)
    } else {
        std::fs::File::create(part)
    }
}

#[cfg(test)]
mod tests {
    use super::super::config::Config;
    use super::super::crypto::enc_data;
    use super::super::device::MODE_ANDROID;
    use super::super::proto::FS_SHARE_FILE;
    use super::*;
    use crate::lanshare::NetEvent;
    use std::net::TcpListener;
    use std::time::Duration;

    fn engine(tag: &str) -> Arc<Engine> {
        let mut config = Config::default();
        config.save_path = std::env::temp_dir().join(format!("lanshare-fsrv5-{tag}")).to_string_lossy().into_owned();
        let _ = std::fs::remove_dir_all(&config.save_path);
        config.unique_uuid = "uuid-v5".to_string();
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

    fn chunk(conn: &Conn, bytes: &[u8]) {
        conn.write_i32(bytes.len() as i32).unwrap();
        conn.write_raw(bytes).unwrap();
    }

    fn spawn(engine: &Arc<Engine>, server: Conn, files: TransferMeta, enc_data: bool) {
        let worker = Arc::clone(engine);
        std::thread::spawn(move || receive(&worker, server, peer(), enc_data, files));
    }

    #[test]
    fn agree_is_followed_by_an_empty_resume_map() {
        let engine = engine("map");
        let (client, server) = pair();
        spawn(&engine, server, meta(r#"{"files":[{"fileId":"f1","name":"a.txt","length":5,"fileType":5}]}"#), false);
        assert_eq!(client.read_i32().unwrap(), FS_AGREE);
        assert_eq!(client.read_str().unwrap(), "{}", "没有分段时偏移量表是空对象");
        chunk(&client, b"12345");
        std::thread::sleep(Duration::from_millis(200));
        let saved = Path::new(&engine.config().save_path).join("a.txt");
        assert_eq!(std::fs::read(&saved).unwrap(), b"12345");
        assert!(!store(&engine).file_part("f1").exists(), "完成后分段要被搬走");
        let _ = std::fs::remove_dir_all(&engine.config().save_path);
    }

    /// 回归：Java 手机的 V5 发送链不在 JSON 里写 `resume`，分流只能靠首帧魔数；
    /// 走错 V4 分支就会把偏移量表的前 4 字节当成首个数据块长度，接收直接乱码。
    #[test]
    fn magic_routed_v5_reads_the_resume_map_without_json_resume() {
        let engine = engine("magic");
        let (client, server) = pair();
        let worker = Arc::clone(&engine);
        std::thread::spawn(move || handle_version(&worker, server, peer()));
        // 对端只写了 NEW_VERSION_5 魔数，JSON 里没有 resume 字段
        client.write_i32(FS_SHARE_FILE).unwrap();
        client.write_bool(false).unwrap();
        client.write_str(r#"{"files":[{"fileId":"m1","name":"pic.png","length":5,"fileType":5}]}"#).unwrap();
        assert_eq!(client.read_i32().unwrap(), FS_AGREE);
        // V5 接收端同意后必回一张偏移量表（哪怕是空的）
        assert_eq!(client.read_str().unwrap(), "{}");
        chunk(&client, b"12345");
        assert!(wait_finish_on(&engine, "m1", true), "必须走 V5 链路收完而不是把量表当数据块");
        let saved = Path::new(&engine.config().save_path).join("pic.png");
        assert_eq!(std::fs::read(&saved).unwrap(), b"12345");
        let _ = std::fs::remove_dir_all(&engine.config().save_path);
    }

    /// 攒事件直到出现指定 id 的收尾（接收跑在工作线程里，一次 `take_events` 收不齐）。
    fn wait_finish_on(engine: &Engine, id: &str, ok: bool) -> bool {
        for _ in 0..100 {
            for event in engine.take_events(Duration::from_millis(50)) {
                if let NetEvent::Finish { id: done, ok: flag, .. } = &event {
                    if done == id && *flag == ok {
                        return true;
                    }
                }
            }
        }
        false
    }

    #[test]
    fn existing_part_is_reported_and_appended_to() {
        let engine = engine("resume");
        let store = store(&engine);
        std::fs::write(store.file_part("f2"), b"123").unwrap();
        let (client, server) = pair();
        spawn(&engine, server, meta(r#"{"resume":1,"files":[{"fileId":"f2","name":"b.txt","length":5,"fileType":5}]}"#), true);
        assert_eq!(client.read_i32().unwrap(), FS_AGREE);
        let map = ResumeStore::parse_resume_map(&client.read_str().unwrap());
        assert_eq!(map.get("f2"), Some(&3), "要把已完成的 3 字节告诉发送端");
        let mut tail = b"45".to_vec();
        // 发送端从偏移 3 继续加密
        enc_data(&mut tail, 3);
        chunk(&client, &tail);
        std::thread::sleep(Duration::from_millis(200));
        let saved = Path::new(&engine.config().save_path).join("b.txt");
        assert_eq!(std::fs::read(&saved).unwrap(), b"12345");
        let events = engine.take_events(Duration::from_millis(50));
        assert!(events.iter().any(|event| matches!(event, NetEvent::Finish { id, ok: true, .. } if id == "f2")));
        let _ = std::fs::remove_dir_all(&engine.config().save_path);
    }

    #[test]
    fn complete_part_finalizes_without_reading_data() {
        let engine = engine("done");
        let store = store(&engine);
        std::fs::write(store.file_part("f3"), b"12345").unwrap();
        let (client, server) = pair();
        spawn(&engine, server, meta(r#"{"resume":1,"files":[{"fileId":"f3","name":"c.txt","length":5,"fileType":5}]}"#), false);
        assert_eq!(client.read_i32().unwrap(), FS_AGREE);
        assert_eq!(client.read_str().unwrap(), r#"{"f3":5}"#);
        std::thread::sleep(Duration::from_millis(200));
        // 对方不再发数据，我们也要把分段改成正式文件
        assert_eq!(std::fs::read(Path::new(&engine.config().save_path).join("c.txt")).unwrap(), b"12345");
        let _ = std::fs::remove_dir_all(&engine.config().save_path);
    }

    #[test]
    fn folder_children_are_staged_then_moved_into_place() {
        let engine = engine("folder");
        let store = store(&engine);
        store.folder_stage("d1");
        std::fs::write(store.child_part("d1", "c1"), b"ab").unwrap();
        let (client, server) = pair();
        let files = meta(
            r#"{"resume":1,"files":[{"fileId":"d1","name":"相册","length":5,"fileType":6,
                "children":[{"fileId":"c1","name":"sub/a.txt","length":2,"fileType":5},{"fileId":"c2","name":"b.txt","length":3,"fileType":5}]}]}"#,
        );
        spawn(&engine, server, files, false);
        assert_eq!(client.read_i32().unwrap(), FS_AGREE);
        let map = ResumeStore::parse_resume_map(&client.read_str().unwrap());
        assert_eq!(map.get("c1"), Some(&2));
        assert_eq!(map.get("c2"), None);
        chunk(&client, b"cde");
        std::thread::sleep(Duration::from_millis(250));
        let saved = engine.config().save_path;
        let root = Path::new(&saved);
        assert_eq!(std::fs::read(root.join("相册/sub/a.txt")).unwrap(), b"ab");
        assert_eq!(std::fs::read(root.join("相册/b.txt")).unwrap(), b"cde");
        assert!(!store.root().join("d1").exists(), "整个文件夹收完要删掉暂存目录");
        let events = engine.take_events(Duration::from_millis(50));
        assert!(events.iter().any(|event| matches!(event, NetEvent::Finish { id, ok: true, .. } if id == "d1")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn broken_stream_keeps_the_part_for_the_next_try() {
        let engine = engine("broken");
        let (client, server) = pair();
        spawn(&engine, server, meta(r#"{"resume":1,"files":[{"fileId":"f4","name":"d.bin","length":9,"fileType":5}]}"#), false);
        assert_eq!(client.read_i32().unwrap(), FS_AGREE);
        assert_eq!(client.read_str().unwrap(), "{}");
        chunk(&client, b"123");
        // 长度非正：V5 视为异常，本文件作废
        client.write_i32(-2).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        let store = store(&engine);
        assert_eq!(std::fs::read(store.file_part("f4")).unwrap(), b"123", "分段必须留着等重发");
        assert!(!Path::new(&engine.config().save_path).join("d.bin").exists());
        let events = engine.take_events(Duration::from_millis(50));
        assert!(events.iter().any(|event| matches!(event, NetEvent::Finish { id, ok: false, .. } if id == "f4")));
        let _ = std::fs::remove_dir_all(&engine.config().save_path);
    }

    #[test]
    fn child_without_file_id_lands_without_a_stage() {
        let engine = engine("noid");
        let (client, server) = pair();
        let files = meta(
            r#"{"resume":1,"files":[{"fileId":"d2","name":"box","length":3,"fileType":6,
                "children":[{"name":"x.txt","length":3,"fileType":5}]}]}"#,
        );
        spawn(&engine, server, files, false);
        assert_eq!(client.read_i32().unwrap(), FS_AGREE);
        assert_eq!(client.read_str().unwrap(), "{}");
        chunk(&client, b"xyz");
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(std::fs::read(Path::new(&engine.config().save_path).join("box/x.txt")).unwrap(), b"xyz");
        let _ = std::fs::remove_dir_all(&engine.config().save_path);
    }

    fn store(engine: &Arc<Engine>) -> ResumeStore {
        ResumeStore::new(&engine.config().save_path)
    }
}
