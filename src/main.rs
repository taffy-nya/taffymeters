#![cfg_attr(
    all(not(debug_assertions), not(feature = "automation")),
    windows_subsystem = "windows"
)]

use std::{path::PathBuf, sync::Arc};

use eframe::{egui_wgpu, wgpu};
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MessageBoxW,
};

pub mod audio;
pub mod dataflow;
pub mod extensions;
pub mod module;
pub mod modules;
pub mod plotting;
pub mod render;
pub mod settings;
pub mod snapshot;
pub mod theme;

mod app;
#[cfg(feature = "automation")]
mod automation;
mod crash;
#[cfg(windows)]
mod fonts;
mod icon;
mod live;
mod persistence;
#[cfg(any(test, feature = "automation"))]
mod probe;
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

fn main() {
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
            std::process::exit(1);
        }
    }
    let root = match parse_config_dir(&args) {
        Ok(Some(root)) => root,
        Ok(None) => persistence::WorkspaceStore::default_root()
            .unwrap_or_else(|error| exit_with_error(&error)),
        Err(error) => exit_with_error(&error),
    };
    crash::install(root.clone());
    let store = persistence::WorkspaceStore::new(root);
    let (live, unavailable) = live::open_chosen(&store.session_input())
        .unwrap_or_else(|error| exit_with_error(&error.to_string()));
    let descriptor = Arc::clone(live.reader().descriptor());
    eprintln!(
        "{}: {} Hz, {} channels, {:?}",
        live.name(),
        descriptor.sample_rate_hz,
        descriptor.channel_count,
        descriptor.channel_layout
    );
    let desktop = Arc::new(workspace::NativeDesktop);
    let mut workspace = workspace::Workspace::open(live, store, desktop)
        .unwrap_or_else(|error| exit_with_error(&format!("couldn't open the workspace: {error}")));
    if let Some(error) = unavailable {
        workspace.input_unavailable(&error);
    }
    if let Err(error) = run_window(app::App::new(workspace)) {
        exit_with_error(&format!("couldn't open the window: {error}"));
    }
}

fn exit_with_error(message: &str) -> ! {
    eprintln!("{message}");
    #[cfg(windows)]
    {
        let wide = |text: &str| text.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let text = wide(&format!("Taffymeters couldn't start.\n\n{message}"));
        let caption = wide("Taffymeters");
        // SAFETY: both strings are NUL-terminated and outlive the call; no owner window.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                caption.as_ptr(),
                MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
            )
        };
    }
    std::process::exit(1);
}

fn parse_config_dir(args: &[String]) -> Result<Option<PathBuf>, String> {
    match args {
        [] => Ok(None),
        [flag, dir] if flag == "--config-dir" && !dir.starts_with("--") => {
            Ok(Some(PathBuf::from(dir)))
        }
        _ => Err(format!(
            "unknown arguments: {} (expected [--config-dir <dir>])",
            args.join(" ")
        )),
    }
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
            #[cfg(windows)]
            fonts::install_fallbacks(&creation_context.egui_ctx);
            #[cfg(windows)]
            app.save_on_session_end();
            Ok(Box::new(app))
        }),
    )
}
