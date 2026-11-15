//! 配置与主题：设置草稿的校验落库，以及那些「控件直接写信号」的 setter。

use crate::views::state::FormDraft;

use crate::app::{ChatApp, ChatMode, parse_port, ThemePref};

impl ChatApp {
    /// 把设置草稿写回引擎；端口或字号非法就退回上一次的值。
    pub(super) fn apply_settings(&mut self, draft: &FormDraft) -> Result<(), String> {
        let mut config = draft.config.clone();
        config.tcp_port = parse_port(&draft.tcp, "TCP 端口")?;
        config.udp_port = parse_port(&draft.udp, "UDP 端口")?;
        config.font_size = draft.font_size.trim().parse::<i32>().map_err(|_| "字号必须是数字".to_owned())?.clamp(6, 72);
        if config.save_path.trim().is_empty() {
            return Err("保存目录不能为空".to_owned());
        }
        self.theme_pref = ThemePref::from_theme(&config.theme);
        self.engine.set_config(config.clone());
        self.local = self.engine.refresh_local();
        self.settings = config;
        self.touch();
        Ok(())
    }

    pub(super) fn open_settings(&mut self) {
        self.settings = self.engine.config();
        self.touch();
    }

    pub(super) fn set_save_path(&mut self, path: &str) {
        self.settings.save_path = path.to_owned();
        self.touch();
    }

    pub(super) fn set_clipboard(&mut self, on: bool) {
        self.clipboard = on;
        self.touch();
    }

    /// 分段控件切模式：只在真的变了才改状态。草稿不跟着模式兑水——两模式共用一份
    /// `Ui::draft`，切来切去不打断用户正在打的那段话（分设备下的逐会话草稿仍在 `select` 里抓存）。
    pub(super) fn set_chat_mode(&mut self, index: usize) {
        let mode = ChatMode::from_index(index);
        if mode == self.mode {
            return;
        }
        self.mode = mode;
        match mode {
            // 切到合并模式：首屏若还没拉过就从历史库拉一页全局流（已拉过则保留实时追加的那份）。
            ChatMode::Merged => self.ensure_merged_loaded(),
            // 切到分设备：没有选中（或选中的会话已不在表里）就默认选第一台，
            // 否则主区会停在「还没有选择设备」的空态。口径与启动时挑首屏会话一致。
            ChatMode::PerDevice => {
                let valid = self.selected.is_some_and(|id| self.find(id).is_some());
                if !valid {
                    if let Some(id) = self.visible().first().map(|conversation| conversation.id) {
                        self.select(id);
                    }
                }
            }
        }
        self.touch();
    }

    pub(super) fn set_query(&mut self, text: &str) {
        if self.query != text {
            self.query = text.to_owned();
            self.touch();
        }
    }

    pub(super) fn set_notice(&mut self, text: Option<String>) {
        if self.notice != text {
            self.notice = text;
            self.touch();
        }
    }

    pub(super) fn cycle_theme(&mut self) {
        self.theme_pref = self.theme_pref.next();
        self.settings.theme = self.theme_pref.as_theme().to_owned();
        let mut config = self.engine.config();
        config.theme = self.settings.theme.clone();
        self.engine.set_config(config);
        self.touch();
    }

    /// 系统深浅色变了就返回 `true`，调用方据此重挂主题。
    pub(super) fn set_system_theme(&mut self, dark: bool) -> bool {
        if self.system_dark == dark {
            return false;
        }
        self.system_dark = dark;
        self.touch();
        true
    }

    pub(super) fn refresh(&mut self) {
        self.local = self.engine.refresh_local();
        self.notice = Some("已重新枚举本机网卡".to_owned());
        self.touch();
    }
}
