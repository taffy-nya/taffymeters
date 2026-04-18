use eframe::egui::{self, Color32};
use taffymeters_core::signal::AudioData;
use crate::views::{traits::View, ViewType};

pub enum PanelAction { None, SplitRight, SplitDown, Remove }

static SPLIT_ID_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn next_split_id() -> u64 {
    SPLIT_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

pub struct Panel {
    pub view: Box<dyn View>,
    pub view_type: ViewType,
    overlay_open: bool,
}

impl Panel {
    fn new(vt: ViewType) -> Self {
        Self { view: vt.create(), view_type: vt, overlay_open: false }
    }

    fn set_view(&mut self, vt: ViewType) {
        self.view_type = vt;
        self.view = vt.create();
    }

    fn draw(
        &mut self,
        ui: &mut egui::Ui,
        data: &AudioData,
        rect: egui::Rect,
        salt: usize,
        multi: bool,
    ) -> PanelAction {
        {
            let mut child = ui.new_child(
                egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::LEFT)),
            );
            self.view.draw(&mut child, data);
            if let Some(interval) = self.view.repaint_interval() {  // view 请求重绘
                ui.request_repaint_after(interval);
            }
        }

        let body = ui.interact(rect, ui.id().with(("body", salt)), egui::Sense::click_and_drag());
        if body.secondary_clicked() { self.overlay_open = true; }
        if !self.overlay_open && body.dragged_by(egui::PointerButton::Primary) {
            ui.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }

        if self.overlay_open { return self.draw_overlay(ui, rect, salt, multi); }
        self.draw_split_edges(ui, rect, salt)
    }

    fn draw_overlay(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        salt: usize,
        multi: bool,
    ) -> PanelAction {
        struct Out { close: bool, switch_to: Option<ViewType>, remove: bool }
        let cur = self.view_type;
        let mut out = Out { close: false, switch_to: None, remove: false };

        egui::Area::new(egui::Id::new(("ov", salt)))
            .fixed_pos(rect.min)
            .order(egui::Order::Foreground)
            .show(ui, |ui| {
                ui.set_clip_rect(rect);
                ui.painter().rect_filled(rect, 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 190));

                let bg = ui.allocate_rect(rect, egui::Sense::click_and_drag());
                if bg.clicked() || bg.secondary_clicked() { out.close = true; }
                if bg.dragged_by(egui::PointerButton::Primary) {
                    ui.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }

                let cr = rect.shrink(40.0);
                if !cr.is_positive() { return; }

                ui.scope_builder(egui::UiBuilder::new().max_rect(cr), |ui| {
                    ui.columns(2, |cols| {
                        let mh = cr.height() - 32.0;
                        cols[0].vertical(|ui| {
                            ui.label(egui::RichText::new("MODULE").strong().color(Color32::from_gray(160)));
                            ui.add_space(16.0);
                            egui::ScrollArea::vertical().id_salt("ovL").max_height(mh).show(ui, |ui| {
                                let w = ui.available_width() - 8.0;
                                for &vt in ViewType::ALL {
                                    let sel = vt == cur;
                                    let txt = if sel {
                                        egui::RichText::new(vt.label()).color(Color32::from_rgb(100, 180, 255)).strong()
                                    } else {
                                        egui::RichText::new(vt.label()).color(Color32::LIGHT_GRAY)
                                    };
                                    let btn = egui::Button::new(txt)
                                        .min_size(egui::vec2(w, 32.0))
                                        .fill(if sel { Color32::from_rgba_unmultiplied(100, 180, 255, 25) } else { Color32::TRANSPARENT });
                                    if ui.add(btn).clicked() { out.switch_to = Some(vt); out.close = true; }
                                }
                            });
                        });
                        cols[1].vertical(|ui| {
                            ui.label(egui::RichText::new("SETTINGS").strong().color(Color32::from_gray(160)));
                            ui.add_space(16.0);
                            egui::ScrollArea::vertical().id_salt("ovR").max_height(mh).show(ui, |ui| {
                                let w = ui.available_width() - 8.0;
                                if multi {
                                    let b = egui::Button::new(
                                        egui::RichText::new("Close Panel").color(Color32::from_rgb(220, 80, 80))
                                    ).min_size(egui::vec2(w, 32.0)).fill(Color32::TRANSPARENT);
                                    if ui.add(b).clicked() { out.remove = true; out.close = true; }
                                    ui.add_space(12.0); ui.separator(); ui.add_space(12.0);
                                }
                                self.view.settings_ui(ui);
                            });
                        });
                    });
                });
            });

        if ui.input(|i| i.key_pressed(egui::Key::Escape)) { out.close = true; }
        if let Some(vt) = out.switch_to { self.set_view(vt); }
        if out.close { self.overlay_open = false; }
        if out.remove { return PanelAction::Remove; }
        PanelAction::None
    }

    fn draw_split_edges(&self, ui: &mut egui::Ui, rect: egui::Rect, salt: usize) -> PanelAction {
        const E: f32 = 20.0;

        let (hover, dragging) = ui.input(|i| (i.pointer.hover_pos(), i.pointer.is_decidedly_dragging()));
        if dragging { return PanelAction::None; }

        let right  = egui::Rect::from_min_max(egui::pos2(rect.max.x - E, rect.min.y), rect.max);
        let bottom = egui::Rect::from_min_max(
            egui::pos2(rect.min.x, rect.max.y - E),
            egui::pos2(rect.max.x - E, rect.max.y),
        );

        let ec = Color32::from_rgba_unmultiplied(100, 180, 255, 50);
        let pc = Color32::from_rgba_unmultiplied(200, 225, 255, 210);
        let pf = egui::FontId::proportional(22.0);

        if hover.map_or(false, |p| right.contains(p)) {
            ui.painter().rect_filled(right, 0.0, ec);
            ui.painter().text(right.center(), egui::Align2::CENTER_CENTER, "+", pf.clone(), pc);
        }
        if ui.interact(right, ui.id().with(("er", salt)), egui::Sense::click()).clicked() {
            return PanelAction::SplitRight;
        }

        if hover.map_or(false, |p| bottom.contains(p)) {
            ui.painter().rect_filled(bottom, 0.0, ec);
            ui.painter().text(bottom.center(), egui::Align2::CENTER_CENTER, "+", pf, pc);
        }
        if ui.interact(bottom, ui.id().with(("eb", salt)), egui::Sense::click()).clicked() {
            return PanelAction::SplitDown;
        }

        PanelAction::None
    }
}

