use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};

pub fn draw(ui: &mut egui::Ui, audio_data: &[f32]) {
    let points = PlotPoints::from_ys_f32(audio_data);
    let line = Line::new("waveform_line", points)
        .color(egui::Color32::LIGHT_BLUE)
        .width(1.5);

    Plot::new("waveform_plot")
        .include_y(1.0)
        .include_y(-1.0)
        .allow_drag(false)
        .allow_zoom(false)
        .show_axes([false, false])
        .show(ui, |plot_ui| {
            plot_ui.line(line);
        });
}
