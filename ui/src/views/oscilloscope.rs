use eframe::egui;
use taffymeters_core::signal::AudioData;
use super::traits::View;

pub struct OscilloscopeView {
    y_zoom: f32,
    channel: ChannelMode,
}

#[derive(PartialEq, Clone, Copy)]
enum ChannelMode { Mono, Left, Right }

impl OscilloscopeView {
    pub fn new() -> Self {
        Self { y_zoom: 1.0, channel: ChannelMode::Mono }
    }
}

impl View for OscilloscopeView {
    fn draw(&mut self, ui: &mut egui::Ui, data: &AudioData) {
        let desired = ui.available_size_before_wrap();
        let (response, painter) = ui.allocate_painter(desired, egui::Sense::hover());
        let rect = response.rect;

        if response.hovered() {
            let scroll = ui.input(|i| {
                let dy = i.smooth_scroll_delta.y;
                if dy.abs() > f32::EPSILON { dy } else { i.raw_scroll_delta.y }
            });
            let factor = (1.0 + scroll * 0.001).clamp(0.8, 1.25);
            self.y_zoom = (self.y_zoom * factor).clamp(0.2, 12.0);
        }

        let audio: &[f32] = match self.channel {
            ChannelMode::Mono => &data.mono,
            ChannelMode::Left => data.channels.first().map(|v| v.as_slice()).unwrap_or(&data.mono),
            ChannelMode::Right => data.channels.get(1).map(|v| v.as_slice()).unwrap_or(&data.mono),
        };

        if audio.len() < 2 || rect.width() <= 1.0 { return; }

        let step = ((audio.len() as f32) / rect.width()).ceil() as usize;
        let step = step.max(1);
        let count = ((audio.len() - 1) / step) + 1;
        let denom = (count.saturating_sub(1)).max(1) as f32;
        let half_h = rect.height() * 0.5;
        let center_y = rect.center().y;

        let points: Vec<egui::Pos2> = audio.iter().step_by(step).enumerate()
            .map(|(idx, &s)| {
                let t = idx as f32 / denom;
                let x = egui::lerp(rect.left()..=rect.right(), t);
                let y = center_y - s.clamp(-1.0, 1.0) * half_h * self.y_zoom;
                egui::pos2(x, y)
            })
            .collect();

        painter.add(egui::Shape::line(points, egui::Stroke::new(1.5, egui::Color32::LIGHT_BLUE)));
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Y Zoom");
        ui.add(egui::Slider::new(&mut self.y_zoom, 0.2..=12.0).logarithmic(true));
        ui.separator();
        ui.label("Stereo");
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.channel, ChannelMode::Mono, "Mono");
            ui.radio_value(&mut self.channel, ChannelMode::Left, "Left");
            ui.radio_value(&mut self.channel, ChannelMode::Right, "Right");
        });
    }
}