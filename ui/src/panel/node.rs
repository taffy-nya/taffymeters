use eframe::egui;
use taffymeters_core::frame::AudioFrame;
use crate::{
    views::ViewType,
    theme::Theme,
};
use super::{
    action::{Dir, PanelAction},
    leaf::Panel,
    dividers::{split_rect, draw_divider},
};

pub struct DrawCtx<'a> {
    pub ui: &'a mut egui::Ui,
    pub frame: &'a AudioFrame,
    pub multi: bool,
    pub repaint_needed: &'a mut bool,
    pub theme: &'a Theme,
}

pub enum Node {
    Leaf(Panel),
    Split { id: usize, dir: Dir, ratio: f32, dragged: bool, a: Box<Node>, b: Box<Node> },
}

impl Node {
    pub fn leaf(vt: ViewType) -> Self { Node::Leaf(Panel::new(vt)) }

    pub fn leaf_count(&self) -> usize {
        match self {
            Node::Leaf(_) => 1,
            Node::Split { a, b, .. } => a.leaf_count() + b.leaf_count(),
        }
    }

    pub fn draw(&mut self, rect: egui::Rect, ctx: &mut DrawCtx<'_>) -> Option<PanelAction> {
        match self {
            Node::Leaf(panel) => {
                if panel.view.needs_repaint() { *ctx.repaint_needed = true; }
                let act = panel.draw(ctx.ui, ctx.frame, rect, ctx.multi, ctx.theme);
                if matches!(act, PanelAction::None) { None } else { Some(act) }
            }
            Node::Split { id, dir, ratio, dragged, a, b } => {
                let (ra, div_rect, rb) = split_rect(rect, *dir, *ratio);
                let res_a = a.draw(ra, ctx);
                let res_b = b.draw(rb, ctx);
                let resp = draw_divider(ctx.ui, div_rect, *dir, ratio, rect, *id, ctx.theme);
                if !*dragged && resp.dragged() {
                    *dragged = true;
                }
                if resp.double_clicked() {
                    *ratio = 0.5;
                    *dragged = false;
                }
                res_a.or(res_b)
            }
        }
    }
}
