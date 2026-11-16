//! UDP 设备发现、心跳与短消息通道（对应原版 `UdpProtocol::handleUdp` + `DeviceManager` 的 UDP 部分）。
//!
//! 报文形态：`[MAGIC:i32BE][混淆后的包头 + 包体]`，收端先剥魔数再去混淆。
//! 心跳 1001 广播出去后，收到的一方会单播回 1002 通知上线；本机网卡 5 秒刷新一次并重新广播，
//! 同时清理 20 秒没心跳的设备（扫码添加的 `can_remove == false` 不清理）。

use std::io::ErrorKind;
use std::net::{Ipv6Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use super::device::{Device, now_ms};
use super::proto::{
    DATA_VERSION, Packet, Payload, UDP_DEVICE_OFF_LINE, UDP_GET_DEVICES, UDP_MESSAGE, UDP_MESSAGE_TO_CLIPBOARD,
    UDP_SEND_MEDIA_MUTE, UDP_SEND_MEDIA_RESTORE, UDP_SET_DEVICES, write_device_fields,
};
use super::Engine;

/// 收包轮询周期：让线程能尽快从阻塞里回来检查停止标志。
const POLL: Duration = Duration::from_millis(300);
/// 扫描周期，与原版一致 5 秒。
const SCAN_MS: u64 = 5 * 1000;
/// 单个数据报上限，与原版 `new mbyte[4096]` 同宽。
const MAX_DATAGRAM: usize = 4096;

/// 一条已解包的 UDP 指令。
struct Incoming {
    cmd: i32,
    device: Device,
    /// 仅消息类命令有值，是解密前的密文（hex 字符串）。
    body: String,
}

/// 拉起收包与扫描线程。IPv4 socket 必绑（设备发现靠 IPv4 广播），绑不上返回空线程表；
/// 再尽力开一条 IPv6 socket 及其收包线程，本机没 IPv6 就跳过。TCP 链路不受影响。
pub fn spawn(engine: Arc<Engine>) -> Vec<JoinHandle<()>> {
    let port = engine.config().udp_port;
    // 主 socket：Linux 走 `[::]` 双栈（一个 socket 同时收发 v4/v6，广播也照常），
    // 其余平台主 socket 绑 IPv4，下面再单独补 IPv6。
    #[cfg(target_os = "linux")]
    let primary = bind_family(true, port);
    #[cfg(not(target_os = "linux"))]
    let primary = bind_family(false, port);
    let Ok(socket) = primary else { return Vec::new() };
    let socket = Arc::new(socket);
    engine.set_udp(Arc::clone(&socket));
    let mut handles = Vec::new();
    {
        let engine = Arc::clone(&engine);
        let reader = Arc::clone(&socket);
        handles.push(std::thread::spawn(move || receive_loop(&engine, &reader)));
    }
    // Windows/macOS：再尽力补一条 IPv6-only socket 及其收包线程；没 IPv6 就跳过。
    #[cfg(not(target_os = "linux"))]
    if let Ok(v6) = bind_family(true, port) {
        let v6 = Arc::new(v6);
        engine.set_udp6(Arc::clone(&v6));
        let engine = Arc::clone(&engine);
        handles.push(std::thread::spawn(move || receive_loop(&engine, &v6)));
    }
    {
        let engine = Arc::clone(&engine);
        handles.push(std::thread::spawn(move || scan_loop(&engine)));
    }
    handles
}

/// 绑定一个指定协议族的 UDP socket：`v6=true` 绑 `[::]`（Windows/macOS 上默认 IPv6-only，
/// 与 IPv4 socket 互不打架），否则绑 `0.0.0.0`。端口被占用时退化成随机端口
/// （同机多实例测试要的就是这个行为）。`SO_BROADCAST` 是 socket 层选项，v4/v6 都能设。
fn bind_family(v6: bool, port: i32) -> std::io::Result<UdpSocket> {
    let port = clamp_port(port);
    let addr = |p: u16| {
        if v6 {
            SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), p)
        } else {
            SocketAddr::from(([0, 0, 0, 0], p))
        }
    };
    let socket = UdpSocket::bind(addr(port)).or_else(|_| UdpSocket::bind(addr(0)))?;
    socket.set_broadcast(true)?;
    socket.set_read_timeout(Some(POLL))?;
    socket.set_write_timeout(Some(POLL))?;
    Ok(socket)
}

