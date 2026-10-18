//! Worker thread, control path and the handles Modules hold.

use std::{
    error::Error,
    fmt,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle, Thread},
    time::{Duration, Instant},
};

use arc_swap::ArcSwapOption;

use crate::{
    audio::{AudioDescriptor, ProcessingEndpoint, SourceTelemetry},
    snapshot::DataReader,
};

use super::{
    engine::{Applied, Engine, Metrics, RuntimeMetrics},
    spec::{DataSpec, SharedAnalysisId, SpecError},
    store::{SharedStore, Store},
};

const CONTROL_QUEUE_CAPACITY: usize = 16;
const COMMAND_BUDGET_PER_TICK: usize = 16;

/// Called on the worker thread after a processed block published new results. Lets
/// a UI sleep until there is something new to draw. It must be cheap and must not
/// block; it never runs in the audio callback.
pub type PublicationWaker = Box<dyn Fn() + Send + Sync>;

type Response<T> = SyncSender<Result<T, ControlError>>;

enum Command {
    Subscribe {
        id: u64,
        spec: DataSpec,
        response: Response<Applied>,
    },
    Update {
        id: u64,
        spec: DataSpec,
        response: Response<Applied>,
    },
    UpdateShared {
        binding: SharedAnalysisId,
        specs: Vec<(u64, DataSpec)>,
        response: Response<Vec<Applied>>,
    },
    SetVisible(u64, bool),
    SetAnalysisPaused(u64, bool),
    ResetState(u64),
    Remove(u64),
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
    InvalidSpec(SpecError),
}

impl fmt::Display for ControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("audio engine busy; try again"),
            Self::Stopped => f.write_str("dataflow worker has stopped"),
            Self::SubscriptionClosed => f.write_str("data subscription is closed"),
            Self::SharedBindingMismatch(id) => {
                write!(f, "shared analysis {} resolves to a different output", id.0)
            }
            Self::InvalidSpec(error) => write!(f, "invalid data spec: {error}"),
        }
    }
}

impl Error for ControlError {}

impl From<SpecError> for ControlError {
    fn from(error: SpecError) -> Self {
        Self::InvalidSpec(error)
    }
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Owner of the worker thread; dropping it stops and joins the worker.
pub(crate) struct Dataflow {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

/// Cloneable capability to create subscriptions; does not keep the worker alive.
#[derive(Clone)]
pub struct DataflowHandle {
    commands: SyncSender<Command>,
    worker: Thread,
    stopped: Arc<AtomicBool>,
    next_subscription_id: Arc<AtomicU64>,
    next_binding_id: Arc<AtomicU64>,
    descriptor: Arc<AudioDescriptor>,
    telemetry: Arc<SourceTelemetry>,
    store: SharedStore,
    metrics: Arc<Metrics>,
    waker: Arc<ArcSwapOption<PublicationWaker>>,
    /// Live controllers, for edits of a whole binding.
    controls: Arc<Mutex<Vec<Weak<ControlShared>>>>,
    #[cfg(any(test, feature = "automation"))]
    processed_through_frame: Arc<AtomicU64>,
    #[cfg(any(test, feature = "automation"))]
    probe: Arc<crate::probe::WorkerProbe>,
}

/// Owns one subscription's demand; dropping it removes the demand.
pub struct DataSubscription {
    controller: DataController,
    reader: DataReader,
}

/// Cloneable control of one subscription; does not add demand.
#[derive(Clone)]
pub struct DataController {
    inner: Arc<ControlShared>,
}

struct ControlShared {
    id: u64,
    commands: SyncSender<Command>,
    worker: Thread,
    stopped: Arc<AtomicBool>,
    descriptor: Arc<AudioDescriptor>,
    /// The applied spec and its revision; held while an update is in flight so
    /// concurrent edits of one subscription serialize.
    applied: Mutex<Applied>,
    active: AtomicBool,
}

impl ControlShared {
    fn ensure_active(&self) -> Result<(), ControlError> {
        if !self.active.load(Ordering::Acquire) {
            Err(ControlError::SubscriptionClosed)
        } else if self.stopped.load(Ordering::Acquire) {
            Err(ControlError::Stopped)
        } else {
            Ok(())
        }
    }

