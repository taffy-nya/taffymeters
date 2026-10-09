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

pub(crate) struct Trace {
    out: BufWriter<File>,
    started: Instant,
    last_state: String,
    last_targets: HashMap<WindowId, String>,
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
        })
    }

    fn write(&mut self, kind: &str, body: serde_json::Value) {
        let line = json!({"t": self.started.elapsed().as_secs_f64(), "kind": kind, "body": body});
        let _ = writeln!(self.out, "{line}");
        let _ = self.out.flush();
    }

    /// Record ownership / demand when it changed.
    pub(crate) fn state(&mut self, workspace: &Workspace) {
        let observation = workspace.observation();
        let metrics = workspace.dataflow_handle_for_automation().metrics();
        let body = json!({
            "windows": observation.windows.iter().map(|w| json!({
                "id": w.id.0,
                "leaves": w.leaves.iter().map(|id| id.0).collect::<Vec<_>>(),
                "splits": w.splits.iter().map(|s| json!({"id": s.id.0, "axis": format!("{:?}", s.axis),
                    "ratio": (s.ratio * 10_000.0).round() / 10_000.0, "manual": s.manual})).collect::<Vec<_>>(),
                "active": w.active.map(|id| id.0),
                "always_on_top": w.always_on_top,
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
            "pipelines": metrics.pipelines,
            "control_error": observation.control_error,
            "source": format!("{:?}", observation.source_status.state),
        });
        let text = body.to_string();
        if text != self.last_state {
            self.last_state = text;
            self.write("state", body);
        }
    }

    /// Record the control rects of `window`'s last pass when they changed.
    pub(crate) fn targets(&mut self, workspace: &Workspace, window: WindowId) {
        let mut targets: Vec<(String, [f32; 4], bool)> = workspace
            .control_targets(window)
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
        let body = json!({"window": window.0, "targets": targets.iter().map(|(k, r, e)| json!([k, r, e])).collect::<Vec<_>>()});
        let text = body.to_string();
        if self.last_targets.get(&window) != Some(&text) {
            self.last_targets.insert(window, text);
            self.write("targets", body);
        }
    }
}
