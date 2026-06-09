use eframe::egui;
use taffymeters_core::frame::AudioFrame;

pub trait View: Send + 'static {
    fn draw(&mut self, ui: &mut egui::Ui, frame: &AudioFrame, rect: egui::Rect);

    fn handle_input(&mut self, _ui: &mut egui::Ui, _rect: egui::Rect) {}

    fn settings_ui(&mut self, _ui: &mut egui::Ui) {}

    fn needs_repaint(&self) -> bool { false }
}
