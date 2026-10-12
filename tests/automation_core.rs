//! Runs the headless core automation suite (C/W/S scenarios) through the real
//! binary, so `cargo test --features automation` covers it. The suite needs no
//! audio device, native window or desktop input.

use std::{
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::Value;

/// A fresh output directory, removed when the test ends.
struct OutputDir(PathBuf);

impl OutputDir {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        Self(std::env::temp_dir().join(format!(
            "taffymeters-automation-core-{}-{nanos}",
            std::process::id()
        )))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for OutputDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn core_suite_passes_every_scenario() {
    let output = OutputDir::new();
    let run = Command::new(env!("CARGO_BIN_EXE_taffymeters"))
        .args(["--automation", "run", "--suite", "core", "--output"])
        .arg(output.path())
        .output()
        .expect("the taffymeters binary should start");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);

    let report_path = output.path().join("report.json");
    let report: Value =
        serde_json::from_slice(&std::fs::read(&report_path).unwrap_or_else(|error| {
            panic!(
                "no report at {} ({error}); exit {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
                report_path.display(),
                run.status.code()
            )
        }))
        .expect("report.json should be valid JSON");

    let cases = report["cases"].as_array().expect("report lists its cases");
    assert!(!cases.is_empty(), "the core suite ran no scenarios");
    let failures: Vec<String> = cases
        .iter()
        .filter(|case| case["status"] != "PASS")
        .map(|case| {
            format!(
                "{} {}: step {} error {}",
                case["id"], case["status"], case["failed_step"], case["error"]
            )
        })
        .collect();
    assert!(
        failures.is_empty(),
        "core scenarios did not pass:\n{}\nstderr:\n{stderr}",
        failures.join("\n")
    );
    assert_eq!(report["status"], "PASS");
    assert!(
        run.status.success(),
        "every case passed but the run exited with {:?}\nstderr:\n{stderr}",
        run.status.code()
    );
}
