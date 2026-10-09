use std::{
    collections::{HashMap, HashSet},
    error::Error,
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle, Thread},
    time::Duration,
};

use arc_swap::ArcSwapOption;

use crate::{
    audio::{AudioBlock, AudioDescriptor, ProcessingEndpoint, SourceTelemetry},
    snapshot::{SpectrumHistoryFrame, SpectrumReader, SpectrumSnapshotSet, TimeDomainSnapshot},
};

use super::{
    ChannelError, ChannelSelection, FftConfig, RequestError, SharedAnalysisId, SpectrumRequest,
    TimeDomainRequest, TimeDomainRequestError, TimeDomainSubscription,
    fft::StreamingFft,
    processing::{HistoryState, PeakState, SmoothState, apply_center_and_gain, apply_tilt},
    time_runtime::{
        TimeDomainProcessor, ValidatedTimeDomainRequest, make_reader, new_slot, next_id,
    },
};

const CONTROL_QUEUE_CAPACITY: usize = 16;
const COMMAND_BUDGET_PER_TICK: usize = 16;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeMetrics {
    pub fft_evaluations: u64,
    pub pipeline_evaluations: u64,
    pub subscriptions: u64,
    pub channel_branches: u64,
    pub fft_branches: u64,
    pub pipelines: u64,
    pub time_pcm_frames: u64,
    pub time_level_frames: u64,
    pub time_publications: u64,
}

/// Called on the worker thread after a processed block published new results (any
/// spectrum or time-domain snapshot). Lets a UI sleep until there is something new to
/// draw. It must be cheap and must not block; it never runs in the audio callback.
pub type PublicationWaker = Box<dyn Fn() + Send + Sync>;

/// What the worker needs to notice publications and wake the UI.
struct PublicationNotifier {
    waker: Arc<ArcSwapOption<PublicationWaker>>,
    metrics: Arc<Metrics>,
    time_metrics: Arc<TimeMetrics>,
}

impl PublicationNotifier {
    fn published(&self) -> u64 {
        self.metrics.pipeline_evaluations.load(Ordering::Relaxed)
            + self.time_metrics.publications.load(Ordering::Relaxed)
    }

    fn wake(&self) {
        if let Some(waker) = self.waker.load().as_ref() {
            waker();
        }
    }
}

#[derive(Default)]
struct Metrics {
    fft_evaluations: AtomicU64,
    pipeline_evaluations: AtomicU64,
    channel_branches: AtomicU64,
    subscriptions: AtomicU64,
    fft_branches: AtomicU64,
    pipelines: AtomicU64,
}

#[derive(Default)]
pub(super) struct TimeMetrics {
    pub(super) pcm_frames: AtomicU64,
    pub(super) level_frames: AtomicU64,
    pub(super) publications: AtomicU64,
}

impl Metrics {
    fn snapshot(&self, time: &TimeMetrics) -> RuntimeMetrics {
        RuntimeMetrics {
            fft_evaluations: self.fft_evaluations.load(Ordering::Relaxed),
            pipeline_evaluations: self.pipeline_evaluations.load(Ordering::Relaxed),
            channel_branches: self.channel_branches.load(Ordering::Acquire),
            subscriptions: self.subscriptions.load(Ordering::Acquire),
            fft_branches: self.fft_branches.load(Ordering::Acquire),
            pipelines: self.pipelines.load(Ordering::Acquire),
            time_pcm_frames: time.pcm_frames.load(Ordering::Relaxed),
            time_level_frames: time.level_frames.load(Ordering::Relaxed),
            time_publications: time.publications.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone)]
pub struct SpectrumController {
    inner: Arc<SubscriptionControl>,
}

struct SubscriptionControl {
    id: u64,
    commands: SyncSender<Command>,
    worker: Thread,
    stopped: Arc<AtomicBool>,
    request: Mutex<SpectrumRequest>,
    requested_channel: Arc<AtomicU32>,
    requested_revision: Arc<AtomicU64>,
    active: Arc<AtomicBool>,
}

pub struct SpectrumSubscription {
    controller: SpectrumController,
    reader: SpectrumReader,
}

impl SpectrumSubscription {
    pub fn reader(&self) -> SpectrumReader {
        self.reader.clone()
    }

    pub fn controller(&self) -> SpectrumController {
        self.controller.clone()
    }
}

impl Drop for SpectrumSubscription {
    fn drop(&mut self) {
        self.controller.inner.active.store(false, Ordering::Release);
        if !self.controller.inner.stopped.load(Ordering::Acquire) {
            let _ = self
                .controller
                .inner
                .commands
                .send(Command::Remove(self.controller.inner.id));
            self.controller.inner.worker.unpark();
        }
    }
}

pub(crate) struct Dataflow {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Clone)]
pub struct DataflowHandle {
    commands: SyncSender<Command>,
    worker: Thread,
    stopped: Arc<AtomicBool>,
    next_subscription_id: Arc<AtomicU64>,
    descriptor: Arc<AudioDescriptor>,
    telemetry: Arc<SourceTelemetry>,
    metrics: Arc<Metrics>,
    time_metrics: Arc<TimeMetrics>,
    waker: Arc<ArcSwapOption<PublicationWaker>>,
    #[cfg(any(test, feature = "automation"))]
    processed_through_frame: Arc<AtomicU64>,
}

enum Command {
    Add {
        subscription: SubscriptionState,
        response: std::sync::mpsc::SyncSender<Result<(), ControlError>>,
    },
    AddTimeDomain {
        id: u64,
        request: ValidatedTimeDomainRequest,
        slot: Arc<ArcSwapOption<TimeDomainSnapshot>>,
        response: std::sync::mpsc::SyncSender<Result<(), ControlError>>,
    },
    Update {
        id: u64,
        request: SpectrumRequest,
        revision: u64,
    },
    SetVisible(u64, bool),
    SetAnalysisPaused(u64, bool),
    ResetState(u64),
    Remove(u64),
    RemoveTimeDomain(u64),
    ResumeBoundary(u64),
    #[cfg(test)]
    BlockWorkerForTest {
        entered: Arc<std::sync::Barrier>,
        release: Arc<std::sync::Barrier>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlError {
    Busy,
    Stopped,
    SubscriptionClosed,
    SharedBindingMismatch(SharedAnalysisId),
    InvalidRequest(RequestError),
    InvalidTimeDomainRequest(TimeDomainRequestError),
}

impl fmt::Display for ControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("dataflow control queue is busy"),
            Self::Stopped => f.write_str("dataflow worker has stopped"),
            Self::SubscriptionClosed => f.write_str("spectrum subscription is closed"),
            Self::SharedBindingMismatch(id) => {
                write!(f, "shared analysis {} has a different request", id.0)
            }
            Self::InvalidRequest(error) => write!(f, "invalid spectrum request: {error}"),
            Self::InvalidTimeDomainRequest(error) => {
                write!(f, "invalid time-domain request: {error}")
            }
        }
    }
}

impl Error for ControlError {}

impl SpectrumController {
    pub fn set_channel(&self, channel: ChannelSelection) -> Result<(), ControlError> {
        self.modify_request(|request| request.channel = channel)
    }

    pub fn set_fft(&self, fft: FftConfig) -> Result<(), ControlError> {
        self.modify_request(|request| request.fft = fft)
    }

    pub fn update_request(&self, request: SpectrumRequest) -> Result<(), ControlError> {
        self.ensure_active()?;
        let request = request.validated().map_err(ControlError::InvalidRequest)?;
        let mut current = self
            .inner
            .request
            .lock()
            .map_err(|_| ControlError::SubscriptionClosed)?;
        self.commit_request(&mut current, request)
    }

    fn modify_request(
        &self,
        update: impl FnOnce(&mut SpectrumRequest),
    ) -> Result<(), ControlError> {
        self.ensure_active()?;
        let mut current = self
            .inner
            .request
            .lock()
            .map_err(|_| ControlError::SubscriptionClosed)?;
        let mut request = *current;
        update(&mut request);
        let request = request.validated().map_err(ControlError::InvalidRequest)?;
        self.commit_request(&mut current, request)
    }

    fn commit_request(
        &self,
        current: &mut SpectrumRequest,
        request: SpectrumRequest,
    ) -> Result<(), ControlError> {
        if *current == request {
            return Ok(());
        }
        let revision = self
            .inner
            .requested_revision
            .load(Ordering::Acquire)
            .wrapping_add(1);
        self.send(Command::Update {
            id: self.inner.id,
            request,
            revision,
        })?;
        *current = request;
        self.inner
            .requested_channel
            .store(request.channel.encode(), Ordering::Release);
        self.inner
            .requested_revision
            .store(revision, Ordering::Release);
        Ok(())
    }

    pub fn set_visible(&self, visible: bool) -> Result<(), ControlError> {
        self.ensure_active()?;
        self.send(Command::SetVisible(self.inner.id, visible))
    }

    pub fn set_analysis_paused(&self, paused: bool) -> Result<(), ControlError> {
        self.ensure_active()?;
        self.send(Command::SetAnalysisPaused(self.inner.id, paused))
    }

    pub fn reset_state(&self) -> Result<(), ControlError> {
        self.ensure_active()?;
        self.send(Command::ResetState(self.inner.id))
    }

    pub fn request(&self) -> Result<SpectrumRequest, ControlError> {
        self.current_request()
    }

    fn current_request(&self) -> Result<SpectrumRequest, ControlError> {
        self.ensure_active()?;
        self.inner
            .request
            .lock()
            .map(|request| *request)
            .map_err(|_| ControlError::SubscriptionClosed)
    }

    fn ensure_active(&self) -> Result<(), ControlError> {
        if !self.inner.active.load(Ordering::Acquire) {
            Err(ControlError::SubscriptionClosed)
        } else if self.inner.stopped.load(Ordering::Acquire) {
            Err(ControlError::Stopped)
        } else {
            Ok(())
        }
    }

