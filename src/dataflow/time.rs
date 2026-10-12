use std::collections::VecDeque;

const MOMENTARY_SECONDS: f32 = 0.4;
const LUFS_FLOOR: f32 = -60.0;

pub(super) struct LevelAnalyzer {
    channels: Vec<ChannelLevel>,
}

impl LevelAnalyzer {
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

    pub(super) fn process(&mut self, interleaved: &[f32], lufs: &mut [f32], peaks: &mut [f32]) {
        let channel_count = self.channels.len();
        assert_eq!(lufs.len(), channel_count);
        assert_eq!(peaks.len(), channel_count);
        peaks.fill(0.0);
        for frame in interleaved.chunks_exact(channel_count) {
            for (index, (&sample, channel)) in frame.iter().zip(&mut self.channels).enumerate() {
                peaks[index] = peaks[index].max(sample.abs());
                channel.process(sample);
            }
        }
        for (output, channel) in lufs.iter_mut().zip(&self.channels) {
            *output = channel.lufs();
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
        Self {
            stage1: Biquad::stage1(sample_rate),
            stage2: Biquad::stage2(sample_rate),
            squared: VecDeque::with_capacity(
                (sample_rate_hz as f32 * MOMENTARY_SECONDS).ceil() as usize
            ),
            sum: 0.0,
            window_size: (sample_rate_hz as f32 * MOMENTARY_SECONDS).ceil() as usize,
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

    fn lufs(&self) -> f32 {
        if self.squared.is_empty() {
            return LUFS_FLOOR;
        }
        let mean_square = (self.sum / self.squared.len() as f64).max(0.0);
        if mean_square < 1.0e-10 {
            LUFS_FLOOR
        } else {
            (-0.691 + 10.0 * mean_square.log10() as f32).max(LUFS_FLOOR)
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
    fn silence_reads_the_lufs_floor() {
        let mut analyzer = LevelAnalyzer::new(RATE, 2);
        let mut lufs = [0.0; 2];
        let mut peaks = [1.0; 2];
        analyzer.process(&vec![0.0; 2 * 4_800], &mut lufs, &mut peaks);
        assert_eq!(lufs, [LUFS_FLOOR; 2]);
        assert_eq!(peaks, [0.0; 2]);
    }

    #[test]
    fn full_scale_1khz_sine_reads_minus_3_01_lufs_after_a_full_momentary_window() {
        // BS.1770: K-weighting is +0.691 dB at 1 kHz, which the -0.691 offset
        // cancels, so a 0 dBFS sine (mean square 1/2) reads 10*log10(0.5) = -3.01
        // LUFS. The 0.1 dB tolerance covers the biquad's deviation from the
        // reference at 48 kHz. One second (> 400 ms window + filter settling).
        let mut analyzer = LevelAnalyzer::new(RATE, 1);
        let mut lufs = [0.0];
        let mut peaks = [0.0];
        analyzer.process(&sine_1khz(RATE as usize), &mut lufs, &mut peaks);
        assert!(
            (lufs[0] + 3.01).abs() < 0.1,
            "momentary loudness {} LUFS",
            lufs[0]
        );
    }

    #[test]
    fn channels_are_measured_independently_in_interleaved_input() {
        let mut analyzer = LevelAnalyzer::new(RATE, 2);
        let interleaved: Vec<f32> = sine_1khz(RATE as usize)
            .into_iter()
            .flat_map(|sample| [sample, 0.0])
            .collect();
        let mut lufs = [0.0; 2];
        let mut peaks = [0.0; 2];
        analyzer.process(&interleaved, &mut lufs, &mut peaks);
        assert!((lufs[0] + 3.01).abs() < 0.1, "left {} LUFS", lufs[0]);
        assert_eq!(lufs[1], LUFS_FLOOR);
        assert!(peaks[0] > 0.99 && peaks[0] <= 1.0, "left peak {}", peaks[0]);
        assert_eq!(peaks[1], 0.0);
    }

    #[test]
    fn loudness_forgets_audio_older_than_the_momentary_window() {
        let mut analyzer = LevelAnalyzer::new(RATE, 1);
        let mut lufs = [0.0];
        let mut peaks = [0.0];
        analyzer.process(&sine_1khz(RATE as usize), &mut lufs, &mut peaks);
        assert!(lufs[0] > -10.0);
        // 0.6 s of silence: the 0.4 s window holds only silence (and no filter
        // ringing) by now.
        analyzer.process(&vec![0.0; 28_800], &mut lufs, &mut peaks);
        assert_eq!(lufs[0], LUFS_FLOOR);
    }

    #[test]
    fn sample_peak_is_the_largest_absolute_value_of_the_interval() {
        let mut analyzer = LevelAnalyzer::new(RATE, 2);
        let mut lufs = [0.0; 2];
        let mut peaks = [0.0; 2];
        // Left: 0.5, -0.8, 0.3 (the negative sample is the peak);
        // right: 0.25, 0.1, -0.2.
        analyzer.process(&[0.5, 0.25, -0.8, 0.1, 0.3, -0.2], &mut lufs, &mut peaks);
        assert_eq!(peaks, [0.8, 0.25]);
        // The next interval starts over: a quieter block does not inherit the peak.
        analyzer.process(&[0.1, -0.05], &mut lufs, &mut peaks);
        assert_eq!(peaks, [0.1, 0.05]);
    }
}
