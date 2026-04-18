#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::{egui, egui_wgpu, wgpu};
use std::sync::Arc;
use taffymeters_core::buffer;
use taffymeters_core::audio::AudioCapture;

mod app;
mod panel;
mod views;

use app::App;

fn load_icon() -> egui::IconData {
    let icon_data = include_bytes!("../assets/taffy.ico");
    let image = image::load_from_memory(icon_data)
        .expect("Failed to open icon path")
        .into_rgba8();
    let (width, height) = image.dimensions();
    egui::IconData { rgba: image.into_raw(), width, height }
}

fn low_power_wgpu_options() -> egui_wgpu::WgpuConfiguration {
    let mut options = egui_wgpu::WgpuConfiguration::default();

    if let egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_setup {
        setup.instance_descriptor.backends = wgpu::Backends::DX12;
        setup.power_preference = wgpu::PowerPreference::LowPower;
    }

    options.desired_maximum_frame_latency = Some(1);
    options
}

fn main() -> eframe::Result<()> {
    let (producer, consumer) = buffer::create_ring_buffer(16384, 2);
    let capture = AudioCapture::new(producer);

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: low_power_wgpu_options(),
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 400.0])
            .with_decorations(false)
            .with_always_on_top()
            .with_icon(Arc::new(load_icon())),
        ..Default::default()
    };

    eframe::run_native(
        "taffymeters",
        options,
        Box::new(|_cc| Ok(Box::new(App::new(consumer, capture)))),
    )
}
