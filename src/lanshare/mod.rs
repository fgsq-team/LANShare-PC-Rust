//! LANShare 移植的核心引擎：线协议、加密、设备表、网络服务与调度。
//!
//! `Engine` 是被 `Arc` 共享的中心对象，网络线程通过事件队列向 UI 汇报，
//! UI 的出站动作（发消息、发文件、取消、接受/拒绝接收）直接调方法，不绕消息队列。

pub mod config;
pub mod crypto;
pub mod db;
pub mod proto;
pub mod stream;
pub mod walk;

// 分层子模块：设备/协议管理与各版本收发
pub mod manager;
pub mod version;

// 兼容旧扁平路径：本文件与外部仍以 device/tcp/udp/legacy/nets、fsend4/fsrv4/… 直呼其名
pub use manager::{device, legacy, nets, tcp, udp};
pub use version::{fsend4, fsend5, fsrv4, fsrv5, resume};

use std::collections::{HashMap, VecDeque};
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use config::Config;
use db::{Entry, HistoryDb, TYPE_FILE, TYPE_TEXT};
use manager::device::{Device, DeviceTable, now_ms};
use manager::device::MODE_WIN;
use proto::{DATA_VERSION, UDP_MESSAGE, UDP_MESSAGE_TO_CLIPBOARD};
use stream::Conn;
use walk::{TransferMeta, size_str, to_items};

/// 一条传输的标识与展示信息，`id` 就是协议里的 `fileId`。
#[derive(Debug, Clone)]
pub struct TransferInfo {
    pub id: String,
    pub device: Device,
    pub name: String,
    pub length: i64,
    pub is_dir: bool,
    /// true = 本机发出，false = 接收。
    pub sending: bool,
}

/// 等待用户确认的接收请求。
#[derive(Debug, Clone)]
pub struct ReceiveOffer {
    pub key: u64,
    pub device: Device,
    pub names: Vec<String>,
    pub total: i64,
}

/// 引擎向 UI 汇报的事件。
#[derive(Debug, Clone)]
pub enum NetEvent {
    /// 在线设备表快照（增删改后整体推送，UI 直接替换列表）。
    Devices(Vec<Device>),
    Message { device: Device, text: String, clipboard: bool, incoming: bool },
    Receive(ReceiveOffer),
    Start(TransferInfo),
    Progress { id: String, progress: i32 },
    Finish { id: String, ok: bool, path: String },
    /// 网页客户端请求授权，UI 弹确认框（对应原版的 `websockets` 授权弹框）。
    WebGrant { token: String, ip: String },
}

/// 挂起等待用户确认的接收上下文。
struct PendingAccept {
    conn: Conn,
    from: Device,
    kind: Pending,
}

/// 待确认接收的两种协议形态。
enum Pending {
    /// 新版（V4/V5）：按 `fileTransferJson` 接收。`resume` 取自首帧魔数（`-3` 为 V5），
    /// 与原版一致：Java 手机发的 V5 JSON 里并没有 `resume` 字段，只能靠魔数分流。
    Modern { enc_data: bool, meta: TransferMeta, resume: bool },
    /// 旧版（V1~V3）：按分帧描述接收。
    Legacy { enc_data: bool, files: Vec<legacy::LegacyFile> },
}

/// 共享中心对象。
pub struct Engine {
    config: RwLock<Config>,
    devices: Mutex<DeviceTable>,
    local: Mutex<Vec<Device>>,
    events: Mutex<VecDeque<NetEvent>>,
    arrive: Condvar,
    stopping: (Mutex<bool>, Condvar),
    udp: Mutex<Option<Arc<UdpSocket>>>,
    /// IPv6 专用 UDP socket（Linux 双栈主 socket 已涵盖，这里为空）；Windows/macOS 下与 IPv4 各一条。
    udp6: Mutex<Option<Arc<UdpSocket>>>,
    listener: Mutex<Option<Arc<std::net::TcpListener>>>,
    pending: Mutex<HashMap<u64, PendingAccept>>,
    offer_seq: AtomicU64,
    cancels: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// 正在进行的传输，传输结束时按它向网页播报（原版是在收发代码里就地推 WS）。
    active: Mutex<HashMap<String, TransferInfo>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    history: Mutex<Option<HistoryDb>>,
}

impl Engine {
    fn with_config(config: Config) -> Self {
        Self {
            config: RwLock::new(config),
            devices: Mutex::new(DeviceTable::new()),
            local: Mutex::new(Vec::new()),
            events: Mutex::new(VecDeque::new()),
            arrive: Condvar::new(),
            stopping: (Mutex::new(false), Condvar::new()),
            udp: Mutex::new(None),
            udp6: Mutex::new(None),
            listener: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            offer_seq: AtomicU64::new(1),
            cancels: Mutex::new(HashMap::new()),
            active: Mutex::new(HashMap::new()),
            workers: Mutex::new(Vec::new()),
            history: Mutex::new(None),
        }
    }

    /// 不起服务的引擎，只给单元测试搭场景用。
    #[doc(hidden)]
    pub fn for_test(config: Config) -> Self {
        Self::with_config(config)
    }

    /// 读配置、打开历史库、拉起 UDP 发现与 TCP 服务。
    pub fn start(config: Config) -> Arc<Self> {
        let engine = Arc::new(Self::with_config(config));
        if engine.config().save_message {
            let opened = HistoryDb::open(&Config::db_path()).ok();
            *engine.history.lock().expect("历史库锁未被污染") = opened;
        }
        resume::ResumeStore::new(&engine.config().save_path).cleanup_expired();
        // 开了网页服务就先后台扫一遍图库：原版也是 `detach` 一个线程去扫「我的图片」，
        // 因为几万张文件不能挡在启动路径上
        if engine.config().web_service {
            crate::web::media::spawn_scan(crate::web::media::default_roots());
        }
        engine.refresh_local();
        // 句柄先落地再收线程，别让 workers 的锁跨到返回处
        {
            let mut workers = engine.workers.lock().expect("线程表锁未被污染");
            workers.extend(udp::spawn(Arc::clone(&engine)));
            workers.extend(tcp::spawn(Arc::clone(&engine)));
        }
        engine
    }

