//! 分发器骨架：`Pump` 的字段、同步门控 `Gate`，以及那些要拿 `EventCtx` 的辅助动作。

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use windui::core::EventCtx;
use windui::prelude::*;

use crate::model;
use crate::theme;
use crate::utils::pick;
use crate::views;
use crate::views::state::{Shape, Ui};

use crate::app::tray::PendingNotice;
use crate::app::{ChatApp, Msg};

impl Pump {
    pub(super) fn sender(&self) -> Sender<Msg> {
        self.tx.borrow().clone().expect("channel 已注册，发送端必然就位")
    }

    /// 把当前草稿存回状态机（切换会话、发送前都要先做这件事）。
    pub(super) fn stash_draft(&self) {
        let text = self.ui.draft.get();
        self.state.borrow_mut().stash_draft(&text);
    }

    /// 换主题并重挂。字体族与字号跟着配置走，所以设置保存后也要调。
    pub(super) fn apply_theme(&self) {
        let state = self.state.borrow();
        let config = state.settings();
        let skin = state.skin();
        self.theme.set(theme::build_theme(skin, &config.font_family, config.font_size));
        // 气泡那些定色是在建树时烘进元素的，不跟着 `ThemeHandle` 走：配色档案本身也得
        // 推给信号，根树才会重建。
        self.ui.skin.set(skin);
    }

    /// 系统深浅色变化。
    ///
    /// 借用来必须开在块里、结果留在块外：`if self.state.borrow_mut().…` 会把写借用
    /// 一直挂到整个 if 语句结束，块内再 `apply_theme` / `sync` 就是二次借用。
    pub(super) fn system_theme(&self, dark: bool) {
        let changed = self.state.borrow_mut().set_system_theme(dark);
        if changed {
            self.apply_theme();
            self.sync(true);
        }
    }

    pub(super) fn pick_files(&self, ctx: &mut EventCtx) {
        let tx = self.sender();
        ctx.request_pick_files(PickDialog::new().title("选择要发送的文件"), move |paths| {
            if let Some(paths) = paths.filter(|list| !list.is_empty()) {
                let _ = tx.send(Msg::SendPaths(paths));
            }
        });
    }

    pub(super) fn pick_folder(&self, ctx: &mut EventCtx) {
        let tx = self.sender();
        ctx.request_pick_folder(PickDialog::new().title("选择要发送的文件夹"), move |path| {
            if let Some(path) = path {
                let _ = tx.send(Msg::SendPaths(vec![path]));
            }
        });
    }

    pub(super) fn pick_save_path(&self, ctx: &mut EventCtx) {
        let tx = self.sender();
        ctx.request_pick_folder(PickDialog::new().title("选择保存目录"), move |path| {
            if let Some(path) = path {
                let _ = tx.send(Msg::SavePathPicked(path.to_string_lossy().into_owned()));
            }
        });
    }

    /// 后台查一次公网 IPv6，回包走 `Msg::QrV6`。查询要出网、可能阻塞好几秒，绝不
    /// 能占 UI 线程：单独一条线程跑，拿到结果（或失败）再发回界面。失败就是 `None`，
    /// 下拉里不多那一行，不影响已有 IPv4 网卡。
    pub(super) fn spawn_v6_query(&self) {
        let tx = self.sender();
        std::thread::spawn(move || {
            let ip = crate::utils::public_ip::query_ipv6();
            let _ = tx.send(Msg::QrV6(ip));
        });
    }

    /// 后台查一次更新，回包走 `Msg::UpdateReport`。和上面那条同个理由：`update::query()`
    /// 是手写 HTTP over `TcpStream`，建连加读完能占上几秒，放 UI 线程上就是直接冻住窗口。
    /// `manual` 跟着结果一起回去：判定要拿它决定说不说话，而从发出到回包中间可能已经
    /// 处理过一堆事件，不能去读那时的界面状态。
    pub(super) fn spawn_update_check(&self, manual: bool) {
        let tx = self.sender();
        std::thread::spawn(move || {
            let result = crate::utils::update::query();
            let _ = tx.send(Msg::UpdateReport { result: Box::new(result), manual });
        });
    }

