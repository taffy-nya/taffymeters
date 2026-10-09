use std::{error::Error, fmt, sync::Arc};

use crate::{
    audio::{
        CaptureEndpoint, OpenSystemInput, SourceReader, SourceState, SourceTelemetry,
        SystemCapture, create_block_transport,
    },
    dataflow::{ControlError, Dataflow, DataflowHandle},
};

#[cfg(any(test, feature = "automation"))]
use crate::audio::synthetic::{
    ManualInputDriver, ManualSource, RealtimeSource, SyntheticConfig, SyntheticError,
};

#[derive(Clone, Copy, Debug)]
pub struct LiveAudioConfig {
    pub transport_blocks: usize,
    pub max_block_frames: usize,
}

impl Default for LiveAudioConfig {
    fn default() -> Self {
        Self {
            transport_blocks: 32,
            max_block_frames: 2048,
        }
    }
}

enum InputDriver {
    System(SystemCapture),
    #[cfg(any(test, feature = "automation"))]
    Manual(ManualSource),
    #[cfg(any(test, feature = "automation"))]
    Realtime(RealtimeSource),
}

pub struct LiveAudio {
    input: Option<InputDriver>,
    dataflow: Option<Dataflow>,
    reader: SourceReader,
    handle: DataflowHandle,
    telemetry: Arc<SourceTelemetry>,
    paused: bool,
}

#[derive(Debug)]
pub enum LiveAudioError {
    Input(String),
    Synthetic(String),
    Transport(String),
    Control(ControlError),
    Stopped,
}

impl fmt::Display for LiveAudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(error) => write!(f, "system audio input failed: {error}"),
            Self::Synthetic(error) => write!(f, "synthetic audio input failed: {error}"),
            Self::Transport(error) => write!(f, "audio transport failed: {error}"),
            Self::Control(error) => write!(f, "dataflow control failed: {error}"),
            Self::Stopped => f.write_str("live audio source is stopped"),
        }
    }
}

impl Error for LiveAudioError {}

impl LiveAudio {
    pub fn open_default(config: LiveAudioConfig) -> Result<Self, LiveAudioError> {
        let system_input = OpenSystemInput::open_default()
            .map_err(|error| LiveAudioError::Input(error.to_string()))?;
        let descriptor = Arc::clone(system_input.descriptor());
        let telemetry = Arc::new(SourceTelemetry::new());
        let (endpoint, dataflow, handle) =
            Self::create_transport(config, Arc::clone(&descriptor), Arc::clone(&telemetry))?;
        let capture = system_input
            .start(endpoint, Arc::clone(&telemetry))
            .map_err(|error| LiveAudioError::Input(error.to_string()))?;

        Ok(Self::from_parts(
            InputDriver::System(capture),
            dataflow,
            handle,
            descriptor,
            telemetry,
        ))
    }

    #[cfg(any(test, feature = "automation"))]
    pub(crate) fn open_manual(
        config: LiveAudioConfig,
        synthetic: SyntheticConfig,
    ) -> Result<(Self, ManualInputDriver), LiveAudioError> {
        let sample_capacity = synthetic
            .validate(config.max_block_frames)
            .map_err(|error| LiveAudioError::Synthetic(error.to_string()))?;
        let descriptor = Arc::new(synthetic.descriptor.clone());
        let telemetry = Arc::new(SourceTelemetry::new());
        let (endpoint, dataflow, handle) =
            Self::create_transport(config, Arc::clone(&descriptor), Arc::clone(&telemetry))?;
        let (source, driver) = ManualSource::new(
            endpoint,
            synthetic,
            sample_capacity,
            handle.clone(),
            Arc::clone(&telemetry),
        );
        Ok((
            Self::from_parts(
                InputDriver::Manual(source),
                dataflow,
                handle,
                descriptor,
                telemetry,
            ),
            driver,
        ))
    }

