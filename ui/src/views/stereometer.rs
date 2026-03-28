use eframe::egui;
use taffymeters_core::signal::AudioData;
use super::traits::View;

pub struct StereometerView {
    point_alpha: u8,
    scale: f32,
    _decay: f32, // TODO: 用于点的衰减效果
}

impl StereometerView {
    pub fn new() -> Self {
        Self { point_alpha: 180, scale: 1.0, _decay: 0.95 }
    }
}

impl View for StereometerView {
    fn draw(&mut self, ui: &mut egui::Ui, data: &AudioData) {
        let desired = ui.available_size_before_wrap();
        let (response, painter) = ui.allocate_painter(desired, egui::Sense::hover());
        let rect = response.rect;

        if ui.rect_contains_pointer(rect) { self.handle_scroll(ui); }

        let center = rect.center();
        let radius = rect.size().min_elem() * 0.45;

        // 背景辅助线
        let gray = egui::Stroke::new(0.5, egui::Color32::from_gray(60));
        painter.line_segment(
            [egui::pos2(center.x, center.y - radius), egui::pos2(center.x, center.y + radius)],
            gray,
        );
        painter.line_segment(
            [egui::pos2(center.x - radius, center.y), egui::pos2(center.x + radius, center.y)],
            gray,
        );
        // L / R 单声道参考线
        let d = radius * 0.707;
        painter.line_segment([egui::pos2(center.x - d, center.y + d), egui::pos2(center.x + d, center.y - d)], gray);
        painter.line_segment([egui::pos2(center.x - d, center.y - d), egui::pos2(center.x + d, center.y + d)], gray);

        if data.num_channels < 2 {
            painter.text(
                center, egui::Align2::CENTER_CENTER,
                "Stereo view requires at least 2 channels",
                egui::FontId::proportional(14.0),
                egui::Color32::GRAY,
            );
            return;
        }

        let l_ch = &data.channels[0];
        let r_ch = &data.channels[1];
        let color = egui::Color32::from_rgba_unmultiplied(100, 220, 255, self.point_alpha);

        let step = (l_ch.len() / 512).max(1);

        for (l, r) in l_ch.iter().step_by(step).zip(r_ch.iter().step_by(step)) {
            let mid  = (l + r) * 0.707;
            let side = (l - r) * 0.707;
            let px = center.x + side * radius * self.scale;
            let py = center.y - mid  * radius * self.scale;
            painter.circle_filled(egui::pos2(px, py), 1.5, color);
        }
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Scale");
        ui.add(egui::Slider::new(&mut self.scale, 0.5..=10.0).logarithmic(true));
        ui.separator();
        ui.label("Point Alpha");
        ui.add(egui::Slider::new(&mut self.point_alpha, 20u8..=255));
        // TODO: 衰减、颜色映射等
    }
}

impl StereometerView {
    fn handle_scroll(&mut self, ui: &mut egui::Ui) {
        let scroll = ui.input(|i| {
            let dy = i.smooth_scroll_delta.y;
            if dy.abs() > f32::EPSILON { dy } else { i.raw_scroll_delta.y }
        });
        let factor = (1.0 + scroll * 0.001).clamp(0.8, 1.25);
        self.scale = (self.scale * factor).clamp(0.5, 10.0);
    }
}