    /// 开一个图片预览子窗。路径列表在**点开那一刻**从当前消息行聚合，开窗后不再变，
    /// 与原版 `openImagePreview` 一致（它也是现扫 `m_messages` 里的 image 气泡）。
    pub(super) fn open_preview(&self, ctx: &mut EventCtx, path: PathBuf) {
        // 开关关掉就交系统默认程序，对应原版 `useInternalImagePreview` 为假时的 `openUrl`。
        // 借用必须在语句内结束，否则后面的行都拿着写借用（参见 `system_theme` 的注释）。
        let internal = self.state.borrow().settings().use_internal_image_preview;
        if !internal {
            ctx.defer_blocking(move || pick::open(&path));
            return;
        }
        // 聚合条件逐条对原版：已完成（`card.thumb`）、路径非空、文件确实还在，按路径去重
        //（合并流里同一张图可能挂着多台设备各一行）。
        let mut files: Vec<PathBuf> = Vec::new();
        for row in self.ui.items.get() {
            let Shape::Image(card) = &row.bubble.shape else { continue };
            if !card.thumb || card.path.as_os_str().is_empty() || !card.path.is_file() {
                continue;
            }
            if !files.contains(&card.path) {
                files.push(card.path.clone());
            }
        }
        // 点的那张不在列表里（刚被删、或路径已失效）：照样把它排到第一位预览，
        // 原版拿不到下标时也是 `prepend + 0`。
        let index = match files.iter().position(|item| *item == path) {
            Some(at) => at,
            None => {
                files.insert(0, path);
                0
            }
        };
        views::preview::open(ctx, files, index);
    }

    /// 后台收内容时弹托盘气泡：仅在**确实隐藏**（`hidden`）时才发。
    ///
    /// 可见时直接清空计数丢弃（消息已在窗口里正常显示，不该再弹气泡扰人）；隐藏且
    /// 有累加才 summarize、清空、`tray.notify`。本方法由心跳 `on_interval` 调用——那是
    /// 窗口隐藏时唯一仍在跑的 UI 时机（绑 `WM_TIMER`，`SW_HIDE` 不拦定时器投递）。
    ///
    /// 锁必须在 `notify` 之前释放：`TrayHandle::notify` 会 `request_repaint`，虽然不重入本
    /// 锁，但持锁去做跨层调用从不是好主意；这里用作用域块把写借用先放掉再弹。
    pub(super) fn flush_notice(&self, hidden: bool) {
        let pending = match self.notice.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        if !hidden || pending.is_empty() {
            // 可见（无需气泡）或无累加：可见时清掉旧累加以免重新隐藏时把陈账弹一遍。
            if !hidden {
                drop(pending);
                if let Ok(mut guard) = self.notice.lock() {
                    *guard = PendingNotice::default();
                }
            }
            return;
        }
        let (title, body) = pending.summarize();
        drop(pending);
        if let Ok(mut guard) = self.notice.lock() {
            *guard = PendingNotice::default();
        }
        self.tray.notify(title, body);
    }

    /// 心跳：只处理「信号里变了但状态机还不知道」的那部分。
    pub(super) fn tick(&self) {
        let today = model::day_key(model::now_ms());
        let crossed = { self.gate.borrow().day != today };
        crate::utils::trace_log::trace(&format!("tick crossed={crossed}"));
        if crossed {
            self.gate.borrow_mut().day = today;
        }
        self.sync(crossed);
    }

