//! LANShare 线协议：包头/包体的编解码与混淆。
//!
//! 对应原版 C++ 的 `DataEnc` / `DataDec` / `ByteUtils`：
//! - 包头 12 字节：`[cmd:i32BE][count:i32BE][len:i32BE]`，`len` 为包体字节数；
//! - 字符串：`[i32BE 长度][UTF-8 裸字节]`，没有结束符；
//! - float 写成 `int(v * 1000)`，double 写成 `long(v * 1e6)`；
//! - 混淆只对 UDP 与旧版 TCP 生效：发送 `b' = ((b - 1) ^ 0x45)`，接收 `b = ((b' ^ 0x45) + 1)`。

/// 包头长度（cmd + count + len）。
pub const HEADER_LEN: usize = 12;
/// 协议魔数，UDP 报文与 TCP 连接首 4 字节都是它。
pub const MAGIC: i32 = 0x6667_7371;

// —— 命令字 ——————————————————————————————————————————————

pub const UDP_GET_DEVICES: i32 = 1001;
pub const UDP_SET_DEVICES: i32 = 1002;
pub const UDP_DEVICE_OFF_LINE: i32 = 1003;
pub const UDP_MESSAGE: i32 = 1004;
pub const UDP_MESSAGE_TO_CLIPBOARD: i32 = 1005;
pub const UDP_SEND_MEDIA_MUTE: i32 = 1006;
pub const UDP_SEND_MEDIA_RESTORE: i32 = 1007;

pub const FS_SHARE_FILE: i32 = 1101;
pub const FS_AGREE: i32 = 1102;
pub const FS_NOT_AGREE: i32 = 1103;
pub const FS_ADD_DEVICE: i32 = 1104;
pub const FS_MESSAGE: i32 = 1105;
pub const FS_GET_NO_SYNC_MEDIA: i32 = 1106;

/// 新版 TCP 首字段：-2 表示 V4，-3 表示 V5。
pub const NEW_VERSION_4: i32 = -2;
pub const NEW_VERSION_5: i32 = -3;
/// 新版文件流中止整个传输。
pub const NEW_FS_BREAK: i32 = -2;

/// 旧版单字节命令字（旧文件流的帧头）。
pub const FS_DATA: u8 = 1;
pub const FS_END: u8 = 2;
pub const FS_CLOSE: u8 = 3;
pub const FS_DATA_RECEIVED: u8 = 4;
pub const FS_NEXT: u8 = 5;
pub const FS_BREAK: u8 = 6;

/// 协议版本。
pub const DATA_VERSION_3: i32 = 3;
pub const DATA_VERSION_4: i32 = 4;
pub const DATA_VERSION_5: i32 = 5;
/// 当前实现使用的协议版本。
pub const DATA_VERSION: i32 = DATA_VERSION_5;

/// 文件类型（旧版协议里的 `fileType` 取值）。
pub const FILE_IMAGE: i32 = 3001;
pub const FILE_VIDEO: i32 = 3002;
pub const FILE_FILE: i32 = 3003;
pub const FILE_FOLDER: i32 = 3004;
/// fileTransferJson 里的 `fileType` 取值。
///
/// 必须与 Java 版 `MessageFileContent` / Qt 版 `FileServer.h` 的宏定义逐一对齐：
/// 对端按 `IMAGE(1)`/`VIDEO(2)` 建媒体气泡、按 `FOLDER(6)` 走文件夹分支，
/// 写成 1/2 会被手机当成「图片/视频」，文件夹分支更是永远命不中。
pub const FILE_TYPE_IMAGE: i32 = 0x1;
pub const FILE_TYPE_VIDEO: i32 = 0x2;
pub const FILE_TYPE_AUDIO: i32 = 0x3;
pub const FILE_TYPE_APK: i32 = 0x4;
pub const FILE_TYPE_FILE: i32 = 0x5;
pub const FILE_TYPE_FOLDER: i32 = 0x6;

/// 发送方向的混淆：对已写出的全部字节原地变换。
pub fn enc_bytes(buf: &mut [u8]) {
    for byte in buf {
        *byte = byte.wrapping_sub(1) ^ 0x45;
    }
}

