//! System loopback emits no samples while idle, and a lost device none at all.
//! Worker-generated silence lets stateful analysis settle; later captured blocks shift
//! by the inserted silence to keep the analysis timeline continuous.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use crate::audio::{AudioBlock, SourceState, SourceTelemetry};

/// Shorter than the time views' grace so they see silent samples before marking a gap.
const GRACE: Duration = Duration::from_millis(100);
/// Filling normally ends once the analysis is at rest; this bounds very long holds
/// and releases.
const LIMIT: Duration = Duration::from_secs(30);
const BLOCK_SECONDS: f64 = 0.01;

pub(super) struct SilenceFill {
    telemetry: Arc<SourceTelemetry>,
    sample_rate: f64,
    block: AudioBlock,
    block_frames: usize,
    inserted_frames: u64,
    inserted_blocks: u64,
    /// Start frame and sequence the next block continues with (engine timeline);
    /// `None` until the source delivered something.
    next: Option<(u64, u64)>,
    /// Start of the current idle interval: last captured block or paused-source observation.
    quiet_since: Instant,
    /// Silent source frames inserted in the current idle interval.
    filled: u64,
    /// Engine frame where the current idle interval's silence starts.
    since: u64,
    /// A running source without samples is silent (loopback); otherwise only a lost
    /// device is.
    infer: bool,
    /// The analysis came to rest in this idle interval.
    settled: bool,
}

impl SilenceFill {
    pub(super) fn new(
        telemetry: Arc<SourceTelemetry>,
        sample_rate: u32,
        channels: usize,
        infer: bool,
    ) -> Self {
        let block_frames = ((f64::from(sample_rate) * BLOCK_SECONDS) as usize).max(1);
        Self {
            telemetry,
            sample_rate: f64::from(sample_rate),
            block: AudioBlock::silence(channels, block_frames),
            block_frames,
            inserted_frames: 0,
            inserted_blocks: 0,
            next: None,
            quiet_since: Instant::now(),
            filled: 0,
            since: 0,
            infer,
            settled: false,
        }
    }

    pub(super) fn captured(&mut self, block: &mut AudioBlock, now: Instant) {
        block.start_frame = block.start_frame.saturating_add(self.inserted_frames);
        block.sequence = block.sequence.wrapping_add(self.inserted_blocks);
        self.next = Some((
            block.start_frame.saturating_add(block.frame_count as u64),
            block.sequence.wrapping_add(1),
        ));
        self.restart(now);
    }

    pub(super) fn due(&mut self, now: Instant) -> Option<&AudioBlock> {
        let lost = match self.telemetry.state() {
            SourceState::Running => false,
            SourceState::DeviceLost => true,
            // A paused (or stopped) source is not silent; its pause never counts.
            _ => {
                self.restart(now);
                return None;
            }
        };
        if self.settled || !(lost || self.infer) {
            return None;
        }
        let (start_frame, sequence) = self.next?;
        let quiet = now.saturating_duration_since(self.quiet_since);
        if !lost && quiet < GRACE {
            return None;
        }
        let frames = self
            .frames_due(now)
            .saturating_sub(self.filled)
            .min(self.block_frames as u64) as usize;
        if frames == 0 {
            return None;
        }
        if self.filled == 0 {
            self.since = start_frame;
        }
        self.block.start_frame = start_frame;
        self.block.sequence = sequence;
        self.block.continuity = self.telemetry.continuity();
        self.block.frame_count = frames;
        self.next = Some((start_frame + frames as u64, sequence.wrapping_add(1)));
        self.inserted_frames += frames as u64;
        self.inserted_blocks = self.inserted_blocks.wrapping_add(1);
        self.filled += frames as u64;
        Some(&self.block)
    }

    /// Engine frame where the silence being filled starts.
    pub(super) fn since(&self) -> u64 {
        self.since
    }

    pub(super) fn settle(&mut self) {
        self.settled = true;
    }

    /// After a demand change during settled silence: fill from now on, not the time since
    /// settling, which the time views already showed as a gap.
    pub(super) fn unsettle(&mut self, now: Instant) {
        if self.settled {
            self.settled = false;
            self.filled = self.filled.max(self.frames_due(now));
        }
    }

    fn frames_due(&self, now: Instant) -> u64 {
        let quiet = now.saturating_duration_since(self.quiet_since);
        (quiet.min(LIMIT).as_secs_f64() * self.sample_rate) as u64
    }

