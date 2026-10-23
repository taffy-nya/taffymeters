use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use eframe::egui::{self, Event, ViewportCommand, pos2, vec2};
use serde_json::{Value, json};

use crate::{
    audio::synthetic::ManualInputDriver,
    dataflow::{ChannelSelection, DataSpec, FftConfig, Keep, NodeRef, NodeSpec, OutputId},
    modules::{
        BARS_SPECTRUM_STYLE_ID, EMPTY_MODULE_ID, LINE_OSCILLOSCOPE_STYLE_ID,
        OSCILLOSCOPE_MODULE_ID, SPECTRUM_MODULE_ID, WAVEFORM_MODULE_ID,
    },
    persistence::{PresetRef, WorkspaceStore},
    settings::{NodeParam, SettingValue},
    theme::{ColorRole, ThemePreset},
    workspace::{
        ColorScope, ControlKey, Display, EdgeLine, FIRST_WINDOW, ModuleInstanceId, PickerPart,
        SettingsItem, Side, Tab, WindowId, Workspace, WorkspaceAction, swatches,
    },
};

use crate::automation::{
    harness::{FakeDesktop, Harness, SETTINGS},
    support::{
        TempDir, action, check, created, deadline, first_leaf, leaves, new_window, read_bytes,
        read_json, settle_subscriptions, synthetic, workspace, write_bytes,
    },
};

use super::Scenario;

pub(super) const CASES: [(&str, Scenario); 13] = [
    ("S01", s01),
    ("S02", s02),
    ("S03", s03),
    ("S04", s04),
    ("S05", s05),
    ("S06", s06),
    ("S07", s07),
    ("S08", s08),
    ("S09", s09),
    ("S10", s10),
    ("S11", s11),
    ("S12", s12),
    ("S13", s13),
];

impl TempDir {
    fn store(&self) -> WorkspaceStore {
        WorkspaceStore::new(self.path())
    }

    fn preset(&self, name: &str) -> PathBuf {
        self.path().join("presets").join(format!("{name}.json"))
    }

    fn session(&self) -> PathBuf {
        self.path().join("session.json")
    }

    fn write_session(&self, file: &Value) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(file).expect("a JSON value serializes");
        write_bytes(&self.session(), &bytes)
    }
}

/// Matches the harness's default monitor so restoration recognizes it.
fn primary_display() -> Value {
    json!({"work_area":[1920.0,1040.0],"hint":{"origin_px":[0.0,0.0],"scale":1.0}})
}

fn reopen(dir: &TempDir) -> Result<(Workspace, ManualInputDriver), String> {
    let (live, driver) = synthetic()?;
    let ws = Workspace::open(live, dir.store(), FakeDesktop::new()).map_err(|e| e.to_string())?;
    Ok((ws, driver))
}

fn saved(name: &str) -> PresetRef {
    PresetRef::Saved(name.to_owned())
}

fn builtin(name: &str) -> PresetRef {
    PresetRef::Builtin(name.to_owned())
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

/// Leaves focus in the settings window.
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

type Shown = fn(&egui::FullOutput, egui::Rect) -> bool;

/// Each drag step supplies less than a new FFT window, so the previous result must stay visible.
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
    stroke_colors(output, rect)
        .contains(&rgba(ThemePreset::Dark.palette().color(ColorRole::Primary)))
}

fn peak_curve_shown(output: &egui::FullOutput, rect: egui::Rect) -> bool {
    // Translucent: compare the color as egui stores it.
    let peak = ThemePreset::Dark.palette().color(ColorRole::Secondary);
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
    settle_subscriptions(&h.ws, 2)?;
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
        copied.active_fft_nodes == 1 && copied.active_spectrum_nodes == 3,
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
    // Peak starts hidden, so enable it before checking continuity during analysis edits.
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
    // Floor restarts the copy's 4096-point FFT; hold restarts Peak.
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
        tilt_of(&spec_a) == crate::modules::DEFAULT_TILT_DB_PER_OCTAVE && tilt_of(&spec_b) > 6.0,
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
    // The 80 Hz range is reachable by zoom, not just by direct setting actions.
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
    // Use a History shorter than the usual minimum view span.
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

/// Equal analysis shares automatically; edits fork only their instance and reverting shares again.
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
    settle_subscriptions(&h.ws, 2)?;
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
    open_settings(&mut h, a, &[SettingsItem::Channel(ChannelSelection::Left)])?;
    check(
        channel_of(&spec(&h.ws, a)?) == Some(ChannelSelection::Left)
            && channel_of(&spec(&h.ws, b)?) == Some(ChannelSelection::MonoAverage),
        "a channel edit reached the other instance",
    )?;
    let split = fft_nodes();
    h.in_settings(&[SettingsItem::Channel(ChannelSelection::MonoAverage)])?;
    check(
        split == 2 && fft_nodes() == 1,
        "returning to the same channel did not share the FFT again",
    )?;
    h.ws.check_ownership()?;
    Ok(json!({"fft_nodes":{"shared":shared,"edited":forked,"rejoined":rejoined}}))
}

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
    let swatch = swatches(true)[3];
    open_settings(
        &mut h,
        b,
        &[SettingsItem::Swatch {
            scope: ColorScope::Instance(b),
            role: ColorRole::Primary,
            index: 3,
        }],
    )?;
    let dark = ThemePreset::Dark.palette();
    let light = ThemePreset::Light.palette();
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
        SettingsItem::Preset(ThemePreset::Light),
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
    let type_color = swatches(false)[6];
    check(
        palette(&h, a)?.color(ColorRole::Secondary) == type_color
            && palette(&h, b)?.color(ColorRole::Secondary) == type_color
            && palette(&h, waveform)?.color(ColorRole::Secondary)
                == light.color(ColorRole::Secondary),
        "type style did not apply to exactly the Spectrum instances",
    )?;
    let swatch_of = |scope, role| {
        ControlKey::Settings(SettingsItem::Swatch {
            scope,
            role,
            index: 0,
        })
    };
    let spectrum_offers_primary =
        h.ws.settings_targets()
            .contains_key(&swatch_of(type_scope, ColorRole::Primary));
    h.in_settings(&[SettingsItem::TypeStyle(WAVEFORM_MODULE_ID)])?;
    let targets = h.ws.settings_targets();
    let waveform_scope = ColorScope::Type(WAVEFORM_MODULE_ID);
    check(
        spectrum_offers_primary
            && targets.contains_key(&swatch_of(waveform_scope, ColorRole::Low))
            && !targets.contains_key(&swatch_of(waveform_scope, ColorRole::Primary))
            && !targets.contains_key(&ControlKey::Settings(SettingsItem::TypeStyle(
                EMPTY_MODULE_ID,
            ))),
        "type style colors do not follow what the type draws with",
    )?;
    h.in_settings(&[SettingsItem::TypeStyle(SPECTRUM_MODULE_ID)])?;
    h.in_settings(&[SettingsItem::Swatch {
        scope: ColorScope::Global,
        role: ColorRole::Primary,
        index: 4,
    }])?;
    let global = swatches(false)[4];
    check(
        palette(&h, a)?.color(ColorRole::Primary) == global
            && palette(&h, waveform)?.color(ColorRole::Primary) == global
            && palette(&h, b)?.color(ColorRole::Primary) == swatch,
        "global edit must reach every instance without its own override",
    )?;
    h.in_settings(&[SettingsItem::ResetColor {
        scope: ColorScope::Global,
        role: ColorRole::Primary,
    }])?;
    check(
        palette(&h, a)?.color(ColorRole::Primary) == light.color(ColorRole::Primary)
            && palette(&h, b)?.color(ColorRole::Primary) == swatch,
        "Reset did not return the global color to the preset",
    )?;
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
    h.in_settings(&[SettingsItem::Tab(Tab::Workspace)])?;
    type_value(
        &mut h,
        SettingsItem::ReferenceLevel,
        "-24",
        egui::Key::Enter,
    )?;
    let typed_reference = h.ws.reference_dbfs();
    h.click(ControlKey::SettingsReset(SettingsItem::ReferenceLevel))?;
    check(
        typed_reference == -24.0 && h.ws.reference_dbfs() == crate::module::DEFAULT_REFERENCE_DBFS,
        &format!(
            "reference typed {typed_reference}, after ↺ {}",
            h.ws.reference_dbfs()
        ),
    )?;
    type_value(&mut h, SettingsItem::FpsLimit, "30", egui::Key::Enter)?;
    let typed_fps = h.ws.fps_limit();
    h.click(ControlKey::SettingsReset(SettingsItem::FpsLimit))?;
    check(
        typed_fps == 30.0 && h.ws.fps_limit() == 60.0,
        &format!(
            "frame rate limit typed {typed_fps}, after ↺ {}",
            h.ws.fps_limit()
        ),
    )?;
    h.in_settings(&[SettingsItem::Tab(Tab::Theme)])?;
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
                .is_none_or(BTreeMap::is_empty)
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

