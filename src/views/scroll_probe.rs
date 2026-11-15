//! 顶部滚动探针：挂在消息区首行上，用户把列表滑到顶时发一条 [`Msg::LoadOlder`] 取更早的一页。
//!
//! windui 不给应用层暴露 `list_signal` 的滚动回调，但 `core` 是公开的：自定义 [`Widget`]
//! 的 `on_update` 会在每次布局前跑一趟，`EventCtx::scroll_metrics()` 又能读到最近滚动祖先的
//! `(scroll_y, content_h, view_h)`。于是这里用「装填 / 发射 / 锚定」三步把自动翻页做得可控：
//! - 只有内容超过一屏（`content_h > view_h`）才谈得上「滑到顶」；不足一屏根本不触发，
//!   免得刚打开会话就连环翻页。
//! - 一旦装填且滑进顶部阈值内，只发**一次**请求；要重新触发，必须先把探针「解除装填」
//!   —— 也就是用户又往下滚开（`scroll_y` 越过阈值），才重新装填。这样反复翻页必须由用户的
//!   滚动动作驱动，而不是在顶部原地一帧一帧地疯狂加载。
//! - 请求带回的那一页前插到头顶后，把视口整体往下挪「新页的像素高度」那么多，让新页最后一条
//!   正好落在消息框顶部、接上原来读到的那条，用户接着上滑就是刚加载的一页——而不是被 `arrange_scroll`
//!   钳回 0、跳到新页最旧的那几条上。锚定用上一帧记下的 `content_h` 基线算增量，只在「本会话刚发射过
//!   且仍贴在顶部、内容又凭空长出一截」时兑现一次，之后即解除装填。
//!
//! 装填态**不能存在 widget 里**：`list_signal` 按键重建时，首行数据（`probe` 标记与行内容）
//! 随每次翻页前插而变，行节点连同 widget 一起被换新，widget 私有的装填位会退回初始值——
//! 于是「发射 → 重建 → 又装填」同帧连环，一次触顶直接把库翻到底。与 `BottomProbe` 的
//! [`PinState`](super::bottom_probe::PinState) 同理，跨重建的状态必须挂在外面共享。

use std::cell::Cell;
use std::rc::Rc;

use windui::core::{EventCtx, Layout, Widget};
use windui::prelude::Sender;

use crate::app::Msg;

/// 距顶小于此像素即视为「到顶」。取一行的量级，够触发又不至于太敏感。
const TOP_PX: i32 = 48;

/// 跨行重建共享的探针状态。
/// - `last_fire`：上一次「发射翻页请求」落在哪个会话（`i64::MIN` = 还没发射过）。
///   不能用 `-1`：合并流哨兵 `MERGED_CONV = u64::MAX` 转 `i64` 后恰好是 `-1`，会误判已发射。
///   同会话发射过即解除装填，要重新触发必须先把视口滚离顶部（`scroll_y` 越过阈值）或切换会话，
///   于是翻页只能由用户的滚动动作驱动，不会在顶部原地连环加载。
/// - `last_h`：上一帧滚动祖先的内容高度基线。翻页前插会让它突然长出一截，据此算出新页的像素高度，
///   把视口整体往下挪那么多，让新页的最后一条正好停在消息框顶部、接上原来的阅读位置连续上滑。
///   `<=0` 表示还没有基线（探针刚随整窗重建建好的首帧），此帧不锚定。
#[derive(Debug)]
pub struct ProbeState {
    last_fire: Cell<i64>,
    last_h: Cell<i32>,
}

impl ProbeState {
    pub fn new() -> Self {
        Self { last_fire: Cell::new(i64::MIN), last_h: Cell::new(0) }
    }
}

/// 顶部探针控件：本身不画任何东西（`measure`/`paint` 走默认，占位交外层定尺寸）。
pub struct ScrollProbe {
    tx: Sender<Msg>,
    conv: u64,
    shared: Rc<ProbeState>,
}

impl ScrollProbe {
    pub fn new(tx: Sender<Msg>, conv: u64, shared: Rc<ProbeState>) -> Self {
        Self { tx, conv, shared }
    }
}

