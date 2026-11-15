//! `ChatApp` 的构造与只读访问器：界面要的每一个值都从这里换算。

use std::collections::HashMap;
use std::sync::Arc;

use windui::event::system_prefers_dark;

use crate::lanshare::config::Config;
use crate::lanshare::device::Device;
use crate::lanshare::Engine;
use crate::model::{self, Conversation};
use crate::theme;

use crate::app::{ip_sort_key, ChatApp, ChatMode, ThemePref};

impl ChatApp {
    /// 起引擎、灌已有设备、回溯历史。只能在 UI 线程上调一次。
    pub(super) fn bootstrap() -> Self {
        let mut app = Self::from_engine(&Engine::start(Config::load()));
        // 一启动就把已有设备建好会话，侧栏不会空着
        let snapshot = app.devices.clone();
        for device in snapshot {
            app.ensure(&device);
        }
        let first = app.visible().first().map(|conversation| conversation.id);
        app.selected = first;
        // 默认就是合并模式：首屏从历史库拉一页全局流，打开即落到最新消息。
        app.ensure_merged_loaded();
        app
    }

    /// 由一个已就位的引擎搭出初始状态机：不灌设备、不选会话。单测靠它造场景。
    pub(super) fn from_engine(engine: &Arc<Engine>) -> Self {
        let settings = engine.config();
        let theme_pref = ThemePref::from_theme(&settings.theme);
        let local = engine.local_devices();
        let devices = engine.devices();
        Self {
            engine: Arc::clone(engine),
            conversations: Vec::new(),
            index: HashMap::new(),
            devices,
            local,
            offers: Vec::new(),
            grants: Vec::new(),
            sources: HashMap::new(),
            selected: None,
            drafts: HashMap::new(),
            query: String::new(),
            clipboard: false,
            mode: ChatMode::default(),
            merged: Vec::new(),
            merged_loaded: false,
            merged_has_more: false,
            merged_cursor: 0,
            settings,
            qr_open: false,
            qr_pick: None,
            qr_v6: None,
            free_copy: None,
            pick_paths: None,
            update: None,
            notice: None,
            theme_pref,
            system_dark: system_prefers_dark(),
            revision: 0,
            reveal: 0,
            next_conv: 1,
            next_msg: 1,
        }
    }

    pub fn engine(&self) -> &Arc<Engine> {
        &self.engine
    }

    /// 状态改动计数，分发器用它决定要不要重算。
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub(super) fn touch(&mut self) {
        self.revision += 1;
    }

    pub fn selected(&self) -> Option<u64> {
        self.selected
    }

    pub fn find(&self, id: u64) -> Option<&Conversation> {
        self.conversations.iter().find(|item| item.id == id)
    }

    /// 当前会话，没有设备时是 `None`。
    pub fn current(&self) -> Option<&Conversation> {
        self.selected.and_then(|id| self.find(id))
    }

    /// 当前聊天列表模式。
    pub fn mode(&self) -> ChatMode {
        self.mode
    }

    /// 在线设备数：合并模式的状态行与「能不能群发」的判据。
    pub fn online_count(&self) -> usize {
        self.devices.len()
    }

    /// 合并（全部设备）流的当前窗口：跨设备有界分页，旧→新。每行自带展示用的设备名与类型。
    pub fn merged_feed(&self) -> &[model::MergedItem] {
        &self.merged
    }

    /// 合并流库里是否还有更早的一页可翻（驱动顶部探针）。
    pub fn has_more_merged(&self) -> bool {
        self.merged_has_more
    }

    /// `Device::uuid` -> 会话号。合并流每行只带 `uuid`（历史库的归属键），而跳转会话要的是
    /// 会话号，靠这张表换算过去。uuid 为空的会话（旧库里无主的那台）不进表：跳不到的
    /// 目标不如不给入口，省得点了没反应。
    pub fn conversation_ids_by_uuid(&self) -> HashMap<&str, u64> {
        self.conversations
            .iter()
            .filter(|conversation| !conversation.device.uuid.is_empty())
            .map(|conversation| (conversation.device.uuid.as_str(), conversation.id))
            .collect()
    }

