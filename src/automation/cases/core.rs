use std::time::{Duration, Instant};

use eframe::egui;
use serde_json::{Value, json};

use crate::{
    LiveAudio, LiveAudioConfig,
    audio::{
        ChannelLayout, SourceState,
        synthetic::{SignalSpec, SyntheticConfig},
    },
    automation::{
        harness::{Harness, SETTINGS},
        support::{
            action, add_module, check, created, deadline, first_leaf, replace_first, rgba_view,
            synthetic, wait_until, workspace,
        },
        visual,
    },
    dataflow::{ChannelSelection, DataSpec, FftConfig, Keep, NodeSpec, OutputId, SOURCE},
    modules::{EMPTY_MODULE_ID, LEVEL_METER_MODULE_ID, SPECTRUM_MODULE_ID, WAVEFORM_MODULE_ID},
    workspace::{
        ControlKey, FIRST_WINDOW, ModuleInstanceId, SettingsItem, Side, Tab, Workspace,
        WorkspaceAction,
    },
};

type Scenario = fn() -> Result<Value, String>;

pub(super) const CASES: [(&str, Scenario); 10] = [
    ("C01", c01),
    ("C02", c02),
    ("C03", c03),
    ("C04", c04),
    ("C05", c05),
    ("C06", c06),
    ("C07", c07),
    ("C08", c08),
    ("C09", c09),
    ("C10", c10),
];

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
            style_id: crate::modules::PIXEL_SPECTRUM_STYLE_ID,
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
            && mb.active_style == Some(crate::modules::PIXEL_SPECTRUM_STYLE_ID)
            && ma.active_style == Some(crate::modules::DEFAULT_SPECTRUM_STYLE_ID),
        "instances not independent",
    )?;
    action(&mut ws, WorkspaceAction::CloseModule { instance_id: b })?;
    action(&mut ws, WorkspaceAction::CloseModule { instance_id: a })?;
    let o = ws.observation();
    let root = o
        .window(FIRST_WINDOW)
        .ok_or("closing modules closed the window")?;
    check(
        root.leaves.is_empty() && o.modules.is_empty(),
        "last close left instances",
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
        // Selecting needs the settings window open.
        WorkspaceAction::SelectModule {
            instance_id: new_id,
        },
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
    h.settings(
        ControlKey::LeafBody(b),
        &[SettingsItem::Channel(ChannelSelection::Right)],
    )?;
    let o = h.ws.observation();
    check(
        o.module(b).and_then(|m| m.channel) == Some(ChannelSelection::Right)
            && o.module(a).and_then(|m| m.channel) == Some(ChannelSelection::MonoAverage),
        "Channel click wrong instance",
    )?;
    h.settings(
        ControlKey::LeafBody(b),
        &[SettingsItem::Style(crate::modules::PIXEL_SPECTRUM_STYLE_ID)],
    )?;
    check(
        h.ws.observation().module(b).and_then(|m| m.active_style)
            == Some(crate::modules::PIXEL_SPECTRUM_STYLE_ID),
        "style popup did not apply",
    )?;
    h.settings(ControlKey::LeafBody(b), &[SettingsItem::CloseModule])?;
    h.settings(ControlKey::LeafBody(a), &[SettingsItem::CloseModule])?;
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
    let button = h.locate(&ControlKey::AddOpen)?.rect;
    let window = h.locate(&ControlKey::EmptyBody)?.rect;
    check(
        (button.center() - window.center()).length() < 1.0 && button.width() < 120.0,
        &format!("Add module button not centered at its own size: {button:?} in {window:?}"),
    )?;
    h.dropdown(
        ControlKey::AddOpen,
        ControlKey::AddModule(SPECTRUM_MODULE_ID),
    )?;
    let c = first_leaf(&h.ws)?;
    check(c != a && c != b, "Add reused an instance")?;
    h.settings(
        ControlKey::LeafBody(c),
        &[SettingsItem::Tab(Tab::Workspace), SettingsItem::Pause],
    )?;
    check(
        h.ws.observation().source_status.state == SourceState::Paused
            && driver.advance_frames(160, deadline()).is_err(),
        "Pause entry failed",
    )?;
    h.settings(
        ControlKey::LeafBody(c),
        &[SettingsItem::Tab(Tab::Workspace), SettingsItem::Pause],
    )?;
    check(
        h.ws.observation().source_status.state == SourceState::Running,
        "Resume entry failed",
    )?;
    let old = h.locate(&ControlKey::LeafBody(c))?;
    // Shrinking the application window does not shrink the settings window.
    h.set_size(egui::vec2(120.0, 60.0));
    h.frame(vec![]);
    check(
        h.click_target(ControlKey::LeafBody(c), old).is_err(),
        "stale control accepted after resize",
    )?;
    h.settings(
        ControlKey::LeafBody(c),
        &[SettingsItem::Tab(Tab::Workspace), SettingsItem::Pause],
    )?;
    let settings_size =
        h.ws.settings_builder()
            .and_then(|builder| builder.inner_size);
    check(
        h.ws.observation().source_status.state == SourceState::Paused
            && settings_size.is_some_and(|size| size.x >= 280.0 && size.y >= 220.0),
        &format!(
            "tiny-window settings Pause target missed or the settings window is tiny: {settings_size:?}"
        ),
    )?;
    h.settings(
        ControlKey::LeafBody(c),
        &[SettingsItem::Tab(Tab::Workspace), SettingsItem::Pause],
    )?;
    check(
        h.ws.observation().source_status.state == SourceState::Running,
        "tiny-window settings Resume target missed",
    )?;
    // Escape in the settings window closes it; its controls are gone.
    h.focus = SETTINGS;
    h.key(egui::Key::Escape);
    h.focus = FIRST_WINDOW;
    h.frame(vec![]);
    check(
        !h.ws.settings_open()
            && h.ws.has_window(FIRST_WINDOW)
            && !h.windows.contains_key(&SETTINGS),
        "Escape did not close only the settings window",
    )?;
    h.ws.check_ownership()?;
    Ok(json!({"split":b.0,"added":c.0,"tiny_window":[120,60]}))
}

