use std::collections::VecDeque;
use eframe::egui;
use taffymeters_core::{
    channel::ChannelMode,
    frame::AudioFrame,
};
use crate::theme::Theme;
use super::{
    components::{ScaleControl, channel_select_ui},
    flow::{Direction, FlowTexture},
    traits::View,
};

pub struct WaveformView {
    history: VecDeque<f32>,
    pending: Vec<f32>,
    y_scale: ScaleControl,
    flow_speed: f32,
    channel: ChannelMode,
    flow: FlowTexture,
    direction: Direction,
    theme: &'static Theme,
}

impl WaveformView {
    pub fn new() -> Self {
        Self {
            history: VecDeque::new(),
            pending: Vec::new(),
            y_scale: ScaleControl::new(1.0, 0.1, 20.0),
            flow_speed: 200.0,
            channel: ChannelMode::Mono,
            flow: FlowTexture::new(),
            direction: Direction::RtoL,
            theme: crate::theme::dark(),
        }
    }

    fn spp(&self, sample_rate: f32) -> f32 {
        (sample_rate / self.flow_speed).max(0.01)
    }

    fn history_len(&self, rect: egui::Rect) -> usize {
        self.direction.history_pixels(rect).max(1.0) as usize
    }

    fn cross_len(&self, rect: egui::Rect) -> usize {
        self.direction.cross_pixels(rect).max(1.0) as usize
    }

    fn new_samples(&self, frame: &AudioFrame) -> Vec<f32> {
        let n = frame.new_sample_count;
        if n == 0 { return vec![]; }
        let src = frame.channel_data(self.channel);
        let start = src.len().saturating_sub(n);
        src[start..].to_vec()
    }

    fn reset_texture(&mut self) {
        self.flow.reset();
        self.pending.clear();
    }
}

impl View for WaveformView {
    fn handle_input(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        if !ui.rect_contains_pointer(rect) { return; }

        let (scroll, ctrl, zoom_delta) = ui.input(|i| {
            (i.smooth_scroll_delta.y, i.modifiers.ctrl, i.zoom_delta())
        });
        if ctrl {
            if (zoom_delta - 1.0).abs() > f32::EPSILON {
                self.flow_speed = (self.flow_speed * zoom_delta).clamp(20.0, 4000.0);
                self.reset_texture();
            }
        } else if scroll.abs() > f32::EPSILON {
            self.y_scale.handle_scroll(ui);
            self.reset_texture();
        }
    }

    fn draw(&mut self, ui: &mut egui::Ui, frame: &AudioFrame, rect: egui::Rect) {
        if rect.width() <= 1.0 || rect.height() <= 1.0 { return; }

        let new_samples = self.new_samples(frame);
        self.history.extend(new_samples.iter().copied());
        self.pending.extend(new_samples);

        let spp = self.spp(frame.sample_rate);
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
                waveform_image(&self.history, history_len, cross_len, spp, self.y_scale.value, self.direction, self.theme),
                options,
            );
            self.pending.clear();
        }

        let samples_per_patch = spp.round().max(1.0) as usize;
        while self.pending.len() >= samples_per_patch {
            let samples: Vec<f32> = self.pending.drain(..samples_per_patch).collect();
            let patch = match self.direction {
                Direction::LtoR | Direction::RtoL => column_image(&samples, cross_len, self.y_scale.value, self.theme),
                Direction::UtoD | Direction::DtoU => row_image(&samples, cross_len, self.y_scale.value, self.theme),
            };
            self.flow.push_patch(self.direction, history_len, patch, options);
        }

        let (response, painter) = ui.allocate_painter(rect.size(), egui::Sense::hover());
        self.flow.paint(&painter, response.rect, self.direction, history_len, cross_len);
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Y Scale");
        if ui.add(egui::Slider::new(&mut self.y_scale.value, 0.1..=20.0).logarithmic(true)).changed() {
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
            channel_select_ui(ui, &mut self.channel);
            if self.channel != old {
                self.history.clear();
                self.reset_texture();
            }
        });
    }

    fn needs_repaint(&self) -> bool { true }
}

fn waveform_image(
    history: &VecDeque<f32>,
    history_len: usize,
    cross_len: usize,
    spp: f32,
    y_scale: f32,
    direction: Direction,
    theme: &Theme,
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
                for (y, color) in column_pixels(samples, cross_len, y_scale, theme).into_iter().enumerate() {
                    pixels[y * size[0] + pos] = color;
                }
            }
            Direction::UtoD | Direction::DtoU => {
                for (x, color) in row_pixels(samples, cross_len, y_scale, theme).into_iter().enumerate() {
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

fn column_image(samples: &[f32], height: usize, y_scale: f32, theme: &Theme) -> egui::ColorImage {
    egui::ColorImage {
        size: [1, height],
        source_size: egui::vec2(1.0, height as f32),
        pixels: column_pixels(samples, height, y_scale, theme),
    }
}

fn row_image(samples: &[f32], width: usize, y_scale: f32, theme: &Theme) -> egui::ColorImage {
    egui::ColorImage {
        size: [width, 1],
        source_size: egui::vec2(width as f32, 1.0),
        pixels: row_pixels(samples, width, y_scale, theme),
    }
}

fn column_pixels(samples: &[f32], height: usize, y_scale: f32, theme: &Theme) -> Vec<egui::Color32> {
    let mut pixels = vec![egui::Color32::TRANSPARENT; height];
    let (lo, hi, peak) = sample_bounds(samples);
    let color = theme.waveform_amplitude((peak * y_scale).clamp(0.0, 1.0));
    let half = height as f32 * 0.5;
    let center = height as f32 * 0.5;
    let y_hi = (center - hi.clamp(-1.0, 1.0) * half * y_scale).clamp(0.0, height.saturating_sub(1) as f32) as usize;
    let y_lo = (center - lo.clamp(-1.0, 1.0) * half * y_scale).clamp(0.0, height.saturating_sub(1) as f32) as usize;
    pixels[y_hi.min(y_lo)..=y_hi.max(y_lo)].fill(color);
    pixels
}

fn row_pixels(samples: &[f32], width: usize, y_scale: f32, theme: &Theme) -> Vec<egui::Color32> {
    let mut pixels = vec![egui::Color32::TRANSPARENT; width];
    let (lo, hi, peak) = sample_bounds(samples);
    let color = theme.waveform_amplitude((peak * y_scale).clamp(0.0, 1.0));
    let half = width as f32 * 0.5;
    let center = width as f32 * 0.5;
    let x_lo = (center + lo.clamp(-1.0, 1.0) * half * y_scale).clamp(0.0, width.saturating_sub(1) as f32) as usize;
    let x_hi = (center + hi.clamp(-1.0, 1.0) * half * y_scale).clamp(0.0, width.saturating_sub(1) as f32) as usize;
    pixels[x_lo.min(x_hi)..=x_lo.max(x_hi)].fill(color);
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
