//! 气泡的本地操作：删除、标已读、清空会话（合并流里的副本一并撤掉）。

use crate::model::{self, Message, Status};

use crate::app::ChatApp;

impl ChatApp {
    pub(super) fn delete_message(&mut self, id: u64) {
        // 同一条消息在会话窗口与合并流里是两个界面 id，按逻辑键两边一起删，
        // 否则合并视图（默认模式）里点删除看不到任何效果。
        let Some(target) = self.all_messages().find(|message| message.id == id).cloned() else { return };
        // 与原版一致：删除进行中的气泡先取消那条传输。
        if target.status == Status::Ongoing {
            if let Some(element) = target.file() {
                self.engine.cancel(&element.id);
            }
        }
        for conversation in &mut self.conversations {
            conversation.messages.retain(|message| !Self::same_message(message, &target));
        }
        // 合并流（默认模式）存的是另一份副本，按同一逻辑键一并删，否则点删除看不到效果。
        self.merged.retain(|item| !Self::same_message(&item.message, &target));
        self.touch();
    }

    /// 两条气泡是不是同一条逻辑消息：文件按协议 fileId；文本没有协议 id，按
    /// （秒级时刻, 方向, 正文）——与合并流去重群发副本用的是同一组键，群发副本一并清除。
    pub(super) fn same_message(left: &Message, right: &Message) -> bool {
        match (left.file(), right.file()) {
            (Some(a), Some(b)) => a.id == b.id,
            (None, None) => match (&left.content, &right.content) {
                (model::Content::Text(a), model::Content::Text(b)) => {
                    left.side == right.side && left.at / 1000 == right.at / 1000 && a == b
                }
                _ => false,
            },
            _ => false,
        }
    }

    pub(super) fn mark_read(&mut self, id: u64) {
        if let Some(conversation) = self.find_mut(id) {
            conversation.unread = 0;
            self.touch();
        }
    }

    pub(super) fn mark_all(&mut self) {
        for conversation in &mut self.conversations {
            conversation.unread = 0;
        }
        self.touch();
    }