/// 接收方向的混淆逆变换。
pub fn dec_bytes(buf: &mut [u8]) {
    for byte in buf {
        *byte = (*byte ^ 0x45).wrapping_add(1);
    }
}

/// 按原版字段顺序构造一个包：写端口、IP、名称、类型、唯一码、协议版本、电量、充电状态。
///
/// UDP 心跳、上线/下线通知、旧版 TCP 首包都复用这个顺序。
pub fn write_device_fields(packet: &mut Packet, port: i32, ip: &str, name: &str, mode: i32, uuid: &str, version: i32, battery: i32, charge: u8) {
    packet.i32(port);
    packet.string(ip);
    packet.string(name);
    packet.i32(mode);
    packet.string(uuid);
    packet.i32(version);
    packet.i32(battery);
    packet.u8(charge);
}

/// 包写入器：先写包体，产出时补上长度并按需混淆。
pub struct Packet {
    cmd: i32,
    count: i32,
    body: Vec<u8>,
}

impl Packet {
    pub fn new(cmd: i32, count: i32) -> Self {
        Self { cmd, count, body: Vec::new() }
    }

    /// 改写命令字（原版 `setCmd` 是在固定偏移写，与写入顺序无关）。
    pub fn set_cmd(&mut self, cmd: i32) {
        self.cmd = cmd;
    }

    pub fn set_count(&mut self, count: i32) {
        self.count = count;
    }

    pub fn body_len(&self) -> usize {
        self.body.len()
    }

    pub fn i32(&mut self, value: i32) -> &mut Self {
        self.body.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub fn i64(&mut self, value: i64) -> &mut Self {
        self.body.extend_from_slice(&value.to_be_bytes());
        self
    }

    pub fn u8(&mut self, value: u8) -> &mut Self {
        self.body.push(value);
        self
    }

    pub fn boolean(&mut self, value: bool) -> &mut Self {
        self.u8(u8::from(value))
    }

    /// 与原版一致：`int(v * 1000)`。
    pub fn f32(&mut self, value: f32) -> &mut Self {
        self.i32((value * 1000.0) as i32)
    }

    /// 与原版一致：`long(v * 1e6)`。
    pub fn f64(&mut self, value: f64) -> &mut Self {
        self.i64((value * 1_000_000.0) as i64)
    }

    pub fn string(&mut self, value: &str) -> &mut Self {
        self.bytes(value.as_bytes())
    }

    /// 长度前缀的裸字节。
    pub fn bytes(&mut self, value: &[u8]) -> &mut Self {
        self.i32(value.len() as i32);
        self.body.extend_from_slice(value);
        self
    }

    fn header(&self) -> [u8; HEADER_LEN] {
        let mut header = [0u8; HEADER_LEN];
        header[0..4].copy_from_slice(&self.cmd.to_be_bytes());
        header[4..8].copy_from_slice(&self.count.to_be_bytes());
        header[8..12].copy_from_slice(&(self.body.len() as i32).to_be_bytes());
        header
    }

    /// 明文完整报文（包头 + 包体），旧版 TCP 直接发这份。
    pub fn plain(&self) -> Vec<u8> {
        let mut out = self.header().to_vec();
        out.extend_from_slice(&self.body);
        out
    }

    /// 混淆后的完整报文，UDP 与旧版应答用这份。
    pub fn encoded(&self) -> Vec<u8> {
        let mut out = self.plain();
        enc_bytes(&mut out);
        out
    }

    /// 带魔数头的 UDP 报文。
    pub fn udp(&self) -> Vec<u8> {
        let mut out = MAGIC.to_be_bytes().to_vec();
        out.extend_from_slice(&self.encoded());
        out
    }
}

/// 包读取器：包头已解出，包体按字段顺序读取；越界时返回零值而不是报错，与原版一致。
pub struct Payload {
    buf: Vec<u8>,
    pos: usize,
}

impl Payload {
    /// 从 UDP 报文（含魔数）解出包体。
    pub fn from_udp(raw: &[u8]) -> Option<(i32, Self)> {
        let magic = i32::from_be_bytes(raw.get(0..4)?.try_into().ok()?);
        if magic != MAGIC {
            return None;
        }
        Self::from_obfuscated(&raw[4..])
    }

