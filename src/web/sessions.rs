//! 已建立的 WebSocket 会话表，以及桌面端向网页端推送的三类消息。
//!
//! 原版是一个全局 `std::vector<LWebSocketServer*>` 加一把名字为 `websockets` 的
//! 字符串锁，推送时顺手把已关闭的对象 `erase + delete`。这里换成
//! `Mutex<Vec<Arc<Session>>>`：先在锁里取快照，**在锁外**写 socket——一个网卡慢的
//! 客户端不会把整条推送链堵住，所有权也由 `Arc` 管，不需要手工释放。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::lanshare::device::Device;
use crate::lanshare::stream::Conn;

/// 命令字，与网页端 `web/js/WSCmd.js` 一一对应。
pub const SEND_MESSAGE: i64 = 1;
pub const SYNC_DEVICE_LIST: i64 = 2;
pub const CHANGE_THEME: i64 = 3;
pub const DRAW_EVENT: i64 = 4;
pub const PING: i64 = 5;
pub const PONG: i64 = 6;

/// 一条活着的会话。`Conn` 的读写各占一把锁，因此推送与收帧互不阻塞。
pub struct Session {
    pub conn: Conn,
    pub ip: String,
    closed: AtomicBool,
}

impl Session {
    pub fn new(conn: Conn, ip: String) -> Arc<Self> {
        Arc::new(Self { conn, ip, closed: AtomicBool::new(false) })
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    fn mark_closed(&self) {
        self.closed.store(true, Ordering::Relaxed);
    }
}

static SESSIONS: Mutex<Vec<Arc<Session>>> = Mutex::new(Vec::new());

pub fn add(session: Arc<Session>) {
    if let Ok(mut table) = SESSIONS.lock() {
        table.push(session);
    }
}

/// 会话线程退出时摘掉自己。按指针比对，不靠 IP（同一台机器可能开好几个标签页）。
pub fn remove(session: &Arc<Session>) {
    session.mark_closed();
    if let Ok(mut table) = SESSIONS.lock() {
        table.retain(|live| !Arc::ptr_eq(live, session));
    }
}

pub fn count() -> usize {
    SESSIONS.lock().map(|table| table.iter().filter(|session| !session.is_closed()).count()).unwrap_or(0)
}

/// 关掉全部会话：阻塞在 `read_frame` 的线程会立刻返回，进程退出时不用等它们。
pub fn close_all() {
    let table = snapshot();
    for session in table {
        session.mark_closed();
        session.conn.shutdown();
    }
}

fn snapshot() -> Vec<Arc<Session>> {
    SESSIONS.lock().map(|table| table.clone()).unwrap_or_default()
}

/// 向所有活着的会话发一条 JSON 文本帧。写失败的会话被摘掉。
pub fn broadcast(value: &Value) {
    let text = value.to_string();
    for session in snapshot() {
        if session.is_closed() || super::ws::send_text(&session.conn, &text).is_err() {
            session.mark_closed();
            remove(&session);
        }
    }
}

/// 主题变更：`{"cmd":3,"theme":"light|dark|emerald|follow_system"}`。
///
/// 配置里的空串在原版是被当成「跟随系统」发给网页的，这里保持同样的字面量。
pub fn push_theme(theme: &str) {
    let name = if theme.is_empty() { "follow_system" } else { theme };
    broadcast(&json!({ "cmd": CHANGE_THEME, "theme": name }));
}

/// 设备表快照。网页端用它填「选择发送对象」的下拉与聊天里的设备列表。
///
/// `devMode` 原样透传：协议里的设备类型码（1 安卓 / 2 Windows …）与原版 `Device.h`
/// 的 `L_ANDROID` / `L_WIN` 是同一套数字，前端 `main.min.js` 就按这套码挑图标。
pub fn push_devices(devices: &[Device]) {
    let data: Vec<Value> = devices
        .iter()
        .map(|device| {
            json!({
                "devName": device.name,
                "devIP": device.ip,
                "devPort": device.port,
                "devMode": device.mode,
                "dataVersion": device.data_version,
                "address": format!("{}:{}", device.ip, device.port),
            })
        })
        .collect();
    broadcast(&json!({ "cmd": SYNC_DEVICE_LIST, "data": data }));
}

/// 聊天区的一条气泡。字段名与顺序都和原版 `sendWebSocketMessage` 保持一致，
/// 其中 `fileSize` 是**已经格式化好的字符串**（原版就是 `QString`）。
///
/// 传入的是协议里的设备类型码，出去前经 [`web_dev_type`] 折算成前端那套 0/1。
#[allow(clippy::too_many_arguments)]
pub fn push_message(
    message: &str,
    device_name: &str,
    file_path: &str,
    message_type: i64,
    dev_mode: i32,
    file_size: &str,
    is_left: bool,
    is_clip: bool,
    is_file: bool,
) {
    broadcast(&json!({
        "cmd": SEND_MESSAGE,
        "isLeft": is_left,
        "message": message,
        "devName": device_name,
        "devType": web_dev_type(dev_mode),
        "messageType": message_type,
        "isClip": is_clip,
        "filePath": file_path,
        "fileSize": file_size,
        "isFile": is_file,
    }));
}

/// 聊天气泡的 `devType`：前端 `lanshareChat.js` 只认两种值——
/// `0` 取 `ic_phone.png`，其余一律取 `ic_win.png`。
///
/// 原版把协议的设备类型码直接当这个值传（安卓是 1，于是显示成了电脑图标），
/// 这是它的一个既有缺陷；这里按前端的约定折算，手机就是手机。
pub fn web_dev_type(mode: i32) -> i32 {
    match mode {
        crate::lanshare::device::MODE_ANDROID | crate::lanshare::device::MODE_IOS => 0,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    /// 会话表是进程级全局的，测试线程并行时会互相串台（收到别人的帧、`count` 对不上），
    /// 所以凡是要往表里挂会话的用例都先抢这把锁，排成队跑。
    static SERIAL: Mutex<()> = Mutex::new(());

    fn pair() -> (Arc<Session>, TcpStream, std::sync::MutexGuard<'static, ()>) {
        let guard = SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // 上一个用例如果在 `remove` 之前就断言失败，会留下一条挂在表里的会话，
        // 干扰下一个用例读到的 `count`；抢到锁后先排干净再开始。
        if let Ok(mut table) = SESSIONS.lock() {
            table.clear();
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = TcpStream::connect(address).unwrap();
        let (server, peer) = listener.accept().unwrap();
        (Session::new(Conn::new(server).unwrap(), peer.ip().to_string()), client, guard)
    }

    /// 客户端侧解一帧正文。载荷超过 125 字节时头是 4 字节（126 + 两个长度字节），
    /// 推送的设备表与气泡都是这种长度，只读两字节头会把正文错一位。
    fn read_text(client: &mut TcpStream) -> (u8, String) {
        let mut head = [0u8; 2];
        std::io::Read::read_exact(client, &mut head).unwrap();
        let mut length = (head[1] & 0x7F) as usize;
        if length == 126 {
            let mut raw = [0u8; 2];
            std::io::Read::read_exact(client, &mut raw).unwrap();
            length = u16::from_be_bytes(raw) as usize;
        } else if length == 127 {
            let mut raw = [0u8; 8];
            std::io::Read::read_exact(client, &mut raw).unwrap();
            length = u64::from_be_bytes(raw) as usize;
        }
        let mut body = vec![0u8; length];
        std::io::Read::read_exact(client, &mut body).unwrap();
        (head[0] & 0x0F, String::from_utf8(body).unwrap())
    }

    /// 推的是压缩后的 JSON 文本帧，浏览器那边 `JSON.parse` 才接得住。
    #[test]
    fn a_broadcast_session_receives_the_frame() {
        let (session, mut client, _serial) = pair();
        add(session.clone());
        push_devices(&[Device { name: "手机".into(), ip: "192.168.1.9".into(), port: 5856, mode: 1, data_version: 5, ..Default::default() }]);
        let (opcode, text) = read_text(&mut client);
        assert_eq!(opcode, super::super::ws::OP_TEXT);
        assert!(text.contains("\"cmd\":2"), "{text}");
        assert!(text.contains("\"address\":\"192.168.1.9:5856\""), "{text}");
        remove(&session);
        assert_eq!(count(), 0);
    }

    /// 写坏掉的会话必须被摘掉，否则表会越积越大、每次推送都多撞一次错。
    #[test]
    fn a_dead_session_is_dropped_on_the_next_push() {
        let (session, client, _serial) = pair();
        add(session.clone());
        assert_eq!(count(), 1);
        drop(client);
        // 第一次写会失败（或对端已 FIN），第二次一定被摘干净
        for _ in 0..3 {
            push_theme("dark");
        }
        assert_eq!(count(), 0, "死连接应当被清理");
        assert!(session.is_closed());
    }

    /// 「跟随系统」在配置里是空串，发给网页的要是对的枚举字面量。
    #[test]
    fn the_empty_theme_is_translated_to_follow_system() {
        let (session, mut client, _serial) = pair();
        add(session.clone());
        push_theme("");
        assert!(read_text(&mut client).1.contains("follow_system"));
        remove(&session);
    }

    /// 气泡的 `devType` 折算：手机归 0，桌面与其它都归 1（前端只有这两种图标）。
    #[test]
    fn phone_modes_map_to_the_phone_icon() {
        assert_eq!(web_dev_type(crate::lanshare::device::MODE_ANDROID), 0);
        assert_eq!(web_dev_type(crate::lanshare::device::MODE_IOS), 0);
        assert_eq!(web_dev_type(crate::lanshare::device::MODE_WIN), 1);
        assert_eq!(web_dev_type(crate::lanshare::device::MODE_UNKNOWN), 1, "认不出的一律按电脑画");
    }

    /// 设备表里的 `devMode` 不能被折算，前端下拉与聊天列表都按协议码读。
    #[test]
    fn the_device_list_keeps_the_protocol_mode() {
        let (session, mut client, _serial) = pair();
        add(session.clone());
        push_devices(&[Device { name: "手机".into(), ip: "10.0.0.1".into(), port: 5856, mode: 1, ..Default::default() }]);
        let text = read_text(&mut client).1;
        assert!(text.contains("\"devMode\":1"), "安卓要还是 1: {text}");
        remove(&session);
    }

    /// 消息气泡推的是折算后的 0/1，安卓设备发来的消息要显示手机图标。
    #[test]
    fn a_pushed_bubble_carries_the_web_device_type() {
        let (session, mut client, _serial) = pair();
        add(session.clone());
        push_message("你好", "手机", "", 0, 1, "", true, false, false);
        let text = read_text(&mut client).1;
        assert!(text.contains("\"devType\":0"), "安卓消息要是手机图标: {text}");
        assert!(text.contains("\"cmd\":1"), "{text}");
        remove(&session);
    }
}