impl Widget for ScrollProbe {
    fn on_update(&mut self, ctx: &mut EventCtx) {
        // `EventCtx::scroll_metrics()` 只读**本节点**的滚动度量，而探针是行内一枚叶子、
        // 自己不是滚动容器，拿不到列表的滚动位置。于是沿父链上溯，找到最近的 `Layout::Scroll`
        // 祖先（`list_signal` 内部那个 `scroll()` 容器：行节点直接挂在其下，前插一页即让它长出一截），
        // 读它的 scroll_y / content_h / 视口高，并记下其 NodeId——锚定时要 `set_scroll_y` 回到同一容器。
        // 先拿本节点号，再拿树的可变借用（`tree_mut` 会占住 `ctx`，其间不能再 `ctx.id()`）。
        let start = ctx.id();
        let tree = ctx.tree_mut();
        let metrics = {
            let mut cur = start;
            loop {
                let Some(node) = tree.get(cur) else { break None };
                if matches!(node.layout, Layout::Scroll) {
                    let view_h = (node.bounds.h - node.padding.vertical() - node.scroll_bars.h).max(0);
                    break Some((cur, node.scroll_y, node.content_h, view_h));
                }
                match node.parent {
                    Some(parent) => cur = parent,
                    None => break None,
                }
            }
        };
        let (scroll_id, scroll_y, content_h, view_h) = match metrics {
            Some(m) => m,
            None => return,
        };
        let conv_key = self.conv as i64;
        let prev_h = self.shared.last_h.get();
        crate::utils::trace_log::trace(&format!(
            "scroll_probe conv={conv_key} scroll_y={scroll_y} content_h={content_h} view_h={view_h} prev_h={prev_h} last_fire={}",
            self.shared.last_fire.get()
        ));

        // 翻页前插刚落地：本会话已发射过翻页（`last_fire` 命中）、视口仍贴在顶部，而内容比上一帧
        // 长出一截。把视口整体往下挪「新增长度」那么多像素，新页最后一条即落在消息框顶部，用户
        // 接着上滑就是刚加载的那一页，形成连续滚动。`windui` 的 `arrange_scroll` 只把 `scroll_y`
        // 钳在 `[0, content_h - view_h]`、不做内容锚定，不补这一步画面就会跳到新页最旧的那几条上。
        // 只在 `last_fire` 命中本会话时锚定：底部来新消息会推 `reveal` 把视口钉到底（`scroll_y` 远离
        // 顶部），缩略图陆续到位也不会让 `content_h` 在贴着顶部时凭空长出一大截，故不会误锚。
        if prev_h > 0 && self.shared.last_fire.get() == conv_key && scroll_y <= TOP_PX && content_h > prev_h {
            tree.set_scroll_y(scroll_id, scroll_y + (content_h - prev_h));
            self.shared.last_h.set(content_h);
            // 锚定后视口已离开顶部，解除装填，等用户重新上滑到顶再翻下一页。
            self.shared.last_fire.set(i64::MIN);
            return;
        }

        // 记下本帧内容高度，作为下一帧判断「是否刚前插了一页」的基线。
        self.shared.last_h.set(content_h);

        // 内容不足一屏：没有「上方」可翻，解除装填（免得刚打开会话就连环翻页）。
        if content_h <= view_h {
            self.shared.last_fire.set(i64::MIN);
            return;
        }
        // 离开顶部区：重新装填，等待下一次上滑到顶。
        if scroll_y > TOP_PX {
            self.shared.last_fire.set(i64::MIN);
            return;
        }
        // 正处于顶部：本会话还没发射过才发一次翻页请求。发射后记下会话号，此后行随翻页重建、
        // 探针仍是「已解除装填」，直到用户往下滚开或切换会话。
        if self.shared.last_fire.get() != conv_key {
            self.shared.last_fire.set(conv_key);
            crate::utils::trace_log::trace(&format!("scroll_probe FIRE LoadOlder conv={conv_key}"));
            let _ = self.tx.send(Msg::LoadOlder(self.conv));
        }
    }
}
