//! TCP 上的裸大端数据流，对应原版 `CustomDataStream`。
//!
//! V4/V5 的新式流程不混淆：魔数之后每个字段都是大端裸字节，字符串是 `[i32BE 长度][UTF-8]`。
//! 原版一个连接同时有「写数据」的线程和「读取消除指令」的线程，所以这里把读写拆成两个句柄，
//! 各自加锁，可以并发使用。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};

/// 一条 TCP 连接的可克隆句柄。
#[derive(Clone)]
pub struct Conn {
    read: Arc<Mutex<TcpStream>>,
    write: Arc<Mutex<TcpStream>>,
}

/// 半关之后排干残余数据的最长等待，对应原版 `HttpServer::gracefulClose` 的 `SO_RCVTIMEO 2s`。
const DRAIN: std::time::Duration = std::time::Duration::from_secs(2);

impl Conn {
    pub fn new(socket: TcpStream) -> std::io::Result<Self> {
        let read = Arc::new(Mutex::new(socket.try_clone()?));
        Ok(Self { read, write: Arc::new(Mutex::new(socket)) })
    }

    pub fn remote_addr(&self) -> std::io::Result<std::net::SocketAddr> {
        let guard = self.read.lock().map_err(locked)?;
        guard.peer_addr()
    }

    pub fn set_read_timeout(&self, timeout: Option<std::time::Duration>) -> std::io::Result<()> {
        let guard = self.read.lock().map_err(locked)?;
        guard.set_read_timeout(timeout)
    }

    pub fn shutdown(&self) {
        if let Ok(guard) = self.write.lock() {
            let _ = guard.shutdown(std::net::Shutdown::Both);
        }
    }

    /// 优雅关闭：先半关写方向让浏览器收到 FIN 认为响应已发完，再带超时排干对端残余字节。
    ///
    /// 直接两向关会在「还有未读的请求体」时触发 RST，浏览器可能读不到我们已经写出去的 302/500。
    /// 已升级成 WebSocket 的连接不能走这里（排干会把会话自己读死），由调用方改用 `shutdown`。
    pub fn graceful_close(&self) {
        if let Ok(guard) = self.write.lock() {
            let _ = guard.shutdown(std::net::Shutdown::Write);
        }
        self.drain();
        self.shutdown();
    }

    /// 带超时把对端没发完的字节丢掉，读错（含超时）即视为排干。
    fn drain(&self) {
        let Ok(guard) = self.read.lock() else { return };
        let _ = guard.set_read_timeout(Some(DRAIN));
        let mut scratch = [0u8; 1024];
        let mut reader = &*guard;
        while reader.read(&mut scratch).map(|read| read > 0).unwrap_or(false) {}
    }

    // —— 写 ——//

    fn write_all(&self, bytes: &[u8]) -> std::io::Result<()> {
        let mut guard = self.write.lock().map_err(locked)?;
        guard.write_all(bytes)
    }

    /// 原样写字节（不带长度前缀）：旧式协议的整帧输出与文件数据块用这个。
    pub fn write_raw(&self, bytes: &[u8]) -> std::io::Result<()> {
        self.write_all(bytes)
    }

    pub fn write_i32(&self, value: i32) -> std::io::Result<()> {
        self.write_all(&value.to_be_bytes())
    }

    pub fn write_i64(&self, value: i64) -> std::io::Result<()> {
        self.write_all(&value.to_be_bytes())
    }

    pub fn write_u8(&self, value: u8) -> std::io::Result<()> {
        self.write_all(&[value])
    }

    pub fn write_bool(&self, value: bool) -> std::io::Result<()> {
        self.write_u8(u8::from(value))
    }

    /// 原版 `writeFloat` 是直接改写 IEEE754 位（与 DataEnc 的 *1000 语义不同）。
    pub fn write_f32_bits(&self, value: f32) -> std::io::Result<()> {
        self.write_i32(value.to_bits() as i32)
    }

    pub fn write_f64_bits(&self, value: f64) -> std::io::Result<()> {
        self.write_i64(f64::from_bits(value.to_bits()) as i64)
    }

    pub fn write_str(&self, value: &str) -> std::io::Result<()> {
        self.write_bytes(value.as_bytes())
    }

    pub fn write_bytes(&self, value: &[u8]) -> std::io::Result<()> {
        self.write_i32(value.len() as i32)?;
        self.write_all(value)
    }

    // —— 读 ——//

    fn read_exact(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut guard = self.read.lock().map_err(locked)?;
        guard.read_exact(buf)?;
        Ok(buf.len())
    }

    /// 原样读满 `buf`：旧式协议需要自己拼包头。
    pub fn read_into(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.read_exact(buf)
    }

    pub fn read_i32(&self) -> std::io::Result<i32> {
        let mut buf = [0u8; 4];
        self.read_exact(&mut buf)?;
        Ok(i32::from_be_bytes(buf))
    }

    pub fn read_i64(&self) -> std::io::Result<i64> {
        let mut buf = [0u8; 8];
        self.read_exact(&mut buf)?;
        Ok(i64::from_be_bytes(buf))
    }

    pub fn read_u8(&self) -> std::io::Result<u8> {
        let mut buf = [0u8; 1];
        self.read_exact(&mut buf)?;
        Ok(buf[0])
    }

