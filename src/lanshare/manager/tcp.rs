//! TCP 服务端：接受连接、识别协议形态并分发。
//!
//! 一条连接的首 4 字节是魔数（`0x66677371`，大端裸写、不混淆）：
//! - 魔数正确 → 再读一个 `i32`：`-2` 是 V4、`-3` 是 V5，紧跟 `str(deviceJson)`，之后交给
//!   `fsrv4` / `fsrv5` 继续读第二个命令字；其余值一律按**旧版**处理；
//! - 旧版：首 4 字节已经是命令字，补读包头剩下 8 字节、按 `len` 读包体（这一路**不混淆**，
//!   与 `DataDec` 在 TCP 分支没调 `decAllData()` 的行为一致），解出设备八字段后分发；
//! - 首 4 字节是 `GET`/`POST` → 交给 [`crate::web`] 的 HTTP/WebSocket 服务（只在配置了
//!   网页服务时），其余一律按非法连接断开。浏览器与文件传输共用这一个端口，
//!   与原版 `TcpProtocol::handleTcp` 的分流完全一致。

use std::io::Write;
use std::net::{Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use super::device::{Device, now_ms};
use super::proto::{
    DATA_VERSION, FS_ADD_DEVICE, FS_GET_NO_SYNC_MEDIA, FS_MESSAGE, FS_SHARE_FILE, MAGIC, NEW_VERSION_4, NEW_VERSION_5, Packet,
    Payload,
};
use super::stream::Conn;
use super::{fsrv4, fsrv5, legacy, proto};
use super::Engine;

/// 握手阶段的读超时：连上来不发魔数的连接不值得一直占着线程。
const HANDSHAKE: Duration = Duration::from_secs(10);

/// 起接受线程：主监听保证 IPv4 通路（Linux 上走 `[::]` 双栈，一个监听同时吃 IPv4/IPv6）。
///
/// Windows/macOS 上 `[::]` 默认是 IPv6-only（不能顺带收 IPv4），所以主监听绑 IPv4，
/// 再尽力在同一端口补一条 IPv6-only 监听，让 IPv6 设备能连进来；本机没 IPv6 就自动跳过。
/// 主监听绑不上（端口被占）时返回空表，其余服务照常。
pub fn spawn(engine: Arc<Engine>) -> Vec<JoinHandle<()>> {
    let port = clamp_port(engine.config().tcp_port);
    let v4 = SocketAddr::from(([0, 0, 0, 0], port));

    #[cfg(target_os = "linux")]
    let primary = TcpListener::bind(SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), port))
        .or_else(|_| TcpListener::bind(v4));
    #[cfg(not(target_os = "linux"))]
    let primary = TcpListener::bind(v4);

    let mut handles = Vec::new();
    let Ok(listener) = primary else { return handles };
    if engine.set_listener(listener).is_err() {
        return handles;
    }
    let Some(listener) = engine.listener() else { return handles };
    handles.push(spawn_accept(Arc::clone(&engine), listener));

    // 额外 IPv6 监听：复用主监听实际占到的端口，best-effort，绑不上不影响 IPv4。
    #[cfg(not(target_os = "linux"))]
    if let Some(actual) = engine.listener().and_then(|l| l.local_addr().ok()).map(|a| a.port()) {
        let v6 = SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), actual);
        if let Ok(extra) = TcpListener::bind(v6) {
            if extra.set_nonblocking(true).is_ok() {
                handles.push(spawn_accept(engine, Arc::new(extra)));
            }
        }
    }
    handles
}

/// 用给定监听跑一条接受线程，线程句柄交回调用方登记进工作线程表。
fn spawn_accept(engine: Arc<Engine>, listener: Arc<TcpListener>) -> JoinHandle<()> {
    std::thread::spawn(move || accept_loop(&engine, listener))
}

fn clamp_port(port: i32) -> u16 {
    port.clamp(0, i32::from(u16::MAX)) as u16
}

