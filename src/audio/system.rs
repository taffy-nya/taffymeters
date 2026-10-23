use std::{error::Error, fmt, sync::Arc};

use cpal::{
    DeviceId, FromSample, Sample, SampleFormat, SizedSample, Stream,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};

use super::{
    AudioDescriptor, CaptureEndpoint, ChannelLayout, Generation, SourceId, SourceState,
    SourceTelemetry,
};

/// Playback endpoints are captured through WASAPI loopback; capture endpoints directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Endpoint {
    DefaultPlayback,
    /// `cpal::DeviceId` in its `host:id` text form.
    Playback(String),
    DefaultCapture,
    Capture(String),
}

impl Endpoint {
    pub(crate) fn is_playback(&self) -> bool {
        matches!(self, Self::DefaultPlayback | Self::Playback(_))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EndpointInfo {
    pub(crate) endpoint: Endpoint,
    pub(crate) name: String,
}

/// Fixed endpoints of both directions, each list led by its current default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Endpoints {
    pub(crate) playback: Vec<EndpointInfo>,
    pub(crate) capture: Vec<EndpointInfo>,
    pub(crate) default_playback: Option<String>,
    pub(crate) default_capture: Option<String>,
}

/// Queries the system; call on demand, not every frame.
pub(crate) fn list_endpoints() -> Endpoints {
    let host = cpal::default_host();
    let infos = |devices: Option<Vec<cpal::Device>>, fixed: fn(String) -> Endpoint| {
        devices
            .unwrap_or_default()
            .iter()
            .filter_map(|device| {
                Some(EndpointInfo {
                    endpoint: fixed(device.id().ok()?.to_string()),
                    name: device_name(device),
                })
            })
            .collect()
    };
    Endpoints {
        playback: infos(
            host.output_devices().ok().map(Iterator::collect),
            Endpoint::Playback,
        ),
        capture: infos(
            host.input_devices().ok().map(Iterator::collect),
            Endpoint::Capture,
        ),
        default_playback: host.default_output_device().as_ref().map(device_name),
        default_capture: host.default_input_device().as_ref().map(device_name),
    }
}

/// The friendly name ("Headphones (Device)") as Windows shows it, or the bare
/// description ("Headphones") when that is all there is.
fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|description| {
            description
                .extended()
                .first()
                .map_or_else(|| description.name().to_owned(), Clone::clone)
        })
        .unwrap_or_else(|_| "Unnamed device".to_owned())
}

pub(crate) struct OpenSystemInput {
    device: cpal::Device,
    stream_config: cpal::StreamConfig,
    sample_format: SampleFormat,
    descriptor: Arc<AudioDescriptor>,
    name: String,
    /// Captured through loopback rather than as a recording device.
    playback: bool,
}

pub(crate) struct SystemCapture {
    stream: Stream,
    telemetry: Arc<SourceTelemetry>,
}

#[derive(Debug)]
pub(crate) enum SystemInputError {
    NoDevice,
    UnknownDevice,
    DefaultConfig(cpal::DefaultStreamConfigError),
    InvalidDescriptor(super::DescriptorError),
    UnsupportedSampleFormat(SampleFormat),
    BuildStream(cpal::BuildStreamError),
    MicrophoneBlocked,
    PlayStream(cpal::PlayStreamError),
    PauseStream(cpal::PauseStreamError),
}

impl fmt::Display for SystemInputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDevice => f.write_str("no default device is available"),
            Self::UnknownDevice => f.write_str("the device is not connected"),
            Self::DefaultConfig(error) => write!(f, "cannot read the device format: {error}"),
            Self::InvalidDescriptor(error) => write!(f, "invalid device format: {error}"),
            Self::UnsupportedSampleFormat(format) => {
                write!(f, "unsupported system sample format: {format}")
            }
            Self::BuildStream(error) => write!(f, "cannot build the WASAPI stream: {error}"),
            Self::MicrophoneBlocked => f.write_str(
                "Windows blocks microphone access for desktop apps \
                 (Settings › Privacy & security › Microphone)",
            ),
            Self::PlayStream(error) => write!(f, "cannot start the WASAPI stream: {error}"),
            Self::PauseStream(error) => write!(f, "cannot pause the WASAPI stream: {error}"),
        }
    }
}

impl Error for SystemInputError {}

impl OpenSystemInput {
    pub(crate) fn open(
        endpoint: &Endpoint,
        generation: Generation,
    ) -> Result<Self, SystemInputError> {
        let host = cpal::default_host();
        let device = match endpoint {
            Endpoint::DefaultPlayback => host.default_output_device(),
            Endpoint::DefaultCapture => host.default_input_device(),
            Endpoint::Playback(id) | Endpoint::Capture(id) => {
                let parsed: DeviceId = id.parse().map_err(|_| SystemInputError::UnknownDevice)?;
                Some(
                    host.device_by_id(&parsed)
                        .ok_or(SystemInputError::UnknownDevice)?,
                )
            }
        }
        .ok_or(SystemInputError::NoDevice)?;
        let supported = if endpoint.is_playback() {
            device.default_output_config()
        } else {
            device.default_input_config()
        }
        .map_err(SystemInputError::DefaultConfig)?;
        let channel_count = supported.channels();
        let channel_layout = match channel_count {
            1 => ChannelLayout::Mono,
            2 => ChannelLayout::Stereo,
            _ => ChannelLayout::UnknownDiscrete,
        };
        let descriptor = AudioDescriptor::new(
            SourceId(1),
            generation,
            supported.sample_rate(),
            channel_count,
            channel_layout,
        )
        .map_err(SystemInputError::InvalidDescriptor)?;
        let sample_format = supported.sample_format();
        let stream_config = supported.into();
        Ok(Self {
            name: device_name(&device),
            playback: endpoint.is_playback(),
            device,
            stream_config,
            sample_format,
            descriptor: Arc::new(descriptor),
        })
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
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
        // WASAPI treats an input stream on an output device as loopback capture.
        let stream = self
            .device
            .build_input_stream(
                &self.stream_config,
                move |data: &[T], _| endpoint.write(data),
                move |error| error_telemetry.device_lost(error.to_string()),
                None,
            )
            .map_err(|error| {
                // Capture endpoints fail with E_ACCESSDENIED when the privacy settings
                // deny desktop apps the microphone; cpal only passes the text on.
                if !self.playback && error.to_string().contains("0x80070005") {
                    SystemInputError::MicrophoneBlocked
                } else {
                    SystemInputError::BuildStream(error)
                }
            })?;
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
