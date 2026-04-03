use std::collections::VecDeque;
use eframe::egui::{self, Color32};
use taffymeters_core::signal::AudioData;
use super::traits::View;

const COLOR_STOPS: &[(f32, (u8, u8, u8))] = &[
    (0.00, (173, 216, 230)),   // LIGHT_BLUE
    (0.25, (  0, 150, 255)),   // 蓝
    (0.55, (140,  80, 255)),   // 紫
    (0.78, (255,  50, 180)),   // 粉
    (1.00, (255,  30,  80)),   // 红
];

fn amp_color(amp: f32) -> Color32 {
    let amp = amp.clamp(0.0, 1.0);
    for w in COLOR_STOPS.windows(2) {
        let (t0, c0) = w[0];
        let (t1, c1) = w[1];
        if amp <= t1 {
            let t = if (t1 - t0).abs() < 1e-6 { 0.0 } else { (amp - t0) / (t1 - t0) };
            let lerp = |a: u8, b: u8| (a as f32 + t * (b as f32 - a as f32)) as u8;
            return Color32::from_rgb(lerp(c0.0, c1.0), lerp(c0.1, c1.1), lerp(c0.2, c1.2));
        }
    }
    Color32::from_rgb(255, 40, 40)
}

#[derive(PartialEq, Clone, Copy)]
enum ChannelMode { Mono, Left, Right }

pub struct WaveformView {
    history: VecDeque<f32>,
    y_scale: f32,
    flow_speed: f32,    // px/s
    channel: ChannelMode,
}

impl WaveformView {
    pub fn new() -> Self {
        Self {
            history: VecDeque::new(),
            y_scale: 1.0,
            flow_speed: 200.0,
            channel: ChannelMode::Mono,
        }
    }

    /// samples per pixel（浮点，不截断）
    fn spp(&self, sample_rate: f32) -> f32 {
        (sample_rate / self.flow_speed).max(0.01)
    }

    fn new_samples<'a>(&self, data: &'a AudioData) -> &'a [f32] {
        let n = data.new_sample_count;
        if n == 0 { return &[]; }
        let src: &[f32] = match self.channel {
            ChannelMode::Mono => &data.mono,
            ChannelMode::Left => data.channels.first().map(|v| v.as_slice()).unwrap_or(&data.mono),
            ChannelMode::Right => data.channels.get(1).map(|v| v.as_slice()).unwrap_or(&data.mono),
        };
        let start = src.len().saturating_sub(n);
        &src[start..]
    }
}

impl View for WaveformView {
    fn draw(&mut self, ui: &mut egui::Ui, data: &AudioData) {
        let desired = ui.available_size_before_wrap();
        let (response, painter) = ui.allocate_painter(desired, egui::Sense::hover());
        let rect = response.rect;
        if rect.width() <= 1.0 || rect.height() <= 1.0 { return; }

        if ui.rect_contains_pointer(rect) { self.handle_scroll(ui); }

        self.history.extend(self.new_samples(data).iter().copied());

        let spp = self.spp(data.sample_rate);

        let max_keep = ((rect.width() * spp) as usize + 2) * 2;
        while self.history.len() > max_keep {
            self.history.pop_front();
        }

        if self.history.is_empty() { return; }

        let hist: Vec<f32> = self.history.iter().copied().collect();
        let hist_len = hist.len() as f32;

        let width = rect.width() as usize;
        let half_h = rect.height() * 0.5;
        let center_y = rect.center().y;

        for px in 0..width {
            let samples_from_end = (width - 1 - px) as f32 * spp;
            let end_f   = hist_len - samples_from_end;
            let start_f = end_f - spp;

            if end_f <= 0.0 || start_f >= hist_len { continue; }

            let i_start = start_f.max(0.0).floor() as usize;
            let i_end   = end_f.min(hist_len - 1.0).ceil() as usize;

            if i_start > i_end { continue; }

            let (mut lo, mut hi, mut peak) = (f32::INFINITY, -f32::INFINITY, 0_f32);
            for &s in &hist[i_start..=i_end] {
                lo = lo.min(s);
                hi = hi.max(s);
                peak = peak.max(s.abs());
            }

            let visual_amp = (peak * self.y_scale).clamp(0.0, 1.0);
            let color = amp_color(visual_amp);

            let y_hi = (center_y - hi.clamp(-1.0, 1.0) * half_h * self.y_scale)
                        .clamp(rect.min.y, rect.max.y);
            let y_lo = (center_y - lo.clamp(-1.0, 1.0) * half_h * self.y_scale)
                        .clamp(rect.min.y, rect.max.y);
            let y_top = y_hi.min(y_lo);
            let y_bot = y_hi.max(y_lo).max(y_top + 1.0);

            let x = rect.left() + px as f32 + 0.5;
            painter.line_segment(
                [egui::pos2(x, y_top), egui::pos2(x, y_bot)],
                egui::Stroke::new(1.0, color),
            );
        }
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Y Scale");
        ui.add(egui::Slider::new(&mut self.y_scale, 0.1..=20.0).logarithmic(true));

        ui.add_space(8.0);
        ui.label("Flow Speed (px/s)");
        ui.add(egui::Slider::new(&mut self.flow_speed, 20.0..=4000.0).logarithmic(true));

        ui.add_space(8.0);
        ui.label("Channel");
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.channel, ChannelMode::Left,  "Left");
            ui.radio_value(&mut self.channel, ChannelMode::Mono,  "Mono");
            ui.radio_value(&mut self.channel, ChannelMode::Right, "Right");
        });
    }

    fn repaint_interval(&self) -> Option<std::time::Duration> {
        // 流动动画需要持续重绘，始终以 60fps 刷新
        Some(std::time::Duration::from_millis(16))
    }
}

impl WaveformView {
    fn handle_scroll(&mut self, ui: &mut egui::Ui) {
        let (scroll, ctrl, zoom_delta) = ui.input(|i| {
            (i.smooth_scroll_delta.y, i.modifiers.ctrl, i.zoom_delta())
        });
        if ctrl {
            if (zoom_delta - 1.0).abs() > f32::EPSILON {
                self.flow_speed = (self.flow_speed * zoom_delta).clamp(20.0, 4000.0);
            }
        } else if scroll.abs() > f32::EPSILON {
            let factor = (1.0 + scroll * 0.001).clamp(0.8, 1.25);
            self.y_scale = (self.y_scale * factor).clamp(0.1, 20.0);
        }
    }
}