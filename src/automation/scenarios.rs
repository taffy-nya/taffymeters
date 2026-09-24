use std::{
    path::Path,
    time::{Duration, Instant},
};

use eframe::egui;
use serde_json::{Value, json};

use crate::{
    LiveAudio, LiveAudioConfig,
    app::{ActionOutcome, App, AppAction, ControlKey, ModuleInstanceId},
    audio::{
        ChannelLayout, SourceState,
        synthetic::{SignalSpec, SyntheticConfig},
    },
    builtins::{EMPTY_MODULE_ID, LEVEL_METER_MODULE_ID, SPECTRUM_MODULE_ID, WAVEFORM_MODULE_ID},
    dataflow::{ChannelSelection, FftConfig, SpectrumRequest, TimeDomainRequest},
};

use super::{
    deadline,
    harness::Harness,
    report::{Case, Report, Status},
    visual, wait_until,
};

fn check(condition: bool, message: &str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}
fn synthetic() -> Result<(LiveAudio, crate::audio::synthetic::ManualInputDriver), String> {
    LiveAudio::open_manual(LiveAudioConfig::default(), SyntheticConfig::default())
        .map_err(|e| e.to_string())
}
fn app() -> Result<(App, crate::audio::synthetic::ManualInputDriver), String> {
    let (live, driver) = synthetic()?;
    Ok((App::new(live).map_err(|e| e.to_string())?, driver))
}
fn created(outcome: ActionOutcome) -> Result<ModuleInstanceId, String> {
    match outcome {
        ActionOutcome::Created(id) => Ok(id),
        _ => Err("expected created instance".into()),
    }
}
fn action(app: &mut App, action: AppAction) -> Result<ActionOutcome, String> {
    app.apply_action(action).map_err(|e| e.to_string())
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
        handle.subscribe_time(TimeDomainRequest::Latest).is_err(),
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
    let mut left = SpectrumRequest::new(
        ChannelSelection::Left,
        FftConfig::new(2048, 512, -160.0).map_err(|e| e.to_string())?,
    );
    left.peak = None;
    let mut right = left;
    right.channel = ChannelSelection::Right;
    let a = handle.subscribe_spectrum(left).map_err(|e| e.to_string())?;
    let b = handle
        .subscribe_spectrum(right)
        .map_err(|e| e.to_string())?;
    driver
        .advance_frames(4096, deadline())
        .map_err(|e| e.to_string())?;
    let ra = a.reader();
    let rb = b.reader();
    let sa = wait_until(deadline(), "left FFT snapshot", || {
        ra.latest().filter(|s| {
            s.config_revision == ra.requested_revision() && s.processed_through_frame >= 4096
        })
    })?;
    let sb = wait_until(deadline(), "right FFT snapshot", || {
        rb.latest().filter(|s| {
            s.config_revision == rb.requested_revision() && s.processed_through_frame >= 4096
        })
    })?;
    check(
        sa.spectrum_dbfs[128] > sa.spectrum_dbfs[127]
            && sb.spectrum_dbfs[256] > sb.spectrum_dbfs[255],
        "wrong tone bins",
    )?;
    let expected = 20.0 * 0.08_f32.log10();
    check(
        (sa.spectrum_dbfs[128] - expected).abs() <= 0.02
            && (sb.spectrum_dbfs[256] - expected).abs() <= 0.02,
        "tone calibration error",
    )?;
    for _ in 0..6 {
        check(
            ra.latest()
                .is_some_and(|s| s.result_version == sa.result_version)
                && rb
                    .latest()
                    .is_some_and(|s| s.result_version == sb.result_version),
            "reading unchanged input advanced result version",
        )?;
    }
    let evaluations = handle.metrics().fft_evaluations;
    Ok((sa.spectrum_dbfs[128], sb.spectrum_dbfs[256], evaluations))
}