/// Excludes process identities so restarts can be compared.
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
                        // Binding numbers are not part of this restart comparison.
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
                "always_on_top":window.always_on_top,"locked":window.locked})
        })
        .collect();
    json!({"windows":windows,"theme":o.theme})
}

/// Explicit preset saves and session autosaves survive restart without overwriting each other.
fn s04() -> Result<Value, String> {
    let dir = TempDir::new("s04")?;
    let (ws, _driver) = workspace()?;
    let mut h = Harness::new(ws.with_store(dir.store()));
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
            style_id: BARS_SPECTRUM_STYLE_ID,
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
        // A 3 s History must shorten the requested 7.5 s visible span.
        WorkspaceAction::SetKeep {
            instance_id: waveform,
            output: OutputId::from_index(0),
            seconds: 3.0,
        },
        WorkspaceAction::SetReferenceLevel { dbfs: Some(-24.0) },
        WorkspaceAction::SetFpsLimit { fps: Some(30.0) },
        WorkspaceAction::SetThemePreset {
            preset: ThemePreset::Light,
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
    action(
        &mut h.ws,
        WorkspaceAction::SetLocked {
            window_id: second,
            enabled: true,
        },
    )?;
    h.window_mut(second)?.position = pos2(1200.0, 300.0);
    h.frame_window(second, vec![])?;
    h.frame_window(FIRST_WINDOW, vec![])?;
    h.focus = FIRST_WINDOW;
    h.settings(
        ControlKey::LeafBody(a),
        &[SettingsItem::Tab(Tab::Workspace)],
    )?;
    h.in_settings(&[SettingsItem::PresetName])?;
    h.frame(vec![Event::Text("Round trip".to_owned())]);
    h.frame(vec![]);
    h.in_settings(&[SettingsItem::SavePresetAs])?;
    check(
        dir.preset("Round trip").exists() && h.ws.observation().preset == Some(saved("Round trip")),
        "Save as did not write the preset",
    )?;
    let preset_file = read_json(&dir.preset("Round trip"))?;
    check(
        preset_file.get("preset").is_none(),
        "a preset records a session origin",
    )?;
    h.focus = FIRST_WINDOW;
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
    check(
        read_json(&dir.preset("Round trip"))? == preset_file,
        "the session's autosave changed the preset",
    )?;
    let saved_session = read_json(&dir.session())?;
    let mut saved_keys = BTreeSet::new();
    keys(&saved_session, &mut saved_keys);
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
    let saved_text = serde_json::to_string(&saved_session).unwrap_or_default();
    check(
        !saved_text.contains("binding"),
        "copies and edits created explicit sharing",
    )?;
    check(
        saved_session["windows"].as_array().map(Vec::len) == Some(2)
            && saved_session["preset"] == json!({"saved":"Round trip"}),
        "the session does not hold both windows and its preset",
    )?;

    let (ws, _driver) = reopen(&dir)?;
    let mut h = Harness::new(ws);
    h.frame_all()?;
    let after = fingerprint(&h.ws);
    check(
        before == after,
        &format!("restored workspace differs:\nbefore {before}\nafter  {after}"),
    )?;
    check(
        h.ws.reference_dbfs() == -24.0 && saved_session["reference_dbfs"] == -24.0,
        &format!(
            "reference level not restored: {} (file {})",
            h.ws.reference_dbfs(),
            saved_session["reference_dbfs"]
        ),
    )?;
    check(
        h.ws.fps_limit() == 30.0 && saved_session["fps_limit"] == 30.0,
        &format!(
            "frame rate limit not restored: {} (file {})",
            h.ws.fps_limit(),
            saved_session["fps_limit"]
        ),
    )?;
    let o = h.ws.observation();
    check(
        o.preset == Some(saved("Round trip")) && o.notices.is_empty(),
        &format!("restore reported {:?}", o.notices),
    )?;
    check(
        o.modules.len() == modules_before,
        "restored a different number of instances",
    )?;
    let restored: Vec<ModuleInstanceId> = o.windows.iter().flat_map(|w| w.leaves.clone()).collect();
    settle_subscriptions(&h.ws, restored.len() as u64)?;
    let metrics = h.ws.dataflow_handle_for_automation().metrics();
    let positions: Vec<_> = h.workspace_windows().map(|(_, w)| w.position).collect();
    check(
        positions.contains(&pos2(1200.0, 300.0)),
        &format!("detached window position not restored: {positions:?}"),
    )?;
    h.ws.autosave();
    check(
        read_json(&dir.session())? == saved_session,
        "saving the restored session changed the file",
    )?;
    action(&mut h.ws, WorkspaceAction::SavePreset)?;
    check(
        read_json(&dir.preset("Round trip"))?["windows"][0]["layout"]["ratio"]
            .as_f64()
            .is_some_and(|ratio| (ratio - 0.3).abs() < 1e-6),
        "Save to did not write the preset",
    )?;
    h.ws.check_ownership()?;
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
    let closed = read_json(&dir.session())?;
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

/// Unsupported types, configuration, styles, inputs and future files remain preserved and reported.
fn s05() -> Result<Value, String> {
    let dir = TempDir::new("s05")?;
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
        {"input":1,"kind":"tilt","params":{"db_per_octave":4.5}},
        {"input":2,"kind":"peak","params":{"hold_ms":1000.0,"fall_db_per_second_squared":30.0}}],
        "outputs":[{"node":2,"keep":"latest"},{"node":3,"keep":"latest"}]}});
    let file = json!({"format_version":2,"input":{"kind":"daw_bridge","pipe":"studio-a"},
        "theme":{"preset":"dark"},"displays":[primary_display()],
        "windows":[{"display":0,"frame":[120.0,140.0,1020.0,560.0],
            "layout":{"node":"split","axis":"left_right","ratio":0.5,
                "first":{"node":"split","axis":"top_bottom","ratio":0.5,
                    "first":{"node":"leaf","module":plugin},
                    "second":{"node":"leaf","module":wrong_shape}},
                "second":{"node":"split","axis":"top_bottom","ratio":0.5,
                    "first":{"node":"leaf","module":{"type_id":OSCILLOSCOPE_MODULE_ID,"style":"vendor.glow",
                        "data":{"nodes":[],"outputs":[{"node":null,"keep":"latest"}]}}},
                    "second":{"node":"leaf","module":unusable_view}}}},
            {"display":0,"frame":[1100.0,140.0,1400.0,340.0],
            "layout":{"node":"split","axis":"left_right","ratio":0.5,
                "first":{"node":"leaf","module":empty_with_data},
                "second":{"node":"leaf","module":huge_fft}}}]});
    dir.write_session(&file)?;
    let (ws, _driver) = reopen(&dir)?;
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
    let placeholder = o.modules[0].instance_id;
    let rejected = h.ws.apply_action(WorkspaceAction::SplitModule {
        instance_id: placeholder,
        side: Side::Right,
    });
    h.frame_all()?;
    let error = h.ws.observation().control_error.unwrap_or_default();
    let badge = h.ws.badge_text(|_| true);
    check(
        rejected.is_err()
            && error.starts_with(char::is_uppercase)
            && badge
                == Some(format!(
                    "{error} | {} notices · Settings › Workspace",
                    o.notices.len()
                )),
        &format!("badge {badge:?} for the error {error:?}"),
    )?;
    h.display_time += crate::workspace::MESSAGE_LIFETIME;
    h.frame_all()?;
    let after = h.ws.observation();
    check(
        h.ws.badge_text(|_| true).is_none()
            && after.control_error.is_none()
            && after.notices == o.notices,
        &format!(
            "badge kept {:?}; notices {:?}",
            h.ws.badge_text(|_| true),
            after.notices
        ),
    )?;
    let oscilloscope = o.modules[2].instance_id;
    check(
        o.modules[2].active_style == Some(LINE_OSCILLOSCOPE_STYLE_ID),
        "unknown style did not fall back to the default",
    )?;
    settle_subscriptions(&h.ws, 1)?;
    h.ws.autosave();
    let saved = read_json(&dir.session())?;
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
    let copy = created(action(
        &mut h.ws,
        WorkspaceAction::SplitModule {
            instance_id: oscilloscope,
            side: Side::Right,
        },
    )?)?;
    h.ws.autosave();
    check(
        read_json(&dir.session())?["windows"][0]["layout"]["second"]["first"]["second"]["module"]["style"]
            == "vendor.glow",
        "the copy dropped the kept style",
    )?;
    action(
        &mut h.ws,
        WorkspaceAction::CloseModule { instance_id: copy },
    )?;
    open_settings(
        &mut h,
        oscilloscope,
        &[SettingsItem::Style(LINE_OSCILLOSCOPE_STYLE_ID)],
    )?;
    h.ws.autosave();
    check(
        read_json(&dir.session())?["windows"][0]["layout"]["second"]["first"]["module"]["style"]
            == LINE_OSCILLOSCOPE_STYLE_ID,
        "an explicit style choice did not replace the kept one",
    )?;
    h.in_settings(&[SettingsItem::Tab(Tab::Workspace)])?;
    h.point_at(ControlKey::Settings(SettingsItem::Notice(0)))?;
    let cursor = h.window(SETTINGS)?.cursor;
    check(
        cursor == egui::CursorIcon::Default,
        &format!("a notice shows the {cursor:?} cursor"),
    )?;
    h.click(ControlKey::Settings(SettingsItem::CopyNotices))?;
    let expected = h.ws.observation().notices.join("\n");
    let copied = &h.window(SETTINGS)?.copied;
    check(
        copied.as_slice() == [expected.clone()],
        &format!("Copy put {copied:?} on the clipboard, not {expected:?}"),
    )?;
    // The restored windows do not touch, leaving no eligible Group neighbor.
    h.in_settings(&[SettingsItem::Tab(Tab::Window)])?;
    let group = h.locate(&ControlKey::Settings(SettingsItem::Group));
    check(
        group
            .as_ref()
            .is_err_and(|error| error.starts_with("Disabled")),
        &format!("Group without a touching window: {group:?}"),
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

    let future = br#"{"format_version": 99, "windows": []}"#;
    write_bytes(&dir.session(), future)?;
    let (mut ws, _driver) = reopen(&dir)?;
    let o = ws.observation();
    let notice = o.notices.first().cloned().unwrap_or_default();
    check(
        o.notices.len() == 1
            && notice.contains("newer version (99)")
            && o.windows.len() == 1
            && o.preset == Some(builtin("Default")),
        "an unreadable session did not open as the protected Default preset",
    )?;
    let fits = ws.badge_text(|_| true).unwrap_or_default();
    let narrow = ws.badge_text(|_| false).unwrap_or_default();
    check(
        fits.ends_with(&notice)
            && narrow.ends_with("1 notice · Settings › Workspace")
            && !narrow.contains(&notice),
        &format!("one notice: {fits:?} when it fits, {narrow:?} when not"),
    )?;
    ws.autosave();
    check(
        read_bytes(&dir.session())? == future,
        "autosave touched the protected session",
    )?;
    check(
        action(
            &mut ws,
            WorkspaceAction::SavePresetAs {
                name: "DEFAULT".to_owned(),
            },
        )
        .is_err_and(|e| e.contains("already exists")),
        "Save as took a built-in preset's name",
    )?;
    action(
        &mut ws,
        WorkspaceAction::SavePresetAs {
            name: "Recovered".to_owned(),
        },
    )?;
    ws.autosave();
    check(
        dir.preset("Recovered").exists() && read_bytes(&dir.session())? == future,
        "Save as did not write a preset, or the session became writable",
    )?;
    Ok(json!({"missing":missing,"notices":o.notices.len()}))
}

