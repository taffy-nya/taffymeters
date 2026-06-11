use eframe::egui;
use taffymeters_core::{
    channel::ChannelMode,
    frame::AudioFrame,
};
use crate::theme::Theme;
use super::components::{ScaleControl, channel_select_ui};
use super::traits::View;

pub struct OscilloscopeView {
    y_scale: ScaleControl,
    channel: ChannelMode,
    theme: &'static Theme,
}

impl OscilloscopeView {
    pub fn new() -> Self {
        Self {
            y_scale: ScaleControl::new(1.0, 0.2, 10.0),
            channel: ChannelMode::Mono,
            theme: crate::theme::dark(),
        }
    }
}

impl View for OscilloscopeView {
    fn handle_input(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        if ui.rect_contains_pointer(rect) {
            self.y_scale.handle_scroll(ui);
        }
    }

    fn draw(&mut self, ui: &mut egui::Ui, _frame: &AudioFrame, rect: egui::Rect) {
        if rect.width() <= 1.0 { return; }

        let audio = _frame.channel_data(self.channel);
        if audio.len() < 2 { return; }

        let (response, painter) = ui.allocate_painter(rect.size(), egui::Sense::hover());
        let r = response.rect;

        let step = ((audio.len() as f32) / r.width()).ceil() as usize;
        let step = step.max(1);
        let count = ((audio.len() - 1) / step) + 1;
        let denom = (count.saturating_sub(1)).max(1) as f32;
        let half_h = r.height() * 0.5;
        let center_y = r.center().y;

        let points: Vec<egui::Pos2> = audio.iter().step_by(step).enumerate()
            .map(|(idx, &s)| {
                let t = idx as f32 / denom;
                let x = egui::lerp(r.left()..=r.right(), t);
                let y = center_y - s.clamp(-1.0, 1.0) * half_h * self.y_scale.value;
                egui::pos2(x, y)
            })
            .collect();

        painter.add(egui::Shape::line(points, egui::Stroke::new(1.5, self.theme.line)));
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Y Scale");
        ui.add(egui::Slider::new(&mut self.y_scale.value, 0.2..=10.0).logarithmic(true));
        ui.label("Stereo");
        channel_select_ui(ui, &mut self.channel);
    }
}
