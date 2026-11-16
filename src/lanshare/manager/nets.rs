//! 本机网卡枚举、同网段判定与电量读取，对应原版 `NetWorldUtils` + `BatteryUtils`。
//!
//! Windows 走 Win32（`GetAdaptersInfo` / `GetSystemPowerStatus`），其余平台走
//! 现成的命令行工具与 sysfs，见底部的 [`unix`] 模块。

use super::device::{Device, MODE_LINUX, MODE_MAC, MODE_UNKNOWN, MODE_WIN, now_ms};
use super::proto::DATA_VERSION;

/// 一块已启用的 IPv4 网卡地址。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nic {
    /// 接口索引，`GetIfEntry` 用它查状态与流量；类 Unix 上只是枚举序号。
    pub index: u32,
    /// 接口名（`GetAdaptersInfo` 的 AdapterName）。
    pub name: String,
    pub ip: String,
    pub mask: String,
    pub broadcast: String,
    /// 收发字节数之和，原版用它给网卡排序。
    pub traffic: u64,
}

/// 点分十进制 IPv4 转大端整数，与原版 `strIP2intIP` 一致；非法输入按 0 处理。
pub fn ip_to_u32(text: &str) -> u32 {
    match text.parse::<std::net::Ipv4Addr>() {
        Ok(address) => u32::from_be_bytes(address.octets()),
        Err(_) => {
            // 原版是 split('.').toInt()，缺段按 0 补；这里保持同样的宽容度
            let parts: Vec<u32> = text.split('.').map(|part| part.trim().parse::<u32>().unwrap_or(0)).collect();
            let mut value = 0u32;
            for index in 0..4 {
                let byte = parts.get(index).copied().unwrap_or(0) as u8;
                value = (value << 8) | byte as u32;
            }
            value
        }
    }
}

/// 大端整数转点分十进制，与原版 `intIP2StrIP` 一致。
pub fn u32_to_ip(value: u32) -> String {
    let bytes = value.to_be_bytes();
    format!("{}.{}.{}.{}", bytes[0], bytes[1], bytes[2], bytes[3])
}

/// 子网掩码的前置 1 个数；全 1（/32）时返回 -1，与原版 `getMaskMapLength` 一致。
pub fn mask_len(mask: &str) -> i32 {
    let value = ip_to_u32(mask);
    let mut count = 0i32;
    for bit in 0..32 {
        if (value << bit) & 0x8000_0000 != 0 {
            count += 1;
        } else {
            return count;
        }
    }
    -1
}

/// 掩码长度还原成点分掩码。
pub fn mask_str(len: i32) -> String {
    u32_to_ip(subnet_mask(len))
}

/// 前 `len` 位为 1 的掩码。`mask_len` 用 -1 表示「没有 0 位」（即 /32），这里按 /32 处理；
/// `len == 0`（空掩码 / 非法掩码）时不限定网段。
fn subnet_mask(len: i32) -> u32 {
    if len < 0 || len >= 32 {
        0xFFFF_FFFF
    } else if len == 0 {
        0
    } else {
        0xFFFF_FFFFu32 << (32 - len)
    }
}

/// 两个 IP 是否同网段（`len` 取自 `mask_len`）。掩码长度 0 时全部算同网段。
pub fn same_subnet(len: i32, a: &str, b: &str) -> bool {
    let mask = subnet_mask(len);
    (ip_to_u32(a) & mask) == (ip_to_u32(b) & mask)
}

/// 广播地址：`(~mask) | ip`。
pub fn broadcast_of(ip: &str, mask: &str) -> String {
    let value = (!ip_to_u32(mask)) | ip_to_u32(ip);
    u32_to_ip(value)
}

/// 把 IP 与端口拼成合法的 socket 地址字符串：IPv6 必须用方括号包裹，否则
/// `fe80::1:5854` 里端口会和多出来的冒号糊在一起、`parse::<SocketAddr>()` 直接失败；
/// IPv4 没有冒号，原样拼接。已带方括号的输入先剥掉再统一包裹，避免重复嵌套。
pub fn socket_address(ip: &str, port: u16) -> String {
    if ip.contains(':') {
        let body = ip.trim_start_matches('[').trim_end_matches(']');
        format!("[{body}]:{port}")
    } else {
        format!("{ip}:{port}")
    }
}