// BSP Tree
// 均分规则：
//   "均分"只作用于 do_split 内部，仅针对被分裂的那条链，不影响其他子树。
//   具体做法：do_split 找到目标叶子并将其替换为 Split 节点后，
//   对从"最顶层的同方向祖先"到新节点这条链调用 rebalance_chain。
//
//   rebalance_chain 收集这条链上所有叶子，令每层 ratio = 1/remaining_leaves，
//   使所有叶子在屏幕上等宽/等高。
//
//   拖动分割线修改的 ratio 永远不会被 rebalance 覆盖，
//   因为 rebalance 只在 do_split 内部、split 完成后调用一次。

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir { H, V }

pub enum Node {
    Leaf(Panel),
    Split { id: u64, dir: Dir, ratio: f32, a: Box<Node>, b: Box<Node> },
}

impl Node {
    fn leaf(vt: ViewType) -> Self { Node::Leaf(Panel::new(vt)) }

    pub fn leaf_count(&self) -> usize {
        match self {
            Node::Leaf(_) => 1,
            Node::Split { a, b, .. } => a.leaf_count() + b.leaf_count(),
        }
    }

    /// DFS 渲染，counter 按前序递增
    fn draw(
        &mut self,
        ui: &mut egui::Ui,
        data: &AudioData,
        rect: egui::Rect,
        counter: &mut usize,
        multi: bool,
    ) -> Option<(usize, PanelAction)> {
        match self {
            Node::Leaf(panel) => {
                let id  = *counter;
                *counter += 1;
                let act = panel.draw(ui, data, rect, id, multi);
                if matches!(act, PanelAction::None) { None } else { Some((id, act)) }
            }
            Node::Split { id, dir, ratio, a, b } => {
                let (ra, div_rect, rb) = split_rect(rect, *dir, *ratio);
                // 分割线 ratio 由拖拽直接修改，不经过任何 rebalance
                let res_a = a.draw(ui, data, ra, counter, multi);
                let res_b = b.draw(ui, data, rb, counter, multi);
                draw_divider(ui, div_rect, *dir, ratio, rect, *id);
                res_a.or(res_b)
            }
        }
    }
}

