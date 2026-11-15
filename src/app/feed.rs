//! 会话与消息流的装载：建会话、往上翻页、追加气泡，以及引擎事件落到状态机。

use std::path::PathBuf;

use crate::lanshare::db::Entry;
use crate::lanshare::device::Device;
use crate::lanshare::NetEvent;
use crate::model::{self, Conversation, Message, Side, Status};

use crate::app::{ChatApp, ChatMode, PAGE_FIRST, PAGE_STEP};

impl ChatApp {
    pub(super) fn find_mut(&mut self, id: u64) -> Option<&mut Conversation> {
        self.conversations.iter_mut().find(|item| item.id == id)
    }

    pub(super) fn take_msg_id(&mut self) -> u64 {
        let id = self.next_msg;
        self.next_msg += 1;
        id
    }

    /// 设备首次出现就建会话，并把历史库里的记录补上；已在表里就刷新设备信息。
    pub(super) fn ensure(&mut self, device: &Device) -> u64 {
        let key = device.key();
        if let Some(id) = self.index.get(&key).copied() {
            if let Some(conversation) = self.find_mut(id) {
                conversation.device = device.clone();
            }
            return id;
        }
        let id = self.next_conv;
        self.next_conv += 1;
        let mut conversation = Conversation::new(id, device.clone());
        if self.engine.config().save_message && !device.uuid.is_empty() {
            let entries = self.engine.history_of(&device.uuid, PAGE_FIRST);
            let loaded = entries.len() as i64;
            crate::utils::trace_log::trace(&format!("ensure new conv for uuid loaded={loaded}"));
            for entry in entries {
                let mut message = Message::from_entry(&entry);
                message.id = self.take_msg_id();
                conversation.messages.push(message);
            }
            // `history_of` 返回正序（旧→新）：最旧一条在头部，它就是往前翻页的游标。
            conversation.history_cursor = conversation.messages.first().map(|message| message.at).unwrap_or(0);
            // 取满一整页 = 库里大概率还有更早的；没取满就是到顶了，不再挂探针。
            conversation.has_more_history = loaded >= PAGE_FIRST;
        }
        self.conversations.push(conversation);
        self.index.insert(key, id);
        self.touch();
        id
    }

    /// 往上翻一页更早的历史，前插到已有窗口头部。返回新增条数（`0` = 没有更多）。
    ///
    /// 顶部探针滑到顶时调；不推 `reveal`，也不刷新未读——翻页是往回看，不该把视口弹到底。
    pub(super) fn load_older(&mut self, id: u64) -> usize {
        // 先把翻页要用的信息取出来，别在借用会话的同时又去动 `self`（`take_msg_id`/`touch`）。
        let (device_id, cursor, has_more) = match self.find_mut(id) {
            Some(conversation) => (
                conversation.device.uuid.clone(),
                conversation.history_cursor,
                conversation.has_more_history && conversation.history_cursor > 0,
            ),
            None => return 0,
        };
        if !has_more || device_id.is_empty() {
            crate::utils::trace_log::trace(&format!("load_older id={id} bail: has_more={has_more} cursor={cursor}"));
            return 0;
        }
        let entries = self.engine.history_before(&device_id, cursor, PAGE_STEP);
        let loaded = entries.len();
        crate::utils::trace_log::trace(&format!("load_older id={id} cursor={cursor} loaded={loaded}"));
        let older: Vec<Message> = entries
            .iter()
            .map(|entry| {
                let mut message = Message::from_entry(entry);
                message.id = self.take_msg_id();
                message
            })
            .collect();
        // 重新拿一次会话的可变借用，改完立即释放，再 `touch`。
        if let Some(conversation) = self.find_mut(id) {
            if older.is_empty() {
                // 空页即到头：摘掉探针，后续不再触发。
                conversation.has_more_history = false;
            } else {
                // `history_before` 也是正序；整页前插到头部，保持全局旧→新。
                conversation.messages.splice(0..0, older);
                conversation.history_cursor =
                    conversation.messages.first().map(|message| message.at).unwrap_or(cursor);
                // 没取满一页 = 库里到底了。
                conversation.has_more_history = loaded as i64 >= PAGE_STEP;
                // 新拉进来的这一页里已完成的接收文件补一次存在性，渲染期才不会拿 `None` 当「在」。
                conversation.refresh_existence();
            }
        }
        self.touch();
        loaded
    }

