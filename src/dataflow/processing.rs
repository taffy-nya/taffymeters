use std::{collections::VecDeque, error::Error, fmt, sync::Arc};

use crate::snapshot::SpectrumHistoryFrame;

use super::{ChannelSelection, FftConfig};

const TILT_REFERENCE_HZ: f32 = 1_000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SharedAnalysisId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SmoothConfig {
    pub attack_ms: f32,
    pub release_ms: f32,
}

impl SmoothConfig {
    pub fn new(attack_ms: f32, release_ms: f32) -> Result<Self, RequestError> {
        let value = Self {
            attack_ms,
            release_ms,
        };
        value.validate()?;
        Ok(value.canonicalized())
    }

    fn validate(self) -> Result<(), RequestError> {
        if !self.attack_ms.is_finite()
            || !self.release_ms.is_finite()
            || self.attack_ms < 0.0
            || self.release_ms < 0.0
        {
            return Err(RequestError::InvalidSmoothTime);
        }
        Ok(())
    }

    fn canonicalized(mut self) -> Self {
        self.attack_ms = canonical_zero(self.attack_ms);
        self.release_ms = canonical_zero(self.release_ms);
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeakConfig {
    pub hold_ms: f32,
    pub decay_db_per_second: f32,
}

impl PeakConfig {
    pub fn new(hold_ms: f32, decay_db_per_second: f32) -> Result<Self, RequestError> {
        let value = Self {
            hold_ms,
            decay_db_per_second,
        };
        value.validate()?;
        Ok(value.canonicalized())
    }

    fn validate(self) -> Result<(), RequestError> {
        if !self.hold_ms.is_finite()
            || !self.decay_db_per_second.is_finite()
            || self.hold_ms < 0.0
            || self.decay_db_per_second < 0.0
        {
            return Err(RequestError::InvalidPeakTime);
        }
        Ok(())
    }

    fn canonicalized(mut self) -> Self {
        self.hold_ms = canonical_zero(self.hold_ms);
        self.decay_db_per_second = canonical_zero(self.decay_db_per_second);
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HistoryConfig {
    pub duration_seconds: f32,
}

impl HistoryConfig {
    pub fn new(duration_seconds: f32) -> Result<Self, RequestError> {
        let value = Self { duration_seconds };
        value.validate()?;
        Ok(value)
    }

    fn validate(self) -> Result<(), RequestError> {
        if !self.duration_seconds.is_finite() || self.duration_seconds <= 0.0 {
            return Err(RequestError::InvalidHistoryDuration);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpectrumRequest {
    pub channel: ChannelSelection,
    pub fft: FftConfig,
    pub tilt_db_per_octave: f32,
    pub smooth: Option<SmoothConfig>,
    pub center_db: f32,
    pub gain_db: f32,
    pub peak: Option<PeakConfig>,
    pub history: Option<HistoryConfig>,
}

impl SpectrumRequest {
    pub fn new(channel: ChannelSelection, fft: FftConfig) -> Self {
        Self {
            channel,
            fft,
            tilt_db_per_octave: 0.0,
            smooth: None,
            center_db: 0.0,
            gain_db: 0.0,
            peak: None,
            history: None,
        }
    }

    pub fn validated(mut self) -> Result<Self, RequestError> {
        FftConfig::new(self.fft.window_size, self.fft.hop_size, self.fft.floor_dbfs)
            .map_err(|_| RequestError::InvalidFft)?;
        if !self.tilt_db_per_octave.is_finite() {
            return Err(RequestError::InvalidTilt);
        }
        if !self.center_db.is_finite() {
            return Err(RequestError::InvalidCenter);
        }
        if !self.gain_db.is_finite() {
            return Err(RequestError::InvalidGain);
        }
        if let Some(smooth) = self.smooth {
            smooth.validate()?;
            self.smooth = Some(smooth.canonicalized());
        }
        if let Some(peak) = self.peak {
            peak.validate()?;
            self.peak = Some(peak.canonicalized());
        }
        if let Some(history) = self.history {
            history.validate()?;
        }
        self.tilt_db_per_octave = canonical_zero(self.tilt_db_per_octave);
        self.center_db = canonical_zero(self.center_db);
        self.gain_db = canonical_zero(self.gain_db);
        Ok(self)
    }

    pub fn is_stateful(self) -> bool {
        self.smooth.is_some() || self.peak.is_some() || self.history.is_some()
    }
}

impl Default for SpectrumRequest {
    fn default() -> Self {
        Self::new(ChannelSelection::MonoAverage, FftConfig::default())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestError {
    InvalidFft,
    InvalidTilt,
    InvalidSmoothTime,
    InvalidCenter,
    InvalidGain,
    InvalidPeakTime,
    InvalidHistoryDuration,
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFft => f.write_str("invalid FFT configuration"),
            Self::InvalidTilt => f.write_str("tilt must be finite"),
            Self::InvalidSmoothTime => {
                f.write_str("smooth attack/release must be finite and non-negative")
            }
            Self::InvalidCenter => f.write_str("center offset must be finite"),
            Self::InvalidGain => f.write_str("gain must be finite"),
            Self::InvalidPeakTime => f.write_str("peak hold/decay must be finite and non-negative"),
            Self::InvalidHistoryDuration => {
                f.write_str("history duration must be finite and positive")
            }
        }
    }
}

impl Error for RequestError {}

pub(crate) fn canonical_zero(value: f32) -> f32 {
    if value == 0.0 { 0.0 } else { value }
}

pub(crate) fn apply_tilt(
    bins: &[f32],
    bin_hz: f32,
    floor_dbfs: f32,
    tilt_db_per_octave: f32,
) -> Box<[f32]> {
    bins.iter()
        .copied()
        .enumerate()
        .map(|(index, value)| {
            let frequency = (index as f32 * bin_hz).max(bin_hz);
            let tilt = tilt_db_per_octave * (frequency / TILT_REFERENCE_HZ).log2();
            (value + tilt).max(floor_dbfs)
        })
        .collect()
}

pub(crate) fn apply_center_and_gain(
    bins: &mut [f32],
    floor_dbfs: f32,
    center_db: f32,
    gain_db: f32,
) {
    for value in bins {
        *value = (*value - center_db + gain_db).max(floor_dbfs);
    }
}

#[derive(Clone)]
pub(crate) struct SmoothState {
    values: Box<[f32]>,
    last_frame: Option<u64>,
}

impl SmoothState {
    pub(crate) fn new() -> Self {
        Self {
            values: Box::new([]),
            last_frame: None,
        }
    }

    pub(crate) fn process(
        &mut self,
        input: &[f32],
        processed_through_frame: u64,
        sample_rate_hz: u32,
        config: SmoothConfig,
    ) -> Box<[f32]> {
        if self.values.len() != input.len() || self.last_frame.is_none() {
            self.values = input.into();
            self.last_frame = Some(processed_through_frame);
            return self.values.clone();
        }
        let elapsed_frames = processed_through_frame.saturating_sub(self.last_frame.unwrap());
        let elapsed_seconds = elapsed_frames as f32 / sample_rate_hz as f32;
        for (current, target) in self.values.iter_mut().zip(input.iter().copied()) {
            let time_ms = if target >= *current {
                config.attack_ms
            } else {
                config.release_ms
            };
            let alpha = if time_ms == 0.0 {
                1.0
            } else {
                1.0 - (-elapsed_seconds / (time_ms / 1_000.0)).exp()
            };
            *current += (target - *current) * alpha;
        }
        self.last_frame = Some(processed_through_frame);
        self.values.clone()
    }

    pub(crate) fn reset(&mut self) {
        self.values = Box::new([]);
        self.last_frame = None;
    }
}

#[derive(Clone)]
pub(crate) struct PeakState {
    values: Box<[f32]>,
    hold_until: Box<[u64]>,
    last_frame: Option<u64>,
}

impl PeakState {
    pub(crate) fn new() -> Self {
        Self {
            values: Box::new([]),
            hold_until: Box::new([]),
            last_frame: None,
        }
    }

    pub(crate) fn process(
        &mut self,
        input: &[f32],
        processed_through_frame: u64,
        sample_rate_hz: u32,
        config: PeakConfig,
    ) -> Arc<[f32]> {
        if self.values.len() != input.len() || self.last_frame.is_none() {
            self.values = input.into();
            self.hold_until = vec![processed_through_frame; input.len()].into_boxed_slice();
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
        Arc::from(self.values.clone())
    }

    pub(crate) fn reset(&mut self) {
        self.values = Box::new([]);
        self.hold_until = Box::new([]);
        self.last_frame = None;
    }
}

#[derive(Clone)]
pub(crate) struct HistoryState {
    frames: VecDeque<SpectrumHistoryFrame>,
    gap_pending: bool,
}

impl HistoryState {
    pub(crate) fn new() -> Self {
        Self {
            frames: VecDeque::new(),
            gap_pending: false,
        }
    }

    pub(crate) fn push(
        &mut self,
        bins_dbfs: Arc<[f32]>,
        processed_through_frame: u64,
        sample_rate_hz: u32,
        config: HistoryConfig,
    ) -> Arc<[SpectrumHistoryFrame]> {
        self.frames.push_back(SpectrumHistoryFrame {
            processed_through_frame,
            discontinuity_before: std::mem::take(&mut self.gap_pending),
            bins_dbfs,
        });
        let capacity_frames = (config.duration_seconds * sample_rate_hz as f32).round() as u64;
        while self.frames.front().is_some_and(|frame| {
            processed_through_frame.saturating_sub(frame.processed_through_frame) > capacity_frames
        }) {
            self.frames.pop_front();
        }
        self.frames.iter().cloned().collect()
    }

    pub(crate) fn mark_gap(&mut self) {
        self.gap_pending = true;
    }

    pub(crate) fn clear(&mut self) {
        self.frames.clear();
        self.gap_pending = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negative_zero_parameters_are_canonicalized() {
        let request = SpectrumRequest {
            tilt_db_per_octave: -0.0,
            center_db: -0.0,
            gain_db: -0.0,
            ..SpectrumRequest::default()
        };
        let request = request.validated().unwrap();
        assert_eq!(request.tilt_db_per_octave.to_bits(), 0.0_f32.to_bits());
        assert_eq!(request.center_db.to_bits(), 0.0_f32.to_bits());
        assert_eq!(request.gain_db.to_bits(), 0.0_f32.to_bits());
    }

    #[test]
    fn non_finite_parameters_are_rejected() {
        let request = SpectrumRequest {
            gain_db: f32::NAN,
            ..SpectrumRequest::default()
        };
        assert_eq!(request.validated(), Err(RequestError::InvalidGain));
    }

    #[test]
    fn tilt_center_and_gain_use_declared_db_semantics() {
        let mut values = apply_tilt(&[-20.0, -20.0, -20.0], 1_000.0, -160.0, 6.0);
        apply_center_and_gain(&mut values, -160.0, 2.0, 5.0);
        assert!((values[1] - -17.0).abs() < 0.000_1);
        assert!((values[2] - -11.0).abs() < 0.000_1);
    }

    #[test]
    fn peak_decays_only_after_hold_expires_within_a_hop() {
        let config = PeakConfig::new(15.0, 20.0).unwrap();
        let mut peak = PeakState::new();
        assert_eq!(peak.process(&[0.0], 0, 1_000, config).as_ref(), &[0.0]);
        assert_eq!(peak.process(&[-60.0], 10, 1_000, config).as_ref(), &[0.0]);
        let after_expiry = peak.process(&[-60.0], 20, 1_000, config);
        assert!((after_expiry[0] - -0.1).abs() < 0.000_01);
    }
}
