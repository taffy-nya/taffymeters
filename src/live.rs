use std::{error::Error, fmt, sync::Arc};

use crate::{
    audio::{OpenSystemInput, SourceState, SourceTelemetry, SystemCapture, create_block_transport},
    dataflow::{
        ChannelSelection, ControlError, FftConfig, HistoryConfig, PeakConfig, RuntimeMetrics,
        SharedAnalysisId, SpectrumController, SpectrumDataflow, SpectrumRequest,
        SpectrumSubscription,
    },
    snapshot::{SpectrumReader, TimeDomainReader},
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
    primary: Option<SpectrumSubscription>,
    reader: SpectrumReader,
    time_domain: TimeDomainReader,
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
        );
        let mut request = SpectrumRequest::new(config.initial_channel, config.fft);
        request.peak = Some(PeakConfig::new(1_000.0, 18.0).expect("default peak is valid"));
        request.history = Some(HistoryConfig::new(8.0).expect("default spectrum history is valid"));
        let primary = dataflow
            .subscribe(request)
            .map_err(LiveSpectrumError::Control)?;
        let controller = primary.controller();
        let reader = primary.reader();
        let time_domain = dataflow.time_domain_reader();
        let mut capture_endpoint = capture_endpoint;
        capture_endpoint.set_worker(dataflow.worker_thread());
        let capture = input
            .start(capture_endpoint, Arc::clone(&telemetry))
            .map_err(|error| LiveSpectrumError::Input(error.to_string()))?;

        Ok(Self {
            capture: Some(capture),
            dataflow: Some(dataflow),
            primary: Some(primary),
            reader,
            controller,
            time_domain,
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

    pub fn time_domain_reader(&self) -> TimeDomainReader {
        self.time_domain.clone()
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
            self.dataflow
                .as_ref()
                .expect("dataflow exists while runtime is live")
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

    pub fn subscribe(
        &self,
        request: SpectrumRequest,
    ) -> Result<SpectrumSubscription, LiveSpectrumError> {
        self.dataflow
            .as_ref()
            .expect("dataflow exists while runtime is live")
            .subscribe(request)
            .map_err(LiveSpectrumError::Control)
    }

    pub fn subscribe_shared(
        &self,
        request: SpectrumRequest,
        binding: SharedAnalysisId,
    ) -> Result<SpectrumSubscription, LiveSpectrumError> {
        self.dataflow
            .as_ref()
            .expect("dataflow exists while runtime is live")
            .subscribe_shared(request, binding)
            .map_err(LiveSpectrumError::Control)
    }

    pub fn dataflow_metrics(&self) -> RuntimeMetrics {
        self.dataflow
            .as_ref()
            .expect("dataflow exists while runtime is live")
            .metrics()
    }
}

impl Drop for LiveSpectrum {
    fn drop(&mut self) {
        self.capture.take();
        self.primary.take();
        self.dataflow.take();
    }
}
