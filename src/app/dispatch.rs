//! 消息分发：把一条 `Msg` 打到状态机上，再把两边对齐。

use windui::core::EventCtx;

use crate::utils::pick;

use crate::app::pump::Pump;
use crate::app::update::Verdict;
use crate::app::{ChatMode, Msg};

impl Pump {
    /// 一条消息的完整处理：改状态 → 对齐信号。
    pub(super) fn dispatch(&self, ctx: &mut EventCtx, message: Msg) {
        let ui = self.ui;
        match message {
            Msg::Events(events) => {
                crate::utils::trace_log::trace(&format!("dispatch Events: {}", events.len()));
                let mut state = self.state.borrow_mut();
                for event in events {
                    state.on_event(event);
                }
            }
            Msg::Select(id) => {
                self.stash_draft();
                let draft = self.state.borrow_mut().select(id);
                ui.draft.set(draft);
                ui.focus_draft.update(|count| *count += 1);
            }
            Msg::LoadOlder(id) => {
                // 顶部探针滑到顶：往前补一页。不推 reveal——翻页是往回看，不该把视口弹到底。
                // 合并模式下探针带的是哨兵会话号：按模式分流，合并流走独立全局分页。
                let mut app = self.state.borrow_mut();
                let merged = app.mode() == ChatMode::Merged;
                crate::utils::trace_log::trace(&format!("dispatch LoadOlder id={id} merged={merged}"));
                if merged {
                    app.load_older_merged();
                } else {
                    app.load_older(id);
                }
            }
            Msg::ThumbsReady(thumbs) => {
                // 回包落缓存；末尾的 `sync(true)` 重建行，`ImageCard.ready` 翻真，那一行才换上图。
                crate::assets::store_thumbs(thumbs);
            }
            Msg::Send => {
                let text = ui.draft.get();
                // 同上：写借用在取结果时就地结束，别把它拖过整个 if。
                let sent = self.state.borrow_mut().submit(&text);
                if sent {
                    ui.draft.set(String::new());
                    ui.focus_draft.update(|count| *count += 1);
                }
            }
            Msg::InsertText(text) => {
                ui.draft.update(|draft| draft.push_str(&text));
                ui.focus_draft.update(|count| *count += 1);
            }
            Msg::Clipboard(on) => self.state.borrow_mut().set_clipboard(on),
            Msg::PickFiles => self.pick_files(ctx),
            Msg::PickFolder => self.pick_folder(ctx),
            Msg::PickSavePath => self.pick_save_path(ctx),
            Msg::SavePathPicked(path) => {
                ui.f_save_path.set(path.clone());
                self.state.borrow_mut().set_save_path(&path);
            }
            Msg::SendPaths(paths) => self.state.borrow_mut().send_paths(&paths),
            Msg::PickTarget(target) => self.state.borrow_mut().pick_target(target),
            Msg::ClosePicker => self.state.borrow_mut().close_picker(),
            Msg::Cancel(id) => self.state.borrow_mut().cancel(&id),
            Msg::Resend(id) => self.state.borrow_mut().resend(&id),
            Msg::OpenFile(path) => {
                // 起外部进程看文件，放到事件栈之外去做
                if path.exists() {
                    ctx.defer_blocking(move || pick::open(&path));
                } else {
                    self.state.borrow_mut().set_notice(Some("文件不存在或已经被删除".to_owned()));
                }
            }
            Msg::OpenPath(path) => {
                // 与原版一致：文件不在就提示（explorer 对不存在的路径也会静默开错地方）
                if path.exists() {
                    ctx.defer_blocking(move || pick::reveal(&path));
                } else {
                    self.state.borrow_mut().set_notice(Some("文件不存在或已经被删除".to_owned()));
                }
            }
            Msg::FreeCopyText(text) => self.state.borrow_mut().open_free_copy(text),
            Msg::CopyFreeCopy => {
                ctx.clipboard_set(&ui.free_copy_text.get());
                ctx.toast_ok("文本已复制到剪切板");
                self.state.borrow_mut().close_free_copy();
            }
            Msg::CloseFreeCopy => self.state.borrow_mut().close_free_copy(),
            Msg::PreviewImage(path) => self.open_preview(ctx, path),
            Msg::DeleteMessage(id) => self.state.borrow_mut().delete_message(id),
            Msg::MarkRead(id) => self.state.borrow_mut().mark_read(id),
            Msg::MarkAll => self.state.borrow_mut().mark_all(),
            Msg::ClearChat(id) => {
                crate::utils::trace_log::trace(&format!("dispatch ClearChat id={id}"));
                self.state.borrow_mut().clear_chat(id)
            }
            Msg::Resolve { key, accept } => self.state.borrow_mut().resolve(key, accept),
            Msg::ResolveWebGrant { token, accept } => self.state.borrow_mut().resolve_grant(&token, accept),
            Msg::OpenQr => {
                self.state.borrow_mut().open_qr();
                self.spawn_v6_query();
            }
            Msg::CloseQr => self.state.borrow_mut().close_qr(),
            Msg::RefreshQr => {
                self.state.borrow_mut().refresh_qr();
                self.spawn_v6_query();
            }
            Msg::QrV6(ip) => self.state.borrow_mut().set_qr_v6(ip),
            Msg::OpenSettings => {
                self.state.borrow_mut().open_settings();
                ui.fill_form(&self.state.borrow());
                ui.settings_open.set(true);
            }
            Msg::CloseSettings => ui.settings_open.set(false),
            // 「关于」整页都是静态内容，开关只活在界面层，不进状态机。
            Msg::OpenAbout => ui.about_open.set(true),
            Msg::CloseAbout => ui.about_open.set(false),
            Msg::CheckUpdate { manual } => self.spawn_update_check(manual),
            Msg::UpdateReport { result, manual } => {
                // 写借用先在这一行结束：`report_update` 里面要读引擎配置、要改状态，出来以后这条
                // arm 还要再拿 `ctx` 弹提示。不要写成 `match self.state.borrow_mut()…`：scrutinee
                // 的临时借用会活到整条 `match` 结束（参见 `SaveSettings` 那一段的注释）。
                let verdict = self.state.borrow_mut().report_update(*result, manual);
                // 「已是最新 / 检测失败」那两句原版用的是模态框，这里走轻提示：一句话说完的
                // 事不必拦住人。要弹的新版面板则由末尾那次 `sync` 摆上来。
                match verdict {
                    Verdict::Silent => {}
                    Verdict::Latest => ctx.toast_ok("当前已是最新版本"),
                    Verdict::Failed(error) => ctx.toast_err(error),
                }
            }
            Msg::CloseUpdate => self.state.borrow_mut().close_update(),
            Msg::DeclineUpdate => self.state.borrow_mut().decline_update(),
            Msg::QuitForUpdate => {
                // 强更没有退路：先让引擎广播下线，再走和托盘那个「退出」同一条路。
                // 不用 `ctx.force_close`：那个仍然受 `hide_on_close` 约束，开着后台常驻时
                // 「退出」会变成「窗口不见了、进程还在跑」。`TrayHandle::quit` 排的是
                // `TrayOp::Quit`，平台层在它那里销毁全部窗口并**绕过那个开关**，没装托盘
                // 时照样生效（退出指令在切队列时就拿走了，不等托盘目标）。
                self.state.borrow().engine().shutdown();
                self.tray.quit();
            }
            Msg::SaveSettings => {
                // 先记下改动前的常驻开关：它是构建期声明，动了就得提醒用户重启。
                let prev_hide = self.state.borrow().settings().hide_on_close;
                let draft = ui.read_form(&self.state.borrow());
                // `apply_settings` 的写借用必须在 `match` 之前结束：match 的 scrutinee
                // 临时值会活到整条语句结束，而成功分支里要 `apply_theme`（读借用）、
                // 失败分支里还要再 `borrow_mut`，留着不放就是 RefCell 二次借用崩溃。
                let result = {
                    let mut state = self.state.borrow_mut();
                    state.apply_settings(&draft)
                };
                match result {
                    Ok(()) => {
                        ui.settings_open.set(false);
                        self.apply_theme();
                        // 「关闭主窗口时收进托盘」靠的是 windui 两个构建期声明（见 `run`）：图标
                        // 造不出也摘不掉，开关没有 setter，改完只能重启。这一项动了就不能只说
                        // 「已保存」，否则用户对着还留在托盘区的图标猜哪儿没生效。
                        if prev_hide != draft.config.hide_on_close {
                            ctx.toast_ok("设置已保存：托盘那一项要重启程序才生效");
                        } else {
                            ctx.toast_ok("设置已保存");
                        }
                    }
                    Err(error) => {
                        ctx.toast_err(&error);
                        self.state.borrow_mut().set_notice(Some(error));
                    }
                }
            }
            Msg::CycleTheme => {
                self.state.borrow_mut().cycle_theme();
                self.apply_theme();
            }
            Msg::SystemTheme(dark) => self.system_theme(dark),
            Msg::Refresh => self.state.borrow_mut().refresh(),
            Msg::Copy(text) => {
                ctx.clipboard_set(&text);
                ctx.toast_ok("已复制到剪贴板");
            }
            Msg::Notice(text) => self.state.borrow_mut().set_notice(Some(text)),
            Msg::DismissNotice => self.state.borrow_mut().set_notice(None),
        }
        // 拖放、选择框之类的动作可能悄悄改了提示条，一律同步一次
        self.sync(true);
        crate::utils::trace_log::trace("dispatch done+sync");
    }
}
