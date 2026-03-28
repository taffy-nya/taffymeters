use rustfft::{Fft, FftPlanner, num_complex::Complex, num_traits::Zero};
use std::sync::Arc;

pub struct FftProcessor {
    fft_size: usize,
    fft: Arc<dyn Fft<f32>>,
    complex_buf: Vec<Complex<f32>>,
    scratch_buf: Vec<Complex<f32>>,
}

impl FftProcessor {
    pub fn new(fft_size: usize) -> Self {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(fft_size);
        Self {
            fft_size,
            fft: Arc::clone(&fft),
            complex_buf: vec![Complex::zero(); fft_size],
            scratch_buf: vec![Complex::zero(); fft_size],
        }
    }

    /// 对输入音频应用 Hann window 后做 FFT，返回前半段的幅度谱
    pub fn compute(&mut self, audio: &[f32]) -> Vec<f32> {
        for (i, &s) in audio.iter().take(self.fft_size).enumerate() {
            let w = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32
                / (self.fft_size as f32 - 1.0)).cos());
            self.complex_buf[i] = Complex::new(s * w, 0.0);
        }
        self.fft.process_with_scratch(&mut self.complex_buf, &mut self.scratch_buf);
        self.complex_buf.iter().take(self.fft_size / 2).map(|c| c.norm()).collect()
    }

    pub fn fft_size(&self) -> usize { self.fft_size }

    pub fn resize(&mut self, new_size: usize) {
        if new_size != self.fft_size { *self = Self::new(new_size); }
    }
}

/// 将原始 FFT bin 映射到 N 个对数间隔的视觉频带
pub struct LogSpectrumMapper {
    pub bands: usize,
    pub min_freq: f32,
    pub max_freq: f32,
}

impl LogSpectrumMapper {
    pub fn new(bands: usize) -> Self {
        Self { bands, min_freq: 20.0, max_freq: 20_000.0 }
    }

    /// 把原始 FFT 幅度谱映射到 self.bands 个对数频带的线性幅度值
    pub fn map(&self, fft: &[f32], sample_rate: f32) -> Vec<f32> {
        let hz_per_bin = sample_rate / (fft.len() * 2) as f32;

        (0..self.bands).map(|i| {
            let t0 = i as f32 / self.bands as f32;
            let t1 = (i + 1) as f32 / self.bands as f32;
            let f0 = self.min_freq * (self.max_freq / self.min_freq).powf(t0);
            let f1 = self.min_freq * (self.max_freq / self.min_freq).powf(t1);
            let b0 = f0 / hz_per_bin;
            let b1 = f1 / hz_per_bin;
            Self::avg_bins(fft, b0, b1)
        }).collect()
    }

    pub fn to_db(amp: f32) -> f32 {
        (amp * 200.0 + 1.0).log10().max(0.0)
    }

    fn avg_bins(data: &[f32], b0: f32, b1: f32) -> f32 {
        if b1 - b0 < 1.0 {
            // 低频区：一个 FFT 点要分给多个视觉点，线性插值
            Self::interpolate(data, (b0 + b1) / 2.0)
        } else {
            // 高频区：多个 FFT 点挤进一个视觉点，取平均
            let start = b0.ceil() as usize;
            let end = (b1.floor() as usize).min(data.len());
            if start < end {
                let sum: f32 = data[start..end].iter().sum();
                sum / (end - start) as f32
            } else {
                Self::interpolate(data, (b0 + b1) / 2.0)
            }
        }
    }

    fn interpolate(data: &[f32], idx: f32) -> f32 {
        let i0 = idx.floor() as usize;
        let i1 = i0 + 1;
        if i0 >= data.len() { return 0.0; }
        if i1 >= data.len() { return data[i0]; }
        let t = idx - i0 as f32;
        data[i0] * (1.0 - t) + data[i1] * t
    }
}