/// 非阻塞轮询接受，线程本身也登记进 `Engine` 的工作线程表。
fn accept_loop(engine: &Arc<Engine>, listener: Arc<TcpListener>) {
    while engine.is_running() {
        match listener.accept() {
            Ok((socket, _)) => {
                let engine = Arc::clone(engine);
                std::thread::spawn(move || {
                    let _ = handle(&engine, socket);
                });
            }
            // 读超时/被中断只是本轮没有新连接
            Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                if !engine.wait_stop(200) {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

/// 连上一台设备并写好魔数，返回可直接走 `CustomDataStream` 语义的连接。
pub fn connect(to: &Device) -> std::io::Result<Conn> {
    // IPv6 地址必须经 `socket_address` 加方括号，否则 `ip:port` 拼接后端口解析失败。
    let address = super::nets::socket_address(&to.ip, clamp_port(to.port));
    let mut socket = TcpStream::connect(address.as_str())?;
    socket.write_all(&MAGIC.to_be_bytes())?;
    Conn::new(socket)
}

/// 长文本（超过 700 字节）走旧版 TCP 帧：`FS_MESSAGE` + 设备八字段 + 密文。
///
/// 字段顺序照原版 `UdpProtocol::broadcastMessage` 里的 `DeviceManager::makeUdpDataEnc`，
/// 唯一码带 `-<版本>` 后缀。数值版本字段写当前 `DATA_VERSION`（与 UDP 造包一致），
/// 不再沿用原版固定写 `3` 的遗留行为——只认数值字段的对端会把本机标成「V3」。
pub fn send_legacy_message(engine: &Engine, to: &Device, encoded: &str) -> std::io::Result<()> {
    let config = engine.config();
    let from = engine.pick_local(&to.ip).ok_or_else(|| std::io::Error::other("没有可用网卡"))?;
    let mut packet = Packet::new(FS_MESSAGE, 0);
    let uuid = format!("{}-{DATA_VERSION}", config.unique_uuid);
    super::proto::write_device_fields(
        &mut packet,
        from.port,
        &from.ip,
        &from.name,
        from.mode,
        &uuid,
        DATA_VERSION,
        from.battery,
        from.charge as u8,
    );
    packet.string(encoded);
    let conn = connect(to)?;
    conn.write_raw(&packet.plain())?;
    conn.shutdown();
    Ok(())
}

/// 处理一条已建立的连接。
fn handle(engine: &Arc<Engine>, socket: TcpStream) -> std::io::Result<()> {
    // 监听套接字为了能轮询停止标志是非阻塞的，而 Windows 上新接受的套接字**会继承这个模式**：
    // 不改回阻塞，文件流里第一次「数据还没到」的读就直接返回 WSAEWOULDBLOCK，传输会在 0% 失败。
    let _ = socket.set_nonblocking(false);
    let _ = socket.set_read_timeout(Some(HANDSHAKE));
    let conn = Conn::new(socket)?;
    let mut head = [0u8; 4];
    conn.read_into(&mut head)?;
    let magic = i32::from_be_bytes(head);
    if magic != MAGIC {
        // 浏览器没有魔数，只认请求行的首 4 个字节；没开网页服务就按非法连接静默断开
        if engine.config().web_service && crate::web::looks_like_http(&head) {
            return crate::web::serve(Arc::clone(engine), conn, head);
        }
        conn.shutdown();
        return Ok(());
    }
    // 握手完成，后续按业务节奏阻塞读（文件流可能长时间没有数据）
    conn.set_read_timeout(None)?;
    let cmd = conn.read_i32()?;
    if cmd == NEW_VERSION_4 || cmd == NEW_VERSION_5 {
        let from = Device::from_handshake_json(&conn.read_str()?);
        note_peer(engine, &from);
        // V4/V5 的分流只看首帧魔数（与原版 Java/Qt 一致）：手机 V5 的 JSON 里没有 `resume` 字段，
        // 但同意后一定会回一张偏移量表，走错分支就会把量表当成首个数据块长度。
        return if cmd == NEW_VERSION_5 {
            fsrv5::handle_version(engine, conn, from)
        } else {
            fsrv4::handle_version(engine, conn, from, false)
        };
    }
    handle_legacy(engine, conn, cmd)
}

/// 旧版一帧：`[cmd][count][len]` + 设备八字段 + 命令自己的内容。
fn handle_legacy(engine: &Arc<Engine>, conn: Conn, cmd: i32) -> std::io::Result<()> {
    let mut header = [0u8; proto::HEADER_LEN];
    header[..4].copy_from_slice(&cmd.to_be_bytes());
    conn.read_into(&mut header[4..])?;
    let len = i32::from_be_bytes(header[8..12].try_into().unwrap_or([0; 4]));
    let mut raw = header.to_vec();
    if len > 0 {
        let mut body = vec![0u8; len as usize];
        conn.read_into(&mut body)?;
        raw.extend_from_slice(&body);
    }
    let (_, mut payload) = Payload::from_plain(&raw).ok_or_else(|| std::io::Error::other("旧版包头不完整"))?;
    let from = Device::from_payload(&mut payload, now_ms());
    note_peer(engine, &from);
    match cmd {
        FS_SHARE_FILE => {
            let enc_data = payload.boolean();
            let files = legacy::read_descriptors(&conn, payload.count())?;
            engine.offer_legacy_receive(conn, from, enc_data, files);
        }
        FS_MESSAGE => {
            let text = payload.string();
            engine.on_message(from, &text, false);
            conn.shutdown();
        }
        FS_ADD_DEVICE => {
            let is_ipv6 = payload.boolean();
            // 扫码添加的设备永久保留（不参与 20 秒心跳清理）
            let mut pinned = from.clone();
            pinned.can_remove = false;
            engine.touch_device(pinned);
            reply_add_device(engine, &conn, &from, is_ipv6)?;
            conn.shutdown();
        }
        // 媒体同步属于阶段二
        FS_GET_NO_SYNC_MEDIA => {
            conn.shutdown();
        }
        _ => {
            conn.shutdown();
        }
    }
    Ok(())
}

/// 回送本机设备信息，字段顺序必须与扫描端解析顺序一致。
fn reply_add_device(engine: &Engine, conn: &Conn, from: &Device, is_ipv6: bool) -> std::io::Result<()> {
    let config = engine.config();
    let Some(local) = engine.pick_local(&from.ip) else { return Ok(()) };
    let mut packet = Packet::new(FS_ADD_DEVICE, 0);
    packet.i32(local.port);
    packet.string(&local.ip);
    packet.string(&local.name);
    packet.i32(local.mode);
    packet.string(&format!("{}-{DATA_VERSION}", config.unique_uuid));
    packet.i32(DATA_VERSION);
    packet.boolean(is_ipv6);
    conn.write_raw(&packet.plain())
}

/// 握手里的设备入表；本机地址与自身网卡都要过滤。
fn note_peer(engine: &Engine, device: &Device) {
    if device.ip.is_empty() || engine.is_local_ip(&device.ip) {
        return;
    }
    engine.touch_device(device.clone());
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::super::config::Config;
    use super::super::crypto::enc_message;
    use super::super::device::MODE_ANDROID;
    use super::*;
    use crate::lanshare::NetEvent;

    fn engine() -> Arc<Engine> {
        let mut config = Config::default();
        config.save_path = std::env::temp_dir().join(format!("lanshare-tcp-{}", std::process::id())).to_string_lossy().into_owned();
        config.unique_uuid = "uuid-tcp".to_string();
        config.allow_same_host_test = true;
        // 关掉自动接收，方便观察「待确认」分支
        config.accept_recv_files = false;
        Arc::new(Engine::for_test(config))
    }

    /// 造一对已连接的套接字，返回（服务端句柄，客户端流）。
    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = TcpStream::connect(address).unwrap();
        let (server, _) = listener.accept().unwrap();
        (server, client)
    }

    fn phone() -> Device {
        Device {
            name: "手机".into(),
            ip: "127.0.0.1".into(),
            port: 5856,
            mode: MODE_ANDROID,
            uuid: "remote".into(),
            data_version: DATA_VERSION,
            ..Default::default()
        }
    }

    #[test]
    fn new_style_message_is_dispatched_to_the_engine() {
        let engine = engine();
        let (server, client) = pair();
        let key = engine.config().message_key;
        let sender = Conn::new(client.try_clone().unwrap()).unwrap();
        sender.write_i32(MAGIC).unwrap();
        sender.write_i32(NEW_VERSION_5).unwrap();
        sender.write_str(&phone().handshake_json()).unwrap();
        sender.write_i32(FS_MESSAGE).unwrap();
        sender.write_bool(true).unwrap();
        sender.write_str(&enc_message("新式消息", &key)).unwrap();
        sender.shutdown();

        handle(&engine, server).expect("握手与分发不应出错");
        assert_eq!(engine.devices().len(), 1, "握手里带的设备要入表");
        let events = engine.take_events(Duration::from_millis(50));
        let text = events
            .iter()
            .find_map(|event| match event {
                NetEvent::Message { text, clipboard, incoming, .. } => Some((text.clone(), *clipboard, *incoming)),
                _ => None,
            })
            .expect("应收到消息事件");
        assert_eq!(text, ("新式消息".to_string(), true, true));
    }

    #[test]
    fn legacy_message_frame_is_parsed_without_obfuscation() {
        let engine = engine();
        let (server, client) = pair();
        let key = engine.config().message_key;
        let mut packet = Packet::new(FS_MESSAGE, 0);
        super::super::proto::write_device_fields(
            &mut packet,
            5856,
            "127.0.0.1",
            "旧版手机",
            MODE_ANDROID,
            "remote-5",
            proto::DATA_VERSION_3,
            42,
            0,
        );
        packet.string(&enc_message("旧版消息", &key));
        let raw = packet.plain();
        client.try_clone().unwrap().write_all(&MAGIC.to_be_bytes()).unwrap();
        client.try_clone().unwrap().write_all(&raw).unwrap();
        drop(client);

        handle(&engine, server).expect("旧版帧应当可解");
        let events = engine.take_events(Duration::from_millis(50));
        assert!(
            events.iter().any(|event| matches!(event, NetEvent::Message { text, .. } if text == "旧版消息")),
            "{events:?}"
        );
        let phone = &engine.devices()[0];
        assert_eq!(phone.battery, 42);
        assert_eq!(phone.data_version, DATA_VERSION, "uuid 后缀里的版本要生效");
    }

    #[test]
    fn non_magic_prefix_closes_the_connection() {
        let engine = engine();
        let mut config = engine.config();
        // 关掉网页服务：这时 `GET` 就是非法连接，与原版「先判 `webService` 再分流」一致
        config.web_service = false;
        // 不走 `set_config`：测试不该把临时配置写进用户的真配置文件
        *engine.config.write().expect("配置写锁未被污染") = config;
        let (server, client) = pair();
        client.try_clone().unwrap().write_all(b"GET ").unwrap();
        drop(client);
        handle(&engine, server).expect("非协议连接只能安静地断开");
        assert!(engine.devices().is_empty());
    }

    /// 开着网页服务时，`GET` 不再被当成坏魔数，而是交给 HTTP 服务器回答。
    #[test]
    fn a_get_request_is_routed_to_the_web_server() {
        let engine = engine();
        let (server, mut client) = pair();
        client.write_all(b"GET /hello HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        // 客户端先半关：服务端的优雅关闭会排干残余数据，不管它就是两秒白等
        client.shutdown(std::net::Shutdown::Write).unwrap();
        handle(&engine, server).expect("GET 应当走网页分支");
        let mut text = String::new();
        client.read_to_string(&mut text).unwrap();
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
        assert!(text.ends_with("ok"), "{text}");
        assert!(engine.devices().is_empty(), "浏览器连接不该造出一台设备");
    }

    #[test]
    fn add_device_reply_carries_the_local_fields() {
        let engine = engine();
        // 应答要用本机网卡信息，测试里手工填一块环回网卡
        *engine.local.lock().unwrap() =
            vec![Device { ip: "127.0.0.1".into(), port: 5856, name: engine.config().client_name, net_mask: "255.0.0.0".into(), ..Default::default() }];
        let (server, mut client) = pair();
        let mut packet = Packet::new(FS_ADD_DEVICE, 0);
        super::super::proto::write_device_fields(&mut packet, 5856, "127.0.0.1", "扫码的手机", MODE_ANDROID, "scan", proto::DATA_VERSION_3, 9, 0);
        packet.boolean(false);
        client.try_clone().unwrap().write_all(&MAGIC.to_be_bytes()).unwrap();
        client.try_clone().unwrap().write_all(&packet.plain()).unwrap();

        handle(&engine, server).expect("扫码应答不应出错");
        let added = &engine.devices()[0];
        assert!(!added.can_remove, "扫码设备永久保留");
        assert_eq!(added.name, "扫码的手机");

        let mut reply = vec![0u8; 12];
        client.try_clone().unwrap().read_exact(&mut reply).unwrap();
        let len = i32::from_be_bytes(reply[8..12].try_into().unwrap()) as usize;
        let mut body = vec![0u8; len];
        client.read_exact(&mut body).unwrap();
        let (cmd, mut payload) = Payload::from_plain(&[reply, body].concat()).unwrap();
        assert_eq!(cmd, FS_ADD_DEVICE);
        assert!(payload.i32() > 0, "应答里要带本机 TCP 端口");
        assert!(!payload.string().is_empty(), "应答里要带本机 IP");
        assert_eq!(payload.string(), engine.config().client_name);
        payload.i32();
        assert!(payload.string().ends_with(&format!("-{DATA_VERSION}")), "唯一码要带版本后缀");
        assert_eq!(payload.i32(), DATA_VERSION);
        assert!(!payload.boolean());
    }

    #[test]
    fn file_share_offer_waits_for_the_user() {
        let engine = engine();
        let (server, client) = pair();
        let sender = Conn::new(client.try_clone().unwrap()).unwrap();
        sender.write_i32(MAGIC).unwrap();
        sender.write_i32(NEW_VERSION_4).unwrap();
        sender.write_str(&phone().handshake_json()).unwrap();
        sender.write_i32(FS_SHARE_FILE).unwrap();
        sender.write_bool(false).unwrap();
        sender.write_str(r#"{"files":[{"fileId":"f1","name":"a.txt","length":3,"fileType":5}]}"#).unwrap();

        handle(&engine, server).expect("应当挂起待确认");
        let events = engine.take_events(Duration::from_millis(50));
        let offer = events
            .iter()
            .find_map(|event| match event {
                NetEvent::Receive(offer) => Some(offer.clone()),
                _ => None,
            })
            .expect("应收到接收确认事件");
        assert_eq!(offer.names, vec!["a.txt".to_string()]);
        assert_eq!(offer.total, 3);
        // 拒绝之后回 1103，并且不再持有连接
        engine.resolve_receive(offer.key, false);
        assert_eq!(sender.read_i32().unwrap(), proto::FS_NOT_AGREE);
        drop(sender);
    }

    #[test]
    fn auto_accept_skips_the_dialog() {
        let engine = engine();
        engine.set_config({
            let mut config = engine.config();
            config.accept_recv_files = true;
            config
        });
        let (server, client) = pair();
        let sender = Conn::new(client.try_clone().unwrap()).unwrap();
        sender.write_i32(MAGIC).unwrap();
        sender.write_i32(NEW_VERSION_4).unwrap();
        sender.write_str(&phone().handshake_json()).unwrap();
        sender.write_i32(FS_SHARE_FILE).unwrap();
        sender.write_bool(false).unwrap();
        sender.write_str(r#"{"files":[{"fileId":"f1","name":"a.txt","length":0,"fileType":5}]}"#).unwrap();

        handle(&engine, server).expect("自动接收不需要用户介入");
        assert_eq!(sender.read_i32().unwrap(), proto::FS_AGREE, "自动接收要立刻回 1102");
        // 接收在工作线程里跑，事件是一条条到的：攒到看见 Start 为止
        let mut events = Vec::new();
        for _ in 0..20 {
            events.extend(engine.take_events(Duration::from_millis(100)));
            if events.iter().any(|event| matches!(event, NetEvent::Start(_))) {
                break;
            }
        }
        assert!(!events.iter().any(|event| matches!(event, NetEvent::Receive(_))), "自动接收不该弹确认: {events:?}");
        assert!(events.iter().any(|event| matches!(event, NetEvent::Start(_))), "要直接收到开始传输的事件: {events:?}");
        drop(sender);
    }
}