    /// 停线程、关套接字，供窗口退出时调用。
    pub fn shutdown(&self) {
        // 先踢掉网页会话：那些线程正阻在 `read_frame` 上，不 shutdown 就 join 不回来
        crate::web::sessions::close_all();
        // 先告诉所有在线设备本机下线，再拆服务
        for device in self.devices() {
            let _ = udp::notice_offline(self, &device.ip);
        }
        {
            let mut flag = self.stopping.0.lock().expect("停止标志锁未被污染");
            *flag = true;
        }
        self.stopping.1.notify_all();
        // UDP 套接字没有 shutdown：收包线程按 300ms 读超时回来查停止标志
        *self.udp.lock().expect("UDP 锁未被污染") = None;
        *self.udp6.lock().expect("UDP6 锁未被污染") = None;
        // 收包线程带着读超时轮询停止标志，这里逐个 join；先取出句柄避免握锁等待
        let handles = std::mem::take(&mut *self.workers.lock().expect("线程表锁未被污染"));
        for handle in handles {
            let _ = handle.join();
        }
    }

    pub fn is_running(&self) -> bool {
        !*self.stopping.0.lock().expect("停止标志锁未被污染")
    }

    /// 可中断等待：正常睡 `ms` 毫秒后返回 true，收到停止信号立刻返回 false。
    pub fn wait_stop(&self, ms: u64) -> bool {
        let guard = self.stopping.0.lock().expect("停止标志锁未被污染");
        if *guard {
            return false;
        }
        let (guard, _) = self.stopping.1.wait_timeout(guard, Duration::from_millis(ms)).expect("停止等待锁未被污染");
        !*guard
    }

    // —— 配置 ——————————————————————————————————————————————

    pub fn config(&self) -> Config {
        self.config.read().expect("配置读锁未被污染").clone()
    }

    /// 换配置并落盘；端口变更需要重启服务才生效。
    pub fn set_config(&self, config: Config) {
        let theme_changed = self.config().theme != config.theme;
        let _ = config.save();
        *self.config.write().expect("配置写锁未被污染") = config;
        // 主题变了才推，免得每次保存设置都刷一遍网页
        if theme_changed {
            crate::web::sessions::push_theme(&self.config().theme);
        }
    }

    pub fn unique_uuid(&self) -> String {
        self.config().unique_uuid
    }

    // —— 事件队列 ——————————————————————————————————————————

    pub fn publish(&self, event: NetEvent) {
        // 在线表变了就顺手推给网页：原版也是在设备上下线的地方直接 `sendDeviceList()`，
        // 不等 UI 泵跑一圈回来
        if let NetEvent::Devices(list) = &event {
            crate::web::sessions::push_devices(list);
        }
        {
            let mut queue = self.events.lock().expect("事件锁未被污染");
            queue.push_back(event);
        }
        self.arrive.notify_all();
    }

    /// 取走当前全部事件；队列为空时最多等 `timeout`。
    pub fn take_events(&self, timeout: Duration) -> Vec<NetEvent> {
        let mut queue = self.events.lock().expect("事件锁未被污染");
        if queue.is_empty() && self.is_running() {
            let (guard, _) = self.arrive.wait_timeout(queue, timeout).expect("事件等待锁未被污染");
            queue = guard;
        }
        queue.drain(..).collect()
    }

    // —— 本机与设备表 ——————————————————————————————————————

    /// 重新枚举本机网卡，并把唯一码补进设备信息。
    pub fn refresh_local(&self) -> Vec<Device> {
        let config = self.config();
        let mut list = nets::local_devices(&config.client_name, config.tcp_port);
        for device in list.iter_mut() {
            device.uuid = config.unique_uuid.clone();
        }
        *self.local.lock().expect("本机房锁未被污染") = list.clone();
        list
    }

    pub fn local_devices(&self) -> Vec<Device> {
        self.local.lock().expect("本机房锁未被污染").clone()
    }

    /// 选一块与对端同网段的网卡（原版 `subNet` 匹配失败时用第一块）。
    pub fn pick_local(&self, peer_ip: &str) -> Option<Device> {
        let list = self.local.lock().expect("本机房锁未被污染");
        for device in list.iter() {
            let len = nets::mask_len(&device.net_mask);
            if len > 0 && nets::same_subnet(len, &device.ip, peer_ip) {
                return Some(device.clone());
            }
        }
        list.first().cloned()
    }

    /// 收到自己发的包是否要过滤掉（同机双实例测试时放开）。
    pub fn is_local_ip(&self, ip: &str) -> bool {
        if self.config().allow_same_host_test {
            return false;
        }
        self.local_devices().iter().any(|device| device.ip == ip) || nets::is_local_ip(ip)
    }

    pub fn devices(&self) -> Vec<Device> {
        self.devices.lock().expect("设备表锁未被污染").list()
    }

    pub fn device_at(&self, ip: &str, port: i32) -> Option<Device> {
        self.devices.lock().expect("设备表锁未被污染").get(ip, port).cloned()
    }

    /// 加入或刷新设备，变化时推送设备表。
    pub fn touch_device(&self, device: Device) {
        let changed = self.devices.lock().expect("设备表锁未被污染").add(device);
        if changed {
            self.publish(NetEvent::Devices(self.devices()));
        }
    }

    pub fn forget_device(&self, device: &Device) {
        if self.devices.lock().expect("设备表锁未被污染").remove(device) {
            self.publish(NetEvent::Devices(self.devices()));
        }
    }

    /// 心跳超时清理，返回被移除的设备。
    pub fn cleanup_expired(&self) -> Vec<Device> {
        let removed = self.devices.lock().expect("设备表锁未被污染").cleanup_expired(now_ms());
        if !removed.is_empty() {
            self.publish(NetEvent::Devices(self.devices()));
        }
        removed
    }

    // —— 套接字 ————————————————————————————————————————————

