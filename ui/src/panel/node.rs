use eframe::egui;
use taffymeters_core::frame::AudioFrame;
use crate::views::ViewType;
use crate::theme::Theme;
use super::action::{Dir, PanelAction};
use super::leaf::Panel;
use super::dividers::{split_rect, draw_divider};

pub struct DrawCtx<'a> {
    pub ui: &'a mut egui::Ui,
    pub frame: &'a AudioFrame,
    pub counter: &'a mut usize,
    pub multi: bool,
    pub repaint_needed: &'a mut bool,
    pub theme: &'a Theme,
}

pub enum Node {
    Leaf(Panel),
    Split { id: u64, dir: Dir, ratio: f32, a: Box<Node>, b: Box<Node> },
}

impl Node {
    pub fn leaf(vt: ViewType) -> Self { Node::Leaf(Panel::new(vt)) }

    pub fn leaf_count(&self) -> usize {
        match self {
            Node::Leaf(_) => 1,
            Node::Split { a, b, .. } => a.leaf_count() + b.leaf_count(),
        }
    }

    pub fn draw(&mut self, rect: egui::Rect, ctx: &mut DrawCtx<'_>) -> Option<(usize, PanelAction)> {
        match self {
            Node::Leaf(panel) => {
                let id  = *ctx.counter;
                *ctx.counter += 1;
                if panel.view.needs_repaint() { *ctx.repaint_needed = true; }
                let act = panel.draw(ctx.ui, ctx.frame, rect, id, ctx.multi, ctx.theme);
                if matches!(act, PanelAction::None) { None } else { Some((id, act)) }
            }
            Node::Split { id, dir, ratio, a, b } => {
                let (ra, div_rect, rb) = split_rect(rect, *dir, *ratio);
                let res_a = a.draw(ra, ctx);
                let res_b = b.draw(rb, ctx);
                draw_divider(ctx.ui, div_rect, *dir, ratio, rect, *id, ctx.theme);
                res_a.or(res_b)
            }
        }
    }
}
