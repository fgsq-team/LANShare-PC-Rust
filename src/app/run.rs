//! 启动装配：建窗口、装托盘、拉起网络泵与缩略图泵，最后挂上各事件回调。

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windui::core::EventCtx;
use windui::prelude::*;

use crate::assets;
use crate::theme;
use crate::views;
use crate::views::state::Ui;

use crate::app::pump::{Gate, Pump};
use crate::app::tray::{accumulate_notice, build_tray, PendingNotice};
use crate::app::{ChatApp, Msg, TICK};

/// 拉起主窗口。`main.rs` 只调这一个函数。
pub fn run() {
    // 诊断先行：设了 LANSHARE_TRACE 就把后续关键路径写进数据目录的 lanshare_trace.log，
    // 并挂 panic 钩子——release 无控制台时这是唯一能抓到「卡死现场」的入口。
    crate::utils::trace_log::init();
    crate::utils::trace_log::install_panic_hook();
    crate::utils::trace_log::trace("run: start");
    // 单实例守卫：对应 C++ 版 `main.cpp` 的 `QLocalServer` 占坑——第二次启动不再起第二套
    // 界面，而是（Windows 上）唤起已在跑的那个窗口后直接退出。占坑资源在 `acquire` 内部
    // 故意持有到进程结束，这里无需再管。放在最前：早于建窗，才能确保本进程自己那扇窗还没起。
    if let crate::utils::single_instance::Outcome::AlreadyRunning = crate::utils::single_instance::acquire() {
        return;
    }
    let state = Rc::new(RefCell::new(ChatApp::bootstrap()));
    // 信号是线程局部的，必须在建窗之前、UI 线程之上造好
    let ui = Ui::new(&state.borrow());
    let mut app = App::new("LANShare", 1180, 760)
        .min_size(940, 620)
        .centered()
        .title(ui.title)
        .theme(build_initial(&state.borrow()));
    // 把 windui 默认的品牌图标换成 LANShare 自己的图标（标题栏 / 任务栏 / Alt-Tab）。
    // 解不出图就维持默认，不能因为一个坏图标就卡住启动。
    if let Some(icon) = assets::window_icon() {
        app = app.icon(icon);
    }
    let theme_handle = app.theme_handle();

    // 系统托盘：引擎跑在独立网络线程上，窗口隐到托盘后照收消息与文件（自动接收
    // 模式直接落盘/入库），重新显示时 channel 排空、UI 补齐。
    // `engine` 提前取出：托盘的「退出」关不上 `on_close_request`，要自己 `shutdown`；网络
    // 泵线程也要它，故各持一份克隆。
    let engine = Arc::clone(state.borrow().engine());
    let in_tray = Rc::new(Cell::new(false));
    // 「已收进托盘」那句去向提示弹过没有：整个运行期只弹第一次。
    let tray_hint_shown: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let notice: Arc<Mutex<PendingNotice>> = Arc::new(Mutex::new(PendingNotice::default()));
    // 「隐藏到托盘」/「显示主窗口」/「退出」都在托盘菜单里，而关窗要不要转成隐藏由 windui
    // 的构建期开关 `hide_on_close` 定：注册了它，`on_close_request` 返回 true 的那次关闭会被
    // 框架层（`resolve_close_inner`）转成窗口隐藏。两样都是**启动快照**——运行期既造不出图标
    // （`TrayOp` 没有增删的口子，`NIM_DELETE` 只随平台宿主 drop），也改不动那个开关（没有 setter），
    // 所以「关闭主窗口时收进托盘」这一项**改完要重启才生效**（曾试过「运行期现读 + 全透明
    // 图标伪装隐去」，不够干净，已退回）。
    // `hide_on_minimize` 不注册：原版 Qt 压根没有「最小化收进托盘」这回事。
    //
    // Linux 上多一道可用性判断：SNI 托盘要会话总线上有 `org.kde.StatusNotifierWatcher` 才现得
    // 出图标（KDE 原生；GNOME 得装 AppIndicator 扩展；无头环境没有），缺席时 windui 只记一次
    // 警告、静默降级。而 `hide_on_close` 照样会把关闭转成隐藏——那时隐掉就是唤不回的黑洞，
    // 所以探测不通过就两样都不注册，退回正常退出。Windows / macOS 这个探测恒为 true（非 Linux
    // 分支不碰任何系统调用）。
    let want_hide = state.borrow().settings().hide_on_close;
    let hide_to_tray = want_hide && windui::platform::tray_supported();
    if want_hide && !hide_to_tray {
        crate::utils::trace_log::trace("run: 系统托盘不可用，关窗退回正常退出");
    }
    if hide_to_tray {
        app = app.tray(build_tray(Arc::clone(&engine), Rc::clone(&in_tray))).hide_on_close();
    }
    let tray_handle = app.tray_handle();

    let pump = Pump {
        state: Rc::clone(&state),
        ui,
        tx: Rc::new(RefCell::new(None)),
        gate: Rc::new(RefCell::new(Gate::of(&ui, &state.borrow()))),
        theme: theme_handle,
        tray: tray_handle,
        in_tray: Rc::clone(&in_tray),
        notice: Arc::clone(&notice),
    };

    let tx = app.channel({
        let pump = pump.clone();
        move |ctx, message| pump.dispatch(ctx, message)
    });
    *pump.tx.borrow_mut() = Some(tx.clone());

    // 启动就查一次更新，对应原版 `LANShareWindow` 构造末尾那句 `UpdateChecker::check(this, false)`。
    // 自动档不拦住人：没新版、新版是用户拒过的那一版、或者根本连不上服务器，都一声不吭；
    // 真有新版的提示面板要等回包之后才挂得起来，所以这一步不影响开窗速度。
    crate::utils::trace_log::trace("run: 启动检测更新");
    pump.spawn_update_check(false);

    // 缩略图即时投递：单独一条线程阻塞在结果通道上，一有回包立刻把当下就绪的攒成一条
    // `Msg::ThumbsReady` 推给 UI。取代原先搭引擎事件泵 `take_events(TICK)` 的那 200ms 轮询——
    // 一张图哪怕几毫秒就解完，也要干等最多 200ms 才显示，这正是「半天才加载出来」的主因。
    let _ = crate::assets::thumb_queue();
    let thumb_tx = tx.clone();
    std::thread::spawn(move || pump_ready_thumbs(thumb_tx));

    // 引擎事件泵：没事件时最多阻塞一个心跳，引擎停了就退出
    let pump_tx = tx;
    let notice_tx = Arc::clone(&notice);
    std::thread::spawn(move || loop {
        let mut events = engine.take_events(TICK);
        // 合批：首批取回后小睡一拍，把这一拍内陆续到达的后续事件一并攒进同一条
        // `Msg::Events`。传输期间的高频 `Progress` 因此合并成一次 dispatch/sync，
        // 而不是每个进度都触发一次整列表重算——这正是切换/传输时卡死的放大器。
        if !events.is_empty() {
            std::thread::sleep(Duration::from_millis(16));
            events.extend(engine.take_events(Duration::ZERO));
        }
        if !events.is_empty() {
            // 先把入向的消息/文件累加进共享计数（供隐藏时心跳弹托盘气泡），再把原事件
            // 照旧推给 UI。此处不靠 UI 帧：网络泵线程一直跑，窗口隐藏时 channel 不排空也
            // 不影响统计。
            accumulate_notice(&events, &notice_tx);
            if pump_tx.send(Msg::Events(events)).is_err() {
                return;
            }
        }
        if !engine.is_running() {
            return;
        }
    });

    let on_tick = {
        let pump = pump.clone();
        move |ctx: &mut EventCtx| {
            pump.tick();
            // 隐藏判据：自维护的 in_tray 标志为主（框架的 visible 在 SW_HIDE 后不刷新），
            // 再拿 minimized 兜底——最小化已不再转隐藏，但窗口同样看不见，后台来的内容照样该弹。
            let hidden = pump.in_tray.get() || ctx.window_state().minimized;
            pump.flush_notice(hidden);
        }
    };
    let on_theme = {
        let pump = pump.clone();
        move |_ctx: &mut EventCtx, dark: bool| pump.system_theme(dark)
    };
    let on_search = {
        let ui = pump.ui;
        move || ui.focus_search.update(|count| *count += 1)
    };
    let on_close = {
        let tray = pump.tray.clone();
        let in_tray = Rc::clone(&pump.in_tray);
        let hint_shown = Rc::clone(&tray_hint_shown);
        // 不常驻时要真退出，得先关引擎；`engine` 已移入网络泵线程，这里从状态现取一份克隆。
        let close_engine = Arc::clone(state.borrow().engine());
        move |_ctx: &mut EventCtx| -> bool {
            if hide_to_tray && close_engine.is_running() {
                // 回调只负责「允许关闭」：注册了 `hide_on_close`，框架层会接着把这次关闭转成
                // 窗口隐藏，引擎照旧跑着。这里只补上自维护的托盘标志与一句去向提示。
                in_tray.set(true);
                // 去向提示只弹**第一次**收进托盘：反复关窗每次都弹就成了骚扰，该说的事第一遍
                // 已经说清，之后再弹只是占掉通知区。
                if !hint_shown.get() {
                    hint_shown.set(true);
                    tray.notify(
                        "LANShare 仍在后台运行",
                        "已收进托盘，双击托盘图标可重新打开",
                    );
                }
            } else {
                // 先让引擎广播下线，再放行关闭。
                close_engine.shutdown();
            }
            true
        }
    };
    let on_show = {
        let in_tray = Rc::clone(&pump.in_tray);
        move |_ctx: &mut EventCtx| {
            // 窗口重新显出来：复位托盘标志，后续心跳不再把新到内容当“后台”弹气泡。
            in_tray.set(false);
        }
    };

    app = app
        .on_interval(TICK, on_tick)
        .on_shortcut(move |_shortcut, event| {
            // Ctrl+L 回搜索框。多行输入框自己吃掉了 Ctrl+Enter（那是「发送」），
            // 所以这里不重复接管，免得发两遍。
            if event.pressed && event.ctrl && !event.alt && !event.meta && event.key == Key::Char('L') {
                on_search();
                return true;
            }
            false
        })
        .on_system_theme_changed(on_theme)
        .on_close_request(on_close)
        .on_show(on_show)
        .screenshot_from_args()
        // 根树挂在配色档案上：颜色变了才重建一次，平时（心跳、打字）完全不碰这棵树。
        // 不这样做的话，换主题只能重染角色色，气泡里那些烘进元素的定色会留在旧档。
        .content({
            let sender = pump.sender();
            Element::host_signal(ui.skin.map(|skin| vec![*skin]), move |skin| views::root(&ui, &sender, skin))
        });
    app.run();
}