    #[cfg(any(test, feature = "automation"))]
    pub(crate) fn open_realtime(
        config: LiveAudioConfig,
        synthetic: SyntheticConfig,
    ) -> Result<Self, LiveAudioError> {
        let sample_capacity = synthetic
            .validate(config.max_block_frames)
            .map_err(|error| LiveAudioError::Synthetic(error.to_string()))?;
        let descriptor = Arc::new(synthetic.descriptor.clone());
        let telemetry = Arc::new(SourceTelemetry::new());
        let (endpoint, dataflow, handle) =
            Self::create_transport(config, Arc::clone(&descriptor), Arc::clone(&telemetry))?;
        let source = RealtimeSource::start(
            endpoint,
            synthetic,
            sample_capacity,
            config.transport_blocks,
            Arc::clone(&telemetry),
        )
        .map_err(|error| LiveAudioError::Synthetic(error.to_string()))?;
        Ok(Self::from_parts(
            InputDriver::Realtime(source),
            dataflow,
            handle,
            descriptor,
            telemetry,
        ))
    }

    fn create_transport(
        config: LiveAudioConfig,
        descriptor: Arc<crate::audio::AudioDescriptor>,
        telemetry: Arc<SourceTelemetry>,
    ) -> Result<(CaptureEndpoint, Dataflow, DataflowHandle), LiveAudioError> {
        let (mut endpoint, processing_endpoint) = create_block_transport(
            usize::from(descriptor.channel_count),
            config.transport_blocks,
            config.max_block_frames,
            Arc::clone(&telemetry),
        )
        .map_err(|error| LiveAudioError::Transport(error.to_string()))?;
        let (dataflow, handle) =
            Dataflow::spawn(Arc::clone(&descriptor), processing_endpoint, telemetry);
        endpoint.set_worker(dataflow.worker_thread());
        Ok((endpoint, dataflow, handle))
    }

    fn from_parts(
        input: InputDriver,
        dataflow: Dataflow,
        handle: DataflowHandle,
        descriptor: Arc<crate::audio::AudioDescriptor>,
        telemetry: Arc<SourceTelemetry>,
    ) -> Self {
        Self {
            input: Some(input),
            dataflow: Some(dataflow),
            reader: SourceReader::new(descriptor, Arc::clone(&telemetry)),
            handle,
            telemetry,
            paused: false,
        }
    }

    pub fn reader(&self) -> SourceReader {
        self.reader.clone()
    }

    pub fn dataflow_handle(&self) -> DataflowHandle {
        self.handle.clone()
    }

    pub fn toggle_pause(&mut self) -> Result<SourceState, LiveAudioError> {
        self.set_paused(!self.paused)
    }

    pub(crate) fn set_paused(&mut self, paused: bool) -> Result<SourceState, LiveAudioError> {
        if self.paused == paused {
            return Ok(self.reader.source_status().state);
        }
        let input = self.input.as_ref().ok_or(LiveAudioError::Stopped)?;
        if paused {
            match input {
                InputDriver::System(capture) => capture
                    .pause()
                    .map_err(|error| LiveAudioError::Input(error.to_string()))?,
                #[cfg(any(test, feature = "automation"))]
                InputDriver::Manual(source) => {
                    source.pause().map_err(map_synthetic_error)?;
                }
                #[cfg(any(test, feature = "automation"))]
                InputDriver::Realtime(source) => {
                    source.pause().map_err(map_synthetic_error)?;
                }
            }
            self.paused = true;
        } else {
            match input {
                InputDriver::System(capture) => {
                    let continuity = self.telemetry.continuity().wrapping_add(1);
                    self.handle
                        .resume_at(continuity)
                        .map_err(LiveAudioError::Control)?;
                    self.telemetry.advance_continuity();
                    capture
                        .resume()
                        .map_err(|error| LiveAudioError::Input(error.to_string()))?;
                }
                #[cfg(any(test, feature = "automation"))]
                InputDriver::Manual(source) => {
                    source.resume(&self.handle).map_err(map_synthetic_error)?;
                }
                #[cfg(any(test, feature = "automation"))]
                InputDriver::Realtime(source) => {
                    source.resume(&self.handle).map_err(map_synthetic_error)?;
                }
            }
            self.paused = false;
        }
        Ok(self.reader.source_status().state)
    }
}

#[cfg(any(test, feature = "automation"))]
fn map_synthetic_error(error: SyntheticError) -> LiveAudioError {
    match error {
        SyntheticError::Control(error) => LiveAudioError::Control(error),
        other => LiveAudioError::Synthetic(other.to_string()),
    }
}

impl Drop for LiveAudio {
    fn drop(&mut self) {
        drop(self.input.take());
        drop(self.dataflow.take());
    }
}
