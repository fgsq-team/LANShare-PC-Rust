//! 首屏 / 切会话「滚到底」探针：进入一个会话时把消息视口钉到最底部（停在最新那条）。
//!
//! 为什么不能用现成的 `reveal_when`：windui 的 `reveal_when` 只在 `AppHandler::render` 里
//! `refresh_focus()` 被调用时才兑现，而那个刷新只在「本帧发生了重排」或「整窗重绘」时才跑，
//! 且 `reveal_seen` 按 `Signal` 记触发基线 `seen`、只认之后的变化。实测它对新消息勉强够用，
//! 对「切换会话」却兜不住：`body()` 不随切换重建，一次性标志钉过首屏后就再也不动，切换时视口
//! 停在顶部，看上去就是「从最早那条开始加载」。
//!
//! 本探针绕开这套时机依赖：它是挂在最后一行上的一枚 `1×1` 响应式叶子，`on_update` 每次
//! `layout_root` 都会跑一趟（切会话时 `items` 变、列表重建，必有一次重排）——沿父链找到
//! `list_signal` 内部那个 `Layout::Scroll` 祖先，把它的 `scroll_y` 直接设成极大值。windui 的
//! `Tree::set_scroll_y` 刻意「不钳制，交下一帧 arrange 钳制」，`arrange_scroll` 会
//! `scroll_y.clamp(0, content_h - view_h)` 把它正好钳到最底部，于是无论内容是否已量出真实高度，
//! 一帧内就落到底。
//!
//! 钉的粒度是「每个会话各钉一次」，靠共享的 [`PinState`] 记住上次钉过的是哪个会话：当前会话与
//! 它不同才重新钉，钉过即记下会话号。这样切换会话（含来回切）都会重新滚到底。同一会话内的
//! 跟随只认「视口正好停在底部」，用户一滚就撒手——留容差会把滚轮废掉，理由见 [`BottomProbe`]。
//! 「来新消息」仍由 `reveal_when` 负责。

use std::cell::Cell;
use std::rc::Rc;

use windui::core::{EventCtx, Layout, Widget};

/// 会话切换的「上次钉过哪个会话」共享状态。`last` 用 `i64::MIN` 表示「还没钉过任何会话」。
///
/// 不能拿 `-1` 当哨兵：合并流的会话号是 `MERGED_CONV = u64::MAX`，转 `i64` 恰好就是 `-1`，
/// 于是合并流第一帧就被当成「已经钉过」，`entered` 恒假——进合并视图永不滚到底，视口
/// 停在最早那条（正好是顶部探针的发射区）。也不能拿 `0`：那是「无选中会话」的号。
/// 与 `scroll_probe::ProbeState::last_fire` 共用同一个哨兵取值。
#[derive(Debug)]
pub struct PinState {
    last: Cell<i64>,
}

impl PinState {
    pub fn new() -> Self {
        Self { last: Cell::new(i64::MIN) }
    }
}

/// 滚到底探针：`shared` 与 `conv`（本行所属会话号）配合，做到「换会话才重钉一次」。
pub struct BottomProbe {
    shared: Rc<PinState>,
    conv: u64,
}

impl BottomProbe {
    pub fn new(shared: Rc<PinState>, conv: u64) -> Self {
        Self { shared, conv }
    }
}

