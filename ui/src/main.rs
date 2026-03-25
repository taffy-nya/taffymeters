use eframe::egui;
use minimeters_core::buffer;
use minimeters_core::audio::AudioEngine;

mod app;
mod views;
use app::App;

fn main() -> eframe::Result<()> {
    let (producer, consumer) = buffer::create_ring_buffer(8192);
    let audio_engine = AudioEngine::new(producer);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 400.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Minimeters",
        options,
        Box::new(|_cc| Ok(Box::new(App::new(consumer, audio_engine)))),
    )
}