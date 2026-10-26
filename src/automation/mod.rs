mod cases;
mod harness;
mod perf;
mod report;
mod support;
mod trace;
mod visual;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use serde_json::json;

use crate::{
    LiveAudio, LiveAudioConfig,
    app::App,
    audio::inputs::synthetic::SyntheticConfig,
    persistence::WorkspaceStore,
    workspace::{NativeDesktop, Workspace},
};

use self::report::new_output;
pub(crate) use self::trace::Trace;

fn option_value(args: &[String], flag: &str) -> Result<Option<String>, String> {
    let positions: Vec<_> = args
        .iter()
        .enumerate()
        .filter(|(_, x)| x.as_str() == flag)
        .map(|(i, _)| i)
        .collect();
    if positions.len() > 1 {
        return Err(format!("duplicate {flag}"));
    }
    positions
        .first()
        .map(|i| {
            args.get(i + 1)
                .filter(|s| !s.starts_with("--"))
                .cloned()
                .ok_or_else(|| format!("missing {flag} value"))
        })
        .transpose()
}

fn parse_run(args: &[String], required: &[&str]) -> Result<Vec<String>, String> {
    if args.len() != required.len() * 2 {
        return Err("missing or unexpected arguments".into());
    }
    required
        .iter()
        .map(|flag| option_value(args, flag)?.ok_or_else(|| format!("missing {flag}")))
        .collect::<Result<Vec<_>, _>>()
}

pub(super) fn run(args: &[String]) -> i32 {
    match dispatch(args) {
        Ok(code) => code,
        Err((code, error)) => {
            eprintln!("automation: {error}");
            code
        }
    }
}

fn dispatch(args: &[String]) -> Result<i32, (i32, String)> {
    let usage = |error: String| (2, error);
    let runtime = |error: String| (1, error);
    match args.first().map(String::as_str) {
        Some("preview") => {
            let tail = &args[1..];
            let mut flags = vec!["--input"];
            for optional in ["--trace", "--config-dir"] {
                if tail.iter().any(|x| x == optional) {
                    flags.push(optional);
                }
            }
            let values = parse_run(tail, &flags).map_err(usage)?;
            let value_of = |flag: &str| {
                flags
                    .iter()
                    .position(|candidate| *candidate == flag)
                    .map(|index| values[index].clone())
            };
            let trace = value_of("--trace")
                .map(|path| Trace::create(Path::new(&path)))
                .transpose()
                .map_err(runtime)?;
            let store = value_of("--config-dir").map(WorkspaceStore::new);
            let local = store.as_ref().map(WorkspaceStore::load_local);
            let mut unavailable = None;
            let live = match values[0].as_str() {
                "synthetic" => {
                    LiveAudio::open_realtime(LiveAudioConfig::default(), SyntheticConfig::default())
                }
                "sweep" => {
                    LiveAudio::open_realtime(LiveAudioConfig::default(), SyntheticConfig::sweep())
                }
                // Like the product: the local settings choose the system input.
                "system" => {
                    let chosen = local
                        .as_ref()
                        .map(|local| local.settings.input.clone())
                        .unwrap_or_default();
                    crate::live::open_chosen(&chosen, &crate::live::open_system).map(
                        |(live, failure)| {
                            unavailable = failure;
                            live
                        },
                    )
                }
                _ => return Err(usage("--input must be synthetic, sweep or system".into())),
            }
            .map_err(|e| runtime(e.to_string()))?;
            let mut workspace = match store.zip(local) {
                Some((store, local)) => {
                    Workspace::open(live, store, local, Arc::new(NativeDesktop))
                }
                None => Workspace::new(live, false),
            }
            .map_err(|e| runtime(e.to_string()))?;
            if let Some(error) = unavailable {
                workspace.input_unavailable(&error);
            }
            if values[0] == "system" {
                workspace.watch_system_endpoints();
            }
            let mut app = App::new(workspace);
            if let Some(trace) = trace {
                app = app.with_trace(trace);
            }
            crate::run_window(app).map_err(|e| runtime(e.to_string()))?;
            Ok(0)
        }
        Some("run") => {
            let tail = &args[1..];
            let suite = option_value(tail, "--suite")
                .map_err(usage)?
                .ok_or_else(|| usage("missing --suite".into()))?;
            let flags: &[&str] = if tail.iter().any(|x| x == "--baseline") {
                &["--suite", "--output", "--baseline"]
            } else {
                &["--suite", "--output"]
            };
            let values = parse_run(tail, flags).map_err(usage)?;
            if !["core", "visual", "dataflow-perf"].contains(&suite.as_str()) {
                return Err(usage(
                    "--suite must be core, visual or dataflow-perf".into(),
                ));
            }
            if suite != "visual" && values.len() > 2 {
                return Err(usage(format!("{suite} does not accept --baseline")));
            }
            if let Some(base) = values.get(2)
                && !Path::new(base).is_dir()
            {
                return Err(runtime(format!("baseline directory not found: {base}")));
            }
            let output = PathBuf::from(&values[1]);
            new_output(&output).map_err(runtime)?;
            let input = if suite == "dataflow-perf" {
                json!({"kind": "manual", "sample_rate_hz": 48000,
                "channels": 2, "layout": "stereo", "block_frames": 480,
                "signal": "preview sweep (left 80 Hz-12 kHz / right pulsed 440 Hz); absolute frame", "seed": null})
            } else {
                json!({"kind": "manual", "sample_rate_hz": 48000,
                "channels": 2, "layout": "stereo", "block_frames": 160,
                "signal": "left 3000Hz/right 6000Hz at 0.08 amplitude; absolute frame", "seed": null})
            };
            let mut report = report::new(&suite, input);
            let baseline = values.get(2).map(Path::new);
            match suite.as_str() {
                "core" => cases::core(&mut report),
                "visual" => cases::visual(&mut report, &output, baseline),
                _ => perf::suite(&mut report),
            }
            report.finish();
            let path = report.save(&output).map_err(runtime)?;
            for case in &report.cases {
                println!("{}: {:?}", case.id, case.status);
            }
            println!("{:?}: {}", report.status, path.display());
            Ok(report.exit_code())
        }
        Some("compare") => {
            let values =
                parse_run(&args[1..], &["--expected", "--actual", "--output"]).map_err(usage)?;
            let output = PathBuf::from(&values[2]);
            new_output(&output).map_err(runtime)?;
            let mut report = report::new("compare", serde_json::Value::Null);
            report.cases.push(cases::compare_case(
                Path::new(&values[0]),
                Path::new(&values[1]),
                &output,
            ));
            report.finish();
            let path = report.save(&output).map_err(runtime)?;
            println!("{:?}: {}", report.status, path.display());
            Ok(report.exit_code())
        }
        Some("crash") => {
            let values = parse_run(&args[1..], &["--config-dir"]).map_err(usage)?;
            crate::crash::install(PathBuf::from(&values[0]));
            panic!("automation crash entry");
        }
        _ => Err(usage("expected run, compare, preview or crash".into())),
    }
}
