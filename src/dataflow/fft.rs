use std::{error::Error, fmt, sync::Arc};

use realfft::{RealFftPlanner, RealToComplex, num_complex::Complex32};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowFunction {
    PeriodicHann,
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FftConfig {
    pub window_size: usize,
    pub hop_size: usize,
    pub floor_dbfs: f32,
}

/// Largest accepted FFT window: four times the largest the settings offer. Saved
/// or hand-written configurations beyond it are rejected before anything is
/// planned or allocated.
pub const MAX_WINDOW_SIZE: usize = 65_536;

impl FftConfig {
    pub fn new(
        window_size: usize,
        hop_size: usize,
        floor_dbfs: f32,
    ) -> Result<Self, FftConfigError> {
        if window_size < 16 {
            return Err(FftConfigError::WindowTooSmall);
        }
        if window_size > MAX_WINDOW_SIZE {
            return Err(FftConfigError::WindowTooLarge);
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

impl FftMetadata {
    pub(crate) fn of(config: FftConfig, sample_rate_hz: u32) -> Self {
        Self {
            window: WindowFunction::PeriodicHann,
            window_size: config.window_size,
            hop_size: config.hop_size,
            bin_hz: sample_rate_hz as f32 / config.window_size as f32,
            floor_dbfs: config.floor_dbfs,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FftConfigError {
    WindowTooSmall,
    WindowTooLarge,
    InvalidHop,
    InvalidFloor,
}

impl fmt::Display for FftConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WindowTooSmall => f.write_str("FFT window must contain at least 16 samples"),
            Self::WindowTooLarge => {
                write!(f, "FFT window must not exceed {MAX_WINDOW_SIZE} samples")
            }
            Self::InvalidHop => f.write_str("FFT hop must be in 1..=window_size"),
            Self::InvalidFloor => f.write_str("FFT floor must be finite and below 0 dBFS"),
        }
    }
}

impl Error for FftConfigError {}

/// Sliding-window magnitude spectrum: the first result after `window_size`
/// samples, then one every `hop_size` samples. Bins are peak amplitude in dBFS
/// (a full-scale sine centered on a bin reads 0 dBFS), clamped to the floor.
pub(crate) struct StreamingFft {
    config: FftConfig,
    fft: Arc<dyn RealToComplex<f32>>,
    window: Box<[f32]>,
    /// Per bin, `20·log10` of the one-sided amplitude scale.
    scale_db: Box<[f32]>,
    ring: Box<[f32]>,
    write_index: usize,
    filled: usize,
    samples_since_evaluation: usize,
    input: Vec<f32>,
    spectrum: Vec<Complex32>,
    scratch: Vec<Complex32>,
    bins_dbfs: Box<[f32]>,
}

impl StreamingFft {
    pub(crate) fn new(config: FftConfig) -> Self {
        let size = config.window_size;
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(size);
        let window: Box<[f32]> = (0..size)
            .map(|index| {
                0.5 - 0.5 * (2.0 * std::f32::consts::PI * index as f32 / size as f32).cos()
            })
            .collect();
        let coherent_sum: f32 = window.iter().sum();
        let nyquist = size / 2;
        let scale_db = (0..=nyquist)
            .map(|index| {
                let scale = if index == 0 || index == nyquist {
                    1.0
                } else {
                    2.0
                };
                20.0 * (scale / coherent_sum).log10()
            })
            .collect();
        Self {
            config,
            input: fft.make_input_vec(),
            spectrum: fft.make_output_vec(),
            scratch: fft.make_scratch_vec(),
            fft,
            window,
            scale_db,
            ring: vec![0.0; size].into_boxed_slice(),
            write_index: 0,
            filled: 0,
            samples_since_evaluation: 0,
            bins_dbfs: vec![config.floor_dbfs; nyquist + 1].into_boxed_slice(),
        }
    }

    /// Consume samples until a result is due. Returns how many samples were
    /// consumed when a new result was computed, or None when all of `samples` were
    /// consumed without one.
    pub(crate) fn push(&mut self, samples: &[f32]) -> Option<usize> {
        let size = self.config.window_size;
        let mut consumed = 0;
        while consumed < samples.len() {
            let needed = (size - self.filled)
                .max(self.config.hop_size - self.samples_since_evaluation.min(self.config.hop_size))
                .max(1);
            let taken = needed.min(samples.len() - consumed);
            self.write(&samples[consumed..consumed + taken]);
            consumed += taken;
            if taken == needed {
                self.samples_since_evaluation = 0;
                self.compute();
                return Some(consumed);
            }
        }
        None
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

    fn write(&mut self, mut samples: &[f32]) {
        let size = self.config.window_size;
        self.filled = (self.filled + samples.len()).min(size);
        self.samples_since_evaluation += samples.len();
        if samples.len() > size {
            samples = &samples[samples.len() - size..];
        }
        let first = samples.len().min(size - self.write_index);
        self.ring[self.write_index..self.write_index + first].copy_from_slice(&samples[..first]);
        self.ring[..samples.len() - first].copy_from_slice(&samples[first..]);
        self.write_index = (self.write_index + samples.len()) % size;
    }

    fn compute(&mut self) {
        let (newest, oldest) = self.ring.split_at(self.write_index);
        let (head, tail) = self.input.split_at_mut(oldest.len());
        let (head_window, tail_window) = self.window.split_at(oldest.len());
        for ((output, sample), weight) in head.iter_mut().zip(oldest).zip(head_window) {
            *output = sample * weight;
        }
        for ((output, sample), weight) in tail.iter_mut().zip(newest).zip(tail_window) {
            *output = sample * weight;
        }
        self.fft
            .process_with_scratch(&mut self.input, &mut self.spectrum, &mut self.scratch)
            .expect("buffers are sized by the planner");
        let floor = self.config.floor_dbfs;
        for ((output, bin), scale_db) in self
            .bins_dbfs
            .iter_mut()
            .zip(&self.spectrum)
            .zip(self.scale_db.iter())
        {
            let power = bin.norm_sqr();
            *output = if power > 0.0 {
                (10.0 * power.log10() + scale_db).max(floor)
            } else {
                floor
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(frames: usize, frequency: f32, amplitude: f32) -> Vec<f32> {
        (0..frames)
            .map(|index| {
                amplitude * (2.0 * std::f32::consts::PI * frequency * index as f32 / 48_000.0).sin()
            })
            .collect()
    }

    #[test]
    fn periodic_hann_reports_bin_centered_peak_amplitude_in_dbfs() {
        let config = FftConfig::new(2048, 512, -160.0).unwrap();
        let mut fft = StreamingFft::new(config);
        let target_bin = 43;
        let frequency = target_bin as f32 * 48_000.0 / config.window_size as f32;
        let amplitude = 0.25_f32;
        assert_eq!(fft.push(&sine(2048, frequency, amplitude)), Some(2048));
        let measured = fft.bins_dbfs()[target_bin];
        let expected = 20.0 * amplitude.log10();
        assert!(
            (measured - expected).abs() < 0.02,
            "{measured} vs {expected}"
        );
    }

    #[test]
    fn results_are_due_at_window_then_every_hop_regardless_of_slicing() {
        let config = FftConfig::new(32, 8, -120.0).unwrap();
        let samples = sine(100, 3_000.0, 0.5);
        let mut whole = StreamingFft::new(config);
        let mut due_whole = Vec::new();
        let mut position = 0;
        while let Some(consumed) = whole.push(&samples[position..]) {
            position += consumed;
            due_whole.push((position, whole.bins_dbfs().to_vec()));
        }
        assert_eq!(
            due_whole.iter().map(|(at, _)| *at).collect::<Vec<_>>(),
            [32, 40, 48, 56, 64, 72, 80, 88, 96]
        );

        let mut sliced = StreamingFft::new(config);
        let mut due_sliced = Vec::new();
        let mut offset = 0;
        for chunk in samples.chunks(7) {
            let mut position = 0;
            while let Some(consumed) = sliced.push(&chunk[position..]) {
                position += consumed;
                due_sliced.push((offset + position, sliced.bins_dbfs().to_vec()));
            }
            offset += chunk.len();
        }
        assert_eq!(due_whole, due_sliced);
    }

    #[test]
    fn window_size_is_accepted_up_to_the_maximum_and_rejected_beyond() {
        let config = FftConfig::new(MAX_WINDOW_SIZE, 512, -160.0).unwrap();
        assert_eq!(config.window_size, MAX_WINDOW_SIZE);
        assert_eq!(
            FftConfig::new(MAX_WINDOW_SIZE + 1, 512, -160.0),
            Err(FftConfigError::WindowTooLarge)
        );
        assert!(FftConfig::new(16, 16, -160.0).is_ok());
        assert_eq!(
            FftConfig::new(15, 8, -160.0),
            Err(FftConfigError::WindowTooSmall)
        );
    }

    #[test]
    fn hop_must_be_between_one_and_the_window_size() {
        assert_eq!(
            FftConfig::new(1024, 0, -160.0),
            Err(FftConfigError::InvalidHop)
        );
        assert_eq!(
            FftConfig::new(1024, 1025, -160.0),
            Err(FftConfigError::InvalidHop)
        );
        assert!(FftConfig::new(1024, 1, -160.0).is_ok());
        assert!(FftConfig::new(1024, 1024, -160.0).is_ok());
    }

    #[test]
    fn floor_must_be_finite_and_below_zero_dbfs() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, -0.0, 0.5] {
            assert_eq!(
                FftConfig::new(1024, 256, bad),
                Err(FftConfigError::InvalidFloor),
                "floor {bad} accepted"
            );
        }
        assert!(FftConfig::new(1024, 256, -0.001).is_ok());
        assert!(FftConfig::new(1024, 256, -160.0).is_ok());
    }
}
