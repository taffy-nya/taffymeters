//! S01–S07: settings, copies, explicit sharing, theme, saving and restoring,
//! missing extensions, workspace switching and window rescue. They run the real
//! Workspace, Module factories and manual source with temporary configuration
//! directories; panel and menu operations use real egui events.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use eframe::egui::{self, Event, ViewportCommand, pos2, vec2};
use serde_json::{Value, json};

use crate::{
    LiveAudio, LiveAudioConfig,
    audio::synthetic::{ManualInputDriver, SyntheticConfig},
    builtins::{
        BAR_SPECTRUM_STYLE_ID, LINE_OSCILLOSCOPE_STYLE_ID, OSCILLOSCOPE_MODULE_ID,
        SPECTRUM_MODULE_ID, WAVEFORM_MODULE_ID,
    },
    dataflow::{ChannelSelection, DataSpec, FftConfig, NodeRef, NodeSpec, OutputId},
    persistence::WorkspaceStore,
    settings::{DataScope, DataSettings, NodeParam, SettingValue},
    theme::{ColorRole, Preset},
    workspace::{
        ActionOutcome, ColorScope, ControlKey, FIRST_WINDOW, MenuItem, ModuleInstanceId,
        PanelControl, SWATCHES, Side, WindowId, Workspace, WorkspaceAction,
    },
};

use super::{
    deadline,
    harness::Harness,
    scenarios::{action, check, created, first_leaf, workspace},
    wait_until,
};

type Scenario = fn() -> Result<Value, String>;

pub(super) const CASES: [(&str, Scenario); 7] = [
    ("S01", s01),
    ("S02", s02),
    ("S03", s03),
    ("S04", s04),
    ("S05", s05),
    ("S06", s06),
    ("S07", s07),
];