fn clamp_port(port: i32) -> u16 {
    port.clamp(0, i32::from(u16::MAX)) as u16
}

/// 收包循环：读超时回来的 `WouldBlock` 只用于检查停止标志。
fn receive_loop(engine: &Engine, socket: &UdpSocket) {
    let mut buf = vec![0u8; MAX_DATAGRAM];
    while engine.is_running() {
        match socket.recv_from(&mut buf) {
            Ok((len, from)) => handle(engine, &buf[..len], from),
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted) => {}
            // 套接字被 shutdown 或系统错误，退出循环
            Err(_) => break,
        }
    }
}

fn scan_loop(engine: &Engine) {
    while engine.is_running() {
        // 网卡可能插拔，每轮重新枚举；广播地址来自各网卡自己
        for device in engine.refresh_local() {
            if device.broadcast_ip.is_empty() {
                continue;
            }
            let _ = send_from(engine, &device, &device.broadcast_ip, UDP_GET_DEVICES, None);
        }
        engine.cleanup_expired();
        if !engine.wait_stop(SCAN_MS) {
            break;
        }
    }
}

/// 解析一个数据报：魔数、混淆、设备八字段，消息类命令再补一条密文。
fn decode(raw: &[u8]) -> Option<Incoming> {
    let (cmd, mut payload) = Payload::from_udp(raw)?;
    let device = Device::from_payload(&mut payload, now_ms());
    if device.ip.is_empty() {
        return None;
    }
    let body = matches!(cmd, UDP_MESSAGE | UDP_MESSAGE_TO_CLIPBOARD).then(|| payload.string()).unwrap_or_default();
    Some(Incoming { cmd, device, body })
}

/// 处理一条已解析的指令。与原版顺序一致：先登记设备，再按命令字分发。
fn handle(engine: &Engine, raw: &[u8], from: SocketAddr) {
    let Some(incoming) = decode(raw) else { return };
    // 收到本机网卡发出的包直接丢弃（同机双实例测试时由配置放开）
    if engine.is_local_ip(&incoming.device.ip) {
        return;
    }
    let clipboard = incoming.cmd == UDP_MESSAGE_TO_CLIPBOARD;
    let message = matches!(incoming.cmd, UDP_MESSAGE | UDP_MESSAGE_TO_CLIPBOARD).then(|| incoming.body.clone());
    let Incoming { cmd, device, .. } = incoming;
    engine.touch_device(device.clone());
    match cmd {
        // 心跳：回一条单播上线通知，让对方立刻拿到本机信息
        UDP_GET_DEVICES => {
            let _ = engine_reply(engine, &device.ip, UDP_SET_DEVICES);
        }
        UDP_SET_DEVICES => {}
        UDP_DEVICE_OFF_LINE => engine.forget_device(&device),
        UDP_MESSAGE | UDP_MESSAGE_TO_CLIPBOARD => {
            if let Some(text) = message {
                engine.on_message(device, &text, clipboard);
            }
        }
        // 静音/恢复音量属于阶段二（需要系统音频 API），这里只保证设备被登记
        UDP_SEND_MEDIA_MUTE | UDP_SEND_MEDIA_RESTORE => {}
        _ => {}
    }
    let _ = from;
}

/// 收到心跳后的单播应答。
fn engine_reply(engine: &Engine, target_ip: &str, cmd: i32) -> std::io::Result<()> {
    let from = pick_nic(engine, target_ip)?;
    send_from(engine, &from, target_ip, cmd, None)
}

/// 按原版 `makeUdpDataEnc` 的字段顺序造包，`uuid` 带上 `-<版本>` 后缀。
///
/// 数值版本字段写当前 `DATA_VERSION`（而不是原版遗留的固定 3）：不少手机端直接把设备徽章
/// 画成这个数值字段，写 3 会让对端把本机标成「V3」。真实版本同时靠 uuid 的 `-<version>`
/// 后缀再传一份，兼容只认后缀的对端（原版桌面、本机自己），两条通道都说同一个版本号。
fn build_packet(from: &Device, unique_uuid: &str, cmd: i32, body: Option<&str>) -> Vec<u8> {
    let mut packet = Packet::new(cmd, 0);
    let uuid = format!("{unique_uuid}-{DATA_VERSION}");
    write_device_fields(&mut packet, from.port, &from.ip, &from.name, from.mode, &uuid, DATA_VERSION, from.battery, from.charge as u8);
    if let Some(text) = body {
        packet.string(text);
    }
    packet.udp()
}