    pub fn set_udp(&self, socket: Arc<UdpSocket>) {
        *self.udp.lock().expect("UDP 锁未被污染") = Some(socket);
    }

    pub fn udp(&self) -> Option<Arc<UdpSocket>> {
        self.udp.lock().expect("UDP 锁未被污染").clone()
    }

    pub fn set_udp6(&self, socket: Arc<UdpSocket>) {
        *self.udp6.lock().expect("UDP6 锁未被污染") = Some(socket);
    }

    pub fn udp6(&self) -> Option<Arc<UdpSocket>> {
        self.udp6.lock().expect("UDP6 锁未被污染").clone()
    }

    pub fn set_listener(&self, listener: std::net::TcpListener) -> std::io::Result<()> {
        listener.set_nonblocking(true)?;
        *self.listener.lock().expect("监听锁未被污染") = Some(Arc::new(listener));
        Ok(())
    }

    pub fn listener(&self) -> Option<Arc<std::net::TcpListener>> {
        self.listener.lock().expect("监听锁未被污染").clone()
    }

    // —— 取消与确认 ————————————————————————————————————————

    /// 登记一个取消标记（收发两端都用 `fileId` 作键）。
    ///
    /// 已经登记过就沿用旧的那份：接收线程重建气泡时不能把用户刚点下的取消抹掉。
    pub fn register_cancel(&self, id: &str) -> Arc<AtomicBool> {
        let mut table = self.cancels.lock().expect("取消表锁未被污染");
        let flag = table.entry(id.to_string()).or_insert_with(|| Arc::new(AtomicBool::new(false)));
        Arc::clone(flag)
    }

    pub fn cancel(&self, id: &str) {
        if let Some(flag) = self.cancels.lock().expect("取消表锁未被污染").get(id) {
            flag.store(true, Ordering::SeqCst);
        }
    }

    pub fn cancelled(&self, id: &str) -> bool {
        self.cancels.lock().expect("取消表锁未被污染").get(id).is_some_and(|flag| flag.load(Ordering::SeqCst))
    }

    pub fn release_cancel(&self, id: &str) {
        self.cancels.lock().expect("取消表锁未被污染").remove(id);
    }

    /// 挂起一个新版（V4/V5）待确认接收；开了自动接收就直接开始收。`resume` 来自首帧魔数。
    pub fn offer_receive(self: &Arc<Self>, conn: Conn, from: Device, enc_data: bool, meta: TransferMeta, resume: bool) {
        let names = meta.files.iter().map(|file| file.name.clone()).collect();
        let total: i64 = meta.files.iter().map(|file| file.length).sum();
        self.offer_or_start(conn, from, Pending::Modern { enc_data, meta, resume }, names, total);
    }

    /// 挂起一个旧版（V1~V3）待确认接收。
    pub fn offer_legacy_receive(self: &Arc<Self>, conn: Conn, from: Device, enc_data: bool, files: Vec<legacy::LegacyFile>) {
        let names = files.iter().map(|file| file.name.clone()).collect();
        let total: i64 = files.iter().map(|file| file.length).sum();
        self.offer_or_start(conn, from, Pending::Legacy { enc_data, files }, names, total);
    }

    /// 需要用户确认时挂起并发事件，配置成自动接收时直接开工（与原版 `acceptRecvFiles` 一致）。
    fn offer_or_start(self: &Arc<Self>, conn: Conn, from: Device, kind: Pending, names: Vec<String>, total: i64) {
        if self.config().accept_recv_files {
            self.start_receive(conn, from, kind);
            return;
        }
        let key = self.offer_seq.fetch_add(1, Ordering::SeqCst);
        let pending = PendingAccept { conn, from: from.clone(), kind };
        self.pending.lock().expect("待确认表锁未被污染").insert(key, pending);
        self.publish(NetEvent::Receive(ReceiveOffer { key, device: from, names, total }));
    }

    /// 用户点了接受/拒绝。
    pub fn resolve_receive(self: &Arc<Self>, key: u64, accept: bool) {
        let taken = self.pending.lock().expect("待确认表锁未被污染").remove(&key);
        let Some(PendingAccept { conn, from, kind }) = taken else { return };
        if !accept {
            self.reject(conn, kind);
            return;
        }
        self.start_receive(conn, from, kind);
    }

    /// 接收当前挂起的全部请求（或全部拒绝），供批量确认的对话框使用。
    pub fn resolve_all(self: &Arc<Self>, accept: bool) {
        let keys: Vec<u64> = self.pending.lock().expect("待确认表锁未被污染").keys().copied().collect();
        for key in keys {
            self.resolve_receive(key, accept);
        }
    }

    /// 在新的工作线程里继续读流接收。
    fn start_receive(self: &Arc<Self>, conn: Conn, from: Device, kind: Pending) {
        // 对方没带 fileId 时先补上，保证气泡键与失败收尾用的一致
        let kind = match kind {
            Pending::Modern { enc_data, meta, resume } => {
                let mut meta = meta;
                for file in meta.files.iter_mut() {
                    file.file_id = receive_id(&file.file_id);
                }
                Pending::Modern { enc_data, meta, resume }
            }
            other => other,
        };
        // 失败收尾只按顶层 id 广播，先把键扫出来（kind 会被搬进接收函数）
        let ids: Vec<String> = ids_of(&kind);
        let engine = Arc::clone(self);
        std::thread::spawn(move || {
            let result = match kind {
                Pending::Modern { enc_data, meta, resume } => {
                    // 魔数是分流的第一依据（原版 Java/Qt 都靠 -2/-3）；JSON 带 `resume` 作兼容兑底
                    if resume || meta.resume_enabled() {
                        fsrv5::receive(&engine, conn, from, enc_data, meta)
                    } else {
                        fsrv4::receive(&engine, conn, from, enc_data, meta)
                    }
                }
                Pending::Legacy { enc_data, files } => legacy::receive(&engine, conn, from, enc_data, files),
            };
            if let Err(error) = result {
                // 走到这里说明流已经断了，只能按顶层 id 统一收尾
                for id in ids.iter() {
                    engine.publish_finish(id, false, "", &error.to_string());
                }
            }
        });
    }