/// 本机自报的平台号，写进 UDP/TCP 包给对端挑图标用。
///
/// 用 `cfg!` 而非 `#[cfg]`：四个常量都以表达式身份参与编译，不会在单一平台上被当成多余引用。
pub const fn local_mode() -> i32 {
    if cfg!(windows) {
        MODE_WIN
    } else if cfg!(target_os = "macos") {
        MODE_MAC
    } else if cfg!(target_os = "linux") {
        MODE_LINUX
    } else {
        MODE_UNKNOWN
    }
}

/// 当前系统的电量百分比与充电状态，取不到时分别是 -1 / -1。
pub fn battery_status() -> (i32, i32) {
    #[cfg(windows)]
    {
        use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
        let mut status = SYSTEM_POWER_STATUS::default();
        if unsafe { GetSystemPowerStatus(&mut status) }.is_ok() {
            let level = if status.BatteryLifePercent == 255 { -1 } else { status.BatteryLifePercent as i32 };
            let charge = if status.ACLineStatus == 1 { 1 } else { 0 };
            return (level, charge);
        }
        (-1, -1)
    }
    #[cfg(not(windows))]
    {
        unix::battery_status()
    }
}

/// 枚举本机所有「已启用且能收发」的 IPv4 网卡地址，按流量从大到小排序。
pub fn nics() -> Vec<Nic> {
    #[cfg(windows)]
    {
        windows_nics()
    }
    #[cfg(not(windows))]
    {
        unix::nics()
    }
}

#[cfg(windows)]
fn c_string(bytes: &[i8]) -> String {
    let end = bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len());
    let ascii: Vec<u8> = bytes[..end].iter().map(|byte| *byte as u8).collect();
    String::from_utf8_lossy(&ascii).into_owned()
}

/// 网卡是否正常：`GetIfEntry` 的操作状态必须是 OPERATIONAL / CONNECTED。
#[cfg(windows)]
fn adapter_ok(index: u32) -> bool {
    use windows::Win32::Foundation::NO_ERROR;
    use windows::Win32::NetworkManagement::IpHelper::{
        GetIfEntry, IF_OPER_STATUS_CONNECTED, IF_OPER_STATUS_OPERATIONAL, MIB_IFROW,
    };
    let mut row = MIB_IFROW::default();
    row.dwIndex = index;
    if unsafe { GetIfEntry(&mut row) } != NO_ERROR.0 {
        return false;
    }
    row.dwOperStatus == IF_OPER_STATUS_OPERATIONAL || row.dwOperStatus == IF_OPER_STATUS_CONNECTED
}

#[cfg(windows)]
fn adapter_traffic(index: u32) -> u64 {
    use windows::Win32::Foundation::NO_ERROR;
    use windows::Win32::NetworkManagement::IpHelper::{GetIfEntry, MIB_IFROW};
    let mut row = MIB_IFROW::default();
    row.dwIndex = index;
    if unsafe { GetIfEntry(&mut row) } == NO_ERROR.0 {
        row.dwInOctets as u64 + row.dwOutOctets as u64
    } else {
        0
    }
}

