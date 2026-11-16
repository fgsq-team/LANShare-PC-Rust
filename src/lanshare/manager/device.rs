//! 设备模型与在线设备表，对应原版 `Device` + `DeviceManager` 的数据部分。

use serde::{Deserialize, Serialize};

use super::proto::Payload;

/// 未知设备类型。
pub const MODE_UNKNOWN: i32 = -1;
/// 安卓。
pub const MODE_ANDROID: i32 = 1;
/// Windows。
pub const MODE_WIN: i32 = 2;
/// Linux。
pub const MODE_LINUX: i32 = 3;
/// macOS。
pub const MODE_MAC: i32 = 4;
/// iOS。
pub const MODE_IOS: i32 = 5;

/// 心跳超时：20 秒没收到包就认为下线（与原版一致）。
pub const HEARTBEAT_TIMEOUT_MS: i64 = 20 * 1000;

/// 毫秒时间戳，原版 `TimeTools::getCurrentTime()` 同为 epoch 毫秒。
pub fn now_ms() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_millis() as i64,
        Err(_) => 0,
    }
}

fn default_true() -> bool {
    true
}

/// 一台局域网设备。序列化字段与原版 `Device::toJsonObject()` 完全一致。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    #[serde(rename = "devName", default)]
    pub name: String,
    /// 原版写出的是 `devIp`，而 `TcpProtocol` 解析时读的是 `devIP`（手机端写法），两个名字都要收。
    #[serde(rename = "devIp", alias = "devIP", default)]
    pub ip: String,
    #[serde(rename = "devNetMask", default)]
    pub net_mask: String,
    #[serde(rename = "devBrotIp", default)]
    pub broadcast_ip: String,
    #[serde(rename = "devPort", default)]
    pub port: i32,
    #[serde(rename = "devMode", default)]
    pub mode: i32,
    #[serde(rename = "dataVersion", default)]
    pub data_version: i32,
    #[serde(rename = "uniqueUUid", default)]
    pub uuid: String,
    #[serde(rename = "batteryLevel", default)]
    pub battery: i32,
    #[serde(rename = "chargeStatus", default)]
    pub charge: i32,
    /// 扫码添加的设备为 `false`，不参与心跳超时清理。
    #[serde(rename = "canRemove", default = "default_true")]
    pub can_remove: bool,
    /// 最后心跳时间，原版不入库，只在内存里用。
    #[serde(skip, default)]
    pub last_seen: i64,
    /// 网卡流量，用于本机网卡排序，远端设备恒为 0。
    #[serde(skip, default)]
    pub traffic: i64,
}

impl Default for Device {
    fn default() -> Self {
        Self {
            name: String::new(),
            ip: String::new(),
            net_mask: String::new(),
            broadcast_ip: String::new(),
            port: 0,
            mode: MODE_UNKNOWN,
            data_version: 0,
            uuid: String::new(),
            battery: -1,
            charge: -1,
            can_remove: true,
            last_seen: 0,
            traffic: 0,
        }
    }
}

impl Device {
    /// 在线表的键：`ip:port`，与原版 `DeviceManager` 一致。
    pub fn key(&self) -> String {
        format!("{}:{}", self.ip, self.port)
    }

    /// 协议是否支持新版 TCP（>= 4 才能走 V4/V5 文件流）。
    pub fn is_new_version(&self) -> bool {
        self.data_version >= super::proto::DATA_VERSION_4
    }

    /// 按 UDP 包体字段顺序解出设备，并套用「uuid 带 `-` 时后半段是协议版本」的规则。
    pub fn from_payload(payload: &mut Payload, seen: i64) -> Self {
        let port = payload.i32();
        let ip = payload.string();
        let name = payload.string();
        let mode = payload.i32();
        let uuid = payload.string();
        let mut data_version = payload.i32();
        let battery = payload.i32();
        let charge = payload.u8() as i32;
        let mut uuid = uuid;
        if let Some((head, tail)) = uuid.split_once('-') {
            if let Ok(version) = tail.trim().parse::<i32>() {
                data_version = version;
            }
            uuid = head.to_string();
        }
        Self {
            name,
            ip,
            net_mask: String::new(),
            broadcast_ip: String::new(),
            port,
            mode,
            data_version,
            uuid,
            battery,
            charge,
            can_remove: true,
            last_seen: seen,
            traffic: 0,
        }
    }

