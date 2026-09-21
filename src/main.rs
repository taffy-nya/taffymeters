#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::{egui, egui_wgpu, wgpu};

pub mod audio;
pub mod builtins;
pub mod dataflow;
pub mod module;
pub mod plotting;
pub mod render;
pub mod snapshot;

mod app;
mod live;

pub use live::{LiveSpectrum, LiveSpectrumConfig};

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
    let live = match LiveSpectrum::open_default(LiveSpectrumConfig::default()) {
        Ok(live) => live,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    let descriptor = live.reader().descriptor().clone();
    eprintln!(
        "system audio: {} Hz, {} channels, {:?}",
        descriptor.sample_rate_hz, descriptor.channel_count, descriptor.channel_layout
    );
    let app = match app::App::new(live) {
        Ok(app) => app,
        Err(error) => {
            eprintln!("module registration failed: {error}");
            std::process::exit(1);
        }
    };
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: low_power_wgpu_options(),
        viewport: egui::ViewportBuilder::default()
            .with_title("Taffymeters Phase 3")
            .with_inner_size([900.0, 420.0])
            .with_min_inner_size([480.0, 240.0])
            .with_always_on_top(),
        ..Default::default()
    };
    eframe::run_native(
        "taffymeters",
        options,
        Box::new(move |_creation_context| Ok(Box::new(app))),
    )
}