/// Preset naming, repeated loading and deletion leave no orphaned instances or subscriptions.
fn s06() -> Result<Value, String> {
    let dir = TempDir::new("s06")?;
    let (ws, _driver) = workspace()?;
    let handle = ws.dataflow_handle_for_automation();
    let mut h = Harness::new(ws.with_store(dir.store()));
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
    settle_subscriptions(&h.ws, 3)?;
    h.frame(vec![]);
    h.settings(
        ControlKey::LeafBody(a),
        &[SettingsItem::Tab(Tab::Workspace)],
    )?;
    // Reserve built-in names case-insensitively, matching Windows filename comparisons.
    check(
        action(
            &mut h.ws,
            WorkspaceAction::SavePresetAs {
                name: "default".to_owned(),
            },
        )
        .is_err()
            && !dir.preset("default").exists(),
        "Save as took a built-in preset's name",
    )?;
    let taken = "bar - top";
    h.in_settings(&[SettingsItem::PresetName])?;
    h.frame(vec![Event::Text(taken.to_owned())]);
    h.frame(vec![]);
    let offered = h.in_settings(&[SettingsItem::SavePresetAs]);
    check(
        offered
            .as_ref()
            .is_err_and(|error| error.starts_with("Disabled control")),
        &format!("Save as was offered under a built-in name: {offered:?}"),
    )?;
    for _ in taken.chars() {
        h.key(egui::Key::Backspace);
    }
    h.frame(vec![Event::Text("A 2".to_owned())]);
    h.frame(vec![]);
    check(
        h.ws.observation().source_status.state == crate::audio::SourceState::Running,
        "typing a name paused the source",
    )?;
    h.in_settings(&[SettingsItem::SavePresetAs])?;
    let o = h.ws.observation();
    check(
        dir.preset("A 2").exists() && o.preset == Some(saved("A 2")) && o.modules.len() == 3,
        &format!("Save as did not save the preset: {:?}", o.control_error),
    )?;
    h.in_settings(&[SettingsItem::BuiltinPreset(0)])?;
    let o = h.ws.observation();
    check(
        o.preset == Some(builtin("Default"))
            && o.windows.len() == 1
            && o.modules.len() == 1
            && o.modules[0].type_id == OSCILLOSCOPE_MODULE_ID,
        &format!(
            "Default did not replace the workspace: {:?} {:?}",
            o.preset, o.control_error
        ),
    )?;
    check(
        !o.windows.iter().any(|w| w.id == FIRST_WINDOW),
        "the loaded preset reused the old window identity",
    )?;
    settle_subscriptions(&h.ws, 1)?;
    let mut baseline = None;
    for round in 0..10 {
        let (preset, expected) = if round % 2 == 0 {
            (saved("A 2"), 3)
        } else {
            (builtin("Default"), 1)
        };
        action(&mut h.ws, WorkspaceAction::LoadPreset { preset })?;
        settle_subscriptions(&h.ws, expected)?;
        h.ws.check_ownership()?;
        check(
            h.ws.observation().modules.len() == expected as usize,
            "instances of the previous workspace survived the load",
        )?;
        let metrics = handle.metrics();
        if round == 0 {
            baseline = Some((metrics.nodes, metrics.rings));
        } else if round % 2 == 0 && baseline != Some((metrics.nodes, metrics.rings)) {
            return Err(format!(
                "dataflow grew across loads: {metrics:?} vs {baseline:?}"
            ));
        }
    }
    let names = h.ws.saved_presets();
    let index = names
        .iter()
        .position(|n| n == "A 2")
        .ok_or("A 2 not listed")? as u16;
    h.in_settings(&[SettingsItem::SavedPreset(index)])?;
    let o = h.ws.observation();
    check(
        o.preset == Some(saved("A 2")) && o.modules.len() == 3,
        "menu load failed",
    )?;
    settle_subscriptions(&h.ws, 3)?;
    h.in_settings(&[SettingsItem::DeletePreset(index)])?;
    check(
        dir.preset("A 2").exists(),
        "Delete acted without confirmation",
    )?;
    h.in_settings(&[
        SettingsItem::CancelDelete,
        SettingsItem::DeletePreset(index),
        SettingsItem::ConfirmDelete,
    ])?;
    let o = h.ws.observation();
    check(
        !dir.preset("A 2").exists() && o.preset.is_none() && o.modules.len() == 3,
        "Delete did not remove the preset or changed the workspace",
    )?;
    check(
        action(&mut h.ws, WorkspaceAction::SavePreset).is_err_and(|e| e.contains("Save as")),
        "Save to was possible without a saved preset",
    )?;
    let metrics = handle.metrics();
    Ok(
        json!({"saved":names,"final_nodes":metrics.nodes,"final_rings":metrics.rings,
        "baseline":baseline}),
    )
}

