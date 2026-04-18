use std::collections::VecDeque;
use eframe::egui::{self, Color32};
use taffymeters_core::signal::AudioData;
use super::flow::{Direction, FlowTexture};
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
    pending: Vec<f32>,
    y_scale: f32,
    flow_speed: f32,
    channel: ChannelMode,
    flow: FlowTexture,
    direction: Direction,
}

impl WaveformView {
    pub fn new() -> Self {
        Self {
            history: VecDeque::new(),
            pending: Vec::new(),
            y_scale: 1.0,
            flow_speed: 200.0,
            channel: ChannelMode::Mono,
            flow: FlowTexture::new(),
            direction: Direction::RtoL,
        }
    }

    /// samples per pixel
    fn spp(&self, sample_rate: f32) -> f32 {
        (sample_rate / self.flow_speed).max(0.01)
    }

    fn history_len(&self, rect: egui::Rect) -> usize {
        self.direction.history_pixels(rect).max(1.0) as usize
    }

    fn cross_len(&self, rect: egui::Rect) -> usize {
        self.direction.cross_pixels(rect).max(1.0) as usize
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

    fn reset_texture(&mut self) {
        self.flow.reset();
        self.pending.clear();
    }
}

impl View for WaveformView {
    fn draw(&mut self, ui: &mut egui::Ui, data: &AudioData) {
        let desired = ui.available_size_before_wrap();
        let (response, painter) = ui.allocate_painter(desired, egui::Sense::hover());
        let rect = response.rect;
        if rect.width() <= 1.0 || rect.height() <= 1.0 { return; }

        if ui.rect_contains_pointer(rect) { self.handle_scroll(ui); }

        let new_samples = self.new_samples(data).to_vec();
        self.history.extend(new_samples.iter().copied());
        self.pending.extend(new_samples);

        let spp = self.spp(data.sample_rate);
        let history_len = self.history_len(rect);
        let cross_len = self.cross_len(rect);
        let size = self.direction.texture_size(history_len, cross_len);
        let options = egui::TextureOptions::LINEAR_REPEAT;

        let max_keep = ((history_len as f32 * spp) as usize + 2) * 2;
        while self.history.len() > max_keep {
            self.history.pop_front();
        }

        if !self.flow.matches_size(size) {
            self.flow.ensure(
                ui,
                "waveform",
                waveform_image(&self.history, history_len, cross_len, spp, self.y_scale, self.direction),
                options,
            );
            self.pending.clear();
        }

        let samples_per_patch = spp.round().max(1.0) as usize;
        while self.pending.len() >= samples_per_patch {
            let samples: Vec<f32> = self.pending.drain(..samples_per_patch).collect();
            let patch = match self.direction {
                Direction::LtoR | Direction::RtoL => column_image(&samples, cross_len, self.y_scale),
                Direction::UtoD | Direction::DtoU => row_image(&samples, cross_len, self.y_scale),
            };
            self.flow.push_patch(self.direction, history_len, patch, options);
        }

        self.flow.paint(&painter, response.rect, self.direction, history_len, cross_len);
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Y Scale");
        if ui.add(egui::Slider::new(&mut self.y_scale, 0.1..=20.0).logarithmic(true)).changed() {
            self.reset_texture();
        }

        ui.add_space(8.0);
        ui.label("Flow Speed (px/s)");
        if ui.add(egui::Slider::new(&mut self.flow_speed, 20.0..=4000.0).logarithmic(true)).changed() {
            self.reset_texture();
        }

        ui.add_space(8.0);
        ui.label("Direction");
        ui.horizontal(|ui| {
            if ui.selectable_value(&mut self.direction, Direction::LtoR, "From Left").changed() { self.reset_texture(); }
            if ui.selectable_value(&mut self.direction, Direction::RtoL, "From Right").changed() { self.reset_texture(); }
            if ui.selectable_value(&mut self.direction, Direction::UtoD, "From Top").changed() { self.reset_texture(); }
            if ui.selectable_value(&mut self.direction, Direction::DtoU, "From Bottom").changed() { self.reset_texture(); }
        });

        ui.add_space(8.0);
        ui.label("Channel");
        ui.horizontal(|ui| {
            let old = self.channel;
            ui.selectable_value(&mut self.channel, ChannelMode::Left, "Left");
            ui.selectable_value(&mut self.channel, ChannelMode::Mono, "Mono");
            ui.selectable_value(&mut self.channel, ChannelMode::Right, "Right");
            if self.channel != old {
                self.history.clear();
                self.reset_texture();
            }
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
                self.reset_texture();
            }
        } else if scroll.abs() > f32::EPSILON {
            let factor = (1.0 + scroll * 0.001).clamp(0.8, 1.25);
            self.y_scale = (self.y_scale * factor).clamp(0.1, 20.0);
            self.reset_texture();
        }
    }
}

fn waveform_image(
    history: &VecDeque<f32>,
    history_len: usize,
    cross_len: usize,
    spp: f32,
    y_scale: f32,
    direction: Direction,
) -> egui::ColorImage {
    let size = direction.texture_size(history_len, cross_len);
    let mut pixels = vec![egui::Color32::TRANSPARENT; size[0] * size[1]];
    let hist: Vec<f32> = history.iter().copied().collect();
    let hist_len = hist.len() as f32;

    for i in 0..history_len {
        let end_f = hist_len - i as f32 * spp;
        let start_f = end_f - spp;
        if end_f <= 0.0 { break; }

        let i_start = start_f.max(0.0).floor() as usize;
        let i_end = end_f.min(hist_len - 1.0).ceil() as usize;
        if i_start > i_end { continue; }

        let pos = direction.history_pos(i, history_len);
        let samples = &hist[i_start..=i_end];
        match direction {
            Direction::LtoR | Direction::RtoL => {
                for (y, color) in column_pixels(samples, cross_len, y_scale).into_iter().enumerate() {
                    pixels[y * size[0] + pos] = color;
                }
            }
            Direction::UtoD | Direction::DtoU => {
                for (x, color) in row_pixels(samples, cross_len, y_scale).into_iter().enumerate() {
                    pixels[pos * size[0] + x] = color;
                }
            }
        }
    }

    egui::ColorImage {
        size,
        source_size: egui::vec2(size[0] as f32, size[1] as f32),
        pixels,
    }
}

fn column_image(samples: &[f32], height: usize, y_scale: f32) -> egui::ColorImage {
    egui::ColorImage {
        size: [1, height],
        source_size: egui::vec2(1.0, height as f32),
        pixels: column_pixels(samples, height, y_scale),
    }
}

fn row_image(samples: &[f32], width: usize, y_scale: f32) -> egui::ColorImage {
    egui::ColorImage {
        size: [width, 1],
        source_size: egui::vec2(width as f32, 1.0),
        pixels: row_pixels(samples, width, y_scale),
    }
}

fn column_pixels(samples: &[f32], height: usize, y_scale: f32) -> Vec<egui::Color32> {
    let mut pixels = vec![egui::Color32::TRANSPARENT; height];
    let (lo, hi, peak) = sample_bounds(samples);
    let color = amp_color((peak * y_scale).clamp(0.0, 1.0));
    let half = height as f32 * 0.5;
    let center = height as f32 * 0.5;
    let y_hi = (center - hi.clamp(-1.0, 1.0) * half * y_scale).clamp(0.0, height.saturating_sub(1) as f32) as usize;
    let y_lo = (center - lo.clamp(-1.0, 1.0) * half * y_scale).clamp(0.0, height.saturating_sub(1) as f32) as usize;
    for y in y_hi.min(y_lo)..=y_hi.max(y_lo) {
        pixels[y] = color;
    }
    pixels
}

fn row_pixels(samples: &[f32], width: usize, y_scale: f32) -> Vec<egui::Color32> {
    let mut pixels = vec![egui::Color32::TRANSPARENT; width];
    let (lo, hi, peak) = sample_bounds(samples);
    let color = amp_color((peak * y_scale).clamp(0.0, 1.0));
    let half = width as f32 * 0.5;
    let center = width as f32 * 0.5;
    let x_lo = (center + lo.clamp(-1.0, 1.0) * half * y_scale).clamp(0.0, width.saturating_sub(1) as f32) as usize;
    let x_hi = (center + hi.clamp(-1.0, 1.0) * half * y_scale).clamp(0.0, width.saturating_sub(1) as f32) as usize;
    for x in x_lo.min(x_hi)..=x_lo.max(x_hi) {
        pixels[x] = color;
    }
    pixels
}

fn sample_bounds(samples: &[f32]) -> (f32, f32, f32) {
    let (mut lo, mut hi, mut peak) = (f32::INFINITY, -f32::INFINITY, 0.0_f32);
    for &s in samples {
        lo = lo.min(s);
        hi = hi.max(s);
        peak = peak.max(s.abs());
    }
    if samples.is_empty() { (0.0, 0.0, 0.0) } else { (lo, hi, peak) }
}