    /// 从混淆后的 `header + body` 解出包体。
    pub fn from_obfuscated(raw: &[u8]) -> Option<(i32, Self)> {
        let mut buf = raw.to_vec();
        dec_bytes(&mut buf);
        Some(Self::split(buf))
    }

    /// 从未混淆的 `header + body` 解出包体（旧版 TCP 的帧不混淆）。
    pub fn from_plain(raw: &[u8]) -> Option<(i32, Self)> {
        Some(Self::split(raw.to_vec()))
    }

    fn split(buf: Vec<u8>) -> (i32, Self) {
        let cmd = read_i32(&buf, 0);
        let header = HEADER_LEN.min(buf.len());
        (cmd, Self { buf, pos: header })
    }

    pub fn cmd(&self) -> i32 {
        read_i32(&self.buf, 0)
    }

    pub fn count(&self) -> i32 {
        read_i32(&self.buf, 4)
    }

    pub fn len(&self) -> i32 {
        read_i32(&self.buf, 8)
    }

    /// 包头之后已读取的字节数。
    pub fn index(&self) -> usize {
        self.pos - HEADER_LEN.min(self.buf.len())
    }

    /// 跳到包体指定偏移继续读。
    pub fn seek(&mut self, offset: usize) {
        self.pos = HEADER_LEN.min(self.buf.len()) + offset;
    }

    pub fn i32(&mut self) -> i32 {
        let value = read_i32(&self.buf, self.pos);
        self.pos += 4;
        value
    }

    pub fn i64(&mut self) -> i64 {
        let value = read_i64(&self.buf, self.pos);
        self.pos += 8;
        value
    }

    /// 原版在不足 8 字节时用默认值兜底（旧版图片/视频包里的 mediaId）。
    pub fn i64_or(&mut self, default: i64) -> i64 {
        if self.pos + 8 <= self.buf.len() {
            self.i64()
        } else {
            default
        }
    }

    pub fn u8(&mut self) -> u8 {
        let value = self.buf.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        value
    }

    pub fn boolean(&mut self) -> bool {
        self.u8() != 0
    }

    pub fn f32(&mut self) -> f32 {
        self.i32() as f32 / 1000.0
    }

    pub fn f64(&mut self) -> f64 {
        self.i64() as f64 / 1_000_000.0
    }

