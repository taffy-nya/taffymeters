use crate::dataflow::time::{LEVELS_FLOOR_DBFS, LUFS_FLOOR};

use super::{Engine, Kernel, NodeId};

/// Smooth approaches its input exponentially; this close counts as arrived.
const SMOOTH_REST_DB: f32 = 0.01;

impl Engine {
    /// Whether every demanded node holds its zero-input value, given zero input from
    /// engine frame `since` on. Only results computed after `since` count where a low
    /// value could still move (an FFT window holding older audio, PCM not yet shown).
    pub(crate) fn at_rest(&mut self, since: u64) -> bool {
        let mut rest = true;
        if let Some(source) = self.plan.source
            && self.node(source).ring_demanded
        {
            rest &= self.pcm_published_from.is_some_and(|start| start >= since);
        }
        for index in 0..self.plan.levels.len() {
            let id = self.plan.levels[index].0;
            let node = self.node_mut(id);
            node.rest = node.produced
                && matches!(&node.kernel, Kernel::Levels { dbfs, .. }
                    if dbfs.iter().all(|&value| value == LEVELS_FLOOR_DBFS));
            rest &= node.rest;
            for order_index in 0..self.plan.levels[index].1.len() {
                rest &= self.chain_rest(self.plan.levels[index].1[order_index]);
            }
        }
        for &id in &self.plan.loudness {
            rest &= matches!(&self.node(id).kernel, Kernel::Loudness { published_lufs, .. }
                if *published_lufs == LUFS_FLOOR);
        }
        for index in 0..self.plan.ffts.len() {
            let id = self.plan.ffts[index].0;
            let node = self.node_mut(id);
            let window_frames = node.fft.map_or(0, |fft| fft.window_size as u64);
            // A window of zeros only: every bin is exactly at the floor from here on.
            node.rest = node.produced && node.produced_through >= since + window_frames;
            rest &= node.rest;
            for order_index in 0..self.plan.ffts[index].1.len() {
                rest &= self.chain_rest(self.plan.ffts[index].1[order_index]);
            }
        }
        rest
    }

    /// Readers run after their input in plan order, so the input's `rest` is current.
    fn chain_rest(&mut self, id: NodeId) -> bool {
        let node = self.node(id);
        let input = self.node(node.input.expect("chained nodes have an input"));
        let rest = input.rest
            && node.produced
            && match &node.kernel {
                Kernel::Smooth { state, .. } => {
                    let values = input.kernel.values();
                    state.values.len() == values.len()
                        && state
                            .values
                            .iter()
                            .zip(values)
                            .all(|(value, target)| (value - target).abs() <= SMOOTH_REST_DB)
                }
                // Without a fall the peak is held on purpose.
                Kernel::Peak { config, state } => {
                    config.fall_db_per_second_squared == 0.0
                        || state.values.as_slice() == input.kernel.values()
                }
                _ => true,
            };
        self.node_mut(id).rest = rest;
        rest
    }
}

#[cfg(test)]
mod tests {
    use std::{
        f32::consts::PI,
        sync::{Arc, Mutex},
    };

    use crate::{
        audio::{
            AudioDescriptor, CaptureEndpoint, ChannelLayout, Generation, ProcessingEndpoint,
            SourceId, SourceTelemetry, create_block_transport,
        },
        dataflow::{
            ChannelSelection, DataSpec, FftConfig, Keep, NodeSpec, PeakConfig, SOURCE,
            SmoothConfig, store::Store,
        },
    };

    use super::super::{Engine, Metrics};

    const RATE: u32 = 48_000;
    const BLOCK: usize = 480;

    struct Rig {
        engine: Engine,
        capture: CaptureEndpoint,
        processing: ProcessingEndpoint,
        frames: u64,
        /// Where the zeros of `rest_after_tone` began.
        since: u64,
    }

    impl Rig {
        fn new(spec: DataSpec) -> Self {
            let descriptor = Arc::new(
                AudioDescriptor::new(SourceId(1), Generation(1), RATE, 1, ChannelLayout::Mono)
                    .unwrap(),
            );
            let telemetry = Arc::new(SourceTelemetry::new());
            let (capture, processing) = create_block_transport(1, 4, BLOCK, telemetry).unwrap();
            let mut engine = Engine::new(
                Arc::clone(&descriptor),
                Arc::new(Mutex::new(Store::default())),
                Arc::new(Metrics::default()),
            );
            engine
                .subscribe(1, spec.validated(&descriptor).unwrap(), true)
                .unwrap();
            Self {
                engine,
                capture,
                processing,
                frames: 0,
                since: 0,
            }
        }