    fn send(&self, command: Command) -> Result<(), ControlError> {
        if self.inner.stopped.load(Ordering::Acquire) {
            return Err(ControlError::Stopped);
        }
        match self.inner.commands.try_send(command) {
            Ok(()) => {
                self.inner.worker.unpark();
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err(ControlError::Busy),
            Err(TrySendError::Disconnected(_)) => Err(ControlError::Stopped),
        }
    }
}

impl Dataflow {
    pub(crate) fn spawn(
        descriptor: Arc<AudioDescriptor>,
        mut endpoint: ProcessingEndpoint,
        telemetry: Arc<SourceTelemetry>,
    ) -> (Self, DataflowHandle) {
        let (command_sender, command_receiver) = sync_channel(CONTROL_QUEUE_CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let metrics = Arc::new(Metrics::default());
        let worker_metrics = Arc::clone(&metrics);
        let time_metrics = Arc::new(TimeMetrics::default());
        let worker_time_metrics = Arc::clone(&time_metrics);
        let worker_descriptor = Arc::clone(&descriptor);
        let waker = Arc::new(ArcSwapOption::<PublicationWaker>::empty());
        let notifier = PublicationNotifier {
            waker: Arc::clone(&waker),
            metrics: Arc::clone(&metrics),
            time_metrics: Arc::clone(&time_metrics),
        };
        #[cfg(any(test, feature = "automation"))]
        let processed_through_frame = Arc::new(AtomicU64::new(0));
        #[cfg(any(test, feature = "automation"))]
        let worker_frontier = Arc::clone(&processed_through_frame);
        let worker = thread::Builder::new()
            .name("taffymeters-dataflow".to_owned())
            .spawn(move || {
                let mut processor =
                    Processor::new(worker_descriptor, worker_metrics, worker_time_metrics);
                #[cfg(any(test, feature = "automation"))]
                run_worker(
                    &mut processor,
                    &mut endpoint,
                    command_receiver,
                    worker_stop,
                    &notifier,
                    worker_frontier,
                );
                #[cfg(not(any(test, feature = "automation")))]
                run_worker(
                    &mut processor,
                    &mut endpoint,
                    command_receiver,
                    worker_stop,
                    &notifier,
                );
            })
            .expect("failed to create dataflow worker");
        let handle = DataflowHandle {
            commands: command_sender,
            worker: worker.thread().clone(),
            stopped: Arc::clone(&stop),
            next_subscription_id: Arc::new(AtomicU64::new(1)),
            descriptor,
            telemetry,
            metrics,
            time_metrics,
            waker,
            #[cfg(any(test, feature = "automation"))]
            processed_through_frame,
        };
        (
            Self {
                stop,
                worker: Some(worker),
            },
            handle,
        )
    }

    pub(crate) fn worker_thread(&self) -> Thread {
        self.worker
            .as_ref()
            .expect("worker exists while dataflow is live")
            .thread()
            .clone()
    }
}

impl DataflowHandle {
    pub fn subscribe_spectrum(
        &self,
        request: SpectrumRequest,
    ) -> Result<SpectrumSubscription, ControlError> {
        self.subscribe(request, Binding::Private)
    }

    pub fn subscribe_shared(
        &self,
        request: SpectrumRequest,
        binding: SharedAnalysisId,
    ) -> Result<SpectrumSubscription, ControlError> {
        self.subscribe(request, Binding::Shared(binding))
    }

    pub fn subscribe_time(
        &self,
        request: TimeDomainRequest,
    ) -> Result<TimeDomainSubscription, ControlError> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(ControlError::Stopped);
        }
        let request = request
            .validate(
                self.descriptor.sample_rate_hz,
                self.descriptor.channel_count as usize,
            )
            .map_err(ControlError::InvalidTimeDomainRequest)?;
        let id = next_id(&self.next_subscription_id);
        let slot = new_slot();
        let reader = make_reader(
            Arc::clone(&slot),
            Arc::clone(&self.descriptor),
            Arc::clone(&self.telemetry),
        );
        let (response_sender, response_receiver) = sync_channel(1);
        self.send(Command::AddTimeDomain {
            id,
            request,
            slot,
            response: response_sender,
        })?;
        response_receiver
            .recv()
            .map_err(|_| ControlError::Stopped)??;

        let commands = self.commands.clone();
        let worker = self.worker.clone();
        let stopped = Arc::clone(&self.stopped);
        Ok(TimeDomainSubscription::new(reader, move || {
            if !stopped.load(Ordering::Acquire) {
                let _ = commands.send(Command::RemoveTimeDomain(id));
                worker.unpark();
            }
        }))
    }

    pub fn metrics(&self) -> RuntimeMetrics {
        self.metrics.snapshot(&self.time_metrics)
    }

    /// Install (or clear) the callback run after the worker publishes new results.
    pub fn set_publication_waker(&self, waker: Option<PublicationWaker>) {
        self.waker.store(waker.map(Arc::new));
    }
    /// Returns the test/automation input drain frontier, not an analysis snapshot frontier.
    #[cfg(any(test, feature = "automation"))]
    pub(crate) fn processed_through_frame(&self) -> Result<u64, ControlError> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(ControlError::Stopped);
        }
        let frontier = self.processed_through_frame.load(Ordering::Acquire);
        if self.stopped.load(Ordering::Acquire) {
            Err(ControlError::Stopped)
        } else {
            Ok(frontier)
        }
    }

    pub(crate) fn resume_at(&self, continuity: u64) -> Result<(), ControlError> {
        self.send(Command::ResumeBoundary(continuity))
    }

    fn subscribe(
        &self,
        request: SpectrumRequest,
        binding: Binding,
    ) -> Result<SpectrumSubscription, ControlError> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(ControlError::Stopped);
        }
        let request = request.validated().map_err(ControlError::InvalidRequest)?;
        let id = next_id(&self.next_subscription_id);
        let slot = Arc::new(ArcSwapOption::<SpectrumSnapshotSet>::empty());
        let analysis_error = Arc::new(ArcSwapOption::<ChannelError>::empty());
        let requested_channel = Arc::new(AtomicU32::new(request.channel.encode()));
        let requested_revision = Arc::new(AtomicU64::new(1));
        let active = Arc::new(AtomicBool::new(true));
        let reader = SpectrumReader::new(
            Arc::clone(&slot),
            Arc::clone(&self.descriptor),
            Arc::clone(&self.telemetry),
            Arc::clone(&analysis_error),
            Arc::clone(&requested_channel),
            Arc::clone(&requested_revision),
        );
        let subscription = SubscriptionState {
            id,
            request,
            binding,
            revision: 1,
            visible: true,
            paused: false,
            slot,
            analysis_error,
        };
        let (response_sender, response_receiver) = sync_channel(1);
        self.send(Command::Add {
            subscription,
            response: response_sender,
        })?;
        response_receiver
            .recv()
            .map_err(|_| ControlError::Stopped)??;
        Ok(SpectrumSubscription {
            controller: SpectrumController {
                inner: Arc::new(SubscriptionControl {
                    id,
                    commands: self.commands.clone(),
                    worker: self.worker.clone(),
                    stopped: Arc::clone(&self.stopped),
                    request: Mutex::new(request),
                    requested_channel,
                    requested_revision,
                    active,
                }),
            },
            reader,
        })
    }

    fn send(&self, command: Command) -> Result<(), ControlError> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(ControlError::Stopped);
        }
        match self.commands.try_send(command) {
            Ok(()) => {
                self.worker.unpark();
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err(ControlError::Busy),
            Err(TrySendError::Disconnected(_)) => Err(ControlError::Stopped),
        }
    }
}

