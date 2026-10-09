#![allow(clippy::missing_const_for_thread_local)] // The initializer is const; Clippy 1.98 flags the macro expansion.

mod harness;
mod report;
mod scenarios;
mod trace;
mod workspace_scenarios;
thread_local! {
    static SCENARIO_DEADLINE: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
}

fn with_scenario_deadline<T>(run: impl FnOnce() -> T) -> T {
    SCENARIO_DEADLINE
        .with(|value| value.set(Some(std::time::Instant::now() + Duration::from_secs(30))));
    let result = run();
    SCENARIO_DEADLINE.with(|value| value.set(None));
    result
}
mod visual;

use serde_json::json;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{LiveAudio, LiveAudioConfig, audio::synthetic::SyntheticConfig, workspace::Workspace};
use report::new_output;
pub(crate) use trace::Trace;

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
            let flags: &[&str] = if tail.iter().any(|x| x == "--trace") {
                &["--input", "--trace"]
            } else {
                &["--input"]
            };
            let values = parse_run(tail, flags).map_err(usage)?;
            let trace = values
                .get(1)
                .map(|path| Trace::create(Path::new(path)))
                .transpose()
                .map_err(runtime)?;
            let live = match values[0].as_str() {
                "synthetic" => {
                    LiveAudio::open_realtime(LiveAudioConfig::default(), SyntheticConfig::default())
                }
                "sweep" => {
                    LiveAudio::open_realtime(LiveAudioConfig::default(), SyntheticConfig::sweep())
                }
                "system" => LiveAudio::open_default(LiveAudioConfig::default()),
                _ => return Err(usage("--input must be synthetic, sweep or system".into())),
            }
            .map_err(|e| runtime(e.to_string()))?;
            let workspace = Workspace::new(live, false).map_err(|e| runtime(e.to_string()))?;
            let mut app = crate::app::App::new(workspace);
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
            if suite != "core" && suite != "visual" {
                return Err(usage("--suite must be core or visual".into()));
            }
            if suite == "core" && values.len() > 2 {
                return Err(usage("core does not accept --baseline".into()));
            }
            if let Some(base) = values.get(2)
                && !Path::new(base).is_dir()
            {
                return Err(runtime(format!("baseline directory not found: {base}")));
            }
            let output = PathBuf::from(&values[1]);
            new_output(&output).map_err(runtime)?;
            let mut report = report::new(
                &suite,
                json!({"kind": "manual", "sample_rate_hz": 48000,
                "channels": 2, "layout": "stereo", "block_frames": 160,
                "signal": "left 3000Hz/right 6000Hz at 0.08 amplitude; absolute frame", "seed": null}),
            );
            let baseline = values.get(2).map(Path::new);
            if suite == "core" {
                scenarios::core(&mut report);
            } else {
                scenarios::visual(&mut report, &output, baseline);
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
            report.cases.push(scenarios::compare_case(
                Path::new(&values[0]),
                Path::new(&values[1]),
                &output,
            ));
            report.finish();
            let path = report.save(&output).map_err(runtime)?;
            println!("{:?}: {}", report.status, path.display());
            Ok(report.exit_code())
        }
        _ => Err(usage("expected run, compare or preview".into())),
    }
}

pub(super) fn wait_until<T>(
    deadline: std::time::Instant,
    label: &str,
    mut observe: impl FnMut() -> Option<T>,
) -> Result<T, String> {
    loop {
        if let Some(value) = observe() {
            return Ok(value);
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!("Timeout waiting for {label}"));
        }
        std::thread::yield_now();
    }
}

pub(super) fn deadline() -> std::time::Instant {
    let condition = std::time::Instant::now() + Duration::from_secs(5);
    SCENARIO_DEADLINE.with(|value| {
        value
            .get()
            .map_or(condition, |scenario| scenario.min(condition))
    })
}
