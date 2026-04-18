use eframe::egui;
use taffymeters_core::dsp::LogSpectrumMapper;
use taffymeters_core::signal::AudioData;
use super::traits::View;

pub struct SpectrumView {
    y_scale: f32,
    mapper: LogSpectrumMapper,
    bands: Vec<f32>,
}

impl SpectrumView {
    pub fn new() -> Self {
        Self {
            y_scale: 1.0,
            mapper: LogSpectrumMapper::new(300),
            bands: Vec::new(),
        }
    }
}

impl View for SpectrumView {
    fn draw(&mut self, ui: &mut egui::Ui, data: &AudioData) {
        let desired = ui.available_size_before_wrap();
        let (response, painter) = ui.allocate_painter(desired, egui::Sense::hover());
        let rect = response.rect;

        if ui.rect_contains_pointer(rect) { self.handle_scroll(ui); }

        self.mapper.map_into(&data.fft, data.sample_rate, &mut self.bands);
        for val in &mut self.bands {
            *val = LogSpectrumMapper::to_db(*val);
        }

        if self.bands.len() < 2 || rect.width() <= 1.0 { return; }

        let y_max = 5.0_f32;
        let last = (self.bands.len() - 1) as f32;

        let points: Vec<egui::Pos2> = self.bands.iter().enumerate().map(|(i, &val)| {
            let t = i as f32 / last;
            let x = egui::lerp(rect.left()..=rect.right(), t);
            let y_norm = (val / y_max).clamp(0.0, 1.0);
            let scaled = (y_norm * self.y_scale).clamp(0.0, 1.0);
            let y = egui::lerp(rect.bottom()..=rect.top(), scaled);
            egui::pos2(x, y)
        }).collect();

        painter.add(egui::Shape::line(points, egui::Stroke::new(1.5, egui::Color32::LIGHT_BLUE)));
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Y Scale");
        ui.add(egui::Slider::new(&mut self.y_scale, 0.2..=10.0).logarithmic(true));
        ui.separator();
        ui.label("Band Count");
        let mut bands = self.mapper.bands;
        if ui.add(egui::Slider::new(&mut bands, 50..=600)).changed() {
            self.mapper = LogSpectrumMapper::new(bands);
            self.bands.clear();
        }
    }
}

impl SpectrumView {
    fn handle_scroll(&mut self, ui: &mut egui::Ui) {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        let factor = (1.0 + scroll * 0.001).clamp(0.8, 1.25);
        self.y_scale = (self.y_scale * factor).clamp(0.2, 10.0);
    }
}
