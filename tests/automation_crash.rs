use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

struct ConfigDir(PathBuf);

impl ConfigDir {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let dir = Self(std::env::temp_dir().join(format!(
            "taffymeters-automation-crash-{}-{nanos}",
            std::process::id()
        )));
        fs::create_dir_all(dir.path()).expect("the temporary config directory should be writable");
        dir
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn reports(&self) -> BTreeSet<PathBuf> {
        fs::read_dir(self.path())
            .expect("the temporary config directory should be readable")
            .map(|entry| {
                entry
                    .expect("the directory entry should be readable")
                    .path()
            })
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("crash-") && name.ends_with(".log"))
            })
            .collect()
    }
}

impl Drop for ConfigDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.path());
    }
}

#[test]
fn crash_entry_reports_panics_and_keeps_five_without_touching_other_files() {
    let dir = ConfigDir::new();
    let untouched = [
        ("session.json", b"session sentinel".as_slice()),
        ("notes.log", b"notes sentinel".as_slice()),
        ("crash-other.txt", b"not a report".as_slice()),
        ("presets/kept.json", b"preset sentinel".as_slice()),
    ];
    fs::create_dir(dir.path().join("presets")).unwrap();
    for (name, bytes) in untouched {
        fs::write(dir.path().join(name), bytes).unwrap();
    }
    let mut written = BTreeSet::new();
    for run_index in 0..6 {
        let child = Command::new(env!("CARGO_BIN_EXE_taffymeters"))
            .args(["--automation", "crash", "--config-dir"])
            .arg(dir.path())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("the taffymeters crash entry should start");
        let pid = child.id();
        let run = child
            .wait_with_output()
            .expect("the crash entry should terminate");
        assert_eq!(
            run.status.code(),
            Some(101),
            "crash run {run_index}: stderr {}",
            String::from_utf8_lossy(&run.stderr)
        );
        let reports = dir.reports();
        let suffix = format!("-{pid}.log");
        let report = reports
            .iter()
            .find(|path| path.to_string_lossy().ends_with(&suffix))
            .unwrap_or_else(|| {
                panic!(
                    "crash run {run_index} produced no crash-*.log for process {pid}: {reports:?}"
                )
            });
        let text = fs::read_to_string(report).expect("the crash report should be UTF-8");
        assert!(
            text.contains(&format!(
                "taffymeters {} panicked",
                env!("CARGO_PKG_VERSION")
            )),
            "report {} has no product version: {text}",
            report.display()
        );
        assert!(
            text.contains("message: automation crash entry"),
            "report {} has no panic message: {text}",
            report.display()
        );
        written.insert(report.clone());
        let expected: BTreeSet<_> = written.iter().rev().take(5).cloned().collect();
        assert_eq!(
            reports, expected,
            "crash report retention did not keep the newest five on run {run_index}"
        );
        for (name, bytes) in untouched {
            assert_eq!(
                fs::read(dir.path().join(name)).unwrap(),
                bytes,
                "crash entry changed unrelated file {name}"
            );
        }
    }
    assert_eq!(written.len(), 6, "crash runs overwrote a report");
    assert_eq!(
        dir.reports().len(),
        5,
        "six crashes must keep exactly five reports"
    );
}