    /// 拒绝对方的文件发送：新版回 `1103`，旧版回一帧 `FS_NOT_AGREE`。
    fn reject(self: &Arc<Self>, conn: Conn, kind: Pending) {
        for id in ids_of(&kind) {
            self.publish_finish(&id, false, "", "对方拒绝接收");
        }
        let written = match &kind {
            Pending::Modern { .. } => conn.write_i32(proto::FS_NOT_AGREE),
            Pending::Legacy { .. } => conn.write_raw(&proto::Packet::new(proto::FS_NOT_AGREE, 0).plain()),
        };
        let _ = written;
        conn.shutdown();
    }

    // —— 出站动作 ——————————————————————————————————————————

    /// 发文本消息：短消息走 UDP，超过 700 字节走 TCP 旧式帧（与原版一致）。
    ///
    /// 桌面发出的消息会同步推给网页（原版 `broadcastMessage` 里 `shareWS=true` 的那一次），
    /// 且只在真正发出去之前推一次。
    pub fn send_message(&self, ip: &str, port: i32, text: &str, clipboard: bool) -> std::io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let to = self.device_at(ip, port).ok_or_else(|| std::io::Error::other("设备不在线"))?;
        self.push_outgoing(text, clipboard);
        self.deliver(&to, text, clipboard)?;
        let from = self.pick_local(ip).unwrap_or_default();
        self.publish(NetEvent::Message { device: to.clone(), text: text.to_string(), clipboard, incoming: false });
        self.note_text(text, &to, &from, false, clipboard);
        Ok(())
    }

    /// 广播文本消息到全部在线设备：向网页也只推一次，与原版 `shareWS` 的位置一致。
    pub fn broadcast_message(&self, text: &str, clipboard: bool) -> std::io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        self.push_outgoing(text, clipboard);
        for device in self.devices() {
            let _ = self.deliver(&device, text, clipboard);
            let from = self.pick_local(&device.ip).unwrap_or_default();
            self.publish(NetEvent::Message { device: device.clone(), text: text.to_string(), clipboard, incoming: false });
            self.note_text(text, &device, &from, false, clipboard);
        }
        Ok(())
    }

    /// 只负责把一条文本送上网络，不碰历史、事件与网页推送。
    fn deliver(&self, to: &Device, text: &str, clipboard: bool) -> std::io::Result<()> {
        let config = self.config();
        let enc = crypto::enc_message(text, &config.message_key);
        if text.as_bytes().len() > 700 {
            tcp::send_legacy_message(self, to, &enc)
        } else {
            udp::send_message(self, to, &enc, if clipboard { UDP_MESSAGE_TO_CLIPBOARD } else { UDP_MESSAGE })
        }
    }

    /// 本机发的消息推给网页：气泡靠右（原版第 7 参 `false`），`devName` 是本机名。
    ///
    /// `isClip` 原版写的是常量 `false`，这里保持不映射（否则网页那边的气泡样式会跟桌面不一致）。
    fn push_outgoing(&self, text: &str, _clipboard: bool) {
        crate::web::sessions::push_message(text, &self.config().client_name, "", 0, MODE_WIN, "", false, false, false);
    }

    /// 发送文件/文件夹，返回每条顶层条目的信息（UI 用它建气泡）。
    pub fn send_files(self: &Arc<Self>, ip: &str, port: i32, paths: &[std::path::PathBuf]) -> std::io::Result<Vec<TransferInfo>> {
        let to = self.device_at(ip, port).ok_or_else(|| std::io::Error::other("设备不在线"))?;
        let from = self.pick_local(ip).ok_or_else(|| std::io::Error::other("没有可用网卡"))?;
        let mut items = to_items(paths);
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let resume = to.data_version >= DATA_VERSION;
        fsend5::prepare_ids(&mut items, &from, &to, resume);
        let infos: Vec<TransferInfo> = items
            .iter()
            .map(|item| TransferInfo {
                id: item.file_id.clone(),
                device: to.clone(),
                name: item.name.clone(),
                length: item.length,
                is_dir: item.is_dir,
                sending: true,
            })
            .collect();
        for (info, item) in infos.iter().zip(items.iter()) {
            self.register_cancel(&info.id);
            self.note_active(info);
            self.publish(NetEvent::Start(info.clone()));
            // 发送侧把源文件路径一并落库：中途退出后重开，「重新发送」才找得到原件
            self.note_file(info, false, 0, &item.path.to_string_lossy());
        }
        let engine = Arc::clone(self);
        let meta = TransferMeta::build(&items, &from, &to, resume);
        // 线程里只用副本广播失败，返回给 UI 的 infos 仍然完整
        let tracker = infos.clone();
        std::thread::spawn(move || {
            let result = if resume {
                fsend5::send(&engine, &from, &to, &items, &meta)
            } else {
                fsend4::send(&engine, &from, &to, &items, &meta)
            };
            if let Err(error) = result {
                for info in tracker.iter() {
                    engine.publish_finish(&info.id, false, "", &error.to_string());
                }
            }
        });
        Ok(infos)
    }

    /// 传输结束：广播事件，成功时向网页播报。
    pub fn publish_finish(&self, id: &str, ok: bool, path: &str, _error: &str) {
        self.release_cancel(id);
        let tracked = self.forget_active(id);
        self.publish(NetEvent::Finish { id: id.to_string(), ok, path: path.to_string() });
        // 原版只在**成功**时推 WS，而且 V4/V5 一律不带本机路径（网页看不到本机文件系统）、
        // `messageType` 恒为 2（文件）；`isLeft` 由收发方向决定气泡靠哪边
        if let (true, Some(info)) = (ok, tracked) {
            crate::web::sessions::push_message(&info.name, &info.device.name, "", 2, info.device.mode, &size_str(info.length), !info.sending, false, false);
        }
    }

    /// 一条传输的完整收尾：成功时补历史行的路径与 100% 进度。
    pub fn finish(&self, info: &TransferInfo, ok: bool, path: &str) {
        if ok && !path.is_empty() {
            self.note_file_done(info, path);
        }
        self.publish_finish(&info.id, ok, path, "");
    }

    /// 接收开始：登记取消标记与网页播报、广播气泡、写一条进度为 0 的历史。
    pub fn begin_receive(&self, info: &TransferInfo) {
        self.register_cancel(&info.id);
        self.note_active(info);
        self.publish(NetEvent::Start(info.clone()));
        // 接收侧此时还没有落点，路径留空，收尾时由 `note_file_done` 补
        self.note_file(info, true, 0, "");
    }

    /// 登记一条要向网页播报的传输；`publish_finish` 会按 id 摘掉它。
    fn note_active(&self, info: &TransferInfo) {
        self.active.lock().expect("传输表锁未被污染").insert(info.id.clone(), info.clone());
    }

    fn forget_active(&self, id: &str) -> Option<TransferInfo> {
        self.active.lock().expect("传输表锁未被污染").remove(id)
    }

    // —— 网页端接口 ——————————————————————————————————

    /// 按 `ip:port` 找在线设备，与网页下拉里的 `address` 同一个键。
    pub fn device_by_key(&self, address: &str) -> Option<Device> {
        self.devices().into_iter().find(|device| device.key() == address)
    }

    /// 网页客户端借本机发消息：转发给某台设备（或全部在线设备），但**不**回推 WebSocket。
    ///
    /// 原版调的是 `broadcastMessage(..., shareWS=false)`：发消息的那位自己就是网页，
    /// 浏览器里已经画好气泡了。桌面端的气泡设备名带 `<- ip` 后缀，用来分清
    /// 「本机操作」与「网页代发」。选中的设备不在表里时原版也不报错，只是不发送。
    pub fn relay_from_web(&self, address: &str, text: &str, clipboard: bool, client_ip: &str) -> std::io::Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        let mut label = "全部设备".to_string();
        if address.is_empty() {
            for device in self.devices() {
                let _ = self.deliver(&device, text, clipboard);
            }
        } else if let Some(device) = self.device_by_key(address) {
            label = device.name.clone();
            let _ = self.deliver(&device, text, clipboard);
        }
        let peer = Device { name: format!("{label} <- {client_ip}"), ip: client_ip.to_string(), mode: MODE_WIN, ..Default::default() };
        self.publish(NetEvent::Message { device: peer.clone(), text: text.to_string(), clipboard, incoming: false });
        self.note_text(text, &peer, &peer, false, clipboard);
        Ok(())
    }

    /// 网页上传到本机的文件：只在桌面长出「已收完」的气泡。
    ///
    /// 与原版 `sigReceiveFile` + `sigReceiveFileSuccess` 一致：不推 WebSocket（上传者
    /// 就是网页自己），也不登记到 `active`——那张表只给「真需要向网页播报」的传输用。
    pub fn note_web_upload(&self, name: &str, path: &str, size: i64) {
        let device = Device { name: "网页设备".to_string(), mode: MODE_WIN, ..Default::default() };
        let info = TransferInfo { id: crypto::new_id(), device, name: name.to_string(), length: size, is_dir: false, sending: false };
        self.register_cancel(&info.id);
        self.publish(NetEvent::Start(info.clone()));
        self.note_file(&info, true, 0, "");
        self.note_file_done(&info, path);
        self.release_cancel(&info.id);
        self.publish(NetEvent::Finish { id: info.id, ok: true, path: path.to_string() });
    }

    /// 用户在网页授权框里做了决定。同意才写库；`custom` 恒为 `false`，与原版
    /// `addToken(token, false, ip)` 一致（那个「本 IP 免确认」并没能从 UI 里勾上）。
    pub fn resolve_web_grant(&self, token: &str, ip: &str, accept: bool) {
        if !accept || token.is_empty() {
            return;
        }
        if let Some(db) = crate::web::tokens::shared() {
            let _ = db.add(token, false, ip);
        }
    }

    // —— 历史库 ————————————————————————————————————————————

    fn note_text(&self, text: &str, peer: &Device, from: &Device, incoming: bool, _clipboard: bool) {
        let entry = Entry {
            id: crypto::new_id(),
            kind: TYPE_TEXT,
            message: text.to_string(),
            device_name: peer.name.clone(),
            device_id: peer.uuid.clone(),
            data_version: peer.data_version as i64,
            to_user: if incoming { from.name.clone() } else { peer.name.clone() },
            is_left: incoming,
            dev_mode: peer.mode as i64,
            time_stamp: now_ms(),
            completed: true,
            received: true,
            progress: 100,
            ..Default::default()
        };
        self.write_history(&entry);
    }

    /// 记录一条文件消息（发送与接收共用），进度与完成度随传输更新。
    /// `path` 是发送侧的源文件路径——中途退出后重开要靠它找回原件；接收侧传空串。
    pub fn note_file(&self, info: &TransferInfo, received: bool, progress: i32, path: &str) {
        let entry = Entry {
            id: info.id.clone(),
            kind: TYPE_FILE,
            message: info.name.clone(),
            device_name: info.device.name.clone(),
            device_id: info.device.uuid.clone(),
            data_version: info.device.data_version as i64,
            to_user: info.device.name.clone(),
            is_left: !info.sending,
            is_file: true,
            dev_mode: info.device.mode as i64,
            file_path: path.to_string(),
            completed: progress >= 100,
            received,
            file_size: info.length,
            size_str: size_str(info.length),
            time_stamp: now_ms(),
            progress: i64::from(progress.clamp(0, 99)),
            ..Default::default()
        };
        self.write_history(&entry);
    }

    /// 传输进度：广播事件，顺带刷新库里的进度列。
    pub fn publish_progress(&self, id: &str, progress: i32) {
        self.publish(NetEvent::Progress { id: id.to_string(), progress });
        self.update_progress(id, progress);
    }

    /// 只更新历史行的进度列（每跨 1% 才调一次，成本可忽略）。中途退出后重开，
    /// 未完成的传输能还原到断点进度，而不是永远停在 0。
    fn update_progress(&self, id: &str, progress: i32) {
        if !self.config().save_message {
            return;
        }
        let mut guard = self.history.lock().expect("历史库锁未被污染");
        if let Some(db) = guard.as_mut() {
            let _ = db.touch_progress(id, i64::from(progress.clamp(0, 99)));
        }
    }

    /// 完成时补路径与 100% 进度。
    pub fn note_file_done(&self, info: &TransferInfo, path: &str) {
        let entry = Entry {
            id: info.id.clone(),
            kind: TYPE_FILE,
            message: info.name.clone(),
            device_name: info.device.name.clone(),
            device_id: info.device.uuid.clone(),
            data_version: info.device.data_version as i64,
            to_user: info.device.name.clone(),
            is_left: !info.sending,
            is_file: true,
            dev_mode: info.device.mode as i64,
            file_path: path.to_string(),
            completed: true,
            received: true,
            file_size: info.length,
            size_str: size_str(info.length),
            time_stamp: now_ms(),
            progress: 100,
            ..Default::default()
        };
        self.write_history(&entry);
    }

    fn write_history(&self, entry: &Entry) {
        if !self.config().save_message {
            return;
        }
        let mut guard = self.history.lock().expect("历史库锁未被污染");
        if let Some(db) = guard.as_mut() {
            let _ = db.insert(entry);
        }
    }

    /// 读某台设备的历史（首屏加载），按设备 `uuid` 归属。
    pub fn history_of(&self, device_id: &str, limit: i64) -> Vec<Entry> {
        let mut guard = self.history.lock().expect("历史库锁未被污染");
        match guard.as_mut() {
            Some(db) => db.recent(device_id, limit).unwrap_or_default(),
            None => Vec::new(),
        }
    }

    /// 读某台设备**早于** `before_ts` 的一页历史（向上自动翻页），按设备 `uuid` 归属。
    pub fn history_before(&self, device_id: &str, before_ts: i64, limit: i64) -> Vec<Entry> {
        let mut guard = self.history.lock().expect("历史库锁未被污染");
        match guard.as_mut() {
            Some(db) => db.older(device_id, before_ts, limit).unwrap_or_default(),
            None => Vec::new(),
        }
    }

    /// 跨全部设备取最近 `limit` 条（合并流首屏），按时间正序。
    pub fn history_all(&self, limit: i64) -> Vec<Entry> {
        let mut guard = self.history.lock().expect("历史库锁未被污染");
        match guard.as_mut() {
            Some(db) => db.recent_all(limit).unwrap_or_default(),
            None => Vec::new(),
        }
    }

    /// 跨全部设备取**早于** `before_ts` 的一页历史（合并流向上翻页），按时间正序。
    pub fn history_before_all(&self, before_ts: i64, limit: i64) -> Vec<Entry> {
        let mut guard = self.history.lock().expect("历史库锁未被污染");
        match guard.as_mut() {
            Some(db) => db.older_all(before_ts, limit).unwrap_or_default(),
            None => Vec::new(),
        }
    }

    /// 软删某台设备的全部历史记录——「清空消息」要落库。
    ///
    /// 与原版 `on_actionClearAll_triggered` / `on_actionClearMessage_triggered` 同一口径：那两个
    /// 都是先 `MessageDB::delete*` 再清列表控件。只清内存的话，往上翻页和重启都会把
    /// 「已清空」的历史一页页拉回来。`device_id` 为空直接不办事：库里那批无主旧行不属于任何人。
    pub fn clear_history(&self, device_id: &str) {
        if device_id.is_empty() {
            return;
        }
        let mut guard = self.history.lock().expect("历史库锁未被污染");
        if let Some(db) = guard.as_mut() {
            let _ = db.remove_device(device_id);
        }
    }

    // —— UDP 命令字转发 ————————————————————————————————————

    /// 收到文本消息（UDP 与新式 TCP 共用）：解密、入历史、报事件。
    pub fn on_message(&self, device: Device, encoded: &str, clipboard: bool) {
        let key = self.config().message_key;
        let text = crypto::dec_message(encoded, &key).unwrap_or_default();
        if text.is_empty() {
            return;
        }
        let from = self.pick_local(&device.ip).unwrap_or_default();
        self.publish(NetEvent::Message { device: device.clone(), text: text.clone(), clipboard, incoming: true });
        self.note_text(&text, &device, &from, true, clipboard);
        // 网页同步收到这一条（原版就在 UDP 收包处直接推 WS）；剪贴板消息不推，
        // 因为原版 `UDP_MESSAGE_TO_CLIPBOARD` 那个分支里根本没有 `sendWebSocketMessage`
        if !clipboard {
            crate::web::sessions::push_message(&text, &device.name, "", 0, device.mode, "", true, false, false);
        }
    }
}