/// Restore and rescue across display sizes and scales preserve visibility and touching groups.
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
    h.desktop.take_placed();
    h.window_mut(FIRST_WINDOW)?.position = pos2(1500.0, 300.0);
    h.frame(vec![]);
    check(
        h.window(FIRST_WINDOW)?.position == pos2(1500.0, 300.0)
            && h.desktop.take_placed().is_empty()
            && !commands(&mut h)
                .iter()
                .any(|c| matches!(c, ViewportCommand::OuterPosition(_))),
        "a partly visible window was moved",
    )?;

    drop(h);

    let dir = TempDir::new("s07")?;
    let spectrum = json!({"node":"leaf","module":{"type_id":SPECTRUM_MODULE_ID}});
    // The saved monitor lies left of the current desktop and no longer exists.
    dir.write_session(&json!({"format_version":2,
        "displays":[{"work_area":[1920.0,1040.0],"hint":{"origin_px":[-1920.0,0.0],"scale":1.0}}],
        "windows":[{"display":0,"frame":[100.0,300.0,500.0,600.0],"layout":spectrum}]}))?;
    let (ws, _driver) = reopen(&dir)?;
    let mut restored = Harness::new(ws);
    let opened_at = restored.window(restored.focus)?.position;
    restored.frame(vec![]);
    let landed = restored.window(restored.focus)?.position;
    check(
        opened_at == pos2(100.0, 300.0) && landed == opened_at,
        &format!("session from a gone display: opened {opened_at:?}, now {landed:?}"),
    )?;
    drop(restored);

    // Restore touching windows from a 100% display onto a larger display at 125%.
    dir.write_session(
        &json!({"format_version":2,"displays":[{"work_area":[1920.0,1040.0]}],
        "windows":[{"display":0,"frame":[100.0,100.0,500.0,400.0],"layout":spectrum},
            {"display":0,"frame":[500.0,100.0,800.0,400.0],"layout":spectrum}]}),
    )?;
    let desktop = FakeDesktop::with_displays(vec![Display {
        work: egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(2560.0, 1400.0)),
        bounds: egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(2560.0, 1440.0)),
        scale: 1.25,
        primary: true,
    }]);
    let (live, _driver) = synthetic()?;
    let ws = Workspace::open(live, dir.store(), Arc::<FakeDesktop>::clone(&desktop))
        .map_err(|e| e.to_string())?;
    let mut scaled = Harness::with_desktop(ws, desktop);
    scaled.frame_all()?;
    let frames: Vec<egui::Rect> = scaled
        .workspace_windows()
        .map(|(_, w)| egui::Rect::from_min_size(w.position, w.size * w.ppp))
        .collect();
    check(
        frames.len() == 2
            && (frames[0].max.x - frames[1].min.x).abs() < 0.5
            && frames[0].min.y == frames[1].min.y
            && (frames[0].width() - 500.0).abs() < 0.5
            && (frames[1].width() - 375.0).abs() < 0.5,
        &format!("touching windows at 125%: {frames:?}"),
    )?;
    drop(scaled);

    let desktop = FakeDesktop::with_displays(vec![
        Display {
            work: egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(1920.0, 1040.0)),
            bounds: egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(1920.0, 1080.0)),
            scale: 1.0,
            primary: true,
        },
        Display {
            work: egui::Rect::from_min_size(pos2(1920.0, 0.0), vec2(2880.0, 1736.0)),
            bounds: egui::Rect::from_min_size(pos2(1920.0, 0.0), vec2(2880.0, 1800.0)),
            scale: 2.0,
            primary: false,
        },
    ]);
    let (ws, _driver) = workspace()?;
    let mut h = Harness::with_desktop(ws, desktop);
    h.window_mut(FIRST_WINDOW)?.position = pos2(2200.0, 300.0);
    h.frame(vec![]);
    h.new_window_ppp = 2.0;
    let leaf = first_leaf(&h.ws)?;
    h.settings(
        ControlKey::LeafBody(leaf),
        &[
            SettingsItem::Tab(Tab::Workspace),
            SettingsItem::BuiltinPreset(1),
        ],
    )?;
    h.frame_all()?;
    let o = h.ws.observation();
    let bar = o.windows.first().ok_or("no window")?;
    let window = h.window(bar.id)?;
    check(
        o.preset == Some(builtin("Bar - Top"))
            && o.windows.len() == 1
            && bar.leaves.len() == 8
            && bar.locked
            && bar.always_on_top
            && window.position == pos2(1920.0, 0.0)
            && window.size == vec2(1440.0, 112.0),
        &format!(
            "bar on the 200% display: {:?} at {:?} size {:?}",
            o.preset, window.position, window.size
        ),
    )?;
    Ok(
        json!({"rescued":[rescued.x,rescued.y],"bar":[window.position.x,window.position.y,window.size.x,window.size.y]}),
    )
}

