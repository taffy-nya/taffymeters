//! Per-hop spectrum algorithms. Each writes into buffers it owns, so steady-state
//! processing does not allocate.

use super::spec::{PeakConfig, SmoothConfig};

const TILT_REFERENCE_HZ: f32 = 1_000.0;

/// `db_per_octave · log2(f / 1 kHz)` per bin; bin 0 uses the first bin frequency.
pub(crate) fn tilt_table(table: &mut Vec<f32>, bins: usize, bin_hz: f32, db_per_octave: f32) {
    table.clear();
    table.extend((0..bins).map(|index| {
        let frequency = (index as f32 * bin_hz).max(bin_hz);
        db_per_octave * (frequency / TILT_REFERENCE_HZ).log2()
    }));
}

pub(crate) fn offset_with_floor(output: &mut Vec<f32>, input: &[f32], offsets: &[f32], floor: f32) {
    output.clear();
    output.extend(
        input
            .iter()
            .zip(offsets)
            .map(|(value, offset)| (value + offset).max(floor)),
    );
}

pub(crate) fn add_with_floor(output: &mut Vec<f32>, input: &[f32], offset: f32, floor: f32) {
    output.clear();
    output.extend(input.iter().map(|value| (value + offset).max(floor)));
}

/// Exponential approach to each bin, with attack for rising and release for
/// falling values, over elapsed source time.
#[derive(Default)]
pub(crate) struct SmoothState {
    pub(crate) values: Vec<f32>,
    last_frame: Option<u64>,
}

impl SmoothState {
    pub(crate) fn process(
        &mut self,
        input: &[f32],
        processed_through_frame: u64,
        sample_rate_hz: u32,
        config: SmoothConfig,
    ) {
        let Some(last_frame) = self.last_frame.filter(|_| self.values.len() == input.len()) else {
            self.values.clear();
            self.values.extend_from_slice(input);
            self.last_frame = Some(processed_through_frame);
            return;
        };
        let elapsed_seconds =
            processed_through_frame.saturating_sub(last_frame) as f32 / sample_rate_hz as f32;
        let alpha = |time_ms: f32| {
            if time_ms == 0.0 {
                1.0
            } else {
                1.0 - (-elapsed_seconds / (time_ms / 1_000.0)).exp()
            }
        };
        let attack = alpha(config.attack_ms);
        let release = alpha(config.release_ms);
        for (current, target) in self.values.iter_mut().zip(input.iter().copied()) {
            let alpha = if target >= *current { attack } else { release };
            *current += (target - *current) * alpha;
        }
        self.last_frame = Some(processed_through_frame);
    }

    pub(crate) fn reset(&mut self) {
        self.values.clear();
        self.last_frame = None;
    }
}

/// Per-bin maximum held for `hold_ms`, then decaying linearly in dB/s toward the
/// current value, in source time.
#[derive(Default)]
pub(crate) struct PeakState {
    pub(crate) values: Vec<f32>,
    hold_until: Vec<u64>,
    last_frame: Option<u64>,
}

impl PeakState {
    pub(crate) fn process(
        &mut self,
        input: &[f32],
        processed_through_frame: u64,
        sample_rate_hz: u32,
        config: PeakConfig,
    ) {
        if self.values.len() != input.len() || self.last_frame.is_none() {
            self.values.clear();
            self.values.extend_from_slice(input);
            self.hold_until.clear();
            self.hold_until.resize(input.len(), processed_through_frame);
        }
        let last_frame = self.last_frame.unwrap_or(processed_through_frame);
        let hold_frames = (config.hold_ms * sample_rate_hz as f32 / 1_000.0).round() as u64;
        for ((peak, hold_until), sample) in self
            .values
            .iter_mut()
            .zip(self.hold_until.iter_mut())
            .zip(input.iter().copied())
        {
            if sample >= *peak {
                *peak = sample;
                *hold_until = processed_through_frame.saturating_add(hold_frames);
            } else if processed_through_frame > *hold_until {
                let decay_start = last_frame.max(*hold_until);
                let elapsed_seconds = processed_through_frame.saturating_sub(decay_start) as f32
                    / sample_rate_hz as f32;
                *peak = (*peak - config.decay_db_per_second * elapsed_seconds).max(sample);
            }
        }
        self.last_frame = Some(processed_through_frame);
    }

    pub(crate) fn reset(&mut self) {
        self.values.clear();
        self.hold_until.clear();
        self.last_frame = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilt_is_zero_at_one_kilohertz_and_scales_per_octave() {
        let mut table = Vec::new();
        tilt_table(&mut table, 5, 500.0, 3.0);
        assert!(table[2].abs() < 1e-6, "1 kHz bin");
        assert!((table[4] - 3.0).abs() < 1e-5, "2 kHz bin");
        assert!((table[1] + 3.0).abs() < 1e-5, "500 Hz bin");
        assert_eq!(table[0], table[1], "DC uses the first bin frequency");
    }

    #[test]
    fn smooth_uses_attack_for_rising_and_release_for_falling_bins() {
        let config = SmoothConfig::new(0.0, 1_000.0).unwrap();
        let mut state = SmoothState::default();
        state.process(&[-60.0, -20.0], 0, 48_000, config);
        state.process(&[-20.0, -60.0], 48_000, 48_000, config);
        assert_eq!(state.values[0], -20.0, "zero attack follows a rise at once");
        let expected = -20.0 + (-60.0 + 20.0) * (1.0 - (-1.0_f32).exp());
        assert!((state.values[1] - expected).abs() < 1e-4);
    }

    #[test]
    fn peak_holds_then_decays_by_source_time() {
        let config = PeakConfig::new(1_000.0, 10.0).unwrap();
        let mut state = PeakState::default();
        state.process(&[-10.0], 0, 1_000, config);
        state.process(&[-50.0], 500, 1_000, config);
        assert_eq!(state.values[0], -10.0, "held");
        state.process(&[-50.0], 2_000, 1_000, config);
        assert!(
            (state.values[0] - -20.0).abs() < 1e-4,
            "{}",
            state.values[0]
        );
    }
}
