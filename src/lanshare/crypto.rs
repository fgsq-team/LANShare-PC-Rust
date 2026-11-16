//! 加密相关：AES-256-ECB/PKCS7、消息密钥派生、数据块混淆、稳定 ID、UUID。
//!
//! 逐字节对齐原版 `QAESEncryption`（AES-256 / ECB / PKCS7）与 `mUtils`：
//! - 用户密钥不足 32 字节时**右侧补零**，超过则取前 32 字节（`expandKey` 的行为）；
//! - 消息密文先 AES 再 hex 编码放进字符串字段；
//! - 配置里的 `messageKey` 本身是用固定密钥 `KEY` AES 解密后得到的明文密钥。

use aes::Aes256;
use cipher::{Block, BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use md5::{Digest, Md5};

/// 固定密钥：解密配置里的 messageKey（原版 `#define KEY`）。
pub const KEY: &str = "6c9b%8ErII@Rc&f";
/// 默认的消息密钥密文（原版 `#define DEFAULT_MESSAGE_KEY`，96 位 hex）。
pub const DEFAULT_MESSAGE_KEY: &str = "e4be1373272c69e0932651d97187b746c6725b17bbe84ad0b0fe2d4e81fc1d6c0c633d8ebd7f0fea65a57a9d5529d214";
/// 稳定 ID 的种子前缀。
pub const RESUME_SEED_PREFIX: &str = "LRESUME:";

const BLOCK: usize = 16;

/// 原版 `expandKey`：右侧补零到 32 字节，超长取前 32 字节。
pub fn expand_key(key: &[u8]) -> [u8; 32] {
    let mut expanded = [0u8; 32];
    let take = key.len().min(32);
    expanded[..take].copy_from_slice(&key[..take]);
    expanded
}

fn cipher(key: &[u8]) -> Aes256 {
    Aes256::new_from_slice(&expand_key(key)).expect("AES-256 密钥补齐后必定是 32 字节")
}

/// PKCS7 填充。
fn pad(data: &mut Vec<u8>) {
    let size = BLOCK - data.len() % BLOCK;
    data.resize(data.len() + size, size as u8);
}

/// PKCS7 去填充，非法填充返回 `None`（原版只盲信最后一字节，这里避免 panic）。
fn unpad(data: &mut Vec<u8>) -> Option<()> {
    let size = *data.last()?;
    if size == 0 || size as usize > BLOCK || size as usize > data.len() {
        return None;
    }
    let start = data.len() - size as usize;
    if data[start..].iter().all(|byte| *byte == size) {
        data.truncate(start);
        Some(())
    } else {
        None
    }
}

/// AES-256-ECB 加密，明文按 PKCS7 填充。
pub fn ecb_encrypt(plain: &[u8], key: &[u8]) -> Vec<u8> {
    let cipher = cipher(key);
    let mut data = plain.to_vec();
    pad(&mut data);
    for chunk in data.chunks_mut(BLOCK) {
        let mut block = block_of(chunk);
        cipher.encrypt_block(&mut block);
        chunk.copy_from_slice(&block);
    }
    data
}

/// 16 字节的切片转成固定长度的块（调用方保证已对齐到 BLOCK）。
fn block_of(chunk: &[u8]) -> Block<Aes256> {
    let mut raw = [0u8; BLOCK];
    let take = chunk.len().min(BLOCK);
    raw[..take].copy_from_slice(&chunk[..take]);
    Block::<Aes256>::from(raw)
}

/// AES-256-ECB 解密并去 PKCS7 填充。
pub fn ecb_decrypt(data: &[u8], key: &[u8]) -> Option<Vec<u8>> {
    let cipher = cipher(key);
    if data.is_empty() || data.len() % BLOCK != 0 {
        return None;
    }
    let mut data = data.to_vec();
    for chunk in data.chunks_mut(BLOCK) {
        let mut block = block_of(chunk);
        cipher.decrypt_block(&mut block);
        chunk.copy_from_slice(&block);
    }
    unpad(&mut data)?;
    Some(data)
}

/// 小写 hex 编码。
pub fn hex_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 2);
    for byte in data {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// 十六进制解码，容忍奇数长度与非法字符（返回已解析部分）。
pub fn hex_decode(text: &str) -> Vec<u8> {
    let digits: Vec<u8> = text
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .map(|byte| match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => 255,
        })
        .collect();
    let mut out = Vec::with_capacity(digits.len() / 2);
    let mut index = 0;
    while index + 1 < digits.len() {
        let high = digits[index];
        let low = digits[index + 1];
        if high == 255 || low == 255 {
            break;
        }
        out.push((high << 4) | low);
        index += 2;
    }
    out
}

