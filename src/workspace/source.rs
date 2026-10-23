//! Switching inputs rebuilds the source with the new format and recreates every Module
//! from its configuration under the same instance ID. A format change within one input
//! belongs in Dataflow and must not take this path.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use super::{ActionOutcome, ModuleInstanceId, Workspace, WorkspaceError};
use crate::{
    LiveAudio, LiveAudioConfig,
    audio::{Generation, SourceState},
    dataflow::{PublicationWaker, SharedAnalysisId},
    live::{LiveAudioError, endpoint_of},
    module::ModuleServices,
    persistence::{DEFAULT_CAPTURE_INPUT, InputConfig, NAME_OPTION},
};

/// Display time between attempts while no fallback input opens.
const RECOVERY_RETRY: Duration = Duration::from_secs(2);

/// Opens an input; replaceable so headless scenarios never touch system devices.
pub(crate) type InputOpener =
    Box<dyn Fn(&InputConfig, Generation) -> Result<LiveAudio, LiveAudioError> + Send>;

pub(super) fn system_opener() -> InputOpener {
    Box::new(|input, generation| LiveAudio::open(LiveAudioConfig::default(), input, generation))
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

impl Workspace {
    pub(crate) fn input(&self) -> &InputConfig {
        &self.input
    }

    /// Name of the running source, which may differ from the chosen input.
    pub(crate) fn source_name(&self) -> &str {
        self.live.as_ref().map_or("", LiveAudio::name)
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
    }

    pub(super) fn set_input(
        &mut self,
        input: InputConfig,
    ) -> Result<ActionOutcome, WorkspaceError> {
        let live = (self.opener)(&input, self.next_generation())?;
        self.replace_source(live);
        self.input = input;
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

    /// Called from `tick`. Switches a lost source to a default device and keeps the
    /// chosen input, which reopens at the next start; the lost device coming back is
    /// not detected. Costs one atomic read while the source runs.
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