#[cfg(windows)]
fn windows_nics() -> Vec<Nic> {
    use windows::Win32::NetworkManagement::IpHelper::{
        GetAdaptersInfo, IF_TYPE_IEEE80211, IP_ADAPTER_INFO, IP_ADDR_STRING, MIB_IF_TYPE_ETHERNET,
    };

    const ERROR_BUFFER_OVERFLOW: u32 = 111;
    let mut size = 0u32;
    if unsafe { GetAdaptersInfo(None, &mut size) } != ERROR_BUFFER_OVERFLOW {
        return Vec::new();
    }
    let mut buffer = vec![0u8; size as usize];
    let head = buffer.as_mut_ptr().cast::<IP_ADAPTER_INFO>();
    if unsafe { GetAdaptersInfo(Some(head), &mut size) } != 0 {
        return Vec::new();
    }

    let mut list = Vec::new();
    let mut cursor = head;
    while !cursor.is_null() {
        let adapter = unsafe { &*cursor };
        if adapter.Type == MIB_IF_TYPE_ETHERNET || adapter.Type == IF_TYPE_IEEE80211 {
            if adapter_ok(adapter.Index) {
                let traffic = adapter_traffic(adapter.Index);
                let name = c_string(&adapter.Description);
                let mut address = &adapter.IpAddressList as *const IP_ADDR_STRING;
                while !address.is_null() {
                    let entry = unsafe { &*address };
                    let ip = c_string(&entry.IpAddress.String);
                    let mask = c_string(&entry.IpMask.String);
                    if !ip.is_empty() && ip != "0.0.0.0" {
                        list.push(Nic {
                            index: adapter.Index,
                            name: name.clone(),
                            broadcast: broadcast_of(&ip, &mask),
                            mask,
                            traffic,
                            ip,
                        });
                    }
                    address = entry.Next;
                }
            }
        }
        cursor = adapter.Next;
    }
    // 与原版一致：流量大的网卡排在前面
    list.sort_by(|a, b| b.traffic.cmp(&a.traffic));
    list
}

/// 用本机网卡信息构造 `Device` 列表：UDP 心跳、TCP 握手、消息发送都要带上真实网卡地址。
pub fn local_devices(client: &str, tcp_port: i32) -> Vec<Device> {
    let (battery, charge) = battery_status();
    let seen = now_ms();
    nics()
        .into_iter()
        .map(|nic| Device {
            name: client.to_string(),
            ip: nic.ip,
            net_mask: nic.mask,
            broadcast_ip: nic.broadcast,
            port: tcp_port,
            mode: local_mode(),
            // 本机在所有主动广播的通道（UDP 心跳/上线、旧版 TCP 长消息、TCP 握手、
            // fileTransferJson）里都自报当前版本；uuid 同时带 "-<version>" 后缀，
            // 兼顾只解析后缀的对端（原版桌面），两条通道报同一个版本号
            data_version: DATA_VERSION,
            uuid: String::new(),
            battery,
            charge,
            can_remove: true,
            last_seen: seen,
            traffic: nic.traffic as i64,
        })
        .collect()
}

/// 某个 IP 是否属于本机网卡（原版用它过滤自己发的广播）。
pub fn is_local_ip(ip: &str) -> bool {
    nics().iter().any(|nic| nic.ip == ip)
}

/// 非 Windows 平台的本机信息读取：靠现成的命令行工具（`ip` / `ifconfig` / `route` / `pmset`）
/// 和 sysfs，不额外引依赖。工具缺失时一律退化成「没有」，绝不 panic。
#[cfg(not(windows))]
mod unix {
    use super::{Nic, broadcast_of, u32_to_ip};
    use std::process::Command;
    #[cfg(target_os = "linux")]
    use super::mask_str;
    #[cfg(target_os = "linux")]
    use std::path::Path;

    /// 跑一条命令拿标准输出；命令不存在、退出码非零或输出不是文本，都当作没有。
    fn stdout(program: &str, args: &[&str]) -> Option<String> {
        let output = Command::new(program).args(args).output().ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8(output.stdout).ok()
    }

    /// 能拿来做局域网广播的地址：跳过空、`0.0.0.0` 与回环。
    fn usable(ip: &str) -> bool {
        !ip.is_empty() && ip != "0.0.0.0" && !ip.starts_with("127.")
    }

    /// 取关键字后面紧跟的那个值，比如 `brd 192.168.1.255`。
    fn value_after(fields: &[&str], key: &str) -> Option<String> {
        fields
            .iter()
            .position(|field| *field == key)
            .and_then(|at| fields.get(at + 1))
            .map(|value| value.trim_matches(|c| c == '"' || c == '\'').to_string())
    }

