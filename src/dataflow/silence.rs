//! Silence for a source that delivers nothing. A system loopback sends no samples
//! at all while nothing plays, so every analysis would stop at its last result:
//! meters hold their level, the Spectrum its curve, the peak markers never fall.
//! The worker then processes silent blocks in real time itself, for a bounded
//! time that lets stateful nodes (peak hold and fall, loudness windows) settle,
//! and stops; the time views keep scrolling on their own after that (see
//! `plotting::ViewClock`). Captured blocks are moved later by the silence
//! inserted before them, so the analysis timeline stays continuous.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use crate::audio::{AudioBlock, SourceState, SourceTelemetry};

/// How long the source may deliver nothing before silence stands in for it
/// (blocks normally arrive every 10 ms). Shorter than the time views' own grace,
/// so they see the silent samples rather than a gap first.
const GRACE: Duration = Duration::from_millis(100);
/// Longest silence inserted per pause in the source: enough for the peak markers
/// (hold up to 3 s, then falling) and the 400 ms loudness window to settle.
const LIMIT: Duration = Duration::from_secs(6);
/// Silent frames per inserted block: 10 ms.
const BLOCK_SECONDS: f64 = 0.01;

pub(super) struct SilenceFill {
    telemetry: Arc<SourceTelemetry>,
    sample_rate: f64,
    block: AudioBlock,
    block_frames: usize,
    /// Frames and blocks inserted so far; every captured block moves on by them.
    inserted_frames: u64,
    inserted_blocks: u64,
    /// Start frame and sequence the next block continues with (engine timeline);
    /// `None` until the source delivered something.
    next: Option<(u64, u64)>,
    /// When the current lack of samples began (the last captured block, or the
    /// source resuming from a pause).
    quiet_since: Instant,
    /// Frames inserted since then.
    filled: u64,
}

impl SilenceFill {
    pub(super) fn new(telemetry: Arc<SourceTelemetry>, sample_rate: u32, channels: usize) -> Self {
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
        }
    }

    /// A captured block, moved later by the silence inserted before it.
    pub(super) fn captured(&mut self, block: &mut AudioBlock, now: Instant) {
        block.start_frame = block.start_frame.saturating_add(self.inserted_frames);
        block.sequence = block.sequence.wrapping_add(self.inserted_blocks);
        self.next = Some((
            block.start_frame.saturating_add(block.frame_count as u64),
            block.sequence.wrapping_add(1),
        ));
        self.quiet_since = now;
        self.filled = 0;
    }

    /// The silent block due now, if the source has been quiet past the grace; it
    /// covers the time since samples stopped, up to the limit, in real time.
    pub(super) fn due(&mut self, now: Instant) -> Option<&AudioBlock> {
        if self.telemetry.state() != SourceState::Running {
            // A paused (or stopped) source is not silent; its pause never counts.
            self.quiet_since = now;
            self.filled = 0;
            return None;
        }
        let (start_frame, sequence) = self.next?;
        let quiet = now.saturating_duration_since(self.quiet_since);
        if quiet < GRACE {
            return None;
        }
        let due = (quiet.min(LIMIT).as_secs_f64() * self.sample_rate) as u64;
        let frames = due
            .saturating_sub(self.filled)
            .min(self.block_frames as u64) as usize;
        if frames == 0 {
            return None;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::create_block_transport;

    #[test]
    fn silence_stands_in_after_the_grace_in_real_time_up_to_the_limit_and_shifts_later_blocks() {
        let telemetry = Arc::new(SourceTelemetry::new());
        telemetry.set_state(SourceState::Running);
        let (mut capture, mut processing) =
            create_block_transport(1, 4, 480, Arc::clone(&telemetry)).unwrap();
        let mut fill = SilenceFill::new(Arc::clone(&telemetry), 48_000, 1);
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

        // 1 s quiet: 1 s of silence follows frame 480, in 10 ms blocks.
        let mut covered = 480;
        while let Some(silent) = fill.due(start + Duration::from_secs(1)) {
            assert_eq!(silent.start_frame, covered);
            assert!(silent.samples(1).iter().all(|&sample| sample == 0.0));
            covered += silent.frame_count as u64;
        }
        assert_eq!(covered, 480 + 48_000);

        // Never more than the limit, however long the source stays quiet.
        while fill.due(start + Duration::from_secs(60)).is_some() {}
        assert_eq!(
            fill.inserted_frames,
            (LIMIT.as_secs_f64() * 48_000.0) as u64
        );

        // Audio again: it continues right after the silence.
        capture.write(&[0.5_f32; 480]);
        let mut block = processing.try_pop().unwrap();
        fill.captured(&mut block, start + Duration::from_secs(61));
        assert_eq!(block.start_frame, 480 + fill.inserted_frames);

        // A paused source is not silence.
        telemetry.set_state(SourceState::Paused);
        assert!(fill.due(start + Duration::from_secs(70)).is_none());
        telemetry.set_state(SourceState::Running);
        assert!(
            fill.due(start + Duration::from_millis(70_050)).is_none(),
            "the pause did not count"
        );
    }
}