    /// 把状态机投影到信号。先抄回那些控件直接写着的信号，再按变化范围决定同步多少。
    ///
    /// 这四个信号都只按**版本差**抄回，`force` 不参与：windui 的 input / checkbox / dropdown /
    /// segmented 都没有回调，只写自己那个信号，而控件每写一次必定 bump 版本，版本差已经把
    /// 「用户刚改过」说得完完备备。带上 `force` 反倒坏事的：后面那次投影回写之前，控件里存的
    /// 还是上一帧的旧值，把它当成用户改动抄进状态机，就把本次消息刚改好的状态盖回去了——
    /// 点一台设备（`select` 已把模式切成 PerDevice）紧跟的 `sync(true)` 就是这样先读到旧的
    /// 「全部设备」、把模式当场翻回去，界面看着就是「点了没反应」。
    pub(super) fn sync(&self, force: bool) {
        let ui = self.ui;

        let query_version = ui.query.version();
        if query_version != self.gate.borrow().query {
            let text = ui.query.get();
            self.gate.borrow_mut().query = query_version;
            self.state.borrow_mut().set_query(&text);
        }

        // 下面三个是双向的：`ui.sync` 会把状态机的值回写进同一个信号（勾回勾、选回选）。
        let web_version = ui.web_service.version();
        if web_version != self.gate.borrow().web {
            self.gate.borrow_mut().web = web_version;
            let on = ui.web_service.get();
            self.state.borrow_mut().set_web_service(on);
        }

        let qr_version = ui.qr_index.version();
        if qr_version != self.gate.borrow().qr {
            self.gate.borrow_mut().qr = qr_version;
            let index = ui.qr_index.get();
            self.state.borrow_mut().set_qr_index(index);
        }

        let mode_version = ui.chat_mode.version();
        if mode_version != self.gate.borrow().mode {
            self.gate.borrow_mut().mode = mode_version;
            let index = ui.chat_mode.get();
            self.state.borrow_mut().set_chat_mode(index);
        }

        let revision = self.state.borrow().revision();
        let draft_version = ui.draft.version();
        let state_changed = force || revision != self.gate.borrow().revision;
        let draft_changed = draft_version != self.gate.borrow().draft;
        if !state_changed && !draft_changed {
            return;
        }
        self.gate.borrow_mut().revision = revision;
        self.gate.borrow_mut().draft = draft_version;
        self.gate.borrow_mut().day = model::day_key(model::now_ms());

        let state = self.state.borrow();
        if state_changed {
            ui.sync(&state);
            // 回写本身也 bump 版本（值真变过才 `set`，而切模式这类改动确实会变）：立刻把新版本号
            // 盖进闸门，否则下一拍会把「自己刚写进去的」当成用户改动再抄一遍回去。
            let mut gate = self.gate.borrow_mut();
            gate.query = ui.query.version();
            gate.web = ui.web_service.version();
            gate.qr = ui.qr_index.version();
            gate.mode = ui.chat_mode.version();
        } else {
            ui.sync_composer(&state);
        }
    }
}

/// 上一次同步看到的状态版本，用来让 200ms 心跳在空闲时零成本。
#[derive(Clone, Copy)]
pub(super) struct Gate {
    revision: u64,
    query: u64,
    draft: u64,
    /// 侧栏「开启网页功能」勾选框：控件直接写信号，得按版本把它抄回状态机。
    web: u64,
    /// 二维码对话框里的网卡下拉，同上。
    qr: u64,
    /// 侧栏顶部「全部设备 / 分设备」分段：控件直接写信号，得按版本抄回状态机。
    mode: u64,
    /// 本地日序号，跨天时日期分隔条与「昨天」要重算。
    day: i64,
}

impl Gate {
    pub(super) fn of(ui: &Ui, state: &ChatApp) -> Self {
        Self {
            revision: state.revision(),
            query: ui.query.version(),
            draft: ui.draft.version(),
            web: ui.web_service.version(),
            qr: ui.qr_index.version(),
            mode: ui.chat_mode.version(),
            day: 0,
        }
    }
}

/// 分发器。`Clone` 是为了把同一份状态挂到 channel、心跳、快捷键、主题回调各一个闭包里；
/// 各闭包共享同一个 `Gate`，谁先同步过别人就不重复干活。
#[derive(Clone)]
pub(super) struct Pump {
    pub(super) state: Rc<RefCell<ChatApp>>,
    pub(super) ui: Ui,
    /// 两阶段初始化：`App::channel` 返回发送端之前，分发器已经建好了。
    pub(super) tx: Rc<RefCell<Option<Sender<Msg>>>>,
    pub(super) gate: Rc<RefCell<Gate>>,
    pub(super) theme: ThemeHandle,
    /// 托盘运行期句柄：心跳排空后台累加的待通知时用它弹气泡（须 UI 线程调）。
    pub(super) tray: TrayHandle,
    /// 自维护的「已收进托盘」标志，与 `build_tray` 里闭包共享同一个。
    pub(super) in_tray: Rc<Cell<bool>>,
    /// 与网络泵线程共享的待通知计数（泵线程累加、心跳排空发气泡）。
    pub(super) notice: Arc<Mutex<PendingNotice>>,
}
