//! WebSocket 的握手与帧编解码，对应原版 `httpserver/LWebSocketServer.cpp`。
//!
//! 只实现网页端 `WSCmd.js` 用到的那几种帧：文本、关闭、ping/pong。
//! 服务端发出的一律不掩码（RFC 6455 规定服务端到客户端方向禁止掩码），
//! 收方向则必须按 `mask` 位解掩码。

use base64::Engine as _;
use sha1::{Digest as _, Sha1};

use crate::lanshare::stream::Conn;

/// 原版 `Response::WS_MAGIC`。
const WS_MAGIC: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

pub const OP_TEXT: u8 = 0x1;
pub const OP_BINARY: u8 = 0x2;
pub const OP_CLOSE: u8 = 0x8;
pub const OP_PING: u8 = 0x9;
pub const OP_PONG: u8 = 0xA;

/// 一帧解出来的内容。
pub struct Frame {
    pub opcode: u8,
    pub payload: Vec<u8>,
}

/// 握手应答：`101 Switching Protocols` + `Sec-WebSocket-Accept`。
pub fn accept(conn: &Conn, key: &str) -> std::io::Result<()> {
    if key.is_empty() {
        return Err(std::io::Error::other("缺少 Sec-WebSocket-Key"));
    }
    let mut hasher = Sha1::new();
    hasher.update(format!("{key}{WS_MAGIC}").as_bytes());
    let digest = hasher.finalize();
    let accept = base64::engine::general_purpose::STANDARD.encode(digest);
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    conn.write_raw(response.as_bytes())
}

fn byte(conn: &Conn) -> std::io::Result<u8> {
    let mut buf = [0u8; 1];
    conn.read_into(&mut buf)?;
    Ok(buf[0])
}

/// 读一帧。`None` 表示对端已断开或帧形不合法，调用方应当据此收尾。
///
/// 保留位非 0、控制帧超长、控制帧未 FIN 都按协议错误处理——与原版一样直接判死，
/// 不去猜对端想发什么。
pub fn read_frame(conn: &Conn) -> std::io::Result<Option<Frame>> {
    let head = match byte(conn) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let fin = head & 0x80 != 0;
    let opcode = head & 0x0F;
    if head & 0x70 != 0 {
        return Ok(None);
    }
    let second = byte(conn)?;
    let masked = second & 0x80 != 0;
    let mut len = (second & 0x7F) as u64;
    if len == 126 {
        len = u16::from_be_bytes([byte(conn)?, byte(conn)?]) as u64;
    } else if len == 127 {
        let mut raw = [0u8; 8];
        conn.read_into(&mut raw)?;
        len = u64::from_be_bytes(raw);
    }
    // 控制帧的载荷必须在同一个包里给全
    if opcode >= 0x8 && (!fin || len > 125) {
        return Ok(None);
    }
    let mut mask = [0u8; 4];
    if masked {
        conn.read_into(&mut mask)?;
    }
    if len > 1 << 30 {
        return Ok(None);
    }
    let mut payload = vec![0u8; len as usize];
    if len > 0 {
        conn.read_into(&mut payload)?;
    }
    if masked {
        for (index, slot) in payload.iter_mut().enumerate() {
            *slot ^= mask[index % 4];
        }
    }
    Ok(Some(Frame { opcode, payload }))
}

/// 发一帧。掩码位为 0，服务端方向本来就不允许掩码。
pub fn write_frame(conn: &Conn, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut out = vec![0x80 | opcode];
    match payload.len() {
        len if len < 126 => out.push(len as u8),
        len if len <= u16::MAX as usize => {
            out.push(126);
            out.extend_from_slice(&(len as u16).to_be_bytes());
        }
        len => {
            out.push(127);
            out.extend_from_slice(&(len as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(payload);
    conn.write_raw(&out)
}

pub fn send_text(conn: &Conn, text: &str) -> std::io::Result<()> {
    write_frame(conn, OP_TEXT, text.as_bytes())
}

/// 关闭握手：回一个 CLOSE 帧。
pub fn send_close(conn: &Conn) {
    let _ = write_frame(conn, OP_CLOSE, &[]);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 握手值有 RFC 6455 §1.3 的官方向量，算错一个字符浏览器就会停在 connecting。
    #[test]
    fn the_accept_hash_matches_the_rfc_example() {
        let mut hasher = Sha1::new();
        hasher.update(format!("{}{WS_MAGIC}", "x3JJHMbDL1EzLkh9GBhXDw==").as_bytes());
        assert_eq!(base64::engine::general_purpose::STANDARD.encode(hasher.finalize()), "HSmrc0sMlYUkAGmm5OPpG2HaGWk=");
    }

    fn pair() -> (Conn, std::net::TcpStream) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = std::net::TcpStream::connect(address).unwrap();
        let (server, _) = listener.accept().unwrap();
        (Conn::new(server).unwrap(), client)
    }

    /// 客户端帧一律带掩码，解不出来就一条消息都收不到。
    #[test]
    fn masked_frames_are_unmasked_on_read() {
        let (conn, client) = pair();
        let mask = [0x37, 0xfa, 0x21, 0x3d];
        let plain = br"hello";
        let mut raw = vec![0x81, 0x80 | plain.len() as u8];
        raw.extend_from_slice(&mask);
        raw.extend(plain.iter().enumerate().map(|(index, byte)| byte ^ mask[index % 4]));
        std::io::Write::write_all(&mut &client, &raw).unwrap();
        let frame = read_frame(&conn).unwrap().expect("应当解出一帧");
        assert_eq!(frame.opcode, OP_TEXT);
        assert_eq!(frame.payload, plain.to_vec());
    }

    /// 126/127 两种扩展长度都要能解，网页发长文本时会用它们。
    #[test]
    fn extended_payload_lengths_round_trip() {
        let (conn, client) = pair();
        let big = vec![b'x'; 300];
        write_frame(&conn, OP_TEXT, &big).unwrap();
        // 126 那一档除了两个标志字节还要带 2 字节扩展长度，整帧是 4 + 300
        let mut raw = [0u8; 4 + 300];
        std::io::Read::read_exact(&mut &client, &mut raw).unwrap();
        assert_eq!(raw[0], 0x81);
        assert_eq!(raw[1], 126);
        assert_eq!(&raw[4..], &big[..]);

        let (conn, client) = pair();
        let huge = vec![b'y'; 70000];
        write_frame(&conn, OP_TEXT, &huge).unwrap();
        let mut head = [0u8; 10];
        std::io::Read::read_exact(&mut &client, &mut head).unwrap();
        assert_eq!(head[1], 127);
        assert_eq!(u64::from_be_bytes(head[2..10].try_into().unwrap()), 70000);
    }

    /// 保留位非 0、控制帧没置 FIN、控制帧超长都是协议错误，不能当正文收下。
    /// 三个用例都只喂到头两个字节就能判死，不会把读游标漂到不存在的载荷上。
    #[test]
    fn malformed_frames_are_rejected_early() {
        let (conn, client) = pair();
        std::io::Write::write_all(&mut &client, &[0x31]).unwrap();
        assert!(read_frame(&conn).unwrap().is_none(), "保留位非 0 应判死");

        let (conn, client) = pair();
        std::io::Write::write_all(&mut &client, &[0x09, 0x05]).unwrap();
        assert!(read_frame(&conn).unwrap().is_none(), "分段的控制帧应判死");

        let (conn, client) = pair();
        std::io::Write::write_all(&mut &client, &[0x88, 0x7E, 0x00, 0x7F]).unwrap();
        assert!(read_frame(&conn).unwrap().is_none(), "超过 125 字节的控制帧应判死");
    }
}