/// Restored bindings share analysis, but settings edits fork only the edited instance.
/// Minimized and empty windows remain operable.
fn s08() -> Result<Value, String> {
    let dir = TempDir::new("s08")?;
    let spectrum = |extra: Value| {
        json!({"type_id":SPECTRUM_MODULE_ID,"data":{"nodes":[
            {"input":null,"kind":"channel","params":"mono_average"},
            {"input":0,"kind":"fft","params":{"window_size":2048,"hop_size":512,"floor_dbfs":-160.0}},
            {"input":1,"kind":"tilt","params":{"db_per_octave":4.5}},
            {"input":2,"kind":"peak","params":{"hold_ms":1000.0,"fall_db_per_second_squared":30.0}}],
            "outputs":[{"node":2,"keep":"latest"},extra]}})
    };
    let bound = json!({"node":3,"keep":"latest","binding":9});
    // The Module permits a window smaller than the empty-window minimum.
    let file = json!({"format_version":2,"theme":{"preset":"light"},"displays":[primary_display()],
        "windows":[{"display":0,"frame":[300.0,200.0,1200.0,620.0],
            "layout":{"node":"split","axis":"left_right","ratio":0.5,
                "first":{"node":"leaf","module":spectrum(bound.clone())},
                "second":{"node":"leaf","module":spectrum(bound)}}},
            {"display":0,"frame":[1300.0,200.0,1420.0,240.0],"always_on_top":true,
            "layout":{"node":"leaf","module":{"type_id":WAVEFORM_MODULE_ID}}}]});
    dir.write_session(&file)?;
    let (ws, _driver) = reopen(&dir)?;
    let handle = ws.dataflow_handle_for_automation();
    let mut h = Harness::new(ws);
    h.frame_all()?;
    settle_subscriptions(&h.ws, 3)?;
    let restored = handle.metrics().active_spectrum_nodes;
    check(
        restored == 2,
        &format!("the saved binding was not restored as one shared Peak: {restored}"),
    )?;
    let small = h.ws.observation().windows[1].id;
    let small_min =
        h.ws.windows()
            .into_iter()
            .find(|window| window.id == small)
            .and_then(|window| window.builder.min_inner_size);
    check(
        h.window(small)?.size == vec2(120.0, 40.0)
            && small_min == Some(vec2(crate::module::MIN_EXTENT, crate::module::MIN_EXTENT)),
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
    h.ws.autosave();
    let saved = serde_json::to_string(&read_json(&dir.session())?).unwrap_or_default();
    check(
        saved.matches("\"binding\":9").count() == 1,
        "the remaining binding did not keep its file number",
    )?;

    // Advance beyond the detach timeout while minimized to expose stale app-clock timing.
    h.window_mut(window)?.minimized = true;
    h.frame_window(window, vec![])?;
    h.display_time += Duration::from_secs(10);
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

    h.in_settings(&[
        SettingsItem::Tab(Tab::Workspace),
        SettingsItem::BuiltinPreset(0),
    ])?;
    h.frame_all()?;
    let o = h.ws.observation();
    check(
        o.preset == Some(builtin("Default"))
            && o.windows.len() == 1
            && o.windows[0].always_on_top
            && o.modules.len() == 1
            && o.modules[0].type_id == OSCILLOSCOPE_MODULE_ID
            && o.theme.preset == "dark"
            && o.theme.colors.is_empty(),
        &format!("Default did not replace the workspace: {:?}", o.preset),
    )?;
    settle_subscriptions(&h.ws, 1)?;
    let new_window = o.windows[0].id;
    let position = h.window(new_window)?.position;
    check(
        position == pos2(720.0, 400.0)
            && h.window(new_window)?.size == vec2(480.0, 240.0)
            && h.ws.settings_open(),
        &format!("Default was not centered: {position:?}"),
    )?;

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
                ViewportCommand::WindowLevel(egui::WindowLevel::Normal)
            )
        }) && !h.ws.observation().windows[0].always_on_top,
        "a minimized window did not receive the settings window's command",
    )?;
    h.window_mut(new_window)?.minimized = false;
    h.frame_window(new_window, vec![])?;

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
    Ok(json!({"restored_spectrum_nodes":restored,"default_position":[position.x,position.y]}))
}

/// The boolean reports whether the outline coincides with the module's leaf.
fn outlined(h: &Harness, window: WindowId) -> Option<(ModuleInstanceId, bool)> {
    let o = h.ws.observation();
    let w = o.window(window)?;
    let (id, rect) = w.module_outline?;
    let leaf = w.leaf_rects.iter().find(|(leaf, _)| *leaf == id)?.1;
    let exact = [
        rect.min.x - leaf.min.x,
        rect.min.y - leaf.min.y,
        rect.max.x - leaf.max.x,
        rect.max.y - leaf.max.y,
    ]
    .iter()
    .all(|d| d.abs() <= 0.5);
    Some((id, exact))
}

/// Raw input allows overlays to intercept selection; the empty-window point avoids Add module.
fn select(h: &mut Harness, window: WindowId, body: ControlKey) -> Result<(), String> {
    h.focus = window;
    h.frame(vec![]);
    let rect = h.locate(&body)?.rect;
    let pos = if body == ControlKey::EmptyBody {
        rect.min + rect.size() * 0.25
    } else {
        rect.center()
    };
    let button = |pressed| Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.frame(vec![Event::PointerMoved(pos)]);
    h.frame(vec![button(true)]);
    h.frame(vec![button(false)]);
    h.frame(vec![]);
    if h.windows.contains_key(&SETTINGS) {
        h.frame_window(SETTINGS, vec![])?;
    }
    h.frame_window(window, vec![])?;
    Ok(())
}

