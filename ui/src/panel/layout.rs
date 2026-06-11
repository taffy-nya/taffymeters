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
        let mut counter = 0;
        let mut ctx = DrawCtx {
            ui,
            frame,
            counter: &mut counter,
            multi,
            repaint_needed: &mut repaint_needed,
            theme,
        };
        let result = self.root.draw(rect, &mut ctx);
        let Some((target, action)) = result else { return repaint_needed };

        let old = std::mem::replace(&mut self.root, Node::leaf(ViewType::OscilloscopeView));

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
        repaint_needed
    }
}
