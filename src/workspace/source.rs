//! Switching inputs rebuilds the source with the new format and recreates every Module
//! from its configuration under the same instance ID. A format change within one input
//! belongs in Dataflow and must not take this path. Following the system default and
//! switching back to a returning device take it, because the device changes.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use super::{ActionOutcome, ModuleInstanceId, Workspace, WorkspaceError};
use crate::{
    LiveAudio,
    audio::{
        Generation, SourceState,
        inputs::{EndpointState, EndpointWaker, EndpointWatch, Endpoints, list_endpoints},
    },
    dataflow::{PublicationWaker, SharedAnalysisId},
    live::{LiveAudioError, endpoint_of, open_system},
    module::ModuleServices,
    persistence::{
        CAPTURE_INPUT, DEFAULT_CAPTURE_INPUT, DEFAULT_PLAYBACK_INPUT, DEVICE_OPTION, InputConfig,
        NAME_OPTION, PLAYBACK_INPUT,
    },
};

/// Display time between attempts while no fallback input opens, or a followed or
/// returning device fails to open.
const RECOVERY_RETRY: Duration = Duration::from_secs(2);

/// Opens an input; replaceable so headless scenarios never touch system devices.
pub(crate) type InputOpener =
    Box<dyn Fn(&InputConfig, Generation) -> Result<LiveAudio, LiveAudioError> + Send>;

pub(super) fn system_opener() -> InputOpener {
    Box::new(open_system)
}

pub(crate) type EndpointLister = Box<dyn Fn() -> Endpoints + Send>;

pub(super) fn system_lister() -> EndpointLister {
    Box::new(list_endpoints)
}

/// The default of the lost input's own group first: a lost microphone falls back to
/// the default microphone, everything else to the default playback device.
fn fallbacks(lost: &InputConfig) -> Vec<InputConfig> {
    let playback = InputConfig::default();
    if endpoint_of(lost).is_some_and(|endpoint| !endpoint.is_playback()) {
        let capture = InputConfig {
            kind: DEFAULT_CAPTURE_INPUT.to_owned(),
            ..InputConfig::default()
        };
        vec![capture, playback]
    } else {
        vec![playback]
    }
}

/// Device ID of a fixed endpoint input.
fn fixed_device(input: &InputConfig) -> Option<&str> {
    matches!(input.kind.as_str(), PLAYBACK_INPUT | CAPTURE_INPUT)
        .then(|| input.option(DEVICE_OPTION))
        .flatten()
}

/// A headset shows one name for its playback and microphone endpoints.
pub(super) fn input_label(input: &InputConfig) -> String {
    let name = || {
        input
            .option(NAME_OPTION)
            .or_else(|| input.option(DEVICE_OPTION))
            .unwrap_or(&input.kind)
    };
    match input.kind.as_str() {
        DEFAULT_PLAYBACK_INPUT => "Default playback".to_owned(),
        DEFAULT_CAPTURE_INPUT => "Default microphone".to_owned(),
        CAPTURE_INPUT => format!("Microphone: {}", name()),
        _ => name().to_owned(),
    }
}

pub(super) struct EndpointFollow {
    watch: EndpointWatch,
    /// Watch version last acted on.
    seen: u64,
    watching: Option<String>,
    /// The watched device was seen absent, so its return switches back to it. A device
    /// that never disappeared (one that failed to open) is not retried on its own.
    watched_absent: bool,
    retry_at: Option<Duration>,
    failure_reported: bool,
    waker: Option<EndpointWaker>,
}

impl EndpointFollow {
    fn new(watch: EndpointWatch) -> Self {
        Self {
            watch,
            seen: 0,
            watching: None,
            watched_absent: false,
            retry_at: None,
            failure_reported: false,
            waker: None,
        }
    }

    /// Never reports a change; headless workspaces never poll system devices.
    pub(super) fn inert() -> Self {
        Self::new(EndpointWatch::manual(Box::new(|_| {
            EndpointState::default()
        })))
    }
}

impl Workspace {
    pub(crate) fn input(&self) -> &InputConfig {
        &self.input
    }

    /// Name of the running source, which may differ from the chosen input.
    pub(crate) fn source_name(&self) -> &str {
        self.live.as_ref().map_or("", LiveAudio::name)
    }

    /// The system device being captured, as `cpal::DeviceId` text.
    pub(crate) fn source_device_id(&self) -> Option<&str> {
        self.live.as_ref().and_then(LiveAudio::device_id)
    }

