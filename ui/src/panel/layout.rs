use eframe::egui;
use taffymeters_core::frame::AudioFrame;
use crate::{
    views::ViewType,
    theme::Theme,
};
use super::{
    action::{Dir, PanelAction},
    node::{DrawCtx, Node},
    tree::{do_split, do_remove},
};

pub struct PanelLayout {
    root: Node,
}

impl PanelLayout {
    pub fn new(vt: ViewType) -> Self {
        Self { root: Node::leaf(vt) }
    }

    pub fn draw(&mut self, ui: &mut egui::Ui, frame: &AudioFrame, theme: &Theme) -> bool {
        let rect  = ui.max_rect();
        let multi = self.root.leaf_count() > 1;

        let mut repaint_needed = false;
        let mut ctx = DrawCtx { ui, frame, multi, repaint_needed: &mut repaint_needed, theme };
        let result = self.root.draw(rect, &mut ctx);
        let Some(action) = result else { return repaint_needed };

        let old = std::mem::replace(&mut self.root, Node::leaf(ViewType::OscilloscopeView));

        self.root = match action {
            PanelAction::SplitRight(id) => do_split(old, id, Dir::H).0,
            PanelAction::SplitDown(id)  => do_split(old, id, Dir::V).0,
            PanelAction::Remove(id)     => do_remove(old, id).0,
            PanelAction::None           => old,
        };
        repaint_needed
    }
}