const DIV_HALF: f32 = 3.0;

fn split_rect(rect: egui::Rect, dir: Dir, ratio: f32) -> (egui::Rect, egui::Rect, egui::Rect) {
    match dir {
        Dir::H => {
            let x = rect.min.x + rect.width() * ratio;
            (
                egui::Rect::from_min_max(rect.min, egui::pos2(x, rect.max.y)),
                egui::Rect::from_min_max(
                    egui::pos2(x - DIV_HALF, rect.min.y),
                    egui::pos2(x + DIV_HALF, rect.max.y),
                ),
                egui::Rect::from_min_max(egui::pos2(x, rect.min.y), rect.max),
            )
        }
        Dir::V => {
            let y = rect.min.y + rect.height() * ratio;
            (
                egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, y)),
                egui::Rect::from_min_max(
                    egui::pos2(rect.min.x, y - DIV_HALF),
                    egui::pos2(rect.max.x, y + DIV_HALF),
                ),
                egui::Rect::from_min_max(egui::pos2(rect.min.x, y), rect.max),
            )
        }
    }
}

fn draw_divider(ui: &mut egui::Ui, div: egui::Rect, dir: Dir, ratio: &mut f32, parent: egui::Rect, split_id: u64) {
    let id = ui.id().with(("div", split_id));
    let resp = ui.interact(div, id, egui::Sense::drag());

    if resp.hovered() || resp.dragged() {
        ui.set_cursor_icon(match dir {
            Dir::H => egui::CursorIcon::ResizeHorizontal,
            Dir::V => egui::CursorIcon::ResizeVertical,
        });
    }
    if resp.dragged() {
        let (span, d) = match dir {
            Dir::H => (parent.width(), resp.drag_delta().x),
            Dir::V => (parent.height(), resp.drag_delta().y),
        };
        if span > 0.0 { *ratio = (*ratio + d / span).clamp(0.05, 0.95); }
    }

    let color = if resp.hovered() || resp.dragged() {
        Color32::from_rgba_unmultiplied(100, 180, 255, 160)
    } else {
        Color32::from_rgba_unmultiplied(255, 255, 255, 30)
    };
    let c = div.center();
    match dir {
        Dir::H => ui.painter().line_segment(
            [egui::pos2(c.x, div.min.y), egui::pos2(c.x, div.max.y)],
            egui::Stroke::new(1.0, color),
        ),
        Dir::V => ui.painter().line_segment(
            [egui::pos2(div.min.x, c.y), egui::pos2(div.max.x, c.y)],
            egui::Stroke::new(1.0, color),
        ),
    };
}

/// 在 target 叶子旁插入新叶子，形成 Split 节点
/// 返回 (新树, 是否在这棵子树内命中过)
/// 命中后对从该 Split 节点开始的同方向链做一次均分
fn do_split(node: Node, target: usize, counter: &mut usize, dir: Dir) -> (Node, bool) {
    match node {
        Node::Leaf(panel) => {
            let id = *counter;
            *counter += 1;
            if id != target { return (Node::Leaf(panel), false); }
            let vt = panel.view_type;
            // 创建新 Split，ratio 初始任意——下面会被 rebalance_chain 修正
            let new_node = Node::Split {
                id: next_split_id(),
                dir,
                ratio: 0.5,
                a: Box::new(Node::Leaf(panel)),
                b: Box::new(Node::leaf(vt)),
            };
            (new_node, true)
        }
        Node::Split { id, dir: sd, ratio, a, b } => {
            let a_count = a.leaf_count();
            if *counter + a_count > target {
                let (new_a, hit) = do_split(*a, target, counter, dir);
                let mut node = Node::Split { id, dir: sd, ratio, a: Box::new(new_a), b };
                if hit && sd == dir {
                    // 当前节点与新 Split 同方向，纳入均分链
                    rebalance_chain(&mut node, dir);
                }
                (node, hit)
            } else {
                *counter += a_count;
                let (new_b, hit) = do_split(*b, target, counter, dir);
                let mut node = Node::Split { id, dir: sd, ratio, a, b: Box::new(new_b) };
                if hit && sd == dir {
                    rebalance_chain(&mut node, dir);
                }
                (node, hit)
            }
        }
    }
}