    /// Starts following the system default devices; the product and system previews only.
    pub(crate) fn watch_system_endpoints(&mut self) {
        match EndpointWatch::system() {
            Ok(watch) => self.install_endpoint_watch(watch),
            Err(error) => self.push_notice(format!("Couldn't watch audio devices: {error}")),
        }
    }

    #[cfg(feature = "automation")]
    pub(crate) fn set_endpoint_watch(&mut self, watch: EndpointWatch) {
        self.install_endpoint_watch(watch);
    }

    #[cfg(feature = "automation")]
    pub(crate) fn poll_endpoints_for_automation(&self) {
        self.follow.watch.poll_now();
    }

    fn install_endpoint_watch(&mut self, watch: EndpointWatch) {
        let waker = self.follow.waker.take();
        watch.set_waker(waker.clone());
        self.follow = EndpointFollow {
            waker,
            ..EndpointFollow::new(watch)
        };
        self.update_watched();
    }

    /// Called when the endpoint state changes. Unlike data wake-ups it must also wake the
    /// host while every window is minimized: the source follows the default regardless.
    pub(crate) fn set_endpoint_waker(&mut self, waker: Option<EndpointWaker>) {
        self.follow.watch.set_waker(waker.clone());
        self.follow.waker = waker;
    }

    /// Display time until a failed follow or switch-back is retried.
    pub(crate) fn endpoint_retry_delay(&self) -> Option<Duration> {
        self.follow
            .retry_at
            .map(|at| at.saturating_sub(self.display_time))
    }

    /// Records what the running source was opened with: a default while falling back
    /// from the chosen input.
    pub(super) fn set_opened(&mut self, opened: InputConfig) {
        self.opened = opened;
        self.follow.retry_at = None;
        self.follow.failure_reported = false;
        self.update_watched();
    }

    /// Watches the chosen fixed device while a fallback runs in its place.
    fn update_watched(&mut self) {
        let watched = fixed_device(&self.input).filter(|_| self.opened != self.input);
        if self.follow.watching.as_deref() != watched {
            self.follow.watching = watched.map(str::to_owned);
            self.follow.watched_absent = false;
        }
        self.follow.watch.watch(watched);
    }

    /// Callback run on the analysis worker after it publishes new results.
    pub(crate) fn set_publication_waker(&mut self, waker: Option<PublicationWaker>) {
        self.waker = waker.map(Arc::from);
        self.install_waker();
    }

    fn install_waker(&self) {
        let forward = self.waker.as_ref().map(|waker| {
            let waker = Arc::clone(waker);
            Box::new(move || waker()) as PublicationWaker
        });
        self.services.dataflow.set_publication_waker(forward);
    }

    /// Called at startup when the chosen input could not be opened and the default
    /// playback device runs instead.
    pub(crate) fn input_unavailable(&mut self, error: &LiveAudioError) {
        let name = self
            .input
            .option(NAME_OPTION)
            .unwrap_or(&self.input.kind)
            .to_owned();
        self.push_notice(format!(
            "Input {name} unavailable: {error}; using the default playback device (choice kept)"
        ));
        self.set_opened(InputConfig::default());
    }

    pub(super) fn set_input(
        &mut self,
        input: InputConfig,
    ) -> Result<ActionOutcome, WorkspaceError> {
        let live = (self.opener)(&input, self.next_generation())?;
        self.replace_source(live);
        self.input = input.clone();
        self.set_opened(input);
        self.next_recovery = None;
        Ok(ActionOutcome::Applied)
    }

    fn next_generation(&self) -> Generation {
        Generation(self.services.source.descriptor().generation.0 + 1)
    }

    #[cfg(feature = "automation")]
    pub(crate) fn set_input_opener(&mut self, opener: InputOpener) {
        self.opener = opener;
    }

    #[cfg(feature = "automation")]
    pub(crate) fn set_endpoint_lister(&mut self, lister: EndpointLister) {
        self.endpoint_lister = lister;
    }

    /// Called from `tick`. Switches a lost source to a default device and keeps the
    /// chosen input, which `follow_endpoints` switches back to when it returns. Costs
    /// one atomic read while the source runs.
    pub(super) fn recover_lost_source(&mut self) {
        if self.source_status().state != SourceState::DeviceLost {
            self.next_recovery = None;
            return;
        }
        let now = self.display_time;
        if self.next_recovery.is_some_and(|at| now < at) {
            return;
        }
        self.next_recovery = Some(now + RECOVERY_RETRY);
        let lost = self.source_name().to_owned();
        let mut failure = None;
        for input in fallbacks(&self.input) {
            match (self.opener)(&input, self.next_generation()) {
                Ok(live) => {
                    self.replace_source(live);
                    self.set_opened(input);
                    self.next_recovery = None;
                    let name = self.source_name().to_owned();
                    self.push_notice(format!("Input {lost} lost; switched to {name}"));
                    return;
                }
                Err(error) => failure = Some(error),
            }
        }
        if let Some(error) = failure {
            self.push_notice(format!(
                "Input {lost} lost; no other device opened: {error}"
            ));
        }
    }

