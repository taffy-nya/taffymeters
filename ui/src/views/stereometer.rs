use eframe::egui;
use taffymeters_core::frame::AudioFrame;
use crate::theme::Theme;
use super::{
    components::ScaleControl,
    traits::View,
};

pub struct StereometerView {
    point_alpha: u8,
    scale: ScaleControl,
    _decay: f32,
    theme: &'static Theme,
}

impl StereometerView {
    pub fn new() -> Self {
        Self {
            point_alpha: 180,
            scale: ScaleControl::new(1.0, 0.5, 10.0),
            _decay: 0.95,
            theme: crate::theme::dark(),
        }
    }
}

impl View for StereometerView {
    fn handle_input(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        if ui.rect_contains_pointer(rect) {
            self.scale.handle_scroll(ui);
        }
    }

    fn draw(&mut self, ui: &mut egui::Ui, frame: &AudioFrame, rect: egui::Rect) {
        let (response, painter) = ui.allocate_painter(rect.size(), egui::Sense::hover());
        let r = response.rect;

        let center = r.center();
        let radius = r.size().min_elem() * 0.45;

        let gray = egui::Stroke::new(0.5, self.theme.goniometer_guide);
        painter.line_segment(
            [egui::pos2(center.x, center.y - radius), egui::pos2(center.x, center.y + radius)],
            gray,
        );
        painter.line_segment(
            [egui::pos2(center.x - radius, center.y), egui::pos2(center.x + radius, center.y)],
            gray,
        );
        let d = radius * 0.707;
        painter.line_segment([egui::pos2(center.x - d, center.y + d), egui::pos2(center.x + d, center.y - d)], gray);
        painter.line_segment([egui::pos2(center.x - d, center.y - d), egui::pos2(center.x + d, center.y + d)], gray);

        if frame.num_channels < 2 {
            painter.text(
                center, egui::Align2::CENTER_CENTER,
                "Stereo view requires at least 2 channels",
                egui::FontId::proportional(14.0),
                egui::Color32::GRAY,
            );
            return;
        }

        let l_ch = &frame.channels[0];
        let r_ch = &frame.channels[1];
        let color = egui::Color32::from_rgba_unmultiplied(
            self.theme.goniometer_points.r(),
            self.theme.goniometer_points.g(),
            self.theme.goniometer_points.b(),
            self.point_alpha,
        );

        let step = (l_ch.len() / 512).max(1);

        for (l, r) in l_ch.iter().step_by(step).zip(r_ch.iter().step_by(step)) {
            let mid  = (l + r) * 0.707;
            let side = (l - r) * 0.707;
            let px = center.x + side * radius * self.scale.value;
            let py = center.y - mid  * radius * self.scale.value;
            painter.circle_filled(egui::pos2(px, py), 1.5, color);
        }
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Scale");
        ui.add(egui::Slider::new(&mut self.scale.value, 0.5..=10.0).logarithmic(true));
        ui.separator();
        ui.label("Point Alpha");
        ui.add(egui::Slider::new(&mut self.point_alpha, 20u8..=255));
    }
}