/// 消息文本加密：AES-256-ECB/PKCS7 后 hex 编码（原版 `mUtils::encMessage`）。
pub fn enc_message(text: &str, key: &str) -> String {
    hex_encode(&ecb_encrypt(text.as_bytes(), key.as_bytes()))
}

/// 消息文本解密（原版 `mUtils::decMessage`），失败返回 `None`。
pub fn dec_message(encoded: &str, key: &str) -> Option<String> {
    let plain = ecb_decrypt(&hex_decode(encoded), key.as_bytes())?;
    Some(String::from_utf8_lossy(&plain).into_owned())
}

/// 把配置项里的 messageKey 还原成真实密钥：hex 解码后用固定 `KEY` AES 解密去填充。
pub fn derive_message_key(config_value: &str) -> String {
    let value = if config_value.is_empty() { DEFAULT_MESSAGE_KEY } else { config_value };
    match ecb_decrypt(&hex_decode(value), KEY.as_bytes()) {
        Some(plain) => String::from_utf8_lossy(&plain).into_owned(),
        None => String::new(),
    }
}

/// 文件数据块混淆：`b' = ((b - 1) ^ ((index + j) & 0xFF))`，`index` 是该文件内的绝对偏移。
pub fn enc_data(buf: &mut [u8], index: i64) {
    for (offset, byte) in buf.iter_mut().enumerate() {
        let key = (index + offset as i64) & 0xFF;
        *byte = byte.wrapping_sub(1) ^ (key as u8);
    }
}

/// 文件数据块解混淆。
pub fn dec_data(buf: &mut [u8], index: i64) {
    for (offset, byte) in buf.iter_mut().enumerate() {
        let key = (index + offset as i64) & 0xFF;
        *byte = (*byte ^ (key as u8)).wrapping_add(1);
    }
}

/// 续传稳定 ID：`md5("LRESUME:" + seed)` 的小写 hex。
pub fn stable_id(seed: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(RESUME_SEED_PREFIX.as_bytes());
    hasher.update(seed.as_bytes());
    hex_encode(&hasher.finalize())
}

/// 随机 ID：32 位小写 hex，与原版 `Utils::getUUID()`（去掉大括号和连字符的 QUuid）同形。
pub fn new_id() -> String {
    let random = uuid_bytes();
    let mut bytes = [random[0]; 16];
    bytes.copy_from_slice(&random);
    // 版本 4 与变体位，保持和 UUID v4 一致
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    let mut hex = [0u8; 32];
    for (index, byte) in bytes.iter().enumerate() {
        hex[index * 2] = to_hex(byte >> 4);
        hex[index * 2 + 1] = to_hex(byte & 0x0F);
    }
    String::from_utf8(hex.to_vec()).unwrap_or_default()
}

fn to_hex(nibble: u8) -> u8 {
    if nibble < 10 { b'0' + nibble } else { b'a' + nibble - 10 }
}

/// 16 字节随机数：优先用系统熵源，退化时用时钟与线程地址混合。
fn uuid_bytes() -> [u8; 16] {
    #[cfg(windows)]
    {
        // RtlGenRandom（advapi32 的 SystemFunction036）不依赖额外 crate，直接 P/Invoke
        #[link(name = "advapi32")]
        unsafe extern "system" {
            fn SystemFunction036(buffer: *mut u8, len: u32) -> u8;
        }
        let mut buf = [0u8; 16];
        let ok = unsafe { SystemFunction036(buf.as_mut_ptr(), 16) };
        if ok != 0 {
            return buf;
        }
        return weak_random();
    }
    #[cfg(not(windows))]
    {
        use std::io::Read;
        // 类 Unix 上 /dev/urandom 就是系统熵源，读不到（沙箱、奇怪的内核）再退到弱随机
        let mut buf = [0u8; 16];
        if let Ok(mut file) = std::fs::File::open("/dev/urandom") {
            if file.read_exact(&mut buf).is_ok() {
                return buf;
            }
        }
        weak_random()
    }
}