impl Widget for BottomProbe {
    fn on_update(&mut self, ctx: &mut EventCtx) {
        // 先取本节点号，再拿树的可变借用（`tree_mut` 占住 `ctx`，其间不能再 `ctx.id()`）。
        let start = ctx.id();
        let tree = ctx.tree_mut();
        // 沿父链找最近的 `Layout::Scroll` 祖先（`list_signal` 内部那个滚动容器），顺带读它的
        // `scroll_y / content_h / view_h`——判断贴底要用。先在只读借用结束前拿到 NodeId，
        // 再调 `set_scroll_y`（可变借用），避免同时持两个借用。
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
                    // 没找到滚动祖先（行不在列表里）：不钉，也不记会话号，等它进列表再钉。
                    None => break None,
                }
            }
        };
        let Some((scroll_id, scroll_y, content_h, view_h)) = metrics else { return };
        let conv_key = self.conv as i64;
        // 进入新会话（含合并↔分设备切换，会话号不同）无条件钉到底。同一会话内只在视口**正好**
        // 停在底部时才跟随：首屏钉过之后缩略图陆续解码会让内容继续变长，不跟随就停在中间而非最新。
        //
        // 判据必须是「正好在底」，不能留容差。`on_update` 跑在本帧 measure 之前，读到的三个数
        // 都是上一帧收尾时的值：内容变长那一帧，视口仍停在旧底部，等式成立，于是钉到新底部；
        // 而用户滚轮上滚一格（windui 每格 48px）本身就打脏重排、也要跑一趟这里，只要容差 ≥ 一格，
        // 滚完仍算「贴底」、同帧就被钉回去——滚轮看着整个失效，只有把视口一次拖出容差（拽滚动条）
        // 才解得开。
        let entered = self.shared.last.get() != conv_key;
        let at_bottom = scroll_y + view_h >= content_h;
        crate::utils::trace_log::trace(&format!(
            "bottom_probe conv={conv_key} scroll_y={scroll_y} content_h={content_h} view_h={view_h} entered={entered} at_bottom={at_bottom}",
        ));
        if entered || at_bottom {
            // 设成极大值，同帧 `arrange_scroll` 钳到真实底部；记下本会话号。
            tree.set_scroll_y(scroll_id, i32::MAX);
            self.shared.last.set(conv_key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windui::core::{NodeId, Tree};
    use windui::event::{MouseButton, PointerEvent, PointerKind};
    use windui::prelude::*;
    use windui::text::NullTextEngine;

    /// 滚轮事件一格 = 120 个 1/120 单位；windui 的 `ScrollWidget` 把一格换算成 48 像素。
    const NOTCH: i32 = 120;
    const NOTCH_PX: i32 = 48;
    const ROW_H: i32 = 30;
    const VIEW_H: i32 = 100;

    /// 离屏搭一棵真列表：`rows` 行、每行 30px，最后一行挂探针（与 `thread::row_of` 同构）。
    /// 返回 (树, 滚动根, 行数信号)；视口 100 高，故底部 = `rows*30 - 100`。
    fn harness(rows: usize) -> (Tree, NodeId, Signal<Vec<(u8, bool)>>) {
        harness_conv(rows, 0)
    }

    /// 同上，但可指探针所属的会话号（合并流那个哨兵要走这条路才能验到）。
    fn harness_conv(rows: usize, conv: u64) -> (Tree, NodeId, Signal<Vec<(u8, bool)>>) {
        let pin = Rc::new(PinState::new());
        let data = signal((0..rows as u8).map(|n| (n, n + 1 == rows as u8)).collect::<Vec<_>>());
        let list = Element::list_signal(data.clone(), |&(n, _)| n, move |(_, last)| {
            let row = Element::col().width_match().height(ROW_H);
            if last {
                row.child(Element::leaf().size(1, 1).widget(BottomProbe::new(pin.clone(), conv)).reactive())
            } else {
                row
            }
        })
        .width(100)
        .height(VIEW_H);
        let mut tree = Tree::new();
        let root = list.build(&mut tree);
        tree.root = Some(root);
        (tree, root, data)
    }

    fn relayout(tree: &mut Tree) {
        tree.layout_root(Size::new(100, VIEW_H), &mut NullTextEngine);
    }

    fn wheel(tree: &mut Tree, delta: i32) {
        let ev = PointerEvent::single(PointerKind::Wheel(delta), Point::new(50, 50), MouseButton::Left);
        let (mut hover, mut capture) = (None, None);
        tree.dispatch_pointer(ev, &mut hover, &mut capture);
    }

    /// 回归：滚轮上滚一格后跑一次布局，视口必须留在滚到的那一处。
    ///
    /// 判据若给贴底留容差（原先是 64px，比一格还大），`scroll_by` 打脏的重排里探针就把视口
    /// 钉回底部，滚轮一格也挪不动画面——看上去就是滚轮整个失效，只有把视口一次拖出容差
    /// （拽滚动条）才解得开。
    #[test]
    fn one_wheel_notch_is_not_snapped_back_to_the_bottom() {
        let (mut tree, root, _) = harness(10);
        relayout(&mut tree);
        let bottom = tree.get(root).unwrap().scroll_y;
        assert_eq!(bottom, 10 * ROW_H - VIEW_H, "首帧应钉到底部");

        wheel(&mut tree, NOTCH);
        relayout(&mut tree);
        assert_eq!(tree.get(root).unwrap().scroll_y, bottom - NOTCH_PX, "滚一格应留在滚到的位置");

        wheel(&mut tree, NOTCH);
        relayout(&mut tree);
        assert_eq!(tree.get(root).unwrap().scroll_y, bottom - 2 * NOTCH_PX, "接着滚还能接着走");
    }

    /// 撒手之后不再回来跟用户较劲，但内容在底部变长时仍要跟随到最新那条。
    #[test]
    fn follows_content_growth_only_while_still_at_the_bottom() {
        // 贴在底部时前插一页：钉到新底部，不让用户被留在半空。
        let (mut tree, root, data) = harness(10);
        relayout(&mut tree);
        data.set((0..12u8).map(|n| (n, n + 1 == 12)).collect::<Vec<_>>());
        relayout(&mut tree);
        assert_eq!(tree.get(root).unwrap().scroll_y, 12 * ROW_H - VIEW_H, "内容变长且贴在底部应跟随");

        // 用户已滚开（不足一格也算）：同样的变长不该把视口拽回底部。
        let (mut tree, root, data) = harness(10);
        relayout(&mut tree);
        wheel(&mut tree, 20);
        relayout(&mut tree);
        let escaped = tree.get(root).unwrap().scroll_y;
        assert!(escaped < 10 * ROW_H - VIEW_H, "20 单位该滚出 8 像素");
        data.set((0..12u8).map(|n| (n, n + 1 == 12)).collect::<Vec<_>>());
        relayout(&mut tree);
        assert_eq!(tree.get(root).unwrap().scroll_y, escaped, "滚开之后不再钉底");
    }

    /// 回归：合并流的会话号 `MERGED_CONV = u64::MAX` 转 `i64` 恰为 `-1`，
    /// 不能和「还没钉过」的哨兵撞号——撞上了合并流就永不「进会话即钉底」，
    /// 视口停在最早那条，看上去像切换模式没生效。
    #[test]
    fn pins_on_entry_even_when_the_feed_key_wraps_to_minus_one() {
        let (mut tree, root, _) = harness_conv(10, u64::MAX);
        relayout(&mut tree);
        assert_eq!(tree.get(root).unwrap().scroll_y, 10 * ROW_H - VIEW_H, "合并流首帧也应钉到底部");
    }
}
