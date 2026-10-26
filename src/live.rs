use std::{error::Error, fmt, io, sync::Arc};

use crate::{
    audio::{
        CaptureEndpoint, Generation, SourceReader, SourceState, SourceTelemetry,
        create_block_transport,
        inputs::{Endpoint, OpenSystemInput, SystemCapture},
    },
    dataflow::{ControlError, Dataflow, DataflowHandle},
    persistence::{
        CAPTURE_INPUT, DEFAULT_CAPTURE_INPUT, DEFAULT_PLAYBACK_INPUT, DEVICE_OPTION, InputConfig,
        PLAYBACK_INPUT,
    },
};

#[cfg(feature = "automation")]
use crate::audio::inputs::synthetic::{
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
    #[cfg(feature = "automation")]
    Manual(ManualSource),
    #[cfg(feature = "automation")]
    Realtime(RealtimeSource),
}

pub struct LiveAudio {
    input: Option<InputDriver>,
    dataflow: Option<Dataflow>,
    reader: SourceReader,
    handle: DataflowHandle,
    telemetry: Arc<SourceTelemetry>,
    paused: bool,
    name: String,
}

#[derive(Debug)]
pub enum LiveAudioError {
    Input(String),
    UnsupportedInput(String),
    Synthetic(String),
    Transport(String),
    Worker(io::Error),
    Control(ControlError),
    Stopped,
}

impl fmt::Display for LiveAudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Input(error) => write!(f, "system audio input failed: {error}"),
            Self::UnsupportedInput(kind) => write!(f, "input {kind:?} is not supported"),
            Self::Synthetic(error) => write!(f, "synthetic audio input failed: {error}"),
            Self::Transport(error) => write!(f, "audio transport failed: {error}"),
            Self::Worker(error) => write!(f, "couldn't start the analysis thread: {error}"),
            Self::Control(error) => error.fmt(f),
            Self::Stopped => f.write_str("live audio source is stopped"),
        }
    }
}

impl Error for LiveAudioError {}

#[cfg(feature = "automation")]
const SYNTHETIC_NAME: &str = "Synthetic";

/// `None` for input kinds this build cannot open.
pub(crate) fn endpoint_of(input: &InputConfig) -> Option<Endpoint> {
    let device = || input.option(DEVICE_OPTION).map(str::to_owned);
    match input.kind.as_str() {
        DEFAULT_PLAYBACK_INPUT => Some(Endpoint::DefaultPlayback),
        PLAYBACK_INPUT => device().map(Endpoint::Playback),
        DEFAULT_CAPTURE_INPUT => Some(Endpoint::DefaultCapture),
        CAPTURE_INPUT => device().map(Endpoint::Capture),
        _ => None,
    }
}

pub(crate) fn open_system(
    input: &InputConfig,
    generation: Generation,
) -> Result<LiveAudio, LiveAudioError> {
    LiveAudio::open(LiveAudioConfig::default(), input, generation)
}

/// Opens the chosen input, or the default playback device when that fails. The error of
/// a supported input that failed comes back for the workspace to report; unsupported
/// kinds it reports itself.
pub(crate) fn open_chosen(
    chosen: &InputConfig,
    opener: &dyn Fn(&InputConfig, Generation) -> Result<LiveAudio, LiveAudioError>,
) -> Result<(LiveAudio, Option<LiveAudioError>), LiveAudioError> {
    let open = |input| opener(input, Generation(1));
    let default = InputConfig::default();
    match open(chosen) {
        Ok(live) => Ok((live, None)),
        Err(error) if *chosen != default => {
            let failure = endpoint_of(chosen).is_some().then_some(error);
            Ok((open(&default)?, failure))
        }
        Err(error) => Err(error),
    }
}

impl LiveAudio {
    pub(crate) fn open(
        config: LiveAudioConfig,
        input: &InputConfig,
        generation: Generation,
    ) -> Result<Self, LiveAudioError> {
        let endpoint = endpoint_of(input)
            .ok_or_else(|| LiveAudioError::UnsupportedInput(input.kind.clone()))?;
        let system_input = OpenSystemInput::open(&endpoint, generation)
            .map_err(|error| LiveAudioError::Input(error.to_string()))?;
        let descriptor = Arc::clone(system_input.descriptor());
        let name = system_input.name().to_owned();
        let telemetry = Arc::new(SourceTelemetry::new());
        // Capture endpoints keep delivering silence; only loopback stops.
        let (capture_endpoint, dataflow, handle) = Self::create_transport(
            config,
            Arc::clone(&descriptor),
            Arc::clone(&telemetry),
            endpoint.is_playback(),
        )?;
        let capture = system_input
            .start(capture_endpoint, Arc::clone(&telemetry))
            .map_err(|error| LiveAudioError::Input(error.to_string()))?;

        Ok(Self::from_parts(
            InputDriver::System(capture),
            dataflow,
            handle,
            descriptor,
            telemetry,
            name,
        ))
    }

