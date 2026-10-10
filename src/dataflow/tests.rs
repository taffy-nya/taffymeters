//! Runtime behavior through the real transport, worker and readers: sharing,
//! state scopes, reconfiguration, demand, continuity and release.

use std::{
    f32::consts::PI,
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use super::*;
use crate::{
    audio::{
        AudioDescriptor, CaptureEndpoint, ChannelLayout, Generation, SourceId, SourceTelemetry,
        create_block_transport,
    },
    snapshot::{DataReader, Frame, HistoryView, Snapshot},
};

struct TestDataflow {
    _owner: Dataflow,
    handle: DataflowHandle,
}

impl TestDataflow {
    fn subscribe(&self, spec: DataSpec) -> DataSubscription {
        self.handle.subscribe(spec).expect("valid subscription")
    }

    fn metrics(&self) -> RuntimeMetrics {
        self.handle.metrics()
    }
}

fn fixture(
    channels: u16,
    block_count: usize,
    max_block_frames: usize,
) -> (CaptureEndpoint, TestDataflow, Arc<SourceTelemetry>) {
    let layout = match channels {
        1 => ChannelLayout::Mono,
        2 => ChannelLayout::Stereo,
        _ => ChannelLayout::UnknownDiscrete,
    };
    let descriptor = Arc::new(
        AudioDescriptor::new(SourceId(1), Generation(7), 48_000, channels, layout).unwrap(),
    );
    let telemetry = Arc::new(SourceTelemetry::new());
    let (mut capture, processing) = create_block_transport(
        channels as usize,
        block_count,
        max_block_frames,
        Arc::clone(&telemetry),
    )
    .unwrap();
    let (owner, handle) = Dataflow::spawn(descriptor, processing, Arc::clone(&telemetry));
    capture.set_worker(owner.worker_thread());
    (
        capture,
        TestDataflow {
            _owner: owner,
            handle,
        },
        telemetry,
    )
}

fn fft32() -> FftConfig {
    FftConfig::new(32, 16, -160.0).unwrap()
}

/// `Channel(Mono) → Fft(32/16)`; returns the spec and the FFT node.
fn fft_spec(fft: FftConfig) -> (DataSpec, NodeRef) {
    let mut spec = DataSpec::new();
    let channel = spec.add(SOURCE, NodeSpec::Channel(ChannelSelection::MonoAverage));
    let fft = spec.add(channel, NodeSpec::Fft(fft));
    (spec, fft)
}

fn peak(hold_ms: f32) -> NodeSpec {
    NodeSpec::Peak(PeakConfig::new(hold_ms, 12.0).unwrap())
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(2)
}

fn wait<T>(label: &str, mut probe: impl FnMut() -> Option<T>) -> T {
    let deadline = deadline();
    loop {
        if let Some(value) = probe() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {label}");
        thread::yield_now();
    }
}

fn latest_at(reader: &DataReader, output: OutputId, frame: u64) -> Arc<Frame> {
    wait(&format!("result at {frame}"), || {
        reader
            .latest(output)
            .filter(|latest| latest.processed_through_frame >= frame)
    })
}

fn history_at(reader: &DataReader, output: OutputId, view: &mut HistoryView, frame: u64) {
    wait(&format!("history at {frame}"), || {
        reader.sync_history(output, view);
        (view.newest_frontier() >= Some(frame)).then_some(())
    });
}

fn settle(
    dataflow: &TestDataflow,
    label: &str,
    done: impl Fn(&RuntimeMetrics) -> bool,
) -> RuntimeMetrics {
    wait(label, || {
        Some(dataflow.metrics()).filter(|metrics| done(metrics))
    })
}

fn bins(frame: &Frame) -> &[f32] {
    &frame.spectrum().expect("spectrum frame").bins_dbfs
}

fn mono_tone(frames: usize, frequency: f32, amplitude: f32) -> Vec<f32> {
    (0..frames)
        .map(|frame| amplitude * (2.0 * PI * frequency * frame as f32 / 48_000.0).sin())
        .collect()
}

fn stereo(start_frame: u64, frames: usize) -> Vec<f32> {
    (0..frames)
        .flat_map(|frame| {
            let phase = 2.0 * PI * 1_000.0 * (start_frame + frame as u64) as f32 / 48_000.0;
            [phase.sin() * 0.5, phase.cos() * 0.25]
        })
        .collect()
}

fn retry_busy(mut operation: impl FnMut() -> Result<(), ControlError>) {
    loop {
        match operation() {
            Ok(()) => return,
            Err(ControlError::Busy) => thread::yield_now(),
            Err(error) => panic!("unexpected control error: {error}"),
        }
    }
}

/// Write interleaved stereo in pieces the transport can hold, waiting for each piece
/// to be processed, so no block is dropped.
fn feed(capture: &mut CaptureEndpoint, dataflow: &TestDataflow, samples: &[f32]) {
    for piece in samples.chunks(2 * 512) {
        let target = dataflow.handle.processed_through_frame().unwrap() + (piece.len() / 2) as u64;
        capture.write(piece);
        wait("piece processed", || {
            (dataflow.handle.processed_through_frame().unwrap() >= target).then_some(())
        });
    }
}

// ---- sharing, state scopes and reconfiguration -----------------------------------

#[test]
fn identical_pure_chains_share_one_fft_and_reads_do_not_advance() {
    let (mut capture, dataflow, _) = fixture(1, 8, 4096);
    let (mut spec, fft) = fft_spec(fft32());
    let output = spec.output(fft, Keep::Latest);
    let first = dataflow.subscribe(spec.clone());
    let second = dataflow.subscribe(spec);
    capture.write(&mono_tone(64, 3_000.0, 0.25));
    let left = latest_at(&first.reader(), output, 64);
    let right = latest_at(&second.reader(), output, 64);
    assert!(Arc::ptr_eq(&left, &right), "one shared result");
    let metrics = dataflow.metrics();
    assert_eq!(metrics.fft_evaluations, 3, "windows end at 32, 48 and 64");
    assert_eq!(
        (metrics.active_channel_nodes, metrics.active_fft_nodes),
        (1, 1)
    );
    for _ in 0..100 {
        assert_eq!(
            first.reader().latest(output).unwrap().result_version,
            left.result_version
        );
    }
    assert_eq!(
        dataflow.metrics(),
        metrics,
        "reading does not advance analysis"
    );
}

#[test]
fn changing_one_fft_forks_without_disturbing_the_other_consumer() {
    let (mut capture, dataflow, _) = fixture(1, 8, 128);
    let (mut spec, fft) = fft_spec(fft32());
    let output = spec.output(fft, Keep::Latest);
    let first = dataflow.subscribe(spec.clone());
    let second = dataflow.subscribe(spec);
    capture.write(&mono_tone(64, 3_000.0, 0.25));
    let before = latest_at(&second.reader(), output, 64);
    first
        .controller()
        .update(|spec| spec.set_node(fft, NodeSpec::Fft(FftConfig::new(64, 32, -160.0).unwrap())))
        .unwrap();
    assert_eq!(first.controller().revision(), 2);
    capture.write(&mono_tone(128, 3_000.0, 0.25));
    let changed = latest_at(&first.reader(), output, 192);
    let unchanged = latest_at(&second.reader(), output, 192);
    assert_eq!(changed.spectrum().unwrap().fft.window_size, 64);
    assert_eq!(unchanged.spectrum().unwrap().fft.window_size, 32);
    assert!(unchanged.result_version > before.result_version);
    let metrics = dataflow.metrics();
    assert_eq!(
        (metrics.active_channel_nodes, metrics.active_fft_nodes),
        (1, 2),
        "the channel prefix stays shared"
    );
}

#[test]
fn shared_state_advances_once_and_private_reset_is_isolated() {
    let (mut capture, dataflow, _) = fixture(1, 8, 128);
    let (mut spec, fft) = fft_spec(fft32());
    let peak_node = spec.add(fft, peak(20.0));
    let output = spec.output(peak_node, Keep::Seconds(1.0));
    let mut shared_spec = spec.clone();
    shared_spec.bind(output, Some(SharedAnalysisId(11)));
    let shared_a = dataflow.subscribe(shared_spec.clone());
    let shared_b = dataflow.subscribe(shared_spec);
    capture.write(&mono_tone(64, 3_000.0, 0.25));
    let a = latest_at(&shared_a.reader(), output, 64);
    let b = latest_at(&shared_b.reader(), output, 64);
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(
        dataflow.metrics().spectrum_evaluations,
        3,
        "a shared Peak advances once per hop, not per reader"
    );

    drop(shared_a);
    settle(&dataflow, "one subscription", |m| m.subscriptions == 1);
    capture.write(&mono_tone(16, 3_000.0, 0.25));
    latest_at(&shared_b.reader(), output, 80);

    let private_a = dataflow.subscribe(spec.clone());
    let private_b = dataflow.subscribe(spec);
    assert_eq!(
        dataflow.metrics().active_spectrum_nodes,
        3,
        "two private Peaks"
    );
    capture.write(&mono_tone(32, 3_000.0, 0.25));
    let (mut view_a, mut view_b) = (HistoryView::new(), HistoryView::new());
    history_at(&private_a.reader(), output, &mut view_a, 112);
    history_at(&private_b.reader(), output, &mut view_b, 112);
    let before = view_b.len();
    private_a.controller().reset_state().unwrap();
    capture.write(&mono_tone(48, 3_000.0, 0.25));
    history_at(&private_a.reader(), output, &mut view_a, 160);
    history_at(&private_b.reader(), output, &mut view_b, 160);
    assert!(
        view_a.len() < view_b.len(),
        "reset restarts only A's History"
    );
    assert!(view_b.len() > before, "B keeps its History");
}

#[test]
fn a_binding_shares_one_peak_while_main_curves_differ() {
    let (mut capture, dataflow, _) = fixture(1, 8, 128);
    let spec = |tilt: f32, hold: f32| {
        let (mut spec, fft) = fft_spec(fft32());
        let main = spec.add(
            fft,
            NodeSpec::Tilt {
                db_per_octave: tilt,
            },
        );
        let peak_node = spec.add(fft, peak(hold));
        let main = spec.output(main, Keep::Latest);
        let peak_output = spec.output(peak_node, Keep::Latest);
        spec.bind(peak_output, Some(SharedAnalysisId(5)));
        (spec, main, peak_output)
    };
    let (flat, main, peak_output) = spec(0.0, 100.0);
    let (tilted, _, _) = spec(3.0, 100.0);
    let a = dataflow.subscribe(flat);
    let b = dataflow.subscribe(tilted);
    assert_eq!(
        dataflow.metrics().active_spectrum_nodes,
        3,
        "two Tilts, one shared Peak"
    );
    capture.write(&mono_tone(64, 3_000.0, 0.25));
    let peak_a = latest_at(&a.reader(), peak_output, 64);
    let peak_b = latest_at(&b.reader(), peak_output, 64);
    assert!(Arc::ptr_eq(&peak_a, &peak_b));
    let main_a = latest_at(&a.reader(), main, 64);
    let main_b = latest_at(&b.reader(), main, 64);
    assert!(
        (bins(&main_a)[8] - bins(&main_b)[8]).abs() > 1.0,
        "own curves"
    );

    let (different, _, _) = spec(0.0, 500.0);
    assert!(matches!(
        dataflow.handle.subscribe(different),
        Err(ControlError::SharedBindingMismatch(SharedAnalysisId(5)))
    ));
}

#[test]
fn shared_updates_change_every_member_at_once_and_local_updates_fork() {
    let (mut capture, dataflow, _) = fixture(1, 8, 128);
    let (mut spec, fft) = fft_spec(fft32());
    let peak_node = spec.add(fft, peak(100.0));
    let output = spec.output(peak_node, Keep::Latest);
    spec.bind(output, Some(SharedAnalysisId(9)));
    let a = dataflow.subscribe(spec.clone());
    let b = dataflow.subscribe(spec);

    dataflow
        .handle
        .update_shared(SharedAnalysisId(9), |spec| {
            spec.set_node(peak_node, peak(300.0))
        })
        .unwrap();
    for member in [&a, &b] {
        assert_eq!(
            member.controller().spec().node(peak_node),
            Some(peak(300.0))
        );
        assert_eq!(member.controller().revision(), 2);
    }
    assert_eq!(
        dataflow.metrics().active_spectrum_nodes,
        1,
        "still one shared Peak"
    );
    let invalid = dataflow.handle.update_shared(SharedAnalysisId(9), |spec| {
        spec.set_node(
            peak_node,
            NodeSpec::Peak(PeakConfig {
                hold_ms: f32::NAN,
                decay_db_per_second: 1.0,
            }),
        )
    });
    assert!(matches!(invalid, Err(ControlError::InvalidSpec(_))));
    assert_eq!(
        b.controller().revision(),
        2,
        "a failed shared update changes nothing"
    );

    a.controller()
        .update(|spec| spec.set_node(peak_node, peak(50.0)))
        .unwrap();
    assert_eq!(
        a.controller().spec().outputs()[0].binding,
        None,
        "a local edit of a shared output forks"
    );
    assert_eq!(
        b.controller().spec().outputs()[0].binding,
        Some(SharedAnalysisId(9))
    );
    assert_eq!(dataflow.metrics().active_spectrum_nodes, 2);
    capture.write(&mono_tone(64, 3_000.0, 0.25));
    let forked = latest_at(&a.reader(), output, 64);
    let kept = latest_at(&b.reader(), output, 64);
    assert!(!Arc::ptr_eq(&forked, &kept));
}

#[test]
fn gain_change_keeps_upstream_smooth_and_peak_but_restarts_history() {
    let (mut capture, dataflow, _) = fixture(1, 8, 128);
    let (mut spec, fft) = fft_spec(fft32());
    let smooth = spec.add(
        fft,
        NodeSpec::Smooth(SmoothConfig::new(0.0, 1_000.0).unwrap()),
    );
    let gain = spec.add(smooth, NodeSpec::Gain { db: 0.0 });
    let peak_node = spec.add(fft, peak(1_000.0));
    let history = spec.output(gain, Keep::Seconds(1.0));
    let peak_output = spec.output(peak_node, Keep::Latest);
    let changed = dataflow.subscribe(spec.clone());
    let control = dataflow.subscribe(spec);
    capture.write(&mono_tone(64, 3_000.0, 0.5));
    let (mut changed_view, mut control_view) = (HistoryView::new(), HistoryView::new());
    history_at(&changed.reader(), history, &mut changed_view, 64);

    changed
        .controller()
        .update(|spec| spec.set_node(gain, NodeSpec::Gain { db: 6.0 }))
        .unwrap();
    capture.write(&mono_tone(16, 3_000.0, 0.05));
    history_at(&changed.reader(), history, &mut changed_view, 80);
    history_at(&control.reader(), history, &mut control_view, 80);
    let changed_peak = latest_at(&changed.reader(), peak_output, 80);
    let control_peak = latest_at(&control.reader(), peak_output, 80);
    assert!(
        (bins(&changed_peak)[2] - bins(&control_peak)[2]).abs() < 1e-4,
        "Peak kept"
    );
    let newest = |view: &HistoryView| bins(&view.iter().last().unwrap().frame).to_vec();
    assert!(
        (newest(&changed_view)[2] - newest(&control_view)[2] - 6.0).abs() < 1e-4,
        "Smooth kept its state; only the gain differs"
    );
    assert_eq!(
        changed_view.len(),
        1,
        "History of the changed output restarts"
    );
    assert_eq!(control_view.len(), 4);
}

#[test]
fn closing_consumers_keeps_shared_work_and_the_last_close_releases_everything() {
    let (mut capture, dataflow, _) = fixture(1, 8, 128);
    let (mut spec, fft) = fft_spec(fft32());
    let output = spec.output(fft, Keep::Seconds(1.0));
    let first = dataflow.subscribe(spec.clone());
    let second = dataflow.subscribe(spec);
    capture.write(&mono_tone(64, 3_000.0, 0.25));
    let retained = latest_at(&first.reader(), output, 64);
    let old_reader = first.reader();
    drop(first);
    settle(&dataflow, "one subscription", |m| m.subscriptions == 1);
    capture.write(&mono_tone(32, 3_000.0, 0.25));
    latest_at(&second.reader(), output, 96);
    drop(second);
    let released = settle(&dataflow, "release", |m| m.subscriptions == 0);
    assert_eq!((released.nodes, released.rings), (0, 0));
    let evaluations = released.fft_evaluations;
    capture.write(&mono_tone(64, 3_000.0, 0.25));
    thread::sleep(Duration::from_millis(20));
    assert_eq!(
        dataflow.metrics().fft_evaluations,
        evaluations,
        "no demand, no work"
    );
    assert_eq!(
        retained.processed_through_frame, 64,
        "held results stay readable"
    );
    assert!(
        old_reader.latest(output).is_none(),
        "a reader does not keep demand"
    );
}

#[test]
fn repeated_updates_replace_nodes_instead_of_accumulating() {
    let (_capture, dataflow, _) = fixture(1, 8, 128);
    let (mut spec, fft) = fft_spec(fft32());
    spec.output(fft, Keep::Latest);
    let subscription = dataflow.subscribe(spec);
    let baseline = dataflow.metrics();
    for index in 0..50 {
        let size = if index % 2 == 0 { 64 } else { 32 };
        subscription
            .controller()
            .update(|spec| {
                spec.set_node(
                    fft,
                    NodeSpec::Fft(FftConfig::new(size, 16, -160.0).unwrap()),
                )
            })
            .unwrap();
    }
    let after = dataflow.metrics();
    assert_eq!((after.nodes, after.rings), (baseline.nodes, baseline.rings));
}

#[test]
fn concurrent_edits_of_one_subscription_serialize() {
    const ROUNDS: usize = 500;
    let (_capture, dataflow, _) = fixture(2, 8, 128);
    let mut base = DataSpec::new();
    let channel = base.add(SOURCE, NodeSpec::Channel(ChannelSelection::Left));
    let fft = base.add(channel, NodeSpec::Fft(fft32()));
    base.output(fft, Keep::Latest);
    let subscription = dataflow.subscribe(base.clone());
    let controller = subscription.controller();
    let target_fft = FftConfig::new(64, 32, -160.0).unwrap();
    let start = Barrier::new(3);
    let finish = Barrier::new(3);
    let failures = AtomicUsize::new(0);
    thread::scope(|scope| {
        scope.spawn(|| {
            for _ in 0..ROUNDS {
                start.wait();
                controller
                    .update(|spec| {
                        spec.set_node(channel, NodeSpec::Channel(ChannelSelection::Right))
                    })
                    .unwrap();
                finish.wait();
            }
        });
        scope.spawn(|| {
            for _ in 0..ROUNDS {
                start.wait();
                controller
                    .update(|spec| spec.set_node(fft, NodeSpec::Fft(target_fft)))
                    .unwrap();
                finish.wait();
            }
        });
        for _ in 0..ROUNDS {
            controller.update(|spec| *spec = base.clone()).unwrap();
            start.wait();
            finish.wait();
            let spec = controller.spec();
            if spec.node(channel) != Some(NodeSpec::Channel(ChannelSelection::Right))
                || spec.node(fft) != Some(NodeSpec::Fft(target_fft))
            {
                failures.fetch_add(1, Ordering::Relaxed);
            }
        }
    });
    assert_eq!(failures.load(Ordering::Relaxed), 0);
}

// ---- demand, visibility, pause and continuity ------------------------------------

#[test]
fn hidden_latest_without_state_stops_while_history_continues() {
    let (mut capture, dataflow, _) = fixture(1, 8, 128);
    let (mut latest_spec, fft) = fft_spec(fft32());
    let latest_output = latest_spec.output(fft, Keep::Latest);
    let latest = dataflow.subscribe(latest_spec);
    latest.controller().set_visible(false).unwrap();
    settle(&dataflow, "hidden pure chain stops", |m| {
        m.active_fft_nodes == 0
    });
    capture.write(&mono_tone(64, 3_000.0, 0.25));
    thread::sleep(Duration::from_millis(10));
    assert_eq!(dataflow.metrics().fft_evaluations, 0);
    assert!(latest.reader().latest(latest_output).is_none());
    latest.controller().set_visible(true).unwrap();
    settle(&dataflow, "shown chain restarts", |m| {
        m.active_fft_nodes == 1
    });
    capture.write(&mono_tone(32, 3_000.0, 0.25));
    latest_at(&latest.reader(), latest_output, 96);

    let (mut history_spec, fft) = fft_spec(fft32());
    let history_output = history_spec.output(fft, Keep::Seconds(1.0));
    let history = dataflow.subscribe(history_spec);
    drop(latest);
    capture.write(&mono_tone(32, 3_000.0, 0.25));
    let mut view = HistoryView::new();
    history_at(&history.reader(), history_output, &mut view, 128);
    let visible = view.len();
    history.controller().set_visible(false).unwrap();
    capture.write(&mono_tone(32, 3_000.0, 0.25));
    // Hidden: the Module does not draw; the History still records.
    wait("hidden history", || {
        (history
            .reader()
            .latest(history_output)?
            .processed_through_frame
            >= 160)
            .then_some(())
    });
    history_at(&history.reader(), history_output, &mut view, 160);
    assert!(view.len() > visible);
}

#[test]
fn explicit_pause_freezes_reads_and_resume_marks_a_gap() {
    let (mut capture, dataflow, _) = fixture(1, 8, 128);
    let (mut spec, fft) = fft_spec(fft32());
    let smooth = spec.add(
        fft,
        NodeSpec::Smooth(SmoothConfig::new(10.0, 100.0).unwrap()),
    );
    let output = spec.output(smooth, Keep::Seconds(1.0));
    let subscription = dataflow.subscribe(spec);
    capture.write(&mono_tone(64, 3_000.0, 0.25));
    let mut view = HistoryView::new();
    history_at(&subscription.reader(), output, &mut view, 64);
    let mut snapshot = Snapshot::default();
    subscription.reader().read(&mut snapshot);
    let paused_version = snapshot.get(output).unwrap().result_version;

    subscription.controller().set_analysis_paused(true).unwrap();
    settle(&dataflow, "paused analysis stops", |m| {
        m.active_spectrum_nodes == 0
    });
    capture.write(&mono_tone(64, 3_000.0, 0.25));
    thread::sleep(Duration::from_millis(10));
    assert!(
        !subscription.reader().read(&mut snapshot),
        "paused reads keep the last results"
    );
    assert_eq!(snapshot.get(output).unwrap().result_version, paused_version);

    subscription
        .controller()
        .set_analysis_paused(false)
        .unwrap();
    capture.write(&mono_tone(64, 3_000.0, 0.25));
    history_at(&subscription.reader(), output, &mut view, 192);
    assert!(
        view.iter().any(|frame| frame.gap_before),
        "resume is not continuous"
    );
    assert!(
        view.iter()
            .all(|frame| !(65..=128).contains(&frame.frame.processed_through_frame)),
        "nothing is claimed for the paused period"
    );
}

#[test]
fn a_new_stateful_node_on_a_warm_fft_ignores_windows_from_before_it_joined() {
    let (mut capture, dataflow, _) = fixture(1, 8, 128);
    let (mut warm, fft) = fft_spec(fft32());
    warm.output(fft, Keep::Latest);
    let _warm = dataflow.subscribe(warm);
    // 3 kHz is bin 2 of a 32-point FFT at 48 kHz.
    capture.write(&mono_tone(64, 3_000.0, 0.5));
    settle(&dataflow, "warm FFT", |m| m.fft_evaluations == 3);
    let (mut spec, fft) = fft_spec(fft32());
    let peak_node = spec.add(fft, peak(1_000.0));
    let output = spec.output(peak_node, Keep::Latest);
    let joined = dataflow.subscribe(spec);
    capture.write(&[0.0; 64]);
    let held = latest_at(&joined.reader(), output, 128);
    assert!(
        bins(&held)[2] < -100.0,
        "the new Peak held a tone from before it joined: {} dBFS",
        bins(&held)[2]
    );
}

#[test]
fn a_paused_reader_of_a_shared_output_keeps_its_pause_point() {
    let (mut capture, dataflow, _) = fixture(2, 128, 16);
    let (spec, output) = pcm_spec(Keep::Latest);
    let paused = dataflow.subscribe(spec.clone());
    let running = dataflow.subscribe(spec);
    feed(&mut capture, &dataflow, &stereo(0, 1_600));
    let before = latest_at(&paused.reader(), output, 1_600);
    paused.controller().set_analysis_paused(true).unwrap();
    feed(&mut capture, &dataflow, &stereo(1_600, 1_600));
    latest_at(&running.reader(), output, 3_200);
    let frozen = paused
        .reader()
        .latest(output)
        .expect("pause keeps a result");
    assert!(
        Arc::ptr_eq(&frozen, &before),
        "a paused reader saw new data"
    );
    paused.controller().set_analysis_paused(false).unwrap();
    feed(&mut capture, &dataflow, &stereo(3_200, 1_600));
    let resumed = latest_at(&paused.reader(), output, 4_800);
    assert_eq!(resumed.pcm().unwrap().start_frame, 3_200);
}

#[test]
fn resetting_a_bound_pure_history_restarts_only_the_requesting_reader() {
    let (mut capture, dataflow, _) = fixture(1, 8, 128);
    let (mut spec, fft) = fft_spec(fft32());
    let output = spec.output(fft, Keep::Seconds(1.0));
    spec.bind(output, Some(SharedAnalysisId(8)));
    let first = dataflow.subscribe(spec.clone());
    let second = dataflow.subscribe(spec);
    capture.write(&mono_tone(64, 3_000.0, 0.25));
    let (mut first_view, mut second_view) = (HistoryView::new(), HistoryView::new());
    history_at(&first.reader(), output, &mut first_view, 64);
    history_at(&second.reader(), output, &mut second_view, 64);
    first.controller().reset_state().unwrap();
    capture.write(&mono_tone(32, 3_000.0, 0.25));
    history_at(&first.reader(), output, &mut first_view, 96);
    history_at(&second.reader(), output, &mut second_view, 96);
    assert_eq!(
        first_view.len(),
        2,
        "the requester restarts at the reset point"
    );
    assert_eq!(
        second_view.len(),
        5,
        "the other member keeps the shared History"
    );
}

#[test]
fn dropped_blocks_mark_discontinuity_and_reset_continuous_state() {
    let (mut capture, dataflow, telemetry) = fixture(1, 1, 32);
    let (mut spec, fft) = fft_spec(FftConfig::new(32, 32, -160.0).unwrap());
    let output = spec.output(fft, Keep::Seconds(1.0));
    let subscription = dataflow.subscribe(spec);
    capture.write(&[0.0; 96]);
    let first = latest_at(&subscription.reader(), output, 32);
    assert_eq!(telemetry.snapshot().dropped_blocks, 2);
    capture.write(&[0.0; 32]);
    let after_gap = latest_at(&subscription.reader(), output, 128);
    assert!(after_gap.discontinuity_count > first.discontinuity_count);
    assert!(after_gap.discontinuity_before);
    assert_eq!(after_gap.processed_through_frame, 128);
}

#[test]
fn outputs_of_one_block_are_read_together() {
    let (mut capture, dataflow, _) = fixture(1, 8, 4096);
    let (mut spec, fft) = fft_spec(fft32());
    let tilt = spec.add(fft, NodeSpec::Tilt { db_per_octave: 3.0 });
    let peak_node = spec.add(fft, peak(100.0));
    let main = spec.output(tilt, Keep::Latest);
    let peak_output = spec.output(peak_node, Keep::Latest);
    let subscription = dataflow.subscribe(spec);
    let reader = subscription.reader();
    let mut snapshot = Snapshot::default();
    for round in 1..=20_u64 {
        capture.write(&mono_tone(16 * (round as usize % 4 + 1), 3_000.0, 0.25));
        reader.read(&mut snapshot);
        if let (Some(main), Some(peak)) = (snapshot.get(main), snapshot.get(peak_output)) {
            assert_eq!(
                main.processed_through_frame, peak.processed_through_frame,
                "main curve and peak come from the same hop"
            );
        }
    }
    assert_eq!(snapshot.config_revision(), 1);
}

// ---- time-domain demand (A3–A7) -------------------------------------------------

fn pcm_spec(keep: Keep) -> (DataSpec, OutputId) {
    let mut spec = DataSpec::new();
    let output = spec.output(SOURCE, keep);
    (spec, output)
}

fn levels_spec() -> (DataSpec, OutputId) {
    let mut spec = DataSpec::new();
    let levels = spec.add(SOURCE, NodeSpec::Levels);
    let output = spec.output(levels, Keep::Latest);
    (spec, output)
}

#[test]
fn time_demand_matrix_only_does_requested_work() {
    let (mut capture, dataflow, _) = fixture(2, 128, 16);
    feed(&mut capture, &dataflow, &stereo(0, 1_600));
    thread::sleep(Duration::from_millis(10));
    let idle = dataflow.metrics();
    assert_eq!(
        (idle.pcm_frames, idle.level_frames, idle.publications),
        (0, 0, 0)
    );

    let (spec, output) = pcm_spec(Keep::Latest);
    let latest = dataflow.subscribe(spec);
    feed(&mut capture, &dataflow, &stereo(1_600, 1_600));
    let chunk = latest_at(&latest.reader(), output, 3_200);
    let pcm = chunk.pcm().unwrap();
    assert_eq!((pcm.start_frame, pcm.frame_count), (1_600, 1_600));
    assert_eq!(
        dataflow.metrics().level_frames,
        0,
        "Latest computes no Levels"
    );
    drop(latest);

    let (spec, output) = pcm_spec(Keep::Seconds(0.01));
    let history = dataflow.subscribe(spec);
    feed(&mut capture, &dataflow, &stereo(3_200, 3_200));
    let mut view = HistoryView::new();
    history_at(&history.reader(), output, &mut view, 6_400);
    assert_eq!(view.window_start(), 6_400 - 480);
    let oldest = view.iter().next().unwrap().frame.pcm().unwrap();
    assert!(oldest.start_frame + oldest.frame_count as u64 > view.window_start());
    drop(history);
    let before_levels = settle(&dataflow, "history closed", |m| m.subscriptions == 0);

    let (spec, output) = levels_spec();
    let levels = dataflow.subscribe(spec);
    feed(&mut capture, &dataflow, &stereo(6_400, 1_600));
    let frame = latest_at(&levels.reader(), output, 8_000);
    let data = frame.levels().unwrap();
    assert!(data.momentary_lufs[0] > -60.0 && data.sample_peaks[0] > 0.4);
    let after = dataflow.metrics();
    assert_eq!(
        after.pcm_frames, before_levels.pcm_frames,
        "Levels copy no PCM"
    );
    assert!(after.level_frames > 0);
}

#[test]
fn small_blocks_merge_into_one_shared_chunk_with_the_interval_peak() {
    let (mut capture, dataflow, _) = fixture(2, 128, 16);
    let (latest_spec, latest_output) = pcm_spec(Keep::Latest);
    let (history_spec, history_output) = pcm_spec(Keep::Seconds(1.0));
    let (levels_spec, levels_output) = levels_spec();
    let latest = dataflow.subscribe(latest_spec);
    let history = dataflow.subscribe(history_spec);
    let levels = dataflow.subscribe(levels_spec);
    let mut samples = vec![0.0_f32; 3_200];
    samples[20] = 0.9;
    for block in samples.chunks(32) {
        capture.write(block);
    }
    let chunk = latest_at(&latest.reader(), latest_output, 1_600);
    let mut view = HistoryView::new();
    history_at(&history.reader(), history_output, &mut view, 1_600);
    assert_eq!(view.len(), 1);
    assert!(
        Arc::ptr_eq(&view.iter().next().unwrap().frame, &chunk),
        "one shared payload"
    );
    assert_eq!(chunk.pcm().unwrap().interleaved, samples);
    let level = latest_at(&levels.reader(), levels_output, 1_600);
    assert_eq!(
        level.levels().unwrap().sample_peaks[0],
        0.9,
        "interval peak, not last block"
    );
    let metrics = settle(&dataflow, "publications", |m| m.publications == 2);
    assert_eq!(metrics.pcm_frames, 1_600);
}

#[test]
fn private_joins_do_not_inherit_earlier_history_or_levels() {
    let (mut capture, dataflow, _) = fixture(2, 128, 16);
    let (history_spec, history_output) = pcm_spec(Keep::Seconds(1.0));
    let first = dataflow.subscribe(history_spec.clone());
    feed(&mut capture, &dataflow, &stereo(0, 2_400));
    let mut first_view = HistoryView::new();
    history_at(&first.reader(), history_output, &mut first_view, 1_600);

    let second = dataflow.subscribe(history_spec);
    feed(&mut capture, &dataflow, &stereo(2_400, 1_600));
    let mut second_view = HistoryView::new();
    history_at(&second.reader(), history_output, &mut second_view, 3_200);
    let joined = second_view.iter().next().unwrap().frame.pcm().unwrap();
    assert_eq!(joined.start_frame, 2_400, "B starts at its join boundary");
    history_at(&first.reader(), history_output, &mut first_view, 3_200);
    let first_frames: u64 = first_view
        .iter()
        .map(|frame| frame.frame.pcm().unwrap().frame_count as u64)
        .sum();
    assert_eq!(
        first_frames, 3_200,
        "A keeps every frame, before and after B joined"
    );

    let (levels_spec, levels_output) = levels_spec();
    let loud = dataflow.subscribe(levels_spec.clone());
    feed(&mut capture, &dataflow, &stereo(4_000, 1_600));
    latest_at(&loud.reader(), levels_output, 4_800);
    let quiet = dataflow.subscribe(levels_spec);
    feed(&mut capture, &dataflow, &vec![0.0; 2 * 2_400]);
    let loud_level = latest_at(&loud.reader(), levels_output, 8_000);
    let quiet_level = latest_at(&quiet.reader(), levels_output, 8_000);
    assert!(
        loud_level.levels().unwrap().momentary_lufs[0] > -60.0,
        "A keeps earlier energy"
    );
    assert_eq!(
        quiet_level.levels().unwrap().momentary_lufs[0],
        -60.0,
        "B hears only silence"
    );
}

#[test]
fn partial_time_closes_keep_the_other_work_and_rejoins_start_fresh() {
    let (mut capture, dataflow, _) = fixture(2, 128, 16);
    let (levels_spec, _) = levels_spec();
    let levels = dataflow.subscribe(levels_spec);
    feed(&mut capture, &dataflow, &stereo(0, 800));
    settle(&dataflow, "levels at 800", |m| m.level_frames == 800);
    let (latest_spec, latest_output) = pcm_spec(Keep::Latest);
    let latest = dataflow.subscribe(latest_spec);
    feed(&mut capture, &dataflow, &stereo(800, 800));
    let first = latest_at(&latest.reader(), latest_output, 1_600);
    assert_eq!(
        first.pcm().unwrap().frame_count,
        800,
        "first chunk holds only joined frames"
    );
    feed(&mut capture, &dataflow, &stereo(1_600, 1_600));
    let second = latest_at(&latest.reader(), latest_output, 3_200);
    assert_eq!(
        second.pcm().unwrap().frame_count,
        1_600,
        "then full intervals"
    );

    drop(latest);
    let pcm_closed = settle(&dataflow, "PCM closed", |m| m.subscriptions == 1);
    feed(&mut capture, &dataflow, &stereo(3_200, 1_600));
    let levels_only = settle(&dataflow, "levels continue", |m| m.level_frames >= 4_800);
    assert_eq!(levels_only.pcm_frames, pcm_closed.pcm_frames);

    let (history_spec, _) = pcm_spec(Keep::Seconds(1.0));
    let history = dataflow.subscribe(history_spec);
    drop(levels);
    let levels_closed = settle(&dataflow, "levels closed", |m| {
        m.active_level_nodes == 0 && m.subscriptions == 1
    });
    feed(&mut capture, &dataflow, &stereo(4_800, 1_600));
    let pcm_only = settle(&dataflow, "PCM continues", |m| {
        m.pcm_frames > levels_closed.pcm_frames
    });
    assert_eq!(pcm_only.level_frames, levels_closed.level_frames);

    drop(history);
    let closed = settle(&dataflow, "all closed", |m| m.subscriptions == 0);
    assert_eq!((closed.nodes, closed.rings), (0, 0));
    feed(&mut capture, &dataflow, &stereo(6_400, 1_600));
    thread::sleep(Duration::from_millis(10));
    let idle = dataflow.metrics();
    assert_eq!(
        (idle.pcm_frames, idle.level_frames, idle.publications),
        (closed.pcm_frames, closed.level_frames, closed.publications)
    );
    let (spec, output) = pcm_spec(Keep::Latest);
    let rejoined = dataflow.subscribe(spec);
    feed(&mut capture, &dataflow, &stereo(8_000, 1_600));
    let fresh = latest_at(&rejoined.reader(), output, 9_600);
    assert_eq!(
        fresh.pcm().unwrap().start_frame,
        8_000,
        "no earlier data after rejoining"
    );
}

#[test]
fn hiding_a_pcm_reader_does_not_merge_audio_across_the_hidden_period() {
    let (mut capture, dataflow, _) = fixture(2, 128, 16);
    // An FFT keeps the source busy while the PCM reader is hidden.
    let mut spectrum = DataSpec::new();
    let channel = spectrum.add(SOURCE, NodeSpec::Channel(ChannelSelection::Left));
    let fft = spectrum.add(channel, NodeSpec::Fft(fft32()));
    spectrum.output(fft, Keep::Seconds(1.0));
    let _spectrum = dataflow.subscribe(spectrum);
    let (spec, output) = pcm_spec(Keep::Latest);
    let oscilloscope = dataflow.subscribe(spec);
    feed(&mut capture, &dataflow, &stereo(0, 800));
    oscilloscope.controller().set_visible(false).unwrap();
    feed(&mut capture, &dataflow, &stereo(800, 800));
    oscilloscope.controller().set_visible(true).unwrap();
    feed(&mut capture, &dataflow, &stereo(1_600, 1_600));
    let chunk = latest_at(&oscilloscope.reader(), output, 3_200);
    let pcm = chunk.pcm().unwrap();
    assert_eq!(
        (pcm.start_frame, pcm.frame_count),
        (1_600, 1_600),
        "the chunk starts where reading resumed"
    );
    assert_eq!(pcm.interleaved, stereo(1_600, 1_600));
}

#[test]
fn time_history_keeps_its_window_and_drops_old_chunks_after_gaps() {
    let (mut capture, dataflow, telemetry) = fixture(2, 1, 1_600);
    let (spec, output) = pcm_spec(Keep::Seconds(0.1));
    let history = dataflow.subscribe(spec);
    capture.write(&stereo(0, 1_600));
    let mut view = HistoryView::new();
    history_at(&history.reader(), output, &mut view, 1_600);
    let drained = |frame: u64| {
        wait("block recycled", || {
            (dataflow.handle.processed_through_frame().unwrap() >= frame).then_some(())
        })
    };
    drained(1_600);
    // With the worker held, the single-block queue accepts one block and drops the
    // rest: a source-frame gap.
    let release = dataflow.handle.block_worker_for_test();
    capture.write(&stereo(1_600, 1_600 * 8));
    release.wait();
    assert_eq!(telemetry.snapshot().dropped_blocks, 7);
    drained(3_200);
    // The capture keeps counting source frames across dropped blocks.
    capture.write(&stereo(14_400, 1_600));
    history_at(&history.reader(), output, &mut view, 16_000);
    assert!(
        view.iter()
            .all(|frame| frame.frame.processed_through_frame > 16_000 - 4_800)
    );
    assert!(view.iter().any(|frame| frame.gap_before));
}

// ---- errors and control path (A8) -----------------------------------------------

#[test]
fn invalid_specs_fail_without_creating_demand() {
    let (capture, dataflow, _) = fixture(1, 8, 128);
    for keep in [f32::NAN, 0.0, -1.0, f32::INFINITY] {
        let (spec, _) = pcm_spec(Keep::Seconds(keep));
        assert!(matches!(
            dataflow.handle.subscribe(spec),
            Err(ControlError::InvalidSpec(SpecError::InvalidKeep))
        ));
    }
    let (spec, _) = pcm_spec(Keep::Seconds(10_000.0));
    assert!(matches!(
        dataflow.handle.subscribe(spec),
        Err(ControlError::InvalidSpec(SpecError::HistoryTooLarge))
    ));
    let (spec, _) = pcm_spec(Keep::Seconds(60.0));
    assert!(
        dataflow.handle.subscribe(spec).is_ok(),
        "the budget is not a 10 s cap"
    );
    let mut spec = DataSpec::new();
    let right = spec.add(SOURCE, NodeSpec::Channel(ChannelSelection::Right));
    spec.output(right, Keep::Latest);
    assert!(matches!(
        dataflow.handle.subscribe(spec),
        Err(ControlError::InvalidSpec(SpecError::InvalidChannel(_)))
    ));
    let mut spec = DataSpec::new();
    let tilt = spec.add(SOURCE, NodeSpec::Tilt { db_per_octave: 1.0 });
    spec.output(tilt, Keep::Latest);
    assert!(matches!(
        dataflow.handle.subscribe(spec),
        Err(ControlError::InvalidSpec(SpecError::KindMismatch {
            node: 0
        }))
    ));
    assert!(matches!(
        dataflow.handle.subscribe(DataSpec::new()),
        Err(ControlError::InvalidSpec(SpecError::NoOutputs))
    ));
    let mut spec = DataSpec::new();
    let mono = spec.add(SOURCE, NodeSpec::Channel(ChannelSelection::MonoAverage));
    spec.output(mono, Keep::Latest);
    assert!(
        matches!(
            dataflow.handle.subscribe(spec),
            Err(ControlError::InvalidSpec(SpecError::UnsupportedOutput))
        ),
        "mono channel samples are never published, so they cannot be an output"
    );
    // 48 kHz, window 16 / hop 1: 1.8 M small results in 38 s. The payload alone
    // would fit 64 MiB; per-result storage does not.
    let (mut spec, fft) = fft_spec(FftConfig::new(16, 1, -160.0).unwrap());
    spec.output(fft, Keep::Seconds(38.0));
    assert!(matches!(
        dataflow.handle.subscribe(spec),
        Err(ControlError::InvalidSpec(SpecError::HistoryTooLarge))
    ));
    // One spec naming one binding for two different Peaks.
    let (mut spec, fft) = fft_spec(fft32());
    let short = spec.add(fft, peak(100.0));
    let long = spec.add(fft, peak(900.0));
    let short = spec.output(short, Keep::Latest);
    let long = spec.output(long, Keep::Latest);
    spec.bind(short, Some(SharedAnalysisId(3)));
    spec.bind(long, Some(SharedAnalysisId(3)));
    assert!(matches!(
        dataflow.handle.subscribe(spec),
        Err(ControlError::SharedBindingMismatch(SharedAnalysisId(3)))
    ));
    settle(&dataflow, "no demand", |m| {
        m.subscriptions == 0 && m.nodes == 0
    });
    let handle = dataflow.handle.clone();
    drop(capture);
    drop(dataflow);
    assert!(matches!(
        handle.subscribe(pcm_spec(Keep::Latest).0),
        Err(ControlError::Stopped)
    ));
}

#[test]
fn a_full_control_queue_reports_busy_and_drop_still_removes_demand() {
    let (_capture, dataflow, _) = fixture(1, 8, 32);
    let (spec, _) = pcm_spec(Keep::Latest);
    let subscription = dataflow.subscribe(spec);
    let controller = subscription.controller();
    let release = dataflow.handle.block_worker_for_test();
    let mut accepted = 0;
    while controller.reset_state().is_ok() {
        accepted += 1;
        assert!(accepted <= 16, "the control queue is bounded");
    }
    assert_eq!(controller.reset_state(), Err(ControlError::Busy));
    let dropper = thread::spawn(move || drop(subscription));
    release.wait();
    dropper.join().unwrap();
    settle(&dataflow, "dropped from a full queue", |m| {
        m.subscriptions == 0
    });
}

#[test]
fn sustained_control_load_does_not_starve_audio() {
    let (mut capture, dataflow, _) = fixture(1, 128, 32);
    let (mut spec, fft) = fft_spec(fft32());
    let output = spec.output(fft, Keep::Seconds(1.0));
    let subscription = dataflow.subscribe(spec);
    let controller = subscription.controller();
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        scope.spawn(|| {
            let mut visible = false;
            while !stop.load(Ordering::Acquire) {
                retry_busy(|| controller.set_visible(visible));
                visible = !visible;
            }
        });
        capture.write(&mono_tone(4_096, 3_000.0, 0.25));
        latest_at(&subscription.reader(), output, 4_096);
        stop.store(true, Ordering::Release);
    });
}

