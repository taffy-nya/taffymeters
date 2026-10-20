//! `preview --trace <file>`: JSON lines describing Workspace ownership, analysis
//! demand and the real control rects of each window's last UI pass. A native
//! acceptance driver reads it to locate controls (logical points in the window's
//! client area) and to prove instance identity; it never feeds anything back.

use std::{
    collections::HashMap,
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    time::Instant,
};

use serde_json::json;

use crate::workspace::{WindowId, Workspace};

const SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

pub(crate) struct Trace {
    out: BufWriter<File>,
    started: Instant,
    last_state: String,
    last_targets: HashMap<WindowId, String>,
    /// Sampling times: state and control rects are captured at most every
    /// `SAMPLE_INTERVAL`, so tracing barely perturbs the CPU being measured.
    state_sampled: Option<Instant>,
    /// Settings window state of the last recorded `state`.
    state_settings_open: Option<bool>,
    targets_sampled: HashMap<WindowId, Instant>,
    /// UI passes per window since `rate_started`, plus analysis counters then.
    passes: HashMap<WindowId, u64>,
    rate_started: Option<(
        Instant,
        crate::dataflow::RuntimeMetrics,
        crate::probe::WorkerSample,
    )>,
}

impl Trace {
    pub(crate) fn create(path: &Path) -> Result<Self, String> {
        if path.exists() {
            return Err(format!("trace file already exists: {}", path.display()));
        }
        let file = File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self {
            out: BufWriter::new(file),
            started: Instant::now(),
            last_state: String::new(),
            last_targets: HashMap::new(),
            state_sampled: None,
            state_settings_open: None,
            targets_sampled: HashMap::new(),
            passes: HashMap::new(),
            rate_started: None,
        })
    }

    fn write(&mut self, kind: &str, body: serde_json::Value) {
        let line = json!({"t": self.started.elapsed().as_secs_f64(), "kind": kind, "body": body});
        let _ = writeln!(self.out, "{line}");
        let _ = self.out.flush();
    }

    /// Count one UI pass of `window`; about once a second write the passes per
    /// window, the analysis evaluations per second and the worker thread's CPU over
    /// that interval.
    pub(crate) fn pass(&mut self, workspace: &Workspace, window: WindowId) {
        *self.passes.entry(window).or_default() += 1;
        let handle = workspace.dataflow_handle_for_automation();
        let metrics = handle.metrics();
        let worker = handle.worker_sample();
        let now = Instant::now();
        let Some((started, previous, previous_worker)) = self.rate_started else {
            self.rate_started = Some((now, metrics, worker));
            self.passes.clear();
            return;
        };
        let elapsed = now.duration_since(started).as_secs_f64();
        if elapsed < 1.0 {
            return;
        }
        let mut passes: Vec<(u64, f64)> = self
            .passes
            .drain()
            .map(|(id, count)| (id.0, count as f64 / elapsed))
            .collect();
        passes.sort_by_key(|(id, _)| *id);
        let per_second = |now: u64, before: u64| (now - before) as f64 / elapsed;
        let body = json!({
            "seconds": elapsed,
            "passes_per_second": passes,
            "fft_evaluations_per_second": per_second(metrics.fft_evaluations, previous.fft_evaluations),
            "spectrum_evaluations_per_second":
                per_second(metrics.spectrum_evaluations, previous.spectrum_evaluations),
            "publications_per_second": per_second(metrics.publications, previous.publications),
            "worker_cpu_ms_per_second": worker.cpu.zip(previous_worker.cpu)
                .map(|(now, before)| (now - before).as_secs_f64() * 1_000.0 / elapsed),
            "worker_megacycles_per_second": worker.cycles.zip(previous_worker.cycles)
                .map(|(now, before)| (now - before) as f64 / 1.0e6 / elapsed),
            "worker_allocations_per_second": per_second(worker.allocations, previous_worker.allocations),
            "nodes": metrics.nodes,
            "active_channel_nodes": metrics.active_channel_nodes,
            "active_fft_nodes": metrics.active_fft_nodes,
            "active_spectrum_nodes": metrics.active_spectrum_nodes,
            "active_level_nodes": metrics.active_level_nodes,
        });
        self.write("rate", body);
        self.rate_started = Some((now, metrics, worker));
    }

    /// Record ownership / demand when it changed. Sampled, except that a settings
    /// window opening or closing is always recorded: it may be the last pass.
    pub(crate) fn state(&mut self, workspace: &Workspace) {
        let now = Instant::now();
        let settings_open = workspace.settings_open();
        if self.state_settings_open == Some(settings_open)
            && self
                .state_sampled
                .is_some_and(|at| now.duration_since(at) < SAMPLE_INTERVAL)
        {
            return;
        }
        self.state_sampled = Some(now);
        self.state_settings_open = Some(settings_open);
        let observation = workspace.observation();
        let metrics = workspace.dataflow_handle_for_automation().metrics();
        let body = json!({
            "windows": observation.windows.iter().map(|w| json!({
                "id": w.id.0,
                "leaves": w.leaves.iter().map(|id| id.0).collect::<Vec<_>>(),
                "splits": w.splits.iter().map(|s| json!({"id": s.id.0, "axis": format!("{:?}", s.axis),
                    "ratio": (s.ratio * 10_000.0).round() / 10_000.0, "manual": s.manual})).collect::<Vec<_>>(),
                "module_outline": w.module_outline.map(|(id, _)| id.0),
                "always_on_top": w.always_on_top,
                "locked": w.locked,
                "group": w.group,
                "pending_detach": w.pending_detach.map(|id| id.0),
                "min_size": [w.min_size.x, w.min_size.y],
                "divider_drag": w.divider_drag.map(|id| id.0),
            })).collect::<Vec<_>>(),
            "modules": observation.modules.iter().map(|m| json!({
                "id": m.instance_id.0, "type": m.type_id, "window": m.window_id.map(|w| w.0),
                "channel": m.channel.map(|c| format!("{c:?}")), "style": m.active_style,
                "object": format!("{:#x}", m.object),
            })).collect::<Vec<_>>(),
            "move_source": observation.move_source.map(|id| id.0),
            "modules_created": observation.modules_created,
            "subscriptions": metrics.subscriptions,
            "nodes": metrics.nodes,
            "control_error": observation.control_error,
            "source": format!("{:?}", observation.source_status.state),
            "preset": observation.preset,
            "theme": observation.theme.preset,
            "notices": observation.notices,
            "settings_open": settings_open,
        });
        let text = body.to_string();
        if text != self.last_state {
            self.last_state = text;
            self.write("state", body);
        }
    }

    /// Record the control rects of `window`'s last pass when they changed.
    pub(crate) fn targets(&mut self, workspace: &Workspace, window: WindowId) {
        let targets = workspace.control_targets(window);
        self.write_targets(window, json!(window.0), targets);
    }

    /// Record the settings window's control rects (`"window": "settings"`).
    pub(crate) fn settings_targets(&mut self, workspace: &Workspace) {
        let targets = workspace.settings_targets();
        self.write_targets(WindowId(u64::MAX), json!("settings"), targets);
    }

    fn write_targets(
        &mut self,
        key: WindowId,
        label: serde_json::Value,
        targets: std::collections::HashMap<
            crate::workspace::ControlKey,
            crate::workspace::ControlTarget,
        >,
    ) {
        let now = Instant::now();
        if self
            .targets_sampled
            .get(&key)
            .is_some_and(|at| now.duration_since(*at) < SAMPLE_INTERVAL)
        {
            return;
        }
        self.targets_sampled.insert(key, now);
        let mut targets: Vec<(String, [f32; 4], bool)> = targets
            .into_iter()
            .map(|(key, target)| {
                let r = target.rect;
                (
                    format!("{key:?}"),
                    [r.min.x, r.min.y, r.max.x, r.max.y].map(|v| (v * 10.0).round() / 10.0),
                    target.enabled,
                )
            })
            .collect();
        targets.sort_by(|a, b| a.0.cmp(&b.0));
        let body = json!({"window": label, "targets": targets.iter().map(|(k, r, e)| json!([k, r, e])).collect::<Vec<_>>()});
        let text = body.to_string();
        if self.last_targets.get(&key) != Some(&text) {
            self.last_targets.insert(key, text);
            self.write("targets", body);
        }
    }
}