fn c02() -> Result<Value, String> {
    let first = fft_run(160)?;
    let second = fft_run(257)?;
    check(
        (first.0 - second.0).abs() <= 0.02 && (first.1 - second.1).abs() <= 0.02,
        "phase reset at block boundary",
    )?;
    let (live, mut driver) = synthetic()?;
    let subscription = live
        .dataflow_handle()
        .subscribe_time(TimeDomainRequest::Latest)
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
            .latest()
            .filter(|s| s.processed_through_frame >= 1600)
    })?;
    let actual: Vec<_> = snapshot
        .chunks
        .iter()
        .flat_map(|c| c.interleaved.iter().copied())
        .collect();
    check(
        actual == samples && snapshot.chunks.first().is_some_and(|c| c.start_frame == 0),
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
        .subscribe_time(TimeDomainRequest::Latest)
        .map_err(|e| e.to_string())?;
    generated_driver
        .advance_frames(1600, deadline())
        .map_err(|e| e.to_string())?;
    let generated_reader = generated_subscription.reader();
    let generated_snapshot = wait_until(deadline(), "impulse and indexed generator", || {
        generated_reader
            .latest()
            .filter(|s| s.processed_through_frame >= 1600)
    })?;
    let generated_samples: Vec<_> = generated_snapshot
        .chunks
        .iter()
        .flat_map(|chunk| chunk.interleaved.iter().copied())
        .collect();
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
    let history = handle
        .subscribe_time(TimeDomainRequest::History { seconds: 1.0 })
        .map_err(|e| e.to_string())?;
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
    driver
        .advance_frames(1600, deadline())
        .map_err(|e| e.to_string())?;
    let snapshot = wait_until(deadline(), "resume history", || {
        history
            .reader()
            .latest()
            .filter(|s| s.processed_through_frame >= 3200)
    })?;
    check(
        snapshot.discontinuity_count > 0,
        "resume did not mark discontinuity",
    )?;
    drop(live);
    check(
        driver.advance_frames(160, deadline()).is_err()
            && handle.processed_through_frame().is_err(),
        "old driver/handle survived stop",
    )?;
    Ok(json!({"frontier":3200,"discontinuities":snapshot.discontinuity_count}))
}

fn c04() -> Result<Value, String> {
    let (mut app, mut driver) = app()?;
    let a = app
        .observation()
        .active_module
        .ok_or("initial Spectrum missing")?;
    let b = created(action(
        &mut app,
        AppAction::AddModule {
            type_id: SPECTRUM_MODULE_ID,
        },
    )?)?;
    action(
        &mut app,
        AppAction::SetChannel {
            instance_id: b,
            channel: ChannelSelection::Right,
        },
    )?;
    action(
        &mut app,
        AppAction::SetStyle {
            instance_id: b,
            style_id: crate::builtins::PIXEL_SPECTRUM_STYLE_ID,
        },
    )?;
    let observation = app.observation();
    check(
        observation.modules.len() == 2
            && observation.modules[0].channel == Some(ChannelSelection::MonoAverage)
            && observation.modules[1].channel == Some(ChannelSelection::Right)
            && observation.modules[1].active_style
                == Some(crate::builtins::PIXEL_SPECTRUM_STYLE_ID),
        "instances not independent",
    )?;
    action(&mut app, AppAction::SelectModule { instance_id: a })?;
    action(&mut app, AppAction::SelectModule { instance_id: b })?;
    action(&mut app, AppAction::CloseModule { instance_id: b })?;
    action(&mut app, AppAction::CloseModule { instance_id: a })?;
    check(
        app.observation().active_module.is_none(),
        "last close left selection",
    )?;
    action(&mut app, AppAction::TogglePause)?;
    check(
        app.observation().source_status.state == SourceState::Paused
            && driver.advance_frames(160, deadline()).is_err(),
        "zero-module pause did not stop source",
    )?;
    action(&mut app, AppAction::TogglePause)?;
    let new_id = created(action(
        &mut app,
        AppAction::AddModule {
            type_id: SPECTRUM_MODULE_ID,
        },
    )?)?;
    check(new_id != a && new_id != b, "reused instance id")?;
    let before = app.observation();
    check(
        action(&mut app, AppAction::SelectModule { instance_id: b }).is_err()
            && action(
                &mut app,
                AppAction::AddModule {
                    type_id: "invalid.module",
                },
            )
            .is_err()
            && action(
                &mut app,
                AppAction::SetChannel {
                    instance_id: new_id,
                    channel: ChannelSelection::Discrete(9),
                },
            )
            .is_err()
            && action(
                &mut app,
                AppAction::SetStyle {
                    instance_id: new_id,
                    style_id: "invalid.style",
                },
            )
            .is_err(),
        "invalid action accepted",
    )?;
    let after = app.observation();
    check(
        after.active_module == before.active_module
            && after.modules.len() == before.modules.len()
            && after.modules[0].channel == before.modules[0].channel
            && after.modules[0].active_style == before.modules[0].active_style,
        "failed action partially committed",
    )?;
    Ok(json!({"original_ids":[a.0,b.0],"new_id":new_id.0,"source":"running"}))
}