/// 用指定网卡发出一个设备报文（广播心跳、单播上线/下线、文本消息共用）。
fn send_from(engine: &Engine, from: &Device, target_ip: &str, cmd: i32, body: Option<&str>) -> std::io::Result<()> {
    let config = engine.config();
    let bytes = build_packet(from, &config.unique_uuid, cmd, body);
    // 目标是 IPv6（含冒号）就走 IPv6 socket，否则走 IPv4；地址族不匹配的 send_to 会直接失败。
    let socket = pick_socket(engine, target_ip.contains(':'))?;
    // IPv6 目标地址必须经 `socket_address` 加方括号，否则 `ip:port` 拼接后 `parse::<SocketAddr>()` 失败。
    let address: SocketAddr = super::nets::socket_address(target_ip, clamp_port(config.udp_port))
        .parse()
        .map_err(|error| std::io::Error::new(ErrorKind::InvalidInput, error))?;
    socket.send_to(&bytes, address).map(|_| ())
}

/// 按目标协议族挑一个可用 socket：优先用服务启动时绑好的；Linux 下 IPv6 由双栈主 socket
/// 涵盖（`udp6()` 为空），回落到主 socket；实在没有就临时绑一个随机端口的同族 socket。
fn pick_socket(engine: &Engine, v6: bool) -> std::io::Result<Arc<UdpSocket>> {
    if v6 {
        if let Some(socket) = engine.udp6().or_else(|| engine.udp()) {
            return Ok(socket);
        }
        return Ok(Arc::new(bind_family(true, 0)?));
    }
    if let Some(socket) = engine.udp() {
        return Ok(socket);
    }
    Ok(Arc::new(bind_family(false, 0)?))
}

/// 发文本消息，`cmd` 取 `UDP_MESSAGE` 或 `UDP_MESSAGE_TO_CLIPBOARD`。
pub fn send_message(engine: &Engine, to: &Device, encoded: &str, cmd: i32) -> std::io::Result<()> {
    let from = pick_nic(engine, &to.ip)?;
    send_from(engine, &from, &to.ip, cmd, Some(encoded))
}

/// 单播通知某台设备：本机已上线（1002）。
pub fn notice_online(engine: &Engine, ip: &str) -> std::io::Result<()> {
    let from = pick_nic(engine, ip)?;
    send_from(engine, &from, ip, UDP_SET_DEVICES, None)
}

/// 单播通知某台设备：本机已下线（1003）。
pub fn notice_offline(engine: &Engine, ip: &str) -> std::io::Result<()> {
    let from = pick_nic(engine, ip)?;
    send_from(engine, &from, ip, UDP_DEVICE_OFF_LINE, None)
}

/// 选一块与目标同网段的网卡；一台都没有时直接报错而不是静默丢包。
fn pick_nic(engine: &Engine, target_ip: &str) -> std::io::Result<Device> {
    engine.pick_local(target_ip).ok_or_else(|| std::io::Error::other("没有可用网卡"))
}

#[cfg(test)]
mod tests {
    use super::super::config::Config;
    use super::super::crypto::enc_message;
    use super::super::proto::udp_device_packet;
    use super::*;

    fn engine() -> Arc<Engine> {
        let mut config = Config::default();
        config.save_path = std::env::temp_dir().join(format!("lanshare-udp-{}", std::process::id())).to_string_lossy().into_owned();
        config.unique_uuid = "uuid-udp".to_string();
        // 放开自过滤，测试里才能用 127.0.0.1 这类本机地址
        config.allow_same_host_test = true;
        Arc::new(Engine::for_test(config))
    }

    fn phone() -> Device {
        Device {
            name: "手机".into(),
            ip: "127.0.0.1".into(),
            port: 5856,
            mode: super::super::device::MODE_ANDROID,
            uuid: "remote".into(),
            data_version: DATA_VERSION,
            battery: 66,
            charge: 1,
            ..Default::default()
        }
    }

    #[test]
    fn decoded_packet_keeps_the_wire_field_order() {
        let bytes = udp_device_packet(UDP_GET_DEVICES, 5856, "192.168.1.9", "张三的手机", 1, "cafe-5", 66, 1);
        let incoming = decode(&bytes).expect("合法报文必须可解");
        assert_eq!(incoming.cmd, UDP_GET_DEVICES);
        assert_eq!(incoming.device.ip, "192.168.1.9");
        assert_eq!(incoming.device.name, "张三的手机");
        assert_eq!(incoming.device.port, 5856);
        assert_eq!(incoming.device.uuid, "cafe");
        assert_eq!(incoming.device.data_version, 5);
        assert_eq!(incoming.device.battery, 66);
        assert_eq!(incoming.device.charge, 1);
        assert!(incoming.body.is_empty(), "心跳包不该解出正文");
    }

