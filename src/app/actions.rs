//! 出站动作与会话切换：发文本、发文件（合并模式先问发给谁）、重发、取消，以及切会话时的草稿存取。

use std::path::PathBuf;

use crate::lanshare::device::Device;
use crate::lanshare::TransferInfo;

use crate::app::{ChatApp, ChatMode};

impl ChatApp {
    pub(super) fn current_device(&self) -> Option<Device> {
        self.current().map(|conversation| conversation.device.clone())
    }

    /// 把当前输入框的内容存回所属会话。切换会话与发送前都要先做这件事。
    pub(super) fn stash_draft(&mut self, text: &str) {
        let Some(id) = self.selected else { return };
        if text.trim().is_empty() {
            self.drafts.remove(&id);
        } else {
            self.drafts.insert(id, text.to_owned());
        }
    }

    /// 切换会话：清未读，返回该会话自己的草稿。
    pub(super) fn select(&mut self, id: u64) -> String {
        // 在合并模式下点某台设备 = 想单独和它聊：顺带切回分设备模式。
        self.mode = ChatMode::PerDevice;
        self.selected = Some(id);
        if let Some(conversation) = self.find_mut(id) {
            conversation.unread = 0;
            // 切到这个会话：低频刷一次已完成接收文件的落盘存在性，供气泡渲染读缓存（渲染期不再 stat）。
            conversation.refresh_existence();
        }
        self.reveal += 1;
        self.touch();
        self.drafts.get(&id).cloned().unwrap_or_default()
    }

    /// 发送草稿。引擎会自己广播出去的那条消息事件，气泡由泵带回来。
    /// 返回 `true` 表示正文已被接受、该清空输入框。
    pub(super) fn submit(&mut self, text: &str) -> bool {
        let text = text.trim().to_owned();
        if text.is_empty() {
            return false;
        }
        match self.mode {
            ChatMode::Merged => {
                // 群发：引擎逐设配投递并回广播每个设备的自身事件，合并视图会把同一条自己发的去重成一根。
                if self.devices.is_empty() {
                    self.notice = Some("当前没有在线设备，消息没处发".to_owned());
                    self.touch();
                    return false;
                }
                if let Err(error) = self.engine.broadcast_message(&text, self.clipboard) {
                    self.notice = Some(format!("消息没发出去：{error}"));
                    self.touch();
                    return false;
                }
                true
            }
            ChatMode::PerDevice => {
                let Some(conv) = self.selected else {
                    self.notice = Some("先在左边选一台设备".to_owned());
                    self.touch();
                    return false;
                };
                let Some(device) = self.current_device() else { return false };
                if let Err(error) = self.engine.send_message(&device.ip, device.port, &text, self.clipboard) {
                    self.notice = Some(format!("消息没发出去：{error}"));
                    self.touch();
                    return false;
                }
                self.drafts.remove(&conv);
                true
            }
        }
    }

    /// 把选中的文件/文件夹发出去。分设备模式只发当前设备；合并模式不当场群发，
    /// 先弹「选择设备发送」让人挑（原版选完文件也是弹 `DeviceSelecter`，只是它那列表里没有
    /// 「全部设备」项，这里按需求补上）。
    pub(super) fn send_paths(&mut self, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        match self.mode {
            ChatMode::Merged => self.open_picker(paths.to_vec()),
            ChatMode::PerDevice => {
                let Some(device) = self.current_device() else {
                    self.notice = Some("先在左边选一台设备".to_owned());
                    self.touch();
                    return;
                };
                match self.engine.send_files(&device.ip, device.port, paths) {
                    Ok(infos) => {
                        for (info, path) in infos.iter().zip(paths) {
                            self.remember(info, path);
                        }
                        self.notice = None;
                        self.touch();
                    }
                    Err(error) => {
                        self.notice = Some(format!("发送失败：{error}"));
                        self.touch();
                    }
                }
            }
        }
    }

    /// 挂起这批待发路径、打开「选择设备发送」。一台设备都没有就没什么可挑的，照旧只提示。
    pub(super) fn open_picker(&mut self, paths: Vec<PathBuf>) {
        if self.devices.is_empty() {
            self.notice = Some("当前没有在线设备，文件没处发".to_owned());
            self.touch();
            return;
        }
        // 重复选文件以最后一次为准：前一批还没挑完就再选一次，不该攒成两拨发出去。
        self.pick_paths = Some(paths);
        self.touch();
    }

    /// 关掉选择器：那批路径直接丢掉，一条都不发（与原版「点 × 关掉就等于没选」一致）。
    pub(super) fn close_picker(&mut self) {
        if self.pick_paths.take().is_some() {
            self.touch();
        }
    }

    /// 选择器里点了一项。`None` = 全部设备（群发），`Some(会话号)` = 只发那台。
    pub(super) fn pick_target(&mut self, target: Option<u64>) {
        let Some(paths) = self.pick_paths.take() else { return };
        // 设备行是按在线设备建的，点下来时可能已经掉线：拿会话里的 `Device` 而不是反查 `devices`，
        // 找不到就走下面的空列表分支，不会把文件发给一台已经不在了的机器还报成功。
        let devices: Vec<Device> = match target {
            None => self.devices.clone(),
            Some(id) => self.find(id).map(|conversation| vec![conversation.device.clone()]).unwrap_or_default(),
        };
        if devices.is_empty() {
            self.notice = Some("当前没有在线设备，文件没处发".to_owned());
            self.touch();
            return;
        }
        let sent = self.send_to(&devices, &paths);
        self.notice = if sent == 0 { Some("文件发送失败：没有可用设备".to_owned()) } else { None };
        self.touch();
    }

