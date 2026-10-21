use std::{
    error::Error,
    fmt,
    sync::{
        Arc, Condvar, Mutex, Weak,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::{
    audio::{
        AudioDescriptor, CaptureEndpoint, ChannelLayout, Generation, SourceId, SourceState,
        SourceTelemetry,
    },
    dataflow::{ControlError, DataflowHandle},
};

static NEXT_SYNTHETIC_SOURCE_ID: AtomicU64 = AtomicU64::new(1 << 63);

#[derive(Clone, Debug)]
pub(crate) struct SyntheticConfig {
    pub(crate) descriptor: AudioDescriptor,
    pub(crate) block_frames: usize,
    pub(crate) signals: Vec<SignalSpec>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SignalSpec {
    Sine {
        frequency_hz: f64,
        amplitude: f32,
        phase_radians: f64,
    },
    Silence,
    Impulse {
        at_frame: u64,
        amplitude: f32,
    },
    Indexed {
        offset: f32,
    },
    /// Repeating exponential sine sweep.
    Sweep {
        start_hz: f64,
        end_hz: f64,
        period_seconds: f64,
        amplitude: f32,
    },
    /// Sine with a raised-cosine amplitude envelope at `rate_hz`.
    Pulse {
        frequency_hz: f64,
        rate_hz: f64,
        amplitude: f32,
    },
}

impl Default for SyntheticConfig {
    fn default() -> Self {
        let source_id = NEXT_SYNTHETIC_SOURCE_ID.fetch_add(1, Ordering::Relaxed);
        let descriptor = AudioDescriptor::new(
            SourceId(source_id),
            Generation(1),
            48_000,
            2,
            ChannelLayout::Stereo,
        )
        .expect("the default synthetic descriptor is valid");
        Self {
            descriptor,
            block_frames: 160,
            signals: vec![
                SignalSpec::Sine {
                    frequency_hz: 3_000.0,
                    amplitude: 0.08,
                    phase_radians: 0.0,
                },
                SignalSpec::Sine {
                    frequency_hz: 6_000.0,
                    amplitude: 0.08,
                    phase_radians: 0.0,
                },
            ],
        }
    }
}

impl SyntheticConfig {
    /// Distinct, continuously changing channels make stalled preview windows visible.
    pub(crate) fn sweep() -> Self {
        Self {
            signals: vec![
                SignalSpec::Sweep {
                    start_hz: 80.0,
                    end_hz: 12_000.0,
                    period_seconds: 6.0,
                    amplitude: 0.25,
                },
                SignalSpec::Pulse {
                    frequency_hz: 440.0,
                    rate_hz: 0.5,
                    amplitude: 0.3,
                },
            ],
            ..Self::default()
        }
    }

    pub(crate) fn validate(&self, max_block_frames: usize) -> Result<usize, SyntheticError> {
        let descriptor = &self.descriptor;
        AudioDescriptor::new(
            descriptor.source_id,
            descriptor.generation,
            descriptor.sample_rate_hz,
            descriptor.channel_count,
            descriptor.channel_layout,
        )
        .map_err(SyntheticError::InvalidDescriptor)?;

        if self.block_frames == 0 {
            return Err(SyntheticError::ZeroBlockFrames);
        }
        u64::try_from(self.block_frames).map_err(|_| SyntheticError::FramePositionOverflow)?;
        if self.block_frames > max_block_frames {
            return Err(SyntheticError::BlockFramesTooLarge {
                block_frames: self.block_frames,
                max_block_frames,
            });
        }
        let channels = usize::from(descriptor.channel_count);
        if self.signals.len() != channels {
            return Err(SyntheticError::SignalCount {
                expected: channels,
                actual: self.signals.len(),
            });
        }
        let capacity = self
            .block_frames
            .checked_mul(channels)
            .ok_or(SyntheticError::SampleCapacityOverflow)?;
        let nyquist_hz = f64::from(descriptor.sample_rate_hz) / 2.0;
        for (channel, signal) in self.signals.iter().copied().enumerate() {
            let valid = match signal {
                SignalSpec::Sine {
                    frequency_hz,
                    amplitude,
                    phase_radians,
                } => {
                    frequency_hz.is_finite()
                        && (0.0..nyquist_hz).contains(&frequency_hz)
                        && amplitude.is_finite()
                        && phase_radians.is_finite()
                }
                SignalSpec::Silence => true,
                SignalSpec::Impulse { amplitude, .. } => amplitude.is_finite(),
                SignalSpec::Indexed { offset } => offset.is_finite(),
                SignalSpec::Sweep {
                    start_hz,
                    end_hz,
                    period_seconds,
                    amplitude,
                } => {
                    start_hz > 0.0
                        && end_hz > 0.0
                        && start_hz < nyquist_hz
                        && end_hz < nyquist_hz
                        && period_seconds.is_finite()
                        && period_seconds > 0.0
                        && amplitude.is_finite()
                }
                SignalSpec::Pulse {
                    frequency_hz,
                    rate_hz,
                    amplitude,
                } => {
                    (0.0..nyquist_hz).contains(&frequency_hz)
                        && rate_hz.is_finite()
                        && rate_hz >= 0.0
                        && amplitude.is_finite()
                }
            };
            if !valid {
                return Err(SyntheticError::InvalidSignal { channel });
            }
        }
        Ok(capacity)
    }

    fn fill_frames(&self, start_frame: u64, frame_count: usize, output: &mut [f32]) {
        let channels = usize::from(self.descriptor.channel_count);
        for frame_offset in 0..frame_count {
            let frame = start_frame + frame_offset as u64;
            let sample_offset = frame_offset * channels;
            for (channel, signal) in self.signals.iter().copied().enumerate() {
                output[sample_offset + channel] =
                    signal.sample(frame, self.descriptor.sample_rate_hz);
            }
        }
    }
}

impl SignalSpec {
    fn sample(self, frame: u64, sample_rate_hz: u32) -> f32 {
        match self {
            Self::Sine {
                frequency_hz,
                amplitude,
                phase_radians,
            } => {
                let phase = std::f64::consts::TAU * frequency_hz * frame as f64
                    / f64::from(sample_rate_hz)
                    + phase_radians;
                (f64::from(amplitude) * phase.sin()) as f32
            }
            Self::Silence => 0.0,
            Self::Impulse {
                at_frame,
                amplitude,
            } => {
                if frame == at_frame {
                    amplitude
                } else {
                    0.0
                }
            }
            Self::Indexed { offset } => ((frame % 1024) as f32 - 512.0) / 1024.0 + offset,
            Self::Sweep {
                start_hz,
                end_hz,
                period_seconds,
                amplitude,
            } => {
                // Phase of f(t) = start * k^(t / T), k = end / start, from the period
                // start; computed from the absolute frame so block size never matters.
                let rate = f64::from(sample_rate_hz);
                let period_frames = (period_seconds * rate).round().max(1.0) as u64;
                let t = (frame % period_frames) as f64 / rate;
                let growth = (end_hz / start_hz).ln();
                let phase = if growth.abs() < 1e-12 {
                    std::f64::consts::TAU * start_hz * t
                } else {
                    std::f64::consts::TAU
                        * start_hz
                        * period_seconds
                        * ((growth * t / period_seconds).exp() - 1.0)
                        / growth
                };
                (f64::from(amplitude) * phase.sin()) as f32
            }
            Self::Pulse {
                frequency_hz,
                rate_hz,
                amplitude,
            } => {
                let t = frame as f64 / f64::from(sample_rate_hz);
                let envelope = 0.5 - 0.5 * (std::f64::consts::TAU * rate_hz * t).cos();
                (f64::from(amplitude) * envelope * (std::f64::consts::TAU * frequency_hz * t).sin())
                    as f32
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SyntheticError {
    InvalidDescriptor(crate::audio::DescriptorError),
    ZeroBlockFrames,
    BlockFramesTooLarge {
        block_frames: usize,
        max_block_frames: usize,
    },
    SignalCount {
        expected: usize,
        actual: usize,
    },
    InvalidSignal {
        channel: usize,
    },
    SampleCapacityOverflow,
    InvalidSampleCount {
        samples: usize,
        channels: usize,
    },
    NonFiniteSample {
        sample_index: usize,
    },
    FramePositionOverflow,
    Stopped,
    Paused,
    Timeout {
        target_frame: u64,
    },
    TransportLoss {
        start_frame: u64,
        end_frame: u64,
        dropped_blocks: u64,
        invalid_samples: u64,
    },
    Control(ControlError),
    LockPoisoned,
    ProducerStart(String),
    ProducerPanicked,
}

impl fmt::Display for SyntheticError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDescriptor(error) => write!(f, "invalid synthetic descriptor: {error}"),
            Self::ZeroBlockFrames => f.write_str("synthetic block_frames must be positive"),
            Self::BlockFramesTooLarge {
                block_frames,
                max_block_frames,
            } => write!(
                f,
                "synthetic block_frames {block_frames} exceeds transport maximum {max_block_frames}"
            ),
            Self::SignalCount { expected, actual } => write!(
                f,
                "synthetic signal count {actual} does not match channel count {expected}"
            ),
            Self::InvalidSignal { channel } => {
                write!(f, "synthetic signal for channel {channel} is invalid")
            }
            Self::SampleCapacityOverflow => f.write_str("synthetic sample capacity overflow"),
            Self::InvalidSampleCount { samples, channels } => write!(
                f,
                "interleaved sample count {samples} is not divisible by channel count {channels}"
            ),
            Self::NonFiniteSample { sample_index } => {
                write!(f, "interleaved sample {sample_index} is not finite")
            }
            Self::FramePositionOverflow => f.write_str("synthetic source frame position overflow"),
            Self::Stopped => f.write_str("synthetic source is stopped"),
            Self::Paused => f.write_str("synthetic source is paused"),
            Self::Timeout { target_frame } => {
                write!(f, "timed out waiting for source frame {target_frame}")
            }
            Self::TransportLoss {
                start_frame,
                end_frame,
                dropped_blocks,
                invalid_samples,
            } => write!(
                f,
                "synthetic transport lost input in [{start_frame}, {end_frame}): {dropped_blocks} dropped blocks, {invalid_samples} invalid samples"
            ),
            Self::Control(error) => write!(f, "dataflow control failed: {error}"),
            Self::LockPoisoned => f.write_str("synthetic source control lock is poisoned"),
            Self::ProducerStart(error) => {
                write!(f, "could not start synthetic realtime producer: {error}")
            }
            Self::ProducerPanicked => f.write_str("synthetic realtime producer panicked"),
        }
    }
}

impl Error for SyntheticError {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ManualStateKind {
    Running,
    Paused,
    Stopped,
}

struct ManualState {
    endpoint: Option<CaptureEndpoint>,
    synthetic: SyntheticConfig,
    scratch: Vec<f32>,
    next_frame: u64,
    state: ManualStateKind,
}

pub(crate) struct ManualSource {
    state: Arc<Mutex<ManualState>>,
    telemetry: Arc<SourceTelemetry>,
}

pub(crate) struct ManualInputDriver {
    state: Weak<Mutex<ManualState>>,
    handle: DataflowHandle,
    telemetry: Arc<SourceTelemetry>,
}

impl ManualSource {
    pub(crate) fn new(
        endpoint: CaptureEndpoint,
        synthetic: SyntheticConfig,
        sample_capacity: usize,
        handle: DataflowHandle,
        telemetry: Arc<SourceTelemetry>,
    ) -> (Self, ManualInputDriver) {
        let state = Arc::new(Mutex::new(ManualState {
            endpoint: Some(endpoint),
            synthetic,
            scratch: vec![0.0; sample_capacity],
            next_frame: 0,
            state: ManualStateKind::Running,
        }));
        telemetry.set_state(SourceState::Running);
        let driver = ManualInputDriver {
            state: Arc::downgrade(&state),
            handle,
            telemetry: Arc::clone(&telemetry),
        };
        (Self { state, telemetry }, driver)
    }

    pub(crate) fn pause(&self) -> Result<(), SyntheticError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| SyntheticError::LockPoisoned)?;
        match state.state {
            ManualStateKind::Running => {
                state.state = ManualStateKind::Paused;
                self.telemetry.set_state(SourceState::Paused);
                Ok(())
            }
            ManualStateKind::Paused => Ok(()),
            ManualStateKind::Stopped => Err(SyntheticError::Stopped),
        }
    }

    pub(crate) fn resume(&self, handle: &DataflowHandle) -> Result<(), SyntheticError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| SyntheticError::LockPoisoned)?;
        match state.state {
            ManualStateKind::Running => return Ok(()),
            ManualStateKind::Stopped => return Err(SyntheticError::Stopped),
            ManualStateKind::Paused => {}
        }
        let continuity = self.telemetry.continuity().wrapping_add(1);
        handle
            .resume_at(continuity)
            .map_err(SyntheticError::Control)?;
        self.telemetry.advance_continuity();
        state.state = ManualStateKind::Running;
        self.telemetry.set_state(SourceState::Running);
        Ok(())
    }

    pub(crate) fn stop(&mut self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.state = ManualStateKind::Stopped;
        drop(state.endpoint.take());
        self.telemetry.set_state(SourceState::Stopped);
    }
}

impl Drop for ManualSource {
    fn drop(&mut self) {
        self.stop();
    }
}

impl ManualInputDriver {
    pub(crate) fn advance_frames(
        &mut self,
        frame_count: u64,
        deadline: Instant,
    ) -> Result<u64, SyntheticError> {
        let (mut next_frame, total_end, block_frames) = {
            let state_owner = self.state.upgrade().ok_or(SyntheticError::Stopped)?;
            let state = state_owner
                .lock()
                .map_err(|_| SyntheticError::LockPoisoned)?;
            if state.state == ManualStateKind::Stopped {
                return Err(SyntheticError::Stopped);
            }
            let end = state
                .next_frame
                .checked_add(frame_count)
                .ok_or(SyntheticError::FramePositionOverflow)?;
            if frame_count != 0 && state.state == ManualStateKind::Paused {
                return Err(SyntheticError::Paused);
            }
            (state.next_frame, end, state.synthetic.block_frames)
        };
        if frame_count == 0 {
            return Ok(next_frame);
        }

        let block_frames =
            u64::try_from(block_frames).map_err(|_| SyntheticError::FramePositionOverflow)?;
        let start_status = self.telemetry.snapshot();
        let dropped_start = start_status.dropped_blocks;
        let invalid_start = start_status.invalid_samples;
        while next_frame < total_end {
            if Instant::now() >= deadline {
                return Err(SyntheticError::Timeout {
                    target_frame: total_end,
                });
            }
            let frames = (total_end - next_frame).min(block_frames);
            let end_frame = self.write_generated_block(
                next_frame,
                frames,
                dropped_start,
                invalid_start,
                deadline,
            )?;
            self.wait_processed(end_frame, deadline)?;
            next_frame = end_frame;
        }
        Ok(total_end)
    }

    pub(crate) fn push_pcm(
        &mut self,
        interleaved: &[f32],
        deadline: Instant,
    ) -> Result<u64, SyntheticError> {
        let (channels, block_frames, current_frame, total_end) = {
            let state_owner = self.state.upgrade().ok_or(SyntheticError::Stopped)?;
            let state = state_owner
                .lock()
                .map_err(|_| SyntheticError::LockPoisoned)?;
            if state.state == ManualStateKind::Stopped {
                return Err(SyntheticError::Stopped);
            }
            let channels = usize::from(state.synthetic.descriptor.channel_count);
            if !interleaved.len().is_multiple_of(channels) {
                return Err(SyntheticError::InvalidSampleCount {
                    samples: interleaved.len(),
                    channels,
                });
            }
            if let Some(sample_index) = interleaved.iter().position(|sample| !sample.is_finite()) {
                return Err(SyntheticError::NonFiniteSample { sample_index });
            }
            let frames = interleaved.len() / channels;
            let frames_u64 =
                u64::try_from(frames).map_err(|_| SyntheticError::FramePositionOverflow)?;
            let total_end = state
                .next_frame
                .checked_add(frames_u64)
                .ok_or(SyntheticError::FramePositionOverflow)?;
            if frames != 0 && state.state == ManualStateKind::Paused {
                return Err(SyntheticError::Paused);
            }
            (
                channels,
                state.synthetic.block_frames,
                state.next_frame,
                total_end,
            )
        };
        if interleaved.is_empty() {
            return Ok(current_frame);
        }

        let start_status = self.telemetry.snapshot();
        let dropped_start = start_status.dropped_blocks;
        let invalid_start = start_status.invalid_samples;
        let total_frames = interleaved.len() / channels;
        let mut frame_offset = 0_usize;
        while frame_offset < total_frames {
            if Instant::now() >= deadline {
                return Err(SyntheticError::Timeout {
                    target_frame: total_end,
                });
            }
            let frames = (total_frames - frame_offset).min(block_frames);
            let sample_offset = frame_offset
                .checked_mul(channels)
                .ok_or(SyntheticError::SampleCapacityOverflow)?;
            let sample_count = frames
                .checked_mul(channels)
                .ok_or(SyntheticError::SampleCapacityOverflow)?;
            let start_frame = current_frame
                .checked_add(
                    u64::try_from(frame_offset)
                        .map_err(|_| SyntheticError::FramePositionOverflow)?,
                )
                .ok_or(SyntheticError::FramePositionOverflow)?;
            let sample_end = sample_offset
                .checked_add(sample_count)
                .ok_or(SyntheticError::SampleCapacityOverflow)?;
            let end_frame = self.write_pcm_block(
                start_frame,
                frames,
                &interleaved[sample_offset..sample_end],
                dropped_start,
                invalid_start,
                deadline,
            )?;
            self.wait_processed(end_frame, deadline)?;
            frame_offset += frames;
        }
        Ok(total_end)
    }

    fn write_generated_block(
        &self,
        expected_start: u64,
        frames: u64,
        dropped_start: u64,
        invalid_start: u64,
        deadline: Instant,
    ) -> Result<u64, SyntheticError> {
        let state_owner = self.state.upgrade().ok_or(SyntheticError::Stopped)?;
        let mut state = state_owner
            .lock()
            .map_err(|_| SyntheticError::LockPoisoned)?;
        if state.state == ManualStateKind::Stopped || state.endpoint.is_none() {
            return Err(SyntheticError::Stopped);
        }
        if state.state == ManualStateKind::Paused {
            return Err(SyntheticError::Paused);
        }
        let frame_count =
            usize::try_from(frames).map_err(|_| SyntheticError::FramePositionOverflow)?;
        let end_frame = expected_start
            .checked_add(frames)
            .ok_or(SyntheticError::FramePositionOverflow)?;
        if Instant::now() >= deadline {
            return Err(SyntheticError::Timeout {
                target_frame: end_frame,
            });
        }
        if state.next_frame != expected_start {
            return Err(SyntheticError::Stopped);
        }
        let channels = usize::from(state.synthetic.descriptor.channel_count);
        let sample_count = frame_count
            .checked_mul(channels)
            .ok_or(SyntheticError::SampleCapacityOverflow)?;
        let ManualState {
            endpoint,
            synthetic,
            scratch,
            next_frame,
            ..
        } = &mut *state;
        synthetic.fill_frames(expected_start, frame_count, &mut scratch[..sample_count]);
        endpoint
            .as_mut()
            .expect("manual endpoint is present while running")
            .write(&scratch[..sample_count]);
        *next_frame = end_frame;
        drop(state);
        self.check_transport(dropped_start, invalid_start, expected_start, end_frame)?;
        Ok(end_frame)
    }

    fn write_pcm_block(
        &self,
        expected_start: u64,
        frames: usize,
        samples: &[f32],
        dropped_start: u64,
        invalid_start: u64,
        deadline: Instant,
    ) -> Result<u64, SyntheticError> {
        let state_owner = self.state.upgrade().ok_or(SyntheticError::Stopped)?;
        let mut state = state_owner
            .lock()
            .map_err(|_| SyntheticError::LockPoisoned)?;
        if state.state == ManualStateKind::Stopped || state.endpoint.is_none() {
            return Err(SyntheticError::Stopped);
        }
        if state.state == ManualStateKind::Paused {
            return Err(SyntheticError::Paused);
        }
        let frames_u64 =
            u64::try_from(frames).map_err(|_| SyntheticError::FramePositionOverflow)?;
        let end_frame = expected_start
            .checked_add(frames_u64)
            .ok_or(SyntheticError::FramePositionOverflow)?;
        if Instant::now() >= deadline {
            return Err(SyntheticError::Timeout {
                target_frame: end_frame,
            });
        }
        if state.next_frame != expected_start {
            return Err(SyntheticError::Stopped);
        }
        let ManualState {
            endpoint,
            next_frame,
            ..
        } = &mut *state;
        endpoint
            .as_mut()
            .expect("manual endpoint is present while running")
            .write(samples);
        *next_frame = end_frame;
        drop(state);
        self.check_transport(dropped_start, invalid_start, expected_start, end_frame)?;
        Ok(end_frame)
    }

    fn check_transport(
        &self,
        dropped_start: u64,
        invalid_start: u64,
        start_frame: u64,
        end_frame: u64,
    ) -> Result<(), SyntheticError> {
        let status = self.telemetry.snapshot();
        if status.dropped_blocks != dropped_start || status.invalid_samples != invalid_start {
            return Err(SyntheticError::TransportLoss {
                start_frame,
                end_frame,
                dropped_blocks: status.dropped_blocks.saturating_sub(dropped_start),
                invalid_samples: status.invalid_samples.saturating_sub(invalid_start),
            });
        }
        Ok(())
    }

    fn wait_processed(&self, target_frame: u64, deadline: Instant) -> Result<(), SyntheticError> {
        // No worker unpark arrives here; yield before parking to avoid Windows timer-tick delays
        // (up to ~15.6 ms) for blocks that normally drain in microseconds.
        const YIELD_ROUNDS: u32 = 1024;
        let mut rounds = 0_u32;
        loop {
            self.ensure_running()?;
            let frontier = self
                .handle
                .processed_through_frame()
                .map_err(SyntheticError::Control)?;
            if frontier >= target_frame {
                self.ensure_running()?;
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(SyntheticError::Timeout { target_frame });
            }
            if rounds < YIELD_ROUNDS {
                rounds += 1;
                thread::yield_now();
            } else {
                thread::park_timeout(Duration::from_millis(1));
            }
        }
    }

    fn ensure_running(&self) -> Result<(), SyntheticError> {
        let state = self.state.upgrade().ok_or(SyntheticError::Stopped)?;
        let state = state.lock().map_err(|_| SyntheticError::LockPoisoned)?;
        match state.state {
            ManualStateKind::Running => Ok(()),
            ManualStateKind::Paused => Err(SyntheticError::Paused),
            ManualStateKind::Stopped => Err(SyntheticError::Stopped),
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum RealtimeState {
    Running,
    Paused,
    Stopped,
}

struct RunControl {
    state: RealtimeState,
    anchor: Instant,
    anchor_frame: u64,
    reanchor_pending: bool,
}

struct RealtimeControl {
    run: Mutex<RunControl>,
    wake: Condvar,
    /// Controllers waiting for `run`; nonzero forces a lock yield between catch-up blocks.
    waiting: AtomicUsize,
}

pub(crate) struct RealtimeSource {
    control: Arc<RealtimeControl>,
    worker: Option<JoinHandle<()>>,
    telemetry: Arc<SourceTelemetry>,
    #[cfg(test)]
    producer_frame: Arc<AtomicU64>,
}

impl RealtimeSource {
    /// `max_lag_blocks` bounds wall-time lag before re-anchoring instead of back-filling.
    /// Use transport capacity to avoid replaying long stalls into the bounded queue.
    pub(crate) fn start(
        endpoint: CaptureEndpoint,
        synthetic: SyntheticConfig,
        sample_capacity: usize,
        max_lag_blocks: usize,
        telemetry: Arc<SourceTelemetry>,
    ) -> Result<Self, SyntheticError> {
        let control = Arc::new(RealtimeControl {
            run: Mutex::new(RunControl {
                state: RealtimeState::Running,
                anchor: Instant::now(),
                anchor_frame: 0,
                reanchor_pending: false,
            }),
            wake: Condvar::new(),
            waiting: AtomicUsize::new(0),
        });
        let thread_control = Arc::clone(&control);
        let thread_telemetry = Arc::clone(&telemetry);
        let scratch = vec![0.0; sample_capacity];
        #[cfg(test)]
        let producer_frame = Arc::new(AtomicU64::new(0));
        telemetry.set_state(SourceState::Running);

        let producer = RealtimeProducer {
            endpoint,
            synthetic,
            scratch,
            max_lag_blocks,
            control: thread_control,
            telemetry: thread_telemetry,
            #[cfg(test)]
            producer_frame: Arc::clone(&producer_frame),
        };
        let worker = thread::Builder::new()
            .name("taffymeters-synthetic-input".to_owned())
            .spawn(move || realtime_worker(producer));
        let worker = worker.map_err(|error| {
            telemetry.set_state(SourceState::Stopped);
            SyntheticError::ProducerStart(error.to_string())
        })?;
        Ok(Self {
            control,
            worker: Some(worker),
            telemetry,
            #[cfg(test)]
            producer_frame,
        })
    }

    fn with_control<R>(&self, change: impl FnOnce(&mut RunControl) -> R) -> R {
        let control = &*self.control;
        control.waiting.fetch_add(1, Ordering::AcqRel);
        let result = match control.run.lock() {
            Ok(mut run) => change(&mut run),
            Err(poisoned) => change(&mut poisoned.into_inner()),
        };
        control.waiting.fetch_sub(1, Ordering::AcqRel);
        control.wake.notify_all();
        result
    }

    pub(crate) fn pause(&self) -> Result<(), SyntheticError> {
        self.with_control(|control| match control.state {
            RealtimeState::Running => {
                control.state = RealtimeState::Paused;
                self.telemetry.set_state(SourceState::Paused);
                Ok(())
            }
            RealtimeState::Paused => Ok(()),
            RealtimeState::Stopped => Err(SyntheticError::Stopped),
        })
    }

    pub(crate) fn resume(&self, handle: &DataflowHandle) -> Result<(), SyntheticError> {
        self.with_control(|control| {
            match control.state {
                RealtimeState::Running => return Ok(()),
                RealtimeState::Stopped => return Err(SyntheticError::Stopped),
                RealtimeState::Paused => {}
            }
            let continuity = self.telemetry.continuity().wrapping_add(1);
            handle
                .resume_at(continuity)
                .map_err(SyntheticError::Control)?;
            self.telemetry.advance_continuity();
            control.anchor = Instant::now();
            control.reanchor_pending = true;
            control.state = RealtimeState::Running;
            self.telemetry.set_state(SourceState::Running);
            Ok(())
        })
    }

    #[cfg(test)]
    fn next_frame(&self) -> u64 {
        self.producer_frame.load(Ordering::Acquire)
    }

    pub(crate) fn stop(&mut self) {
        self.with_control(|control| control.state = RealtimeState::Stopped);
        self.telemetry.set_state(SourceState::Stopped);
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            self.telemetry
                .device_lost(SyntheticError::ProducerPanicked.to_string());
        }
    }
}

impl Drop for RealtimeSource {
    fn drop(&mut self) {
        self.stop();
    }
}

struct RealtimeProducer {
    endpoint: CaptureEndpoint,
    synthetic: SyntheticConfig,
    scratch: Vec<f32>,
    max_lag_blocks: usize,
    control: Arc<RealtimeControl>,
    telemetry: Arc<SourceTelemetry>,
    #[cfg(test)]
    producer_frame: Arc<AtomicU64>,
}

fn realtime_worker(producer: RealtimeProducer) {
    let RealtimeProducer {
        mut endpoint,
        synthetic,
        mut scratch,
        max_lag_blocks,
        control,
        telemetry,
        #[cfg(test)]
        producer_frame,
    } = producer;
    // Fallback lock-yield interval; a controller's notification normally wakes us sooner.
    const CONTROL_YIELD: Duration = Duration::from_millis(1);
    let sample_rate_hz = synthetic.descriptor.sample_rate_hz;
    let block_frames = synthetic.block_frames;
    let block_frames_u64 =
        u64::try_from(block_frames).expect("synthetic block size is validated before start");
    let channels = usize::from(synthetic.descriptor.channel_count);
    let sample_count = block_frames
        .checked_mul(channels)
        .expect("synthetic block capacity is validated before start");
    let max_lag = frame_duration(
        block_frames_u64.saturating_mul(u64::try_from(max_lag_blocks.max(1)).unwrap_or(u64::MAX)),
        sample_rate_hz,
    );
    let mut next_frame = 0_u64;
    let Ok(mut state) = control.run.lock() else {
        telemetry.device_lost(SyntheticError::LockPoisoned.to_string());
        return;
    };
    loop {
        let wait = match state.state {
            RealtimeState::Stopped => return,
            RealtimeState::Paused => None,
            RealtimeState::Running if control.waiting.load(Ordering::Acquire) != 0 => {
                Some(CONTROL_YIELD)
            }
            RealtimeState::Running => {
                if state.reanchor_pending {
                    state.anchor_frame = next_frame;
                    state.reanchor_pending = false;
                }
                let Some(end_frame) = next_frame.checked_add(block_frames_u64) else {
                    state.state = RealtimeState::Stopped;
                    telemetry.device_lost(SyntheticError::FramePositionOverflow.to_string());
                    return;
                };
                let frames_from_anchor = end_frame.saturating_sub(state.anchor_frame);
                let now = Instant::now();
                let deadline = state
                    .anchor
                    .checked_add(frame_duration(frames_from_anchor, sample_rate_hz))
                    .unwrap_or(now);
                if now < deadline {
                    Some(deadline - now)
                } else {
                    if now - deadline > max_lag {
                        // Avoid flooding the bounded queue; keep source frame numbering contiguous.
                        state.anchor = now;
                        state.anchor_frame = next_frame;
                    }
                    synthetic.fill_frames(next_frame, block_frames, &mut scratch[..sample_count]);
                    endpoint.write(&scratch[..sample_count]);
                    next_frame = end_frame;
                    #[cfg(test)]
                    producer_frame.store(next_frame, Ordering::Release);
                    // The lock covers each bounded block write, so a returned pause
                    // means no further block is produced.
                    continue;
                }
            }
        };
        let woke = match wait {
            Some(timeout) => control
                .wake
                .wait_timeout(state, timeout)
                .map(|(state, _)| state)
                .map_err(drop),
            None => control.wake.wait(state).map_err(drop),
        };
        state = match woke {
            Ok(state) => state,
            Err(_) => {
                telemetry.device_lost(SyntheticError::LockPoisoned.to_string());
                return;
            }
        };
    }
}

fn frame_duration(frame_count: u64, sample_rate_hz: u32) -> Duration {
    let nanos = u128::from(frame_count) * 1_000_000_000_u128 / u128::from(sample_rate_hz);
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::create_block_transport;
    use crate::dataflow::Dataflow;

    fn wait_frontier(handle: &DataflowHandle, frame: u64, deadline: Instant) -> u64 {
        loop {
            let frontier = handle
                .processed_through_frame()
                .expect("live dataflow should expose its drain frontier");
            if frontier >= frame {
                return frontier;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for processed frame {frame}; frontier is {frontier}"
            );
            thread::yield_now();
        }
    }

    #[test]
    fn realtime_source_reanchors_after_suspension_instead_of_back_filling() {
        let synthetic = SyntheticConfig {
            block_frames: 48,
            ..SyntheticConfig::default()
        };
        let sample_capacity = synthetic.validate(48).unwrap();
        let descriptor = Arc::new(synthetic.descriptor.clone());
        let telemetry = Arc::new(SourceTelemetry::new());
        let (mut endpoint, processing) =
            create_block_transport(2, 8, 48, Arc::clone(&telemetry)).unwrap();
        let (dataflow, handle) =
            Dataflow::spawn(Arc::clone(&descriptor), processing, Arc::clone(&telemetry));
        endpoint.set_worker(dataflow.worker_thread());
        let mut source = RealtimeSource::start(
            endpoint,
            synthetic,
            sample_capacity,
            8,
            Arc::clone(&telemetry),
        )
        .unwrap();
        wait_frontier(&handle, 48, Instant::now() + Duration::from_secs(5));

        // Ten minutes far exceeds queue capacity, exercising the re-anchor path.
        let suspended = Duration::from_secs(600);
        source.with_control(|control| {
            control.anchor = Instant::now()
                .checked_sub(suspended)
                .expect("monotonic clock has 10 minutes of history");
            control.anchor_frame = 0;
        });
        let resumed_at = source.next_frame();
        let observe_until = Instant::now() + Duration::from_millis(50);
        while Instant::now() < observe_until {
            thread::yield_now();
        }
        let started = Instant::now();
        source.pause().unwrap();
        let pause_latency = started.elapsed();
        let produced = source.next_frame() - resumed_at;

        assert!(
            produced < 48_000,
            "producer back-filled {produced} frames after suspension"
        );
        assert_eq!(
            telemetry.snapshot().dropped_blocks,
            0,
            "catch-up burst overflowed the transport"
        );
        assert!(
            pause_latency < Duration::from_millis(100),
            "pause waited {pause_latency:?} behind the producer"
        );
        source.stop();
        drop(dataflow);
    }

    #[test]
    fn realtime_source_pauses_resumes_and_stops_its_real_producer() {
        let synthetic = SyntheticConfig {
            block_frames: 48,
            ..SyntheticConfig::default()
        };
        let sample_capacity = synthetic.validate(48).unwrap();
        let descriptor = Arc::new(synthetic.descriptor.clone());
        let telemetry = Arc::new(SourceTelemetry::new());
        let (mut endpoint, processing) =
            create_block_transport(2, 64, 48, Arc::clone(&telemetry)).unwrap();
        let (dataflow, handle) =
            Dataflow::spawn(Arc::clone(&descriptor), processing, Arc::clone(&telemetry));
        endpoint.set_worker(dataflow.worker_thread());

        let mut source = RealtimeSource::start(
            endpoint,
            synthetic,
            sample_capacity,
            64,
            Arc::clone(&telemetry),
        )
        .unwrap();
        assert_eq!(telemetry.snapshot().state, SourceState::Running);
        let first_frontier = wait_frontier(&handle, 48, Instant::now() + Duration::from_secs(5));

        source.pause().unwrap();
        assert_eq!(telemetry.snapshot().state, SourceState::Paused);
        let paused_frame = source.next_frame();
        assert!(paused_frame >= first_frontier);
        wait_frontier(
            &handle,
            paused_frame,
            Instant::now() + Duration::from_secs(5),
        );
        let quiet_until = Instant::now()
            .checked_add(frame_duration(48 * 3, 48_000))
            .expect("short realtime confirmation interval fits");
        let quiet_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert_eq!(
                source.next_frame(),
                paused_frame,
                "realtime producer advanced while paused"
            );
            if Instant::now() >= quiet_until {
                break;
            }
            assert!(
                Instant::now() < quiet_deadline,
                "timed out confirming realtime producer pause"
            );
            thread::yield_now();
        }

        assert_eq!(source.next_frame(), paused_frame);

        let previous_continuity = telemetry.continuity();
        source.resume(&handle).unwrap();
        assert_eq!(telemetry.continuity(), previous_continuity + 1);
        assert_eq!(telemetry.snapshot().state, SourceState::Running);
        wait_frontier(
            &handle,
            paused_frame + 48,
            Instant::now() + Duration::from_secs(5),
        );

        source.stop();
        assert_eq!(telemetry.snapshot().state, SourceState::Stopped);
        drop(dataflow);
        assert_eq!(handle.processed_through_frame(), Err(ControlError::Stopped));
    }

    #[test]
    fn sweep_preview_signal_rises_in_pitch_and_pulses_in_level() {
        let config = SyntheticConfig::sweep();
        config.validate(2048).unwrap();
        let rate = config.descriptor.sample_rate_hz;
        let crossings = |signal: SignalSpec, start: u64| {
            (start..start + 4800)
                .filter(|&frame| {
                    (signal.sample(frame, rate) >= 0.0) != (signal.sample(frame + 1, rate) >= 0.0)
                })
                .count()
        };
        let sweep = config.signals[0];
        // 0.1 s windows near the start and the end of the 6 s period.
        assert!(crossings(sweep, 0) * 20 < crossings(sweep, 5 * 48_000 + 24_000));
        let peak = |signal: SignalSpec, start: u64| {
            (start..start + 480)
                .map(|frame| signal.sample(frame, rate).abs())
                .fold(0.0_f32, f32::max)
        };
        let pulse = config.signals[1];
        assert!(peak(pulse, 0) < 0.01);
        assert!(peak(pulse, 48_000) > 0.29);
    }
}