    /// `192.168.1.7/24` -> `("192.168.1.7", 24)`。
    #[cfg(target_os = "linux")]
    fn prefix(text: &str) -> Option<(String, i32)> {
        let (ip, len) = text.split_once('/')?;
        Some((ip.to_string(), len.trim().parse().ok()?))
    }

    pub fn nics() -> Vec<Nic> {
        #[cfg(target_os = "linux")]
        let mut list = ip_route2();
        #[cfg(not(target_os = "linux"))]
        let mut list = Vec::new();
        // 没有 iproute2（或它没给出可用网卡）就退到通用的 ifconfig
        if list.is_empty() {
            list = ifconfig();
        }
        prioritize_default(&mut list);
        // 与 Windows 分支一致：流量大的排前面，这里只有默认路由那块是确定的
        list.sort_by(|a, b| b.traffic.cmp(&a.traffic));
        list
    }

    /// 默认路由那块网卡挪到最前：类 Unix 上收发计数不像 Win32 那样随手可得，
    /// 而「主力网卡排第一」才是原版排序的真意图。
    fn prioritize_default(list: &mut [Nic]) {
        let name = default_interface();
        if name.is_empty() {
            return;
        }
        if let Some(nic) = list.iter_mut().find(|nic| nic.name == name) {
            nic.traffic = u64::MAX;
        }
    }

    /// 默认路由走的那个接口名，拿不到给空串。
    fn default_interface() -> String {
        #[cfg(target_os = "linux")]
        let text = stdout("ip", &["-o", "route", "show", "default"]);
        #[cfg(not(target_os = "linux"))]
        let text = stdout("route", &["-n", "get", "default"]);
        let Some(text) = text else {
            return String::new();
        };
        #[cfg(target_os = "linux")]
        {
            let fields: Vec<&str> = text.split_whitespace().collect();
            value_after(&fields, "dev").unwrap_or_default()
        }
        #[cfg(not(target_os = "linux"))]
        {
            text.lines()
                .find_map(|line| line.trim().strip_prefix("interface:"))
                .map(|name| name.trim().to_string())
                .unwrap_or_default()
        }
    }

    /// Linux：`ip -o -4 addr show up`，一行一个地址，形如
    /// `2: eth0  inet 192.168.1.7/24 brd 192.168.1.255 scope global eth0`。
    #[cfg(target_os = "linux")]
    fn ip_route2() -> Vec<Nic> {
        let mut list = Vec::new();
        let Some(text) = stdout("ip", &["-o", "-4", "addr", "show", "up"]) else {
            return list;
        };
        for line in text.lines() {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 4 || fields[2] != "inet" {
                continue;
            }
            let index = fields[0].trim_end_matches(':').parse::<u32>().unwrap_or(0);
            let Some((ip, len)) = prefix(fields[3]) else { continue };
            if !usable(&ip) {
                continue;
            }
            let mask = mask_str(len);
            let broadcast = value_after(&fields, "brd").unwrap_or_else(|| broadcast_of(&ip, &mask));
            list.push(Nic {
                index,
                name: fields[1].to_string(),
                ip,
                mask,
                broadcast,
                traffic: sysfs_traffic(fields[1]),
            });
        }
        list
    }

    /// Linux 的累计收发字节，读不到算 0；和 Windows 一样只用来排序。
    #[cfg(target_os = "linux")]
    fn sysfs_traffic(name: &str) -> u64 {
        let base = Path::new("/sys/class/net").join(name).join("statistics");
        ["rx_bytes", "tx_bytes"].iter().map(|file| read_number(&base.join(file)).unwrap_or(0)).sum()
    }

    #[cfg(target_os = "linux")]
    fn read_number(path: &Path) -> Option<u64> {
        std::fs::read_to_string(path).ok()?.trim().parse().ok()
    }

