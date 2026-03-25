use rustfft::{Fft, FftPlanner, num_complex::Complex, num_traits::Zero};
use std::sync::Arc;

pub struct DspProcessor {
    fft_size: usize,
    fft: Arc<dyn Fft<f32>>,

    complex_buffer: Vec<Complex<f32>>,
    scratch_buffer: Vec<Complex<f32>>,
}

impl DspProcessor {
    pub fn new(fft_size: usize) -> Self {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(fft_size);
        
        Self {
            fft_size,
            fft: Arc::clone(&fft),
            complex_buffer: vec![Complex::zero(); fft_size],
            scratch_buffer: vec![Complex::zero(); fft_size],
        }
    }

    /// 处理音频数据并返回幅度谱
    pub fn process(&mut self, audio_data: &[f32]) -> Vec<f32> {
        for (i, &sample) in audio_data.iter().take(self.fft_size).enumerate() {
            self.complex_buffer[i] = Complex::new(sample, 0.0);
        }

        self.fft.process_with_scratch(&mut self.complex_buffer, &mut self.scratch_buffer);

        self.complex_buffer.iter()
            .take(self.fft_size / 2)    // 输出是共轭对称的，只需要前半部分 (Nyquist Frequency)
            .map(|c| c.norm())
            .collect()
    }

    pub fn set_fft_size(&mut self, new_size: usize) {
        if new_size != self.fft_size {
            *self = Self::new(new_size);
        }
    }
}