    fn try_send(&self, command: Command) -> Result<(), ControlError> {
        send(&self.commands, &self.worker, &self.stopped, command)
    }
}

fn send(
    commands: &SyncSender<Command>,
    worker: &Thread,
    stopped: &AtomicBool,
    command: Command,
) -> Result<(), ControlError> {
    if stopped.load(Ordering::Acquire) {
        return Err(ControlError::Stopped);
    }
    match commands.try_send(command) {
        Ok(()) => {
            worker.unpark();
            Ok(())
        }
        Err(TrySendError::Full(_)) => Err(ControlError::Busy),
        Err(TrySendError::Disconnected(_)) => Err(ControlError::Stopped),
    }
}

/// Send and wait for the worker's answer. Control path only: never call from the
/// audio callback or the worker.
fn request<T>(
    commands: &SyncSender<Command>,
    worker: &Thread,
    stopped: &AtomicBool,
    make: impl FnOnce(Response<T>) -> Command,
) -> Result<T, ControlError> {
    let (response, answer) = sync_channel(1);
    send(commands, worker, stopped, make(response))?;
    answer.recv().map_err(|_| ControlError::Stopped)?
}

impl DataSubscription {
    pub fn reader(&self) -> DataReader {
        self.reader.clone()
    }

    pub fn controller(&self) -> DataController {
        self.controller.clone()
    }
}

impl Drop for DataSubscription {
    fn drop(&mut self) {
        let inner = &self.controller.inner;
        inner.active.store(false, Ordering::Release);
        if !inner.stopped.load(Ordering::Acquire) {
            // Blocking send: a full queue must not leave a permanent demand.
            let _ = inner.commands.send(Command::Remove(inner.id));
            inner.worker.unpark();
        }
    }
}

impl DataController {
    /// The spec as the runtime applied it.
    pub fn spec(&self) -> DataSpec {
        lock(&self.inner.applied).spec.clone()
    }

    pub fn with_spec<T>(&self, read: impl FnOnce(&DataSpec) -> T) -> T {
        read(&lock(&self.inner.applied).spec)
    }

    pub fn revision(&self) -> u64 {
        lock(&self.inner.applied).revision
    }

    /// Edit this subscription's spec and wait until the runtime switched to it.
    /// Unchanged nodes keep their state; changed nodes and their stateful
    /// descendants start fresh. If a bound output would change while other members
    /// keep the binding, this subscription forks to a private copy. A failed
    /// update leaves the subscription unchanged.
    pub fn update(&self, edit: impl FnOnce(&mut DataSpec)) -> Result<(), ControlError> {
        self.inner.ensure_active()?;
        let mut applied = lock(&self.inner.applied);
        let mut spec = applied.spec.clone();
        edit(&mut spec);
        let spec = spec.validated(&self.inner.descriptor)?;
        if spec == applied.spec {
            return Ok(());
        }
        let id = self.inner.id;
        *applied = request(
            &self.inner.commands,
            &self.inner.worker,
            &self.inner.stopped,
            |response| Command::Update { id, spec, response },
        )?;
        Ok(())
    }

    /// Only stops or restarts work that serves this subscription's Latest outputs
    /// without state; continuous analysis and History keep running.
    pub fn set_visible(&self, visible: bool) -> Result<(), ControlError> {
        self.inner.ensure_active()?;
        // Reliable send: losing "visible again" would leave the view empty.
        self.inner
            .commands
            .send(Command::SetVisible(self.inner.id, visible))
            .map_err(|_| ControlError::Stopped)?;
        self.inner.worker.unpark();
        Ok(())
    }

    /// Stop recording for this subscription; resuming reads from the resume point
    /// and marks a gap.
    pub fn set_analysis_paused(&self, paused: bool) -> Result<(), ControlError> {
        self.inner.ensure_active()?;
        self.inner
            .try_send(Command::SetAnalysisPaused(self.inner.id, paused))
    }

    /// Clear the state in this subscription's scope (shared state for all its
    /// members) and restart its History.
    pub fn reset_state(&self) -> Result<(), ControlError> {
        self.inner.ensure_active()?;
        self.inner.try_send(Command::ResetState(self.inner.id))
    }
}

impl Dataflow {
    /// A worker for a source that always delivers (test fixtures).
    #[cfg(test)]
    pub(crate) fn spawn(
        descriptor: Arc<AudioDescriptor>,
        endpoint: ProcessingEndpoint,
        telemetry: Arc<SourceTelemetry>,
    ) -> (Self, DataflowHandle) {
        Self::spawn_with(descriptor, endpoint, telemetry, false)
    }