/// A configuration directory removed when the scenario ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new(case: &str) -> Result<Self, String> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |time| time.as_nanos());
        let path =
            std::env::temp_dir().join(format!("taffymeters-{case}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self(path))
    }

    fn store(&self) -> WorkspaceStore {
        WorkspaceStore::new(&self.0)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join("workspaces").join(format!("{name}.json"))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn manual_live() -> Result<(LiveAudio, ManualInputDriver), String> {
    LiveAudio::open_manual(LiveAudioConfig::default(), SyntheticConfig::default())
        .map_err(|e| e.to_string())
}

fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

fn spec(ws: &Workspace, id: ModuleInstanceId) -> Result<DataSpec, String> {
    ws.observation()
        .module(id)
        .and_then(|module| module.data.clone())
        .ok_or_else(|| format!("instance {} has no analysis", id.0))
}

fn node(spec: &DataSpec, kind: fn(&NodeSpec) -> bool) -> Result<NodeRef, String> {
    spec.nodes()
        .find(|(_, _, node)| kind(node))
        .map(|(node, _, _)| node)
        .ok_or_else(|| "node kind missing".to_owned())
}

fn channel_of(spec: &DataSpec) -> Option<ChannelSelection> {
    spec.nodes().find_map(|(_, _, node)| match node {
        NodeSpec::Channel(channel) => Some(channel),
        _ => None,
    })
}

/// The Spectrum's peak curve output.
fn peak() -> OutputId {
    OutputId::from_index(1)
}

fn new_window(outcome: ActionOutcome) -> Result<WindowId, String> {
    match outcome {
        ActionOutcome::WindowCreated(id) => Ok(id),
        other => Err(format!("expected a new window, got {other:?}")),
    }
}

fn leaves(ws: &Workspace, window: WindowId) -> Vec<ModuleInstanceId> {
    ws.observation()
        .window(window)
        .map(|w| w.leaves.clone())
        .unwrap_or_default()
}

fn settle(ws: &Workspace, expected: u64) -> Result<(), String> {
    let handle = ws.dataflow_handle_for_automation();
    wait_until(deadline(), &format!("{expected} subscriptions"), || {
        (handle.metrics().subscriptions == expected).then_some(())
    })
}

/// Run frames until a newly shown egui window passed its invisible sizing pass.
fn settle_frames(h: &mut Harness) {
    h.frame(vec![]);
    h.frame(vec![]);
}

fn open_settings(h: &mut Harness, leaf: ModuleInstanceId) -> Result<(), String> {
    // Instances created by direct actions get their controls in the next pass.
    h.frame(vec![]);
    h.menu(ControlKey::LeafBody(leaf), &[MenuItem::Settings])?;
    settle_frames(h);
    Ok(())
}

fn panel(h: &mut Harness, control: PanelControl) -> Result<(), String> {
    h.click(ControlKey::Panel(control))?;
    h.frame(vec![]);
    Ok(())
}

fn tall(h: &mut Harness) {
    h.set_size(vec2(900.0, 1600.0));
    h.frame(vec![]);
}

/// Copy, settings shown once per node, and edits that only reach the target.
fn s01() -> Result<Value, String> {
    let (ws, _driver) = workspace()?;
    let handle = ws.dataflow_handle_for_automation();
    let mut h = Harness::new(ws);
    tall(&mut h);
    let a = first_leaf(&h.ws)?;
    let a_object = h.ws.observation().module(a).ok_or("a missing")?.object;
    h.menu(
        ControlKey::LeafBody(a),
        &[MenuItem::LayoutOpen, MenuItem::Duplicate],
    )?;
    let window = leaves(&h.ws, FIRST_WINDOW);
    check(
        window.len() == 2 && window[0] == a,
        "Duplicate did not add a copy beside the original",
    )?;
    let b = window[1];
    settle(&h.ws, 2)?;
    let (spec_a, spec_b) = (spec(&h.ws, a)?, spec(&h.ws, b)?);
    let o = h.ws.observation();
    check(
        spec_a == spec_b
            && o.module(b).map(|m| m.type_id) == Some(SPECTRUM_MODULE_ID)
            && o.module(a).map(|m| m.object) == Some(a_object)
            && o.module(b).map(|m| m.object) != Some(a_object),
        "the copy is not a new Spectrum with the same DataSpec",
    )?;
    let copied = handle.metrics();
    check(
        copied.active_fft_nodes == 1 && copied.active_spectrum_nodes == 5,
        &format!("copy did not share the pure chain and keep private Peaks: {copied:?}"),
    )?;

    open_settings(&mut h, b)?;
    let channel = node(&spec_b, |n| matches!(n, NodeSpec::Channel(_)))?;
    let fft = node(&spec_b, |n| matches!(n, NodeSpec::Fft(_)))?;
    let tilt = node(&spec_b, |n| matches!(n, NodeSpec::Tilt { .. }))?;
    let targets = h.ws.control_targets(FIRST_WINDOW);
    let count = |matches: fn(&PanelControl) -> bool| {
        targets
            .keys()
            .filter(|key| matches!(key, ControlKey::Panel(control) if matches(control)))
            .count()
    };
    let channel_controls = count(|c| matches!(c, PanelControl::Channel { .. }));
    let fft_controls = count(|c| matches!(c, PanelControl::FftWindow { .. }));
    check(
        channel_controls == 3 && fft_controls == 6,
        &format!(
            "Channel/FFT shared by both outputs must appear once: {channel_controls} channel, {fft_controls} FFT controls"
        ),
    )?;
    panel(
        &mut h,
        PanelControl::Channel {
            node: channel,
            channel: ChannelSelection::Right,
        },
    )?;
    panel(
        &mut h,
        PanelControl::FftWindow {
            node: fft,
            size: 4096,
        },
    )?;
    let key = ControlKey::Panel(PanelControl::Number {
        node: tilt,
        param: NodeParam::Tilt,
    });
    let slider = h.locate(&key)?.rect;
    h.drag(
        key,
        &[pos2(slider.max.x - 2.0, slider.center().y)],
        egui::Modifiers::NONE,
        None,
    )?;
    let (spec_a, spec_b) = (spec(&h.ws, a)?, spec(&h.ws, b)?);
    let tilt_of = |spec: &DataSpec| match spec.node(tilt) {
        Some(NodeSpec::Tilt { db_per_octave }) => db_per_octave,
        _ => f32::NAN,
    };
    let fft_of = |spec: &DataSpec| match spec.node(fft) {
        Some(NodeSpec::Fft(config)) => Some(config),
        _ => None,
    };
    check(
        channel_of(&spec_a) == Some(ChannelSelection::MonoAverage)
            && channel_of(&spec_b) == Some(ChannelSelection::Right),
        "changing the copy's channel touched the original",
    )?;
    check(
        fft_of(&spec_a) == Some(FftConfig::default())
            && fft_of(&spec_b).is_some_and(|f| f.window_size == 4096 && f.hop_size == 1024),
        "FFT size did not change for the copy only (hop keeps its 1/4)",
    )?;
    check(
        tilt_of(&spec_a) == 0.0 && tilt_of(&spec_b) > 6.0,
        &format!(
            "Tilt slider: original {}, copy {}",
            tilt_of(&spec_a),
            tilt_of(&spec_b)
        ),
    )?;
    let edited = handle.metrics();
    check(
        edited.active_fft_nodes == 2,
        &format!("the copy's FFT did not fork: {edited:?}"),
    )?;
    h.ws.check_ownership()?;
    Ok(
        json!({"channel_controls":channel_controls,"fft_controls":fft_controls,
        "copy_tilt_db_per_octave":tilt_of(&spec_b),"fft_nodes":edited.active_fft_nodes}),
    )
}

/// An explicit binding: copies keep it, shared edits reach every member (also by
/// shortcut), unlinking forks only the instance that leaves.
fn s02() -> Result<Value, String> {
    let (ws, _driver) = workspace()?;
    let handle = ws.dataflow_handle_for_automation();
    let mut h = Harness::new(ws);
    tall(&mut h);
    let a = first_leaf(&h.ws)?;
    open_settings(&mut h, a)?;
    panel(&mut h, PanelControl::Share(peak()))?;
    let binding = spec(&h.ws, a)?.outputs()[1].binding;
    check(binding.is_some(), "Share did not bind the Peak output")?;
    let b = created(action(
        &mut h.ws,
        WorkspaceAction::DuplicateModule { instance_id: a },
    )?)?;
    settle(&h.ws, 2)?;
    let spec_b = spec(&h.ws, b)?;
    let shared = handle.metrics();
    check(
        spec_b.outputs()[1].binding == binding && shared.active_spectrum_nodes == 4,
        &format!("the copy did not join the binding with one shared Peak: {shared:?}"),
    )?;
    let described = DataSettings::describe(&spec_b);
    let scopes: Vec<(String, bool)> = described
        .nodes
        .iter()
        .map(|entry| {
            (
                crate::settings::node_label(&entry.spec).to_owned(),
                matches!(entry.scope, DataScope::Shared(_)),
            )
        })
        .collect();
    check(
        scopes
            == [
                ("Channel".to_owned(), true),
                ("FFT".to_owned(), true),
                ("Tilt".to_owned(), false),
                ("Center".to_owned(), false),
                ("Gain".to_owned(), false),
                ("Peak".to_owned(), true),
            ],
        &format!("shared group not shown as Peak with its ancestors: {scopes:?}"),
    )?;
    panel(&mut h, PanelControl::Close)?;
    open_settings(&mut h, b)?;
    h.locate(&ControlKey::Panel(PanelControl::Unshare(peak())))?;
    let channel = node(&spec_b, |n| matches!(n, NodeSpec::Channel(_)))?;
    panel(
        &mut h,
        PanelControl::Channel {
            node: channel,
            channel: ChannelSelection::Right,
        },
    )?;
    let both_right = [a, b]
        .iter()
        .map(|id| spec(&h.ws, *id).map(|s| channel_of(&s)))
        .collect::<Result<Vec<_>, _>>()?;
    let after_shared_edit = handle.metrics();
    check(
        both_right == [Some(ChannelSelection::Right); 2]
            && after_shared_edit.active_spectrum_nodes == 4
            && spec(&h.ws, a)?.outputs()[1].binding == binding,
        &format!("shared edit did not apply to every member: {both_right:?} {after_shared_edit:?}"),
    )?;
    action(&mut h.ws, WorkspaceAction::SelectModule { instance_id: a })?;
    h.key(egui::Key::Num1);
    let both_left = [a, b]
        .iter()
        .map(|id| spec(&h.ws, *id).map(|s| channel_of(&s)))
        .collect::<Result<Vec<_>, _>>()?;
    check(
        both_left == [Some(ChannelSelection::Left); 2],
        &format!("channel shortcut did not follow the shared semantics: {both_left:?}"),
    )?;
    panel(&mut h, PanelControl::Unshare(peak()))?;
    let (spec_a, spec_b) = (spec(&h.ws, a)?, spec(&h.ws, b)?);
    let unlinked = handle.metrics();
    check(
        spec_b.outputs()[1].binding.is_none()
            && spec_a.outputs()[1].binding == binding
            && unlinked.active_spectrum_nodes == 5,
        &format!("unlinking did not fork the copy to a private Peak: {unlinked:?}"),
    )?;
    panel(
        &mut h,
        PanelControl::Channel {
            node: channel,
            channel: ChannelSelection::Right,
        },
    )?;
    check(
        channel_of(&spec(&h.ws, a)?) == Some(ChannelSelection::Left)
            && channel_of(&spec(&h.ws, b)?) == Some(ChannelSelection::Right),
        "after unlinking an edit still reached the other instance",
    )?;
    h.ws.check_ownership()?;
    Ok(
        json!({"scopes":scopes,"spectrum_nodes":{"shared":shared.active_spectrum_nodes,
        "unlinked":unlinked.active_spectrum_nodes}}),
    )
}

/// Stroke colors of the polylines drawn within the horizontal span of `rect`.
fn stroke_colors(output: &egui::FullOutput, rect: egui::Rect) -> BTreeSet<[u8; 4]> {
    let span = rect.expand(1.0).x_range();
    output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Path(path)
                if path.points.len() > 2
                    && path.points.iter().all(|point| span.contains(point.x)) =>
            {
                match path.stroke.color {
                    egui::epaint::ColorMode::Solid(color) => Some(color.to_srgba_unmultiplied()),
                    egui::epaint::ColorMode::UV(_) => None,
                }
            }
            _ => None,
        })
        .collect()
}

