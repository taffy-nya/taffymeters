use eframe::egui;
use taffymeters_core::frame::AudioFrame;
use super::traits::View;

pub struct EmptyView;

impl EmptyView {
    pub fn new() -> Self {
        Self
    }
}

impl View for EmptyView {
    fn draw(&mut self, _ui: &mut egui::Ui, _frame: &AudioFrame, _rect: egui::Rect) {}
}