    fn restart(&mut self, now: Instant) {
        self.quiet_since = now;
        self.filled = 0;
        self.settled = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{ProcessingEndpoint, create_block_transport};

    /// A filler that has seen one 480-frame block captured at `start`.
    fn after_one_block(
        infer: bool,
    ) -> (
        SilenceFill,
        Arc<SourceTelemetry>,
        ProcessingEndpoint,
        Instant,
    ) {
        let telemetry = Arc::new(SourceTelemetry::new());
        telemetry.set_state(SourceState::Running);
        let (mut capture, mut processing) =
            create_block_transport(1, 4, 480, Arc::clone(&telemetry)).unwrap();
        let mut fill = SilenceFill::new(Arc::clone(&telemetry), 48_000, 1, infer);
        let start = Instant::now();
        capture.write(&[0.5_f32; 480]);
        let mut block = processing.try_pop().unwrap();
        fill.captured(&mut block, start);
        processing.recycle(block);
        (fill, telemetry, processing, start)
    }

    fn drain(fill: &mut SilenceFill, now: Instant) -> u64 {
        let mut frames = 0;
        while let Some(silent) = fill.due(now) {
            frames += silent.frame_count as u64;
        }
        frames
    }

    #[test]
    fn silence_stands_in_after_the_grace_in_real_time_up_to_the_limit_and_shifts_later_blocks() {
        let telemetry = Arc::new(SourceTelemetry::new());
        telemetry.set_state(SourceState::Running);
        let (mut capture, mut processing) =
            create_block_transport(1, 4, 480, Arc::clone(&telemetry)).unwrap();
        let mut fill = SilenceFill::new(Arc::clone(&telemetry), 48_000, 1, true);
        let start = Instant::now();
        assert!(
            fill.due(start + Duration::from_secs(1)).is_none(),
            "nothing before any audio"
        );

        capture.write(&[0.5_f32; 480]);
        let mut block = processing.try_pop().unwrap();
        fill.captured(&mut block, start);
        processing.recycle(block);
        assert!(
            fill.due(start + Duration::from_millis(50)).is_none(),
            "within the grace"
        );

        // One second idle must add 48,000 frames after the captured 480, in 10 ms blocks.
        let mut covered = 480;
        while let Some(silent) = fill.due(start + Duration::from_secs(1)) {
            assert_eq!(silent.start_frame, covered);
            assert!(silent.samples(1).iter().all(|&sample| sample == 0.0));
            covered += silent.frame_count as u64;
        }
        assert_eq!(covered, 480 + 48_000);
        assert_eq!(fill.since(), 480);

        drain(&mut fill, start + Duration::from_secs(60));
        assert_eq!(
            fill.inserted_frames,
            (LIMIT.as_secs_f64() * 48_000.0) as u64
        );

        capture.write(&[0.5_f32; 480]);
        let mut block = processing.try_pop().unwrap();
        fill.captured(&mut block, start + Duration::from_secs(61));
        assert_eq!(block.start_frame, 480 + fill.inserted_frames);

        telemetry.set_state(SourceState::Paused);
        assert!(fill.due(start + Duration::from_secs(70)).is_none());
        telemetry.set_state(SourceState::Running);
        assert!(
            fill.due(start + Duration::from_millis(70_050)).is_none(),
            "the pause did not count"
        );
    }

    #[test]
    fn rest_ends_filling_until_audio_returns() {
        let (mut fill, _telemetry, _processing, start) = after_one_block(true);
        assert!(fill.due(start + Duration::from_secs(1)).is_some());
        fill.settle();
        assert_eq!(drain(&mut fill, start + Duration::from_secs(10)), 0);

        let mut block = AudioBlock::silence(1, 480);
        fill.captured(&mut block, start + Duration::from_secs(10));
        assert!(
            fill.due(start + Duration::from_millis(10_200)).is_some(),
            "the next silence fills again"
        );
    }

    #[test]
    fn a_new_demand_after_rest_fills_from_now_not_the_settled_time() {
        let (mut fill, _telemetry, _processing, start) = after_one_block(true);
        drain(&mut fill, start + Duration::from_secs(1));
        fill.settle();
        fill.unsettle(start + Duration::from_secs(5));
        assert_eq!(drain(&mut fill, start + Duration::from_secs(5)), 0);
        assert_eq!(
            drain(&mut fill, start + Duration::from_millis(5_500)),
            24_000,
            "half a second since the demand changed"
        );
        assert_eq!(fill.since(), 480, "the same silence continues");
    }

    #[test]
    fn a_lost_device_fills_without_grace_even_where_silence_is_not_inferred() {
        let (mut fill, telemetry, _processing, start) = after_one_block(false);
        assert!(
            fill.due(start + Duration::from_secs(1)).is_none(),
            "a running microphone without samples is not silent"
        );
        telemetry.set_state(SourceState::DeviceLost);
        assert_eq!(
            drain(&mut fill, start + Duration::from_millis(1_050)),
            50_400
        );
    }
}
