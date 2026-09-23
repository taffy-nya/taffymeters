use std::{error::Error, fmt, sync::Arc};

use crate::{
    audio::{
        OpenSystemInput, SourceReader, SourceState, SourceTelemetry, SystemCapture,
        create_block_transport,
    },
    dataflow::{ControlError, Dataflow, DataflowHandle},
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

pub struct LiveAudio {
    capture: Option<SystemCapture>,
    dataflow: Option<Dataflow>,
    reader: SourceReader,
    handle: DataflowHandle,
    telemetry: Arc<SourceTelemetry>,
    paused: bool,
}

#[derive(Debug)]
pub enum LiveAudioError {
    Input(String),
    Transport(String),
    Control(ControlError),
}

impl fmt::Display for LiveAudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(error) => write!(f, "system audio input failed: {error}"),
            Self::Transport(error) => write!(f, "audio transport failed: {error}"),
            Self::Control(error) => write!(f, "dataflow control failed: {error}"),
        }
    }
}

impl Error for LiveAudioError {}

impl LiveAudio {
    pub fn open_default(config: LiveAudioConfig) -> Result<Self, LiveAudioError> {
        let input = OpenSystemInput::open_default()
            .map_err(|error| LiveAudioError::Input(error.to_string()))?;
        let descriptor = Arc::clone(input.descriptor());
        let telemetry = Arc::new(SourceTelemetry::new());
        let (mut capture_endpoint, processing_endpoint) = create_block_transport(
            descriptor.channel_count as usize,
            config.transport_blocks,
            config.max_block_frames,
            Arc::clone(&telemetry),
        )
        .map_err(|error| LiveAudioError::Transport(error.to_string()))?;
        let (dataflow, handle) = Dataflow::spawn(
            Arc::clone(&descriptor),
            processing_endpoint,
            Arc::clone(&telemetry),
        );
        capture_endpoint.set_worker(dataflow.worker_thread());
        let capture = input
            .start(capture_endpoint, Arc::clone(&telemetry))
            .map_err(|error| LiveAudioError::Input(error.to_string()))?;

        Ok(Self {
            capture: Some(capture),
            dataflow: Some(dataflow),
            reader: SourceReader::new(descriptor, Arc::clone(&telemetry)),
            handle,
            telemetry,
            paused: false,
        })
    }

    pub fn reader(&self) -> SourceReader {
        self.reader.clone()
    }

    pub fn dataflow_handle(&self) -> DataflowHandle {
        self.handle.clone()
    }

    pub fn toggle_pause(&mut self) -> Result<SourceState, LiveAudioError> {
        let capture = self
            .capture
            .as_ref()
            .expect("capture exists while live audio is running");
        if self.paused {
            let continuity = self.telemetry.continuity().wrapping_add(1);
            self.handle
                .resume_at(continuity)
                .map_err(LiveAudioError::Control)?;
            self.telemetry.advance_continuity();
            capture
                .resume()
                .map_err(|error| LiveAudioError::Input(error.to_string()))?;
            self.paused = false;
            Ok(SourceState::Running)
        } else {
            capture
                .pause()
                .map_err(|error| LiveAudioError::Input(error.to_string()))?;
            self.paused = true;
            Ok(SourceState::Paused)
        }
    }
}

impl Drop for LiveAudio {
    fn drop(&mut self) {
        self.capture.take();
        self.dataflow.take();
    }
}