/// 对以 `node` 为根的、连续同方向 (`dir`) 的 Split 链做均分。
/// 只遍历链上的直接 Split 节点，遇到不同方向或叶子时停止。
///
/// 算法：设链上共有 n 个叶子，则：
///   第 1 层 ratio = 1/n        （a 占 1/n，b 子树占 (n-1)/n）
///   第 2 层 ratio = 1/(n-1)    （b 的 a 占剩余的 1/(n-1)）
///   ……以此类推
///
/// 这样每个叶子最终获得的屏幕比例恰好是 1/n。
fn rebalance_chain(node: &mut Node, dir: Dir) {
    let total = count_chain_leaves(node, dir);
    apply_chain_ratios(node, dir, total);
}

/// 统计同方向链上的叶子总数（不同方向的子树整体算一个叶子）
fn count_chain_leaves(node: &Node, dir: Dir) -> usize {
    match node {
        Node::Leaf(_) => 1,
        Node::Split { dir: sd, a, b, .. } if *sd == dir => {
            count_chain_leaves(a, dir) + count_chain_leaves(b, dir)
        }
        // 不同方向的 Split 节点——对外表现为一个整体
        Node::Split { .. } => 1,
    }
}

/// 递归设置同方向链上各层的 ratio
fn apply_chain_ratios(node: &mut Node, dir: Dir, remaining: usize) {
    if let Node::Split { dir: sd, ratio, b, .. } = node {  // id not needed here
        if *sd == dir && remaining > 1 {
            *ratio = 1.0 / remaining as f32;
            apply_chain_ratios(b, dir, remaining - 1);
        }
    }
}

/// 移除 target 叶子，用其兄弟替换父 Split 节点。
/// 返回 (新树, 是否命中)。命中标志只向上传递一层（父节点消费后不再向上冒泡）。
fn do_remove(node: Node, target: usize, counter: &mut usize) -> (Node, bool) {
    match node {
        Node::Leaf(panel) => {
            let id = *counter;
            *counter += 1;
            // 叶子自身无法删除自己，返回 hit=true 让父节点处理
            (Node::Leaf(panel), id == target)
        }
        Node::Split { id, dir, ratio, a, b } => {
            let a_count = a.leaf_count();

            if *counter + a_count > target {
                // target 在 a 子树
                let (new_a, hit) = do_remove(*a, target, counter);
                if hit {
                    // 父节点消费 hit：用 b 替换整个 Split，hit 不再向上传
                    return (*b, false);
                }
                (Node::Split { id, dir, ratio, a: Box::new(new_a), b }, false)
            } else {
                *counter += a_count;
                // target 在 b 子树
                let (new_b, hit) = do_remove(*b, target, counter);
                if hit {
                    // 用 a 替换整个 Split
                    return (*a, false);
                }
                (Node::Split { id, dir, ratio, a, b: Box::new(new_b) }, false)
            }
        }
    }
}

pub struct PanelLayout {
    root: Node,
}

impl PanelLayout {
    pub fn new(vt: ViewType) -> Self {
        Self { root: Node::leaf(vt) }
    }

    pub fn draw(&mut self, ui: &mut egui::Ui, data: &AudioData) {
        let rect  = ui.max_rect();
        let multi = self.root.leaf_count() > 1;

        let result = self.root.draw(ui, data, rect, &mut 0, multi);
        let Some((target, action)) = result else { return };

        let old = std::mem::replace(&mut self.root, Node::leaf(ViewType::Oscilloscope));

        self.root = match action {
            PanelAction::SplitRight => {
                let (new_root, _) = do_split(old, target, &mut 0, Dir::H);
                new_root
            }
            PanelAction::SplitDown => {
                let (new_root, _) = do_split(old, target, &mut 0, Dir::V);
                new_root
            }
            PanelAction::Remove => {
                let (new_root, _) = do_remove(old, target, &mut 0);
                new_root
            }
            PanelAction::None => old,
        };
    }
}