impl Drop for Dataflow {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Binding {
    Private,
    Shared(SharedAnalysisId),
}

struct SubscriptionState {
    id: u64,
    request: SpectrumRequest,
    binding: Binding,
    revision: u64,
    visible: bool,
    paused: bool,
    slot: Arc<ArcSwapOption<SpectrumSnapshotSet>>,
    analysis_error: Arc<ArcSwapOption<ChannelError>>,
}

impl SubscriptionState {
    fn should_evaluate(&self) -> bool {
        !self.paused && (self.visible || self.request.is_stateful())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct FftKey {
    channel: ChannelSelection,
    window_size: usize,
    hop_size: usize,
    floor_bits: u32,
}

impl FftKey {
    fn new(request: SpectrumRequest) -> Self {
        Self {
            channel: request.channel,
            window_size: request.fft.window_size,
            hop_size: request.fft.hop_size,
            floor_bits: request.fft.floor_dbfs.to_bits(),
        }
    }

    fn config(self) -> FftConfig {
        FftConfig {
            window_size: self.window_size,
            hop_size: self.hop_size,
            floor_dbfs: f32::from_bits(self.floor_bits),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct TransformKey {
    tilt_bits: u32,
    smooth: Option<(u32, u32)>,
    center_bits: u32,
    gain_bits: u32,
    peak: Option<(u32, u32)>,
    history: Option<u32>,
}

impl TransformKey {
    fn new(request: SpectrumRequest) -> Self {
        Self {
            tilt_bits: request.tilt_db_per_octave.to_bits(),
            smooth: request
                .smooth
                .map(|value| (value.attack_ms.to_bits(), value.release_ms.to_bits())),
            center_bits: request.center_db.to_bits(),
            gain_bits: request.gain_db.to_bits(),
            peak: request
                .peak
                .map(|value| (value.hold_ms.to_bits(), value.decay_db_per_second.to_bits())),
            history: request
                .history
                .map(|value| value.duration_seconds.to_bits()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum StateIdentity {
    Pure,
    Private(u64),
    Shared(SharedAnalysisId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct PipelineKey {
    fft: FftKey,
    transform: TransformKey,
    state: StateIdentity,
}

impl PipelineKey {
    fn new(subscription: &SubscriptionState) -> Self {
        Self::from_parts(subscription.id, subscription.request, subscription.binding)
    }

    fn from_parts(id: u64, request: SpectrumRequest, binding: Binding) -> Self {
        let state = if request.is_stateful() {
            match binding {
                Binding::Private => StateIdentity::Private(id),
                Binding::Shared(id) => StateIdentity::Shared(id),
            }
        } else {
            StateIdentity::Pure
        };
        Self {
            fft: FftKey::new(request),
            transform: TransformKey::new(request),
            state,
        }
    }
}

struct ChannelBranch {
    resolved: super::channel::ResolvedChannel,
}

struct FftBranch {
    fft: StreamingFft,
    channel_index: usize,
}

type PipelineOutput = (Arc<[f32]>, Option<Arc<[f32]>>, Arc<[SpectrumHistoryFrame]>);

struct Pipeline {
    request: SpectrumRequest,
    subscribers: Vec<u64>,
    active: bool,
    minimum_frontier: u64,
    smooth: SmoothState,
    peak: PeakState,
    history: HistoryState,
}

struct PipelineMigration {
    smooth: Option<SmoothState>,
    peak: Option<PeakState>,
    history: Option<HistoryState>,
}

impl Pipeline {
    fn new(request: SpectrumRequest) -> Self {
        Self {
            request,
            subscribers: Vec::new(),
            active: false,
            minimum_frontier: 0,
            smooth: SmoothState::new(),
            peak: PeakState::new(),
            history: HistoryState::new(),
        }
    }

    fn migration_to(&self, request: SpectrumRequest) -> PipelineMigration {
        let same_fft = FftKey::new(self.request) == FftKey::new(request);
        let same_tilt =
            self.request.tilt_db_per_octave.to_bits() == request.tilt_db_per_octave.to_bits();
        let same_smooth = self.request.smooth == request.smooth;
        let same_center = self.request.center_db.to_bits() == request.center_db.to_bits();
        let same_gain = self.request.gain_db.to_bits() == request.gain_db.to_bits();
        PipelineMigration {
            smooth: (request.smooth.is_some() && same_fft && same_tilt && same_smooth)
                .then(|| self.smooth.clone()),
            peak: (request.peak.is_some() && same_fft && self.request.peak == request.peak)
                .then(|| self.peak.clone()),
            history: (request.history.is_some()
                && same_fft
                && same_tilt
                && same_smooth
                && same_center
                && same_gain
                && self.request.history == request.history)
                .then(|| self.history.clone()),
        }
    }

    fn apply_migration(&mut self, migration: PipelineMigration) {
        if let Some(smooth) = migration.smooth {
            self.smooth = smooth;
        }
        if let Some(peak) = migration.peak {
            self.peak = peak;
        }
        if let Some(history) = migration.history {
            self.history = history;
        }
    }

    fn process(
        &mut self,
        raw: &[f32],
        bin_hz: f32,
        floor_dbfs: f32,
        processed_through_frame: u64,
        sample_rate_hz: u32,
    ) -> PipelineOutput {
        let tilted = apply_tilt(raw, bin_hz, floor_dbfs, self.request.tilt_db_per_octave);
        let mut spectrum = if let Some(config) = self.request.smooth {
            self.smooth
                .process(&tilted, processed_through_frame, sample_rate_hz, config)
        } else {
            tilted
        };
        apply_center_and_gain(
            &mut spectrum,
            floor_dbfs,
            self.request.center_db,
            self.request.gain_db,
        );
        let spectrum: Arc<[f32]> = Arc::from(spectrum);
        let peak = self.request.peak.map(|config| {
            self.peak
                .process(raw, processed_through_frame, sample_rate_hz, config)
        });
        let history = if let Some(config) = self.request.history {
            self.history.push(
                Arc::clone(&spectrum),
                processed_through_frame,
                sample_rate_hz,
                config,
            )
        } else {
            Arc::from([])
        };
        (spectrum, peak, history)
    }

    fn mark_gap(&mut self) {
        self.smooth.reset();
        self.peak.reset();
        self.history.mark_gap();
    }

    fn reset(&mut self) {
        self.smooth.reset();
        self.peak.reset();
        self.history.clear();
    }
}

struct Evaluation {
    key: FftKey,
    result_version: u64,
    processed_through_frame: u64,
    source_sequence: u64,
    metadata: super::FftMetadata,
    raw: Arc<[f32]>,
}

struct Processor {
    descriptor: Arc<AudioDescriptor>,
    subscriptions: HashMap<u64, SubscriptionState>,
    fft_branches: HashMap<FftKey, FftBranch>,
    channel_branches: Vec<ChannelBranch>,
    channel_samples: Vec<f32>,
    pipelines: HashMap<PipelineKey, Pipeline>,
    metrics: Arc<Metrics>,
    time_metrics: Arc<TimeMetrics>,
    time_domain: Option<TimeDomainProcessor>,
    pending: Vec<Evaluation>,
    result_version: u64,
    discontinuity_count: u64,
    expected_sequence: Option<u64>,
    expected_start_frame: Option<u64>,
    minimum_continuity: u64,
    active_continuity: u64,
    current_frontier: u64,
}

impl Processor {
    fn new(
        descriptor: Arc<AudioDescriptor>,
        metrics: Arc<Metrics>,
        time_metrics: Arc<TimeMetrics>,
    ) -> Self {
        Self {
            descriptor,
            subscriptions: HashMap::new(),
            fft_branches: HashMap::new(),
            channel_branches: Vec::new(),
            channel_samples: Vec::new(),
            pipelines: HashMap::new(),
            metrics,
            time_metrics,
            time_domain: None,
            pending: Vec::new(),
            result_version: 0,
            discontinuity_count: 0,
            expected_sequence: None,
            expected_start_frame: None,
            minimum_continuity: 0,
            active_continuity: 0,
            current_frontier: 0,
        }
    }

    fn add_subscription(&mut self, subscription: SubscriptionState) -> Result<(), ControlError> {
        if let Binding::Shared(binding) = subscription.binding
            && self.subscriptions.values().any(|existing| {
                existing.binding == Binding::Shared(binding)
                    && existing.request != subscription.request
            })
        {
            return Err(ControlError::SharedBindingMismatch(binding));
        }
        if let Some(snapshot) = self.shared_snapshot_for(&subscription) {
            subscription.slot.store(Some(snapshot));
        }
        self.set_channel_error(&subscription);
        self.subscriptions.insert(subscription.id, subscription);
        self.reconcile();
        Ok(())
    }

    fn add_time_subscription(
        &mut self,
        id: u64,
        request: ValidatedTimeDomainRequest,
        slot: Arc<ArcSwapOption<TimeDomainSnapshot>>,
    ) {
        if self.time_domain.is_none() {
            self.time_domain = Some(TimeDomainProcessor::new(
                Arc::clone(&self.descriptor),
                Arc::clone(&self.time_metrics),
            ));
        }
        self.time_domain
            .as_mut()
            .expect("time-domain processor was just created")
            .add_subscription(id, request, slot, self.current_frontier);
    }

    fn remove_time_subscription(&mut self, id: u64) {
        let remove_processor = if let Some(time_domain) = &mut self.time_domain {
            time_domain.remove_subscription(id);
            time_domain.is_empty()
        } else {
            false
        };
        if remove_processor {
            self.time_domain = None;
        }
    }

    fn update_subscription(&mut self, id: u64, request: SpectrumRequest, revision: u64) {
        let Some(current) = self.subscriptions.get(&id) else {
            return;
        };
        let old_key = PipelineKey::new(current);
        let should_fork = matches!(current.binding, Binding::Shared(_))
            && self
                .subscriptions
                .values()
                .any(|other| other.id != id && other.binding == current.binding);
        let binding = if should_fork {
            Binding::Private
        } else {
            current.binding
        };
        let new_key = PipelineKey::from_parts(id, request, binding);
        let migration = self
            .pipelines
            .get(&old_key)
            .map(|pipeline| pipeline.migration_to(request));
        let subscription = self
            .subscriptions
            .get_mut(&id)
            .expect("subscription was checked above");
        subscription.binding = binding;
        subscription.request = request;
        subscription.revision = revision;
        subscription.slot.store(None);
        self.set_channel_error_by_id(id);
        self.reconcile();
        if old_key != new_key
            && let Some(migration) = migration
            && let Some(pipeline) = self.pipelines.get_mut(&new_key)
        {
            pipeline.apply_migration(migration);
        }
    }

    fn set_visible(&mut self, id: u64, visible: bool) {
        if let Some(subscription) = self.subscriptions.get_mut(&id)
            && subscription.visible != visible
        {
            subscription.visible = visible;
            self.reconcile();
        }
    }

    fn set_paused(&mut self, id: u64, paused: bool) {
        let Some(subscription) = self.subscriptions.get(&id) else {
            return;
        };
        if subscription.paused == paused {
            return;
        }
        let key = PipelineKey::new(subscription);
        let other_active = self.subscriptions.values().any(|other| {
            other.id != id && PipelineKey::new(other) == key && other.should_evaluate()
        });
        if let Some(subscription) = self.subscriptions.get_mut(&id) {
            subscription.paused = paused;
            if !paused {
                subscription.slot.store(None);
            }
        }
        if !paused
            && !other_active
            && let Some(pipeline) = self.pipelines.get_mut(&key)
        {
            pipeline.mark_gap();
            pipeline.minimum_frontier = self
                .current_frontier
                .saturating_add(key.fft.window_size as u64);
        }
        self.reconcile();
    }

    fn shared_snapshot_for(
        &self,
        subscription: &SubscriptionState,
    ) -> Option<Arc<SpectrumSnapshotSet>> {
        let Binding::Shared(binding) = subscription.binding else {
            return None;
        };
        let snapshot = self
            .subscriptions
            .values()
            .find(|existing| {
                existing.binding == Binding::Shared(binding)
                    && existing.request == subscription.request
            })?
            .slot
            .load_full()?;
        let mut snapshot = (*snapshot).clone();
        snapshot.config_revision = subscription.revision;
        Some(Arc::new(snapshot))
    }
    fn reset_state(&mut self, id: u64) {
        let Some(subscription) = self.subscriptions.get(&id) else {
            return;
        };
        let key = PipelineKey::new(subscription);
        if !subscription.request.is_stateful() {
            return;
        }
        if let Some(pipeline) = self.pipelines.get_mut(&key) {
            pipeline.reset();
            pipeline.minimum_frontier = self
                .current_frontier
                .saturating_add(key.fft.window_size as u64);
            for subscriber_id in &pipeline.subscribers {
                if let Some(subscriber) = self.subscriptions.get(subscriber_id) {
                    subscriber.slot.store(None);
                }
            }
        }
    }

    fn remove_subscription(&mut self, id: u64) {
        self.subscriptions.remove(&id);
        self.reconcile();
    }

    fn set_channel_error(&self, subscription: &SubscriptionState) {
        match subscription.request.channel.resolve(&self.descriptor) {
            Ok(_) => subscription.analysis_error.store(None),
            Err(error) => subscription.analysis_error.store(Some(Arc::new(error))),
        }
    }

    fn set_channel_error_by_id(&self, id: u64) {
        if let Some(subscription) = self.subscriptions.get(&id) {
            self.set_channel_error(subscription);
        }
    }

    fn reconcile(&mut self) {
        let mut desired: HashMap<PipelineKey, (SpectrumRequest, Vec<u64>, bool)> = HashMap::new();
        for subscription in self.subscriptions.values() {
            if subscription
                .request
                .channel
                .resolve(&self.descriptor)
                .is_err()
            {
                continue;
            }
            let entry = desired
                .entry(PipelineKey::new(subscription))
                .or_insert_with(|| (subscription.request, Vec::new(), false));
            entry.1.push(subscription.id);
            entry.2 |= subscription.should_evaluate();
        }
        self.pipelines.retain(|key, _| desired.contains_key(key));
        let current_frontier = self.current_frontier;
        for (key, (request, subscribers, active)) in desired {
            if let Some(pipeline) = self.pipelines.get_mut(&key) {
                if !pipeline.active && active && request.is_stateful() {
                    pipeline.mark_gap();
                    pipeline.minimum_frontier =
                        current_frontier.saturating_add(key.fft.window_size as u64);
                }
                pipeline.subscribers = subscribers;
                pipeline.active = active;
            } else {
                let mut pipeline = Pipeline::new(request);
                pipeline.subscribers = subscribers;
                pipeline.active = active;
                self.pipelines.insert(key, pipeline);
            }
        }
        let desired_fft: HashSet<_> = self
            .pipelines
            .iter()
            .filter_map(|(key, pipeline)| pipeline.active.then_some(key.fft))
            .collect();
        self.fft_branches.retain(|key, _| desired_fft.contains(key));
        for key in desired_fft {
            self.fft_branches.entry(key).or_insert_with(|| FftBranch {
                fft: StreamingFft::new(key.config()),
                channel_index: 0,
            });
        }
        let mut selections = Vec::with_capacity(self.fft_branches.len());
        for key in self.fft_branches.keys() {
            if !selections.contains(&key.channel) {
                selections.push(key.channel);
            }
        }
        self.channel_branches = selections
            .iter()
            .copied()
            .map(|selection| ChannelBranch {
                resolved: selection
                    .resolve(&self.descriptor)
                    .expect("invalid channels are excluded during reconciliation"),
            })
            .collect();
        self.channel_samples
            .resize(self.channel_branches.len(), 0.0);
        for (key, branch) in &mut self.fft_branches {
            branch.channel_index = selections
                .iter()
                .position(|selection| *selection == key.channel)
                .expect("FFT channel has a shared channel prefix");
        }
        self.metrics
            .subscriptions
            .store(self.subscriptions.len() as u64, Ordering::Release);
        self.metrics
            .channel_branches
            .store(self.channel_branches.len() as u64, Ordering::Release);
        self.metrics
            .fft_branches
            .store(self.fft_branches.len() as u64, Ordering::Release);
        self.metrics
            .pipelines
            .store(self.pipelines.len() as u64, Ordering::Release);
    }

    fn resume_at(&mut self, continuity: u64) {
        self.minimum_continuity = continuity;
        self.active_continuity = continuity;
        for subscription in self.subscriptions.values() {
            subscription.slot.store(None);
        }
        self.reset_continuity();
    }

    fn reset_continuity(&mut self) {
        for branch in self.fft_branches.values_mut() {
            branch.fft.reset();
        }
        for pipeline in self.pipelines.values_mut() {
            pipeline.mark_gap();
            pipeline.minimum_frontier = 0;
        }
        self.expected_sequence = None;
        self.expected_start_frame = None;
        if let Some(time_domain) = &mut self.time_domain {
            time_domain.mark_gap();
        }
        self.discontinuity_count = self.discontinuity_count.wrapping_add(1);
    }

    fn process_block(&mut self, block: &AudioBlock) {
        if block.continuity < self.minimum_continuity {
            return;
        }
        if block.continuity != self.active_continuity {
            self.minimum_continuity = block.continuity;
            self.active_continuity = block.continuity;
            for subscription in self.subscriptions.values() {
                subscription.slot.store(None);
            }
            self.reset_continuity();
        }
        let sequence_continuous = self
            .expected_sequence
            .is_none_or(|expected| expected == block.sequence);
        let frame_continuous = self
            .expected_start_frame
            .is_none_or(|expected| expected == block.start_frame);
        if !sequence_continuous || !frame_continuous {
            self.reset_continuity();
        }
        self.expected_sequence = Some(block.sequence.wrapping_add(1));
        self.expected_start_frame =
            Some(block.start_frame.saturating_add(block.frame_count as u64));
        if let Some(time_domain) = &mut self.time_domain {
            time_domain.process_block(
                block.start_frame,
                block.sequence,
                block.frame_count,
                block.samples(self.descriptor.channel_count as usize),
                self.discontinuity_count,
            );
        }

        let channels = self.descriptor.channel_count as usize;
        self.pending.clear();
        for (frame_index, frame) in block.samples(channels).chunks_exact(channels).enumerate() {
            let processed_through_frame = block.start_frame + frame_index as u64 + 1;
            for (output, channel) in self.channel_samples.iter_mut().zip(&self.channel_branches) {
                *output = channel.resolved.sample(frame);
            }
            for (key, branch) in &mut self.fft_branches {
                let sample = self.channel_samples[branch.channel_index];
                if branch.fft.push(sample) {
                    self.result_version = self.result_version.wrapping_add(1);
                    self.metrics.fft_evaluations.fetch_add(1, Ordering::Relaxed);
                    self.pending.push(Evaluation {
                        key: *key,
                        result_version: self.result_version,
                        processed_through_frame,
                        source_sequence: block.sequence,
                        metadata: branch.fft.metadata(self.descriptor.sample_rate_hz),
                        raw: Arc::from(branch.fft.bins_dbfs()),
                    });
                }
            }
        }
        self.current_frontier = block.start_frame.saturating_add(block.frame_count as u64);
        let mut pending = std::mem::take(&mut self.pending);
        for evaluation in pending.drain(..) {
            self.publish(evaluation);
        }
        self.pending = pending;
    }

    fn publish(&mut self, evaluation: Evaluation) {
        for (key, pipeline) in &mut self.pipelines {
            if key.fft != evaluation.key
                || !pipeline.active
                || evaluation.processed_through_frame < pipeline.minimum_frontier
            {
                continue;
            }
            let (spectrum, peak, history) = pipeline.process(
                &evaluation.raw,
                evaluation.metadata.bin_hz,
                evaluation.metadata.floor_dbfs,
                evaluation.processed_through_frame,
                self.descriptor.sample_rate_hz,
            );
            pipeline.minimum_frontier = 0;
            self.metrics
                .pipeline_evaluations
                .fetch_add(1, Ordering::Relaxed);
            for id in &pipeline.subscribers {
                let Some(subscription) = self.subscriptions.get(id) else {
                    continue;
                };
                if !subscription.should_evaluate() {
                    continue;
                }
                subscription.slot.store(Some(Arc::new(SpectrumSnapshotSet {
                    descriptor: Arc::clone(&self.descriptor),
                    channel: subscription.request.channel,
                    config_revision: subscription.revision,
                    result_version: evaluation.result_version,
                    processed_through_frame: evaluation.processed_through_frame,
                    source_sequence: evaluation.source_sequence,
                    discontinuity_count: self.discontinuity_count,
                    fft: evaluation.metadata,
                    spectrum_dbfs: Arc::clone(&spectrum),
                    peak_dbfs: peak.clone(),
                    history: Arc::clone(&history),
                })));
            }
        }
    }
}

fn run_worker(
    processor: &mut Processor,
    endpoint: &mut ProcessingEndpoint,
    commands: Receiver<Command>,
    stop: Arc<AtomicBool>,
    notifier: &PublicationNotifier,
    #[cfg(any(test, feature = "automation"))] processed_frontier: Arc<AtomicU64>,
) {
    while !stop.load(Ordering::Acquire) {
        let mut command_budget_exhausted = true;
        for _ in 0..COMMAND_BUDGET_PER_TICK {
            match commands.try_recv() {
                Ok(Command::Add {
                    subscription,
                    response,
                }) => {
                    let _ = response.send(processor.add_subscription(subscription));
                }
                Ok(Command::AddTimeDomain {
                    id,
                    request,
                    slot,
                    response,
                }) => {
                    processor.add_time_subscription(id, request, slot);
                    let _ = response.send(Ok(()));
                }
                #[cfg(test)]
                Ok(Command::BlockWorkerForTest { entered, release }) => {
                    entered.wait();
                    release.wait();
                }
                Ok(Command::Update {
                    id,
                    request,
                    revision,
                }) => processor.update_subscription(id, request, revision),
                Ok(Command::SetVisible(id, visible)) => processor.set_visible(id, visible),
                Ok(Command::SetAnalysisPaused(id, paused)) => processor.set_paused(id, paused),
                Ok(Command::ResetState(id)) => processor.reset_state(id),
                Ok(Command::Remove(id)) => processor.remove_subscription(id),
                Ok(Command::RemoveTimeDomain(id)) => processor.remove_time_subscription(id),
                Ok(Command::ResumeBoundary(continuity)) => processor.resume_at(continuity),
                Err(TryRecvError::Empty) => {
                    command_budget_exhausted = false;
                    break;
                }
                Err(TryRecvError::Disconnected) => return,
            }
        }
        if let Some(block) = endpoint.try_pop() {
            #[cfg(any(test, feature = "automation"))]
            let block_end = u64::try_from(block.frame_count)
                .ok()
                .and_then(|frames| block.start_frame.checked_add(frames));
            let published = notifier.published();
            processor.process_block(&block);
            endpoint.recycle(block);
            if notifier.published() != published {
                notifier.wake();
            }
            #[cfg(any(test, feature = "automation"))]
            if let Some(block_end) = block_end {
                processed_frontier.store(block_end, Ordering::Release);
            }
        } else if !command_budget_exhausted {
            thread::park_timeout(Duration::from_millis(5));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        f32::consts::PI,
        sync::{Barrier, atomic::AtomicUsize},
        time::Instant,
    };

    use super::*;
    use crate::audio::{
        ChannelLayout, Generation, SourceId, SourceTelemetry, create_block_transport,
    };
    use crate::dataflow::{HistoryConfig, PeakConfig, SmoothConfig, TimeDomainRequest};
    use crate::snapshot::TimeDomainReader;

    struct TestDataflow {
        _owner: Dataflow,
        handle: DataflowHandle,
    }

    impl TestDataflow {
        fn subscribe(
            &self,
            request: SpectrumRequest,
        ) -> Result<SpectrumSubscription, ControlError> {
            self.handle.subscribe_spectrum(request)
        }

        fn subscribe_shared(
            &self,
            request: SpectrumRequest,
            binding: SharedAnalysisId,
        ) -> Result<SpectrumSubscription, ControlError> {
            self.handle.subscribe_shared(request, binding)
        }

        fn subscribe_time(
            &self,
            request: TimeDomainRequest,
        ) -> Result<TimeDomainSubscription, ControlError> {
            self.handle.subscribe_time(request)
        }

        fn metrics(&self) -> RuntimeMetrics {
            self.handle.metrics()
        }
    }

    fn fixture(
        channels: u16,
        block_count: usize,
        max_block_frames: usize,
    ) -> (
        crate::audio::CaptureEndpoint,
        TestDataflow,
        Arc<SourceTelemetry>,
    ) {
        let layout = if channels == 1 {
            ChannelLayout::Mono
        } else if channels == 2 {
            ChannelLayout::Stereo
        } else {
            ChannelLayout::UnknownDiscrete
        };
        let descriptor = Arc::new(
            AudioDescriptor::new(SourceId(1), Generation(7), 48_000, channels, layout).unwrap(),
        );
        let telemetry = Arc::new(SourceTelemetry::new());
        let (mut capture, processing) = create_block_transport(
            channels as usize,
            block_count,
            max_block_frames,
            Arc::clone(&telemetry),
        )
        .unwrap();
        let (owner, handle) = Dataflow::spawn(descriptor, processing, Arc::clone(&telemetry));
        capture.set_worker(owner.worker_thread());
        (
            capture,
            TestDataflow {
                _owner: owner,
                handle,
            },
            telemetry,
        )
    }

    fn wait_for(
        reader: &SpectrumReader,
        revision: u64,
        minimum_frame: u64,
    ) -> Arc<SpectrumSnapshotSet> {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(snapshot) = reader.latest()
                && snapshot.config_revision == revision
                && snapshot.processed_through_frame >= minimum_frame
            {
                return snapshot;
            }
            assert!(Instant::now() < deadline, "timed out waiting for snapshot");
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn wait_spectrum_frontier(
        reader: &SpectrumReader,
        minimum_frame: u64,
    ) -> Arc<SpectrumSnapshotSet> {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(snapshot) = reader.latest()
                && snapshot.processed_through_frame >= minimum_frame
            {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for spectrum frontier"
            );
            thread::yield_now();
        }
    }

    fn wait_time_frontier(
        reader: &TimeDomainReader,
        minimum_frame: u64,
    ) -> Arc<TimeDomainSnapshot> {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(snapshot) = reader.latest()
                && snapshot.processed_through_frame >= minimum_frame
            {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for time-domain frontier {minimum_frame}, latest {:?}",
                reader
                    .latest()
                    .as_ref()
                    .map(|snapshot| snapshot.processed_through_frame)
            );
            thread::yield_now();
        }
    }

    fn wait_time_metrics(
        dataflow: &TestDataflow,
        expected_pcm: u64,
        expected_levels: u64,
        expected_publications: u64,
    ) -> RuntimeMetrics {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let metrics = dataflow.metrics();
            if metrics.time_pcm_frames == expected_pcm
                && metrics.time_level_frames == expected_levels
                && metrics.time_publications == expected_publications
            {
                return metrics;
            }
            assert!(
                Instant::now() < deadline,
                "time-domain counters did not settle: {metrics:?}"
            );
            thread::yield_now();
        }
    }

    fn wait_metrics(dataflow: &TestDataflow, expected: RuntimeMetrics) -> RuntimeMetrics {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let actual = dataflow.metrics();
            if actual.subscriptions == expected.subscriptions
                && actual.fft_branches == expected.fft_branches
                && actual.channel_branches == expected.channel_branches
                && actual.pipelines == expected.pipelines
            {
                return actual;
            }
            assert!(
                Instant::now() < deadline,
                "metrics did not settle: {actual:?}"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn retry_busy(mut operation: impl FnMut() -> Result<(), ControlError>) {
        loop {
            match operation() {
                Ok(()) => return,
                Err(ControlError::Busy) => thread::yield_now(),
                Err(error) => panic!("unexpected control error: {error}"),
            }
        }
    }

    fn mono_tone(frames: usize, frequency: f32) -> Vec<f32> {
        mono_tone_at(frames, frequency, 0.25)
    }

    fn mono_tone_at(frames: usize, frequency: f32, amplitude: f32) -> Vec<f32> {
        (0..frames)
            .map(|frame| amplitude * (2.0 * PI * frequency * frame as f32 / 48_000.0).sin())
            .collect()
    }

    fn stereo_tone(start_frame: u64, frames: usize) -> Vec<f32> {
        let mut samples = Vec::with_capacity(frames * 2);
        for frame in 0..frames {
            let phase = 2.0 * PI * 1_000.0 * (start_frame + frame as u64) as f32 / 48_000.0;
            samples.extend_from_slice(&[phase.sin() * 0.5, phase.cos() * 0.25]);
        }
        samples
    }

    fn spectrum_probe(dataflow: &TestDataflow) -> SpectrumSubscription {
        let request = SpectrumRequest::new(
            ChannelSelection::MonoAverage,
            FftConfig::new(32, 16, -160.0).unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match dataflow.handle.subscribe_spectrum(request) {
                Ok(subscription) => return subscription,
                Err(ControlError::Busy) => {
                    assert!(Instant::now() < deadline, "control queue stayed busy");
                    thread::yield_now();
                }
                Err(error) => panic!("unexpected spectrum probe error: {error}"),
            }
        }
    }

    fn block_worker(handle: &DataflowHandle) -> Arc<Barrier> {
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        assert!(matches!(
            handle.commands.try_send(Command::BlockWorkerForTest {
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            }),
            Ok(())
        ));
        handle.worker.unpark();
        entered.wait();
        release
    }

    #[test]
    fn identical_consumers_share_each_fft_frontier_and_reads_do_not_advance() {
        let (mut capture, dataflow, _) = fixture(1, 8, 4096);
        let request = SpectrumRequest::new(
            ChannelSelection::MonoAverage,
            FftConfig::new(32, 16, -160.0).unwrap(),
        );
        let first = dataflow.subscribe(request).unwrap();
        let second = dataflow.subscribe(request).unwrap();
        capture.write(&mono_tone(64, 3_000.0));
        let left = wait_for(&first.reader(), 1, 64);
        let right = wait_for(&second.reader(), 1, 64);
        assert_eq!(left.processed_through_frame, right.processed_through_frame);
        assert!(Arc::ptr_eq(&left.spectrum_dbfs, &right.spectrum_dbfs));
        let metrics = dataflow.metrics();
        assert_eq!(metrics.fft_evaluations, 3);
        assert_eq!(metrics.pipeline_evaluations, 3);
        assert_eq!(metrics.channel_branches, 1);
        for _ in 0..100 {
            assert_eq!(first.reader().latest().unwrap().result_version, 3);
        }
        assert_eq!(dataflow.metrics(), metrics);
    }

    #[test]
    fn changing_one_fft_forks_without_disturbing_the_other_consumer() {
        let (mut capture, dataflow, _) = fixture(1, 8, 128);
        let request = SpectrumRequest::new(
            ChannelSelection::MonoAverage,
            FftConfig::new(32, 16, -160.0).unwrap(),
        );
        let first = dataflow.subscribe(request).unwrap();
        let second = dataflow.subscribe(request).unwrap();
        capture.write(&mono_tone(64, 3_000.0));
        let before = wait_for(&second.reader(), 1, 64);
        first
            .controller()
            .set_fft(FftConfig::new(64, 32, -160.0).unwrap())
            .unwrap();
        capture.write(&mono_tone(128, 3_000.0));
        let changed = wait_for(&first.reader(), 2, 192);
        let unchanged = wait_for(&second.reader(), 1, 192);
        assert_eq!(changed.fft.window_size, 64);
        assert_eq!(unchanged.fft.window_size, 32);
        assert!(unchanged.result_version > before.result_version);
        let metrics = dataflow.metrics();
        assert_eq!(metrics.fft_branches, 2);
        assert_eq!(metrics.channel_branches, 1);
        assert_eq!(metrics.pipelines, 2);
    }

    #[test]
    fn shared_state_advances_once_and_private_reset_is_isolated() {
        let (mut capture, dataflow, _) = fixture(1, 8, 128);
        let mut request = SpectrumRequest::new(
            ChannelSelection::MonoAverage,
            FftConfig::new(32, 16, -160.0).unwrap(),
        );
        request.history = Some(HistoryConfig::new(1.0).unwrap());
        request.peak = Some(PeakConfig::new(20.0, 12.0).unwrap());
        let shared_a = dataflow
            .subscribe_shared(request, SharedAnalysisId(11))
            .unwrap();
        let shared_b = dataflow
            .subscribe_shared(request, SharedAnalysisId(11))
            .unwrap();
        capture.write(&mono_tone(64, 3_000.0));
        let a = wait_for(&shared_a.reader(), 1, 64);
        let b = wait_for(&shared_b.reader(), 1, 64);
        assert_eq!(a.history.len(), 3);
        assert!(Arc::ptr_eq(&a.history, &b.history));
        assert_eq!(dataflow.metrics().pipeline_evaluations, 3);
        assert!(a.peak_dbfs.is_some());
        assert_eq!(
            a.history.last().unwrap().processed_through_frame,
            a.processed_through_frame
        );

        drop(shared_a);
        wait_metrics(
            &dataflow,
            RuntimeMetrics {
                subscriptions: 1,
                fft_branches: 1,
                channel_branches: 1,
                pipelines: 1,
                ..RuntimeMetrics::default()
            },
        );
        capture.write(&mono_tone(16, 3_000.0));
        assert_eq!(wait_for(&shared_b.reader(), 1, 80).history.len(), 4);

        let private_a = dataflow.subscribe(request).unwrap();
        let private_b = dataflow.subscribe(request).unwrap();
        capture.write(&mono_tone(32, 3_000.0));
        wait_for(&private_a.reader(), 1, 112);
        let private_b_before = wait_for(&private_b.reader(), 1, 112);
        private_a.controller().reset_state().unwrap();
        capture.write(&mono_tone(48, 3_000.0));
        let reset_a = wait_for(&private_a.reader(), 1, 160);
        let private_b_after = wait_for(&private_b.reader(), 1, 160);
        assert!(reset_a.history.len() < private_b_after.history.len());
        assert!(private_b_after.history.len() > private_b_before.history.len());
    }

    #[test]
    fn history_continues_while_hidden_and_marks_explicit_pause_gap() {
        let (mut capture, dataflow, _) = fixture(1, 8, 128);
        let mut request = SpectrumRequest::new(
            ChannelSelection::MonoAverage,
            FftConfig::new(32, 16, -160.0).unwrap(),
        );
        request.smooth = Some(SmoothConfig::new(10.0, 100.0).unwrap());
        request.history = Some(HistoryConfig::new(1.0).unwrap());
        let subscription = dataflow.subscribe(request).unwrap();
        capture.write(&mono_tone(64, 3_000.0));
        let visible = wait_for(&subscription.reader(), 1, 64);
        subscription.controller().set_visible(false).unwrap();
        capture.write(&mono_tone(32, 3_000.0));
        let hidden = wait_for(&subscription.reader(), 1, 96);
        assert!(hidden.history.len() > visible.history.len());

        subscription.controller().set_analysis_paused(true).unwrap();
        capture.write(&mono_tone(64, 3_000.0));
        thread::sleep(Duration::from_millis(10));
        let paused_version = subscription.reader().latest().unwrap().result_version;
        subscription
            .controller()
            .set_analysis_paused(false)
            .unwrap();
        capture.write(&mono_tone(64, 3_000.0));
        let resumed = wait_for(&subscription.reader(), 1, 224);
        assert!(resumed.result_version > paused_version);
        assert!(
            resumed
                .history
                .iter()
                .any(|frame| frame.discontinuity_before)
        );
    }

    #[test]
    fn dropped_block_resets_continuous_state_and_retained_snapshot_survives_cleanup() {
        let (mut capture, dataflow, telemetry) = fixture(1, 1, 32);
        let mut request = SpectrumRequest::new(
            ChannelSelection::MonoAverage,
            FftConfig::new(32, 32, -160.0).unwrap(),
        );
        request.history = Some(HistoryConfig::new(1.0).unwrap());
        let subscription = dataflow.subscribe(request).unwrap();
        capture.write(&[0.0; 96]);
        let first = wait_for(&subscription.reader(), 1, 32);
        assert_eq!(telemetry.snapshot().dropped_blocks, 2);
        capture.write(&[0.0; 32]);
        let after_gap = wait_for(&subscription.reader(), 1, 128);
        assert!(after_gap.discontinuity_count > first.discontinuity_count);
        assert!(after_gap.history.last().unwrap().discontinuity_before);

        let retained = Arc::clone(&after_gap);
        drop(subscription);
        wait_metrics(
            &dataflow,
            RuntimeMetrics {
                subscriptions: 0,
                fft_branches: 0,
                channel_branches: 0,
                pipelines: 0,
                ..RuntimeMetrics::default()
            },
        );
        assert_eq!(retained.descriptor.generation, Generation(7));
        assert_eq!(retained.processed_through_frame, 128);
    }

    #[test]
    fn repeated_updates_replace_resources_instead_of_accumulating() {
        let (_capture, dataflow, _) = fixture(1, 8, 128);
        let subscription = dataflow.subscribe(SpectrumRequest::default()).unwrap();
        for index in 0..50 {
            let fft = if index % 2 == 0 {
                FftConfig::new(32, 16, -160.0).unwrap()
            } else {
                FftConfig::new(64, 32, -160.0).unwrap()
            };
            retry_busy(|| subscription.controller().set_fft(fft));
        }
        wait_metrics(
            &dataflow,
            RuntimeMetrics {
                subscriptions: 1,
                fft_branches: 1,
                channel_branches: 1,
                pipelines: 1,
                ..RuntimeMetrics::default()
            },
        );
    }

    #[test]
    fn pure_snapshot_demand_stops_when_hidden_and_restarts_asynchronously() {
        let (mut capture, dataflow, _) = fixture(1, 8, 128);
        let subscription = dataflow
            .subscribe(SpectrumRequest::new(
                ChannelSelection::MonoAverage,
                FftConfig::new(32, 16, -160.0).unwrap(),
            ))
            .unwrap();
        subscription.controller().set_visible(false).unwrap();
        wait_metrics(
            &dataflow,
            RuntimeMetrics {
                subscriptions: 1,
                fft_branches: 0,
                channel_branches: 0,
                pipelines: 1,
                ..RuntimeMetrics::default()
            },
        );
        capture.write(&mono_tone(64, 3_000.0));
        thread::sleep(Duration::from_millis(10));
        assert_eq!(dataflow.metrics().fft_evaluations, 0);
        assert!(subscription.reader().latest().is_none());

        subscription.controller().set_visible(true).unwrap();
        wait_metrics(
            &dataflow,
            RuntimeMetrics {
                subscriptions: 1,
                fft_branches: 1,
                channel_branches: 1,
                pipelines: 1,
                ..RuntimeMetrics::default()
            },
        );
        capture.write(&mono_tone(32, 3_000.0));
        assert_eq!(wait_for(&subscription.reader(), 1, 96).config_revision, 1);
    }

    #[test]
    fn shared_reset_has_shared_scope_and_local_update_forks_state() {
        let (mut capture, dataflow, _) = fixture(1, 8, 128);
        let mut request = SpectrumRequest::new(
            ChannelSelection::MonoAverage,
            FftConfig::new(32, 16, -160.0).unwrap(),
        );
        request.history = Some(HistoryConfig::new(1.0).unwrap());
        let first = dataflow
            .subscribe_shared(request, SharedAnalysisId(21))
            .unwrap();
        let second = dataflow
            .subscribe_shared(request, SharedAnalysisId(21))
            .unwrap();
        capture.write(&mono_tone(64, 3_000.0));
        wait_for(&first.reader(), 1, 64);

        first.controller().reset_state().unwrap();
        capture.write(&mono_tone(48, 3_000.0));
        let reset_first = wait_for(&first.reader(), 1, 112);
        let reset_second = wait_for(&second.reader(), 1, 112);
        assert_eq!(reset_first.history.len(), 2);
        assert!(Arc::ptr_eq(&reset_first.history, &reset_second.history));

        let mut changed = request;
        changed.gain_db = 6.0;
        first.controller().update_request(changed).unwrap();
        capture.write(&mono_tone(32, 3_000.0));
        let forked = wait_for(&first.reader(), 2, 144);
        let unchanged = wait_for(&second.reader(), 1, 144);
        assert!(forked.history.len() < unchanged.history.len());
        assert_eq!(dataflow.metrics().fft_branches, 1);
        assert_eq!(dataflow.metrics().pipelines, 2);
        let peak_bin = 2;
        assert!(
            (forked.spectrum_dbfs[peak_bin] - unchanged.spectrum_dbfs[peak_bin] - 6.0).abs()
                < 0.001
        );
    }

    #[test]
    fn one_shared_binding_rejects_different_state_contracts() {
        let (_capture, dataflow, _) = fixture(1, 8, 128);
        let mut request = SpectrumRequest::new(
            ChannelSelection::MonoAverage,
            FftConfig::new(32, 16, -160.0).unwrap(),
        );
        request.history = Some(HistoryConfig::new(1.0).unwrap());
        let _first = dataflow
            .subscribe_shared(request, SharedAnalysisId(31))
            .unwrap();
        request.gain_db = 1.0;
        let result = dataflow.subscribe_shared(request, SharedAnalysisId(31));
        assert!(matches!(
            result,
            Err(ControlError::SharedBindingMismatch(SharedAnalysisId(31)))
        ));
    }

    #[test]
    fn gain_change_preserves_upstream_smooth_and_peak_but_resets_history() {
        let (mut capture, dataflow, _) = fixture(1, 8, 128);
        let mut request = SpectrumRequest::new(
            ChannelSelection::MonoAverage,
            FftConfig::new(32, 16, -160.0).unwrap(),
        );
        request.smooth = Some(SmoothConfig::new(0.0, 1_000.0).unwrap());
        request.peak = Some(PeakConfig::new(1_000.0, 20.0).unwrap());
        request.history = Some(HistoryConfig::new(1.0).unwrap());
        let changed = dataflow.subscribe(request).unwrap();
        let control = dataflow.subscribe(request).unwrap();
        capture.write(&mono_tone_at(64, 3_000.0, 0.5));
        wait_for(&changed.reader(), 1, 64);
        wait_for(&control.reader(), 1, 64);

        let mut changed_request = request;
        changed_request.gain_db = 6.0;
        changed
            .controller()
            .update_request(changed_request)
            .unwrap();
        capture.write(&mono_tone_at(16, 3_000.0, 0.05));
        let changed_after = wait_for(&changed.reader(), 2, 80);
        let control_after = wait_for(&control.reader(), 1, 80);
        let target_bin = 2;
        assert!(
            (changed_after.peak_dbfs.as_ref().unwrap()[target_bin]
                - control_after.peak_dbfs.as_ref().unwrap()[target_bin])
                .abs()
                < 0.000_1
        );
        assert!(
            (changed_after.spectrum_dbfs[target_bin]
                - control_after.spectrum_dbfs[target_bin]
                - 6.0)
                .abs()
                < 0.000_1
        );
        assert_eq!(changed_after.history.len(), 1);
        assert_eq!(control_after.history.len(), 4);
    }

    #[test]
    fn reset_on_pure_subscription_is_local_no_op() {
        let (mut capture, dataflow, _) = fixture(1, 8, 128);
        let request = SpectrumRequest::new(
            ChannelSelection::MonoAverage,
            FftConfig::new(32, 16, -160.0).unwrap(),
        );
        let first = dataflow.subscribe(request).unwrap();
        let second = dataflow.subscribe(request).unwrap();
        capture.write(&mono_tone(64, 3_000.0));
        wait_for(&first.reader(), 1, 64);
        wait_for(&second.reader(), 1, 64);

        first.controller().reset_state().unwrap();
        capture.write(&mono_tone(16, 3_000.0));
        let first_after = wait_for(&first.reader(), 1, 80);
        let second_after = wait_for(&second.reader(), 1, 80);
        assert_eq!(first_after.processed_through_frame, 80);
        assert_eq!(second_after.processed_through_frame, 80);
        assert!(Arc::ptr_eq(
            &first_after.spectrum_dbfs,
            &second_after.spectrum_dbfs
        ));
    }

    #[test]
    fn concurrent_field_setters_preserve_both_successful_updates() {
        const ROUNDS: usize = 2_000;
        let (_capture, dataflow, _) = fixture(2, 8, 128);
        let base = SpectrumRequest::new(
            ChannelSelection::Left,
            FftConfig::new(32, 16, -160.0).unwrap(),
        );
        let subscription = dataflow.subscribe(base).unwrap();
        let controller = subscription.controller();
        let channel_controller = controller.clone();
        let fft_controller = controller.clone();
        let start = Barrier::new(3);
        let finish = Barrier::new(3);
        let failures = AtomicUsize::new(0);
        let target_fft = FftConfig::new(64, 32, -160.0).unwrap();

        thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..ROUNDS {
                    start.wait();
                    retry_busy(|| channel_controller.set_channel(ChannelSelection::Right));
                    finish.wait();
                }
            });
            scope.spawn(|| {
                for _ in 0..ROUNDS {
                    start.wait();
                    retry_busy(|| fft_controller.set_fft(target_fft));
                    finish.wait();
                }
            });
            for _ in 0..ROUNDS {
                retry_busy(|| controller.update_request(base));
                start.wait();
                finish.wait();
                let request = controller.request().unwrap();
                if request.channel != ChannelSelection::Right || request.fft != target_fft {
                    failures.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
        assert_eq!(failures.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn joining_shared_analysis_receives_current_snapshot_without_new_input() {
        let (mut capture, dataflow, _) = fixture(1, 8, 128);
        let mut request = SpectrumRequest::new(
            ChannelSelection::MonoAverage,
            FftConfig::new(32, 16, -160.0).unwrap(),
        );
        request.history = Some(HistoryConfig::new(1.0).unwrap());
        let first = dataflow
            .subscribe_shared(request, SharedAnalysisId(41))
            .unwrap();
        capture.write(&mono_tone(64, 3_000.0));
        let existing = wait_for(&first.reader(), 1, 64);
        assert_eq!(existing.history.len(), 3);

        let joined = dataflow
            .subscribe_shared(request, SharedAnalysisId(41))
            .unwrap();
        let current = joined
            .reader()
            .latest()
            .expect("joining a shared analysis reuses its current result");
        assert_eq!(current.processed_through_frame, 64);
        assert_eq!(current.history.len(), 3);
        assert!(Arc::ptr_eq(&existing.history, &current.history));
    }

    #[test]
    fn sustained_control_load_does_not_starve_audio() {
        let (mut capture, dataflow, _) = fixture(1, 128, 32);
        let mut request = SpectrumRequest::new(
            ChannelSelection::MonoAverage,
            FftConfig::new(32, 16, -160.0).unwrap(),
        );
        request.history = Some(HistoryConfig::new(1.0).unwrap());
        let subscription = dataflow.subscribe(request).unwrap();
        let controller = subscription.controller();
        let stop = AtomicBool::new(false);
        let processed = thread::scope(|scope| {
            scope.spawn(|| {
                let mut visible = false;
                while !stop.load(Ordering::Acquire) {
                    match controller.set_visible(visible) {
                        Ok(()) => visible = !visible,
                        Err(ControlError::Busy) => {}
                        Err(error) => panic!("unexpected control error: {error}"),
                    }
                }
            });
            capture.write(&mono_tone(4_096, 3_000.0));
            let deadline = Instant::now() + Duration::from_secs(2);
            let processed = loop {
                if let Some(snapshot) = subscription.reader().latest()
                    && snapshot.processed_through_frame >= 4_096
                {
                    break true;
                }
                if Instant::now() >= deadline {
                    break false;
                }
                thread::yield_now();
            };
            stop.store(true, Ordering::Release);
            processed
        });
        assert!(processed, "control traffic starved queued audio blocks");
    }

    #[test]
    fn full_control_queue_reports_busy_through_public_controller() {
        let (_capture, dataflow, _) = fixture(1, 8, 32);
        let subscription = dataflow
            .subscribe(SpectrumRequest::new(
                ChannelSelection::MonoAverage,
                FftConfig::new(32, 16, -160.0).unwrap(),
            ))
            .unwrap();
        let controller = subscription.controller();
        let release = block_worker(&dataflow.handle);
        for index in 0..CONTROL_QUEUE_CAPACITY {
            assert_eq!(
                controller.set_visible(index % 2 == 0),
                Ok(()),
                "command {index} fits in the bounded queue"
            );
        }
        assert_eq!(controller.set_visible(true), Err(ControlError::Busy));
        release.wait();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match controller.set_visible(true) {
                Ok(()) => break,
                Err(ControlError::Busy) => {
                    assert!(
                        Instant::now() < deadline,
                        "queue never drained after release"
                    );
                    thread::yield_now();
                }
                Err(error) => panic!("unexpected control error: {error}"),
            }
        }
    }

    #[test]
    fn shared_analysis_reactivation_marks_history_discontinuity() {
        let (mut capture, dataflow, _) = fixture(1, 8, 128);
        let mut request = SpectrumRequest::new(
            ChannelSelection::MonoAverage,
            FftConfig::new(32, 16, -160.0).unwrap(),
        );
        request.history = Some(HistoryConfig::new(1.0).unwrap());
        let first = dataflow
            .subscribe_shared(request, SharedAnalysisId(42))
            .unwrap();
        capture.write(&mono_tone(64, 3_000.0));
        let before_pause = wait_for(&first.reader(), 1, 64);
        assert_eq!(before_pause.history.len(), 3);

        first.controller().set_analysis_paused(true).unwrap();
        wait_metrics(
            &dataflow,
            RuntimeMetrics {
                subscriptions: 1,
                pipelines: 1,
                ..RuntimeMetrics::default()
            },
        );
        let joined = dataflow
            .subscribe_shared(request, SharedAnalysisId(42))
            .unwrap();
        let inherited = joined
            .reader()
            .latest()
            .expect("reactivating subscriber receives retained history");
        assert_eq!(inherited.processed_through_frame, 64);
        assert_eq!(inherited.history.len(), 3);

        capture.write(&mono_tone(32, 3_000.0));
        let reactivated = wait_for(&joined.reader(), 1, 96);
        assert_eq!(reactivated.history.len(), 4);
        assert!(
            reactivated.history.last().unwrap().discontinuity_before,
            "reactivated shared state must not imply continuous analysis"
        );
    }
    #[test]
    fn time_demand_matrix_is_lazy_and_closes_independently() {
        let (mut capture, dataflow, telemetry) = fixture(2, 128, 16);
        let probe = spectrum_probe(&dataflow);

        capture.write(&stereo_tone(0, 1_600));
        wait_spectrum_frontier(&probe.reader(), 1_600);
        assert_eq!(dataflow.metrics().time_pcm_frames, 0);
        assert_eq!(dataflow.metrics().time_level_frames, 0);
        assert_eq!(dataflow.metrics().time_publications, 0);

        let latest = dataflow.subscribe_time(TimeDomainRequest::Latest).unwrap();
        capture.write(&stereo_tone(1_600, 1_600));
        let latest_snapshot = wait_time_frontier(&latest.reader(), 3_200);
        assert_eq!(latest_snapshot.result_version, 1);
        assert_eq!(latest_snapshot.chunks.len(), 1);
        assert_eq!(latest_snapshot.chunks[0].start_frame, 1_600);
        assert_eq!(latest_snapshot.chunks[0].frame_count, 1_600);
        assert!(latest_snapshot.levels.is_none());
        wait_time_metrics(&dataflow, 1_600, 0, 1);

        drop(latest);
        let history = dataflow
            .subscribe_time(TimeDomainRequest::History { seconds: 0.01 })
            .unwrap();
        capture.write(&stereo_tone(3_200, 1_600));
        let history_snapshot = wait_time_frontier(&history.reader(), 4_800);
        assert_eq!(history_snapshot.chunks.len(), 1);
        assert_eq!(history_snapshot.chunks[0].start_frame, 4_320);
        assert_eq!(history_snapshot.chunks[0].frame_count, 480);
        assert_eq!(history_snapshot.chunks[0].interleaved.len(), 960);
        assert!(history_snapshot.levels.is_none());
        wait_time_metrics(&dataflow, 3_200, 0, 2);

        drop(history);
        let levels = dataflow.subscribe_time(TimeDomainRequest::Levels).unwrap();
        capture.write(&stereo_tone(4_800, 1_600));
        let levels_snapshot = wait_time_frontier(&levels.reader(), 6_400);
        assert!(levels_snapshot.chunks.is_empty());
        let levels_data = levels_snapshot.levels.as_ref().unwrap();
        assert_eq!(levels_data.momentary_lufs.len(), 2);
        assert!(
            levels_data
                .momentary_lufs
                .iter()
                .all(|level| *level > -60.0)
        );
        assert!(levels_data.sample_peaks[0] > levels_data.sample_peaks[1]);
        wait_time_metrics(&dataflow, 3_200, 1_600, 3);

        drop(levels);
        let drain_probe = spectrum_probe(&dataflow);
        capture.write(&stereo_tone(6_400, 1_600));
        wait_spectrum_frontier(&drain_probe.reader(), 8_000);
        assert_eq!(
            (
                dataflow.metrics().time_pcm_frames,
                dataflow.metrics().time_level_frames,
                dataflow.metrics().time_publications,
            ),
            (3_200, 1_600, 3)
        );
        assert_eq!(telemetry.snapshot().dropped_blocks, 0);
    }

    #[test]
    fn mixed_time_demands_share_one_pcm_interval_and_interval_peak() {
        let (mut capture, dataflow, _) = fixture(2, 128, 16);
        let latest = dataflow.subscribe_time(TimeDomainRequest::Latest).unwrap();
        let history = dataflow
            .subscribe_time(TimeDomainRequest::History { seconds: 1.0 })
            .unwrap();
        let levels = dataflow.subscribe_time(TimeDomainRequest::Levels).unwrap();
        let mut samples = vec![0.01; 1_600 * 2];
        samples[10 * 2] = 0.95;
        samples[10 * 2 + 1] = 0.4;
        capture.write(&samples);

        let latest_snapshot = wait_time_frontier(&latest.reader(), 1_600);
        let history_snapshot = wait_time_frontier(&history.reader(), 1_600);
        let levels_snapshot = wait_time_frontier(&levels.reader(), 1_600);
        assert_eq!(latest_snapshot.chunks.len(), 1);
        assert_eq!(history_snapshot.chunks.len(), 1);
        assert_eq!(latest_snapshot.chunks[0].frame_count, 1_600);
        assert_eq!(latest_snapshot.chunks[0].interleaved[20..22], [0.95, 0.4]);
        assert_eq!(latest_snapshot.chunks[0].interleaved[22..24], [0.01, 0.01]);
        assert!(Arc::ptr_eq(
            &latest_snapshot.chunks[0],
            &history_snapshot.chunks[0]
        ));
        assert!(latest_snapshot.levels.is_none());
        assert!(history_snapshot.levels.is_none());
        assert!(levels_snapshot.chunks.is_empty());
        let level = levels_snapshot.levels.as_ref().unwrap();
        assert!((level.sample_peaks[0] - 0.95).abs() < f32::EPSILON);
        assert!((level.sample_peaks[1] - 0.4).abs() < f32::EPSILON);
        wait_time_metrics(&dataflow, 1_600, 1_600, 3);
    }

    #[test]
    fn history_subscribers_clip_their_first_shared_interval_at_join() {
        let (mut capture, dataflow, _) = fixture(2, 128, 16);
        let probe = spectrum_probe(&dataflow);
        let first = dataflow
            .subscribe_time(TimeDomainRequest::History { seconds: 2.0 })
            .unwrap();

        capture.write(&stereo_tone(0, 800));
        wait_spectrum_frontier(&probe.reader(), 800);
        let joined = dataflow
            .subscribe_time(TimeDomainRequest::History { seconds: 2.0 })
            .unwrap();
        capture.write(&stereo_tone(800, 800));

        let first_snapshot = wait_time_frontier(&first.reader(), 1_600);
        let joined_snapshot = wait_time_frontier(&joined.reader(), 1_600);
        assert_eq!(first_snapshot.chunks.len(), 1);
        assert_eq!(first_snapshot.chunks[0].start_frame, 0);
        assert_eq!(first_snapshot.chunks[0].frame_count, 1_600);
        assert_eq!(joined_snapshot.chunks.len(), 1);
        assert_eq!(joined_snapshot.chunks[0].start_frame, 800);
        assert_eq!(joined_snapshot.chunks[0].frame_count, 800);
        assert_eq!(joined_snapshot.chunks[0].interleaved.len(), 1_600);
        assert!(!Arc::ptr_eq(
            &first_snapshot.chunks[0],
            &joined_snapshot.chunks[0]
        ));
    }

    #[test]
    fn levels_subscribers_start_private_windows_at_their_join_frontier() {
        let (mut capture, dataflow, _) = fixture(2, 128, 16);
        let probe = spectrum_probe(&dataflow);
        let first = dataflow.subscribe_time(TimeDomainRequest::Levels).unwrap();

        for start_frame in (0_u64..19_200).step_by(800) {
            capture.write(&stereo_tone(start_frame, 800));
            wait_spectrum_frontier(&probe.reader(), start_frame + 800);
        }
        let before_join = wait_time_frontier(&first.reader(), 19_200);
        assert!(before_join.levels.as_ref().unwrap().momentary_lufs[0] > -60.0);
        let joined = dataflow.subscribe_time(TimeDomainRequest::Levels).unwrap();
        let silence = vec![0.0; 800 * 2];
        capture.write(&silence);
        wait_spectrum_frontier(&probe.reader(), 20_000);
        capture.write(&silence);
        wait_spectrum_frontier(&probe.reader(), 20_800);

        let first_after = wait_time_frontier(&first.reader(), 20_800);
        let joined_after = wait_time_frontier(&joined.reader(), 20_800);
        let first_levels = first_after.levels.as_ref().unwrap();
        let joined_levels = joined_after.levels.as_ref().unwrap();
        assert!(first_levels.momentary_lufs[0] > joined_levels.momentary_lufs[0]);
        assert_eq!(joined_levels.momentary_lufs[0], -60.0);
        assert_eq!(joined_levels.sample_peaks.as_ref(), &[0.0, 0.0]);
        wait_time_metrics(&dataflow, 0, 22_400, 14);
    }

    #[test]
    fn partial_time_demands_and_last_close_preserve_only_live_analysis() {
        let (mut capture, dataflow, _) = fixture(2, 128, 16);
        let probe = spectrum_probe(&dataflow);
        let levels = dataflow.subscribe_time(TimeDomainRequest::Levels).unwrap();
        capture.write(&stereo_tone(0, 800));
        wait_spectrum_frontier(&probe.reader(), 800);
        assert!(levels.reader().latest().is_none());

        let latest = dataflow.subscribe_time(TimeDomainRequest::Latest).unwrap();
        capture.write(&stereo_tone(800, 800));
        let first_interval = wait_time_frontier(&latest.reader(), 1_600);
        assert_eq!(first_interval.chunks[0].start_frame, 800);
        assert_eq!(first_interval.chunks[0].frame_count, 800);
        assert!(first_interval.levels.is_none());
        // Each subscription's slot is written in turn; wait for this one as well.
        let levels_interval = wait_time_frontier(&levels.reader(), 1_600);
        assert!(levels_interval.levels.is_some());
        wait_time_metrics(&dataflow, 800, 1_600, 2);

        drop(latest);
        let _removal_barrier = spectrum_probe(&dataflow);
        capture.write(&stereo_tone(1_600, 1_600));
        let continuing_levels = wait_time_frontier(&levels.reader(), 3_200);
        assert!(continuing_levels.levels.as_ref().unwrap().momentary_lufs[0] > -60.0);
        wait_time_metrics(&dataflow, 800, 3_200, 3);

        let latest = dataflow.subscribe_time(TimeDomainRequest::Latest).unwrap();
        capture.write(&stereo_tone(3_200, 1_600));
        let rejoined_pcm = wait_time_frontier(&latest.reader(), 4_800);
        assert_eq!(rejoined_pcm.chunks[0].start_frame, 3_200);
        assert_eq!(rejoined_pcm.chunks[0].frame_count, 1_600);
        wait_time_metrics(&dataflow, 2_400, 4_800, 5);

        drop(levels);
        let _level_removal_barrier = spectrum_probe(&dataflow);
        capture.write(&stereo_tone(4_800, 1_600));
        let pcm_only = wait_time_frontier(&latest.reader(), 6_400);
        assert_eq!(pcm_only.chunks[0].start_frame, 4_800);
        assert_eq!(pcm_only.chunks[0].frame_count, 1_600);
        wait_time_metrics(&dataflow, 4_000, 4_800, 6);

        drop(latest);
        let no_demand_probe = spectrum_probe(&dataflow);
        capture.write(&stereo_tone(6_400, 1_600));
        wait_spectrum_frontier(&no_demand_probe.reader(), 8_000);
        wait_time_metrics(&dataflow, 4_000, 4_800, 6);

        let reopened = dataflow.subscribe_time(TimeDomainRequest::Levels).unwrap();
        capture.write(&vec![0.0; 1_600 * 2]);
        let fresh_levels = wait_time_frontier(&reopened.reader(), 9_600);
        assert_eq!(
            fresh_levels
                .levels
                .as_ref()
                .unwrap()
                .momentary_lufs
                .as_ref(),
            &[-60.0, -60.0]
        );
        wait_time_metrics(&dataflow, 4_000, 6_400, 7);
    }

    #[test]
    fn time_history_keeps_the_latest_ten_seconds_without_drawing() {
        let (mut capture, dataflow, telemetry) = fixture(2, 8, 1_600);
        let history = dataflow
            .subscribe_time(TimeDomainRequest::History { seconds: 10.0 })
            .unwrap();
        let samples = vec![0.1_f32; 1_600 * 2];
        for cycle in 1..=330 {
            capture.write(&samples);
            wait_time_frontier(&history.reader(), cycle * 1_600);
        }
        let retained = history.reader().latest().unwrap();
        assert_eq!(retained.processed_through_frame, 528_000);
        assert_eq!(retained.chunks[0].start_frame, 48_000);
        assert_eq!(
            retained
                .chunks
                .iter()
                .map(|chunk| chunk.frame_count)
                .sum::<usize>(),
            480_000
        );
        assert_eq!(telemetry.snapshot().dropped_blocks, 0);
    }

    #[test]
    fn history_discards_old_pcm_outside_source_time_after_dropped_blocks() {
        let (mut capture, dataflow, telemetry) = fixture(2, 1, 1_600);
        let probe = spectrum_probe(&dataflow);
        let history = dataflow
            .subscribe_time(TimeDomainRequest::History { seconds: 0.05 })
            .unwrap();
        capture.write(&stereo_tone(0, 1_600));
        wait_time_frontier(&history.reader(), 1_600);
        wait_spectrum_frontier(&probe.reader(), 1_600);

        let release = block_worker(&dataflow.handle);
        capture.write(&stereo_tone(1_600, 1_600));
        for start_frame in (3_200..80_000).step_by(1_600) {
            capture.write(&stereo_tone(start_frame, 1_600));
        }
        assert_eq!(telemetry.snapshot().dropped_blocks, 48);
        release.wait();
        wait_spectrum_frontier(&probe.reader(), 3_200);

        capture.write(&stereo_tone(80_000, 1_600));
        let snapshot = wait_time_frontier(&history.reader(), 81_600);
        assert_eq!(snapshot.chunks.len(), 1);
        assert_eq!(snapshot.chunks[0].start_frame, 80_000);
        assert_eq!(snapshot.chunks[0].frame_count, 1_600);
        assert!(snapshot.chunks[0].discontinuity_before);
    }

    #[test]
    fn sequence_gaps_and_source_boundaries_reset_time_state() {
        let (mut capture, dataflow, telemetry) = fixture(2, 1, 1_600);
        let probe = spectrum_probe(&dataflow);
        let history = dataflow
            .subscribe_time(TimeDomainRequest::History { seconds: 10.0 })
            .unwrap();
        capture.write(&stereo_tone(0, 1_600));
        let before_gap = wait_time_frontier(&history.reader(), 1_600);
        wait_spectrum_frontier(&probe.reader(), 1_600);
        assert_eq!(before_gap.chunks.len(), 1);

        let release = block_worker(&dataflow.handle);
        capture.write(&stereo_tone(1_600, 1_600));
        capture.write(&stereo_tone(3_200, 1_600));
        capture.write(&stereo_tone(4_800, 1_600));
        assert_eq!(telemetry.snapshot().dropped_blocks, 2);
        release.wait();
        wait_spectrum_frontier(&probe.reader(), 3_200);
        capture.write(&stereo_tone(6_400, 1_600));
        let after_sequence_gap = wait_time_frontier(&history.reader(), 8_000);
        assert_eq!(after_sequence_gap.chunks.len(), 3);
        assert_eq!(after_sequence_gap.chunks[1].start_frame, 1_600);
        assert_eq!(after_sequence_gap.chunks[1].frame_count, 1_600);
        assert_eq!(after_sequence_gap.chunks[2].start_frame, 6_400);
        assert!(after_sequence_gap.chunks[2].discontinuity_before);
        assert!(after_sequence_gap.discontinuity_count > before_gap.discontinuity_count);

        let _processed_barrier = spectrum_probe(&dataflow);
        let continuity = telemetry.advance_continuity();
        retry_busy(|| dataflow.handle.resume_at(continuity));
        capture.write(&stereo_tone(8_000, 1_600));
        let after_source_boundary = wait_time_frontier(&history.reader(), 9_600);
        assert!(
            after_source_boundary
                .chunks
                .last()
                .unwrap()
                .discontinuity_before
        );
        let settled = wait_time_metrics(&dataflow, 6_400, 0, 4);
        assert_eq!(dataflow.metrics(), settled);
    }

    #[test]
    fn invalid_and_stopped_time_requests_fail_without_creating_demand() {
        let (capture, dataflow, _) = fixture(2, 8, 128);
        for seconds in [f32::NAN, 0.0, -1.0, 10.01, f32::INFINITY] {
            assert!(matches!(
                dataflow.subscribe_time(TimeDomainRequest::History { seconds }),
                Err(ControlError::InvalidTimeDomainRequest(
                    TimeDomainRequestError::InvalidHistoryDuration
                ))
            ));
        }
        let spectrum = SpectrumRequest {
            history: Some(HistoryConfig::new(11.0).unwrap()),
            ..SpectrumRequest::default()
        };
        drop(dataflow.subscribe(spectrum).unwrap());
        assert_eq!(dataflow.metrics().time_publications, 0);

        let TestDataflow {
            _owner: owner,
            handle,
        } = dataflow;
        drop(capture);
        drop(owner);
        assert!(matches!(
            handle.subscribe_time(TimeDomainRequest::Latest),
            Err(ControlError::Stopped)
        ));
        assert!(matches!(
            handle.subscribe_spectrum(SpectrumRequest::default()),
            Err(ControlError::Stopped)
        ));
    }

    #[test]
    fn repeated_time_subscription_cycles_release_old_snapshots_and_stop_work() {
        let (mut capture, dataflow, _) = fixture(2, 8, 1_600);
        let probe = spectrum_probe(&dataflow);
        for cycle in 0..16 {
            let subscription = dataflow.subscribe_time(TimeDomainRequest::Latest).unwrap();
            let reader = subscription.reader();
            capture.write(&vec![0.1_f32; 1_600 * 2]);
            let snapshot = wait_time_frontier(&reader, (cycle * 2 + 1) * 1_600);
            let weak = Arc::downgrade(&snapshot);
            drop(subscription);
            let _removal_barrier = spectrum_probe(&dataflow);
            drop(reader);
            drop(snapshot);
            assert!(
                weak.upgrade().is_none(),
                "a closed cycle retained a PCM snapshot"
            );
            let pcm_before = dataflow.metrics().time_pcm_frames;
            capture.write(&vec![0.0_f32; 1_600 * 2]);
            wait_spectrum_frontier(&probe.reader(), (cycle * 2 + 2) * 1_600);
            assert_eq!(dataflow.metrics().time_pcm_frames, pcm_before);
        }
    }

    #[test]
    fn time_subscription_drop_reliably_removes_from_a_full_control_queue() {
        let (mut capture, dataflow, _) = fixture(2, 128, 16);
        let subscription = dataflow.subscribe_time(TimeDomainRequest::Latest).unwrap();
        let release = block_worker(&dataflow.handle);
        for _ in 0..CONTROL_QUEUE_CAPACITY {
            assert!(matches!(
                dataflow
                    .handle
                    .commands
                    .try_send(Command::SetVisible(u64::MAX, false)),
                Ok(())
            ));
        }

        let (dropped_sender, dropped_receiver) = std::sync::mpsc::channel();
        let drop_thread = thread::spawn(move || {
            drop(subscription);
            let _ = dropped_sender.send(());
        });
        release.wait();
        dropped_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("full-queue subscription drop must finish");
        drop_thread.join().unwrap();

        let probe = spectrum_probe(&dataflow);
        capture.write(&stereo_tone(0, 1_600));
        wait_spectrum_frontier(&probe.reader(), 1_600);
        assert_eq!(
            (
                dataflow.metrics().time_pcm_frames,
                dataflow.metrics().time_level_frames,
                dataflow.metrics().time_publications,
            ),
            (0, 0, 0)
        );
    }
}
