#![allow(clippy::missing_const_for_thread_local)] // The initializer is const; Clippy 1.98 flags the macro expansion.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use serde_json::Value;

use crate::{
    LiveAudio, LiveAudioConfig,
    audio::synthetic::{ManualInputDriver, SyntheticConfig},
    workspace::{
        ActionOutcome, FIRST_WINDOW, ModuleInstanceId, Side, WindowId, Workspace, WorkspaceAction,
    },
};

use super::{report::Case, visual};

pub(super) fn check(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}
pub(super) fn synthetic() -> Result<(LiveAudio, ManualInputDriver), String> {
    LiveAudio::open_manual(LiveAudioConfig::default(), SyntheticConfig::default())
        .map_err(|e| e.to_string())
}
/// A Workspace over the manual synthetic source: one window, one Spectrum.
pub(super) fn workspace() -> Result<(Workspace, ManualInputDriver), String> {
    let (live, driver) = synthetic()?;
    Ok((
        Workspace::new(live, false).map_err(|e| e.to_string())?,
        driver,
    ))
}
pub(super) fn created(outcome: ActionOutcome) -> Result<ModuleInstanceId, String> {
    match outcome {
        ActionOutcome::Created(id) => Ok(id),
        other => Err(format!("expected created instance, got {other:?}")),
    }
}
pub(super) fn action(ws: &mut Workspace, action: WorkspaceAction) -> Result<ActionOutcome, String> {
    ws.apply_action(action).map_err(|e| e.to_string())
}
pub(super) fn new_window(outcome: ActionOutcome) -> Result<WindowId, String> {
    match outcome {
        ActionOutcome::WindowCreated(id) => Ok(id),
        other => Err(format!("expected a new window, got {other:?}")),
    }
}
pub(super) fn leaves(ws: &Workspace, window: WindowId) -> Vec<ModuleInstanceId> {
    ws.observation()
        .window(window)
        .map(|w| w.leaves.clone())
        .unwrap_or_default()
}
/// Wait until the dataflow has exactly `expected` subscriptions.
pub(super) fn settle_subscriptions(ws: &Workspace, expected: u64) -> Result<(), String> {
    let handle = ws.dataflow_handle_for_automation();
    wait_until(deadline(), &format!("{expected} subscriptions"), || {
        (handle.metrics().subscriptions == expected).then_some(())
    })
}
/// The first leaf of the root window.
pub(super) fn first_leaf(ws: &Workspace) -> Result<ModuleInstanceId, String> {
    ws.observation()
        .window(FIRST_WINDOW)
        .and_then(|window| window.leaves.first().copied())
        .ok_or_else(|| "root window has no module".to_owned())
}
/// Replace the root window's first leaf with a default instance of `type_id`.
pub(super) fn replace_first(
    ws: &mut Workspace,
    type_id: &'static str,
) -> Result<ModuleInstanceId, String> {
    let leaf = first_leaf(ws)?;
    created(action(
        ws,
        WorkspaceAction::ReplaceModule {
            instance_id: leaf,
            type_id,
        },
    )?)
}
/// Split the widest root-window leaf that has room and turn the new leaf into
/// `type_id` (the user path: split, then pick the type).
pub(super) fn add_module(
    ws: &mut Workspace,
    type_id: &'static str,
) -> Result<ModuleInstanceId, String> {
    let observation = ws.observation();
    let window = observation
        .window(FIRST_WINDOW)
        .ok_or("root window closed")?;
    let mut leaves = window.leaf_rects.clone();
    leaves.sort_by(|a, b| b.1.area().total_cmp(&a.1.area()));
    let (target, side) = leaves
        .iter()
        .flat_map(|(leaf, _)| [(*leaf, Side::Right), (*leaf, Side::Below)])
        .find(|(leaf, side)| ws.can_split(*leaf, *side))
        .ok_or("no leaf has room for another module")?;
    let new = created(action(
        ws,
        WorkspaceAction::SplitModule {
            instance_id: target,
            side,
        },
    )?)?;
    if observation.module(target).map(|module| module.type_id) == Some(type_id) {
        return Ok(new);
    }
    created(action(
        ws,
        WorkspaceAction::ReplaceModule {
            instance_id: new,
            type_id,
        },
    )?)
}
pub(super) fn run_case(id: &str, f: impl FnOnce() -> Result<Value, String>) -> Case {
    let started = Instant::now();
    match with_scenario_deadline(f) {
        Ok(observed) if started.elapsed() <= Duration::from_secs(30) => Case::pass(id, observed),
        Ok(_) => Case::fail(id, "scenario deadline", "exceeded 30 seconds"),
        Err(error) => Case::fail(id, "behavior", error),
    }
}

pub(super) fn rgba_view(image: &image::RgbaImage) -> visual::Rgba8View<'_> {
    visual::Rgba8View {
        width: image.width(),
        height: image.height(),
        rgba8: image.as_raw(),
    }
}

pub(super) fn read_bytes(path: &Path) -> Result<Vec<u8>, String> {
    fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

pub(super) fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

pub(super) fn read_json(path: &Path) -> Result<Value, String> {
    serde_json::from_slice(&read_bytes(path)?).map_err(|e| format!("{}: {e}", path.display()))
}

/// A fresh directory, removed when the value drops.
pub(super) struct TempDir(PathBuf);

impl TempDir {
    pub(super) fn new(label: &str) -> Result<Self, String> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "taffymeters-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        // A directory left by a crashed run with the same process ID.
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self(path))
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

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