    #[cfg(feature = "automation")]
    pub(crate) fn open_manual(
        config: LiveAudioConfig,
        synthetic: SyntheticConfig,
    ) -> Result<(Self, ManualInputDriver), LiveAudioError> {
        let sample_capacity = synthetic
            .validate(config.max_block_frames)
            .map_err(|error| LiveAudioError::Synthetic(error.to_string()))?;
        let descriptor = Arc::new(synthetic.descriptor.clone());
        let telemetry = Arc::new(SourceTelemetry::new());
        let (endpoint, dataflow, handle) = Self::create_transport(
            config,
            Arc::clone(&descriptor),
            Arc::clone(&telemetry),
            false,
        )?;
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
                SYNTHETIC_NAME.to_owned(),
            ),
            driver,
        ))
    }

    #[cfg(feature = "automation")]
    pub(crate) fn open_realtime(
        config: LiveAudioConfig,
        synthetic: SyntheticConfig,
    ) -> Result<Self, LiveAudioError> {
        let sample_capacity = synthetic
            .validate(config.max_block_frames)
            .map_err(|error| LiveAudioError::Synthetic(error.to_string()))?;
        let descriptor = Arc::new(synthetic.descriptor.clone());
        let telemetry = Arc::new(SourceTelemetry::new());
        let (endpoint, dataflow, handle) = Self::create_transport(
            config,
            Arc::clone(&descriptor),
            Arc::clone(&telemetry),
            false,
        )?;
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
            SYNTHETIC_NAME.to_owned(),
        ))
    }

    /// WASAPI loopback stops callbacks during silence; `fill_silence` lets the worker fill the gap.
    fn create_transport(
        config: LiveAudioConfig,
        descriptor: Arc<crate::audio::AudioDescriptor>,
        telemetry: Arc<SourceTelemetry>,
        fill_silence: bool,
    ) -> Result<(CaptureEndpoint, Dataflow, DataflowHandle), LiveAudioError> {
        let (mut endpoint, processing_endpoint) = create_block_transport(
            usize::from(descriptor.channel_count),
            config.transport_blocks,
            config.max_block_frames,
            Arc::clone(&telemetry),
        )
        .map_err(|error| LiveAudioError::Transport(error.to_string()))?;
        let (dataflow, handle) = Dataflow::spawn_with(
            Arc::clone(&descriptor),
            processing_endpoint,
            telemetry,
            fill_silence,
        )
        .map_err(LiveAudioError::Worker)?;
        endpoint.set_worker(dataflow.worker_thread());
        Ok((endpoint, dataflow, handle))
    }

    fn from_parts(
        input: InputDriver,
        dataflow: Dataflow,
        handle: DataflowHandle,
        descriptor: Arc<crate::audio::AudioDescriptor>,
        telemetry: Arc<SourceTelemetry>,
        name: String,
    ) -> Self {
        Self {
            input: Some(input),
            dataflow: Some(dataflow),
            reader: SourceReader::new(descriptor, Arc::clone(&telemetry)),
            handle,
            telemetry,
            paused: false,
            name,
        }
    }

    /// Device or source name shown to the user.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Reports the loss the way a failing system stream does.
    #[cfg(feature = "automation")]
    pub(crate) fn lose_device_for_automation(&self) {
        self.telemetry
            .device_lost("device unplugged (simulated)".to_owned());
    }

    pub(crate) fn paused(&self) -> bool {
        self.paused
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
                #[cfg(feature = "automation")]
                InputDriver::Manual(source) => {
                    source.pause().map_err(map_synthetic_error)?;
                }
                #[cfg(feature = "automation")]
                InputDriver::Realtime(source) => {
                    source.pause().map_err(map_synthetic_error)?;
                }
            }
            self.paused = true;
        } else {
            match input {
                InputDriver::System(capture) => {
                    // Queue the continuity boundary before capture can publish resumed blocks.
                    let continuity = self.telemetry.continuity().wrapping_add(1);
                    self.handle
                        .resume_at(continuity)
                        .map_err(LiveAudioError::Control)?;
                    self.telemetry.advance_continuity();
                    capture
                        .resume()
                        .map_err(|error| LiveAudioError::Input(error.to_string()))?;
                }
                #[cfg(feature = "automation")]
                InputDriver::Manual(source) => {
                    source.resume(&self.handle).map_err(map_synthetic_error)?;
                }
                #[cfg(feature = "automation")]
                InputDriver::Realtime(source) => {
                    source.resume(&self.handle).map_err(map_synthetic_error)?;
                }
            }
            self.paused = false;
        }
        Ok(self.reader.source_status().state)
    }
}

#[cfg(feature = "automation")]
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
