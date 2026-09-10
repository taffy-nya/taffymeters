use std::{error::Error, fmt, sync::Arc};

use cpal::{
    FromSample, Sample, SampleFormat, SizedSample, Stream,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};

use super::{
    AudioDescriptor, CaptureEndpoint, ChannelLayout, Generation, SourceId, SourceState,
    SourceTelemetry,
};

pub(crate) struct OpenSystemInput {
    device: cpal::Device,
    stream_config: cpal::StreamConfig,
    sample_format: SampleFormat,
    descriptor: Arc<AudioDescriptor>,
}

pub(crate) struct SystemCapture {
    stream: Stream,
    telemetry: Arc<SourceTelemetry>,
}

#[derive(Debug)]
pub(crate) enum SystemInputError {
    NoOutputDevice,
    DefaultConfig(cpal::DefaultStreamConfigError),
    InvalidDescriptor(super::DescriptorError),
    UnsupportedSampleFormat(SampleFormat),
    BuildStream(cpal::BuildStreamError),
    PlayStream(cpal::PlayStreamError),
    PauseStream(cpal::PauseStreamError),
}

impl fmt::Display for SystemInputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoOutputDevice => f.write_str("no default output device is available"),
            Self::DefaultConfig(error) => write!(f, "cannot read default output format: {error}"),
            Self::InvalidDescriptor(error) => write!(f, "invalid output descriptor: {error}"),
            Self::UnsupportedSampleFormat(format) => {
                write!(f, "unsupported system sample format: {format}")
            }
            Self::BuildStream(error) => write!(f, "cannot build WASAPI loopback stream: {error}"),
            Self::PlayStream(error) => write!(f, "cannot start WASAPI loopback stream: {error}"),
            Self::PauseStream(error) => write!(f, "cannot pause WASAPI loopback stream: {error}"),
        }
    }
}

impl Error for SystemInputError {}

impl OpenSystemInput {
    pub(crate) fn open_default() -> Result<Self, SystemInputError> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or(SystemInputError::NoOutputDevice)?;
        let supported = device
            .default_output_config()
            .map_err(SystemInputError::DefaultConfig)?;
        let channel_count = supported.channels();
        let channel_layout = match channel_count {
            1 => ChannelLayout::Mono,
            2 => ChannelLayout::Stereo,
            _ => ChannelLayout::UnknownDiscrete,
        };
        let descriptor = AudioDescriptor::new(
            SourceId(1),
            Generation(1),
            supported.sample_rate(),
            channel_count,
            channel_layout,
        )
        .map_err(SystemInputError::InvalidDescriptor)?;
        let sample_format = supported.sample_format();
        let stream_config = supported.into();
        Ok(Self {
            device,
            stream_config,
            sample_format,
            descriptor: Arc::new(descriptor),
        })
    }

    pub(crate) fn descriptor(&self) -> &Arc<AudioDescriptor> {
        &self.descriptor
    }

    pub(crate) fn start(
        self,
        endpoint: CaptureEndpoint,
        telemetry: Arc<SourceTelemetry>,
    ) -> Result<SystemCapture, SystemInputError> {
        macro_rules! build {
            ($sample:ty) => {
                self.build_stream::<$sample>(endpoint, telemetry)
            };
        }
        match self.sample_format {
            SampleFormat::I8 => build!(i8),
            SampleFormat::I16 => build!(i16),
            SampleFormat::I24 => build!(cpal::I24),
            SampleFormat::I32 => build!(i32),
            SampleFormat::I64 => build!(i64),
            SampleFormat::U8 => build!(u8),
            SampleFormat::U16 => build!(u16),
            SampleFormat::U24 => build!(cpal::U24),
            SampleFormat::U32 => build!(u32),
            SampleFormat::U64 => build!(u64),
            SampleFormat::F32 => build!(f32),
            SampleFormat::F64 => build!(f64),
            other => Err(SystemInputError::UnsupportedSampleFormat(other)),
        }
    }

    fn build_stream<T>(
        self,
        mut endpoint: CaptureEndpoint,
        telemetry: Arc<SourceTelemetry>,
    ) -> Result<SystemCapture, SystemInputError>
    where
        T: Sample + SizedSample,
        f32: FromSample<T>,
    {
        let error_telemetry = Arc::clone(&telemetry);
        let stream = self
            .device
            .build_input_stream(
                &self.stream_config,
                move |data: &[T], _| endpoint.write(data),
                move |error| error_telemetry.device_lost(error.to_string()),
                None,
            )
            .map_err(SystemInputError::BuildStream)?;
        stream.play().map_err(SystemInputError::PlayStream)?;
        telemetry.set_state(SourceState::Running);
        Ok(SystemCapture { stream, telemetry })
    }
}

impl SystemCapture {
    pub(crate) fn pause(&self) -> Result<(), SystemInputError> {
        self.stream.pause().map_err(SystemInputError::PauseStream)?;
        self.telemetry.set_state(SourceState::Paused);
        Ok(())
    }

    pub(crate) fn resume(&self) -> Result<(), SystemInputError> {
        self.stream.play().map_err(SystemInputError::PlayStream)?;
        self.telemetry.set_state(SourceState::Running);
        Ok(())
    }
}

impl Drop for SystemCapture {
    fn drop(&mut self) {
        self.telemetry.set_state(SourceState::Stopped);
    }
}