fn rgba(color: crate::render::Color) -> [u8; 4] {
    [color.red, color.green, color.blue, color.alpha]
}

/// Global theme, type style and instance overrides resolve in that order.
fn s03() -> Result<Value, String> {
    let (ws, mut driver) = workspace()?;
    let mut h = Harness::new(ws);
    tall(&mut h);
    let a = first_leaf(&h.ws)?;
    let b = created(action(
        &mut h.ws,
        WorkspaceAction::DuplicateModule { instance_id: a },
    )?)?;
    let waveform = created(action(
        &mut h.ws,
        WorkspaceAction::SplitModule {
            instance_id: b,
            side: Side::Below,
        },
    )?)?;
    let waveform = created(action(
        &mut h.ws,
        WorkspaceAction::ReplaceModule {
            instance_id: waveform,
            type_id: WAVEFORM_MODULE_ID,
        },
    )?)?;
    open_settings(&mut h, b)?;
    let swatch = SWATCHES[3];
    panel(
        &mut h,
        PanelControl::Override {
            scope: ColorScope::Instance(b),
            role: ColorRole::Primary,
        },
    )?;
    panel(
        &mut h,
        PanelControl::Swatch {
            scope: ColorScope::Instance(b),
            role: ColorRole::Primary,
            index: 3,
        },
    )?;
    panel(&mut h, PanelControl::Close)?;
    let dark = Preset::Dark.palette();
    let light = Preset::Light.palette();
    let palette = |h: &Harness, id: ModuleInstanceId| {
        h.ws.observation()
            .module(id)
            .map(|m| m.palette)
            .ok_or_else(|| format!("instance {} missing", id.0))
    };
    check(
        palette(&h, b)?.color(ColorRole::Primary) == swatch
            && palette(&h, a)?.color(ColorRole::Primary) == dark.color(ColorRole::Primary),
        "instance override did not stay on the instance",
    )?;
    // The override is what the Module actually draws with.
    driver
        .advance_frames(4_096, deadline())
        .map_err(|e| e.to_string())?;
    let output = h.frame(vec![]);
    let rects =
        h.ws.observation()
            .window(FIRST_WINDOW)
            .ok_or("window closed")?
            .leaf_rects
            .clone();
    let rect_of = |id| rects.iter().find(|(leaf, _)| *leaf == id).map(|(_, r)| *r);
    let drawn_b = stroke_colors(&output, rect_of(b).ok_or("b not placed")?);
    let drawn_a = stroke_colors(&output, rect_of(a).ok_or("a not placed")?);
    check(
        drawn_b.contains(&rgba(swatch)) && drawn_a.contains(&rgba(dark.color(ColorRole::Primary))),
        &format!("drawn curve colors: original {drawn_a:?}, copy {drawn_b:?}"),
    )?;

    h.menu(
        ControlKey::LeafBody(a),
        &[MenuItem::WorkspaceOpen, MenuItem::Theme],
    )?;
    settle_frames(&mut h);
    panel(&mut h, PanelControl::Preset(Preset::Light))?;
    check(
        palette(&h, a)?.color(ColorRole::Primary) == light.color(ColorRole::Primary)
            && palette(&h, b)?.color(ColorRole::Primary) == swatch
            && palette(&h, b)?.color(ColorRole::Low) == light.color(ColorRole::Low),
        "after the global change unset roles must follow and overrides stay",
    )?;
    panel(&mut h, PanelControl::TypeStyle(SPECTRUM_MODULE_ID))?;
    let type_scope = ColorScope::Type(SPECTRUM_MODULE_ID);
    panel(
        &mut h,
        PanelControl::Override {
            scope: type_scope,
            role: ColorRole::Secondary,
        },
    )?;
    panel(
        &mut h,
        PanelControl::Swatch {
            scope: type_scope,
            role: ColorRole::Secondary,
            index: 6,
        },
    )?;
    let type_color = SWATCHES[6];
    check(
        palette(&h, a)?.color(ColorRole::Secondary) == type_color
            && palette(&h, b)?.color(ColorRole::Secondary) == type_color
            && palette(&h, waveform)?.color(ColorRole::Secondary)
                == light.color(ColorRole::Secondary),
        "type style did not apply to exactly the Spectrum instances",
    )?;
    panel(
        &mut h,
        PanelControl::Override {
            scope: ColorScope::Global,
            role: ColorRole::Primary,
        },
    )?;
    panel(
        &mut h,
        PanelControl::Swatch {
            scope: ColorScope::Global,
            role: ColorRole::Primary,
            index: 4,
        },
    )?;
    let global = SWATCHES[4];
    check(
        palette(&h, a)?.color(ColorRole::Primary) == global
            && palette(&h, waveform)?.color(ColorRole::Primary) == global
            && palette(&h, b)?.color(ColorRole::Primary) == swatch,
        "global edit must reach every instance without its own override",
    )?;
    let theme = h.ws.observation().theme;
    Ok(
        json!({"preset":theme.preset,"type_styles":theme.type_styles.len(),
        "drawn_copy":drawn_b,"drawn_original":drawn_a}),
    )
}

