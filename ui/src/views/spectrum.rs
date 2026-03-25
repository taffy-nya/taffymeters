use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};

pub fn draw(ui: &mut egui::Ui, fft_data: &[f32]) {
    let points: PlotPoints = fft_data
        .iter()
        .enumerate()
        .map(|(i, &val)| {
            // 对数缩放，视觉效果更好
            let y = (val * 10.0).log10().max(0.0);
            [i as f64, y as f64]
        })
        .collect();

    let line = Line::new("spectrum_line", points)
        .color(egui::Color32::LIGHT_BLUE)
        .fill(0.0)
        .width(2.0);

        Plot::new("spectrum_plot")
        .include_y(0.0)
        .include_y(5.0)
        .allow_drag(false)
        .allow_zoom(false)
        .show_axes([false, false])
        .show(ui, |plot_ui| {
            plot_ui.line(line);
        });
}