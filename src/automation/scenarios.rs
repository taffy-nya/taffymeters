use std::{
    path::Path,
    time::{Duration, Instant},
};

use eframe::egui;
use serde_json::{Value, json};

use crate::{
    LiveAudio, LiveAudioConfig,
    audio::{
        ChannelLayout, SourceState,
        synthetic::{ManualInputDriver, SignalSpec, SyntheticConfig},
    },
    builtins::{EMPTY_MODULE_ID, LEVEL_METER_MODULE_ID, SPECTRUM_MODULE_ID, WAVEFORM_MODULE_ID},
    dataflow::{ChannelSelection, DataSpec, FftConfig, Keep, NodeSpec, OutputId, SOURCE},
    workspace::{
        ActionOutcome, ControlKey, FIRST_WINDOW, MenuItem, ModuleInstanceId, Side, Workspace,
        WorkspaceAction,
    },
};

use super::{
    deadline,
    harness::Harness,
    report::{Case, Report, Status},
    visual, wait_until,
};

pub(super) fn check(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}
fn synthetic() -> Result<(LiveAudio, ManualInputDriver), String> {
    LiveAudio::open_manual(LiveAudioConfig::default(), SyntheticConfig::default())
        .map_err(|e| e.to_string())
}
/// Merged source PCM kept as `keep`.
fn pcm_spec(keep: Keep) -> (DataSpec, OutputId) {
    let mut spec = DataSpec::new();
    let output = spec.output(SOURCE, keep);
    (spec, output)
}
/// One channel's 2048/512 FFT, latest result only.
fn fft_spec(channel: ChannelSelection) -> Result<(DataSpec, OutputId), String> {
    let mut spec = DataSpec::new();
    let channel = spec.add(SOURCE, NodeSpec::Channel(channel));
    let fft = spec.add(
        channel,
        NodeSpec::Fft(FftConfig::new(2048, 512, -160.0).map_err(|e| e.to_string())?),
    );
    let output = spec.output(fft, Keep::Latest);
    Ok((spec, output))
}
fn bins(frame: &crate::snapshot::Frame) -> &[f32] {
    frame
        .spectrum()
        .map_or(&[], |data| data.bins_dbfs.as_slice())
}
fn pcm(frame: &crate::snapshot::Frame) -> &[f32] {
    frame
        .pcm()
        .map_or(&[], |chunk| chunk.interleaved.as_slice())
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
fn run_case(id: &str, f: impl FnOnce() -> Result<Value, String>) -> Case {
    let started = Instant::now();
    match super::with_scenario_deadline(f) {
        Ok(observed) if started.elapsed() <= Duration::from_secs(30) => Case::pass(id, observed),
        Ok(_) => Case::fail(id, "scenario deadline", "exceeded 30 seconds"),
        Err(error) => Case::fail(id, "behavior", error),
    }
}

pub(super) fn core(report: &mut Report) {
    report.cases.push(run_case("C01", c01));
    report.cases.push(run_case("C02", c02));
    report.cases.push(run_case("C03", c03));
    report.cases.push(run_case("C04", c04));
    report.cases.push(run_case("C05", c05));
    report.cases.push(run_case("C06", c06));
    report.cases.push(run_case("C07", c07));
    report.cases.push(run_case("C08", c08));
    report.cases.push(run_case("C09", c09));
    report.cases.push(run_case("C10", c10));
    for (id, scenario) in super::workspace_scenarios::CASES {
        report.cases.push(run_case(id, scenario));
    }
}

fn c01() -> Result<Value, String> {
    let (live, mut driver) = synthetic()?;
    let handle = live.dataflow_handle();
    let before = handle.metrics();
    let end = driver
        .advance_frames(4800, deadline())
        .map_err(|e| e.to_string())?;
    check(
        end == 4800
            && handle
                .processed_through_frame()
                .map_err(|e| e.to_string())?
                >= end,
        "zero-demand frontier",
    )?;
    let after = handle.metrics();
    check(after == before, "zero-demand analysis work changed")?;
    let status = live.reader().source_status();
    check(
        status.dropped_blocks == 0 && status.invalid_samples == 0,
        "unexpected transport drop/invalid",
    )?;
    drop(live);
    check(
        handle.subscribe(pcm_spec(Keep::Latest).0).is_err(),
        "stopped handle accepted subscription",
    )?;
    Ok(json!({"frontier":end,"analysis": "unchanged", "dropped": status.dropped_blocks}))
}

fn fft_run(block_frames: usize) -> Result<(f32, f32, u64), String> {
    let cfg = SyntheticConfig {
        block_frames,
        ..SyntheticConfig::default()
    };
    let (live, mut driver) =
        LiveAudio::open_manual(LiveAudioConfig::default(), cfg).map_err(|e| e.to_string())?;
    let handle = live.dataflow_handle();
    let (left, output) = fft_spec(ChannelSelection::Left)?;
    let (right, _) = fft_spec(ChannelSelection::Right)?;
    let a = handle.subscribe(left).map_err(|e| e.to_string())?;
    let b = handle.subscribe(right).map_err(|e| e.to_string())?;
    driver
        .advance_frames(4096, deadline())
        .map_err(|e| e.to_string())?;
    let ra = a.reader();
    let rb = b.reader();
    let sa = wait_until(deadline(), "left FFT snapshot", || {
        ra.latest(output)
            .filter(|s| s.processed_through_frame >= 4096)
    })?;
    let sb = wait_until(deadline(), "right FFT snapshot", || {
        rb.latest(output)
            .filter(|s| s.processed_through_frame >= 4096)
    })?;
    let (left_bins, right_bins) = (bins(&sa), bins(&sb));
    check(
        left_bins[128] > left_bins[127] && right_bins[256] > right_bins[255],
        "wrong tone bins",
    )?;
    let expected = 20.0 * 0.08_f32.log10();
    check(
        (left_bins[128] - expected).abs() <= 0.02 && (right_bins[256] - expected).abs() <= 0.02,
        "tone calibration error",
    )?;
    // No new audio: repeated reads spread over several 30 Hz publication periods of
    // wall time must not advance the result version (UI-only display-time advance on a
    // real App is covered by C07).
    for _ in 0..6 {
        std::thread::sleep(Duration::from_millis(40));
        check(
            ra.latest(output)
                .is_some_and(|s| s.result_version == sa.result_version)
                && rb
                    .latest(output)
                    .is_some_and(|s| s.result_version == sb.result_version),
            "reading unchanged input advanced result version",
        )?;
    }
    let evaluations = handle.metrics().fft_evaluations;
    Ok((left_bins[128], right_bins[256], evaluations))
}

fn c02() -> Result<Value, String> {
    let first = fft_run(160)?;
    let second = fft_run(257)?;
    check(
        (first.0 - second.0).abs() <= 0.02 && (first.1 - second.1).abs() <= 0.02,
        "phase reset at block boundary",
    )?;
    let (live, mut driver) = synthetic()?;
    let (spec, output) = pcm_spec(Keep::Latest);
    let subscription = live
        .dataflow_handle()
        .subscribe(spec)
        .map_err(|e| e.to_string())?;
    let mut samples = Vec::with_capacity(3200);
    for n in 0..1600 {
        let value = ((n % 1024) as f32 - 512.0) / 1024.0;
        samples.extend([value, -value]);
    }
    driver
        .push_pcm(&samples, deadline())
        .map_err(|e| e.to_string())?;
    let reader = subscription.reader();
    let snapshot = wait_until(deadline(), "indexed PCM snapshot", || {
        reader
            .latest(output)
            .filter(|s| s.processed_through_frame >= 1600)
    })?;
    check(
        pcm(&snapshot) == samples.as_slice()
            && snapshot.pcm().is_some_and(|chunk| chunk.start_frame == 0),
        "indexed PCM order/channel mismatch",
    )?;
    let signals = SyntheticConfig {
        signals: vec![
            SignalSpec::Impulse {
                at_frame: 73,
                amplitude: 0.75,
            },
            SignalSpec::Indexed { offset: 0.05 },
        ],
        ..SyntheticConfig::default()
    };
    let (generated, mut generated_driver) =
        LiveAudio::open_manual(LiveAudioConfig::default(), signals).map_err(|e| e.to_string())?;
    let generated_subscription = generated
        .dataflow_handle()
        .subscribe(pcm_spec(Keep::Latest).0)
        .map_err(|e| e.to_string())?;
    generated_driver
        .advance_frames(1600, deadline())
        .map_err(|e| e.to_string())?;
    let generated_reader = generated_subscription.reader();
    let generated_snapshot = wait_until(deadline(), "impulse and indexed generator", || {
        generated_reader
            .latest(output)
            .filter(|s| s.processed_through_frame >= 1600)
    })?;
    let generated_samples = pcm(&generated_snapshot);
    check(
        generated_samples.len() == 3200
            && generated_samples[73 * 2] == 0.75
            && generated_samples[72 * 2] == 0.0
            && (generated_samples[73 * 2 + 1] - ((73.0 - 512.0) / 1024.0 + 0.05)).abs() < 1e-6,
        "synthetic impulse/indexed source did not preserve frame or channel",
    )?;
    Ok(
        json!({"left_dbfs": first.0,"right_dbfs":first.1,"alternate_left_dbfs":second.0,"fft_evaluations": first.2,"pcm_frames":1600}),
    )
}

fn c03() -> Result<Value, String> {
    let (mut live, mut driver) = synthetic()?;
    let handle = live.dataflow_handle();
    let (spec, output) = pcm_spec(Keep::Seconds(1.0));
    let history = handle.subscribe(spec).map_err(|e| e.to_string())?;
    driver
        .advance_frames(1600, deadline())
        .map_err(|e| e.to_string())?;
    live.set_paused(true).map_err(|e| e.to_string())?;
    check(
        live.reader().source_status().state == SourceState::Paused,
        "pause not observable",
    )?;
    check(
        driver.advance_frames(160, deadline()).is_err(),
        "paused advance accepted",
    )?;
    check(
        handle
            .processed_through_frame()
            .map_err(|e| e.to_string())?
            == 1600,
        "pause advanced frame",
    )?;
    live.set_paused(true).map_err(|e| e.to_string())?;
    live.set_paused(false).map_err(|e| e.to_string())?;
    live.set_paused(false).map_err(|e| e.to_string())?;
    driver
        .advance_frames(1600, deadline())
        .map_err(|e| e.to_string())?;
    let snapshot = wait_until(deadline(), "resume history", || {
        history
            .reader()
            .latest(output)
            .filter(|s| s.processed_through_frame >= 3200)
    })?;
    check(
        snapshot.discontinuity_count == 1,
        &format!(
            "pause/repeat-pause/resume/repeat-resume marked {} discontinuities, expected 1",
            snapshot.discontinuity_count
        ),
    )?;
    drop(live);
    check(
        driver.advance_frames(160, deadline()).is_err()
            && handle.processed_through_frame().is_err(),
        "old driver/handle survived stop",
    )?;
    let realtime = realtime_smoke()?;
    Ok(json!({"frontier":3200,"discontinuities":snapshot.discontinuity_count,"realtime":realtime}))
}

/// Bounded smoke of the wall-clock synthetic producer: frames advance with time,
/// stop while paused, resume, and the producer is joined on drop. Wall-clock sample
/// accuracy is not a pass threshold.
fn realtime_smoke() -> Result<Value, String> {
    let frontier = |handle: &crate::dataflow::DataflowHandle| {
        handle.processed_through_frame().map_err(|e| e.to_string())
    };
    let mut live = LiveAudio::open_realtime(LiveAudioConfig::default(), SyntheticConfig::default())
        .map_err(|e| e.to_string())?;
    let handle = live.dataflow_handle();
    let started = Instant::now();
    wait_until(deadline(), "realtime frames to advance", || {
        frontier(&handle).ok().filter(|frame| *frame >= 4_800)
    })?;
    live.set_paused(true).map_err(|e| e.to_string())?;
    check(
        live.reader().source_status().state == SourceState::Paused,
        "realtime pause not observable",
    )?;
    // Pause returns only after the producer stopped writing; wait for the worker to
    // drain what was already queued, then confirm nothing new arrives.
    let mut paused_at = frontier(&handle)?;
    let settle_deadline = deadline();
    loop {
        std::thread::sleep(Duration::from_millis(20));
        let now = frontier(&handle)?;
        if now == paused_at {
            break;
        }
        check(
            Instant::now() < settle_deadline,
            "Timeout waiting for paused realtime frontier to settle",
        )?;
        paused_at = now;
    }
    std::thread::sleep(Duration::from_millis(100));
    check(
        frontier(&handle)? == paused_at,
        "realtime source produced blocks while paused",
    )?;
    live.set_paused(false).map_err(|e| e.to_string())?;
    wait_until(deadline(), "realtime frames after resume", || {
        frontier(&handle)
            .ok()
            .filter(|frame| *frame >= paused_at + 4_800)
    })?;
    let elapsed = started.elapsed();
    drop(live);
    check(
        handle.processed_through_frame().is_err(),
        "realtime handle survived stop",
    )?;
    Ok(
        json!({"paused_frontier":paused_at,"resumed_by_frames":4_800,
        "elapsed_ms":elapsed.as_millis(),"stopped":"producer joined"}),
    )
}

fn c04() -> Result<Value, String> {
    let (mut ws, mut driver) = workspace()?;
    let a = first_leaf(&ws)?;
    let b = created(action(
        &mut ws,
        WorkspaceAction::SplitModule {
            instance_id: a,
            side: Side::Right,
        },
    )?)?;
    action(
        &mut ws,
        WorkspaceAction::SetChannel {
            instance_id: b,
            channel: ChannelSelection::Right,
        },
    )?;
    action(
        &mut ws,
        WorkspaceAction::SetStyle {
            instance_id: b,
            style_id: crate::builtins::PIXEL_SPECTRUM_STYLE_ID,
        },
    )?;
    let o = ws.observation();
    let (ma, mb) = (
        o.module(a).ok_or("A missing")?,
        o.module(b).ok_or("B missing")?,
    );
    check(
        o.modules.len() == 2
            && a != b
            && ma.channel == Some(ChannelSelection::MonoAverage)
            && mb.channel == Some(ChannelSelection::Right)
            && mb.active_style == Some(crate::builtins::PIXEL_SPECTRUM_STYLE_ID)
            && ma.active_style == Some(crate::builtins::DEFAULT_SPECTRUM_STYLE_ID),
        "instances not independent",
    )?;
    action(&mut ws, WorkspaceAction::SelectModule { instance_id: a })?;
    action(&mut ws, WorkspaceAction::SelectModule { instance_id: b })?;
    action(&mut ws, WorkspaceAction::CloseModule { instance_id: b })?;
    action(&mut ws, WorkspaceAction::CloseModule { instance_id: a })?;
    let o = ws.observation();
    let root = o
        .window(FIRST_WINDOW)
        .ok_or("closing modules closed the window")?;
    check(
        root.leaves.is_empty() && root.active.is_none() && o.modules.is_empty(),
        "last close left selection or instances",
    )?;
    action(&mut ws, WorkspaceAction::TogglePause)?;
    check(
        ws.observation().source_status.state == SourceState::Paused
            && driver.advance_frames(160, deadline()).is_err(),
        "zero-module pause did not stop source",
    )?;
    action(&mut ws, WorkspaceAction::TogglePause)?;
    let new_id = created(action(
        &mut ws,
        WorkspaceAction::AddModule {
            window_id: FIRST_WINDOW,
            type_id: SPECTRUM_MODULE_ID,
        },
    )?)?;
    check(new_id != a && new_id != b, "reused instance id")?;
    let before = ws.observation();
    let rejected = [
        WorkspaceAction::SelectModule { instance_id: b },
        WorkspaceAction::AddModule {
            window_id: FIRST_WINDOW,
            type_id: SPECTRUM_MODULE_ID,
        },
        WorkspaceAction::SetChannel {
            instance_id: new_id,
            channel: ChannelSelection::Discrete(9),
        },
        WorkspaceAction::SetStyle {
            instance_id: new_id,
            style_id: "invalid.style",
        },
        WorkspaceAction::ReplaceModule {
            instance_id: new_id,
            type_id: "invalid.module",
        },
        WorkspaceAction::SplitModule {
            instance_id: b,
            side: Side::Right,
        },
    ];
    for rejected_action in rejected {
        let label = format!("{rejected_action:?}");
        check(
            action(&mut ws, rejected_action).is_err(),
            &format!("invalid action accepted: {label}"),
        )?;
    }
    let after = ws.observation();
    check(
        after.windows == before.windows
            && after.modules == before.modules
            && after.modules_created == before.modules_created,
        "failed action partially committed",
    )?;
    ws.check_ownership()?;
    Ok(
        json!({"original_ids":[a.0,b.0],"new_id":new_id.0,"source":"running",
        "rejected_without_commit":6}),
    )
}

fn c05() -> Result<Value, String> {
    let (ws, mut driver) = workspace()?;
    let mut h = Harness::new(ws);
    h.frame(vec![]);
    let a = first_leaf(&h.ws)?;
    h.click(ControlKey::SplitZone {
        instance_id: a,
        side: Side::Right,
    })?;
    let o = h.ws.observation();
    let leaves = o.window(FIRST_WINDOW).ok_or("root closed")?.leaves.clone();
    check(
        leaves.len() == 2 && leaves[0] == a,
        &format!("split entry did not add a right leaf: {leaves:?}"),
    )?;
    let b = leaves[1];
    h.menu(
        ControlKey::LeafBody(b),
        &[
            MenuItem::ChannelOpen,
            MenuItem::Channel(ChannelSelection::Right),
        ],
    )?;
    let o = h.ws.observation();
    check(
        o.module(b).and_then(|m| m.channel) == Some(ChannelSelection::Right)
            && o.module(a).and_then(|m| m.channel) == Some(ChannelSelection::MonoAverage),
        "Channel click wrong instance",
    )?;
    h.menu(
        ControlKey::LeafBody(b),
        &[
            MenuItem::StyleOpen,
            MenuItem::Style(crate::builtins::PIXEL_SPECTRUM_STYLE_ID),
        ],
    )?;
    check(
        h.ws.observation().module(b).and_then(|m| m.active_style)
            == Some(crate::builtins::PIXEL_SPECTRUM_STYLE_ID),
        "style popup did not apply",
    )?;
    h.menu(ControlKey::LeafBody(b), &[MenuItem::CloseModule])?;
    h.menu(ControlKey::LeafBody(a), &[MenuItem::CloseModule])?;
    h.frame(vec![]);
    let remaining = h.ws.observation().modules.len();
    check(
        remaining == 0 && h.locate(&ControlKey::EmptyBody).is_ok(),
        &format!("Close clicks left {remaining} modules or no empty-window entry"),
    )?;
    check(
        h.locate(&ControlKey::LeafBody(a)).is_err(),
        "closed leaf still has a target",
    )?;
    h.dropdown(
        ControlKey::Menu(MenuItem::AddOpen),
        ControlKey::Menu(MenuItem::AddModule(SPECTRUM_MODULE_ID)),
    )?;
    let c = first_leaf(&h.ws)?;
    check(c != a && c != b, "Add reused an instance")?;
    h.menu(
        ControlKey::LeafBody(c),
        &[MenuItem::SourceOpen, MenuItem::Pause],
    )?;
    check(
        h.ws.observation().source_status.state == SourceState::Paused
            && driver.advance_frames(160, deadline()).is_err(),
        "Pause entry failed",
    )?;
    h.menu(
        ControlKey::LeafBody(c),
        &[MenuItem::SourceOpen, MenuItem::Pause],
    )?;
    check(
        h.ws.observation().source_status.state == SourceState::Running,
        "Resume entry failed",
    )?;
    let old = h.locate(&ControlKey::LeafBody(c))?;
    h.set_size(egui::vec2(480.0, 240.0));
    h.frame(vec![]);
    check(
        h.click_target(ControlKey::LeafBody(c), old).is_err(),
        "stale control accepted after resize",
    )?;
    check(
        h.locate(&ControlKey::Menu(MenuItem::Style(
            crate::builtins::PIXEL_SPECTRUM_STYLE_ID,
        )))
        .is_err(),
        "closed popup item incorrectly available",
    )?;
    h.menu(
        ControlKey::LeafBody(c),
        &[MenuItem::SourceOpen, MenuItem::Pause],
    )?;
    check(
        h.ws.observation().source_status.state == SourceState::Paused,
        "narrow-layout Pause target missed",
    )?;
    h.menu(
        ControlKey::LeafBody(c),
        &[MenuItem::SourceOpen, MenuItem::Pause],
    )?;
    check(
        h.ws.observation().source_status.state == SourceState::Running,
        "narrow-layout Resume target missed",
    )?;
    h.ws.check_ownership()?;
    Ok(json!({"split":b.0,"added":c.0,"narrow_size":[480,240]}))
}

fn c06() -> Result<Value, String> {
    let (ws, _driver) = workspace()?;
    let mut h = Harness::new(ws);
    h.frame(vec![]);
    let a = first_leaf(&h.ws)?;
    let channel = |h: &Harness, id| h.ws.observation().module(id).and_then(|m| m.channel);
    h.key(egui::Key::Num2);
    check(
        channel(&h, a) == Some(ChannelSelection::Right),
        "Num2 not routed",
    )?;
    h.key(egui::Key::Num1);
    check(
        channel(&h, a) == Some(ChannelSelection::Left),
        "Num1 not routed",
    )?;
    h.key(egui::Key::M);
    check(
        channel(&h, a) == Some(ChannelSelection::MonoAverage),
        "M not routed",
    )?;
    // Repeated press/release pairs: each pair applies once, setting the same value is
    // idempotent, and no key stays held into later idle frames.
    h.key(egui::Key::Num2);
    h.key(egui::Key::Num2);
    for _ in 0..3 {
        h.frame(vec![]);
    }
    check(
        channel(&h, a) == Some(ChannelSelection::Right)
            && h.context().input(|input| input.keys_down.is_empty()),
        "repeated Num2 left a held key or changed channel while idle",
    )?;
    h.key(egui::Key::M);
    check(
        channel(&h, a) == Some(ChannelSelection::MonoAverage),
        "M after repeated Num2 not routed",
    )?;
    let mut non_channel = Vec::new();
    for type_id in [
        crate::builtins::STEREOMETER_MODULE_ID,
        LEVEL_METER_MODULE_ID,
        EMPTY_MODULE_ID,
    ] {
        let id = add_module(&mut h.ws, type_id)?;
        h.frame(vec![]);
        h.click(ControlKey::LeafBody(id))?;
        let observation = h.ws.observation();
        check(
            observation.window(FIRST_WINDOW).and_then(|w| w.active) == Some(id)
                && observation
                    .module(id)
                    .is_some_and(|module| module.channel.is_none()),
            &format!("{type_id} not selected or reports a Channel capability"),
        )?;
        h.right_click(ControlKey::LeafBody(id))?;
        h.frame(vec![]);
        check(
            h.locate(&ControlKey::Menu(MenuItem::CloseModule)).is_ok()
                && h.locate(&ControlKey::Menu(MenuItem::ChannelOpen)).is_err(),
            &format!("{type_id} menu exposes Channel or misses its entries"),
        )?;
        // Escape with the menu open closes only the menu.
        h.key(egui::Key::Escape);
        h.frame(vec![]);
        check(
            h.ws.has_window(FIRST_WINDOW)
                && h.locate(&ControlKey::Menu(MenuItem::CloseModule)).is_err(),
            "Escape with an open menu closed the window or left the menu open",
        )?;
        for key in [egui::Key::Num1, egui::Key::Num2] {
            h.key(key);
            check(
                channel(&h, a) == Some(ChannelSelection::MonoAverage),
                &format!("shortcut on {type_id} touched the unselected Spectrum"),
            )?;
        }
        non_channel.push(json!({"type":type_id,"id":id.0}));
    }
    h.key(egui::Key::Space);
    check(
        h.ws.observation().source_status.state == SourceState::Paused,
        "Space did not pause",
    )?;
    h.key(egui::Key::Space);
    check(
        h.ws.observation().source_status.state == SourceState::Running,
        "Space did not resume",
    )?;
    let mono = SyntheticConfig {
        descriptor: crate::audio::AudioDescriptor::new(
            crate::audio::SourceId(7),
            crate::audio::Generation(1),
            48_000,
            1,
            ChannelLayout::Mono,
        )
        .map_err(|e| e.to_string())?,
        signals: vec![SignalSpec::Silence],
        ..SyntheticConfig::default()
    };
    let (mono_live, _) =
        LiveAudio::open_manual(LiveAudioConfig::default(), mono).map_err(|e| e.to_string())?;
    let mut mono_h = Harness::new(Workspace::new(mono_live, false).map_err(|e| e.to_string())?);
    mono_h.frame(vec![]);
    let mono_leaf = first_leaf(&mono_h.ws)?;
    mono_h.menu(ControlKey::LeafBody(mono_leaf), &[MenuItem::ChannelOpen])?;
    check(
        mono_h
            .locate(&ControlKey::Menu(MenuItem::Channel(
                ChannelSelection::Right,
            )))
            .is_err()
            && mono_h
                .locate(&ControlKey::Menu(MenuItem::Channel(
                    ChannelSelection::Discrete(0),
                )))
                .is_ok(),
        "mono descriptor exposed Right or missed its channel",
    )?;
    mono_h.key(egui::Key::Escape);
    mono_h.key(egui::Key::Num2);
    check(
        mono_h
            .ws
            .observation()
            .module(mono_leaf)
            .and_then(|m| m.channel)
            == Some(ChannelSelection::MonoAverage),
        "mono Num2 selected missing channel",
    )?;
    h.key(egui::Key::Escape);
    check(
        h.ws.windows().is_empty() && h.ws.observation().modules.is_empty(),
        "idle Escape did not close the window and its modules",
    )?;
    Ok(json!({"spectrum":a.0,"non_channel_modules":non_channel,
        "escape":"menu first, then closes this Workspace window; native close is the adapter's"}))
}

fn c07() -> Result<Value, String> {
    let (mut ws, mut driver) = workspace()?;
    let osc = replace_first(&mut ws, crate::builtins::OSCILLOSCOPE_MODULE_ID)?;
    action(
        &mut ws,
        WorkspaceAction::SetStyle {
            instance_id: osc,
            style_id: crate::builtins::PERSISTENCE_OSCILLOSCOPE_STYLE_ID,
        },
    )?;
    let handle = ws.dataflow_handle_for_automation();
    let mut h = Harness::new(ws);
    h.display_time = Duration::from_secs(1);
    h.frame(vec![]);
    driver
        .advance_frames(1600, deadline())
        .map_err(|e| e.to_string())?;
    let early = h.frame(vec![]);
    let frontier = handle
        .processed_through_frame()
        .map_err(|e| e.to_string())?;
    let metrics = handle.metrics();
    h.display_time = Duration::from_millis(2300);
    let late = h.frame(vec![]);
    check(
        early.shapes.len() > late.shapes.len(),
        "Persistence did not expire trace at display time 1.3s later",
    )?;
    check(
        handle
            .processed_through_frame()
            .map_err(|e| e.to_string())?
            == frontier
            && handle.metrics().publications == metrics.publications,
        "UI-only frame advanced audio",
    )?;
    driver
        .advance_frames(1600, deadline())
        .map_err(|e| e.to_string())?;
    let with_trace = h.frame(vec![]);
    let published = handle.metrics().publications;
    h.menu(
        ControlKey::LeafBody(osc),
        &[MenuItem::StyleOpen, MenuItem::ClearVisual],
    )?;
    let after_clear = h.frame(vec![]);
    check(
        with_trace.shapes.len() > after_clear.shapes.len()
            && handle.metrics().publications == published,
        "Clear did not remove trace without resetting audio",
    )?;
    let hidden = waveform_visibility_run(true)?;
    let visible = waveform_visibility_run(false)?;
    check(
        hidden.pcm_frames == HIDDEN_FRAMES,
        &format!(
            "hidden Waveform History ingested {} PCM frames, expected {HIDDEN_FRAMES}",
            hidden.pcm_frames
        ),
    )?;
    check(
        hidden.restored.len() > hidden.before_audio.len(),
        "restored Waveform drew no bars from audio delivered while hidden",
    )?;
    check(
        hidden.restored == visible.restored,
        "Waveform restored after hiding differs from one drawn throughout",
    )?;
    Ok(
        json!({"audio_frontier":frontier,"persistence_shapes":[early.shapes.len(),late.shapes.len()],
        "hidden_history_pcm_frames":hidden.pcm_frames,
        "waveform_rects":{"before_audio":hidden.before_audio.len(),"restored":hidden.restored.len(),
            "matches_always_visible":true}}),
    )
}

const HIDDEN_FRAMES: u64 = 5 * 48_000;

struct WaveformRun {
    pcm_frames: u64,
    before_audio: Vec<(egui::Rect, egui::Color32)>,
    restored: Vec<(egui::Rect, egui::Color32)>,
}

/// Filled rectangles drawn this pass: individual rect shapes plus the quads of the
/// batched fill meshes the render target emits.
fn filled_rects(output: &egui::FullOutput) -> Vec<(egui::Rect, egui::Color32)> {
    let mut rects = Vec::new();
    for clipped in &output.shapes {
        match &clipped.shape {
            egui::Shape::Rect(rect) => rects.push((rect.rect, rect.fill)),
            egui::Shape::Mesh(mesh) => {
                for quad in mesh.vertices.as_chunks::<4>().0 {
                    let rect =
                        egui::Rect::from_points(&quad.iter().map(|v| v.pos).collect::<Vec<_>>());
                    rects.push((rect, quad[0].color));
                }
            }
            _ => {}
        }
    }
    rects
}

/// One Waveform History instance (the window's only leaf) receives `HIDDEN_FRAMES`
/// of audio. With `hidden` its window runs no UI pass meanwhile (a minimized /
/// invisible viewport is not drawn); otherwise it is drawn after every second of
/// audio. Both runs then draw the window once with identical input.
fn waveform_visibility_run(hidden: bool) -> Result<WaveformRun, String> {
    let (mut ws, mut driver) = workspace()?;
    let handle = ws.dataflow_handle_for_automation();
    replace_first(&mut ws, WAVEFORM_MODULE_ID)?;
    let mut h = Harness::new(ws);
    let before_audio = filled_rects(&h.frame(vec![]));
    let before = handle.metrics().pcm_frames;
    for _ in 0..HIDDEN_FRAMES / 48_000 {
        driver
            .advance_frames(48_000, deadline())
            .map_err(|e| e.to_string())?;
        if !hidden {
            h.frame(vec![]);
        }
    }
    let pcm_frames = handle.metrics().pcm_frames - before;
    let restored = filled_rects(&h.frame(vec![]));
    Ok(WaveformRun {
        pcm_frames,
        before_audio,
        restored,
    })
}

fn c08() -> Result<Value, String> {
    let (mut ws, mut driver) = workspace()?;
    let handle = ws.dataflow_handle_for_automation();
    let spectrum = first_leaf(&ws)?;
    let waveform = add_module(&mut ws, WAVEFORM_MODULE_ID)?;
    let levels = add_module(&mut ws, LEVEL_METER_MODULE_ID)?;
    driver
        .advance_frames(3200, deadline())
        .map_err(|e| e.to_string())?;
    let both = handle.metrics();
    check(
        both.pcm_frames > 0 && both.level_frames > 0,
        "mixed time needs did not compute",
    )?;
    action(
        &mut ws,
        WorkspaceAction::CloseModule {
            instance_id: waveform,
        },
    )?;
    // The next drained block follows queued removal commands.
    let before = handle.metrics();
    driver
        .advance_frames(3200, deadline())
        .map_err(|e| e.to_string())?;
    let only_levels = handle.metrics();
    check(
        only_levels.pcm_frames == before.pcm_frames
            && only_levels.level_frames > before.level_frames,
        "PCM close affected Levels",
    )?;
    let new_waveform = add_module(&mut ws, WAVEFORM_MODULE_ID)?;
    action(
        &mut ws,
        WorkspaceAction::CloseModule {
            instance_id: levels,
        },
    )?;
    let before = handle.metrics();
    driver
        .advance_frames(3200, deadline())
        .map_err(|e| e.to_string())?;
    let only_pcm = handle.metrics();
    check(
        only_pcm.pcm_frames > before.pcm_frames && only_pcm.level_frames == before.level_frames,
        "Levels close affected PCM",
    )?;
    action(
        &mut ws,
        WorkspaceAction::CloseModule {
            instance_id: new_waveform,
        },
    )?;
    action(
        &mut ws,
        WorkspaceAction::CloseModule {
            instance_id: spectrum,
        },
    )?;
    wait_until(deadline(), "all needs released", || {
        (handle.metrics().subscriptions == 0).then_some(())
    })?;
    let before = handle.metrics();
    driver
        .advance_frames(1600, deadline())
        .map_err(|e| e.to_string())?;
    check(
        before == handle.metrics(),
        "closed instances still analyzing",
    )?;
    for _ in 0..20 {
        let id = created(action(
            &mut ws,
            WorkspaceAction::AddModule {
                window_id: FIRST_WINDOW,
                type_id: WAVEFORM_MODULE_ID,
            },
        )?)?;
        action(&mut ws, WorkspaceAction::CloseModule { instance_id: id })?;
    }
    wait_until(deadline(), "cycled needs released", || {
        (handle.metrics().subscriptions == 0).then_some(())
    })?;
    let after_cycles = handle.metrics();
    driver
        .advance_frames(1600, deadline())
        .map_err(|e| e.to_string())?;
    check(
        handle.metrics() == after_cycles,
        "create/close cycles retained analysis demand",
    )?;
    Ok(
        json!({"pcm_frames":both.pcm_frames,"level_frames":both.level_frames,"cycles":20,"active_subscriptions":handle.metrics().subscriptions}),
    )
}

fn c09() -> Result<Value, String> {
    let (mut ws, mut driver) = workspace()?;
    action(&mut ws, WorkspaceAction::TogglePause)?;
    check(
        driver.advance_frames(160, deadline()).is_err(),
        "paused input accepted",
    )?;
    action(&mut ws, WorkspaceAction::TogglePause)?;
    check(
        action(
            &mut ws,
            WorkspaceAction::SelectModule {
                instance_id: ModuleInstanceId(u64::MAX),
            },
        )
        .is_err(),
        "invalid ID accepted",
    )?;
    let empty = add_module(&mut ws, EMPTY_MODULE_ID)?;
    check(
        action(
            &mut ws,
            WorkspaceAction::SetChannel {
                instance_id: empty,
                channel: ChannelSelection::Right,
            },
        )
        .is_err(),
        "unsupported channel accepted",
    )?;
    let mut h = Harness::new(ws);
    h.frame(vec![]);
    h.right_click(ControlKey::LeafBody(empty))?;
    h.frame(vec![]);
    check(
        h.locate(&ControlKey::Menu(MenuItem::StyleOpen)).is_err()
            && h.locate(&ControlKey::Menu(MenuItem::ClearVisual)).is_err()
            && h.locate(&ControlKey::Menu(MenuItem::ModuleOpen)).is_ok(),
        "Empty menu shows style controls or misses Module",
    )?;
    h.key(egui::Key::Escape);
    let old = h.locate(&ControlKey::LeafBody(empty))?;
    h.frame(vec![]);
    check(
        h.click_target(ControlKey::LeafBody(empty), old).is_err(),
        "stale token accepted",
    )?;
    let timeout = wait_until(
        Instant::now() + Duration::from_millis(2),
        "deliberately false condition",
        || None::<()>,
    );
    check(
        timeout.is_err() && timeout.unwrap_err().contains("Timeout"),
        "deadline did not report Timeout",
    )?;
    Ok(
        json!({"invalid_id":"rejected","hidden_control":"missing","stale":"rejected","timeout":"observed"}),
    )
}

fn rgba_view(image: &image::RgbaImage) -> visual::Rgba8View<'_> {
    visual::Rgba8View {
        width: image.width(),
        height: image.height(),
        rgba8: image.as_raw(),
    }
}