/// Every object key of a JSON value, recursively.
fn keys(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                out.insert(key.clone());
                keys(value, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| keys(item, out)),
        _ => {}
    }
}

/// What a restored workspace must reproduce, without process identities.
fn fingerprint(ws: &Workspace) -> Value {
    let o = ws.observation();
    let windows: Vec<Value> = o
        .windows
        .iter()
        .map(|window| {
            let modules: Vec<Value> = window
                .leaves
                .iter()
                .filter_map(|leaf| o.module(*leaf))
                .map(|module| {
                    let data = module.data.as_ref().map(|data| {
                        let mut data = data.clone();
                        // Bindings are compared by who shares them, not by number.
                        data.rebind(|_| crate::dataflow::SharedAnalysisId(0));
                        serde_json::to_value(data).unwrap_or(Value::Null)
                    });
                    json!({"type":module.type_id,"style":module.active_style,"data":data,
                        "view":module.view.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
                        "colors":module.colors})
                })
                .collect();
            json!({"modules":modules,
                "splits":window.splits.iter().map(|s| json!([format!("{:?}", s.axis), s.ratio, s.manual])).collect::<Vec<_>>(),
                "active":window.active.and_then(|a| window.leaves.iter().position(|l| *l == a)),
                "always_on_top":window.always_on_top})
        })
        .collect();
    json!({"windows":windows,"theme":o.theme})
}