    #[test]
    fn message_packet_carries_the_cipher_text() {
        let bytes = build_packet(&phone(), "uuid-udp", UDP_MESSAGE, Some("deadbeef"));
        let incoming = decode(&bytes).expect("消息包必须可解");
        assert_eq!(incoming.cmd, UDP_MESSAGE);
        assert_eq!(incoming.body, "deadbeef");
    }

    /// 回归：只认数值字段的手机会把本机标成「V3」，所以主动广播的报文必须在
    /// 数值版本字段与 uuid 后缀两处都写当前版本——无论对端读哪个都是 V5。
    #[test]
    fn broadcast_packets_advertise_the_real_version_in_both_channels() {
        let bytes = build_packet(&phone(), "uuid-udp", UDP_GET_DEVICES, None);
        let (cmd, mut payload) = Payload::from_udp(&bytes).expect("合法报文必须可解");
        assert_eq!(cmd, UDP_GET_DEVICES);
        let _port = payload.i32();
        let _ip = payload.string();
        let _name = payload.string();
        let _mode = payload.i32();
        let uuid = payload.string();
        let numeric_version = payload.i32();
        assert_eq!(numeric_version, DATA_VERSION, "数值版本字段 must 写当前版本而非遗留的 3");
        assert!(uuid.ends_with(&format!("-{DATA_VERSION}")), "uuid 后缀要带上同一个版本：{uuid}");
    }

    #[test]
    fn bad_magic_and_empty_ip_are_dropped() {
        assert!(decode(&[0, 0, 0, 0, 1, 2, 3]).is_none());
        let mut packet = Packet::new(UDP_GET_DEVICES, 0);
        write_device_fields(&mut packet, 5856, "", "n", 1, "u-5", DATA_VERSION, 0, 0);
        assert!(decode(&packet.udp()).is_none(), "空 IP 的包没有意义");
    }

    #[test]
    fn heartbeat_registers_the_device_and_answers_online() {
        let engine = engine();
        let bytes = build_packet(&phone(), "remote", UDP_GET_DEVICES, None);
        handle(&engine, &bytes, "127.0.0.1:4573".parse().unwrap());
        let list = engine.devices();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "手机");
        // 心跳只产生设备表快照事件，不该有消息事件
        let events = engine.take_events(Duration::from_millis(50));
        assert!(events.iter().all(|event| matches!(event, super::super::NetEvent::Devices(_))), "{events:?}");
    }

    #[test]
    fn offline_packet_forgets_the_device() {
        let engine = engine();
        let bytes = build_packet(&phone(), "remote", UDP_DEVICE_OFF_LINE, None);
        handle(&engine, &bytes, "127.0.0.1:4573".parse().unwrap());
        assert!(engine.devices().is_empty());
    }

    #[test]
    fn text_message_is_decrypted_and_published() {
        let engine = engine();
        let key = engine.config().message_key;
        let bytes = build_packet(&phone(), "remote", UDP_MESSAGE_TO_CLIPBOARD, Some(&enc_message("剪贴板你好", &key)));
        handle(&engine, &bytes, "127.0.0.1:4573".parse().unwrap());
        let events = engine.take_events(Duration::from_millis(50));
        let text = events
            .iter()
            .find_map(|event| match event {
                super::super::NetEvent::Message { text, clipboard, incoming, .. } => Some((text.clone(), *clipboard, *incoming)),
                _ => None,
            })
            .expect("应当收到一条消息事件");
        assert_eq!(text, ("剪贴板你好".to_string(), true, true));
    }

    #[test]
    fn wrong_key_yields_no_message_event() {
        let engine = engine();
        let bytes = build_packet(&phone(), "remote", UDP_MESSAGE, Some(&enc_message("hi", "别的密钥")));
        handle(&engine, &bytes, "127.0.0.1:4573".parse().unwrap());
        let events = engine.take_events(Duration::from_millis(50));
        assert!(
            events.iter().all(|event| !matches!(event, super::super::NetEvent::Message { .. })),
            "解不开的密文不应产生消息"
        );
    }
}