/// Selection and outlines follow settings tabs without leaking edits across modules or overlays.
fn s09() -> Result<Value, String> {
    let (mut ws, _driver) = workspace()?;
    let a = first_leaf(&ws)?;
    let b = created(action(
        &mut ws,
        WorkspaceAction::SplitModule {
            instance_id: a,
            side: Side::Right,
        },
    )?)?;
    let second = new_window(action(
        &mut ws,
        WorkspaceAction::NewWindow {
            window_id: FIRST_WINDOW,
        },
    )?)?;
    let c = created(action(
        &mut ws,
        WorkspaceAction::AddModule {
            window_id: second,
            type_id: SPECTRUM_MODULE_ID,
        },
    )?)?;
    let empty = new_window(action(
        &mut ws,
        WorkspaceAction::NewWindow {
            window_id: FIRST_WINDOW,
        },
    )?)?;
    let mut h = Harness::new(ws);
    h.frame_all()?;
    let target = |h: &Harness| h.ws.observation().settings_target;

    select(&mut h, FIRST_WINDOW, ControlKey::LeafBody(b))?;
    check(
        !h.ws.settings_open() && outlined(&h, FIRST_WINDOW).is_none(),
        "a left click without the settings window selected or opened something",
    )?;

    open_settings(&mut h, a, &[])?;
    h.frame_window(FIRST_WINDOW, vec![])?;
    check(
        outlined(&h, FIRST_WINDOW) == Some((a, true)),
        &format!("Module tab outline: {:?}", outlined(&h, FIRST_WINDOW)),
    )?;
    h.take_commands(SETTINGS);
    select(&mut h, FIRST_WINDOW, ControlKey::LeafBody(b))?;
    let focused = h.take_commands(SETTINGS).contains(&ViewportCommand::Focus);
    check(
        target(&h) == Some((Some(b), Some(FIRST_WINDOW)))
            && outlined(&h, FIRST_WINDOW) == Some((b, true))
            && !focused,
        &format!(
            "left click in the same window: target {:?}, outline {:?}, focused {focused}",
            target(&h),
            outlined(&h, FIRST_WINDOW)
        ),
    )?;
    select(&mut h, second, ControlKey::LeafBody(c))?;
    h.frame_window(FIRST_WINDOW, vec![])?;
    check(
        target(&h) == Some((Some(c), Some(second)))
            && outlined(&h, second) == Some((c, true))
            && outlined(&h, FIRST_WINDOW).is_none(),
        &format!(
            "left click in another window: target {:?}, outlines {:?} / {:?}",
            target(&h),
            outlined(&h, second),
            outlined(&h, FIRST_WINDOW)
        ),
    )?;
    select(&mut h, empty, ControlKey::EmptyBody)?;
    check(
        target(&h) == Some((Some(c), Some(second))),
        "an empty window replaced the Module tab's module",
    )?;

    h.in_settings(&[SettingsItem::Tab(Tab::Window)])?;
    select(&mut h, FIRST_WINDOW, ControlKey::LeafBody(a))?;
    let window_outline =
        h.ws.observation()
            .window(FIRST_WINDOW)
            .is_some_and(|w| w.edges.iter().any(|e| e.line == EdgeLine::Outline));
    check(
        target(&h) == Some((Some(a), Some(FIRST_WINDOW)))
            && window_outline
            && outlined(&h, FIRST_WINDOW).is_none(),
        &format!(
            "Window tab after a left click: target {:?}, window outline {window_outline}, module outline {:?}",
            target(&h),
            outlined(&h, FIRST_WINDOW)
        ),
    )?;
    select(&mut h, empty, ControlKey::EmptyBody)?;
    check(
        target(&h) == Some((None, Some(empty))),
        &format!(
            "Window tab after clicking the empty window: {:?}",
            target(&h)
        ),
    )?;

    h.in_settings(&[SettingsItem::Tab(Tab::Workspace)])?;
    select(&mut h, FIRST_WINDOW, ControlKey::LeafBody(b))?;
    select(&mut h, empty, ControlKey::EmptyBody)?;
    let stayed =
        h.ws.settings_targets()
            .contains_key(&ControlKey::Settings(SettingsItem::Pause));
    h.in_settings(&[SettingsItem::Tab(Tab::Module)])?;
    h.frame_window(FIRST_WINDOW, vec![])?;
    check(
        stayed && outlined(&h, FIRST_WINDOW) == Some((b, true)),
        &format!(
            "Workspace tab: stayed {stayed}, Module tab then outlines {:?}",
            outlined(&h, FIRST_WINDOW)
        ),
    )?;

    action(
        &mut h.ws,
        WorkspaceAction::SetLocked {
            window_id: second,
            enabled: true,
        },
    )?;
    select(&mut h, second, ControlKey::LeafBody(c))?;
    check(
        target(&h) == Some((Some(b), Some(FIRST_WINDOW))),
        "a locked window took a selection",
    )?;
    action(
        &mut h.ws,
        WorkspaceAction::SetLocked {
            window_id: second,
            enabled: false,
        },
    )?;

    let max_frequency = |h: &Harness, id| {
        h.ws.observation().module(id).and_then(|m| {
            m.view
                .iter()
                .find(|(k, _)| *k == "max_frequency_hz")
                .cloned()
        })
    };
    let before = (max_frequency(&h, a), max_frequency(&h, b));
    h.in_settings(&[])?;
    h.click(ControlKey::SettingsValue(SettingsItem::ViewNumber(
        "max_frequency_hz",
    )))?;
    h.frame(vec![Event::Text("1.5k".to_owned())]);
    select(&mut h, FIRST_WINDOW, ControlKey::LeafBody(a))?;
    h.focus = SETTINGS;
    h.key(egui::Key::Enter);
    h.frame(vec![]);
    let after = (max_frequency(&h, a), max_frequency(&h, b));
    check(
        target(&h) == Some((Some(a), Some(FIRST_WINDOW))) && after == before,
        &format!("a typed value crossed to another module: {before:?} -> {after:?}"),
    )?;

    h.in_settings(&[SettingsItem::MoveTo, SettingsItem::Tab(Tab::Theme)])?;
    select(&mut h, FIRST_WINDOW, ControlKey::LeafBody(b))?;
    let picking = (outlined(&h, FIRST_WINDOW), target(&h));
    action(&mut h.ws, WorkspaceAction::CloseSettings)?;
    h.frame_window(FIRST_WINDOW, vec![])?;
    let closed = outlined(&h, FIRST_WINDOW);
    h.key(egui::Key::Escape);
    h.frame(vec![]);
    check(
        picking == (Some((a, true)), Some((Some(a), Some(FIRST_WINDOW))))
            && closed == Some((a, true))
            && outlined(&h, FIRST_WINDOW).is_none()
            && h.ws.observation().move_source.is_none(),
        &format!(
            "move outline: picking {picking:?}, settings closed {closed:?}, after Escape {:?}",
            outlined(&h, FIRST_WINDOW)
        ),
    )?;
    h.ws.check_ownership()?;
    Ok(
        json!({"module_tab":"follows left clicks","window_tab":"clicked window",
        "workspace_tab":"stays","locked":"no selection","typed_value":"dropped",
        "move":"outlines the moving module"}),
    )
}

/// The session is written once a change stops changing and never while it stays the same.
fn s10() -> Result<Value, String> {
    let dir = TempDir::new("s10")?;
    let (ws, _driver) = workspace()?;
    let mut h = Harness::new(ws.with_store(dir.store()));
    h.frame(vec![]);
    let a = first_leaf(&h.ws)?;
    let pass = |h: &mut Harness, after: Duration| {
        h.display_time += after;
        h.ws.tick(h.display_time);
        h.ws.autosave_when_settled()
    };
    let second = Duration::from_secs(1);
    let first = pass(&mut h, Duration::ZERO);
    let between = pass(&mut h, second / 2);
    check(
        first == Some(second) && between == Some(second / 2) && !dir.session().exists(),
        &format!("first change: {first:?}, then {between:?}"),
    )?;
    let settled = pass(&mut h, second / 2);
    check(
        settled.is_none() && dir.session().exists(),
        "an unchanged session was not written after one check interval",
    )?;

    std::fs::remove_file(dir.session()).map_err(|e| format!("{}: {e}", dir.session().display()))?;
    let steady: Vec<_> = (0..3).map(|_| pass(&mut h, second)).collect();
    check(
        steady.iter().all(Option::is_none) && !dir.session().exists(),
        &format!("a steady session was written again: {steady:?}"),
    )?;

    action(
        &mut h.ws,
        WorkspaceAction::SetStyle {
            instance_id: a,
            style_id: BARS_SPECTRUM_STYLE_ID,
        },
    )?;
    let seen = pass(&mut h, second);
    action(
        &mut h.ws,
        WorkspaceAction::SetViewSetting {
            instance_id: a,
            key: "max_frequency_hz",
            value: SettingValue::Number(10_000.0),
        },
    )?;
    let still_changing = pass(&mut h, second);
    check(
        seen == Some(second) && still_changing == Some(second) && !dir.session().exists(),
        "a session still being edited was written",
    )?;
    let written = pass(&mut h, second);
    let saved = read_json(&dir.session())?;
    let module = &saved["windows"][0]["layout"]["module"];
    check(
        written.is_none()
            && module["style"] == BARS_SPECTRUM_STYLE_ID
            && module["view"]["max_frequency_hz"]
                .to_string()
                .contains("10000"),
        &format!("the settled edits were not written: {module}"),
    )?;
    h.ws.check_ownership()?;
    Ok(
        json!({"interval_seconds":1,"first_write":"after one unchanged check",
        "steady":"no writes","edits":"written once they stop"}),
    )
}

