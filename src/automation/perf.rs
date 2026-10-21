//! Headless draws retain and release analysis results at 60 fps of audio time,
//! matching reader lifetimes in the product without measuring native windows or devices.

use std::time::{Duration, Instant};

use eframe::egui::vec2;
use serde_json::{Value, json};

use crate::{
    LiveAudio, LiveAudioConfig,
    audio::synthetic::SyntheticConfig,
    modules::{
        EMPTY_MODULE_ID, LEVEL_METER_MODULE_ID, LOUDNESS_METER_MODULE_ID, OSCILLOSCOPE_MODULE_ID,
        SPECTROGRAM_MODULE_ID, STEREOMETER_MODULE_ID, WAVEFORM_MODULE_ID,
    },
    workspace::Workspace,
};

use super::{
    harness::Harness,
    report::{Case, Report},
    support::{add_module, replace_first},
};

// A 10 ms WASAPI-sized callback at 48 kHz.
const BLOCK_FRAMES: usize = 480;
const DRAW_EVERY_FRAMES: u64 = 800;
/// Longer than the longest built-in History (Waveform, 10 s), so measurement starts
/// after every ring has filled and reached its steady size.
const WARMUP_FRAMES: u64 = 528_000;
const MEASURED_FRAMES: u64 = 1_440_000;

const ALL_OTHER_TYPES: [&str; 7] = [
    SPECTROGRAM_MODULE_ID,
    OSCILLOSCOPE_MODULE_ID,
    WAVEFORM_MODULE_ID,
    STEREOMETER_MODULE_ID,
    LEVEL_METER_MODULE_ID,
    LOUDNESS_METER_MODULE_ID,
    EMPTY_MODULE_ID,
];

type Setup = fn(&mut Workspace) -> Result<(), String>;

pub(super) fn suite(report: &mut Report) {
    let cases: [(&str, &str, Setup); 4] = [
        ("P01", "one Spectrum", |_| Ok(())),
        ("P02", "two Spectrograms", |ws| {
            replace_first(ws, SPECTROGRAM_MODULE_ID)?;
            add_module(ws, SPECTROGRAM_MODULE_ID).map(drop)
        }),
        ("P03", "all eight Module types", |ws| {
            for type_id in ALL_OTHER_TYPES {
                add_module(ws, type_id)?;
            }
            Ok(())
        }),
        ("P04", "all eight Module types, four times", |ws| {
            for round in 0..4 {
                if round > 0 {
                    add_module(ws, crate::modules::SPECTRUM_MODULE_ID)?;
                }
                for type_id in ALL_OTHER_TYPES {
                    add_module(ws, type_id)?;
                }
            }
            Ok(())
        }),
    ];
    for (id, label, setup) in cases {
        report.cases.push(match measure(label, setup) {
            Ok(observed) => Case::pass(id, observed),
            Err(error) => Case::fail(id, "measurement", error),
        });
    }
}

fn measure(label: &str, setup: Setup) -> Result<Value, String> {
    let synthetic = SyntheticConfig {
        block_frames: BLOCK_FRAMES,
        ..SyntheticConfig::sweep()
    };
    let (live, mut driver) =
        LiveAudio::open_manual(LiveAudioConfig::default(), synthetic).map_err(|e| e.to_string())?;
    let mut h = Harness::new(Workspace::new(live, false).map_err(|e| e.to_string())?);
    h.set_size(vec2(1800.0, 1000.0));
    h.frame(vec![]);
    setup(&mut h.ws)?;
    h.frame(vec![]);
    let modules = h.ws.observation().modules.len();
    let handle = h.ws.dataflow_handle_for_automation();
    let run = |h: &mut Harness, driver: &mut crate::audio::synthetic::ManualInputDriver, frames| {
        let mut done = 0;
        while done < frames {
            let step = DRAW_EVERY_FRAMES.min(frames - done);
            driver
                .advance_frames(step, Instant::now() + Duration::from_secs(10))
                .map_err(|e| e.to_string())?;
            h.frame(vec![]);
            done += step;
        }
        Ok::<_, String>(())
    };
    run(&mut h, &mut driver, WARMUP_FRAMES)?;
    let before = handle.worker_sample();
    let started = Instant::now();
    run(&mut h, &mut driver, MEASURED_FRAMES)?;
    let wall = started.elapsed();
    let after = handle.worker_sample();
    let audio_seconds = MEASURED_FRAMES as f64 / 48_000.0;
    let cpu_ms = match (before.cpu, after.cpu) {
        (Some(before), Some(after)) => {
            Some((after - before).as_secs_f64() * 1_000.0 / audio_seconds)
        }
        _ => None,
    };
    let megacycles = match (before.cycles, after.cycles) {
        (Some(before), Some(after)) => Some((after - before) as f64 / 1.0e6 / audio_seconds),
        _ => None,
    };
    Ok(json!({
        "label": label,
        "modules": modules,
        "block_frames": BLOCK_FRAMES,
        "draw_every_frames": DRAW_EVERY_FRAMES,
        "audio_seconds": audio_seconds,
        "wall_seconds": wall.as_secs_f64(),
        "worker_cpu_ms_per_audio_second": cpu_ms,
        "worker_megacycles_per_audio_second": megacycles,
        "worker_allocations_per_audio_second":
            (after.allocations - before.allocations) as f64 / audio_seconds,
    }))
}