/// 缩略图投递线程主体：阻塞等第一张就绪，把当下已就绪的一并攒成一条 `Msg::ThumbsReady` 推给 UI。
///
/// `recv()` 阻塞至多一张的间隔就醒（不再是固定 200ms 轮询），随后 `try_recv()` 把这一瞬时
/// 陆续到达的其余回包全 drain 进来，一次推一条——既把延迟压到接近一帧，又保住「小批
/// 合批、不逐张 `sync(true)`」。发送失败 = UI 已关，退出。结果通道与解码线程池同在
/// `assets::thumb_queue()` 里懒初始化（在 UI 线程先调一次保证通道就绪）。
fn pump_ready_thumbs(tx: Sender<Msg>) {
    let rx = crate::assets::thumb_queue();
    loop {
        let batch = {
            let Ok(guard) = rx.lock() else { return };
            let mut batch = match guard.recv() {
                Ok(batch) => batch,
                // 发送端已全部丢弃：不会再有回包，退出。
                Err(_) => return,
            };
            while let Ok(more) = guard.try_recv() {
                batch.extend(more);
            }
            batch
        };
        if tx.send(Msg::ThumbsReady(batch)).is_err() {
            return;
        }
    }
}

/// 开局主题：跟随系统时按系统当前深浅色落定。
fn build_initial(state: &ChatApp) -> Theme {
    let config = state.settings();
    theme::build_theme(state.skin(), &config.font_family, config.font_size)
}