    /// TCP 握手用的设备 JSON。原版接收端读 `devIP`，发送端写 `devIp`，这里两个键都写。
    pub fn handshake_json(&self) -> String {
        let mut value = match serde_json::to_value(self) {
            Ok(value) => value,
            Err(_) => return String::from("{}"),
        };
        if let Some(object) = value.as_object_mut() {
            if let Some(ip) = object.get("devIp").cloned() {
                object.insert("devIP".to_string(), ip);
            }
        }
        serde_json::to_string(&value).unwrap_or_default()
    }

    /// 从 TCP 握手的设备 JSON 还原设备；解析失败时返回可用设备（字段为空）。
    pub fn from_handshake_json(text: &str) -> Self {
        let value = serde_json::from_str::<serde_json::Value>(text).unwrap_or_default();
        Self::from_json_value(&value)
    }

    /// 同一个 JSON 对象的入口：先归一化 `devIP`/`devIp`，避免 serde 把两种拼写当成重复字段。
    pub fn from_json_value(value: &serde_json::Value) -> Self {
        let mut fixed = value.clone();
        if let Some(object) = fixed.as_object_mut() {
            if let Some(ip) = object.remove("devIP") {
                object.entry("devIp").or_insert(ip);
            }
        }
        let mut device: Device = serde_json::from_value(fixed).unwrap_or_default();
        if device.data_version < super::proto::DATA_VERSION_4 {
            // 原版把低于 4 的一律抬到 4，避免走不回新版分支
            device.data_version = super::proto::DATA_VERSION_4;
        }
        device
    }
}

/// 在线设备表。`Engine` 里用 `Mutex` 包一层。
#[derive(Debug, Default)]
pub struct DeviceTable {
    devices: std::collections::BTreeMap<String, Device>,
}