    /// macOS / BSD，以及只有 `ifconfig` 的 Linux：块首行是
    /// `en0: flags=8863<UP,BROADCAST,SMART,RUNNING,...>`，缩进续行里是
    /// `inet 192.168.1.7 netmask 0xffffff00 broadcast 192.168.1.255`。
    fn ifconfig() -> Vec<Nic> {
        let mut list = Vec::new();
        let Some(text) = stdout("ifconfig", &["-a"]) else {
            return list;
        };
        let mut name = String::new();
        let mut live = false;
        let mut index = 0u32;
        for line in text.lines() {
            if !line.starts_with(' ') && !line.starts_with('\t') {
                let head = line.split(':').next().unwrap_or_default().trim().to_string();
                // 只有同时 UP、RUNNING 的接口才收得到广播
                live = !head.is_empty() && line.contains("<UP,") && line.contains("RUNNING");
                if live {
                    index += 1;
                }
                name = head;
                continue;
            }
            if !live {
                continue;
            }
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 2 || fields[0] != "inet" {
                continue;
            }
            let ip = fields[1].to_string();
            if !usable(&ip) {
                continue;
            }
            // 极少数驱动不写掩码，按 /24 兜住（掩码只用来算广播与同网段）
            let mask = value_after(&fields, "netmask").map(|hex| hex_to_mask(&hex)).unwrap_or_else(|| "255.255.255.0".to_string());
            let broadcast = value_after(&fields, "broadcast").unwrap_or_else(|| broadcast_of(&ip, &mask));
            list.push(Nic { index, name: name.clone(), ip, mask, broadcast, traffic: 0 });
        }
        list
    }

    /// `0xffffff00` -> `255.255.255.0`；解不动给空掩码，即「不限网段」。
    fn hex_to_mask(text: &str) -> String {
        let digits = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")).unwrap_or(text);
        match u32::from_str_radix(digits.trim(), 16) {
            Ok(value) => u32_to_ip(value),
            Err(_) => String::new(),
        }
    }

    /// 电量：Linux 读 sysfs，macOS 问 `pmset`；台式机与容器里通常什么都没有，退成 -1 / -1。
    pub fn battery_status() -> (i32, i32) {
        #[cfg(target_os = "linux")]
        {
            if let Some(pair) = sysfs_battery() {
                return pair;
            }
        }
        #[cfg(target_os = "macos")]
        {
            if let Some(pair) = pmset_battery() {
                return pair;
            }
        }
        (-1, -1)
    }

