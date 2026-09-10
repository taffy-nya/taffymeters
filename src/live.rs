use std::{error::Error, fmt, sync::Arc};

use crate::{
    audio::{OpenSystemInput, SourceState, SourceTelemetry, SystemCapture, create_block_transport},
    dataflow::{ChannelSelection, ControlError, FftConfig, SpectrumController, SpectrumDataflow},
    snapshot::SpectrumReader,
};

#[derive(Clone, Copy, Debug)]
pub struct LiveSpectrumConfig {
    pub fft: FftConfig,
    pub initial_channel: ChannelSelection,
    pub transport_blocks: usize,
    pub max_block_frames: usize,
}

impl Default for LiveSpectrumConfig {
    fn default() -> Self {
        Self {
            fft: FftConfig::default(),
            initial_channel: ChannelSelection::MonoAverage,
            transport_blocks: 32,
            max_block_frames: 2048,
        }
    }
}

pub struct LiveSpectrum {
    capture: Option<SystemCapture>,
    dataflow: Option<SpectrumDataflow>,
    reader: SpectrumReader,
    controller: SpectrumController,
    telemetry: Arc<SourceTelemetry>,
    paused: bool,
}

#[derive(Debug)]
pub enum LiveSpectrumError {
    Input(String),
    Transport(String),
    Control(ControlError),
}

impl fmt::Display for LiveSpectrumError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(error) => write!(f, "system audio input failed: {error}"),
            Self::Transport(error) => write!(f, "audio transport failed: {error}"),
            Self::Control(error) => write!(f, "dataflow control failed: {error}"),
        }
    }
}

impl Error for LiveSpectrumError {}

impl LiveSpectrum {
    pub fn open_default(config: LiveSpectrumConfig) -> Result<Self, LiveSpectrumError> {
        let input = OpenSystemInput::open_default()
            .map_err(|error| LiveSpectrumError::Input(error.to_string()))?;
        let descriptor = Arc::clone(input.descriptor());
        let telemetry = Arc::new(SourceTelemetry::new());
        let (capture_endpoint, processing_endpoint) = create_block_transport(
            descriptor.channel_count as usize,
            config.transport_blocks,
            config.max_block_frames,
            Arc::clone(&telemetry),
        )
        .map_err(|error| LiveSpectrumError::Transport(error.to_string()))?;
        let dataflow = SpectrumDataflow::spawn(
            Arc::clone(&descriptor),
            processing_endpoint,
            Arc::clone(&telemetry),
            config.fft,
            config.initial_channel,
        );
        let controller = dataflow.controller();
        let reader = dataflow.reader();
        let mut capture_endpoint = capture_endpoint;
        capture_endpoint.set_worker(dataflow.worker_thread());
        let capture = input
            .start(capture_endpoint, Arc::clone(&telemetry))
            .map_err(|error| LiveSpectrumError::Input(error.to_string()))?;

        Ok(Self {
            capture: Some(capture),
            dataflow: Some(dataflow),
            reader,
            controller,
            telemetry,
            paused: false,
        })
    }

    pub fn reader(&self) -> SpectrumReader {
        self.reader.clone()
    }

    pub fn controller(&self) -> SpectrumController {
        self.controller.clone()
    }

    pub fn set_channel(&self, channel: ChannelSelection) -> Result<(), LiveSpectrumError> {
        self.controller
            .set_channel(channel)
            .map_err(LiveSpectrumError::Control)
    }

    pub fn toggle_pause(&mut self) -> Result<SourceState, LiveSpectrumError> {
        let capture = self
            .capture
            .as_ref()
            .expect("capture exists while runtime is live");
        if self.paused {
            let continuity = self.telemetry.advance_continuity();
            self.controller
                .resume_at(continuity)
                .map_err(LiveSpectrumError::Control)?;
            capture
                .resume()
                .map_err(|error| LiveSpectrumError::Input(error.to_string()))?;
            self.paused = false;
            Ok(SourceState::Running)
        } else {
            capture
                .pause()
                .map_err(|error| LiveSpectrumError::Input(error.to_string()))?;
            self.paused = true;
            Ok(SourceState::Paused)
        }
    }
}

impl Drop for LiveSpectrum {
    fn drop(&mut self) {
        self.capture.take();
        self.dataflow.take();
    }
}