fn c05() -> Result<Value, String> {
    let (app, mut driver) = app()?;
    let mut h = Harness::new(app);
    h.frame(vec![]);
    let a = h
        .app
        .observation()
        .active_module
        .ok_or("initial module absent")?;
    h.dropdown(
        ControlKey::ModuleAddOpen,
        ControlKey::ModuleAddItem(SPECTRUM_MODULE_ID),
    )?;
    let b = h
        .app
        .observation()
        .active_module
        .ok_or("add popup did not create")?;
    check(
        a != b && h.app.observation().modules.len() == 2,
        "Add click did not add instance",
    )?;
    h.dropdown(
        ControlKey::ModuleSelectOpen,
        ControlKey::ModuleSelectInstance(a),
    )?;
    check(
        h.app.observation().active_module == Some(a),
        "selector did not select A",
    )?;
    h.dropdown(
        ControlKey::ModuleSelectOpen,
        ControlKey::ModuleSelectInstance(b),
    )?;
    h.click(ControlKey::Channel(ChannelSelection::Right))?;
    check(
        h.app.observation().modules[1].channel == Some(ChannelSelection::Right)
            && h.app.observation().modules[0].channel == Some(ChannelSelection::MonoAverage),
        "Channel click wrong instance",
    )?;
    h.dropdown(
        ControlKey::StyleOpen(b),
        ControlKey::StyleOption {
            instance_id: b,
            style_id: crate::builtins::PIXEL_SPECTRUM_STYLE_ID,
        },
    )?;
    check(
        h.app.observation().modules[1].active_style
            == Some(crate::builtins::PIXEL_SPECTRUM_STYLE_ID),
        "style popup did not apply",
    )?;
    h.click(ControlKey::ModuleClose)?;
    h.click(ControlKey::ModuleClose)?;
    let remaining = h.app.observation().modules.len();
    check(
        remaining == 0,
        &format!("Close clicks left {remaining} modules"),
    )?;
    // The release pass recorded the pre-action Response; query the settled layout.
    h.frame(vec![]);
    let close = h.control_state(&ControlKey::ModuleClose);
    check(
        matches!(close, Some(false)),
        &format!("zero-module Close should be disabled, got {close:?}"),
    )?;
    h.dropdown(
        ControlKey::ModuleAddOpen,
        ControlKey::ModuleAddItem(SPECTRUM_MODULE_ID),
    )?;
    h.click(ControlKey::SourcePause)?;
    check(
        h.app.observation().source_status.state == SourceState::Paused
            && driver.advance_frames(160, deadline()).is_err(),
        "Pause button failed",
    )?;
    h.click(ControlKey::SourcePause)?;
    check(
        h.app.observation().source_status.state == SourceState::Running,
        "Resume button failed",
    )?;
    let old = h.locate(&ControlKey::ModuleClose)?;
    h.width = 480.0;
    h.height = 420.0;
    h.frame(vec![]);
    check(
        h.click_target(ControlKey::ModuleClose, old).is_err(),
        "stale control accepted after resize",
    )?;
    check(
        h.locate(&ControlKey::StyleOption {
            instance_id: b,
            style_id: crate::builtins::PIXEL_SPECTRUM_STYLE_ID,
        })
        .is_err(),
        "invisible popup item incorrectly available",
    )?;
    h.click(ControlKey::SourcePause)?;
    check(
        h.app.observation().source_status.state == SourceState::Paused,
        "narrow-layout Pause target missed",
    )?;
    h.click(ControlKey::SourcePause)?;
    check(
        h.app.observation().source_status.state == SourceState::Running,
        "narrow-layout Resume target missed",
    )?;
    Ok(
        json!({"created":b.0,"restored_instances":h.app.observation().modules.len(),"narrow_size":[480,420]}),
    )
}