fn weak_random() -> [u8; 16] {
    let tick = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos() as u64)
        .unwrap_or(0);
    let pointer = &tick as *const u64 as u64;
    let mut state = tick ^ pointer.rotate_left(32);
    let mut buf = [0u8; 16];
    for byte in buf.iter_mut() {
        // xorshift64，仅作退化兜底
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = (state >> 24) as u8;
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_expansion_pads_on_the_right() {
        let expanded = expand_key(b"1234");
        assert_eq!(&expanded[..4], b"1234");
        assert!(expanded[4..].iter().all(|byte| *byte == 0));

        let long = expand_key(&[7u8; 40]);
        assert_eq!(long, [7u8; 32]);
    }

    #[test]
    fn ecb_round_trip_with_pkcs7() {
        for text in ["", "a", &"x".repeat(16), &"y".repeat(17), "局域网文件传输"] {
            let encrypted = ecb_encrypt(text.as_bytes(), b"k");
            assert_eq!(encrypted.len() % BLOCK, 0);
            let decrypted = ecb_decrypt(&encrypted, b"k").expect("填充必须合法");
            assert_eq!(String::from_utf8(decrypted).unwrap(), text);
        }
    }

    #[test]
    fn message_round_trip_is_hex() {
        let key = "0123456789abcdef";
        let encoded = enc_message("你好", key);
        assert!(encoded.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(dec_message(&encoded, key).as_deref(), Some("你好"));
    }

    #[test]
    fn default_message_key_derives_to_readable_text() {
        // 默认配置值必须能解出真实密钥；语义（AES-256-ECB/PKCS7 + 密钥右补零）稍有偏差，
        // 解出来的字节就不可能是这段可打印 ASCII
        let derived = derive_message_key(DEFAULT_MESSAGE_KEY);
        assert!(!derived.is_empty(), "默认密钥应能解密");
        assert!(derived.len() >= 32, "真实密钥长度应不小于 32: {}", derived.len());
        assert!(derived.bytes().all(|byte| byte.is_ascii_graphic()), "派生密钥应可打印: {derived:?}");
        // 空配置回落到默认常量
        assert_eq!(derive_message_key(""), derived);
    }

    #[test]
    fn data_obfuscation_matches_cpp_formula() {
        let mut buf = b"ABCDEF".to_vec();
        let original = buf.clone();
        enc_data(&mut buf, 0);
        assert_ne!(buf, original);
        dec_data(&mut buf, 0);
        assert_eq!(buf, original);

        // index 跨 256 边界时按 & 0xFF 取低字节
        let mut single = vec![0x00u8];
        enc_data(&mut single, 255);
        assert_eq!(single, vec![0xFF ^ 0xFF]);
        dec_data(&mut single, 255);
        assert_eq!(single, vec![0x00]);
    }

    #[test]
    fn stable_id_matches_md5_of_prefixed_seed() {
        // md5("LRESUME:seed") 的期望值，用独立实现算出后固化，防止前缀或大小写回归
        assert_eq!(stable_id("seed"), "40b7682ab4ac8a2d9662aa402f2e4f53");
        assert_eq!(stable_id("sender|C:/a.txt|123").len(), 32);
        assert!(stable_id("a").chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(stable_id("seed"), stable_id("seed"));
        assert_ne!(stable_id("seed"), stable_id("seed2"));
    }

    #[test]
    fn generated_ids_look_like_uuidv4_without_dashes() {
        let id = new_id();
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(&id[12..13], "4");
        assert_ne!(new_id(), new_id());
    }

    #[test]
    fn hex_decode_tolerates_bad_input() {
        assert_eq!(hex_decode("414243"), b"ABC");
        assert_eq!(hex_decode("41 42"), b"AB");
        assert_eq!(hex_decode("zz"), Vec::<u8>::new());
    }
}