    /// 首屏：从历史库拉最近一页全局消息构建合并流。只拉一次，之后靠实时追加。
    pub(super) fn ensure_merged_loaded(&mut self) {
        if self.merged_loaded {
            return;
        }
        self.merged_loaded = true;
        let entries = self.engine.history_all(PAGE_FIRST);
        crate::utils::trace_log::trace(&format!("ensure_merged_loaded loaded={}", entries.len()));
        for entry in &entries {
            let mut message = Message::from_entry(entry);
            message.id = self.take_msg_id();
            self.merged.push(self.build_merged_item(message, entry));
        }
        // 游标停在最旧一条，下次翻页按它往前取。
        self.merged_cursor = self.merged.first().map(|item| item.message.at).unwrap_or(0);
        // 取满一整页 = 库里大概率还有更早的。
        self.merged_has_more = entries.len() as i64 >= PAGE_FIRST;
    }

    /// 合并模式的顶部翻页：用全局查询取比当前游标更早的一页，前插到合并流头部。
    ///
    /// 合并流有自己独立的游标 `merged_cursor`，与分设备模式的会话游标互不干扰。
    pub(super) fn load_older_merged(&mut self) -> usize {
        self.ensure_merged_loaded();
        if !self.merged_has_more || self.merged_cursor <= 0 {
            crate::utils::trace_log::trace(&format!(
                "load_older_merged bail: has_more={} cursor={}",
                self.merged_has_more, self.merged_cursor
            ));
            return 0;
        }
        let entries = self.engine.history_before_all(self.merged_cursor, PAGE_STEP);
        crate::utils::trace_log::trace(&format!(
            "load_older_merged cursor={} loaded={}",
            self.merged_cursor,
            entries.len()
        ));
        if entries.is_empty() {
            self.merged_has_more = false;
            return 0;
        }
        let older: Vec<model::MergedItem> = entries
            .iter()
            .map(|entry| {
                let mut message = Message::from_entry(entry);
                message.id = self.take_msg_id();
                self.build_merged_item(message, entry)
            })
            .collect();
        self.merged.splice(0..0, older);
        self.merged_cursor = self.merged.first().map(|item| item.message.at).unwrap_or(self.merged_cursor);
        self.merged_has_more = entries.len() as i64 >= PAGE_STEP;
        self.touch();
        self.merged.len()
    }

    /// 从一条 DB 记录构建合并流的一行（消息 + 展示用的设备名与类型）。
    pub(super) fn build_merged_item(&self, message: Message, entry: &Entry) -> model::MergedItem {
        model::MergedItem {
            message,
            name: if entry.device_name.is_empty() { entry.device_id.clone() } else { entry.device_name.clone() },
            mode: entry.dev_mode as i32,
            device_id: entry.device_id.clone(),
        }
    }

    /// 追加一条气泡；对方发来且当前没在看这个会话时累加未读。
    ///
    /// 合并流已加载时同步往全局流尾部追加一份副本（带展示用的设备名与类型），
    /// 这样合并视图不依赖会话窗口，实时收到 / 群发的消息也能立即出现在流尾。
    pub(super) fn attach(&mut self, conv: u64, mut message: Message, incoming: bool) {
        message.id = self.take_msg_id();
        let selected = self.selected;
        let meta = self.find_mut(conv).map(|conversation| {
            if incoming && selected != Some(conv) {
                conversation.unread = conversation.unread.saturating_add(1);
            }
            conversation.messages.push(message.clone());
            (conversation.name().to_owned(), conversation.device.mode, conversation.device.uuid.clone())
        });
        if self.merged_loaded {
            if let Some((name, mode, device_id)) = meta {
                self.merged.push(model::MergedItem { message, name, mode, device_id });
            }
        }
        self.reveal += 1;
        self.touch();
    }

    /// 按协议 `fileId` 找回气泡（会话窗口内）。
    pub(super) fn file_mut(&mut self, id: &str) -> Option<&mut Message> {
        self.conversations.iter_mut().find_map(|conversation| {
            conversation.messages.iter_mut().find(|message| message.file().is_some_and(|file| file.id == id))
        })
    }

    /// 按协议 `fileId` 找回合并流里的那条气泡，供进度 / 收尾事件同步（feed 与会话各存一份副本）。
    pub(super) fn merged_file_mut(&mut self, id: &str) -> Option<&mut Message> {
        self.merged
            .iter_mut()
            .find_map(|item| if item.message.file().is_some_and(|file| file.id == id) { Some(&mut item.message) } else { None })
    }