fn c06() -> Result<Value, String> {
    let (app, _driver) = app()?;
    let mut h = Harness::new(app);
    h.frame(vec![]);
    let a = h.app.observation().active_module.ok_or("no spectrum")?;
    h.key(egui::Key::Num2);
    check(
        h.app.observation().modules[0].channel == Some(ChannelSelection::Right),
        "Num2 not routed",
    )?;
    h.key(egui::Key::Num1);
    check(
        h.app.observation().modules[0].channel == Some(ChannelSelection::Left),
        "Num1 not routed",
    )?;
    h.key(egui::Key::M);
    check(
        h.app.observation().modules[0].channel == Some(ChannelSelection::MonoAverage),
        "M not routed",
    )?;
    let empty = created(action(
        &mut h.app,
        AppAction::AddModule {
            type_id: EMPTY_MODULE_ID,
        },
    )?)?;
    h.frame(vec![]);
    check(
        h.locate(&ControlKey::Channel(ChannelSelection::Right))
            .is_err(),
        "unsupported module exposes channel",
    )?;
    h.key(egui::Key::Num2);
    check(
        h.app.observation().modules[0].channel == Some(ChannelSelection::MonoAverage),
        "shortcut touched inactive instance",
    )?;
    h.key(egui::Key::Space);
    check(
        h.app.observation().source_status.state == SourceState::Paused,
        "Space did not pause",
    )?;
    h.key(egui::Key::Space);
    check(
        h.app.observation().source_status.state == SourceState::Running,
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
    let mut mono_h = Harness::new(App::new(mono_live).map_err(|e| e.to_string())?);
    mono_h.frame(vec![]);
    check(
        mono_h
            .locate(&ControlKey::Channel(ChannelSelection::Right))
            .is_err(),
        "mono descriptor exposed Right",
    )?;
    mono_h.key(egui::Key::Num2);
    check(
        mono_h.app.observation().modules[0].channel == Some(ChannelSelection::MonoAverage),
        "mono Num2 selected missing channel",
    )?;
    let output = h.frame(vec![egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }]);
    check(
        output.viewport_output.values().any(|v| {
            v.commands
                .iter()
                .any(|cmd| matches!(cmd, egui::ViewportCommand::Close))
        }),
        "Escape did not issue viewport Close",
    )?;
    Ok(json!({"spectrum":a.0,"non_channel_module":empty.0,"escape":"viewport command only"}))
}

