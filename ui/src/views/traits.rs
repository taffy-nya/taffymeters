use eframe::egui;
use taffymeters_core::signal::AudioData;

pub trait View: Send + 'static {
    fn draw(&mut self, ui: &mut egui::Ui, data: &AudioData);

    fn settings_ui(&mut self, _ui: &mut egui::Ui) {}
    
    fn repaint_interval(&self) -> Option<std::time::Duration> { None }
}
