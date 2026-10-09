#![cfg_attr(
    all(not(debug_assertions), not(feature = "automation")),
    windows_subsystem = "windows"
)]

use eframe::{egui_wgpu, wgpu};

pub mod audio;
pub mod builtins;
pub mod dataflow;
pub mod extensions;
pub mod module;
pub mod plotting;
pub mod render;
pub mod snapshot;

mod app;
#[cfg(feature = "automation")]
mod automation;
mod live;
mod workspace;

pub use live::{LiveAudio, LiveAudioConfig};

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
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args
        .first()
        .is_some_and(|argument| argument == "--automation")
    {
        #[cfg(feature = "automation")]
        std::process::exit(automation::run(&args[1..]));
        #[cfg(not(feature = "automation"))]
        {
            eprintln!("--automation requires a build with the automation feature");
            std::process::exit(2);
        }
    }
    if !args.is_empty() {
        eprintln!("unknown arguments: {}", args.join(" "));
        std::process::exit(2);
    }
    let live = match LiveAudio::open_default(LiveAudioConfig::default()) {
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
    let workspace = match workspace::Workspace::new(live, true) {
        Ok(workspace) => workspace,
        Err(error) => {
            eprintln!("module initialization failed: {error}");
            std::process::exit(1);
        }
    };
    run_window(app::App::new(workspace))
}

fn run_window(app: app::App) -> eframe::Result<()> {
    let viewport = app::App::host_builder();
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        wgpu_options: low_power_wgpu_options(),
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "taffymeters",
        options,
        Box::new(move |creation_context| {
            app.install_waker(&creation_context.egui_ctx);
            Ok(Box::new(app))
        }),
    )
}
