use std::{error::Error, fmt, sync::Arc};

use rustfft::{Fft, FftPlanner, num_complex::Complex32};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowFunction {
    PeriodicHann,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FftConfig {
    pub window_size: usize,
    pub hop_size: usize,
    pub floor_dbfs: f32,
}

impl FftConfig {
    pub fn new(
        window_size: usize,
        hop_size: usize,
        floor_dbfs: f32,
    ) -> Result<Self, FftConfigError> {
        if window_size < 16 {
            return Err(FftConfigError::WindowTooSmall);
        }
        if hop_size == 0 || hop_size > window_size {
            return Err(FftConfigError::InvalidHop);
        }
        if !floor_dbfs.is_finite() || floor_dbfs >= 0.0 {
            return Err(FftConfigError::InvalidFloor);
        }
        Ok(Self {
            window_size,
            hop_size,
            floor_dbfs,
        })
    }
}

impl Default for FftConfig {
    fn default() -> Self {
        Self {
            window_size: 2048,
            hop_size: 512,
            floor_dbfs: -160.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FftMetadata {
    pub window: WindowFunction,
    pub window_size: usize,
    pub hop_size: usize,
    pub bin_hz: f32,
    pub floor_dbfs: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FftConfigError {
    WindowTooSmall,
    InvalidHop,
    InvalidFloor,
}

impl fmt::Display for FftConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WindowTooSmall => f.write_str("FFT window must contain at least 16 samples"),
            Self::InvalidHop => f.write_str("FFT hop must be in 1..=window_size"),
            Self::InvalidFloor => f.write_str("FFT floor must be finite and below 0 dBFS"),
        }
    }
}

impl Error for FftConfigError {}

pub(crate) struct StreamingFft {
    config: FftConfig,
    fft: Arc<dyn Fft<f32>>,
    window: Box<[f32]>,
    coherent_sum: f32,
    ring: Box<[f32]>,
    write_index: usize,
    filled: usize,
    samples_since_evaluation: usize,
    complex: Box<[Complex32]>,
    scratch: Box<[Complex32]>,
    bins_dbfs: Box<[f32]>,
}

impl StreamingFft {
    pub(crate) fn new(config: FftConfig) -> Self {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(config.window_size);
        let window: Box<[f32]> = (0..config.window_size)
            .map(|index| {
                0.5 - 0.5
                    * (2.0 * std::f32::consts::PI * index as f32 / config.window_size as f32).cos()
            })
            .collect();
        let coherent_sum = window.iter().sum();
        let scratch_len = fft.get_inplace_scratch_len();
        Self {
            config,
            fft,
            window,
            coherent_sum,
            ring: vec![0.0; config.window_size].into_boxed_slice(),
            write_index: 0,
            filled: 0,
            samples_since_evaluation: 0,
            complex: vec![Complex32::default(); config.window_size].into_boxed_slice(),
            scratch: vec![Complex32::default(); scratch_len].into_boxed_slice(),
            bins_dbfs: vec![config.floor_dbfs; config.window_size / 2 + 1].into_boxed_slice(),
        }
    }

    pub(crate) fn metadata(&self, sample_rate_hz: u32) -> FftMetadata {
        FftMetadata {
            window: WindowFunction::PeriodicHann,
            window_size: self.config.window_size,
            hop_size: self.config.hop_size,
            bin_hz: sample_rate_hz as f32 / self.config.window_size as f32,
            floor_dbfs: self.config.floor_dbfs,
        }
    }

    pub(crate) fn push(&mut self, sample: f32) -> bool {
        self.ring[self.write_index] = sample;
        self.write_index += 1;
        if self.write_index == self.config.window_size {
            self.write_index = 0;
        }
        self.filled = (self.filled + 1).min(self.config.window_size);
        self.samples_since_evaluation += 1;

        let due = self.filled == self.config.window_size
            && (self.samples_since_evaluation >= self.config.window_size
                || self.samples_since_evaluation >= self.config.hop_size);
        if !due {
            return false;
        }
        self.samples_since_evaluation = 0;
        self.compute();
        true
    }

    pub(crate) fn bins_dbfs(&self) -> &[f32] {
        &self.bins_dbfs
    }

    pub(crate) fn reset(&mut self) {
        self.ring.fill(0.0);
        self.write_index = 0;
        self.filled = 0;
        self.samples_since_evaluation = 0;
    }

    fn compute(&mut self) {
        for index in 0..self.config.window_size {
            let sample = self.ring[(self.write_index + index) % self.config.window_size];
            self.complex[index] = Complex32::new(sample * self.window[index], 0.0);
        }
        self.fft
            .process_with_scratch(&mut self.complex, &mut self.scratch);

        let nyquist = self.config.window_size / 2;
        for (index, output) in self.bins_dbfs.iter_mut().enumerate() {
            let one_sided_scale = if index == 0 || index == nyquist {
                1.0 / self.coherent_sum
            } else {
                2.0 / self.coherent_sum
            };
            let peak_amplitude = self.complex[index].norm() * one_sided_scale;
            *output = if peak_amplitude > 0.0 {
                (20.0 * peak_amplitude.log10()).max(self.config.floor_dbfs)
            } else {
                self.config.floor_dbfs
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periodic_hann_reports_bin_centered_peak_amplitude_in_dbfs() {
        let config = FftConfig::new(2048, 512, -160.0).unwrap();
        let mut fft = StreamingFft::new(config);
        let sample_rate = 48_000.0;
        let target_bin = 43;
        let frequency = target_bin as f32 * sample_rate / config.window_size as f32;
        let amplitude = 0.25_f32;
        let mut produced = false;
        for index in 0..config.window_size {
            produced |= fft.push(
                amplitude
                    * (2.0 * std::f32::consts::PI * frequency * index as f32 / sample_rate).sin(),
            );
        }
        assert!(produced);
        let measured = fft.bins_dbfs()[target_bin];
        let expected = 20.0 * amplitude.log10();
        assert!(
            (measured - expected).abs() < 0.02,
            "{measured} vs {expected}"
        );
    }

    #[test]
    fn fft_advances_only_at_window_and_hop_boundaries() {
        let config = FftConfig::new(32, 8, -120.0).unwrap();
        let mut fft = StreamingFft::new(config);
        assert_eq!((0..31).filter(|_| fft.push(0.0)).count(), 0);
        assert!(fft.push(0.0));
        assert_eq!((0..7).filter(|_| fft.push(0.0)).count(), 0);
        assert!(fft.push(0.0));
    }
}