        fn feed(&mut self, samples: &[f32]) {
            self.capture.write(samples);
            let block = self.processing.try_pop().unwrap();
            self.engine.process_block(&block);
            self.processing.recycle(block);
            self.frames += samples.len() as u64;
        }

        /// One second of a -6 dBFS 1 kHz tone, then zeros; the seconds of zeros until
        /// the engine first reports rest, or `None` within `limit_seconds`.
        fn rest_after_tone(&mut self, limit_seconds: f32) -> Option<f32> {
            let tone: Vec<f32> = (0..BLOCK)
                .map(|index| 0.5 * (2.0 * PI * 1_000.0 * index as f32 / RATE as f32).sin())
                .collect();
            for _ in 0..RATE as usize / BLOCK {
                self.feed(&tone);
            }
            let since = self.frames;
            self.since = since;
            let zeros = [0.0_f32; BLOCK];
            while self.frames - since < (limit_seconds * RATE as f32) as u64 {
                self.feed(&zeros);
                if self.engine.at_rest(since) {
                    return Some((self.frames - since) as f32 / RATE as f32);
                }
            }
            None
        }
    }

    fn spectrum(tail: NodeSpec) -> DataSpec {
        let mut spec = DataSpec::new();
        let fft = spec.chain(
            SOURCE,
            [
                NodeSpec::Channel(ChannelSelection::MonoAverage),
                NodeSpec::Fft(FftConfig::new(2048, 512, -160.0).unwrap()),
                NodeSpec::Tilt { db_per_octave: 4.5 },
            ],
        );
        let tail = spec.add(fft, tail);
        spec.output(fft, Keep::Latest);
        spec.output(tail, Keep::Latest);
        spec
    }

    #[test]
    fn a_falling_peak_comes_to_rest_only_at_its_input_after_the_hold() {
        let mut rig = Rig::new(spectrum(NodeSpec::Peak(
            PeakConfig::new(1_000.0, 30.0).unwrap(),
        )));
        // About 134 dB to fall at 30 dB/s² after the 1 s hold: 1 + sqrt(2 * 134 / 30) s.
        let seconds = rig.rest_after_tone(8.0).expect("the peak comes to rest");
        assert!((3.5..4.5).contains(&seconds), "rest after {seconds} s");
        rig.feed(&[0.0; BLOCK]);
        assert!(
            rig.engine.at_rest(rig.since),
            "rest holds while zeros continue"
        );
    }

    #[test]
    fn a_held_peak_without_fall_rests_once_the_fft_window_is_silent() {
        let mut rig = Rig::new(spectrum(NodeSpec::Peak(
            PeakConfig::new(1_000.0, 0.0).unwrap(),
        )));
        let seconds = rig
            .rest_after_tone(1.0)
            .expect("the held peak counts as at rest");
        // The first result computed over a window of zeros: 2048 frames, then a 512 hop.
        assert!(
            seconds <= (2048.0 + 512.0) / RATE as f32,
            "rest after {seconds} s"
        );
    }

    #[test]
    fn a_long_release_rests_within_the_tolerance_of_its_input() {
        let mut rig = Rig::new(spectrum(NodeSpec::Smooth(
            SmoothConfig::new(0.0, 2_000.0).unwrap(),
        )));
        // ln(Δ / 0.01 dB) release time constants for a fall of about 150 dB.
        let seconds = rig
            .rest_after_tone(25.0)
            .expect("the smoothed curve comes to rest");
        assert!((17.0..21.0).contains(&seconds), "rest after {seconds} s");
    }

    #[test]
    fn levels_loudness_and_pcm_rest_after_the_level_peak_falls() {
        let mut spec = DataSpec::new();
        let levels = spec.add(SOURCE, NodeSpec::Levels);
        let peak = spec.add(
            levels,
            NodeSpec::Peak(PeakConfig::new(1_000.0, 30.0).unwrap()),
        );
        let loudness = spec.add(SOURCE, NodeSpec::Loudness);
        spec.output(peak, Keep::Latest);
        spec.output(loudness, Keep::Latest);
        spec.output(SOURCE, Keep::Seconds(1.0));
        let mut rig = Rig::new(spec);
        // -6 dBFS to the -150 dBFS level floor: 1 + sqrt(2 * 144 / 30) s.
        let seconds = rig.rest_after_tone(8.0).expect("the meter comes to rest");
        assert!((3.8..4.5).contains(&seconds), "rest after {seconds} s");
    }
}