fn c07() -> Result<Value, String> {
    let (mut app, mut driver) = app()?;
    let osc = created(action(
        &mut app,
        AppAction::AddModule {
            type_id: crate::builtins::OSCILLOSCOPE_MODULE_ID,
        },
    )?)?;
    action(
        &mut app,
        AppAction::SetStyle {
            instance_id: osc,
            style_id: crate::builtins::PERSISTENCE_OSCILLOSCOPE_STYLE_ID,
        },
    )?;
    let handle = app.dataflow_handle_for_automation();
    let mut h = Harness::new(app);
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
            && handle.metrics().time_publications == metrics.time_publications,
        "UI-only frame advanced audio",
    )?;
    driver
        .advance_frames(1600, deadline())
        .map_err(|e| e.to_string())?;
    let with_trace = h.frame(vec![]);
    let published = handle.metrics().time_publications;
    h.click(ControlKey::VisualClear(osc))?;
    let after_clear = h.frame(vec![]);
    check(
        with_trace.shapes.len() > after_clear.shapes.len()
            && handle.metrics().time_publications == published,
        "Clear did not remove trace without resetting audio",
    )?;
    let waveform = created(action(
        &mut h.app,
        AppAction::AddModule {
            type_id: WAVEFORM_MODULE_ID,
        },
    )?)?;
    action(&mut h.app, AppAction::SelectModule { instance_id: osc })?;
    h.frame(vec![]);
    let before = handle.metrics().time_pcm_frames;
    for _ in 0..5 {
        driver
            .advance_frames(48_000, deadline())
            .map_err(|e| e.to_string())?;
    }
    check(
        handle.metrics().time_pcm_frames > before,
        "hidden Waveform stopped History PCM ingestion",
    )?;
    action(
        &mut h.app,
        AppAction::SelectModule {
            instance_id: waveform,
        },
    )?;
    let visible = h.frame(vec![]);
    check(
        visible.shapes.len() > late.shapes.len(),
        "resumed Waveform did not draw retained PCM",
    )?;
    Ok(
        json!({"audio_frontier":frontier,"persistence_shapes":[early.shapes.len(),late.shapes.len()],
        "hidden_history_pcm_frames":handle.metrics().time_pcm_frames-before,"restored_shapes":visible.shapes.len()}),
    )
}

fn c08() -> Result<Value, String> {
    let (mut app, mut driver) = app()?;
    let handle = app.dataflow_handle_for_automation();
    let spectrum = app.observation().active_module.ok_or("no spectrum")?;
    let waveform = created(action(
        &mut app,
        AppAction::AddModule {
            type_id: WAVEFORM_MODULE_ID,
        },
    )?)?;
    let levels = created(action(
        &mut app,
        AppAction::AddModule {
            type_id: LEVEL_METER_MODULE_ID,
        },
    )?)?;
    driver
        .advance_frames(3200, deadline())
        .map_err(|e| e.to_string())?;
    let both = handle.metrics();
    check(
        both.time_pcm_frames > 0 && both.time_level_frames > 0,
        "mixed time needs did not compute",
    )?;
    action(
        &mut app,
        AppAction::CloseModule {
            instance_id: waveform,
        },
    )?;
    // The next drained block follows queued removal commands; the frequency-only
    // `subscriptions` metric does not count time-domain consumers.
    let before = handle.metrics();
    driver
        .advance_frames(3200, deadline())
        .map_err(|e| e.to_string())?;
    let only_levels = handle.metrics();
    check(
        only_levels.time_pcm_frames == before.time_pcm_frames
            && only_levels.time_level_frames > before.time_level_frames,
        "PCM close affected Levels",
    )?;
    let new_waveform = created(action(
        &mut app,
        AppAction::AddModule {
            type_id: WAVEFORM_MODULE_ID,
        },
    )?)?;
    action(
        &mut app,
        AppAction::CloseModule {
            instance_id: levels,
        },
    )?;
    let before = handle.metrics();
    driver
        .advance_frames(3200, deadline())
        .map_err(|e| e.to_string())?;
    let only_pcm = handle.metrics();
    check(
        only_pcm.time_pcm_frames > before.time_pcm_frames
            && only_pcm.time_level_frames == before.time_level_frames,
        "Levels close affected PCM",
    )?;
    action(
        &mut app,
        AppAction::CloseModule {
            instance_id: new_waveform,
        },
    )?;
    action(
        &mut app,
        AppAction::CloseModule {
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
            &mut app,
            AppAction::AddModule {
                type_id: WAVEFORM_MODULE_ID,
            },
        )?)?;
        action(&mut app, AppAction::CloseModule { instance_id: id })?;
    }
    let after_cycles = handle.metrics();
    driver
        .advance_frames(1600, deadline())
        .map_err(|e| e.to_string())?;
    check(
        handle.metrics() == after_cycles,
        "create/close cycles retained analysis demand",
    )?;
    Ok(
        json!({"pcm_frames":both.time_pcm_frames,"level_frames":both.time_level_frames,"cycles":20,"active_subscriptions":handle.metrics().subscriptions}),
    )
}