/// The picker's hex and channel fields commit typed values; a gray keeps the hue it
/// was picked from.
fn s11() -> Result<Value, String> {
    use crate::render::Color;

    let (ws, _driver) = workspace()?;
    let mut h = Harness::new(ws);
    h.frame(vec![]);
    let a = first_leaf(&h.ws)?;
    let part = |part| {
        ControlKey::Settings(SettingsItem::ColorPicker {
            scope: ColorScope::Instance(a),
            role: ColorRole::Primary,
            part,
        })
    };
    let primary = |h: &Harness| {
        h.ws.observation()
            .module(a)
            .map(|m| m.palette.color(ColorRole::Primary))
    };
    let replace_text = |h: &mut Harness, key: ControlKey, text: &str, end: egui::Key| {
        h.click(key)?;
        h.key_with(egui::Key::A, egui::Modifiers::COMMAND);
        h.frame(vec![Event::Text(text.to_owned())]);
        h.key(end);
        h.frame(vec![]);
        Ok::<_, String>(())
    };
    open_settings(&mut h, a, &[])?;
    let original = primary(&h);
    h.click(part(PickerPart::Button))?;

    replace_text(&mut h, part(PickerPart::Hex), "89b4fa80", egui::Key::Enter)?;
    let blue = Color::rgba(0x89, 0xb4, 0xfa, 0x80);
    check(
        primary(&h) == Some(blue) && original != Some(blue),
        &format!("hex without # was not applied: {:?}", primary(&h)),
    )?;
    replace_text(&mut h, part(PickerPart::Hex), "#+1+2+3", egui::Key::Enter)?;
    check(
        primary(&h) == Some(blue),
        &format!("an invalid hex changed the color: {:?}", primary(&h)),
    )?;
    replace_text(&mut h, part(PickerPart::Channel(0)), "10", egui::Key::Enter)?;
    let edited = Color::rgba(10, 0xb4, 0xfa, 0x80);
    check(
        primary(&h) == Some(edited),
        &format!("the red field did not edit only red: {:?}", primary(&h)),
    )?;
    let alpha = h.locate(&part(PickerPart::Alpha))?.rect;
    h.drag(
        part(PickerPart::Alpha),
        &[egui::pos2(alpha.right() + 20.0, alpha.center().y)],
        egui::Modifiers::NONE,
        None,
    )?;
    check(
        primary(&h)
            == Some(Color {
                alpha: 255,
                ..edited
            }),
        &format!("the alpha bar changed more than alpha: {:?}", primary(&h)),
    )?;

    // Through premultiplied RGB, a zero alpha would also zero the hue.
    replace_text(&mut h, part(PickerPart::Channel(3)), "0", egui::Key::Enter)?;
    check(
        primary(&h) == Some(Color { alpha: 0, ..edited }),
        &format!("the alpha field changed more than alpha: {:?}", primary(&h)),
    )?;
    let plane = h.locate(&part(PickerPart::Plane))?.rect;
    let middle = plane.center();
    h.drag(
        part(PickerPart::Plane),
        &[egui::pos2(plane.left() - 20.0, middle.y)],
        egui::Modifiers::NONE,
        None,
    )?;
    let gray = primary(&h).ok_or("instance missing")?;
    h.drag(
        part(PickerPart::Plane),
        &[egui::pos2(plane.right() + 20.0, middle.y)],
        egui::Modifiers::NONE,
        None,
    )?;
    let resaturated = primary(&h).ok_or("instance missing")?;
    check(
        gray.red == gray.green
            && gray.green == gray.blue
            && resaturated.blue > resaturated.red
            && resaturated.alpha == 0,
        &format!("gray {gray:?} lost the blue hue: {resaturated:?}"),
    )?;
    h.ws.check_ownership()?;
    Ok(
        json!({"hex":"accepted without #, invalid ignored","channel":"red only",
        "gray":"keeps its hue"}),
    )
}