    /// 清空一个会话的消息。与原版同口径：**先落库再清内存**，合并流里那台设备的副本一并撤掉。
    ///
    /// 只清内存窗口不算清空：历史库还在，往上翻一页或重启都会把「已清空」的消息
    /// 一页页拉回来，看上去就是清空没生效。
    pub(super) fn clear_chat(&mut self, id: u64) {
        crate::utils::trace_log::trace(&format!("clear_chat id={id}"));
        let Some(conversation) = self.find_mut(id) else { return };
        let uuid = conversation.device.uuid.clone();
        // 与原版一致：进行中的传输先取消，否则后台还在往一个已清空的会话里写文件、
        // 气泡又会被收尾事件追回来。
        let ongoing: Vec<String> = conversation
            .messages
            .iter()
            .filter(|message| message.status == Status::Ongoing)
            .filter_map(|message| message.file().map(|file| file.id.clone()))
            .collect();
        conversation.messages.clear();
        // 清空即到头：游标与「库里还有更早」一起归零，顶部探针随之不再挂上。
        conversation.has_more_history = false;
        conversation.history_cursor = 0;
        conversation.unread = 0;
        for file_id in ongoing {
            self.engine.cancel(&file_id);
        }
        self.engine.clear_history(&uuid);
        self.purge_merged_of(&uuid);
        self.touch();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lanshare::config::Config;
    use crate::lanshare::device::Device;
    use crate::lanshare::Engine;
    use crate::model::Side;
    use std::sync::Arc;

    /// 不碰真机历史库、不起网络服务的状态机。
    fn app_for_test() -> ChatApp {
        let mut config = Config::default();
        config.save_message = false;
        ChatApp::from_engine(&Arc::new(Engine::for_test(config)))
    }

    fn peer(uuid: &str, ip: &str) -> Device {
        Device { ip: ip.into(), port: 5856, name: uuid.into(), uuid: uuid.into(), ..Default::default() }
    }

    /// 清空一台会话必须同时撤掉合并流里那台设备的副本：合并模式是默认视图，
    /// 只清会话窗口的话「清空消息」点下去画面根本不动。
    #[test]
    fn clear_chat_also_purges_the_merged_feed() {
        let mut app = app_for_test();
        let a = app.ensure(&peer("uuid-a", "10.0.0.9"));
        let b = app.ensure(&peer("uuid-b", "10.0.0.8"));
        // 走真实的 `attach`：会话窗口与合并流各存一份副本，副本的设备归属也由它带上。
        app.merged_loaded = true;
        app.attach(a, Message::text(Side::Peer, "甲一", 1, false), false);
        app.attach(a, Message::text(Side::Peer, "甲二", 2, false), false);
        app.attach(b, Message::text(Side::Peer, "乙一", 3, false), false);
        app.merged_cursor = 1;
        app.merged_has_more = true;

        app.clear_chat(a);

        let owners: Vec<&str> = app.merged.iter().map(|item| item.device_id.as_str()).collect();
        assert_eq!(owners, ["uuid-b"], "合并流里只该留下另一台设备的副本: {owners:?}");
        assert_eq!(app.merged_cursor, 3, "游标重锚到残留的头部");
        assert!(app.merged_loaded, "残留非空就不能重新装首屏，否则下次进合并模式会叠出一屏重复行");
        let conversation = app.find(a).unwrap();
        assert!(conversation.messages.is_empty());
        assert!(!conversation.has_more_history, "清空即到头，不该再挂顶部探针");
        assert_eq!(conversation.history_cursor, 0);
        assert_eq!(app.find(b).unwrap().messages.len(), 1, "另一台会话不受影响");
    }

    /// 合并流被清干净时要连「首屏已拉」一起复位：`ensure_merged_loaded` 只追加不清空，
    /// 留着旧标记下次进合并模式就会在残留之上叠一页。
    #[test]
    fn clearing_the_whole_merged_feed_arms_a_fresh_first_page() {
        let mut app = app_for_test();
        let a = app.ensure(&peer("uuid-a", "10.0.0.9"));
        app.merged_loaded = true;
        app.attach(a, Message::text(Side::Peer, "唯一一条", 1, false), false);

        app.clear_chat(a);

        assert!(app.merged.is_empty(), "合并流里那台设备的副本要一起没");
        assert_eq!(app.merged_cursor, 0);
        assert!(!app.merged_has_more);
        assert_eq!(app.find(a).unwrap().messages.len(), 0);
    }

    /// 合并（全部设备）流里每一条收到的气泡都得标上自己那台会话的号：点头像与昵称就是靠它
    /// 跳回那台的分设备会话。标错就会跳到别的设备上去，所以本机发出的、以及旧库里那两个
    /// 无主的行（uuid 为空、会话已经没了）一律留 `None`——跳不过去就别给入口。
    #[test]
    fn merged_rows_carry_the_conversation_to_jump_to() {
        let mut app = app_for_test();
        let a = app.ensure(&peer("uuid-a", "10.0.0.9"));
        app.merged_loaded = true;
        app.attach(a, Message::text(Side::Peer, "甲一", 1, false), false);
        app.attach(a, Message::text(Side::Mine, "群发一条", 2, false), false);
        for (at, uuid) in [(3, ""), (4, "uuid-gone")] {
            app.merged.push(crate::model::MergedItem {
                message: Message::text(Side::Peer, "收的一条", at, false),
                name: "散客".to_owned(),
                mode: 0,
                device_id: uuid.to_owned(),
            });
        }

        let rows = crate::views::state::merged_rows(&app);
        let targets: Vec<Option<u64>> = rows.iter().map(|row| row.bubble.target).collect();
        assert_eq!(targets, [Some(a), None, None, None], "只有收到的、且那台会话还在的行才带得上会话号: {targets:?}");
    }
}