    /// 合并模式标题栏下的说明行：在线数 + 已加载条数 + 群发提示。
    pub fn merged_head_detail(&self) -> String {
        format!("{} 台设备 · 已加载 {} 条消息 · 发消息将群发", self.devices.len(), self.merged.len())
    }

    pub fn clipboard_on(&self) -> bool {
        self.clipboard
    }

    pub fn local(&self) -> &[Device] {
        &self.local
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub fn settings(&self) -> &Config {
        &self.settings
    }

    pub fn is_online(&self, key: &str) -> bool {
        self.devices.iter().any(|device| device.key() == key)
    }

    /// 全部会话的未读总数。
    pub fn unread_total(&self) -> usize {
        self.conversations.iter().map(|conversation| conversation.unread).sum()
    }

    /// 新消息到来时递增，`Ui::reveal` 绑到最后一行上把视口滚到底。
    pub fn reveal_key(&self) -> u64 {
        self.reveal
    }

    /// 当前配色档案（已解析过「跟随系统」）。视图层拿它取每一档颜色。
    pub fn skin(&self) -> theme::Skin {
        self.theme_pref.skin(self.system_dark)
    }

    /// 解析过「跟随系统」之后的实际深浅色。现在只是配色的一个属性，不再单独存着。
    pub fn is_dark(&self) -> bool {
        self.skin().dark
    }

    pub fn theme_label(&self) -> &'static str {
        self.theme_pref.label()
    }

    /// 窗口标题：带上第一块网卡的名字与地址，任务栏上一眼认出是哪台机器。
    pub fn title_text(&self) -> String {
        match self.local.first() {
            Some(device) => format!("LANShare · {} · {}", device.name, device.ip),
            None => format!("LANShare · {}", self.settings.client_name),
        }
    }

    /// 本机活动地址：流量最大的那块网卡（`local.first()`，`nics()` 已按流量降序）的
    /// `ip:tcp端口`，供侧栏「本机」一行显示与一键复制。不再罗列全部网卡、也不再带掩码与协议版本。
    /// 多网卡全集仍可在「网页二维码」对话框与设置里查看。没有可用网卡时给一句提示。
    pub fn banner_text(&self) -> String {
        match self.local.first() {
            Some(device) => format!("{}:{}", device.ip, device.port),
            None => "未连接网络".to_owned(),
        }
    }

    /// 侧栏底部的状态条。
    pub fn status_text(&self) -> String {
        // 括号里是解析后的方向：选「跟随系统」时看得出系统当前是哪档。
        let dark = if self.is_dark() { "深色" } else { "浅色" };
        format!(
            "在线 {} 台 · 未读 {} 条 · {} · {}（{dark}）",
            self.devices.len(),
            self.unread_total(),
            self.mode.label(),
            self.theme_label()
        )
    }

    /// 会话标题栏下的说明行。
    pub fn head_detail(&self, conversation: &Conversation) -> String {
        let online = if self.is_online(&conversation.key()) { "在线" } else { "离线" };
        format!("{} · {} · {online}", conversation.subtitle(), conversation.battery_text())
    }

    pub fn placeholder(&self) -> String {
        match self.mode {
            ChatMode::Merged => "给所有在线设备发消息（群发）…".to_owned(),
            ChatMode::PerDevice => match self.current() {
                Some(conversation) => format!("给 {} 发消息…", conversation.name()),
                None => "先在左边选一台设备…".to_owned(),
            },
        }
    }

    /// 搜索过滤后的会话顺序：在线优先，其次按设备 IP 数值升序（同 IP 再比端口）。
    pub fn visible(&self) -> Vec<&Conversation> {
        let query = self.query.trim().to_lowercase();
        let mut list: Vec<&Conversation> = self
            .conversations
            .iter()
            .filter(|conversation| {
                query.is_empty()
                    || conversation.name().to_lowercase().contains(&query)
                    || conversation.key().to_lowercase().contains(&query)
                    || conversation.preview().to_lowercase().contains(&query)
            })
            .collect();
        list.sort_by(|left, right| {
            let online = |item: &Conversation| self.is_online(&item.key()) as u8;
            online(right)
                .cmp(&online(left))
                .then_with(|| ip_sort_key(&left.device.ip).cmp(&ip_sort_key(&right.device.ip)))
                .then_with(|| left.device.port.cmp(&right.device.port))
        });
        list
    }
}