impl DeviceTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// 加入或覆盖设备；扫码设备（`can_remove == false`）的标记具有粘性。返回是否真的变化。
    pub fn add(&mut self, device: Device) -> bool {
        let key = device.key();
        let mut store = device;
        if let Some(existing) = self.devices.get(&key) {
            if !existing.can_remove {
                store.can_remove = false;
            }
            if existing == &store {
                return false;
            }
        }
        self.devices.insert(key, store);
        true
    }

    pub fn remove(&mut self, device: &Device) -> bool {
        self.devices.remove(&device.key()).is_some()
    }

    pub fn get(&self, ip: &str, port: i32) -> Option<&Device> {
        self.devices.get(&format!("{ip}:{port}"))
    }

    pub fn list(&self) -> Vec<Device> {
        self.devices.values().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    /// 清理心跳超时的设备，返回被移除的那些。
    pub fn cleanup_expired(&mut self, now: i64) -> Vec<Device> {
        let mut removed = Vec::new();
        self.devices.retain(|_, device| {
            if !device.can_remove {
                return true;
            }
            if now - device.last_seen > HEARTBEAT_TIMEOUT_MS {
                removed.push(device.clone());
                false
            } else {
                true
            }
        });
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::super::proto::{Packet, udp_device_packet, DATA_VERSION_3, UDP_GET_DEVICES};
    use super::*;

    #[test]
    fn device_json_round_trip_keeps_wire_keys() {
        let device = Device {
            name: "小明的手机".to_string(),
            ip: "192.168.1.9".to_string(),
            port: 5856,
            mode: MODE_ANDROID,
            data_version: 5,
            uuid: "abcd".to_string(),
            battery: 88,
            charge: 1,
            ..Default::default()
        };
        let text = device.handshake_json();
        assert!(text.contains("\"devIp\":\"192.168.1.9\""), "{text}");
        assert!(text.contains("\"devIP\":\"192.168.1.9\""), "{text}");
        assert!(text.contains("\"uniqueUUid\":\"abcd\""), "{text}");
        let back = Device::from_handshake_json(&text);
        assert_eq!(back.ip, device.ip);
        assert_eq!(back.name, device.name);
        assert_eq!(back.uuid, device.uuid);
        assert_eq!(back.data_version, device.data_version);
    }

    #[test]
    fn handshake_json_only_dev_ip_is_still_parsed() {
        // 手机端的 JSON 只有 devIP
        let device = Device::from_handshake_json(r#"{"devIP":"10.0.0.2","devName":"M","devPort":5856,"dataVersion":5}"#);
        assert_eq!(device.ip, "10.0.0.2");
        assert_eq!(device.name, "M");
        assert_eq!(device.port, 5856);
    }

    #[test]
    fn old_protocol_version_is_bumped_to_four() {
        let device = Device::from_handshake_json(r#"{"devIp":"10.0.0.2","dataVersion":3}"#);
        assert_eq!(device.data_version, super::super::proto::DATA_VERSION_4);
        assert!(device.is_new_version());
    }

    #[test]
    fn uuid_suffix_carries_the_real_protocol_version() {
        let bytes = udp_device_packet(UDP_GET_DEVICES, 5856, "192.168.1.9", "PC", MODE_WIN, "feedcafe-5", 70, 0);
        let (_, mut payload) = Payload::from_udp(&bytes).expect("报文可解析");
        let device = Device::from_payload(&mut payload, 123);
        assert_eq!(device.uuid, "feedcafe");
        assert_eq!(device.data_version, 5);
        assert_eq!(device.port, 5856);
        assert_eq!(device.battery, 70);
        assert_eq!(device.last_seen, 123);
    }

    #[test]
    fn plain_packet_without_version_suffix_keeps_header_version() {
        let mut packet = Packet::new(UDP_GET_DEVICES, 0);
        super::super::proto::write_device_fields(&mut packet, 1, "1.1.1.1", "n", 2, "nodash", DATA_VERSION_3, 5, 6);
        let (_, mut payload) = Payload::from_plain(&packet.plain()).expect("可解析");
        let device = Device::from_payload(&mut payload, 0);
        assert_eq!(device.uuid, "nodash");
        assert_eq!(device.data_version, DATA_VERSION_3);
    }

    #[test]
    fn table_key_is_ip_and_port_and_sticky_can_remove() {
        let mut table = DeviceTable::new();
        let mut device = Device { ip: "10.0.0.5".into(), port: 5856, ..Default::default() };
        assert!(table.add(device.clone()));
        device.can_remove = false;
        assert!(table.add(device.clone()));
        assert!(!table.add(device.clone()), "完全相同的设备不应报变化");
        // 普通心跳覆盖时也要保留永久在线标记
        let heartbeat = Device { ip: "10.0.0.5".into(), port: 5856, name: "改名".into(), ..Default::default() };
        assert!(table.add(heartbeat));
        assert!(!table.get("10.0.0.5", 5856).expect("在表里").can_remove);
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn expired_devices_are_purged_but_pinned_ones_survive() {
        let mut table = DeviceTable::new();
        let now = now_ms();
        table.add(Device { ip: "a".into(), port: 1, last_seen: now - 21_000, ..Default::default() });
        table.add(Device { ip: "b".into(), port: 1, last_seen: now - 21_000, can_remove: false, ..Default::default() });
        table.add(Device { ip: "c".into(), port: 1, last_seen: now, ..Default::default() });
        let removed = table.cleanup_expired(now);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].ip, "a");
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn now_ms_is_epoch_milliseconds() {
        // 与秒级时间戳同量级：10 位秒 -> 13 位毫秒
        assert!(now_ms() > 1_700_000_000_000);
    }
}