fn c10() -> Result<Value, String> {
    let mut a = image::RgbaImage::from_pixel(2, 2, image::Rgba([0, 20, 30, 255]));
    let mut b = a.clone();
    check(
        visual::compare_rgba(rgba_view(&a), rgba_view(&b))
            .map_err(|e| e.to_string())?
            .changed_pixels
            == Some(0),
        "equal pixels differ",
    )?;
    b.put_pixel(1, 0, image::Rgba([1, 20, 30, 255]));
    let diff = visual::compare_rgba(rgba_view(&a), rgba_view(&b)).map_err(|e| e.to_string())?;
    check(
        diff.changed_pixels == Some(1)
            && diff.diff_bbox.is_some_and(|bbox| {
                bbox.min_x == 1 && bbox.max_x == 1 && bbox.min_y == 0 && bbox.max_y == 0
            }),
        "single-pixel difference not located",
    )?;
    a = image::RgbaImage::from_pixel(3, 2, image::Rgba([0, 20, 30, 255]));
    check(
        !visual::compare_rgba(rgba_view(&a), rgba_view(&b))
            .map_err(|e| e.to_string())?
            .dimensions_match,
        "dimension mismatch not detected",
    )?;
    // Exercise the project comparator on real files; core has no output directory, so
    // use process-unique temporary files and remove them afterwards.
    let dir = std::env::temp_dir().join(format!("taffymeters-c10-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let good = dir.join("good.png");
    let bad = dir.join("bad.png");
    let result = (|| {
        visual::save_png(&good, b.width(), b.height(), b.as_raw()).map_err(|e| e.to_string())?;
        std::fs::write(&bad, b"bad png").map_err(|e| e.to_string())?;
        check(
            visual::compare_png_paths(&good, &good)
                .map_err(|e| e.to_string())?
                .changed_pixels
                == Some(0),
            "comparator rejected identical PNG files",
        )?;
        match visual::compare_png_paths(&good, &bad) {
            Err(visual::VisualError::Png { path, .. }) if path == bad => Ok(()),
            other => Err(format!(
                "corrupt PNG not reported as a PNG error: {other:?}"
            )),
        }
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result?;
    Ok(
        json!({"changed_pixels":diff.changed_pixels,"bad_png":"compare_png_paths returned Png error","dimension_mismatch":"rejected"}),
    )
}

pub(super) fn compare_case(expected: &Path, actual: &Path, output: &Path) -> Case {
    match visual::compare_png_paths(expected, actual) {
        Ok(metrics) if metrics.dimensions_match && metrics.changed_pixels == Some(0) => {
            Case::pass("compare", json!({"comparison":metrics}))
        }
        Ok(metrics) => {
            let mut case = Case::fail(
                "compare",
                "pixel comparison",
                format!("different pixels or dimensions: {metrics:?}"),
            );
            case.expected = json!({"path":expected});
            case.observed = json!({"comparison":metrics,"path":actual});
            if metrics.dimensions_match
                && let (Ok(a), Ok(b)) = (image::open(expected), image::open(actual))
            {
                let diff = output.join("diff.png");
                let a = a.to_rgba8();
                let b = b.to_rgba8();
                if visual::save_diff_png(&diff, rgba_view(&a), rgba_view(&b)).is_ok() {
                    case.artifacts.push("diff.png".into());
                }
            }
            case
        }
        Err(e) => Case::fail("compare", "PNG decode", e),
    }
}

fn capture_frame(
    renderer: &mut visual::OffscreenRenderer,
    h: &mut Harness,
) -> Result<visual::Rendered, String> {
    let first = h.frame(vec![]);
    let context = h.context();
    let size = h.window(h.focus)?.size;
    renderer
        .render(&context, first, size.x as u32, size.y as u32)
        .map_err(|e| e.to_string())
}

/// Setup for an image of a single full-window Module with an optional style.
fn single_module(
    type_id: &'static str,
    style: Option<&'static str>,
) -> impl FnOnce(&mut Workspace) -> Result<(), String> {
    move |ws| {
        let id = if type_id == SPECTRUM_MODULE_ID {
            first_leaf(ws)?
        } else {
            replace_first(ws, type_id)?
        };
        if let Some(style_id) = style {
            action(
                ws,
                WorkspaceAction::SetStyle {
                    instance_id: id,
                    style_id,
                },
            )?;
        }
        Ok(())
    }
}

fn workspace_layout(ws: &mut Workspace) -> Result<(), String> {
    let spectrum = first_leaf(ws)?;
    let waveform = created(action(
        ws,
        WorkspaceAction::SplitModule {
            instance_id: spectrum,
            side: Side::Right,
        },
    )?)?;
    let waveform = created(action(
        ws,
        WorkspaceAction::ReplaceModule {
            instance_id: waveform,
            type_id: WAVEFORM_MODULE_ID,
        },
    )?)?;
    let levels = created(action(
        ws,
        WorkspaceAction::SplitModule {
            instance_id: waveform,
            side: Side::Below,
        },
    )?)?;
    action(
        ws,
        WorkspaceAction::ReplaceModule {
            instance_id: levels,
            type_id: LEVEL_METER_MODULE_ID,
        },
    )?;
    Ok(())
}

fn image_case(
    renderer: &mut visual::OffscreenRenderer,
    id: &str,
    setup: impl FnOnce(&mut Workspace) -> Result<(), String>,
    dpi: f32,
    output: &Path,
    baseline: Option<&Path>,
) -> Result<(Case, Value), String> {
    let (mut ws, mut driver) = workspace()?;
    setup(&mut ws)?;
    let handle = ws.dataflow_handle_for_automation();
    let mut h = Harness::new(ws);
    h.window_mut(FIRST_WINDOW)?.ppp = dpi;
    capture_frame(renderer, &mut h)?;
    driver
        .advance_frames(48_000, deadline())
        .map_err(|e| e.to_string())?;
    check(
        handle
            .processed_through_frame()
            .map_err(|e| e.to_string())?
            >= 48_000,
        "input not processed",
    )?;
    let frame = capture_frame(renderer, &mut h)?;
    let image = image::RgbaImage::from_raw(frame.pixel_size[0], frame.pixel_size[1], frame.rgba8)
        .ok_or("invalid readback length")?;
    let hash = super::report::digest(image.as_raw());
    let manifest = json!({"case_id":id,"scene_version":1,"frame_token":h.frame,
        "egui_frame_nr":frame.egui_frame_nr,"egui_pass_nr":frame.egui_pass_nr,
        "gpu_submission_id":frame.submission_id,"readback_submission_id":frame.submission_id,
        "source": {"id":h.ws.source_descriptor_for_automation().source_id.0,
        "generation":h.ws.source_descriptor_for_automation().generation.0}, "delivered_through_frame":48_000,
        "processed_through_frame":handle.processed_through_frame().map_err(|e|e.to_string())?,
        "result_revision":null,"result_version":null,
        "result_marker_note":"Module trait does not expose subscription snapshots; no extra analysis probe is created",
        "display_time_seconds":h.display_time.as_secs_f64(),"logical_size":frame.logical_size,
        "pixel_size":frame.pixel_size,"pixels_per_point":frame.pixels_per_point,
        "signal":"48k stereo left 3000Hz/right 6000Hz peak 0.08 block 160",
        "renderer":frame.renderer,"font":"egui bundled defaults","theme":"dark","os":std::env::consts::OS,
        "dependency":{"egui":"0.34.1","egui-wgpu":"0.34.1","wgpu":"29.0.1"},"pixel_sha256":hash});
    let candidate = output.join(format!("{id}.png"));
    let manifest_path = output.join(format!("{id}.json"));
    let mut case = judge_baseline(id, &manifest, &image, baseline, output);
    // Retain the actual image whenever it is not an approved pixel match.
    if case.status != Status::Pass {
        visual::save_png(
            &candidate,
            frame.pixel_size[0],
            frame.pixel_size[1],
            image.as_raw(),
        )
        .map_err(|e| e.to_string())?;
        case.artifacts.push(format!("images/{id}.png"));
    }
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    case.artifacts.push(format!("images/{id}.json"));
    case.observed = json!({"pixel_sha256":hash,"frame_token":h.frame,"processed_through_frame":48_000,"comparison":case.observed});
    Ok((case, manifest))
}

/// Judge one rendered image against a reviewed baseline directory. A missing or
/// unapproved baseline is never auto-accepted; integrity problems are failures.
fn judge_baseline(
    id: &str,
    manifest: &Value,
    image: &image::RgbaImage,
    baseline: Option<&Path>,
    output: &Path,
) -> Case {
    let Some(base) = baseline else {
        return Case::incomplete(
            id,
            Status::NeedsReview,
            "candidate requires human baseline review",
        );
    };
    if !base.join("approval.json").is_file() {
        return Case::incomplete(
            id,
            Status::NeedsReview,
            "baseline exists but approval.json is missing",
        );
    }
    compare_with_baseline(id, manifest, image, base, output)
        .unwrap_or_else(|error| Case::fail(id, "baseline integrity", error))
}

fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

fn compare_with_baseline(
    id: &str,
    manifest: &Value,
    image: &image::RgbaImage,
    base: &Path,
    output: &Path,
) -> Result<Case, String> {
    const COMPARABLE: [&str; 11] = [
        "case_id",
        "scene_version",
        "logical_size",
        "pixel_size",
        "pixels_per_point",
        "signal",
        "renderer",
        "font",
        "theme",
        "os",
        "dependency",
    ];
    let approved = read_json(&base.join("approval.json"))?;
    check(
        approved.get("reviewer").and_then(Value::as_str).is_some()
            && approved.get("date").and_then(Value::as_str).is_some()
            && approved.get("reason").and_then(Value::as_str).is_some(),
        "baseline lacks reviewer/date/reason",
    )?;
    let previous = read_json(&base.join(format!("{id}.json")))?;
    check(
        approved["candidate_hashes"][id] == previous["pixel_sha256"]
            && previous["pixel_sha256"].as_str().is_some(),
        "baseline approval hash mismatches manifest",
    )?;
    if COMPARABLE
        .iter()
        .any(|field| previous[*field] != manifest[*field])
    {
        return Ok(Case::incomplete(
            id,
            Status::NotComparable,
            "baseline profile differs",
        ));
    }
    let base_image = base.join(format!("{id}.png"));
    let expected = visual::load_png(&base_image).map_err(|e| e.to_string())?;
    check(
        super::report::digest(&expected.rgba8) == previous["pixel_sha256"],
        "baseline PNG hash mismatches manifest",
    )?;
    let metrics =
        visual::compare_rgba(expected.as_view(), rgba_view(image)).map_err(|e| e.to_string())?;
    if metrics.dimensions_match && metrics.changed_pixels == Some(0) {
        return Ok(Case::pass(id, json!({"comparison":metrics})));
    }
    let mut fail = Case::fail(id, "pixel comparison", format!("{metrics:?}"));
    fail.expected = json!({"path":base_image});
    fail.observed = json!({"comparison":metrics});
    if metrics.dimensions_match {
        let diff = format!("{id}-diff.png");
        visual::save_diff_png(&output.join(&diff), expected.as_view(), rgba_view(image))
            .map_err(|e| e.to_string())?;
        fail.artifacts.push(format!("images/{diff}"));
    }
    Ok(fail)
}

fn repeat_frame(renderer: &mut visual::OffscreenRenderer) -> Result<image::RgbaImage, String> {
    let (ws, mut driver) = workspace()?;
    let handle = ws.dataflow_handle_for_automation();
    let mut h = Harness::new(ws);
    capture_frame(renderer, &mut h)?;
    driver
        .advance_frames(48_000, deadline())
        .map_err(|error| error.to_string())?;
    check(
        handle
            .processed_through_frame()
            .map_err(|error| error.to_string())?
            >= 48_000,
        "repeat input not processed",
    )?;
    let frame = capture_frame(renderer, &mut h)?;
    image::RgbaImage::from_raw(frame.pixel_size[0], frame.pixel_size[1], frame.rgba8)
        .ok_or_else(|| "invalid repeat readback length".to_owned())
}

fn persistence_visual(
    renderer: &mut visual::OffscreenRenderer,
    output: &Path,
    baseline: Option<&Path>,
) -> Result<Case, String> {
    let (mut ws, mut driver) = workspace()?;
    let osc = replace_first(&mut ws, crate::builtins::OSCILLOSCOPE_MODULE_ID)?;
    action(
        &mut ws,
        WorkspaceAction::SetStyle {
            instance_id: osc,
            style_id: crate::builtins::PERSISTENCE_OSCILLOSCOPE_STYLE_ID,
        },
    )?;
    let handle = ws.dataflow_handle_for_automation();
    let mut h = Harness::new(ws);
    h.display_time = Duration::ZERO;
    capture_frame(renderer, &mut h)?;
    driver
        .advance_frames(1600, deadline())
        .map_err(|e| e.to_string())?;
    h.display_time = Duration::from_millis(300);
    let fresh = capture_frame(renderer, &mut h)?;
    h.display_time = Duration::from_millis(1300);
    let expired = capture_frame(renderer, &mut h)?;
    check(
        fresh.rgba8 != expired.rgba8,
        "Persistence did not change when its trace expired",
    )?;
    check(
        handle
            .processed_through_frame()
            .map_err(|e| e.to_string())?
            == 1600,
        "display-only frames changed audio frontier",
    )?;
    let approved = baseline.filter(|path| path.join("approval.json").is_file());
    let mut case = if approved.is_some() {
        Case::pass("V03-persistence", Value::Null)
    } else {
        Case::incomplete(
            "V03-persistence",
            Status::NeedsReview,
            "fixed-time candidate frames require visual review",
        )
    };
    for (name, rendered, time) in [("fresh", &fresh, 0.3), ("expired", &expired, 1.3)] {
        let id = format!("V03-persistence-{name}");
        let hash = super::report::digest(&rendered.rgba8);
        if let Some(base) = approved {
            let approval: Value = serde_json::from_slice(
                &std::fs::read(base.join("approval.json")).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let previous: Value = serde_json::from_slice(
                &std::fs::read(base.join(format!("{id}.json"))).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            if approval["candidate_hashes"][&id] != previous["pixel_sha256"] {
                return Err(format!("{id} baseline approval hash mismatch"));
            }
            let previous_image =
                visual::load_png(&base.join(format!("{id}.png"))).map_err(|e| e.to_string())?;
            if super::report::digest(&previous_image.rgba8) != previous["pixel_sha256"] {
                return Err(format!("{id} baseline file hash mismatch"));
            }
            if previous["renderer"] != json!(rendered.renderer)
                || previous["pixel_size"] != json!(rendered.pixel_size)
                || previous["display_time_seconds"] != json!(time)
            {
                case.status = Status::NotComparable;
                case.error = Some(format!("{id} profile differs"));
            } else if visual::compare_rgba(
                previous_image.as_view(),
                visual::Rgba8View {
                    width: rendered.pixel_size[0],
                    height: rendered.pixel_size[1],
                    rgba8: &rendered.rgba8,
                },
            )
            .map_err(|e| e.to_string())?
            .changed_pixels
                != Some(0)
            {
                case = Case::fail(
                    "V03-persistence",
                    "baseline pixels",
                    format!("{id} differs"),
                );
            }
        }
        let file = format!("V03-persistence-{name}.png");
        if case.status != Status::Pass {
            visual::save_png(
                &output.join(&file),
                rendered.pixel_size[0],
                rendered.pixel_size[1],
                &rendered.rgba8,
            )
            .map_err(|e| e.to_string())?;
        }
        let manifest = format!("V03-persistence-{name}.json");
        std::fs::write(output.join(&manifest),serde_json::to_vec_pretty(&json!({
            "case_id":"V03-persistence","scene_version":1,"frame_token":h.frame,
            "egui_frame_nr":rendered.egui_frame_nr,"egui_pass_nr":rendered.egui_pass_nr,
            "gpu_submission_id":rendered.submission_id,"readback_submission_id":rendered.submission_id,
            "source":h.ws.source_descriptor_for_automation().source_id.0,"generation":h.ws.source_descriptor_for_automation().generation.0,
            "delivered_through_frame":1600,"processed_through_frame":1600,
            "result_version":null,"result_marker_note":"no extra analysis probe",
            "display_time_seconds":time,"logical_size":rendered.logical_size,"pixel_size":rendered.pixel_size,
            "pixels_per_point":rendered.pixels_per_point,"renderer":rendered.renderer,
            "font":"egui bundled defaults","theme":"dark","pixel_sha256":hash
        })).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
        if case.status != Status::Pass {
            case.artifacts.push(format!("images/{file}"));
        }
        case.artifacts.push(format!("images/{manifest}"));
    }
    case.observed = json!({"fresh_sha256":super::report::digest(&fresh.rgba8),"expired_sha256":super::report::digest(&expired.rgba8)});
    Ok(case)
}

pub(super) fn visual(report: &mut Report, output: &Path, baseline: Option<&Path>) {
    // V05 checks that the run never modifies a supplied baseline.
    let baseline_before = baseline.map(tree_digest);
    let mut renderer = match visual::OffscreenRenderer::new() {
        Ok(renderer) => renderer,
        Err(visual::VisualError::Initialization { stage, message }) => {
            report.cases.push(Case::incomplete(
                "V01",
                Status::Unavailable,
                format!("{stage}: {message}"),
            ));
            for id in ["V02", "V03", "V04", "V05", "V06"] {
                report.cases.push(Case::incomplete(
                    id,
                    Status::NotRun,
                    "V01 adapter/device unavailable",
                ));
            }
            return;
        }
        Err(e) => {
            report
                .cases
                .push(Case::fail("V01", "renderer initialization", e));
            for id in ["V02", "V03", "V04", "V05", "V06"] {
                report
                    .cases
                    .push(Case::incomplete(id, Status::NotRun, "V01 failed"));
            }
            return;
        }
    };
    if let Err(e) = std::fs::create_dir_all(output.join("images")) {
        report.cases.push(Case::fail("V01", "output images", e));
        return;
    }
    let cases = [
        ("spectrum-curve", SPECTRUM_MODULE_ID, None),
        (
            "spectrum-bars",
            SPECTRUM_MODULE_ID,
            Some(crate::builtins::BAR_SPECTRUM_STYLE_ID),
        ),
        (
            "spectrum-pixel",
            SPECTRUM_MODULE_ID,
            Some(crate::builtins::PIXEL_SPECTRUM_STYLE_ID),
        ),
        (
            "oscilloscope-line",
            crate::builtins::OSCILLOSCOPE_MODULE_ID,
            None,
        ),
        (
            "oscilloscope-persistence",
            crate::builtins::OSCILLOSCOPE_MODULE_ID,
            Some(crate::builtins::PERSISTENCE_OSCILLOSCOPE_STYLE_ID),
        ),
        ("spectrogram", crate::builtins::SPECTROGRAM_MODULE_ID, None),
        ("waveform", WAVEFORM_MODULE_ID, None),
        ("stereometer", crate::builtins::STEREOMETER_MODULE_ID, None),
        ("level-meter", LEVEL_METER_MODULE_ID, None),
        ("empty", EMPTY_MODULE_ID, None),
    ];
    let images = output.join("images");
    match image_case(
        &mut renderer,
        "spectrum-curve",
        single_module(SPECTRUM_MODULE_ID, None),
        1.0,
        &images,
        baseline,
    ) {
        Ok((case, manifest)) => {
            report.environment["renderer"] = manifest["renderer"].clone();
            report.cases.push(Case::pass(
                "V01",
                json!({"renderer":manifest["renderer"],"frame_token":manifest["frame_token"]}),
            ));
            let mut results = vec![case];
            for (id, type_id, style) in cases.into_iter().skip(1) {
                match image_case(
                    &mut renderer,
                    id,
                    single_module(type_id, style),
                    1.0,
                    &images,
                    baseline,
                ) {
                    Ok((case, _)) => results.push(case),
                    Err(e) => results.push(Case::fail(id, "real frame", e)),
                }
            }
            let mut summary = Case::pass("V02", json!({"image_cases":results.len()}));
            if let Some(failure) = results.iter().find(|case| case.status == Status::Fail) {
                summary = Case::fail(
                    "V02",
                    "image",
                    format!("{} failed: {:?}", failure.id, failure.error),
                );
            } else if results
                .iter()
                .any(|case| case.status == Status::NotComparable)
            {
                summary = Case::incomplete("V02", Status::NotComparable, "profile differs");
            } else if results
                .iter()
                .any(|case| case.status == Status::NeedsReview)
            {
                summary =
                    Case::incomplete("V02", Status::NeedsReview, "candidate images need review");
            }
            report.cases.push(summary);
            report.cases.extend(results);
        }
        Err(e) => {
            report.cases.push(Case::fail("V01", "real frame", e));
            for id in ["V02", "V03", "V04", "V05", "V06"] {
                report
                    .cases
                    .push(Case::incomplete(id, Status::NotRun, "V01 failed"));
            }
            return;
        }
    }
    report.cases.push(
        match image_case(
            &mut renderer,
            "spectrum-2x",
            single_module(SPECTRUM_MODULE_ID, None),
            2.0,
            &images,
            baseline,
        ) {
            Ok((case, _)) => {
                let mut result = Case::pass(
                    "V03",
                    json!({"two_x_case":case.status,"candidate":case.observed}),
                );
                result.status = case.status;
                result.error = case.error;
                result.artifacts = case.artifacts;
                result
            }
            Err(e) => Case::fail("V03", "2x DPI frame", e),
        },
    );
    // V06: a three-leaf BSP layout (Spectrum | Waveform over Level Meter) at 1x and
    // 2x; layout, dividers and per-leaf clipping come from the production UI body.
    for (id, dpi) in [("workspace-layout", 1.0), ("workspace-layout-2x", 2.0)] {
        report.cases.push(
            match image_case(&mut renderer, id, workspace_layout, dpi, &images, baseline) {
                Ok((mut case, _)) => {
                    case.id = format!("V06-{id}");
                    case
                }
                Err(e) => Case::fail(&format!("V06-{id}"), "workspace layout frame", e),
            },
        );
    }
    report
        .cases
        .push(match persistence_visual(&mut renderer, &images, baseline) {
            Ok(case) => case,
            Err(error) => Case::fail("V03-persistence", "fixed-time render", error),
        });
    report.cases.push(
        match (repeat_frame(&mut renderer), repeat_frame(&mut renderer)) {
            (Ok(a), Ok(b)) => repeat_case(&a, &b),
            (a, b) => Case::fail(
                "V04",
                "repeat capture",
                format!("first: {:?}, second: {:?}", a.err(), b.err()),
            ),
        },
    );
    let guards = baseline_guards(&output.join("v05-guards"));
    let baseline_after = baseline.map(tree_digest);
    report
        .cases
        .push(match (guards, baseline_before, baseline_after) {
            (Err(error), _, _) => Case::fail("V05", "baseline guard", error),
            (Ok(_), Some(Err(error)), _) | (Ok(_), _, Some(Err(error))) => {
                Case::fail("V05", "baseline digest", error)
            }
            (Ok(_), Some(Ok(before)), Some(Ok(after))) if before != after => Case::fail(
                "V05",
                "baseline read-only",
                "visual run modified the baseline directory",
            ),
            (Ok(mut observed), before, _) => {
                observed["real_baseline"] = match before {
                    Some(Ok(files)) => json!({"files":files.len(),"modified":false}),
                    _ => json!("none supplied"),
                };
                let mut case = Case::pass("V05", observed);
                case.artifacts.push("v05-guards".into());
                case
            }
        });
}

/// V04: two independent scene runs must match, and a one-pixel change to the
/// comparator input must be detected and located.
fn repeat_case(a: &image::RgbaImage, b: &image::RgbaImage) -> Case {
    let repeat = match visual::compare_rgba(rgba_view(a), rgba_view(b)) {
        Ok(metrics) if metrics.dimensions_match && metrics.changed_pixels == Some(0) => metrics,
        Ok(metrics) => {
            return Case::fail("V04", "independent scene repeat", format!("{metrics:?}"));
        }
        Err(error) => return Case::fail("V04", "compare", error),
    };
    let mut negative = b.clone();
    let (x, y) = (b.width() / 2, b.height() / 2);
    let pixel = negative.get_pixel_mut(x, y);
    pixel.0[0] ^= 0x01;
    match visual::compare_rgba(rgba_view(a), rgba_view(&negative)) {
        Ok(metrics)
            if metrics.changed_pixels == Some(1)
                && metrics.diff_bbox.is_some_and(|bbox| {
                    bbox.min_x == x && bbox.max_x == x && bbox.min_y == y && bbox.max_y == y
                }) =>
        {
            Case::pass(
                "V04",
                json!({"repeat":repeat,"pixel_sha256":super::report::digest(a.as_raw()),
                    "negative":{"pixel":[x,y],"comparison":metrics,"result":"FAIL detected"}}),
            )
        }
        Ok(metrics) => Case::fail(
            "V04",
            "one-pixel negative",
            format!("changed pixel not detected/located: {metrics:?}"),
        ),
        Err(error) => Case::fail("V04", "one-pixel negative", error),
    }
}

/// Relative path and SHA-256 of every file below `root`, sorted.
fn tree_digest(root: &Path) -> Result<Vec<(String, String)>, String> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                let relative = path.strip_prefix(root).unwrap_or(&path);
                files.push((
                    relative.display().to_string(),
                    super::report::digest(&bytes),
                ));
            }
        }
    }
    files.sort();
    Ok(files)
}

/// V05: drive `judge_baseline` through every protection path with small local
/// baselines and require the documented status for each; none may be modified.
fn baseline_guards(root: &Path) -> Result<Value, String> {
    const ID: &str = "V05-probe";
    let io = |e: std::io::Error| e.to_string();
    let image = image::RgbaImage::from_pixel(4, 4, image::Rgba([10, 20, 30, 255]));
    let hash = super::report::digest(image.as_raw());
    let manifest = |renderer: &str| {
        json!({"case_id":ID,"scene_version":1,"logical_size":[4.0,4.0],"pixel_size":[4,4],
            "pixels_per_point":1.0,"signal":"constant probe","renderer":renderer,
            "font":"egui bundled defaults","theme":"dark","os":std::env::consts::OS,
            "dependency":{},"pixel_sha256":hash})
    };
    let current = manifest("probe-renderer");
    let write_baseline =
        |name: &str, stored: &Value, png: Option<&[u8]>, approved: bool| -> Result<_, String> {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).map_err(io)?;
            std::fs::write(
                dir.join(format!("{ID}.json")),
                serde_json::to_vec_pretty(stored).map_err(|e| e.to_string())?,
            )
            .map_err(io)?;
            match png {
                Some(bytes) => std::fs::write(dir.join(format!("{ID}.png")), bytes).map_err(io)?,
                None => visual::save_png(&dir.join(format!("{ID}.png")), 4, 4, image.as_raw())
                    .map_err(|e| e.to_string())?,
            }
            if approved {
                std::fs::write(
                    dir.join("approval.json"),
                    serde_json::to_vec_pretty(&json!({"reviewer":"V05 guard probe",
                        "date":"1970-01-01","reason":"synthetic guard fixture",
                        "candidate_hashes":{ID:hash}}))
                    .map_err(|e| e.to_string())?,
                )
                .map_err(io)?;
            }
            Ok(dir)
        };
    let mut changed = image.clone();
    changed.put_pixel(2, 1, image::Rgba([11, 20, 30, 255]));
    let out = root.join("out");
    std::fs::create_dir_all(&out).map_err(io)?;
    let fixtures = [
        ("missing", None, &image, Status::NeedsReview),
        (
            "unapproved",
            Some(write_baseline("unapproved", &current, None, false)?),
            &image,
            Status::NeedsReview,
        ),
        (
            "profile",
            Some(write_baseline(
                "profile",
                &manifest("other-renderer"),
                None,
                true,
            )?),
            &image,
            Status::NotComparable,
        ),
        (
            "corrupt",
            Some(write_baseline(
                "corrupt",
                &current,
                Some(b"not a png"),
                true,
            )?),
            &image,
            Status::Fail,
        ),
        (
            "match",
            Some(write_baseline("match", &current, None, true)?),
            &image,
            Status::Pass,
        ),
        (
            "pixel",
            Some(write_baseline("pixel", &current, None, true)?),
            &changed,
            Status::Fail,
        ),
    ];
    let mut observed = serde_json::Map::new();
    for (name, dir, actual, expected) in fixtures {
        let before = dir.as_deref().map(tree_digest).transpose()?;
        let base = dir.as_deref();
        let case = judge_baseline(ID, &current, actual, base, &out);
        if case.status != expected {
            return Err(format!(
                "{name}: expected {expected:?}, got {:?} ({:?})",
                case.status, case.error
            ));
        }
        if name == "pixel" && !out.join(format!("{ID}-diff.png")).is_file() {
            return Err("pixel failure did not save a diff image".into());
        }
        if name == "pixel" && case.expected["path"].as_str().is_none() {
            return Err("pixel failure did not record the baseline path".into());
        }
        if dir.as_deref().map(tree_digest).transpose()? != before {
            return Err(format!("{name}: judging modified the baseline"));
        }
        observed.insert(
            name.into(),
            json!({"status":case.status,"error":case.error}),
        );
    }
    Ok(Value::Object(observed))
}
