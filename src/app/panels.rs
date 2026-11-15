//! 各个对话框面板的状态：接收确认、网页授权、二维码、自由复制、选设备发送。

use crate::lanshare::walk::size_str;
use crate::lanshare::ReceiveOffer;
use crate::model::Conversation;

use crate::app::{ip_sort_key, ChatApp};

/// 「选择设备发送」的说明行最多列几个文件名，多了用「等 N 项」收尾。
const PICKER_PREVIEW: usize = 5;

impl ChatApp {
    pub fn pending_offer(&self) -> Option<&ReceiveOffer> {
        self.offers.first()
    }

    /// 排队最靠前的那条网页授权请求。
    pub fn pending_grant(&self) -> Option<(&str, &str)> {
        self.grants.first().map(|(token, ip)| (token.as_str(), ip.as_str()))
    }

    /// 「网页二维码」对话框开着没。
    pub fn qr_open(&self) -> bool {
        self.qr_open
    }

    /// 「自由复制文本」对话框要显示的正文；`None` = 关着。
    pub fn free_copy(&self) -> Option<&str> {
        self.free_copy.as_deref()
    }

    /// 「选择设备发送」对话框开着没。
    pub fn picker_open(&self) -> bool {
        self.pick_paths.is_some()
    }

    /// 对话框顶上那句「待发几项、都是什么」。
    ///
    /// 只拼名字不 `stat`：这行在每次同步都会重算，不能往渲染路径里塞系统调用。
    pub fn picker_files(&self) -> String {
        let Some(paths) = self.pick_paths.as_ref() else { return String::new() };
        let names: Vec<String> = paths
            .iter()
            .take(PICKER_PREVIEW)
            .map(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string())
            })
            .collect();
        let mut text = format!("待发 {} 项：{}", paths.len(), names.join("、"));
        if paths.len() > PICKER_PREVIEW {
            text.push_str(" 等");
        }
        text
    }

    /// 对话框里的设备行：当前在线设备对应的会话，按 IP 数值再端口升序（与侧栏同一套口径）。
    /// 没建过会话的设备会被跳过：进不了列表就点不到，比放一行空白行安全。
    pub fn picker_rows(&self) -> Vec<&Conversation> {
        let mut list: Vec<&Conversation> = self
            .devices
            .iter()
            .filter_map(|device| self.index.get(&device.key()).and_then(|id| self.find(*id)))
            .collect();
        list.sort_by(|left, right| {
            ip_sort_key(&left.device.ip)
                .cmp(&ip_sort_key(&right.device.ip))
                .then_with(|| left.device.port.cmp(&right.device.port))
        });
        list
    }

    /// 可选的网卡列表：去重、丢掉空串，与原版 `refresh()` 里攒的那个 `QStringList` 一样。
    ///
    /// 末尾再挂上后台查回的公网 IPv6（如果有）：只多这一项展示，不改上面 `local` 的 IPv4
    /// 网卡表——实际收发、绑定、广播仍只认 IPv4，与「只是二维码展示」的诉求一致。
    pub fn qr_ips(&self) -> Vec<String> {
        let mut list: Vec<String> = Vec::new();
        for device in &self.local {
            if !device.ip.is_empty() && !list.contains(&device.ip) {
                list.push(device.ip.clone());
            }
        }
        if let Some(v6) = &self.qr_v6 {
            if !v6.is_empty() && !list.contains(v6) {
                list.push(v6.clone());
            }
        }
        list
    }

    /// 下拉该停在哪一项：按记住的文字找位置，找不到就回到第一项。
    pub fn qr_index(&self) -> usize {
        let ips = self.qr_ips();
        let index = self.qr_pick.as_deref().and_then(|pick| ips.iter().position(|ip| ip == pick)).unwrap_or(0);
        index.min(ips.len().saturating_sub(1))
    }

    /// 侧栏要显示的网页入口。与原版 `updateWebServiceIp()` 同一条判断：开了网页服务、
    /// 且有网卡才给出 `http://<第一块网卡>:<tcp 端口>`，否则空串（整行不显示）。
    pub fn web_url(&self) -> String {
        if !self.settings.web_service {
            return String::new();
        }
        self.local
            .first()
            .map(|device| crate::web::qr::web_url(&device.ip, self.settings.tcp_port))
            .unwrap_or_default()
    }

    /// 接收确认对话框的正文：总大小加逐条文件名。
    pub fn offer_body(&self) -> String {
        let Some(offer) = self.offers.first() else { return String::new() };
        let mut lines = vec![format!("共 {} 项 · {}", offer.names.len(), size_str(offer.total))];
        lines.extend(offer.names.iter().enumerate().map(|(index, name)| format!("{}. {}", index + 1, name)));
        lines.join("\n")
    }

    pub(super) fn resolve(&mut self, key: u64, accept: bool) {
        self.engine.resolve_receive(key, accept);
        self.offers.retain(|offer| offer.key != key);
        if !accept {
            self.notice = Some("已拒绝这次文件发送".to_owned());
        }
        self.touch();
    }

    /// 用户在网页授权框里做了决定。拒绝时不写库：那张令牌从此问不出 `pass=true`，
    /// 网页会始终停在授权页，与原版一致。
    pub(super) fn resolve_grant(&mut self, token: &str, accept: bool) {
        let ip = self
            .grants
            .iter()
            .find(|(pending, _)| pending == token)
            .map(|(_, ip)| ip.clone())
            .unwrap_or_default();
        self.engine.resolve_web_grant(token, &ip, accept);
        self.grants.retain(|(pending, _)| pending != token);
        self.notice =
            Some(if accept { "已同意该网页客户端访问".to_owned() } else { "已拒绝该网页客户端访问".to_owned() });
        self.touch();
    }

    pub(super) fn open_qr(&mut self) {
        // 打开时先抓一份最新的本机表：对话框里那个下拉要的就是这一份
        self.local = self.engine.local_devices();
        self.qr_pick = None;
        self.qr_open = true;
        self.touch();
    }

    pub(super) fn close_qr(&mut self) {
        self.qr_open = false;
        self.touch();
    }

    pub(super) fn open_free_copy(&mut self, text: String) {
        self.free_copy = Some(text);
        self.touch();
    }

    pub(super) fn close_free_copy(&mut self) {
        self.free_copy = None;
        self.touch();
    }

    /// 对话框里的「刷新」：重新枚举一次网卡。选中项按文字保持，与原版 `refresh()` 一致。
    pub(super) fn refresh_qr(&mut self) {
        self.local = self.engine.refresh_local();
        self.touch();
    }

    /// 侧栏那个「开启网页功能」勾选框：改了立即写进引擎并落盘。分流是按连接现读配置的，
    /// 所以不用重启就生效，与原版 `on_webService_clicked()` 一样。
    pub(super) fn set_web_service(&mut self, on: bool) {
        if self.settings.web_service == on {
            return;
        }
        self.settings.web_service = on;
        let mut config = self.engine.config();
        config.web_service = on;
        self.engine.set_config(config);
        self.touch();
    }

    /// 下拉换了网卡。值没变就不 `touch`：控件写信号会自增版本，别和心跳绕成死循环。
    pub(super) fn set_qr_index(&mut self, index: usize) {
        let Some(ip) = self.qr_ips().get(index).cloned() else { return };
        if self.qr_pick.as_deref() == Some(ip.as_str()) {
            return;
        }
        self.qr_pick = Some(ip);
        self.touch();
    }

    /// 后台查回的公网 IPv6 落地。只在真变了时 `touch`，免得重复回包把列表刷得反复重建。
    pub(super) fn set_qr_v6(&mut self, ip: Option<String>) {
        if self.qr_v6 == ip {
            return;
        }
        self.qr_v6 = ip;
        self.touch();
    }
}
