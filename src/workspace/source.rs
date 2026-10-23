//! Switching inputs rebuilds the source with the new format and recreates every Module
//! from its configuration under the same instance ID. A format change within one input
//! belongs in Dataflow and must not take this path.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use super::{ActionOutcome, ModuleInstanceId, Workspace, WorkspaceError};
use crate::{
    LiveAudio, LiveAudioConfig,
    audio::Generation,
    dataflow::{PublicationWaker, SharedAnalysisId},
    live::LiveAudioError,
    module::ModuleServices,
    persistence::{InputConfig, NAME_OPTION},
};

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
        let generation = Generation(self.services.source.descriptor().generation.0 + 1);
        let live = LiveAudio::open(LiveAudioConfig::default(), &input, generation)?;
        self.replace_source(live);
        self.input = input;
        Ok(ActionOutcome::Applied)
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
