//! S01–S08: settings window, copies, invisible sharing, theme, saving and
//! restoring, missing extensions, workspace switching, window rescue and reset.
//! They run the real Workspace, Module factories and manual source with temporary
//! configuration directories; settings operations use real egui events.

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
    dataflow::{ChannelSelection, DataSpec, FftConfig, Keep, NodeRef, NodeSpec, OutputId},
    modules::{
        BAR_SPECTRUM_STYLE_ID, EMPTY_MODULE_ID, LINE_OSCILLOSCOPE_STYLE_ID, OSCILLOSCOPE_MODULE_ID,
        SPECTRUM_MODULE_ID, WAVEFORM_MODULE_ID,
    },
    persistence::WorkspaceStore,
    settings::{NodeParam, SettingValue},
    theme::{ColorRole, Preset},
    workspace::{
        ActionOutcome, ColorScope, ControlKey, FIRST_WINDOW, ModuleInstanceId, SWATCHES,
        SettingsItem, Side, Tab, WindowId, Workspace, WorkspaceAction,
    },
};

use crate::automation::{
    harness::Harness,
    support::{action, check, created, deadline, first_leaf, wait_until, workspace},
};

type Scenario = fn() -> Result<Value, String>;

pub(super) const CASES: [(&str, Scenario); 8] = [
    ("S01", s01),
    ("S02", s02),
    ("S03", s03),
    ("S04", s04),
    ("S05", s05),
    ("S06", s06),
    ("S07", s07),
    ("S08", s08),
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

/// Right-click `leaf` (opening the settings window on it) and click `items` there;
/// focus stays in the settings window.
fn open_settings(
    h: &mut Harness,
    leaf: ModuleInstanceId,
    items: &[SettingsItem],
) -> Result<(), String> {
    // Instances created by direct actions get their controls in the next pass.
    h.focus =
        h.ws.observation()
            .module(leaf)
            .and_then(|module| module.window_id)
            .ok_or_else(|| format!("instance {} is not placed", leaf.0))?;
    h.frame(vec![]);
    h.settings(ControlKey::LeafBody(leaf), &[])?;
    h.in_settings(items)
}

/// Whether `instance` (in the window `h` draws) shows something, judged from the
/// shapes drawn within its leaf.
type Shown = fn(&egui::FullOutput, egui::Rect) -> bool;

/// Feed enough audio for settled results, then drag `key`'s slider in the open
/// settings window three 6-point steps to the right without releasing, feeding
/// about one display frame of audio per step, and check that the parameter
/// follows the drag and that `instance` keeps showing `what` before and at every
/// step: a changed node fills its first window while the view keeps the previous
/// result.
fn drag_keeps_showing(
    h: &mut Harness,
    driver: &mut ManualInputDriver,
    key: SettingsItem,
    instance: ModuleInstanceId,
    what: &str,
    shown: Shown,
) -> Result<(), String> {
    let window =
        h.ws.observation()
            .module(instance)
            .and_then(|module| module.window_id)
            .ok_or("instance not placed")?;
    let showing = |h: &mut Harness| -> Result<bool, String> {
        let settings = std::mem::replace(&mut h.focus, window);
        let output = h.frame(vec![]);
        h.focus = settings;
        let rect =
            h.ws.observation()
                .window(window)
                .and_then(|w| {
                    w.leaf_rects
                        .iter()
                        .find(|(id, _)| *id == instance)
                        .map(|(_, rect)| *rect)
                })
                .ok_or("instance has no leaf")?;
        Ok(shown(&output, rect))
    };
    driver
        .advance_frames(8_192, deadline())
        .map_err(|e| e.to_string())?;
    check(showing(h)?, &format!("no {what} before the drag"))?;
    let before = spec(&h.ws, instance)?;
    let slider = h.locate(&ControlKey::Settings(key))?.rect;
    let y = slider.center().y;
    let mut x = slider.center().x;
    let button = |x, pressed| Event::PointerButton {
        pos: pos2(x, y),
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.frame(vec![Event::PointerMoved(pos2(x, y))]);
    h.frame(vec![button(x, true)]);
    for step in 1..=3 {
        x += 6.0;
        h.frame(vec![Event::PointerMoved(pos2(x, y))]);
        driver
            .advance_frames(800, deadline())
            .map_err(|e| e.to_string())?;
        check(
            showing(h)?,
            &format!("{what} went blank at drag step {step} of {key:?}"),
        )?;
    }
    let dragged = spec(&h.ws, instance)?;
    h.frame(vec![button(x, false)]);
    h.frame(vec![]);
    check(
        dragged != before,
        &format!("{key:?} did not apply during the drag"),
    )
}

fn main_curve_shown(output: &egui::FullOutput, rect: egui::Rect) -> bool {
    stroke_colors(output, rect).contains(&rgba(Preset::Dark.palette().color(ColorRole::Primary)))
}

fn peak_curve_shown(output: &egui::FullOutput, rect: egui::Rect) -> bool {
    // Translucent: compare the color as egui stores it.
    let peak = Preset::Dark.palette().color(ColorRole::Secondary);
    let drawn = egui::Color32::from_rgba_unmultiplied(peak.red, peak.green, peak.blue, peak.alpha)
        .to_srgba_unmultiplied();
    stroke_colors(output, rect).contains(&drawn)
}

fn heat_map_shown(output: &egui::FullOutput, rect: egui::Rect) -> bool {
    output.shapes.iter().any(|clipped| match &clipped.shape {
        egui::Shape::Mesh(mesh) => mesh.vertices.iter().any(|vertex| rect.contains(vertex.pos)),
        _ => false,
    })
}

/// Click the typed value of `item` in the open settings window, type `text` and
/// finish with `key`: Enter applies it, Escape keeps the old value.
fn type_value(
    h: &mut Harness,
    item: SettingsItem,
    text: &str,
    key: egui::Key,
) -> Result<(), String> {
    h.click(ControlKey::SettingsValue(item))?;
    h.frame(vec![Event::Text(text.to_owned())]);
    h.key(key);
    h.frame(vec![]);
    Ok(())
}

/// Copy, analysis settings shown once per node, edits that only reach the target.
fn s01() -> Result<Value, String> {
    let (ws, mut driver) = workspace()?;
    let handle = ws.dataflow_handle_for_automation();
    let mut h = Harness::new(ws);
    h.frame(vec![]);
    let a = first_leaf(&h.ws)?;
    let a_object = h.ws.observation().module(a).ok_or("a missing")?.object;
    open_settings(&mut h, a, &[SettingsItem::SplitRight])?;
    let window = leaves(&h.ws, FIRST_WINDOW);
    check(
        window.len() == 2 && window[0] == a,
        "Split right did not add a copy beside the original",
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
        copied.active_fft_nodes == 1 && copied.active_spectrum_nodes == 4,
        &format!("copy did not share the pure chain and keep private Peaks: {copied:?}"),
    )?;

    open_settings(&mut h, b, &[])?;
    let fft = node(&spec_b, |n| matches!(n, NodeSpec::Fft(_)))?;
    let tilt = node(&spec_b, |n| matches!(n, NodeSpec::Tilt { .. }))?;
    let targets = h.ws.settings_targets();
    let count = |matches: fn(&SettingsItem) -> bool| {
        targets
            .keys()
            .filter(|key| matches!(key, ControlKey::Settings(item) if matches(item)))
            .count()
    };
    let channel_controls = count(|c| matches!(c, SettingsItem::Channel(_)));
    let fft_controls = count(|c| matches!(c, SettingsItem::FftWindow { .. }));
    check(
        channel_controls == 3 && fft_controls == 6,
        &format!(
            "Channel/FFT read by both outputs must appear once: {channel_controls} channel, {fft_controls} FFT controls"
        ),
    )?;
    h.in_settings(&[
        SettingsItem::Channel(ChannelSelection::Right),
        SettingsItem::FftWindow {
            node: fft,
            size: 4096,
        },
    ])?;
    let key = ControlKey::Settings(SettingsItem::Number {
        node: tilt,
        param: NodeParam::Tilt,
    });
    // The slider applies while it is dragged, before the button is released.
    let slider = h.locate(&key)?.rect;
    let (start, end) = (slider.center(), pos2(slider.max.x - 2.0, slider.center().y));
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.frame(vec![egui::Event::PointerMoved(start)]);
    h.frame(vec![button(start, true)]);
    h.frame(vec![egui::Event::PointerMoved(end)]);
    let live = match spec(&h.ws, b)?.node(tilt) {
        Some(NodeSpec::Tilt { db_per_octave }) => db_per_octave,
        _ => f32::NAN,
    };
    h.frame(vec![button(end, false)]);
    h.frame(vec![]);
    check(
        live > 6.0,
        &format!("Tilt did not follow the slider during the drag: {live}"),
    )?;
    // Overlays start hidden; the peak curve is shown through its toggle.
    h.in_settings(&[SettingsItem::ViewToggle("show_peak")])?;
    let shown_peak = h.ws.observation().module(b).and_then(|m| {
        m.view
            .iter()
            .find(|(key, _)| *key == "show_peak")
            .map(|(_, value)| value.clone())
    });
    check(
        shown_peak == Some(SettingValue::Bool(true)),
        &format!("the Peak curve toggle did not show the peak curve: {shown_peak:?}"),
    )?;
    // While a slider is dragged the view keeps the previous result until the
    // changed nodes produce: the copy's 4096-point FFT restarts at every Floor step
    // and its Peak at every hold step, and a step brings less audio than a window.
    let peak_node = node(&spec(&h.ws, b)?, |n| matches!(n, NodeSpec::Peak(_)))?;
    for (key, what, shown) in [
        (
            SettingsItem::Number {
                node: fft,
                param: NodeParam::FftFloor,
            },
            "the main curve",
            main_curve_shown as Shown,
        ),
        (
            SettingsItem::Number {
                node: peak_node,
                param: NodeParam::PeakHold,
            },
            "the peak curve",
            peak_curve_shown,
        ),
    ] {
        drag_keeps_showing(&mut h, &mut driver, key, b, what, shown)?;
    }
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
    let view_of = |h: &Harness, id: ModuleInstanceId, key: &str| {
        h.ws.observation().module(id).and_then(|m| {
            m.view
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.clone())
        })
    };
    // A range the zoom gesture reaches (down to 80 Hz) is a valid setting: copies
    // and restores keep it.
    action(
        &mut h.ws,
        WorkspaceAction::SetViewSetting {
            instance_id: a,
            key: "max_frequency_hz",
            value: SettingValue::Number(400.0),
        },
    )?;
    let zoomed = created(action(
        &mut h.ws,
        WorkspaceAction::SplitModule {
            instance_id: a,
            side: Side::Right,
        },
    )?)?;
    let copied_range = view_of(&h, zoomed, "max_frequency_hz");
    check(
        copied_range == Some(SettingValue::Number(400.0)),
        &format!("a zoomed frequency range did not survive a copy: {copied_range:?}"),
    )?;
    // A History shorter than the usual shortest span bounds the view to itself.
    let waveform = created(action(
        &mut h.ws,
        WorkspaceAction::ReplaceModule {
            instance_id: zoomed,
            type_id: WAVEFORM_MODULE_ID,
        },
    )?)?;
    for step in [
        WorkspaceAction::SetKeep {
            instance_id: waveform,
            output: OutputId::from_index(0),
            seconds: 0.1,
        },
        WorkspaceAction::SetViewSetting {
            instance_id: waveform,
            key: "visible_seconds",
            value: SettingValue::Number(0.1),
        },
    ] {
        action(&mut h.ws, step)?;
    }
    let short_view = view_of(&h, waveform, "visible_seconds");
    check(
        short_view == Some(SettingValue::Number(0.1)),
        &format!("the view outgrew a short History: {short_view:?}"),
    )?;
    // A Spectrogram keeps its earlier columns while its FFT restarts.
    let spectrogram = created(action(
        &mut h.ws,
        WorkspaceAction::ReplaceModule {
            instance_id: waveform,
            type_id: crate::modules::SPECTROGRAM_MODULE_ID,
        },
    )?)?;
    open_settings(&mut h, spectrogram, &[])?;
    let spectrogram_fft = node(&spec(&h.ws, spectrogram)?, |n| {
        matches!(n, NodeSpec::Fft(_))
    })?;
    drag_keeps_showing(
        &mut h,
        &mut driver,
        SettingsItem::Number {
            node: spectrogram_fft,
            param: NodeParam::FftFloor,
        },
        spectrogram,
        "the Spectrogram heat map",
        heat_map_shown,
    )?;
    // A value can be typed, with or without its unit; text that is no number in
    // that unit, or Escape, keeps the value, and Escape stays in the field
    // instead of closing the window.
    let visible = SettingsItem::ViewNumber("visible_seconds");
    let seconds = |h: &Harness| view_of(h, spectrogram, "visible_seconds");
    type_value(&mut h, visible, "2 s", egui::Key::Enter)?;
    check(
        seconds(&h) == Some(SettingValue::Number(2.0)),
        &format!("typed visible time: {:?}", seconds(&h)),
    )?;
    type_value(&mut h, visible, "abc", egui::Key::Enter)?;
    type_value(&mut h, visible, "3", egui::Key::Escape)?;
    check(
        seconds(&h) == Some(SettingValue::Number(2.0)) && h.ws.settings_open(),
        &format!(
            "invalid or escaped typing changed the value or closed the window: {:?}",
            seconds(&h)
        ),
    )?;
    // ↺ beside a value returns it to the type's default and then goes away.
    h.in_settings(&[])?;
    h.click(ControlKey::SettingsReset(visible))?;
    let reset_gone = !h
        .ws
        .settings_targets()
        .contains_key(&ControlKey::SettingsReset(visible));
    check(
        seconds(&h) == Some(SettingValue::Number(6.0)) && reset_gone,
        &format!(
            "row reset: {:?}, ↺ still shown: {}",
            seconds(&h),
            !reset_gone
        ),
    )?;
    // The Analysis card's ↺ returns every analysis parameter, History length
    // included, to the defaults; the channel belongs to the Module card and stays.
    type_value(
        &mut h,
        SettingsItem::Keep(OutputId::from_index(0)),
        "3",
        egui::Key::Enter,
    )?;
    h.in_settings(&[
        SettingsItem::Channel(ChannelSelection::Right),
        SettingsItem::FftWindow {
            node: spectrogram_fft,
            size: 4096,
        },
        SettingsItem::ResetAnalysis,
    ])?;
    let restored = spec(&h.ws, spectrogram)?;
    check(
        restored.node(spectrogram_fft) == Some(NodeSpec::Fft(FftConfig::default()))
            && restored.outputs()[0].keep == Keep::Seconds(8.0)
            && channel_of(&restored) == Some(ChannelSelection::Right),
        &format!("Analysis reset left {restored:?}"),
    )?;
    // Frequencies take kilohertz; the Display card's ↺ restores the view.
    open_settings(&mut h, b, &[])?;
    let max_frequency = SettingsItem::ViewNumber("max_frequency_hz");
    type_value(&mut h, max_frequency, "1.5k", egui::Key::Enter)?;
    let typed_frequency = view_of(&h, b, "max_frequency_hz");
    h.in_settings(&[SettingsItem::ResetSection("Display")])?;
    check(
        typed_frequency == Some(SettingValue::Number(1_500.0))
            && view_of(&h, b, "max_frequency_hz") == Some(SettingValue::Number(20_000.0)),
        &format!(
            "typed {typed_frequency:?}, after the Display reset {:?}",
            view_of(&h, b, "max_frequency_hz")
        ),
    )?;
    h.ws.check_ownership()?;
    Ok(
        json!({"channel_controls":channel_controls,"fft_controls":fft_controls,
        "copy_tilt_db_per_octave":tilt_of(&spec_b),"fft_nodes":edited.active_fft_nodes}),
    )
}

/// Sharing is invisible: identical analysis is shared by the dataflow on its own,
/// an edit only changes its instance (forking what changed), and going back to the
/// same parameters shares again. The settings offer nothing about sharing.
fn s02() -> Result<Value, String> {
    let (ws, _driver) = workspace()?;
    let handle = ws.dataflow_handle_for_automation();
    let mut h = Harness::new(ws);
    h.frame(vec![]);
    let a = first_leaf(&h.ws)?;
    let b = created(action(
        &mut h.ws,
        WorkspaceAction::SplitModule {
            instance_id: a,
            side: Side::Right,
        },
    )?)?;
    settle(&h.ws, 2)?;
    let fft_nodes = || handle.metrics().active_fft_nodes;
    let shared = fft_nodes();
    let fft = node(&spec(&h.ws, b)?, |n| matches!(n, NodeSpec::Fft(_)))?;
    open_settings(&mut h, b, &[])?;
    let labels: Vec<String> =
        h.ws.settings_targets()
            .keys()
            .map(|key| format!("{key:?}"))
            .collect();
    check(
        !labels
            .iter()
            .any(|label| label.contains("Share") || label.contains("Bind")),
        "the settings expose sharing controls",
    )?;
    h.in_settings(&[SettingsItem::FftWindow {
        node: fft,
        size: 8192,
    }])?;
    let forked = fft_nodes();
    h.in_settings(&[SettingsItem::FftWindow {
        node: fft,
        size: 2048,
    }])?;
    let rejoined = fft_nodes();
    check(
        (shared, forked, rejoined) == (1, 2, 1),
        &format!("FFT nodes identical → edited → identical again: {shared}, {forked}, {rejoined}"),
    )?;
    // Shortcuts change only the selected instance as well.
    h.focus = FIRST_WINDOW;
    action(&mut h.ws, WorkspaceAction::SelectModule { instance_id: a })?;
    h.key(egui::Key::Num1);
    check(
        channel_of(&spec(&h.ws, a)?) == Some(ChannelSelection::Left)
            && channel_of(&spec(&h.ws, b)?) == Some(ChannelSelection::MonoAverage),
        "a channel shortcut reached the other instance",
    )?;
    let split = fft_nodes();
    h.key(egui::Key::M);
    check(
        split == 2 && fft_nodes() == 1,
        "returning to the same channel did not share the FFT again",
    )?;
    h.ws.check_ownership()?;
    Ok(json!({"fft_nodes":{"shared":shared,"edited":forked,"rejoined":rejoined}}))
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
    h.frame(vec![]);
    let a = first_leaf(&h.ws)?;
    let b = created(action(
        &mut h.ws,
        WorkspaceAction::SplitModule {
            instance_id: a,
            side: Side::Right,
        },
    )?)?;
    let waveform = created(action(
        &mut h.ws,
        WorkspaceAction::SplitModule {
            instance_id: b,
            side: Side::Below,
        },
    )?)?;
    // The type picker replaces the instance; the settings follow its successor.
    open_settings(&mut h, waveform, &[])?;
    h.dropdown(
        ControlKey::Settings(SettingsItem::TypePicker),
        ControlKey::Settings(SettingsItem::ModuleType(WAVEFORM_MODULE_ID)),
    )?;
    let waveform =
        h.ws.observation()
            .modules
            .iter()
            .find(|module| module.type_id == WAVEFORM_MODULE_ID)
            .map(|module| module.instance_id)
            .ok_or("the type picker did not replace the module with a Waveform")?;
    check(
        h.ws.observation().module(b).map(|m| m.type_id) == Some(SPECTRUM_MODULE_ID),
        "the type picker replaced the wrong instance",
    )?;
    let swatch = SWATCHES[3];
    // A swatch sets the instance's own color directly.
    open_settings(
        &mut h,
        b,
        &[SettingsItem::Swatch {
            scope: ColorScope::Instance(b),
            role: ColorRole::Primary,
            index: 3,
        }],
    )?;
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
    h.focus = FIRST_WINDOW;
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

    let type_scope = ColorScope::Type(SPECTRUM_MODULE_ID);
    h.in_settings(&[
        SettingsItem::Tab(Tab::Theme),
        SettingsItem::Preset(Preset::Light),
    ])?;
    check(
        palette(&h, a)?.color(ColorRole::Primary) == light.color(ColorRole::Primary)
            && palette(&h, b)?.color(ColorRole::Primary) == swatch
            && palette(&h, b)?.color(ColorRole::Low) == light.color(ColorRole::Low),
        "after the global change unset roles must follow and overrides stay",
    )?;
    h.in_settings(&[
        SettingsItem::TypeStyle(SPECTRUM_MODULE_ID),
        SettingsItem::Swatch {
            scope: type_scope,
            role: ColorRole::Secondary,
            index: 6,
        },
    ])?;
    let type_color = SWATCHES[6];
    check(
        palette(&h, a)?.color(ColorRole::Secondary) == type_color
            && palette(&h, b)?.color(ColorRole::Secondary) == type_color
            && palette(&h, waveform)?.color(ColorRole::Secondary)
                == light.color(ColorRole::Secondary),
        "type style did not apply to exactly the Spectrum instances",
    )?;
    h.in_settings(&[SettingsItem::Swatch {
        scope: ColorScope::Global,
        role: ColorRole::Primary,
        index: 4,
    }])?;
    let global = SWATCHES[4];
    check(
        palette(&h, a)?.color(ColorRole::Primary) == global
            && palette(&h, waveform)?.color(ColorRole::Primary) == global
            && palette(&h, b)?.color(ColorRole::Primary) == swatch,
        "global edit must reach every instance without its own override",
    )?;
    // Reset returns the global role to the preset; the instance keeps its own.
    h.in_settings(&[SettingsItem::ResetColor {
        scope: ColorScope::Global,
        role: ColorRole::Primary,
    }])?;
    check(
        palette(&h, a)?.color(ColorRole::Primary) == light.color(ColorRole::Primary)
            && palette(&h, b)?.color(ColorRole::Primary) == swatch,
        "Reset did not return the global color to the preset",
    )?;
    // Typed text size; its ↺ follows the preset's size again.
    type_value(&mut h, SettingsItem::TextSize, "14", egui::Key::Enter)?;
    let typed_size = h.ws.observation().theme.text_size;
    h.click(ControlKey::SettingsReset(SettingsItem::TextSize))?;
    check(
        typed_size == Some(14.0) && h.ws.observation().theme.text_size.is_none(),
        &format!(
            "text size typed {typed_size:?}, after ↺ {:?}",
            h.ws.observation().theme.text_size
        ),
    )?;
    // Card ↺: every global edit, the shown type's style, every instance override.
    h.in_settings(&[
        SettingsItem::Swatch {
            scope: ColorScope::Global,
            role: ColorRole::Text,
            index: 1,
        },
        SettingsItem::ResetColors(ColorScope::Global),
        SettingsItem::ResetColors(type_scope),
        SettingsItem::Tab(Tab::Module),
        SettingsItem::ResetColors(ColorScope::Instance(b)),
    ])?;
    let theme = h.ws.observation().theme;
    check(
        theme.colors.is_empty()
            && theme
                .type_styles
                .get(SPECTRUM_MODULE_ID)
                .is_none_or(|s| s.is_empty())
            && h.ws
                .observation()
                .module(b)
                .is_some_and(|m| m.colors.is_empty())
            && palette(&h, b)? == palette(&h, a)?,
        &format!(
            "card resets left global {:?}, type {:?}",
            theme.colors, theme.type_styles
        ),
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
    let mut h = Harness::new(ws.with_store(dir.store(), "Round trip"));
    h.frame(vec![]);
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
    ] {
        action(&mut h.ws, step)?;
    }
    let b = created(action(
        &mut h.ws,
        WorkspaceAction::SplitModule {
            instance_id: a,
            side: Side::Right,
        },
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
    // Save through the settings window.
    h.settings(
        ControlKey::LeafBody(a),
        &[SettingsItem::Tab(Tab::Workspace), SettingsItem::Save],
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
    h.settings(
        ControlKey::LeafBody(a),
        &[SettingsItem::Tab(Tab::Workspace), SettingsItem::Quit],
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
        !saved_text.contains("binding"),
        "copies and edits created explicit sharing",
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
    let metrics = h.ws.dataflow_handle_for_automation().metrics();
    let positions: Vec<_> = h.workspace_windows().map(|(_, w)| w.position).collect();
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
    // Empty has no analysis to put saved analysis into.
    let empty_with_data = json!({"type_id":EMPTY_MODULE_ID,
        "data":{"nodes":[],"outputs":[{"node":null,"keep":"latest"}]}});
    // Right shape, but an FFT far too large to plan or allocate.
    let huge_fft = json!({"type_id":SPECTRUM_MODULE_ID,"data":{"nodes":[
        {"input":null,"kind":"channel","params":"mono_average"},
        {"input":0,"kind":"fft","params":{"window_size":4_611_686_018_427_387_904_u64,"hop_size":1,"floor_dbfs":-160.0}},
        {"input":1,"kind":"tilt","params":{"db_per_octave":0.0}},
        {"input":2,"kind":"gain","params":{"db":0.0}},
        {"input":3,"kind":"peak","params":{"hold_ms":1000.0,"decay_db_per_second":18.0}}],
        "outputs":[{"node":3,"keep":"latest"},{"node":4,"keep":"latest"}]}});
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
                    "second":{"node":"leaf","module":unusable_view}}}},
            {"outer_position":[1100.0,140.0],"pixels_per_point":1.0,"inner_size":[300.0,200.0],
            "layout":{"node":"split","axis":"left_right","ratio":0.5,
                "first":{"node":"leaf","module":empty_with_data},
                "second":{"node":"leaf","module":huge_fft}}}]});
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
                Some(EMPTY_MODULE_ID.to_owned()),
                Some(SPECTRUM_MODULE_ID.to_owned()),
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
            && layout["second"]["second"]["module"] == unusable_view
            && saved["windows"][1]["layout"]["first"]["module"] == empty_with_data
            && saved["windows"][1]["layout"]["second"]["module"] == huge_fft,
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
        WorkspaceAction::SplitModule {
            instance_id: oscilloscope,
            side: Side::Right,
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
    open_settings(
        &mut h,
        oscilloscope,
        &[SettingsItem::Style(LINE_OSCILLOSCOPE_STYLE_ID)],
    )?;
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
            WorkspaceAction::SplitModule {
                instance_id: placeholder,
                side: Side::Right,
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
    h.frame(vec![]);
    h.settings(
        ControlKey::LeafBody(a),
        &[SettingsItem::Tab(Tab::Workspace)],
    )?;
    // New first saves the current workspace, so its own name (unsaved here, and in
    // any letter case: Windows file names) cannot become the new one's.
    check(
        action(
            &mut h.ws,
            WorkspaceAction::NewWorkspace {
                name: "a".to_owned(),
            },
        )
        .is_err()
            && h.ws.observation().modules.len() == 3
            && !dir.file("A").exists(),
        "New accepted the current workspace's name",
    )?;
    h.in_settings(&[SettingsItem::Name])?;
    h.frame(vec![Event::Text("a".to_owned())]);
    h.frame(vec![]);
    let offered = h.in_settings(&[SettingsItem::NewWorkspace]);
    check(
        offered
            .as_ref()
            .is_err_and(|error| error.starts_with("Disabled control")),
        &format!("New was offered under the current name: {offered:?}"),
    )?;
    h.key(egui::Key::Backspace);
    // Typed text (a space included) is text, not the Space shortcut.
    h.frame(vec![Event::Text("B 2".to_owned())]);
    h.frame(vec![]);
    check(
        h.ws.observation().source_status.state == crate::audio::SourceState::Running,
        "typing a name paused the source",
    )?;
    h.in_settings(&[SettingsItem::NewWorkspace])?;
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
    // Switching through the settings window.
    h.focus = h.ws.windows().first().map(|w| w.id).ok_or("no window")?;
    h.frame_all()?;
    let index = names.iter().position(|n| n == "A").ok_or("A not listed")? as u16;
    h.in_settings(&[SettingsItem::Switch(index)])?;
    let o = h.ws.observation();
    check(o.name == "A" && o.modules.len() == 3, "menu switch failed")?;
    settle(&h.ws, 3)?;
    // The record of the workspace to open next time cannot be written (a directory
    // stands where the file belongs): Save As still commits to the new name and
    // reports the record separately.
    let state = dir.0.join("state.json");
    fs::remove_file(&state).map_err(|e| e.to_string())?;
    fs::create_dir(&state).map_err(|e| e.to_string())?;
    action(
        &mut h.ws,
        WorkspaceAction::SaveWorkspaceAs {
            name: "C".to_owned(),
        },
    )?;
    let o = h.ws.observation();
    check(
        o.name == "C"
            && dir.file("C").exists()
            && o.notices.iter().any(|n| n.contains("open next time")),
        &format!(
            "Save As did not keep its committed name: {} {:?}",
            o.name, o.notices
        ),
    )?;
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

/// Reset of the workspace (with confirmation), settings for an empty window, and a
/// saved explicit binding (as a plugin or an older file may carry): kept on
/// restore, but an edit through the settings still changes only its instance.
fn s08() -> Result<Value, String> {
    let dir = TempDir::new("s08")?;
    let spectrum = |extra: Value| {
        json!({"type_id":SPECTRUM_MODULE_ID,"data":{"nodes":[
            {"input":null,"kind":"channel","params":"mono_average"},
            {"input":0,"kind":"fft","params":{"window_size":2048,"hop_size":512,"floor_dbfs":-160.0}},
            {"input":1,"kind":"tilt","params":{"db_per_octave":0.0}},
            {"input":2,"kind":"gain","params":{"db":0.0}},
            {"input":3,"kind":"peak","params":{"hold_ms":1000.0,"decay_db_per_second":18.0}}],
            "outputs":[{"node":3,"keep":"latest"},extra]}})
    };
    let bound = json!({"node":4,"keep":"latest","binding":9});
    // The second window is always on top, the first (whose place Reset keeps) is
    // not. The second is smaller than an empty window may be, which its Module
    // allows.
    let file = json!({"format_version":1,"name":"Bound","theme":{"preset":"light"},
        "windows":[{"outer_position":[300.0,200.0],"pixels_per_point":1.0,"inner_size":[900.0,420.0],
            "always_on_top":false,
            "layout":{"node":"split","axis":"left_right","ratio":0.5,
                "first":{"node":"leaf","module":spectrum(bound.clone())},
                "second":{"node":"leaf","module":spectrum(bound)}}},
            {"outer_position":[1300.0,200.0],"pixels_per_point":1.0,"inner_size":[120.0,40.0],
            "always_on_top":true,
            "layout":{"node":"leaf","module":{"type_id":WAVEFORM_MODULE_ID}}}]});
    fs::create_dir_all(dir.0.join("workspaces")).map_err(|e| e.to_string())?;
    fs::write(
        dir.file("Bound"),
        serde_json::to_vec(&file).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    dir.store()
        .set_last_used("Bound")
        .map_err(|e| e.to_string())?;
    let (live, _driver) = manual_live()?;
    let ws = Workspace::open(live, dir.store(), false).map_err(|e| e.to_string())?;
    let handle = ws.dataflow_handle_for_automation();
    let mut h = Harness::new(ws);
    h.frame_all()?;
    settle(&h.ws, 3)?;
    let restored = handle.metrics().active_spectrum_nodes;
    check(
        restored == 3,
        &format!("the saved binding was not restored as one shared Peak: {restored}"),
    )?;
    let small = h.ws.observation().windows[1].id;
    let small_min =
        h.ws.windows()
            .into_iter()
            .find(|window| window.id == small)
            .and_then(|window| window.builder.min_inner_size);
    check(
        h.window(small)?.size == vec2(120.0, 40.0) && small_min == Some(vec2(40.0, 32.0)),
        &format!(
            "a small window was not restored at its size: {:?}, minimum {small_min:?}",
            h.window(small)?.size
        ),
    )?;
    let window = h.focus;
    let leaves = leaves(&h.ws, window);
    let (a, b) = (leaves[0], leaves[1]);
    open_settings(&mut h, b, &[SettingsItem::Channel(ChannelSelection::Right)])?;
    check(
        channel_of(&spec(&h.ws, a)?) == Some(ChannelSelection::MonoAverage)
            && channel_of(&spec(&h.ws, b)?) == Some(ChannelSelection::Right)
            && spec(&h.ws, b)?.outputs()[1].binding.is_none()
            && spec(&h.ws, a)?.outputs()[1].binding.is_some(),
        "an edit of a bound instance did not stay on that instance",
    )?;
    action(&mut h.ws, WorkspaceAction::SaveWorkspace)?;
    let saved = serde_json::to_string(&read_json(&dir.file("Bound"))?).unwrap_or_default();
    check(
        saved.matches("\"binding\":9").count() == 1,
        "the remaining binding did not keep its file number",
    )?;

    // Detach from the settings window while the source window has been minimized
    // for longer than the detach timeout: timed on the app clock, it still opens.
    h.window_mut(window)?.minimized = true;
    h.frame_window(window, vec![])?;
    h.display_time += std::time::Duration::from_secs(10);
    h.in_settings(&[SettingsItem::Detach])?;
    h.window_mut(window)?.minimized = false;
    h.frame_all()?;
    let o = h.ws.observation();
    let detached_to = o
        .modules
        .iter()
        .find(|module| module.instance_id == b)
        .and_then(|module| module.window_id);
    check(
        detached_to.is_some_and(|id| id != window) && o.control_error.is_none(),
        &format!(
            "detach from a minimized window timed out: {detached_to:?} {:?}",
            o.control_error
        ),
    )?;

    // Reset: one default Spectrum, Dark theme, same name and place; saved.
    let before = h.ws.observation().modules.len();
    h.in_settings(&[SettingsItem::Tab(Tab::Workspace), SettingsItem::Reset])?;
    check(
        h.ws.observation().modules.len() == before,
        "Reset acted without confirmation",
    )?;
    h.in_settings(&[
        SettingsItem::CancelReset,
        SettingsItem::Reset,
        SettingsItem::ConfirmReset,
    ])?;
    h.frame_all()?;
    let o = h.ws.observation();
    check(
        o.name == "Bound"
            && o.windows.len() == 1
            && !o.windows[0].always_on_top
            && o.modules.len() == 1
            && o.modules[0].type_id == SPECTRUM_MODULE_ID
            && o.theme.preset == "dark"
            && o.theme.colors.is_empty(),
        &format!("Reset did not restore the defaults: {:?}", o.windows.len()),
    )?;
    settle(&h.ws, 1)?;
    let reset_file = read_json(&dir.file("Bound"))?;
    let new_window = o.windows[0].id;
    let position = h.window(new_window)?.position;
    check(
        reset_file["theme"]["preset"] == "dark"
            && reset_file["windows"].as_array().map(Vec::len) == Some(1)
            && position == pos2(300.0, 200.0)
            && h.ws.settings_open(),
        &format!("Reset was not saved or moved the window: {position:?}"),
    )?;

    // A window minimized while the settings window edits it still receives the
    // native command.
    let only = o.modules[0].instance_id;
    h.focus = new_window;
    h.settings(
        ControlKey::LeafBody(only),
        &[SettingsItem::Tab(Tab::Window)],
    )?;
    h.window_mut(new_window)?.minimized = true;
    h.frame_window(new_window, vec![])?;
    h.take_commands(new_window);
    h.in_settings(&[SettingsItem::AlwaysOnTop])?;
    h.frame_window(new_window, vec![])?;
    check(
        h.take_commands(new_window).iter().any(|command| {
            matches!(
                command,
                ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop)
            )
        }) && h.ws.observation().windows[0].always_on_top,
        "a minimized window did not receive the settings window's command",
    )?;
    h.window_mut(new_window)?.minimized = false;
    h.frame_window(new_window, vec![])?;

    // An empty window's right click opens the Window tab, which can add a module.
    action(
        &mut h.ws,
        WorkspaceAction::CloseModule { instance_id: only },
    )?;
    h.focus = new_window;
    h.frame(vec![]);
    h.settings(ControlKey::EmptyBody, &[])?;
    h.in_settings(&[SettingsItem::AddModule(WAVEFORM_MODULE_ID)])?;
    let o = h.ws.observation();
    check(
        o.modules.len() == 1 && o.modules[0].type_id == WAVEFORM_MODULE_ID,
        "Add module in the settings window failed",
    )?;
    h.ws.check_ownership()?;
    Ok(json!({"restored_spectrum_nodes":restored,"reset_position":[position.x,position.y]}))
}