    /// 引擎事件落到状态机上。
    pub(super) fn on_event(&mut self, event: NetEvent) {
        match event {
            NetEvent::Devices(devices) => {
                self.devices = devices.clone();
                for device in devices {
                    self.ensure(&device);
                }
                // 设备表变了就重新看一眼本机网卡（端口可能被系统改过）
                self.local = self.engine.local_devices();
                self.touch();
            }
            NetEvent::Message { device, text, clipboard, incoming } => {
                let conv = self.ensure(&device);
                let side = if incoming { Side::Peer } else { Side::Mine };
                let message = Message::text(side, text, model::now_ms(), clipboard);
                self.attach(conv, message, incoming);
            }
            NetEvent::WebGrant { token, ip } => {
                // 同一张令牌只问一次：网页在等用户点的这几秒里会反复轮 `/checkPass`
                if !self.grants.iter().any(|(pending, _)| *pending == token) {
                    self.grants.push((token, ip));
                    self.touch();
                }
            }
            NetEvent::Receive(offer) => {
                self.ensure(&offer.device);
                if !self.offers.iter().any(|item| item.key == offer.key) {
                    self.offers.push(offer);
                    self.touch();
                }
            }
            NetEvent::Start(info) => {
                let conv = self.ensure(&info.device);
                crate::utils::trace_log::trace(&format!("event Start id={} sending={} conv={conv}", info.id, info.sending));
                // 续传时接收线程会重新广播同一个 id，别生成第二条气泡
                if self.file_mut(&info.id).is_none() {
                    let incoming = !info.sending;
                    let message = Message::transfer(0, &info, model::now_ms());
                    self.attach(conv, message, incoming);
                }
            }
            NetEvent::Progress { id, progress } => {
                let mut hit = false;
                if let Some(message) = self.file_mut(&id) {
                    message.status = Status::Ongoing;
                    if let Some(element) = message.file_mut() {
                        element.progress = progress.clamp(0, 100);
                        element.completed = false;
                    }
                    hit = true;
                }
                // 合并流里存的是副本，进度也要同步一份，否则合并视图下传输中的气泡永远停在旧值。
                if let Some(message) = self.merged_file_mut(&id) {
                    message.status = Status::Ongoing;
                    if let Some(element) = message.file_mut() {
                        element.progress = progress.clamp(0, 100);
                        element.completed = false;
                    }
                    hit = true;
                }
                if hit {
                    self.touch();
                }
            }
            NetEvent::Finish { id, ok, path } => {
                let mut hit = false;
                if let Some(message) = self.file_mut(&id) {
                    message.status = if ok { Status::Finished } else { Status::Failed };
                    if let Some(element) = message.file_mut() {
                        element.completed = ok;
                        if ok {
                            element.progress = 100;
                            // 刚落盘的文件必然在；缓存下这个真值，气泡渲染就不必再去 stat。
                            element.exists = Some(true);
                        }
                        if !path.is_empty() {
                            element.path = PathBuf::from(&path);
                        }
                    }
                    hit = true;
                }
                if let Some(message) = self.merged_file_mut(&id) {
                    message.status = if ok { Status::Finished } else { Status::Failed };
                    if let Some(element) = message.file_mut() {
                        element.completed = ok;
                        if ok {
                            element.progress = 100;
                            element.exists = Some(true);
                        }
                        if !path.is_empty() {
                            element.path = PathBuf::from(&path);
                        }
                    }
                    hit = true;
                }
                if hit {
                    self.reveal += 1;
                    self.touch();
                }
            }
        }
    }

    /// 会话窗口与合并流里的全部气泡（同一条逻辑消息可能在两处各有一份）。
    pub(super) fn all_messages(&self) -> impl Iterator<Item = &Message> {
        self.conversations
            .iter()
            .flat_map(|conversation| conversation.messages.iter())
            .chain(self.merged.iter().map(|item| &item.message))
    }

    /// 按协议 fileId 找那条文件气泡的路径。
    pub(super) fn path_of_file(&self, id: &str) -> Option<PathBuf> {
        self.all_messages()
            .find_map(|message| message.file().filter(|element| element.id == id).map(|element| element.path.clone()))
            .filter(|path| !path.as_os_str().is_empty())
    }

    /// 把合并流里属于这台设备的副本全部撤掉，并把全局游标重锚到残留的头部。
    ///
    /// 残留空了要连「首屏已拉」标记一起复位：`ensure_merged_loaded` 只追加不清空，
    /// 留着旧内容再拉一次会在残留之上叠出一屏重复行。
    pub(super) fn purge_merged_of(&mut self, uuid: &str) {
        if uuid.is_empty() {
            return;
        }
        self.merged.retain(|item| item.device_id != uuid);
        match self.merged.first() {
            Some(item) => self.merged_cursor = item.message.at,
            None => {
                self.merged_loaded = false;
                self.merged_cursor = 0;
                self.merged_has_more = false;
            }
        }
        // 正看着合并流就当场补一页（库已软删那台，重拉不会把它带回来）；
        // 否则留给下次进合并模式时再拉。
        if self.mode == ChatMode::Merged {
            self.ensure_merged_loaded();
        }
    }
}