    /// Linux：`/sys/class/power_supply` 下 `BAT*` 给容量，`AC*` / `ADP*` / `USB*` 的 `online` 给是否插电。
    #[cfg(target_os = "linux")]
    fn sysfs_battery() -> Option<(i32, i32)> {
        let entries = std::fs::read_dir("/sys/class/power_supply").ok()?;
        let mut level = None;
        let mut charge = None;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("BAT") {
                level = read_number(&entry.path().join("capacity")).map(|value| value as i32);
            } else if name.starts_with("AC") || name.starts_with("ADP") || name.starts_with("USB") {
                charge = read_number(&entry.path().join("online")).map(|online| if online == 0 { 0 } else { 1 });
            }
        }
        // 有电池却没读到电源条目就当未插电，与 Windows 的 ACLineStatus == 0 同义
        Some((level?, charge.unwrap_or(0)))
    }

    /// macOS：`pmset -g batt` 头一行说电源（`Now drawing from 'AC Power'`），
    /// 电池行形如 `-InternalBattery-0\t85%; discharging; 1:12 remaining present: true`。
    #[cfg(target_os = "macos")]
    fn pmset_battery() -> Option<(i32, i32)> {
        let text = stdout("pmset", &["-g", "batt"])?;
        let charge = if text.contains("'AC Power'") {
            1
        } else if text.contains("'Battery Power'") {
            0
        } else {
            return None;
        };
        let level = text
            .lines()
            .find_map(|line| {
                let head = line.split('%').next()?;
                head.trim().split_whitespace().last()?.parse::<i32>().ok()
            })?;
        Some((level, charge))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_conversion_matches_cpp_big_endian_reading() {
        assert_eq!(ip_to_u32("192.168.1.7"), 0xC0A8_0107);
        assert_eq!(u32_to_ip(0xC0A8_0107), "192.168.1.7");
        assert_eq!(ip_to_u32("255.255.255.255"), u32::MAX);
        // 非法/缺段按 0 补，跟 QString::toInt 的宽容度一致
        assert_eq!(ip_to_u32("10.20"), ip_to_u32("10.20.0.0"));
        assert_eq!(ip_to_u32(""), 0);
    }

    #[test]
    fn mask_length_counts_leading_ones() {
        assert_eq!(mask_len("255.255.255.0"), 24);
        assert_eq!(mask_len("255.255.0.0"), 16);
        assert_eq!(mask_len("255.255.255.254"), 31);
        assert_eq!(mask_len("0.0.0.0"), 0);
        // 全 1 掩码原版返回 -1
        assert_eq!(mask_len("255.255.255.255"), -1);
        assert_eq!(mask_str(24), "255.255.255.0");
        assert_eq!(mask_str(16), "255.255.0.0");
    }

    #[test]
    fn subnet_and_broadcast() {
        assert!(same_subnet(24, "192.168.1.7", "192.168.1.200"));
        assert!(!same_subnet(24, "192.168.1.7", "192.168.2.7"));
        assert!(same_subnet(16, "192.168.1.7", "192.168.9.7"));
        assert_eq!(broadcast_of("192.168.1.7", "255.255.255.0"), "192.168.1.255");
        assert_eq!(broadcast_of("10.0.0.5", "255.0.0.0"), "10.255.255.255");
    }

    #[test]
    fn degenerate_masks_follow_the_documented_convention() {
        // 空掩码读成 0.0.0.0，长度 0 就是不限网段
        assert_eq!(mask_len(""), 0);
        assert!(same_subnet(0, "1.1.1.1", "2.2.2.2"));
        assert_eq!(mask_str(0), "0.0.0.0");
        // /32 原版用 -1 表示，等价于 32：只有同一地址才同网段
        assert_eq!(mask_len("255.255.255.255"), -1);
        assert_eq!(mask_str(-1), "255.255.255.255");
        assert!(same_subnet(-1, "1.1.1.1", "1.1.1.1"));
        assert!(!same_subnet(-1, "1.1.1.1", "2.2.2.2"));
    }

    #[test]
    fn socket_address_brackets_ipv6_only() {
        // IPv4 直接拼，不加括号
        assert_eq!(socket_address("192.168.1.7", 5856), "192.168.1.7:5856");
        assert_eq!(socket_address("255.255.255.255", 5854), "255.255.255.255:5854");
        // IPv6 必须加方括号，否则 ip:port 混在一起解不出来
        assert_eq!(socket_address("fe80::1", 5856), "[fe80::1]:5856");
        assert_eq!(socket_address("::1", 5856), "[::1]:5856");
        assert_eq!(socket_address("2001:db8::8a2e:370:7334", 5854), "[2001:db8::8a2e:370:7334]:5854");
        // 已带括号的输入不重复包裹
        assert_eq!(socket_address("[fe80::1]", 5856), "[fe80::1]:5856");
        // 拼出来的字符串都能被 SocketAddr 解析回去
        for text in [
            socket_address("192.168.1.7", 5856),
            socket_address("fe80::1", 5856),
            socket_address("::1", 5856),
        ] {
            assert!(text.parse::<std::net::SocketAddr>().is_ok(), "{text} 无法解析");
        }
    }

    #[test]
    fn battery_status_is_in_valid_ranges() {
        let (level, charge) = battery_status();
        assert!(level == -1 || (0..=100).contains(&level), "电量异常: {level}");
        assert!(matches!(charge, -1 | 0 | 1), "充电状态异常: {charge}");
    }

    #[test]
    fn local_devices_use_real_interfaces_when_present() {
        let devices = local_devices("测试机", 5856);
        for device in &devices {
            assert_eq!(device.port, 5856);
            assert_eq!(device.mode, local_mode());
            assert_eq!(device.data_version, DATA_VERSION);
            assert_eq!(device.name, "测试机");
            assert!(!device.ip.is_empty());
            assert_eq!(device.broadcast_ip, broadcast_of(&device.ip, &device.net_mask));
        }
    }
}
