#![cfg_attr(
    all(not(debug_assertions), not(feature = "automation")),
    windows_subsystem = "windows"
)]

use std::{path::PathBuf, sync::Arc};

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
#[cfg(windows)]
mod instance;
mod live;
#[cfg(feature = "perf-trace")]
mod perf;
mod persistence;
#[cfg(any(test, feature = "automation", feature = "perf-trace"))]
mod probe;
mod workspace;

pub use live::{LiveAudio, LiveAudioConfig};

fn main() {
    #[cfg_attr(not(feature = "perf-trace"), allow(unused_mut))]
    let mut args: Vec<String> = std::env::args().skip(1).collect();
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
    #[cfg(feature = "perf-trace")]
    let perf = match perf::take_arg(&mut args) {
        Ok(dir) => dir.map(|dir| {
            perf::enable();
            perf::PerfTrace::new(dir)
        }),
        Err(error) => exit_with_error(&error),
    };
    let root = match parse_config_dir(&args) {
        Ok(Some(root)) => root,
        Ok(None) => persistence::WorkspaceStore::default_root()
            .unwrap_or_else(|error| exit_with_error(&error)),
        Err(error) => exit_with_error(&error),
    };
    #[cfg(windows)]
    let _instance = match instance::claim(&root) {
        Ok(instance::Claim::First(instance)) => Some(instance),
        Ok(instance::Claim::Running { pid }) => {
            eprintln!("already running with {}", root.display());
            instance::raise(pid);
            std::process::exit(0);
        }
        Err(error) => {
            eprintln!("couldn't check for another instance: {error}");
            None
        }
    };
    crash::install(root.clone());
    let store = persistence::WorkspaceStore::new(root);
    let local = store.load_local();
    let (live, unavailable) = live::open_chosen(&local.settings.input, &live::open_system)
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
    let mut workspace = workspace::Workspace::open(live, store, local, desktop)
        .unwrap_or_else(|error| exit_with_error(&format!("couldn't open the workspace: {error}")));
    if let Some(error) = unavailable {
        workspace.input_unavailable(&error);
    }
    workspace.watch_system_endpoints();
    #[cfg_attr(not(feature = "perf-trace"), allow(unused_mut))]
    let mut app = app::App::new(workspace);
    #[cfg(feature = "perf-trace")]
    if let Some(perf) = perf {
        app = app.with_perf(perf);
    }
    if let Err(error) = run_window(app) {
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

fn run_window(app: app::App) -> Result<(), String> {
    app::run(app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dir_accepts_only_an_optional_flag_and_its_value() {
        let args = |values: &[&str]| {
            values
                .iter()
                .map(|value| (*value).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(parse_config_dir(&[]), Ok(None));
        assert_eq!(
            parse_config_dir(&args(&["--config-dir", "X"])),
            Ok(Some(PathBuf::from("X")))
        );
        for values in [
            vec!["--config-dir"],
            vec!["--config-dir", "--other"],
            vec!["--unknown"],
        ] {
            let error = parse_config_dir(&args(&values)).expect_err("invalid arguments must fail");
            assert!(
                error.contains("unknown arguments") && error.contains("--config-dir <dir>"),
                "{error}"
            );
        }
    }
}