/// 待确认接收里各顶层条目的展示键。
fn ids_of(kind: &Pending) -> Vec<String> {
    match kind {
        Pending::Modern { meta, .. } => meta.files.iter().map(|file| receive_id(&file.file_id)).collect(),
        Pending::Legacy { files, .. } => files.iter().map(|file| file.id.clone()).collect(),
    }
}

/// 新版接收的展示键：对方没带 `fileId` 时自己生一个，保证气泡有唯一归属。
pub fn receive_id(file_id: &str) -> String {
    if file_id.is_empty() { crypto::new_id() } else { file_id.to_string() }
}

/// 由接收元数据拼出展示用的传输信息（`sending` 恒为 `false`）。
pub fn receive_info(meta: &walk::WireFile, id: &str, from: &Device) -> TransferInfo {
    TransferInfo {
        id: id.to_string(),
        device: from.clone(),
        name: meta.name.clone(),
        length: meta.length,
        is_dir: meta.file_type == proto::FILE_TYPE_FOLDER,
        sending: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> Arc<Engine> {
        let mut config = Config::default();
        config.save_path = std::env::temp_dir().join(format!("lanshare-engine-{}", std::process::id())).to_string_lossy().into_owned();
        config.unique_uuid = "uuid-test".to_string();
        config.allow_same_host_test = true;
        let engine = Arc::new(Engine::for_test(config));
        engine
    }

    #[test]
    fn events_are_queued_in_order_and_drained_once() {
        let engine = engine();
        engine.publish(NetEvent::Devices(Vec::new()));
        engine.publish(NetEvent::Progress { id: "a".into(), progress: 1 });
        let list = engine.take_events(Duration::from_millis(10));
        assert_eq!(list.len(), 2);
        assert!(engine.take_events(Duration::from_millis(10)).is_empty());
    }

    #[test]
    fn take_events_wakes_up_when_an_event_arrives() {
        let engine = engine();
        let other = Arc::clone(&engine);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            other.publish(NetEvent::Progress { id: "late".into(), progress: 50 });
        });
        let list = engine.take_events(Duration::from_secs(2));
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn cancel_flags_are_isolated_per_file_id() {
        let engine = engine();
        let flag = engine.register_cancel("f1");
        engine.register_cancel("f2");
        assert!(!engine.cancelled("f1"));
        engine.cancel("f1");
        assert!(flag.load(Ordering::SeqCst));
        assert!(engine.cancelled("f1"));
        assert!(!engine.cancelled("f2"));
        engine.release_cancel("f1");
        assert!(!engine.cancelled("f1"));
    }

    #[test]
    fn device_table_updates_are_published() {
        let engine = engine();
        let device = Device { ip: "10.0.0.9".into(), port: 5856, name: "手机".into(), ..Default::default() };
        engine.touch_device(device.clone());
        assert_eq!(engine.devices().len(), 1);
        let list = engine.take_events(Duration::from_millis(10));
        assert!(matches!(list.first(), Some(NetEvent::Devices(_))));
        // 完全相同的设备不再重复推送
        engine.touch_device(device);
        assert!(engine.take_events(Duration::from_millis(10)).is_empty());
    }

    #[test]
    fn pick_local_prefers_the_matching_interface() {
        let engine = engine();
        let uuid = engine.unique_uuid();
        let matched = Device {
            ip: "192.168.1.2".into(),
            net_mask: "255.255.255.0".into(),
            port: 5856,
            uuid: uuid.clone(),
            ..Default::default()
        };
        let other = Device { ip: "10.0.0.2".into(), net_mask: "255.0.0.0".into(), port: 5856, uuid, ..Default::default() };
        *engine.local.lock().unwrap() = vec![other, matched.clone()];
        assert_eq!(engine.pick_local("192.168.1.7").expect("有匹配网卡").ip, matched.ip);
        // 匹配不到时退回第一块网卡，与原版一致
        assert_eq!(engine.pick_local("172.16.0.1").expect("有兜底网卡").ip, "10.0.0.2");
    }

    #[test]
    fn expired_devices_are_reported_removed() {
        let engine = engine();
        let mut device = Device { ip: "10.0.0.9".into(), port: 5856, ..Default::default() };
        device.last_seen = now_ms() - 30_000;
        engine.touch_device(device);
        let removed = engine.cleanup_expired();
        assert_eq!(removed.len(), 1);
        assert!(engine.devices().is_empty());
    }

    /// 只起 TCP 的一台引擎：端口交给系统分配，UDP 不参与（测试不该往局域网广播心跳）。
    /// 网卡手工写成环回，保证不依赖真机的网络环境。
    fn serving(tag: &str, uid: &str, accept: bool) -> (Arc<Engine>, u16) {
        let mut config = Config::default();
        config.save_path = std::env::temp_dir().join(format!("lanshare-e2e-{tag}")).to_string_lossy().into_owned();
        let _ = std::fs::remove_dir_all(&config.save_path);
        config.unique_uuid = uid.to_string();
        config.client_name = tag.to_string();
        config.tcp_port = 0;
        config.udp_port = 0;
        // 不碰 %LOCALAPPDATA% 里的历史库
        config.save_message = false;
        config.accept_recv_files = accept;
        config.allow_same_host_test = true;
        // 端到端顺带验证数据块混淆
        config.enc_data = true;
        let engine = Arc::new(Engine::for_test(config));
        let workers = tcp::spawn(Arc::clone(&engine));
        let port = engine.listener().expect("监听已就绪").local_addr().expect("读得到端口").port();
        *engine.local.lock().expect("本机房锁未被污染") = vec![Device {
            name: tag.to_string(),
            ip: "127.0.0.1".into(),
            net_mask: "255.0.0.0".into(),
            port: port as i32,
            mode: device::MODE_WIN,
            uuid: uid.to_string(),
            data_version: DATA_VERSION,
            ..Default::default()
        }];
        *engine.workers.lock().expect("线程表锁未被污染") = workers;
        (engine, port)
    }

    /// 攒事件直到出现指定 id 的收尾：传输跑在工作线程里，一次 `take_events` 收不齐。
    fn wait_finish(engine: &Engine, id: &str, ok: bool) -> bool {
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

    /// 攒事件直到收到指定文本的入站消息。
    fn wait_message(engine: &Engine, text: &str) -> bool {
        for _ in 0..100 {
            for event in engine.take_events(Duration::from_millis(50)) {
                if let NetEvent::Message { text: got, incoming: true, .. } = &event {
                    if got == text {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// 端到端：同机两个引擎，V5 发一个文件夹过去，再用旧版 TCP 帧发一条长文本。
    #[test]
    fn two_engines_exchange_a_folder_and_a_long_message() {
        let (receiver, receive_port) = serving("recv", "uuid-recv", true);
        let (sender, send_port) = serving("send", "uuid-send", true);
        // 真机上这一步是 UDP 心跳干的，测试里直接登记
        sender.touch_device(Device {
            name: "recv".into(),
            ip: "127.0.0.1".into(),
            port: receive_port as i32,
            mode: device::MODE_WIN,
            uuid: "uuid-recv".into(),
            data_version: DATA_VERSION,
            ..Default::default()
        });

        let dir = std::env::temp_dir().join("lanshare-e2e-src");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("相册/子")).unwrap();
        std::fs::write(dir.join("相册/a.txt"), b"alpha").unwrap();
        std::fs::write(dir.join("相册/子/b.txt"), b"beta-content").unwrap();

        let infos = sender.send_files("127.0.0.1", receive_port as i32, &[dir.join("相册")]).expect("设备在线就能发");
        assert_eq!(infos.len(), 1, "一个文件夹就是一条气泡");
        let id = infos[0].id.clone();
        assert!(!id.is_empty(), "V5 的 fileId 必须由发送端生成");
        assert!(wait_finish(&sender, &id, true), "发送端要看到收尾");
        assert!(wait_finish(&receiver, &id, true), "接收端沿用同一个 fileId");

        let saved = receiver.config().save_path;
        let root = std::path::Path::new(&saved);
        assert_eq!(std::fs::read(root.join("相册/a.txt")).unwrap(), b"alpha");
        assert_eq!(std::fs::read(root.join("相册/子/b.txt")).unwrap(), b"beta-content", "子目录要按相对路径建出来");
        assert!(!receiver.cancelled(&id), "收尾后取消标记要释放");
        // 握手 JSON 里的 ip:port 让接收端也把发送端登记进表
        assert!(receiver.device_at("127.0.0.1", send_port as i32).is_some(), "{:#?}", receiver.devices());

        // 超过 700 字节的消息走旧版 TCP 帧
        let text = "甲".repeat(800);
        sender.send_message("127.0.0.1", receive_port as i32, &text, false).expect("长消息走 TCP 旧式帧");
        assert!(wait_message(&receiver, &text), "接收端要能解密出原文");

        sender.shutdown();
        receiver.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(root);
    }

    /// 端到端：接收端要用户确认时，拒绝应当让两端都以失败收尾。
    #[test]
    fn a_refused_transfer_fails_on_both_sides() {
        let (receiver, receive_port) = serving("deny-recv", "uuid-deny-recv", false);
        let (sender, _) = serving("deny-send", "uuid-deny-send", true);
        sender.touch_device(Device {
            ip: "127.0.0.1".into(),
            port: receive_port as i32,
            uuid: "uuid-deny-recv".into(),
            data_version: DATA_VERSION,
            ..Default::default()
        });
        let dir = std::env::temp_dir().join("lanshare-e2e-deny");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"12345").unwrap();

        let infos = sender.send_files("127.0.0.1", receive_port as i32, &[dir.join("a.txt")]).expect("设备在线就能发");
        let id = infos[0].id.clone();
        // 接收端弹出确认框：文件名与总大小都要给 UI 看
        let mut offer = None;
        for _ in 0..100 {
            offer = receiver.take_events(Duration::from_millis(50)).into_iter().find_map(|event| match event {
                NetEvent::Receive(offer) => Some(offer),
                _ => None,
            });
            if offer.is_some() {
                break;
            }
        }
        let offer = offer.expect("关了自动接收就该等用户确认");
        assert_eq!(offer.names, vec!["a.txt".to_string()]);
        assert_eq!(offer.total, 5);
        receiver.resolve_receive(offer.key, false);
        assert!(wait_finish(&receiver, &id, false), "接收端要按失败收尾");
        assert!(wait_finish(&sender, &id, false), "发送端读到 1103 也要失败");
        let saved = receiver.config().save_path;
        assert!(!std::path::Path::new(&saved).join("a.txt").exists(), "拒绝了就不该落盘");

        sender.shutdown();
        receiver.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&saved);
    }

    /// 端到端：接收端留着半截分段时，重发要跳过已完成的字节并拼出完整文件。
    ///
    /// 这条测的是「发送端稳定 ID → 接收端偏移量表 → 发送端 seek 续推」整条链，
    /// 等价于真机上中途杀进程再重发的场景，只是把「杀进程」换成了预先写好分段。
    #[test]
    fn a_half_finished_part_resumes_over_the_real_stream() {
        let (receiver, receive_port) = serving("resume-recv", "uuid-resume-recv", true);
        let (sender, _) = serving("resume-send", "uuid-resume-send", true);
        let to = Device {
            name: "resume-recv".into(),
            ip: "127.0.0.1".into(),
            port: receive_port as i32,
            mode: device::MODE_WIN,
            uuid: "uuid-resume-recv".into(),
            data_version: DATA_VERSION,
            ..Default::default()
        };
        sender.touch_device(to.clone());

        let dir = std::env::temp_dir().join("lanshare-e2e-resume");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"12345").unwrap();

        // 用与 `send_files` 完全相同的方式取 ID：稳定 ID 是分段能被命中的前提
        let from = sender.pick_local("127.0.0.1").expect("测试里备好了环回网卡");
        let path = dir.join("a.txt");
        let mut items = to_items(&[path.clone()]);
        fsend5::prepare_ids(&mut items, &from, &to, true);
        let id = items[0].file_id.clone();
        let store = resume::ResumeStore::new(&receiver.config().save_path);
        std::fs::write(store.file_part(&id), b"123").expect("写入半截分段");

        let infos = sender.send_files("127.0.0.1", receive_port as i32, &[path]).expect("设备在线就能发");
        assert_eq!(infos[0].id, id, "重发同一条内容必须落在同一个 fileId 上");
        assert!(wait_finish(&sender, &id, true), "发送端要从断点推完");
        assert!(wait_finish(&receiver, &id, true), "接收端要收到剩下的两个字节");

        let saved = receiver.config().save_path;
        let root = std::path::Path::new(&saved);
        assert_eq!(std::fs::read(root.join("a.txt")).unwrap(), b"12345", "拼接后不能被截断或重复");
        assert!(!store.file_part(&id).exists(), "续传完成后分段要被搬走");

        sender.shutdown();
        receiver.shutdown();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(root);
    }
}