fn c06() -> Result<Value, String> {
    let (ws, _driver) = workspace()?;
    let mut h = Harness::new(ws);
    h.frame(vec![]);
    let mut non_channel = Vec::new();
    for type_id in [
        crate::modules::STEREOMETER_MODULE_ID,
        LEVEL_METER_MODULE_ID,
        crate::modules::LOUDNESS_METER_MODULE_ID,
        EMPTY_MODULE_ID,
    ] {
        let id = add_module(&mut h.ws, type_id)?;
        h.frame(vec![]);
        check(
            h.ws.observation()
                .module(id)
                .is_some_and(|module| module.channel.is_none()),
            &format!("{type_id} reports a Channel capability"),
        )?;
        h.settings(ControlKey::LeafBody(id), &[])?;
        let settings = h.ws.settings_targets();
        check(
            settings.contains_key(&ControlKey::Settings(SettingsItem::CloseModule))
                && !settings
                    .keys()
                    .any(|key| matches!(key, ControlKey::Settings(SettingsItem::Channel(_)))),
            &format!("{type_id} settings expose Channel or miss their entries"),
        )?;
        // Escape in the settings window closes only the settings window.
        h.focus = SETTINGS;
        h.key(egui::Key::Escape);
        h.focus = FIRST_WINDOW;
        h.frame(vec![]);
        check(
            h.ws.has_window(FIRST_WINDOW) && !h.ws.settings_open(),
            "Escape in the settings window closed a Workspace window or left settings open",
        )?;
        non_channel.push(json!({"type":type_id,"id":id.0}));
    }
    // Each press/release pair applies once and no key stays held into idle frames.
    h.key(egui::Key::Space);
    check(
        h.ws.observation().source_status.state == SourceState::Paused,
        "Space did not pause",
    )?;
    h.key(egui::Key::Space);
    for _ in 0..3 {
        h.frame(vec![]);
    }
    check(
        h.ws.observation().source_status.state == SourceState::Running
            && h.context().input(|input| input.keys_down.is_empty()),
        "Space did not resume, or left a held key",
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
    mono_h.settings(ControlKey::LeafBody(mono_leaf), &[])?;
    let mono_settings = mono_h.ws.settings_targets();
    check(
        !mono_settings.contains_key(&ControlKey::Settings(SettingsItem::Channel(
            ChannelSelection::Right,
        ))) && mono_settings.contains_key(&ControlKey::Settings(SettingsItem::Channel(
            ChannelSelection::Discrete(0),
        ))),
        "mono descriptor exposed Right or missed its channel",
    )?;
    h.key(egui::Key::Escape);
    check(
        h.ws.windows().len() == 1 && !h.ws.observation().modules.is_empty(),
        "idle Escape closed the window",
    )?;
    h.key_with(egui::Key::W, egui::Modifiers::COMMAND);
    check(
        h.ws.windows().is_empty() && h.ws.observation().modules.is_empty(),
        "Ctrl+W did not close the window and its modules",
    )?;
    Ok(json!({"non_channel_modules":non_channel,
        "escape":"settings window closes itself; idle Escape leaves this Workspace window open; Ctrl+W closes it; native close is the adapter's"}))
}

fn c07() -> Result<Value, String> {
    let (mut ws, mut driver) = workspace()?;
    replace_first(&mut ws, crate::modules::OSCILLOSCOPE_MODULE_ID)?;
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
        early.shapes.len() == late.shapes.len(),
        "Oscilloscope Line changed with display time alone",
    )?;
    check(
        handle
            .processed_through_frame()
            .map_err(|e| e.to_string())?
            == frontier
            && handle.metrics().publications == metrics.publications,
        "UI-only frame advanced audio",
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
        json!({"audio_frontier":frontier,"oscilloscope_shapes":[early.shapes.len(),late.shapes.len()],
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
    h.settings(ControlKey::LeafBody(empty), &[])?;
    let settings = h.ws.settings_targets();
    check(
        !settings
            .keys()
            .any(|key| matches!(key, ControlKey::Settings(SettingsItem::Style(_))))
            && settings.contains_key(&ControlKey::Settings(SettingsItem::TypePicker)),
        "Empty settings show style controls or miss the module type",
    )?;
    h.focus = SETTINGS;
    h.key(egui::Key::Escape);
    h.focus = FIRST_WINDOW;
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
