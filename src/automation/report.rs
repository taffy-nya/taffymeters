use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(super) enum Status {
    Pass,
    Fail,
    Unavailable,
    NeedsReview,
    NotComparable,
    NotRun,
}

#[derive(Serialize)]
pub(super) struct Case {
    pub id: String,
    pub status: Status,
    pub failed_step: Option<String>,
    pub expected: Value,
    pub observed: Value,
    pub error: Option<String>,
    pub artifacts: Vec<String>,
}

impl Case {
    pub fn pass(id: &str, observed: Value) -> Self {
        Self {
            id: id.into(),
            status: Status::Pass,
            failed_step: None,
            expected: Value::Null,
            observed,
            error: None,
            artifacts: vec![],
        }
    }
    pub fn fail(id: &str, step: &str, error: impl ToString) -> Self {
        Self {
            id: id.into(),
            status: Status::Fail,
            failed_step: Some(step.into()),
            expected: Value::Null,
            observed: Value::Null,
            error: Some(error.to_string()),
            artifacts: vec![],
        }
    }
    pub fn incomplete(id: &str, status: Status, reason: impl ToString) -> Self {
        Self {
            id: id.into(),
            status,
            failed_step: None,
            expected: Value::Null,
            observed: Value::Null,
            error: Some(reason.to_string()),
            artifacts: vec![],
        }
    }
}

#[derive(Serialize)]
pub(super) struct Report {
    pub schema_version: u32,
    pub suite: String,
    pub status: Status,
    pub build: Value,
    pub environment: Value,
    pub input: Value,
    pub cases: Vec<Case>,
    pub uncovered: Vec<String>,
}

pub(super) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn file_digest(path: &Path) -> Value {
    fs::read(path).map_or_else(|_| json!("unknown"), |bytes| json!(digest(&bytes)))
}

/// The lock file this binary was compiled against, not the workspace's current one.
const BUILD_CARGO_LOCK: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.lock"));

fn compiled_target() -> String {
    let env = if cfg!(target_env = "gnu") {
        "gnu"
    } else if cfg!(target_env = "msvc") {
        "msvc"
    } else {
        "unknown"
    };
    let vendor = if cfg!(target_os = "windows") {
        "pc"
    } else {
        "unknown"
    };
    format!(
        "{}-{vendor}-{}-{env}",
        std::env::consts::ARCH,
        std::env::consts::OS
    )
}

pub(super) fn new(suite: &str, input: Value) -> Report {
    let executable = std::env::current_exe().ok();
    let executable_sha256 = executable
        .as_deref()
        .map_or_else(|| json!("unknown"), file_digest);
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    Report {
        schema_version: 1, suite: suite.into(), status: Status::NotRun,
        build: json!({"package_version": env!("CARGO_PKG_VERSION"), "profile": profile,
            "automation_enabled": true, "target": compiled_target(),
            "executable": executable.map_or_else(|| json!("unknown"), |path| json!(path)),
            "executable_sha256": executable_sha256,
            "cargo_lock_sha256": digest(BUILD_CARGO_LOCK)}),
        environment: json!({"os": std::env::consts::OS, "renderer": null}),
        input, cases: vec![],
        uncovered: vec!["native window, system audio/device, cross-monitor DPI and performance require separate verification".into()],
    }
}

impl Report {
    pub fn finish(&mut self) {
        self.status = if self.cases.iter().any(|c| c.status == Status::Fail) {
            Status::Fail
        } else if self.cases.iter().any(|c| c.status == Status::Unavailable) {
            Status::Unavailable
        } else if self.cases.iter().any(|c| c.status == Status::NotComparable) {
            Status::NotComparable
        } else if self
            .cases
            .iter()
            .any(|c| c.status == Status::NeedsReview || c.status == Status::NotRun)
        {
            Status::NeedsReview
        } else {
            Status::Pass
        };
    }
    pub fn exit_code(&self) -> i32 {
        match self.status {
            Status::Pass => 0,
            Status::Fail => 1,
            _ => 3,
        }
    }
    pub fn save(&self, output: &Path) -> Result<PathBuf, String> {
        let path = output.join("report.json");
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        fs::write(&path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(path)
    }
}

pub(super) fn new_output(path: &Path) -> Result<(), String> {
    if path.exists() {
        if !path.is_dir()
            || fs::read_dir(path)
                .map_err(|e| e.to_string())?
                .next()
                .is_some()
        {
            return Err(format!(
                "output must be a new or empty directory: {}",
                path.display()
            ));
        }
    } else {
        fs::create_dir_all(path).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}