fn c09() -> Result<Value, String> {
    let (mut app, mut driver) = app()?;
    let id = app.observation().active_module.ok_or("no module")?;
    action(&mut app, AppAction::TogglePause)?;
    check(
        driver.advance_frames(160, deadline()).is_err(),
        "paused input accepted",
    )?;
    action(&mut app, AppAction::TogglePause)?;
    check(
        action(
            &mut app,
            AppAction::SelectModule {
                instance_id: ModuleInstanceId(u64::MAX),
            },
        )
        .is_err(),
        "invalid ID accepted",
    )?;
    let empty = created(action(
        &mut app,
        AppAction::AddModule {
            type_id: EMPTY_MODULE_ID,
        },
    )?)?;
    check(
        action(
            &mut app,
            AppAction::SetChannel {
                instance_id: empty,
                channel: ChannelSelection::Right,
            },
        )
        .is_err(),
        "unsupported channel accepted",
    )?;
    let mut h = Harness::new(app);
    h.frame(vec![]);
    check(
        h.locate(&ControlKey::StyleOpen(id)).is_err(),
        "hidden style control present",
    )?;
    let old = h.locate(&ControlKey::ModuleClose)?;
    h.frame(vec![]);
    check(
        h.click_target(ControlKey::ModuleClose, old).is_err(),
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
    check(
        image::load_from_memory_with_format(b"bad png", image::ImageFormat::Png).is_err(),
        "bad PNG decoded",
    )?;
    Ok(
        json!({"changed_pixels":diff.changed_pixels,"bad_png":"rejected","dimension_mismatch":"rejected"}),
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
    renderer
        .render(&h.context, first, h.width as u32, h.height as u32)
        .map_err(|e| e.to_string())
}

fn image_case(
    renderer: &mut visual::OffscreenRenderer,
    id: &str,
    type_id: &'static str,
    style: Option<&'static str>,
    dpi: f32,
    output: &Path,
    baseline: Option<&Path>,
) -> Result<(Case, Value), String> {
    let (mut app, mut driver) = app()?;
    if type_id != SPECTRUM_MODULE_ID {
        action(&mut app, AppAction::AddModule { type_id })?;
    }
    if let Some(style_id) = style {
        let instance_id = app.observation().active_module.ok_or("missing module")?;
        action(
            &mut app,
            AppAction::SetStyle {
                instance_id,
                style_id,
            },
        )?;
    }
    let handle = app.dataflow_handle_for_automation();
    let mut h = Harness::new(app);
    h.ppp = dpi;
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
        "source": {"id":h.app.source_descriptor_for_automation().source_id.0,
        "generation":h.app.source_descriptor_for_automation().generation.0}, "delivered_through_frame":48_000,
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
    let approved_baseline = baseline.filter(|base| base.join("approval.json").is_file());
    let mut case = if let Some(base) = approved_baseline {
        let approved: Value = serde_json::from_slice(
            &std::fs::read(base.join("approval.json"))
                .map_err(|e| format!("baseline approval: {e}"))?,
        )
        .map_err(|e| e.to_string())?;
        check(
            approved.get("reviewer").and_then(Value::as_str).is_some()
                && approved.get("date").and_then(Value::as_str).is_some()
                && approved.get("reason").and_then(Value::as_str).is_some(),
            "baseline lacks reviewer/date/reason",
        )?;
        let previous: Value = serde_json::from_slice(
            &std::fs::read(base.join(format!("{id}.json"))).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        check(
            approved["candidate_hashes"][id] == previous["pixel_sha256"]
                && previous["pixel_sha256"].as_str().is_some(),
            "baseline approval hash mismatches manifest",
        )?;
        let comparable = [
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
        if comparable
            .iter()
            .any(|field| previous[*field] != manifest[*field])
        {
            Case::incomplete(id, Status::NotComparable, "baseline profile differs")
        } else {
            let base_image = base.join(format!("{id}.png"));
            match image::open(&base_image) {
                Ok(expected) => {
                    let expected = expected.to_rgba8();
                    check(
                        super::report::digest(expected.as_raw()) == previous["pixel_sha256"],
                        "baseline PNG hash mismatches manifest",
                    )?;
                    match visual::compare_rgba(rgba_view(&expected), rgba_view(&image)) {
                        Ok(metrics)
                            if metrics.dimensions_match && metrics.changed_pixels == Some(0) =>
                        {
                            Case::pass(id, json!({"comparison":metrics,"pixel_sha256":hash}))
                        }
                        Ok(metrics) => {
                            let mut fail =
                                Case::fail(id, "pixel comparison", format!("{metrics:?}"));
                            fail.observed = json!({"comparison":metrics});
                            fail
                        }
                        Err(e) => Case::fail(id, "pixel comparison", e),
                    }
                }
                Err(e) => Case::fail(id, "baseline decode", e),
            }
        }
    } else {
        Case::incomplete(
            id,
            Status::NeedsReview,
            "candidate requires human baseline review",
        )
    };
    // Retain the actual image when no baseline or when comparison failed.
    if approved_baseline.is_none() || case.status != Status::Pass {
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

fn repeat_frame(renderer: &mut visual::OffscreenRenderer) -> Result<image::RgbaImage, String> {
    let (app, mut driver) = app()?;
    let handle = app.dataflow_handle_for_automation();
    let mut h = Harness::new(app);
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
    let (mut app, mut driver) = app()?;
    let osc = created(action(
        &mut app,
        AppAction::AddModule {
            type_id: crate::builtins::OSCILLOSCOPE_MODULE_ID,
        },
    )?)?;
    action(
        &mut app,
        AppAction::SetStyle {
            instance_id: osc,
            style_id: crate::builtins::PERSISTENCE_OSCILLOSCOPE_STYLE_ID,
        },
    )?;
    let handle = app.dataflow_handle_for_automation();
    let mut h = Harness::new(app);
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
            "source":h.app.source_descriptor_for_automation().source_id.0,"generation":h.app.source_descriptor_for_automation().generation.0,
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
    let mut renderer = match visual::OffscreenRenderer::new() {
        Ok(renderer) => renderer,
        Err(visual::VisualError::Initialization { stage, message }) => {
            report.cases.push(Case::incomplete(
                "V01",
                Status::Unavailable,
                format!("{stage}: {message}"),
            ));
            for id in ["V02", "V03", "V04", "V05"] {
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
            for id in ["V02", "V03", "V04", "V05"] {
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
        SPECTRUM_MODULE_ID,
        None,
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
                match image_case(&mut renderer, id, type_id, style, 1.0, &images, baseline) {
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
            for id in ["V02", "V03", "V04", "V05"] {
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
            SPECTRUM_MODULE_ID,
            None,
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
    report
        .cases
        .push(match persistence_visual(&mut renderer, &images, baseline) {
            Ok(case) => case,
            Err(error) => Case::fail("V03-persistence", "fixed-time render", error),
        });
    report.cases.push(
        match (repeat_frame(&mut renderer), repeat_frame(&mut renderer)) {
            (Ok(a), Ok(b)) => match visual::compare_rgba(rgba_view(&a), rgba_view(&b)) {
                Ok(metrics) if metrics.changed_pixels == Some(0) => Case::pass(
                    "V04",
                    json!({"repeat":metrics,"pixel_sha256":super::report::digest(a.as_raw())}),
                ),
                Ok(metrics) => {
                    Case::fail("V04", "independent scene repeat", format!("{metrics:?}"))
                }
                Err(error) => Case::fail("V04", "compare", error),
            },
            (a, b) => Case::fail(
                "V04",
                "repeat capture",
                format!("first: {:?}, second: {:?}", a.err(), b.err()),
            ),
        },
    );
    report.cases.push(if let Some(base) = baseline {
        if base.join("approval.json").is_file() {
            Case::pass("V05", json!({"baseline":"read-only approved"}))
        } else {
            Case::incomplete(
                "V05",
                Status::NeedsReview,
                "baseline exists but approval.json is missing",
            )
        }
    } else {
        Case::incomplete(
            "V05",
            Status::NeedsReview,
            "no approved baseline; never auto-accepted",
        )
    });
}