// ---- steady-state cost ------------------------------------------------------------

#[test]
fn steady_state_worker_does_not_allocate_while_readers_keep_up() {
    let (mut capture, dataflow, _) = fixture(2, 32, 480);
    let mut spectrum = DataSpec::new();
    let channel = spectrum.add(SOURCE, NodeSpec::Channel(ChannelSelection::MonoAverage));
    let fft = spectrum.add(channel, NodeSpec::Fft(FftConfig::default()));
    let curve = spectrum.chain(
        fft,
        [
            NodeSpec::Tilt { db_per_octave: 3.0 },
            NodeSpec::Smooth(SmoothConfig::new(10.0, 200.0).unwrap()),
            NodeSpec::Center { db: 0.0 },
            NodeSpec::Gain { db: 0.0 },
        ],
    );
    let peak_node = spectrum.add(fft, peak(1_000.0));
    spectrum.output(curve, Keep::Latest);
    spectrum.output(peak_node, Keep::Latest);
    let history_output = spectrum.output(fft, Keep::Seconds(2.0));
    let spectrum = dataflow.subscribe(spectrum);
    let (pcm, pcm_output) = pcm_spec(Keep::Seconds(1.0));
    let pcm = dataflow.subscribe(pcm);
    let (levels, _) = levels_spec();
    let levels = dataflow.subscribe(levels);

    let mut snapshot = Snapshot::default();
    let mut level_snapshot = Snapshot::default();
    let mut history = HistoryView::new();
    let mut waveform = HistoryView::new();
    let mut frame = 0_u64;
    let mut run = |seconds: u64| {
        for _ in 0..seconds * 100 {
            capture.write(&stereo(frame, 480));
            frame += 480;
            wait("block processed", || {
                (dataflow.handle.processed_through_frame().unwrap() >= frame).then_some(())
            });
            spectrum.reader().read(&mut snapshot);
            levels.reader().read(&mut level_snapshot);
            spectrum.reader().sync_history(history_output, &mut history);
            pcm.reader().sync_history(pcm_output, &mut waveform);
        }
    };
    run(3);
    let before = dataflow.handle.worker_sample().allocations;
    run(5);
    let after = dataflow.handle.worker_sample().allocations;
    assert_eq!(after - before, 0, "steady-state allocations on the worker");
}

