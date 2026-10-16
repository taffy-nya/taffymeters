use std::collections::VecDeque;

const MOMENTARY_SECONDS: f32 = 0.4;
pub(super) const LUFS_FLOOR: f32 = -60.0;
/// Sample peaks below this (and silence) read as this, never −∞.
pub(super) const LEVELS_FLOOR_DBFS: f32 = -150.0;

/// Fold the largest absolute sample per channel of `interleaved` into `peaks`.
pub(super) fn accumulate_peaks(interleaved: &[f32], peaks: &mut [f32]) {
    for frame in interleaved.chunks_exact(peaks.len()) {
        for (peak, sample) in peaks.iter_mut().zip(frame) {
            *peak = peak.max(sample.abs());
        }
    }
}

/// Sample peak amplitude in dBFS, floored at `LEVELS_FLOOR_DBFS`.
pub(super) fn peak_dbfs(amplitude: f32) -> f32 {
    if amplitude > 0.0 {
        (20.0 * amplitude.log10()).max(LEVELS_FLOOR_DBFS)
    } else {
        LEVELS_FLOOR_DBFS
    }
}

/// Momentary loudness of the whole programme: every channel K-weighted, the
/// 400 ms mean squares summed with weight 1 (BS.1770).
pub(super) struct LoudnessAnalyzer {
    channels: Vec<ChannelLevel>,
}

impl LoudnessAnalyzer {
    pub(super) fn new(sample_rate_hz: u32, channel_count: usize) -> Self {
        Self {
            channels: (0..channel_count)
                .map(|_| ChannelLevel::new(sample_rate_hz))
                .collect(),
        }
    }

    pub(super) fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.reset();
        }
    }

    pub(super) fn process(&mut self, interleaved: &[f32]) {
        for frame in interleaved.chunks_exact(self.channels.len()) {
            for (&sample, channel) in frame.iter().zip(&mut self.channels) {
                channel.process(sample);
            }
        }
    }

    pub(super) fn lufs(&self) -> f32 {
        let mean_square: f64 = self.channels.iter().map(ChannelLevel::mean_square).sum();
        if mean_square < 1.0e-10 {
            LUFS_FLOOR
        } else {
            (-0.691 + 10.0 * mean_square.log10() as f32).max(LUFS_FLOOR)
        }
    }
}

struct ChannelLevel {
    stage1: Biquad,
    stage2: Biquad,
    squared: VecDeque<f64>,
    sum: f64,
    window_size: usize,
}

impl ChannelLevel {
    fn new(sample_rate_hz: u32) -> Self {
        let sample_rate = sample_rate_hz as f64;
        let window_size = (sample_rate_hz as f32 * MOMENTARY_SECONDS).ceil() as usize;
        Self {
            stage1: Biquad::stage1(sample_rate),
            stage2: Biquad::stage2(sample_rate),
            squared: VecDeque::with_capacity(window_size + 1),
            sum: 0.0,
            window_size,
        }
    }

    fn process(&mut self, sample: f32) {
        let filtered = self.stage2.process(self.stage1.process(sample as f64));
        let squared = filtered * filtered;
        self.squared.push_back(squared);
        self.sum += squared;
        if self.squared.len() > self.window_size
            && let Some(oldest) = self.squared.pop_front()
        {
            self.sum -= oldest;
        }
    }

    /// Mean square of the window so far; zero before any sample.
    fn mean_square(&self) -> f64 {
        if self.squared.is_empty() {
            0.0
        } else {
            (self.sum / self.squared.len() as f64).max(0.0)
        }
    }

    fn reset(&mut self) {
        self.stage1.reset();
        self.stage2.reset();
        self.squared.clear();
        self.sum = 0.0;
    }
}

struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl Biquad {
    #[inline]
    fn process(&mut self, input: f64) -> f64 {
        let output = self.b0 * input + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = input;
        self.y2 = self.y1;
        self.y1 = output;
        output
    }

    fn reset(&mut self) {
        self.x1 = 0.0;
        self.x2 = 0.0;
        self.y1 = 0.0;
        self.y2 = 0.0;
    }

    fn stage1(sample_rate: f64) -> Self {
        let frequency = 1_681.974_450_955_533;
        let gain_db = 3.999_843_853_973_347;
        let q = 0.707_175_236_955_419_6;
        let k = (std::f64::consts::PI * frequency / sample_rate).tan();
        let high = 10_f64.powf(gain_db / 20.0);
        let band = 10_f64.powf(gain_db / 40.0);
        let a0 = 1.0 + k / q + k * k;
        Self {
            b0: (high + band * k / q + k * k) / a0,
            b1: 2.0 * (k * k - high) / a0,
            b2: (high - band * k / q + k * k) / a0,
            a1: 2.0 * (k * k - 1.0) / a0,
            a2: (1.0 - k / q + k * k) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    fn stage2(sample_rate: f64) -> Self {
        let frequency = 38.135_470_876_024_44;
        let q = 0.500_327_037_323_877_3;
        let k = (std::f64::consts::PI * frequency / sample_rate).tan();
        let a0 = 1.0 + k / q + k * k;
        Self {
            b0: 1.0 / a0,
            b1: -2.0 / a0,
            b2: 1.0 / a0,
            a1: 2.0 * (k * k - 1.0) / a0,
            a2: (1.0 - k / q + k * k) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn sine_1khz(frames: usize) -> Vec<f32> {
        (0..frames)
            .map(|index| {
                (2.0 * std::f64::consts::PI * 1_000.0 * index as f64 / f64::from(RATE)).sin() as f32
            })
            .collect()
    }

    #[test]
    fn silence_reads_the_floors() {
        let mut analyzer = LoudnessAnalyzer::new(RATE, 2);
        assert_eq!(analyzer.lufs(), LUFS_FLOOR, "before any input");
        analyzer.process(&vec![0.0; 2 * 4_800]);
        assert_eq!(analyzer.lufs(), LUFS_FLOOR);
        assert_eq!(peak_dbfs(0.0), LEVELS_FLOOR_DBFS);
        assert_eq!(peak_dbfs(1.0e-12), LEVELS_FLOOR_DBFS);
        assert!((peak_dbfs(0.5) - -6.020_6).abs() < 1e-3);
    }

    #[test]
    fn full_scale_1khz_sine_reads_minus_3_01_lufs_after_a_full_momentary_window() {
        // BS.1770: K-weighting is +0.691 dB at 1 kHz, which the -0.691 offset
        // cancels, so a 0 dBFS sine (mean square 1/2) reads 10*log10(0.5) = -3.01
        // LUFS. The 0.1 dB tolerance covers the biquad's deviation from the
        // reference at 48 kHz. One second (> 400 ms window + filter settling).
        let mut analyzer = LoudnessAnalyzer::new(RATE, 1);
        analyzer.process(&sine_1khz(RATE as usize));
        let lufs = analyzer.lufs();
        assert!((lufs + 3.01).abs() < 0.1, "momentary loudness {lufs} LUFS");
    }

    #[test]
    fn programme_loudness_sums_the_channels() {
        // The same sine in both channels is twice the power of one channel:
        // -3.01 + 3.01 = 0 LUFS; in the left channel only, -3.01 LUFS.
        let sine = sine_1khz(RATE as usize);
        let mut both = LoudnessAnalyzer::new(RATE, 2);
        both.process(&sine.iter().flat_map(|&s| [s, s]).collect::<Vec<_>>());
        let mut left = LoudnessAnalyzer::new(RATE, 2);
        left.process(&sine.iter().flat_map(|&s| [s, 0.0]).collect::<Vec<_>>());
        assert!(both.lufs().abs() < 0.1, "both {} LUFS", both.lufs());
        assert!(
            (left.lufs() + 3.01).abs() < 0.1,
            "left {} LUFS",
            left.lufs()
        );
    }

    #[test]
    fn loudness_forgets_audio_older_than_the_momentary_window() {
        let mut analyzer = LoudnessAnalyzer::new(RATE, 1);
        analyzer.process(&sine_1khz(RATE as usize));
        assert!(analyzer.lufs() > -10.0);
        // 0.6 s of silence: the 0.4 s window holds only silence (and no filter
        // ringing) by now.
        analyzer.process(&vec![0.0; 28_800]);
        assert_eq!(analyzer.lufs(), LUFS_FLOOR);
    }

    #[test]
    fn sample_peak_is_the_largest_absolute_value_per_channel() {
        let mut peaks = [0.0; 2];
        // Left: 0.5, -0.8, 0.3 (the negative sample is the peak);
        // right: 0.25, 0.1, -0.2.
        accumulate_peaks(&[0.5, 0.25, -0.8, 0.1, 0.3, -0.2], &mut peaks);
        assert_eq!(peaks, [0.8, 0.25]);
        // Accumulates over blocks until the caller starts a new interval.
        accumulate_peaks(&[0.1, -0.9], &mut peaks);
        assert_eq!(peaks, [0.8, 0.9]);
    }
}