    /// Called from `tick` after `recover_lost_source`. Reopens a default input when the
    /// system default moves to another device, and the chosen fixed device when it
    /// returns during a fallback. Costs one atomic read while nothing changes.
    pub(super) fn follow_endpoints(&mut self) {
        if self.source_status().state == SourceState::DeviceLost {
            // Recovery reopens a device, which clears this; its own polling keeps passes coming.
            self.follow.retry_at = None;
            return;
        }
        let now = self.display_time;
        let version = self.follow.watch.version();
        match self.follow.retry_at {
            Some(at) if now < at => return,
            None if version == self.follow.seen => return,
            _ => {}
        }
        self.follow.seen = version;
        let state = self.follow.watch.state();
        let switch_back = self.chosen_returned(&state);
        let target = if switch_back {
            self.input.clone()
        } else if self.default_moved(&state) {
            self.opened.clone()
        } else {
            self.follow.retry_at = None;
            return;
        };
        match (self.opener)(&target, self.next_generation()) {
            Ok(live) => {
                self.replace_source(live);
                self.set_opened(target);
                if switch_back {
                    let name = self.source_name().to_owned();
                    self.push_notice(format!("Input {name} is back; switched back"));
                }
            }
            Err(error) => {
                self.follow.retry_at = Some(now + RECOVERY_RETRY);
                if !std::mem::replace(&mut self.follow.failure_reported, true) {
                    self.push_notice(format!(
                        "Couldn't open {}: {error}; retrying",
                        input_label(&target)
                    ));
                }
            }
        }
    }

    fn chosen_returned(&mut self, state: &EndpointState) -> bool {
        let Some(watching) = self.follow.watching.as_deref() else {
            return false;
        };
        match &state.watched {
            Some((device, false)) if device == watching => {
                self.follow.watched_absent = true;
                false
            }
            Some((device, true)) if device == watching => self.follow.watched_absent,
            _ => false,
        }
    }

    /// The running device differs from the default its input follows.
    fn default_moved(&self, state: &EndpointState) -> bool {
        let default = match self.opened.kind.as_str() {
            DEFAULT_PLAYBACK_INPUT => &state.default_playback,
            DEFAULT_CAPTURE_INPUT => &state.default_capture,
            _ => return false,
        };
        matches!(
            (default, self.source_device_id()),
            (Some(default), Some(running)) if default != running
        )
    }

    /// Modules whose configuration no longer fits the new format become placeholders
    /// that keep it, so switching back restores them.
    pub(crate) fn replace_source(&mut self, live: LiveAudio) {
        let paused = self.live.as_ref().is_some_and(LiveAudio::paused);
        let mut numbers = std::mem::take(&mut self.binding_numbers);
        let mut used: HashSet<u64> = numbers.values().copied().collect();
        for entry in self.modules.values() {
            if let Some(preserved) = &entry.preserved {
                used.extend(preserved.file.bindings());
            }
        }
        let mut ids: Vec<ModuleInstanceId> = self.modules.keys().copied().collect();
        ids.sort();
        let files: Vec<_> = ids
            .into_iter()
            .map(|id| {
                let file = self.module_file(&self.modules[&id], &mut numbers, &mut used);
                (id, file)
            })
            .collect();
        // Every Module drops before the source and worker it reads from.
        self.modules.clear();
        self.services = ModuleServices {
            source: live.reader(),
            dataflow: live.dataflow_handle(),
        };
        drop(self.live.replace(live));
        self.install_waker();
        let mut bindings: HashMap<u64, SharedAnalysisId> = HashMap::new();
        for (id, file) in &files {
            self.restore_module_as(*id, file, &mut bindings);
        }
        self.binding_numbers = bindings
            .into_iter()
            .map(|(number, binding)| (binding, number))
            .collect();
        self.sync_visibility();
        if paused && let Some(Err(error)) = self.live.as_mut().map(|live| live.set_paused(true)) {
            self.push_notice(format!("Couldn't keep the new input paused: {error}"));
        }
    }
}
