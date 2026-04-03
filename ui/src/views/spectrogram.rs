use eframe::egui;
use std::collections::VecDeque;
use taffymeters_core::dsp::LogSpectrumMapper;
use taffymeters_core::signal::AudioData;
use super::traits::View;

pub struct SpectrogramView {
    history: VecDeque<Vec<f32>>,
    max_history: usize,
    mapper: LogSpectrumMapper,
    texture: Option<egui::TextureHandle>,
}

impl SpectrogramView {
    pub fn new() -> Self {
        Self {
            history: VecDeque::new(),
            max_history: 500,
            mapper: LogSpectrumMapper::new(300),
            texture: None,
        }
    }
}

impl View for SpectrogramView {
    fn draw(&mut self, ui: &mut egui::Ui, data: &AudioData) {
        let column: Vec<f32> = self.mapper
            .map(&data.fft, data.sample_rate)
            .into_iter()
            .map(LogSpectrumMapper::to_db)
            .collect();

        self.history.push_front(column);
        self.history.truncate(self.max_history);

        let w = self.max_history;
        let h = self.mapper.bands;
        let mut pixels = vec![egui::Color32::TRANSPARENT; w * h];

        for (x, col) in self.history.iter().enumerate() {
            if x >= w { break; } 
            for (y_freq, &val) in col.iter().enumerate() {
                if y_freq >= h { continue; }
                let y_img = h - 1 - y_freq;
                pixels[y_img * w + x] = amplitude_to_color(val);
            }
        }

        let image = egui::ColorImage {
            size: [w, h],
            source_size: egui::vec2(w as f32, h as f32),
            pixels,
        };

        match &mut self.texture {
            Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
            None => {
                self.texture = Some(ui.ctx().load_texture(
                    "spectrogram", image, egui::TextureOptions::LINEAR,
                ));
            }
        }

        let desired = ui.available_size_before_wrap();
        let (response, painter) = ui.allocate_painter(desired, egui::Sense::hover());
        let rect = response.rect;

        if ui.rect_contains_pointer(rect) { self.handle_scroll(ui); }

        if let Some(tex) = &self.texture {
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            painter.image(tex.id(), response.rect, uv, egui::Color32::WHITE);
        }
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Length");
        if ui.add(egui::Slider::new(&mut self.max_history, 100..=2000)).changed() {
            self.history.truncate(self.max_history);
            self.texture = None;
        }
        ui.separator();
        ui.label("Band Count");
        let mut bands = self.mapper.bands;
        if ui.add(egui::Slider::new(&mut bands, 50..=600)).changed() {
            self.mapper = LogSpectrumMapper::new(bands);
            self.history.clear();
            self.texture = None;
        }
    }

    fn repaint_interval(&self) -> Option<std::time::Duration> {
        // 流动动画需要持续重绘，始终以 60fps 刷新
        Some(std::time::Duration::from_millis(16))
    }
}

impl SpectrogramView {
    fn handle_scroll(&mut self, ui: &mut egui::Ui) {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        let factor = (1.0 + scroll * 0.001).clamp(0.8, 1.25);
        self.max_history = ((self.max_history as f32) * factor).clamp(100.0, 2000.0) as usize;
    }
}

/// 幅度 (0.0 ~ 2.5) -> 热力图颜色 (黑 -> 蓝 -> 紫 -> 红 -> 黄 -> 白)
fn amplitude_to_color(val: f32) -> egui::Color32 {
    let t = (val / 2.5).clamp(0.0, 1.0);
    if t < 0.05 { return egui::Color32::TRANSPARENT; }
    let r = (t * 3.0 - 1.0).clamp(0.0, 1.0) * 255.0;
    let g = (t * 3.0 - 2.0).clamp(0.0, 1.0) * 255.0;
    let b = (t * 3.0).clamp(0.0, 1.0) * 255.0;
    let a = t * 255.0;
    egui::Color32::from_rgba_unmultiplied(r as u8, g as u8, b as u8, a as u8)
}