/// Save (menu and autosave on closing the last window), restart, compare.
fn s04() -> Result<Value, String> {
    let dir = TempDir::new("s04")?;
    let (ws, _driver) = workspace()?;
    let handle = ws.dataflow_handle_for_automation();
    // Push runtime binding numbers away from the file's.
    for _ in 0..5 {
        handle.allocate_binding();
    }
    let mut h = Harness::new(ws.with_store(dir.store(), "Round trip"));
    tall(&mut h);
    let a = first_leaf(&h.ws)?;
    let spec_a = spec(&h.ws, a)?;
    let channel = node(&spec_a, |n| matches!(n, NodeSpec::Channel(_)))?;
    let fft = node(&spec_a, |n| matches!(n, NodeSpec::Fft(_)))?;
    for step in [
        WorkspaceAction::EditNode {
            instance_id: a,
            node: channel,
            spec: NodeSpec::Channel(ChannelSelection::Right),
        },
        WorkspaceAction::EditNode {
            instance_id: a,
            node: fft,
            spec: NodeSpec::Fft(FftConfig::new(4096, 512, -160.0).map_err(|e| format!("{e:?}"))?),
        },
        WorkspaceAction::SetStyle {
            instance_id: a,
            style_id: BAR_SPECTRUM_STYLE_ID,
        },
        WorkspaceAction::SetViewSetting {
            instance_id: a,
            key: "max_frequency_hz",
            value: SettingValue::Number(10_000.0),
        },
        WorkspaceAction::SetColor {
            scope: ColorScope::Instance(a),
            role: ColorRole::Primary,
            color: Some(crate::render::Color::rgb(250, 230, 90)),
        },
        WorkspaceAction::ShareOutput {
            instance_id: a,
            output: peak(),
        },
    ] {
        action(&mut h.ws, step)?;
    }
    let b = created(action(
        &mut h.ws,
        WorkspaceAction::DuplicateModule { instance_id: a },
    )?)?;
    let waveform = created(action(
        &mut h.ws,
        WorkspaceAction::SplitModule {
            instance_id: b,
            side: Side::Below,
        },
    )?)?;
    let waveform = created(action(
        &mut h.ws,
        WorkspaceAction::ReplaceModule {
            instance_id: waveform,
            type_id: WAVEFORM_MODULE_ID,
        },
    )?)?;
    for step in [
        WorkspaceAction::SetViewSetting {
            instance_id: waveform,
            key: "visible_seconds",
            value: SettingValue::Number(7.5),
        },
        WorkspaceAction::SetViewSetting {
            instance_id: waveform,
            key: "channel",
            value: SettingValue::Channel(ChannelSelection::Left),
        },
        // Shorter History than the visible span: the span follows it down.
        WorkspaceAction::SetKeep {
            instance_id: waveform,
            output: OutputId::from_index(0),
            seconds: 3.0,
        },
        WorkspaceAction::SetThemePreset {
            preset: Preset::Light,
        },
        WorkspaceAction::SetColor {
            scope: ColorScope::Type(WAVEFORM_MODULE_ID),
            role: ColorRole::Low,
            color: Some(crate::render::Color::rgb(10, 20, 30)),
        },
        WorkspaceAction::SetColor {
            scope: ColorScope::Global,
            role: ColorRole::Accent,
            color: Some(crate::render::Color::rgba(200, 100, 50, 128)),
        },
    ] {
        action(&mut h.ws, step)?;
    }
    let visible = h.ws.observation().module(waveform).and_then(|m| {
        m.view
            .iter()
            .find(|(k, _)| *k == "visible_seconds")
            .map(|(_, v)| v.clone())
    });
    check(
        visible == Some(SettingValue::Number(3.0)),
        &format!("visible span not kept within the shorter History: {visible:?}"),
    )?;
    let oscilloscope = created(action(
        &mut h.ws,
        WorkspaceAction::SplitModule {
            instance_id: a,
            side: Side::Below,
        },
    )?)?;
    let oscilloscope = created(action(
        &mut h.ws,
        WorkspaceAction::ReplaceModule {
            instance_id: oscilloscope,
            type_id: OSCILLOSCOPE_MODULE_ID,
        },
    )?)?;
    let second = new_window(action(
        &mut h.ws,
        WorkspaceAction::DetachModule {
            instance_id: oscilloscope,
        },
    )?)?;
    h.frame_all()?;
    action(
        &mut h.ws,
        WorkspaceAction::SetAlwaysOnTop {
            window_id: second,
            enabled: true,
        },
    )?;
    h.window_mut(second)?.position = pos2(1200.0, 300.0);
    h.frame_window(second, vec![])?;
    h.frame_window(FIRST_WINDOW, vec![])?;
    h.focus = FIRST_WINDOW;
    // Save through the menu.
    h.menu(
        ControlKey::LeafBody(a),
        &[MenuItem::WorkspaceOpen, MenuItem::SaveWorkspace],
    )?;
    check(
        dir.file("Round trip").exists(),
        "Save did not write the workspace",
    )?;
    // A later change is kept by Quit, which saves before closing every window.
    let split =
        h.ws.observation()
            .window(FIRST_WINDOW)
            .and_then(|w| w.splits.first().map(|s| s.id))
            .ok_or("no split")?;
    action(
        &mut h.ws,
        WorkspaceAction::SetSplitRatio {
            window_id: FIRST_WINDOW,
            split_id: split,
            ratio: 0.3,
            manual: true,
        },
    )?;
    let before = fingerprint(&h.ws);
    let modules_before = h.ws.observation().modules.len();
    h.frame(vec![]);
    h.menu(
        ControlKey::LeafBody(a),
        &[MenuItem::WorkspaceOpen, MenuItem::Quit],
    )?;
    check(
        h.ws.windows().is_empty() && h.ws.observation().modules.is_empty(),
        "Quit left windows or instances",
    )?;
    drop(h);
    let saved = read_json(&dir.file("Round trip"))?;
    let mut saved_keys = BTreeSet::new();
    keys(&saved, &mut saved_keys);
    const RUNTIME: [&str; 12] = [
        "instance_id",
        "window_id",
        "split_id",
        "id",
        "subscription",
        "reader",
        "snapshot",
        "result_version",
        "processed_through_frame",
        "texture",
        "hover",
        "cache",
    ];
    let leaked: Vec<&str> = RUNTIME
        .iter()
        .copied()
        .filter(|key| saved_keys.contains(*key))
        .collect();
    check(
        leaked.is_empty(),
        &format!("runtime state in the file: {leaked:?}"),
    )?;
    let saved_text = serde_json::to_string(&saved).unwrap_or_default();
    check(
        saved_text.contains("\"binding\":1") && !saved_text.contains("\"binding\":6"),
        "bindings were not renumbered for the file",
    )?;
    check(
        saved["windows"].as_array().map(Vec::len) == Some(2),
        "the file does not hold both windows",
    )?;

    // Restart.
    let (live, _driver) = manual_live()?;
    let ws = Workspace::open(live, dir.store(), false).map_err(|e| e.to_string())?;
    let mut h = Harness::new(ws);
    h.frame_all()?;
    let after = fingerprint(&h.ws);
    check(
        before == after,
        &format!("restored workspace differs:\nbefore {before}\nafter  {after}"),
    )?;
    let o = h.ws.observation();
    check(
        o.name == "Round trip" && o.notices.is_empty(),
        &format!("restore reported {:?}", o.notices),
    )?;
    check(
        o.modules.len() == modules_before,
        "restored a different number of instances",
    )?;
    let restored: Vec<ModuleInstanceId> = o.windows.iter().flat_map(|w| w.leaves.clone()).collect();
    settle(&h.ws, restored.len() as u64)?;
    let shared: Vec<_> = o
        .modules
        .iter()
        .filter_map(|m| m.data.as_ref())
        .filter_map(|data| data.outputs().get(1).and_then(|output| output.binding))
        .collect();
    let metrics = h.ws.dataflow_handle_for_automation().metrics();
    check(
        shared.len() == 2 && shared[0] == shared[1],
        &format!("restored copies do not share one binding: {shared:?}"),
    )?;
    let positions: Vec<_> = h.windows.values().map(|w| w.position).collect();
    check(
        positions.contains(&pos2(1200.0, 300.0)),
        &format!("detached window position not restored: {positions:?}"),
    )?;
    // Saving the restored workspace writes the same file.
    action(&mut h.ws, WorkspaceAction::SaveWorkspace)?;
    let resaved = read_json(&dir.file("Round trip"))?;
    check(
        resaved == saved,
        "saving the restored workspace changed the file",
    )?;
    h.ws.check_ownership()?;
    // A binding nobody uses any more frees its number for the next one.
    let members: Vec<ModuleInstanceId> =
        h.ws.observation()
            .modules
            .iter()
            .filter(|m| {
                m.data
                    .as_ref()
                    .is_some_and(|d| d.outputs().get(1).is_some_and(|o| o.binding.is_some()))
            })
            .map(|m| m.instance_id)
            .collect();
    for instance_id in &members {
        action(
            &mut h.ws,
            WorkspaceAction::UnshareOutput {
                instance_id: *instance_id,
                output: peak(),
            },
        )?;
    }
    action(
        &mut h.ws,
        WorkspaceAction::ShareOutput {
            instance_id: *members.last().ok_or("no members")?,
            output: peak(),
        },
    )?;
    action(&mut h.ws, WorkspaceAction::SaveWorkspace)?;
    let reshared = serde_json::to_string(&read_json(&dir.file("Round trip"))?).unwrap_or_default();
    check(
        reshared.matches("\"binding\":1").count() == 1 && !reshared.contains("\"binding\":2"),
        "a new binding did not take the freed number 1",
    )?;
    // Closing the last window ends the session and saves first.
    let windows: Vec<WindowId> = h.ws.windows().iter().map(|w| w.id).collect();
    let (last, others) = windows.split_first().ok_or("no window")?;
    for window_id in others {
        action(
            &mut h.ws,
            WorkspaceAction::CloseWindow {
                window_id: *window_id,
            },
        )?;
    }
    let split =
        h.ws.observation()
            .window(*last)
            .and_then(|w| w.splits.first().map(|s| s.id))
            .ok_or("no split after restore")?;
    action(
        &mut h.ws,
        WorkspaceAction::SetSplitRatio {
            window_id: *last,
            split_id: split,
            ratio: 0.6,
            manual: true,
        },
    )?;
    action(&mut h.ws, WorkspaceAction::CloseWindow { window_id: *last })?;
    let closed = read_json(&dir.file("Round trip"))?;
    check(
        closed["windows"].as_array().map(Vec::len) == Some(1)
            && closed["windows"][0]["layout"]["ratio"]
                .as_f64()
                .is_some_and(|ratio| (ratio - 0.6).abs() < 1e-6),
        "closing the last window did not save it",
    )?;
    Ok(
        json!({"windows":2,"modules":modules_before,"file_keys":saved_keys,
        "spectrum_nodes":metrics.active_spectrum_nodes,"positions":positions.iter().map(|p| [p.x, p.y]).collect::<Vec<_>>()}),
    )
}