/// Switching inputs rebuilds every instance in place for the new format, keeps the
/// configurations that no longer fit, and leaves nothing running on the old source.
fn s12() -> Result<Value, String> {
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::{
        LiveAudio, LiveAudioConfig,
        audio::synthetic::{SignalSpec, SyntheticConfig},
        audio::{AudioDescriptor, ChannelLayout, Generation, SourceId, SourceState},
        persistence::InputConfig,
    };

    let source = |generation: u64, rate: u32, layout: ChannelLayout| {
        let channels = if layout == ChannelLayout::Mono { 1 } else { 2 };
        let tone = SignalSpec::Sine {
            frequency_hz: 1_000.0,
            amplitude: 0.1,
            phase_radians: 0.0,
        };
        let config = SyntheticConfig {
            descriptor: AudioDescriptor::new(
                SourceId(1),
                Generation(generation),
                rate,
                channels,
                layout,
            )
            .map_err(|e| e.to_string())?,
            signals: vec![tone; usize::from(channels)],
            ..SyntheticConfig::default()
        };
        LiveAudio::open_manual(LiveAudioConfig::default(), config).map_err(|e| e.to_string())
    };
    let (ws, mut first_driver) = workspace()?;
    let mut h = Harness::new(ws);
    let wakes = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&wakes);
    h.ws.set_publication_waker(Some(Box::new(move || {
        counted.fetch_add(1, Ordering::Relaxed);
    })));
    h.frame(vec![]);
    let left = first_leaf(&h.ws)?;
    action(
        &mut h.ws,
        WorkspaceAction::SetChannel {
            instance_id: left,
            channel: ChannelSelection::Left,
        },
    )?;
    let copy = created(action(
        &mut h.ws,
        WorkspaceAction::SplitModule {
            instance_id: left,
            side: Side::Right,
        },
    )?)?;
    let picture = created(action(
        &mut h.ws,
        WorkspaceAction::ReplaceModule {
            instance_id: copy,
            type_id: crate::modules::SPECTROGRAM_MODULE_ID,
        },
    )?)?;
    let tint = swatches(true)[2];
    action(
        &mut h.ws,
        WorkspaceAction::SetColor {
            scope: ColorScope::Instance(picture),
            role: ColorRole::Primary,
            color: Some(tint),
        },
    )?;
    action(&mut h.ws, WorkspaceAction::TogglePause)?;
    open_settings(&mut h, picture, &[])?;
    let layout = leaves(&h.ws, FIRST_WINDOW);

    let (mono, mut mono_driver) = source(2, 44_100, ChannelLayout::Mono)?;
    h.ws.replace_source(mono);
    h.frame(vec![]);
    let o = h.ws.observation();
    let descriptor = h.ws.source_descriptor_for_automation().clone();
    check(
        leaves(&h.ws, FIRST_WINDOW) == layout
            && h.ws.settings_open()
            && (
                descriptor.sample_rate_hz,
                descriptor.channel_count,
                descriptor.generation,
            ) == (44_100, 1, Generation(2))
            && o.source_status.state == SourceState::Paused,
        &format!(
            "after the switch: layout {:?} (was {layout:?}), source {descriptor:?}, {:?}",
            leaves(&h.ws, FIRST_WINDOW),
            o.source_status.state
        ),
    )?;
    let module = |h: &Harness, id: ModuleInstanceId| {
        h.ws.observation()
            .module(id)
            .cloned()
            .ok_or_else(|| format!("instance {} missing", id.0))
    };
    let (kept, rebuilt) = (module(&h, left)?, module(&h, picture)?);
    check(
        kept.missing.as_deref() == Some(SPECTRUM_MODULE_ID)
            && rebuilt.type_id == crate::modules::SPECTROGRAM_MODULE_ID
            && rebuilt.palette.color(ColorRole::Primary) == tint,
        &format!(
            "mono input: Left Spectrum {:?}/{:?}, Spectrogram {} with {:?}",
            kept.type_id,
            kept.missing,
            rebuilt.type_id,
            rebuilt.palette.color(ColorRole::Primary)
        ),
    )?;
    check(
        first_driver.advance_frames(160, deadline()).is_err(),
        "the replaced source still accepts input",
    )?;

    action(&mut h.ws, WorkspaceAction::TogglePause)?;
    let woken = wakes.load(Ordering::Relaxed);
    mono_driver
        .advance_frames(44_100, deadline())
        .map_err(|e| e.to_string())?;
    let metrics = h.ws.dataflow_handle_for_automation().metrics();
    check(
        metrics.fft_evaluations > 0 && wakes.load(Ordering::Relaxed) > woken,
        &format!(
            "the new source did not analyze or wake the UI: {} FFTs, wakes {woken} -> {}",
            metrics.fft_evaluations,
            wakes.load(Ordering::Relaxed)
        ),
    )?;

    let (stereo, _stereo_driver) = source(3, 48_000, ChannelLayout::Stereo)?;
    h.ws.replace_source(stereo);
    h.frame(vec![]);
    let o = h.ws.observation();
    let (restored, rebuilt) = (module(&h, left)?, module(&h, picture)?);
    check(
        restored.type_id == SPECTRUM_MODULE_ID
            && restored.missing.is_none()
            && restored.channel == Some(ChannelSelection::Left)
            && rebuilt.palette.color(ColorRole::Primary) == tint,
        &format!(
            "stereo again: Spectrum {}/{:?} on {:?}",
            restored.type_id, restored.missing, restored.channel
        ),
    )?;

    let objects: Vec<usize> = o.modules.iter().map(|m| m.object).collect();
    let unknown = InputConfig {
        kind: "vendor.file".to_owned(),
        options: BTreeMap::new(),
    };
    let rejected =
        h.ws.apply_action(WorkspaceAction::SetInput { input: unknown });
    let after: Vec<usize> =
        h.ws.observation()
            .modules
            .iter()
            .map(|m| m.object)
            .collect();
    check(
        rejected.is_err()
            && after == objects
            && h.ws.source_descriptor_for_automation().generation == Generation(3),
        &format!("an unsupported input changed the workspace: {rejected:?}"),
    )?;
    h.ws.check_ownership()?;
    Ok(
        json!({"mono":"Left Spectrum kept as placeholder, others rebuilt",
        "stereo":"Left Spectrum restored","paused":"kept","old_source":"stopped",
        "unsupported":"rejected without changes"}),
    )
}

/// A lost source switches to the default device of its own group, keeps the chosen
/// input, and retries every two seconds while nothing opens.
fn s13() -> Result<Value, String> {
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };

    use crate::{
        LiveAudio, LiveAudioConfig,
        audio::{AudioDescriptor, Generation, SourceState, synthetic::SyntheticConfig},
        live::LiveAudioError,
        persistence::{CAPTURE_INPUT, DEFAULT_CAPTURE_INPUT, DEFAULT_PLAYBACK_INPUT, InputConfig},
    };

    let (ws, _driver) = workspace()?;
    let mut h = Harness::new(ws);
    let requests = Arc::new(Mutex::new(Vec::<String>::new()));
    let refuse = Arc::new(AtomicBool::new(false));
    let (seen, refused) = (Arc::clone(&requests), Arc::clone(&refuse));
    h.ws.set_input_opener(Box::new(move |input, generation| {
        if let Ok(mut seen) = seen.lock() {
            seen.push(input.kind.clone());
        }
        if refused.load(Ordering::Relaxed) {
            return Err(LiveAudioError::Input("no device".to_owned()));
        }
        let defaults = SyntheticConfig::default();
        let d = &defaults.descriptor;
        let config = SyntheticConfig {
            descriptor: AudioDescriptor::new(
                d.source_id,
                generation,
                d.sample_rate_hz,
                d.channel_count,
                d.channel_layout,
            )
            .expect("only the generation differs from a valid descriptor"),
            ..defaults
        };
        LiveAudio::open_manual(LiveAudioConfig::default(), config).map(|(live, _)| live)
    }));
    let taken = |requests: &Mutex<Vec<String>>| {
        requests
            .lock()
            .map(|mut seen| std::mem::take(&mut *seen))
            .unwrap_or_default()
    };
    h.frame(vec![]);
    let leaf = first_leaf(&h.ws)?;
    let microphone = InputConfig {
        kind: CAPTURE_INPUT.to_owned(),
        options: BTreeMap::from([("device".to_owned(), json!("wasapi:mic"))]),
    };
    action(
        &mut h.ws,
        WorkspaceAction::SetInput {
            input: microphone.clone(),
        },
    )?;
    action(&mut h.ws, WorkspaceAction::TogglePause)?;
    taken(&requests);
    let state = |h: &Harness| h.ws.observation().source_status.state;

    refuse.store(true, Ordering::Relaxed);
    h.ws.lose_device_for_automation();
    h.frame(vec![]);
    let first_attempt = taken(&requests);
    h.display_time += Duration::from_secs(1);
    h.frame(vec![]);
    let too_soon = taken(&requests);
    check(
        first_attempt == [DEFAULT_CAPTURE_INPUT, DEFAULT_PLAYBACK_INPUT]
            && too_soon.is_empty()
            && state(&h) == SourceState::DeviceLost
            && h.ws
                .observation()
                .notices
                .iter()
                .any(|notice| notice.contains("no other device opened")),
        &format!(
            "while nothing opens: tried {first_attempt:?}, then {too_soon:?}, state {:?}",
            state(&h)
        ),
    )?;

    refuse.store(false, Ordering::Relaxed);
    h.display_time += Duration::from_millis(1_100);
    h.frame(vec![]);
    let retried = taken(&requests);
    let o = h.ws.observation();
    check(
        retried == [DEFAULT_CAPTURE_INPUT]
            && state(&h) == SourceState::Paused
            && h.ws.source_descriptor_for_automation().generation == Generation(3)
            && *h.ws.input() == microphone
            && o.module(leaf).is_some_and(|m| m.missing.is_none())
            && o.notices
                .iter()
                .any(|notice| notice.contains("lost; switched to")),
        &format!(
            "retry: tried {retried:?}, state {:?}, input {:?}, notices {:?}",
            state(&h),
            h.ws.input(),
            o.notices
        ),
    )?;
    h.display_time += Duration::from_secs(5);
    h.frame(vec![]);
    check(
        taken(&requests).is_empty(),
        "a recovered source kept retrying",
    )?;
    h.ws.check_ownership()?;
    Ok(json!({"order":"default microphone, then default playback",
        "retry_seconds":2,"choice":"kept","paused":"kept"}))
}