    /// `fill_silence`: a source that may stop delivering samples while silent (the
    /// system loopback); the worker then stands in silence (see `silence`).
    pub(crate) fn spawn_with(
        descriptor: Arc<AudioDescriptor>,
        mut endpoint: ProcessingEndpoint,
        telemetry: Arc<SourceTelemetry>,
        fill_silence: bool,
    ) -> (Self, DataflowHandle) {
        let silence = fill_silence.then(|| {
            super::silence::SilenceFill::new(
                Arc::clone(&telemetry),
                descriptor.sample_rate_hz,
                usize::from(descriptor.channel_count),
            )
        });
        let (command_sender, command_receiver) = sync_channel(CONTROL_QUEUE_CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let metrics = Arc::new(Metrics::default());
        let store: SharedStore = Arc::new(Mutex::new(Store::default()));
        let waker = Arc::new(ArcSwapOption::<PublicationWaker>::empty());
        let mut engine = Engine::new(
            Arc::clone(&descriptor),
            Arc::clone(&store),
            Arc::clone(&metrics),
        );
        let worker_metrics = Arc::clone(&metrics);
        let worker_waker = Arc::clone(&waker);
        #[cfg(any(test, feature = "automation"))]
        let processed_through_frame = Arc::new(AtomicU64::new(0));
        #[cfg(any(test, feature = "automation"))]
        let worker_frontier = Arc::clone(&processed_through_frame);
        #[cfg(any(test, feature = "automation"))]
        let probe = Arc::new(crate::probe::WorkerProbe::default());
        #[cfg(any(test, feature = "automation"))]
        let worker_probe = Arc::clone(&probe);
        let worker = thread::Builder::new()
            .name("taffymeters-dataflow".to_owned())
            .spawn(move || {
                #[cfg(any(test, feature = "automation"))]
                let _counted = worker_probe.count_this_thread();
                let wake = || {
                    if let Some(waker) = worker_waker.load().as_ref() {
                        waker();
                    }
                };
                run_worker(Worker {
                    engine: &mut engine,
                    endpoint: &mut endpoint,
                    silence,
                    commands: command_receiver,
                    stop: worker_stop,
                    metrics: &worker_metrics,
                    wake: &wake,
                    #[cfg(any(test, feature = "automation"))]
                    processed_frontier: worker_frontier,
                });
            })
            .expect("failed to create dataflow worker");
        #[cfg(any(test, feature = "automation"))]
        probe.attach(&worker);
        let handle = DataflowHandle {
            commands: command_sender,
            worker: worker.thread().clone(),
            stopped: Arc::clone(&stop),
            next_subscription_id: Arc::new(AtomicU64::new(1)),
            next_binding_id: Arc::new(AtomicU64::new(1)),
            descriptor,
            telemetry,
            store,
            metrics,
            waker,
            controls: Arc::new(Mutex::new(Vec::new())),
            #[cfg(any(test, feature = "automation"))]
            processed_through_frame,
            #[cfg(any(test, feature = "automation"))]
            probe,
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
    /// Register `spec` as a new subscription. Private stateful nodes start empty;
    /// pure nodes are shared with identical chains of other subscriptions.
    pub fn subscribe(&self, spec: DataSpec) -> Result<DataSubscription, ControlError> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(ControlError::Stopped);
        }
        let spec = spec.validated(&self.descriptor)?;
        let id = self.next_subscription_id.fetch_add(1, Ordering::Relaxed);
        let applied = request(&self.commands, &self.worker, &self.stopped, |response| {
            Command::Subscribe { id, spec, response }
        })?;
        let inner = Arc::new(ControlShared {
            id,
            commands: self.commands.clone(),
            worker: self.worker.clone(),
            stopped: Arc::clone(&self.stopped),
            descriptor: Arc::clone(&self.descriptor),
            applied: Mutex::new(applied),
            active: AtomicBool::new(true),
        });
        {
            let mut controls = lock(&self.controls);
            controls.retain(|control| control.strong_count() > 0);
            controls.push(Arc::downgrade(&inner));
        }
        Ok(DataSubscription {
            controller: DataController { inner },
            reader: DataReader::new(
                id,
                Arc::clone(&self.store),
                Arc::clone(&self.descriptor),
                Arc::clone(&self.telemetry),
            ),
        })
    }

    /// Apply `edit` to the spec of every member of `binding` and switch them all at
    /// one block boundary. Fails without change when the members would no longer
    /// resolve the binding to one output.
    pub fn update_shared(
        &self,
        binding: SharedAnalysisId,
        edit: impl Fn(&mut DataSpec),
    ) -> Result<(), ControlError> {
        let mut members: Vec<Arc<ControlShared>> = lock(&self.controls)
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|control| control.active.load(Ordering::Acquire))
            .collect();
        members.sort_by_key(|control| control.id);
        let mut guards = Vec::new();
        let mut specs = Vec::new();
        for control in &members {
            let guard = lock(&control.applied);
            if guard
                .spec
                .outputs()
                .iter()
                .any(|output| output.binding == Some(binding))
            {
                let mut spec = guard.spec.clone();
                edit(&mut spec);
                specs.push((control.id, spec.validated(&self.descriptor)?));
                guards.push(guard);
            }
        }
        if specs.is_empty() {
            return Err(ControlError::SharedBindingMismatch(binding));
        }
        let applied = request(&self.commands, &self.worker, &self.stopped, |response| {
            Command::UpdateShared {
                binding,
                specs,
                response,
            }
        })?;
        for (mut guard, applied) in guards.into_iter().zip(applied) {
            *guard = applied;
        }
        Ok(())
    }

