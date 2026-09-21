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

    pub(super) fn process(&mut self, interleaved: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let channel_count = self.channels.len();
        let mut peaks = vec![0.0_f32; channel_count];
        for frame in interleaved.chunks_exact(channel_count) {
            for (index, (&sample, channel)) in frame.iter().zip(&mut self.channels).enumerate() {
                peaks[index] = peaks[index].max(sample.abs());
                channel.process(sample);
            }
        }
        let lufs = self.channels.iter().map(ChannelLevel::lufs).collect();
        (lufs, peaks)
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
