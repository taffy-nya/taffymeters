use eframe::egui;

pub struct OscilloscopeView {
    y_zoom: f32,
    min_zoom: f32,
    max_zoom: f32,
}

impl OscilloscopeView {
    pub fn new() -> Self {
        Self {
            y_zoom: 1.0,
            min_zoom: 0.2,
            max_zoom: 12.0,
        }
    }

    pub fn draw(&mut self, ui: &mut egui::Ui, audio_data: &[f32]) {
        let desired_size = ui.available_size_before_wrap();
        let (response, painter) = ui.allocate_painter(desired_size, egui::Sense::hover());
        let rect = response.rect;
        
        if response.hovered() {
            let scroll = ui.input(|i| {
                let dy = i.smooth_scroll_delta.y;
                if dy.abs() > f32::EPSILON { dy } else { i.raw_scroll_delta.y }
            });
            let factor = (1.0 + scroll * 0.001).clamp(0.8, 1.25);
            self.y_zoom = (self.y_zoom * factor).clamp(self.min_zoom, self.max_zoom);
        }

        if audio_data.len() < 2 || rect.width() <= 1.0 || rect.height() <= 1.0 {
            return;
        }

        let step = ((audio_data.len() as f32) / rect.width().max(1.0)).ceil() as usize;
        let step = step.max(1);
        let half_h = rect.height() * 0.5;
        let center_y = rect.center().y;
        let count = ((audio_data.len() - 1) / step) + 1;
        let denom = (count.saturating_sub(1)).max(1) as f32;

        let mut points = Vec::with_capacity(count);
        for (idx, &sample) in audio_data.iter().step_by(step).enumerate() {
            let t = idx as f32 / denom;
            let x = egui::lerp(rect.left()..=rect.right(), t);
            let y = center_y - sample.clamp(-1.0, 1.0) * half_h * self.y_zoom;
            points.push(egui::pos2(x, y));
        }

        painter.add(egui::Shape::line(
            points,
            egui::Stroke::new(1.5, egui::Color32::LIGHT_BLUE),
        ));
    }
}