    /// A binding no subscription of this process uses yet.
    pub fn allocate_binding(&self) -> SharedAnalysisId {
        SharedAnalysisId(self.next_binding_id.fetch_add(1, Ordering::Relaxed))
    }

    pub fn metrics(&self) -> RuntimeMetrics {
        self.metrics.snapshot()
    }

    /// Install (or clear) the callback run after the worker publishes new results.
    pub fn set_publication_waker(&self, waker: Option<PublicationWaker>) {
        self.waker.store(waker.map(Arc::new));
    }

    /// Allocations and CPU time of the worker thread so far (test/automation only).
    #[cfg(any(test, feature = "automation"))]
    pub(crate) fn worker_sample(&self) -> crate::probe::WorkerSample {
        self.probe.sample()
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
        send(
            &self.commands,
            &self.worker,
            &self.stopped,
            Command::ResumeBoundary(continuity),
        )
    }

    #[cfg(test)]
    pub(crate) fn block_worker_for_test(&self) -> Arc<std::sync::Barrier> {
        let entered = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        send(
            &self.commands,
            &self.worker,
            &self.stopped,
            Command::BlockWorkerForTest {
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            },
        )
        .expect("worker accepts the test block");
        entered.wait();
        release
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

struct Worker<'a> {
    engine: &'a mut Engine,
    endpoint: &'a mut ProcessingEndpoint,
    silence: Option<super::silence::SilenceFill>,
    commands: Receiver<Command>,
    stop: Arc<AtomicBool>,
    metrics: &'a Metrics,
    wake: &'a dyn Fn(),
    #[cfg(any(test, feature = "automation"))]
    processed_frontier: Arc<AtomicU64>,
}

fn run_worker(worker: Worker<'_>) {
    let Worker {
        engine,
        endpoint,
        mut silence,
        commands,
        stop,
        metrics,
        wake,
        #[cfg(any(test, feature = "automation"))]
        processed_frontier,
    } = worker;
    let published = || metrics.publications.load(Ordering::Relaxed);
    while !stop.load(Ordering::Acquire) {
        let before_commands = published();
        let mut command_budget_exhausted = true;
        for _ in 0..COMMAND_BUDGET_PER_TICK {
            match commands.try_recv() {
                Ok(Command::Subscribe { id, spec, response }) => {
                    let _ = response.send(engine.subscribe(id, spec, true));
                }
                Ok(Command::Update { id, spec, response }) => {
                    let _ = response.send(engine.update(id, spec));
                }
                Ok(Command::UpdateShared {
                    binding,
                    specs,
                    response,
                }) => {
                    let _ = response.send(engine.update_shared(binding, specs));
                }
                Ok(Command::SetVisible(id, visible)) => engine.set_visible(id, visible),
                Ok(Command::SetAnalysisPaused(id, paused)) => engine.set_paused(id, paused),
                Ok(Command::ResetState(id)) => engine.reset_state(id),
                Ok(Command::Remove(id)) => engine.remove(id),
                Ok(Command::ResumeBoundary(continuity)) => engine.resume_at(continuity),
                #[cfg(test)]
                Ok(Command::BlockWorkerForTest { entered, release }) => {
                    entered.wait();
                    release.wait();
                }
                Err(TryRecvError::Empty) => {
                    command_budget_exhausted = false;
                    break;
                }
                Err(TryRecvError::Disconnected) => return,
            }
        }
        if published() != before_commands {
            wake();
        }
        if let Some(mut block) = endpoint.try_pop() {
            if let Some(silence) = &mut silence {
                silence.captured(&mut block, Instant::now());
            }
            #[cfg(any(test, feature = "automation"))]
            let block_end = u64::try_from(block.frame_count)
                .ok()
                .and_then(|frames| block.start_frame.checked_add(frames));
            let before = published();
            engine.process_block(&block);
            endpoint.recycle(block);
            if published() != before {
                wake();
            }
            #[cfg(any(test, feature = "automation"))]
            if let Some(block_end) = block_end {
                processed_frontier.store(block_end, Ordering::Release);
            }
        } else if let Some(block) = silence
            .as_mut()
            .and_then(|silence| silence.due(Instant::now()))
        {
            let before = published();
            engine.process_block(block);
            if published() != before {
                wake();
            }
        } else if !command_budget_exhausted {
            thread::park_timeout(Duration::from_millis(5));
        }
    }
}
