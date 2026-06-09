use eframe::egui;
use taffymeters_core::dsp::LogSpectrumMapper;
use taffymeters_core::frame::AudioFrame;
use crate::theme::Theme;
use super::components::ScaleControl;
use super::traits::View;

pub struct SpectrumView {
    y_scale: ScaleControl,
    mapper: LogSpectrumMapper,
    bands: Vec<f32>,
    theme: &'static Theme,
}

impl SpectrumView {
    pub fn new() -> Self {
        Self {
            y_scale: ScaleControl::new(1.0, 0.2, 10.0),
            mapper: LogSpectrumMapper::new(300),
            bands: Vec::new(),
            theme: crate::theme::dark(),
        }
    }
}

impl View for SpectrumView {
    fn handle_input(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        if ui.rect_contains_pointer(rect) {
            self.y_scale.handle_scroll(ui);
        }
    }

    fn draw(&mut self, ui: &mut egui::Ui, frame: &AudioFrame, rect: egui::Rect) {
        if rect.width() <= 1.0 { return; }

        self.mapper.map_into(&frame.fft, frame.sample_rate, &mut self.bands);
        for val in &mut self.bands {
            *val = LogSpectrumMapper::to_db(*val);
        }

        if self.bands.len() < 2 { return; }

        let (response, painter) = ui.allocate_painter(rect.size(), egui::Sense::hover());
        let r = response.rect;

        let y_max = 5.0_f32;
        let last = (self.bands.len() - 1) as f32;

        let points: Vec<egui::Pos2> = self.bands.iter().enumerate().map(|(i, &val)| {
            let t = i as f32 / last;
            let x = egui::lerp(r.left()..=r.right(), t);
            let y_norm = (val / y_max).clamp(0.0, 1.0);
            let scaled = (y_norm * self.y_scale.value).clamp(0.0, 1.0);
            let y = egui::lerp(r.bottom()..=r.top(), scaled);
            egui::pos2(x, y)
        }).collect();

        painter.add(egui::Shape::line(points, egui::Stroke::new(1.5, self.theme.line)));
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Y Scale");
        ui.add(egui::Slider::new(&mut self.y_scale.value, 0.2..=10.0).logarithmic(true));
        ui.separator();
        ui.label("Band Count");
        let mut bands = self.mapper.bands;
        if ui.add(egui::Slider::new(&mut bands, 50..=600)).changed() {
            self.mapper = LogSpectrumMapper::new(bands);
            self.bands.clear();
        }
    }
}
