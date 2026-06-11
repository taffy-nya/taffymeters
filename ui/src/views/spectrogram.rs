use eframe::egui;
use std::collections::VecDeque;
use taffymeters_core::{
    dsp::LogSpectrumMapper,
    frame::AudioFrame,
};
use crate::theme::Theme;
use super::{
    flow::{Direction, FlowTexture},
    traits::View,
};

pub struct SpectrogramView {
    history: VecDeque<Vec<f32>>,
    flow_speed: f32,
    mapper: LogSpectrumMapper,
    flow: FlowTexture,
    direction: Direction,
    theme: &'static Theme,
}

impl SpectrogramView {
    pub fn new() -> Self {
        Self {
            history: VecDeque::new(),
            flow_speed: 200.0,
            mapper: LogSpectrumMapper::new(300),
            flow: FlowTexture::new(),
            direction: Direction::RtoL,
            theme: crate::theme::dark(),
        }
    }

    fn history_len(&self, rect: egui::Rect) -> usize {
        let pixels = self.direction.history_pixels(rect).max(1.0);
        ((pixels * 60.0 / self.flow_speed) as usize).clamp(4, 4000)
    }

    fn reset_texture(&mut self) {
        self.flow.reset();
    }
}

impl View for SpectrogramView {
    fn handle_input(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        if !ui.rect_contains_pointer(rect) { return; }
        let (scroll, zoom_delta) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
        let factor = if (zoom_delta - 1.0).abs() > f32::EPSILON {
            zoom_delta
        } else {
            (1.0 + scroll * 0.001).clamp(0.8, 1.25)
        };
        if (factor - 1.0).abs() > f32::EPSILON {
            self.flow_speed = (self.flow_speed * factor).clamp(20.0, 4000.0);
            self.reset_texture();
        }
    }

    fn draw(&mut self, ui: &mut egui::Ui, frame: &AudioFrame, rect: egui::Rect) {
        let column: Vec<f32> = self.mapper
            .map(&frame.fft, frame.sample_rate)
            .into_iter()
            .map(LogSpectrumMapper::to_db)
            .collect();

        let w = self.history_len(rect);
        let h = self.mapper.bands;
        let size = self.direction.texture_size(w, h);
        let options = egui::TextureOptions::LINEAR_REPEAT;

        self.history.push_front(column);
        self.history.truncate(w);

        if self.flow.matches_size(size) {
            let column = self.history.front().unwrap();
            match self.direction {
                Direction::LtoR | Direction::RtoL => {
                    self.flow.push_patch(self.direction, w, column_image(column, h, self.theme), options);
                }
                Direction::UtoD | Direction::DtoU => {
                    self.flow.push_patch(self.direction, w, row_image(column, h, self.theme), options);
                }
            }
        } else {
            self.flow.ensure(ui, "spectrogram", history_image(&self.history, w, h, self.direction, self.theme), options);
        }

        let (response, painter) = ui.allocate_painter(rect.size(), egui::Sense::hover());
        self.flow.paint(&painter, response.rect, self.direction, w, h);
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Flow Speed (px/s)");
        if ui.add(egui::Slider::new(&mut self.flow_speed, 20.0..=4000.0).logarithmic(true)).changed() {
            self.reset_texture();
        }
        ui.separator();
        ui.label("Band Count");
        let mut bands = self.mapper.bands;
        if ui.add(egui::Slider::new(&mut bands, 50..=600)).changed() {
            self.mapper = LogSpectrumMapper::new(bands);
            self.history.clear();
            self.reset_texture();
        }
        ui.separator();
        ui.label("Direction");
        ui.horizontal(|ui| {
            if ui.selectable_value(&mut self.direction, Direction::LtoR, "From Left").changed() { self.reset_texture(); }
            if ui.selectable_value(&mut self.direction, Direction::RtoL, "From Right").changed() { self.reset_texture(); }
            if ui.selectable_value(&mut self.direction, Direction::UtoD, "From Top").changed() { self.reset_texture(); }
            if ui.selectable_value(&mut self.direction, Direction::DtoU, "From Bottom").changed() { self.reset_texture(); }
        });
    }

    fn needs_repaint(&self) -> bool { true }
}

fn history_image(history: &VecDeque<Vec<f32>>, w: usize, h: usize, direction: Direction, theme: &Theme) -> egui::ColorImage {
    let size = direction.texture_size(w, h);
    let mut pixels = vec![egui::Color32::TRANSPARENT; size[0] * size[1]];
    for (i, column) in history.iter().take(w).enumerate() {
        let pos = direction.history_pos(i, w);
        match direction {
            Direction::LtoR | Direction::RtoL => {
                for (y, color) in column_pixels(column, h, theme).into_iter().enumerate() {
                    pixels[y * size[0] + pos] = color;
                }
            }
            Direction::UtoD | Direction::DtoU => {
                for (x, color) in row_pixels(column, h, theme).into_iter().enumerate() {
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

fn column_image(column: &[f32], h: usize, theme: &Theme) -> egui::ColorImage {
    egui::ColorImage {
        size: [1, h],
        source_size: egui::vec2(1.0, h as f32),
        pixels: column_pixels(column, h, theme),
    }
}

fn row_image(column: &[f32], h: usize, theme: &Theme) -> egui::ColorImage {
    egui::ColorImage {
        size: [h, 1],
        source_size: egui::vec2(h as f32, 1.0),
        pixels: row_pixels(column, h, theme),
    }
}

fn column_pixels(column: &[f32], h: usize, theme: &Theme) -> Vec<egui::Color32> {
    let mut pixels = vec![egui::Color32::TRANSPARENT; h];
    for (y_freq, &val) in column.iter().take(h).enumerate() {
        pixels[h - 1 - y_freq] = theme.heatmap_db(val);
    }
    pixels
}

fn row_pixels(column: &[f32], h: usize, theme: &Theme) -> Vec<egui::Color32> {
    let mut pixels = vec![egui::Color32::TRANSPARENT; h];
    for (x_freq, &val) in column.iter().take(h).enumerate() {
        pixels[x_freq] = theme.heatmap_db(val);
    }
    pixels
}