    pub fn read_bool(&self) -> std::io::Result<bool> {
        Ok(self.read_u8()? != 0)
    }

    pub fn read_f32_bits(&self) -> std::io::Result<f32> {
        Ok(f32::from_bits(self.read_i32()? as u32))
    }

    pub fn read_f64_bits(&self) -> std::io::Result<f64> {
        Ok(f64::from_bits(self.read_i64()? as u64))
    }

    /// 长度前缀字符串；负长度按空串处理（与原版 `readString` 一致）。
    pub fn read_str(&self) -> std::io::Result<String> {
        Ok(String::from_utf8_lossy(&self.read_bytes_vec()?).into_owned())
    }

    pub fn read_bytes_vec(&self) -> std::io::Result<Vec<u8>> {
        let len = self.read_i32()?;
        if len <= 0 {
            return Ok(Vec::new());
        }
        let mut buf = vec![0u8; len as usize];
        self.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// 读满 `buf[..len]`，返回实际读取长度。
    pub fn read_fully(&self, buf: &mut [u8], len: usize) -> std::io::Result<usize> {
        let len = len.min(buf.len());
        self.read_exact(&mut buf[..len])
    }

    /// 跳过 `n` 字节。
    pub fn skip(&self, n: usize) -> std::io::Result<()> {
        let mut left = n;
        let mut scratch = [0u8; 1024];
        while left > 0 {
            let take = left.min(scratch.len());
            self.read_exact(&mut scratch[..take])?;
            left -= take;
        }
        Ok(())
    }

    /// 把对端已经写好的整帧（含 12 字节包头）读出来，用于旧版协议。
    pub fn read_frame(&self, header: &mut [u8; 12], body: &mut Vec<u8>) -> std::io::Result<usize> {
        self.read_exact(header)?;
        let len = i32::from_be_bytes(header[8..12].try_into().unwrap_or([0; 4]));
        if len <= 0 {
            body.clear();
            return Ok(0);
        }
        body.resize(len as usize, 0);
        self.read_exact(body)
    }
}

fn locked(error: std::sync::PoisonError<std::sync::MutexGuard<'_, TcpStream>>) -> std::io::Error {
    std::io::Error::other(format!("连接锁被污染: {}", error))
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use super::*;

    fn pair() -> (Conn, Conn) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = TcpStream::connect(address).unwrap();
        let (server, _) = listener.accept().unwrap();
        (Conn::new(client).unwrap(), Conn::new(server).unwrap())
    }

    #[test]
    fn fields_round_trip_over_tcp() {
        let (a, b) = pair();
        a.write_i32(1101).unwrap();
        a.write_bool(true).unwrap();
        a.write_str("{\"files\":[]}").unwrap();
        assert_eq!(b.read_i32().unwrap(), 1101);
        assert!(b.read_bool().unwrap());
        assert_eq!(b.read_str().unwrap(), "{\"files\":[]}");
    }

    #[test]
    fn negative_string_length_reads_empty() {
        let (a, b) = pair();
        a.write_i32(-1).unwrap();
        assert_eq!(b.read_str().unwrap(), "");
    }

    /// 服务端只写响应、不读多余的请求体，客户端仍应能读到完整响应而不是连接被重置。
    #[test]
    fn graceful_close_delivers_response_after_unread_body() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(address).unwrap();
        let (server, _) = listener.accept().unwrap();
        let conn = Conn::new(server).unwrap();
        client.write_all(b"POST /x HTTP/1.1\r\nContent-Length: 4096\r\n\r\n").unwrap();
        client.write_all(&[7u8; 4096]).unwrap();
        conn.write_all(b"HTTP/1.1 302 Found\r\nContent-Length: 0\r\n\r\n").unwrap();
        conn.graceful_close();
        let mut echoed = String::new();
        client.read_to_string(&mut echoed).unwrap();
        assert!(echoed.starts_with("HTTP/1.1 302"), "{echoed}");
    }

    /// 排干有超时兜底：对端不发 FIN 也不会永久卡住。
    #[test]
    fn graceful_close_gives_up_when_peer_keeps_the_socket_open() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = TcpStream::connect(address).unwrap();
        let (server, _) = listener.accept().unwrap();
        let conn = Conn::new(server).unwrap();
        let cost = std::time::Instant::now();
        conn.graceful_close();
        assert!(cost.elapsed() < std::time::Duration::from_secs(6), "排干没有超时兜底");
        drop(client);
    }

    #[test]
    fn chunked_binary_round_trip() {
        let (a, b) = pair();
        let payload: Vec<u8> = (0..4096u32).map(|i| (i % 251) as u8).collect();
        a.write_i32(payload.len() as i32).unwrap();
        a.write_all(&payload).unwrap();
        // 文件数据块就是「先读长度、再按长度读满」，不经过长度前缀解包
        let len = b.read_i32().unwrap() as usize;
        assert_eq!(len, payload.len());
        let mut buf = vec![0u8; payload.len()];
        let read = b.read_fully(&mut buf, payload.len()).unwrap();
        assert_eq!(read, payload.len());
        assert_eq!(buf, payload);
    }
}