#[test]
fn saved_spec_round_trips_and_malformed_graphs_are_rejected_before_use() {
    let mut spec = DataSpec::new();
    let channel = spec.add(SOURCE, NodeSpec::Channel(ChannelSelection::Discrete(3)));
    let fft = spec.add(
        channel,
        NodeSpec::Fft(FftConfig::new(4096, 1024, -150.0).unwrap()),
    );
    let peak = spec.add(fft, NodeSpec::Peak(PeakConfig::new(250.0, 30.0).unwrap()));
    spec.add(SOURCE, NodeSpec::Levels);
    spec.output(fft, Keep::Seconds(2.5));
    let bound = spec.output(peak, Keep::Latest);
    spec.output(SOURCE, Keep::Latest);
    spec.bind(bound, Some(SharedAnalysisId(7)));
    let text = serde_json::to_string(&spec).unwrap();
    assert_eq!(serde_json::from_str::<DataSpec>(&text).unwrap(), spec);

    // A node reading a later node, or an output naming a missing node, would make
    // the graph walk out of bounds: refused when reading the file.
    for broken in [
        r#"{"nodes":[{"input":1,"kind":"levels"},{"input":null,"kind":"levels"}],"outputs":[]}"#,
        r#"{"nodes":[{"input":null,"kind":"levels"}],"outputs":[{"node":4,"keep":"latest"}]}"#,
        r#"{"nodes":[{"input":0,"kind":"levels"}],"outputs":[]}"#,
    ] {
        assert!(
            serde_json::from_str::<DataSpec>(broken).is_err(),
            "{broken}"
        );
    }
    // Structurally sound but wrong for the source: rejected by subscribe.
    let (_capture, dataflow, _) = fixture(2, 4, 512);
    let saved: DataSpec = serde_json::from_str(&text).unwrap();
    assert!(matches!(
        dataflow.handle.subscribe(saved),
        Err(ControlError::InvalidSpec(SpecError::InvalidChannel(_)))
    ));
}