    pub fn string(&mut self) -> String {
        let bytes = self.raw();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// 长度前缀的裸字节。
    pub fn raw(&mut self) -> Vec<u8> {
        let len = self.i32();
        if len <= 0 {
            return Vec::new();
        }
        let len = len as usize;
        let bytes = self.buf.get(self.pos..self.pos + len).unwrap_or(&[]).to_vec();
        self.pos += len;
        bytes
    }

    /// 剩余全部字节。
    pub fn surplus(&mut self) -> Vec<u8> {
        let bytes = self.buf.get(self.pos..).unwrap_or(&[]).to_vec();
        self.pos = self.buf.len();
        bytes
    }
}

fn read_i32(buf: &[u8], at: usize) -> i32 {
    match buf.get(at..at + 4) {
        Some(slice) => i32::from_be_bytes(slice.try_into().unwrap_or([0; 4])),
        None => 0,
    }
}

fn read_i64(buf: &[u8], at: usize) -> i64 {
    match buf.get(at..at + 8) {
        Some(slice) => i64::from_be_bytes(slice.try_into().unwrap_or([0; 8])),
        None => 0,
    }
}

/// 把设备字段按原版顺序写进 UDP 报文，并附上魔数。
pub fn udp_device_packet(cmd: i32, port: i32, ip: &str, name: &str, mode: i32, uuid: &str, battery: i32, charge: u8) -> Vec<u8> {
    let mut packet = Packet::new(cmd, 0);
    // 原版固定写 DATA_VERSION_3，真实版本靠 uuid 的 "-<version>" 后缀传递。
    write_device_fields(&mut packet, port, ip, name, mode, uuid, DATA_VERSION_3, battery, charge);
    packet.udp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obfuscation_round_trip() {
        let mut plain = vec![0x00, 0x01, 0x44, 0x45, 0xBA, 0xFF, 0x7F];
        let original = plain.clone();
        enc_bytes(&mut plain);
        assert_ne!(plain, original);
        dec_bytes(&mut plain);
        assert_eq!(plain, original);
    }

    #[test]
    fn obfuscation_matches_cpp_formula() {
        // (b - 1) ^ 0x45，逐字节与 C++ 的 unsigned char 运算等价
        let mut buf = vec![0x00, 0x01, 0x80, 0xFF];
        enc_bytes(&mut buf);
        assert_eq!(buf, vec![0xBA, 0x45, 0x3A, 0xBB]);
        dec_bytes(&mut buf);
        assert_eq!(buf, vec![0x00, 0x01, 0x80, 0xFF]);
    }

    #[test]
    fn header_layout_and_length() {
        let mut packet = Packet::new(UDP_MESSAGE, 0);
        packet.i32(5856).string("192.168.1.5");
        let plain = packet.plain();
        assert_eq!(read_i32(&plain, 0), UDP_MESSAGE);
        assert_eq!(read_i32(&plain, 4), 0);
        // len = 4(端口) + 4(长度前缀) + 11(ip)
        assert_eq!(read_i32(&plain, 8), 19);
        assert_eq!(plain.len(), HEADER_LEN + 19);
    }

    #[test]
    fn udp_packet_matches_cpp_field_order() {
        // 与 C++ makeUdpDataEnc 的字节序列逐字节对齐的固定样本
        let mut expected = Packet::new(UDP_GET_DEVICES, 0);
        write_device_fields(&mut expected, 5856, "192.168.1.7", "PC", 2, "uuid-5", DATA_VERSION_3, 88, 1);
        let expected = expected.udp();
        let bytes = udp_device_packet(UDP_GET_DEVICES, 5856, "192.168.1.7", "PC", 2, "uuid-5", 88, 1);
        assert_eq!(read_i32(&bytes, 0), MAGIC);
        assert_eq!(bytes, expected);

        let (cmd, mut payload) = Payload::from_udp(&bytes).expect("魔数与包头必须可解析");
        assert_eq!(cmd, UDP_GET_DEVICES);
        assert_eq!(payload.i32(), 5856);
        assert_eq!(payload.string(), "192.168.1.7");
        assert_eq!(payload.string(), "PC");
        assert_eq!(payload.i32(), 2);
        assert_eq!(payload.string(), "uuid-5");
        assert_eq!(payload.i32(), DATA_VERSION_3);
        assert_eq!(payload.i32(), 88);
        assert_eq!(payload.u8(), 1);
    }

    #[test]
    fn string_field_round_trip_with_utf8() {
        let mut packet = Packet::new(FS_MESSAGE, 0);
        packet.string("你好，局域网");
        let (cmd, mut payload) = Payload::from_plain(&packet.plain()).unwrap();
        assert_eq!(cmd, FS_MESSAGE);
        assert_eq!(payload.string(), "你好，局域网");
    }

    #[test]
    fn float_and_double_scale() {
        let mut packet = Packet::new(0, 0);
        packet.f32(1.2345).f64(9.876543);
        let (_, mut payload) = Payload::from_plain(&packet.plain()).unwrap();
        assert_eq!(payload.i32(), 1234);
        assert_eq!(payload.i64(), 9_876_543);
        payload.seek(0);
        assert!((payload.f32() - 1.234).abs() < 1e-6);
        assert!((payload.f64() - 9.876543).abs() < 1e-6);
    }

    #[test]
    fn short_read_returns_zero_like_cpp() {
        // 原版 DataDec 越界时返回 0 而不是报错
        let (_, mut payload) = Payload::from_plain(&[1, 2, 3]).unwrap();
        assert_eq!(payload.i32(), 0);
        assert_eq!(payload.u8(), 0);
        assert!(payload.string().is_empty());
        assert_eq!(payload.i64_or(-1), -1);
    }
}
