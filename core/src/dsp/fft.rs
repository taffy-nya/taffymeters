use rustfft::{Fft, FftPlanner, num_complex::Complex, num_traits::Zero};
use std::sync::Arc;

pub struct FftProcessor {
    fft_size: usize,
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    complex_buf: Vec<Complex<f32>>,
    scratch_buf: Vec<Complex<f32>>,
}

impl FftProcessor {
    pub fn new(fft_size: usize) -> Self {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(fft_size);
        let window = (0..fft_size)
            .map(|i| {
                0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32
                    / (fft_size as f32 - 1.0)).cos())
            })
            .collect();
        Self {
            fft_size,
            fft: Arc::clone(&fft),
            window,
            complex_buf: vec![Complex::zero(); fft_size],
            scratch_buf: vec![Complex::zero(); fft_size],
        }
    }

    pub fn compute(&mut self, audio: &[f32]) -> Vec<f32> {
        let mut out = Vec::new();
        self.compute_into(audio, &mut out);
        out
    }

    pub fn compute_into(&mut self, audio: &[f32], out: &mut Vec<f32>) {
        for i in 0..self.fft_size {
            let s = audio.get(i).copied().unwrap_or_default();
            self.complex_buf[i] = Complex::new(s * self.window[i], 0.0);
        }
        self.fft.process_with_scratch(&mut self.complex_buf, &mut self.scratch_buf);

        out.resize(self.fft_size / 2, 0.0);
        for (dst, c) in out.iter_mut().zip(self.complex_buf.iter()) {
            *dst = c.norm();
        }
    }

    pub fn fft_size(&self) -> usize { self.fft_size }

    pub fn resize(&mut self, new_size: usize) {
        if new_size != self.fft_size { *self = Self::new(new_size); }
    }
}