/// Missing types, unusable configuration, unknown style and input, and a file
/// from a newer build: reported, kept verbatim, never overwritten.
fn s05() -> Result<Value, String> {
    let dir = TempDir::new("s05")?;
    let store = dir.store();
    let plugin = json!({"type_id":"vendor.plugin.meter",
        "data":{"nodes":[{"input":null,"kind":"vendor_node","params":{"q":3}}],
                "outputs":[{"node":0,"keep":"latest","binding":7}]},
        "view":{"needle":{"number":0.5}},"colors":{"primary":"#112233"}});
    let wrong_shape = json!({"type_id":SPECTRUM_MODULE_ID,
        "data":{"nodes":[],"outputs":[{"node":null,"keep":"latest"}]}});
    // The analysis fits a stereo source, the saved display channel does not.
    let unusable_view = json!({"type_id":OSCILLOSCOPE_MODULE_ID,
        "data":{"nodes":[],"outputs":[{"node":null,"keep":"latest"}]},
        "view":{"channel":{"channel":{"discrete":5}},"scale":{"number":2.0}}});
    let file = json!({"format_version":1,"name":"Imported","input":{"kind":"daw_bridge","pipe":"studio-a"},
        "theme":{"preset":"dark"},
        "windows":[{"outer_position":[120.0,140.0],"pixels_per_point":1.0,"inner_size":[900.0,420.0],
            "layout":{"node":"split","axis":"left_right","ratio":0.5,
                "first":{"node":"split","axis":"top_bottom","ratio":0.5,
                    "first":{"node":"leaf","module":plugin},
                    "second":{"node":"leaf","module":wrong_shape}},
                "second":{"node":"split","axis":"top_bottom","ratio":0.5,
                    "first":{"node":"leaf","module":{"type_id":OSCILLOSCOPE_MODULE_ID,"style":"vendor.glow",
                        "data":{"nodes":[],"outputs":[{"node":null,"keep":"latest"}]}}},
                    "second":{"node":"leaf","module":unusable_view}}}}]});
    fs::create_dir_all(dir.0.join("workspaces")).map_err(|e| e.to_string())?;
    fs::write(
        dir.file("Imported"),
        serde_json::to_vec_pretty(&file).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    store.set_last_used("Imported").map_err(|e| e.to_string())?;
    let (live, _driver) = manual_live()?;
    let ws = Workspace::open(live, store.clone(), false).map_err(|e| e.to_string())?;
    let mut h = Harness::new(ws);
    h.frame_all()?;
    let o = h.ws.observation();
    let missing: Vec<Option<String>> = o.modules.iter().map(|m| m.missing.clone()).collect();
    check(
        missing
            == [
                Some("vendor.plugin.meter".to_owned()),
                Some(SPECTRUM_MODULE_ID.to_owned()),
                None,
                Some(OSCILLOSCOPE_MODULE_ID.to_owned()),
            ],
        &format!("stand-ins not created for exactly the unusable modules: {missing:?}"),
    )?;
    let notices = o.notices.join("\n");
    for expected in [
        "vendor.plugin.meter",
        "daw_bridge",
        "vendor.glow",
        "does not match",
    ] {
        check(
            notices.contains(expected),
            &format!("no notice about {expected}: {notices}"),
        )?;
    }
    let oscilloscope = o.modules[2].instance_id;
    check(
        o.modules[2].active_style == Some(LINE_OSCILLOSCOPE_STYLE_ID),
        "unknown style did not fall back to the default",
    )?;
    settle(&h.ws, 1)?;
    action(&mut h.ws, WorkspaceAction::SaveWorkspace)?;
    let saved = read_json(&dir.file("Imported"))?;
    let layout = &saved["windows"][0]["layout"];
    check(
        layout["first"]["first"]["module"] == plugin
            && layout["first"]["second"]["module"] == wrong_shape
            && layout["second"]["second"]["module"] == unusable_view,
        "unusable modules were not saved verbatim",
    )?;
    check(
        layout["second"]["first"]["module"]["style"] == "vendor.glow"
            && saved["input"] == json!({"kind":"daw_bridge","pipe":"studio-a"}),
        "unknown style or input was not kept",
    )?;
    // A copy carries the kept style too.
    let copy = created(action(
        &mut h.ws,
        WorkspaceAction::DuplicateModule {
            instance_id: oscilloscope,
        },
    )?)?;
    action(&mut h.ws, WorkspaceAction::SaveWorkspace)?;
    check(
        read_json(&dir.file("Imported"))?["windows"][0]["layout"]["second"]["first"]["second"]["module"]
            ["style"]
            == "vendor.glow",
        "the copy dropped the kept style",
    )?;
    action(
        &mut h.ws,
        WorkspaceAction::CloseModule { instance_id: copy },
    )?;
    // Choosing the shown fallback style in the settings panel is an explicit
    // choice and replaces the kept one.
    h.set_size(vec2(900.0, 1400.0));
    open_settings(&mut h, oscilloscope)?;
    panel(&mut h, PanelControl::Style(LINE_OSCILLOSCOPE_STYLE_ID))?;
    action(&mut h.ws, WorkspaceAction::SaveWorkspace)?;
    check(
        read_json(&dir.file("Imported"))?["windows"][0]["layout"]["second"]["first"]["module"]["style"]
            == LINE_OSCILLOSCOPE_STYLE_ID,
        "an explicit style choice did not replace the kept one",
    )?;
    let placeholder = o.modules[0].instance_id;
    check(
        action(
            &mut h.ws,
            WorkspaceAction::DuplicateModule {
                instance_id: placeholder,
            },
        )
        .is_err(),
        "a stand-in was copied",
    )?;
    drop(h);

    // A newer build's file is protected.
    let future = br#"{"format_version": 99, "name": "Future", "windows": []}"#;
    fs::write(dir.file("Future"), future).map_err(|e| e.to_string())?;
    store.set_last_used("Future").map_err(|e| e.to_string())?;
    let (live, _driver) = manual_live()?;
    let mut ws = Workspace::open(live, store.clone(), false).map_err(|e| e.to_string())?;
    let o = ws.observation();
    check(
        o.notices.iter().any(|n| n.contains("version 99")) && o.windows.len() == 1,
        "a newer file did not open as a protected default workspace",
    )?;
    check(
        action(&mut ws, WorkspaceAction::SaveWorkspace)
            .is_err_and(|e| e.contains("could not be read")),
        "Save overwrote a protected workspace",
    )?;
    ws.autosave();
    check(
        fs::read(dir.file("Future")).map_err(|e| e.to_string())? == future,
        "autosave touched the protected file",
    )?;
    check(
        action(
            &mut ws,
            WorkspaceAction::SaveWorkspaceAs {
                name: "Future".to_owned(),
            },
        )
        .is_err_and(|e| e.contains("already exists"))
            && fs::read(dir.file("Future")).map_err(|e| e.to_string())? == future,
        "Save as overwrote the protected file under its own name",
    )?;
    action(
        &mut ws,
        WorkspaceAction::SaveWorkspaceAs {
            name: "Recovered".to_owned(),
        },
    )?;
    check(
        dir.file("Recovered").exists(),
        "Save as did not write a new workspace",
    )?;
    Ok(json!({"missing":missing,"notices":o.notices.len()}))
}

/// New workspace through the naming panel, then repeated switching: old instances
/// and their subscriptions are released, nothing accumulates.
fn s06() -> Result<Value, String> {
    let dir = TempDir::new("s06")?;
    let (ws, _driver) = workspace()?;
    let handle = ws.dataflow_handle_for_automation();
    let mut h = Harness::new(ws.with_store(dir.store(), "A"));
    tall(&mut h);
    let a = first_leaf(&h.ws)?;
    for type_id in [WAVEFORM_MODULE_ID, OSCILLOSCOPE_MODULE_ID] {
        let new = created(action(
            &mut h.ws,
            WorkspaceAction::SplitModule {
                instance_id: a,
                side: Side::Right,
            },
        )?)?;
        action(
            &mut h.ws,
            WorkspaceAction::ReplaceModule {
                instance_id: new,
                type_id,
            },
        )?;
    }
    settle(&h.ws, 3)?;
    h.menu(
        ControlKey::LeafBody(a),
        &[MenuItem::WorkspaceOpen, MenuItem::NewWorkspace],
    )?;
    settle_frames(&mut h);
    h.click(ControlKey::Panel(PanelControl::Name))?;
    h.frame(vec![Event::Text("B 2".to_owned())]);
    h.frame(vec![]);
    h.click(ControlKey::Panel(PanelControl::Confirm))?;
    let o = h.ws.observation();
    check(
        o.name == "B 2" && o.windows.len() == 1 && o.modules.len() == 1,
        &format!(
            "New did not start a default workspace: {} {:?}",
            o.name, o.control_error
        ),
    )?;
    check(
        !o.windows.iter().any(|w| w.id == FIRST_WINDOW),
        "the new workspace reused the old window identity",
    )?;
    settle(&h.ws, 1)?;
    check(
        read_json(&dir.file("A"))?["windows"][0]["layout"]["node"] == "split"
            && dir.file("B 2").exists(),
        "switching did not save the previous workspace",
    )?;
    let names = h.ws.saved_workspaces();
    let mut baseline = None;
    for round in 0..10 {
        let target = if round % 2 == 0 { "A" } else { "B 2" };
        action(
            &mut h.ws,
            WorkspaceAction::SwitchWorkspace {
                name: target.to_owned(),
            },
        )?;
        let expected = if target == "A" { 3 } else { 1 };
        settle(&h.ws, expected)?;
        h.ws.check_ownership()?;
        check(
            h.ws.observation().modules.len() == expected as usize,
            "instances of the previous workspace survived the switch",
        )?;
        let metrics = handle.metrics();
        if round == 0 {
            baseline = Some((metrics.nodes, metrics.rings));
        } else if round % 2 == 0 && baseline != Some((metrics.nodes, metrics.rings)) {
            return Err(format!(
                "dataflow grew across switches: {metrics:?} vs {baseline:?}"
            ));
        }
    }
    // Switching through the menu.
    h.focus = h.ws.windows().first().map(|w| w.id).ok_or("no window")?;
    h.frame_all()?;
    h.frame(vec![]);
    let leaf =
        h.ws.observation()
            .windows
            .first()
            .and_then(|w| w.leaves.first().copied())
            .ok_or("no leaf")?;
    let index = names.iter().position(|n| n == "A").ok_or("A not listed")? as u16;
    h.menu(
        ControlKey::LeafBody(leaf),
        &[
            MenuItem::WorkspaceOpen,
            MenuItem::SwitchOpen,
            MenuItem::Switch(index),
        ],
    )?;
    let o = h.ws.observation();
    check(o.name == "A" && o.modules.len() == 3, "menu switch failed")?;
    settle(&h.ws, 3)?;
    let metrics = handle.metrics();
    Ok(
        json!({"saved":names,"final_nodes":metrics.nodes,"final_rings":metrics.rings,
        "baseline":baseline}),
    )
}

/// Windows that end up off every monitor come back; partly visible ones stay.
fn s07() -> Result<Value, String> {
    let (ws, _driver) = workspace()?;
    let mut h = Harness::new(ws);
    h.frame(vec![]);
    let commands = |h: &mut Harness| -> Vec<ViewportCommand> { h.take_commands(FIRST_WINDOW) };
    commands(&mut h);
    // The monitor left of the primary one was disconnected.
    h.window_mut(FIRST_WINDOW)?.position = pos2(-2500.0, 200.0);
    h.frame(vec![]);
    let rescued = h.window(FIRST_WINDOW)?.position;
    check(
        rescued == pos2(0.0, 200.0),
        &format!("off-screen window not brought back: {rescued:?}"),
    )?;
    commands(&mut h);
    h.frame(vec![]);
    h.window_mut(FIRST_WINDOW)?.position = pos2(1500.0, 300.0);
    h.frame(vec![]);
    check(
        h.window(FIRST_WINDOW)?.position == pos2(1500.0, 300.0)
            && !commands(&mut h)
                .iter()
                .any(|c| matches!(c, ViewportCommand::OuterPosition(_))),
        "a partly visible window was moved",
    )?;

    // Restored onto a monitor that is no longer there.
    let dir = TempDir::new("s07")?;
    let file = json!({"format_version":1,"name":"Away",
        "windows":[{"outer_position":[-5000.0,300.0],"pixels_per_point":1.0,"inner_size":[400.0,300.0],
            "layout":{"node":"leaf","module":{"type_id":SPECTRUM_MODULE_ID}}}]});
    fs::create_dir_all(dir.0.join("workspaces")).map_err(|e| e.to_string())?;
    fs::write(
        dir.file("Away"),
        serde_json::to_vec(&file).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    dir.store()
        .set_last_used("Away")
        .map_err(|e| e.to_string())?;
    let (live, _driver) = manual_live()?;
    let ws = Workspace::open(live, dir.store(), false).map_err(|e| e.to_string())?;
    let mut restored = Harness::new(ws);
    let opened_at = restored.window(restored.focus)?.position;
    restored.frame(vec![]);
    let landed = restored.window(restored.focus)?.position;
    check(
        opened_at == pos2(-5000.0, 300.0) && landed == pos2(0.0, 300.0),
        &format!("restored off-screen window: opened {opened_at:?}, now {landed:?}"),
    )?;
    Ok(json!({"rescued":[rescued.x,rescued.y],"restored":[landed.x,landed.y]}))
}