    /// 逐台设备各开一条传输，返回成功开启的台数。每台一条发送气泡，进度独立
    /// （群发本质是多条真实传输）。
    fn send_to(&mut self, devices: &[Device], paths: &[PathBuf]) -> usize {
        let mut sent = 0usize;
        for device in devices {
            if let Ok(infos) = self.engine.send_files(&device.ip, device.port, paths) {
                // `send_files` 的返回顺序与传入路径一致，记下来才能「重新发送」
                for (info, path) in infos.iter().zip(paths) {
                    self.remember(info, path);
                }
                sent += 1;
            }
        }
        sent
    }

    pub(super) fn remember(&mut self, info: &TransferInfo, path: &PathBuf) {
        self.sources.insert(info.id.clone(), path.clone());
    }

    /// 「发送」找回原件：优先取发送时记下的源路径；找不到（收到的文件、重启后 sources 已空）
    /// 就按 fileId 找那条气泡的路径——与原版一致，收到的文件也能直接回传。
    pub(super) fn resend(&mut self, id: &str) {
        let path = self.sources.get(id).cloned().or_else(|| self.path_of_file(id));
        match path {
            Some(path) if path.exists() => self.send_paths(&[path]),
            _ => {
                self.notice = Some("文件不存在或已经被删除".to_owned());
                self.touch();
            }
        }
    }

    pub(super) fn cancel(&mut self, id: &str) {
        self.engine.cancel(id);
        self.notice = Some("已请求取消，对端确认后这条传输会结束".to_owned());
        self.touch();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lanshare::config::Config;
    use crate::lanshare::Engine;
    use std::sync::Arc;

    /// 不碰真机历史库、不起网络服务的状态机。
    fn app_for_test() -> ChatApp {
        let config = Config { save_message: false, ..Config::default() };
        ChatApp::from_engine(&Arc::new(Engine::for_test(config)))
    }

    fn peer(name: &str, ip: &str) -> Device {
        Device { ip: ip.into(), port: 5856, name: name.into(), uuid: name.into(), ..Default::default() }
    }

    /// 会话已建好、`devices` 快照就位的状态机。默认就是合并模式，不用另外切。
    fn merged_app(devices: Vec<Device>) -> ChatApp {
        let mut app = app_for_test();
        for device in &devices {
            app.ensure(device);
        }
        app.devices = devices;
        app
    }

    /// 合并模式下选文件不当场群发：先把这批路径挂起来，问一句「发给谁」。
    #[test]
    fn merged_send_opens_the_picker_instead() {
        let mut app = merged_app(vec![peer("a", "10.0.0.9")]);
        let paths = vec![PathBuf::from("D:/tmp/甲.pdf"), PathBuf::from("D:/tmp/乙")];

        app.send_paths(&paths);

        assert!(app.picker_open(), "选完文件要弹选择器");
        assert_eq!(app.pick_paths.as_ref().expect("待发路径"), &paths);
        assert_eq!(app.picker_files(), "待发 2 项：甲.pdf、乙");
    }

    /// 一台设备都没有时没得挑：照旧只提示，不开一个只有「全部设备」的空对话框。
    #[test]
    fn merged_send_without_devices_does_not_open_the_picker() {
        let mut app = merged_app(Vec::new());

        app.send_paths(&[PathBuf::from("D:/tmp/甲.pdf")]);

        assert!(!app.picker_open());
        assert_eq!(app.notice().unwrap(), "当前没有在线设备，文件没处发");
    }

    /// 取消就是没选：路径丢掉、下次同步不再弹，不能留在状态机里择机发出去。
    #[test]
    fn closing_the_picker_discards_the_batch() {
        let mut app = merged_app(vec![peer("a", "10.0.0.9")]);
        app.send_paths(&[PathBuf::from("D:/tmp/甲.pdf")]);

        app.close_picker();

        assert!(!app.picker_open());
        assert!(app.pick_paths.is_none(), "那批路径不该留着");
        // 没挂待发路径时再点一行什么都不能发生：不能拿空路径去开一条传输。
        let revision = app.revision();
        app.pick_target(None);
        assert_eq!(app.revision(), revision);
    }

    /// 选定目标就收尾：路径当场取走，选择器不会在发送之后还挂在屏上。
    /// 真机投递走不进单测（没对端），这里只钉「取走 + 关掉」这个不变量。
    #[test]
    fn picking_a_target_consumes_the_batch() {
        let mut app = merged_app(vec![peer("a", "10.0.0.9"), peer("b", "10.0.0.8")]);
        app.send_paths(&[PathBuf::from("D:/tmp/甲.pdf")]);
        let a = app.index["10.0.0.9:5856"];

        app.pick_target(Some(a));

        assert!(!app.picker_open());
        assert!(app.pick_paths.is_none());
    }

    /// 列表只列在线设备，按 IP 数值升序——字典序会把 `.10` 插到 `.9` 前面。
    #[test]
    fn picker_rows_list_online_devices_in_numeric_ip_order() {
        let devices = vec![peer("c", "10.0.0.10"), peer("a", "10.0.0.9"), peer("b", "192.168.1.1")];
        let mut app = merged_app(devices);

        let ips: Vec<&str> = app.picker_rows().iter().map(|row| row.device.ip.as_str()).collect();
        assert_eq!(ips, ["10.0.0.9", "10.0.0.10", "192.168.1.1"]);

        // 掉线的设备下一轮同步就从列表里消失，不能让人点到一台已经不在的机器。
        app.devices.retain(|device| device.ip != "10.0.0.9");
        let ips: Vec<&str> = app.picker_rows().iter().map(|row| row.device.ip.as_str()).collect();
        assert_eq!(ips, ["10.0.0.10", "192.168.1.1"]);
    }
}
