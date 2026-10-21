//! The worker publishes immutable `Arc<Frame>`s; readers clone them under a short store lock.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use crate::{
    audio::{AudioDescriptor, SourceStatus, SourceTelemetry},
    dataflow::{FftMetadata, OutputId, store::Store},
};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioChunk {
    pub start_frame: u64,
    pub frame_count: usize,
    /// `frame_count × channels` interleaved samples.
    pub interleaved: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpectrumData {
    pub fft: FftMetadata,
    pub bins_dbfs: Vec<f32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LevelsData {
    /// Per channel in dBFS: the sample peak of the publication interval, or after
    /// a Peak node its held and falling value.
    pub dbfs: Vec<f32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LoudnessData {
    /// 400 ms K-weighted loudness of the whole programme.
    pub momentary_lufs: f32,
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum FrameData {
    Spectrum(SpectrumData),
    Pcm(AudioChunk),
    Levels(LevelsData),
    Loudness(LoudnessData),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// Unique and increasing across every output of the worker.
    pub result_version: u64,
    /// Exclusive end of the source frames this result accounts for.
    pub processed_through_frame: u64,
    pub source_sequence: u64,
    pub discontinuity_count: u64,
    /// The output was interrupted (source gap, reactivation) before this result.
    pub discontinuity_before: bool,
    pub data: FrameData,
}

impl Frame {
    pub fn spectrum(&self) -> Option<&SpectrumData> {
        match &self.data {
            FrameData::Spectrum(data) => Some(data),
            _ => None,
        }
    }

    pub fn pcm(&self) -> Option<&AudioChunk> {
        match &self.data {
            FrameData::Pcm(data) => Some(data),
            _ => None,
        }
    }

    pub fn levels(&self) -> Option<&LevelsData> {
        match &self.data {
            FrameData::Levels(data) => Some(data),
            _ => None,
        }
    }

    pub fn loudness(&self) -> Option<&LoudnessData> {
        match &self.data {
            FrameData::Loudness(data) => Some(data),
            _ => None,
        }
    }
}

/// Reads outputs of a processed block together under one lock. Slots retain the last result
/// across spec changes until replacement data arrives; use [`DataReader::latest`] and
/// [`DataReader::config_revision`] when results must belong to the new spec.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    frames: Vec<Option<Arc<Frame>>>,
}

impl Snapshot {
    pub fn get(&self, output: OutputId) -> Option<&Arc<Frame>> {
        self.frames.get(output.index()).and_then(Option::as_ref)
    }
}

#[derive(Clone, Debug)]
pub struct ViewFrame {
    pub frame: Arc<Frame>,
    /// Source discontinuity, reactivation or resumed reading before this frame.
    pub gap_before: bool,
}

/// Spec changes retain frames with their original metadata; runtime resets clear accumulated history.
/// Frames are kept oldest first and pruned to the retained source-time duration.
#[derive(Clone, Debug, Default)]
pub struct HistoryView {
    frames: VecDeque<ViewFrame>,
    last_version: u64,
    reset_epoch: u64,
    gap_epoch: u64,
    gap_pending: bool,
    keep_frames: u64,
}

impl HistoryView {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn frames(&mut self) -> &[ViewFrame] {
        self.frames.make_contiguous()
    }

    pub fn iter(&self) -> impl Iterator<Item = &ViewFrame> {
        self.frames.iter()
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn newest_frontier(&self) -> Option<u64> {
        self.frames
            .back()
            .map(|frame| frame.frame.processed_through_frame)
    }

    /// First source frame of the kept window. The oldest PCM chunk may start before it;
    /// readers must clip that chunk themselves.
    pub fn window_start(&self) -> u64 {
        self.newest_frontier()
            .unwrap_or(0)
            .saturating_sub(self.keep_frames)
    }

    pub fn clear(&mut self) {
        self.frames.clear();
        self.gap_pending = false;
    }
}

#[derive(Clone)]
pub struct DataReader {
    id: u64,
    store: Arc<Mutex<Store>>,
    descriptor: Arc<AudioDescriptor>,
    telemetry: Arc<SourceTelemetry>,
}

impl DataReader {
    pub(crate) fn new(
        id: u64,
        store: Arc<Mutex<Store>>,
        descriptor: Arc<AudioDescriptor>,
        telemetry: Arc<SourceTelemetry>,
    ) -> Self {
        Self {
            id,
            store,
            descriptor,
            telemetry,
        }
    }

    /// Returns false without changing `snapshot` when the subscription is closed or paused.
    pub fn read(&self, snapshot: &mut Snapshot) -> bool {
        let store = crate::dataflow::store::lock(&self.store);
        let Some(route) = store.routes.get(&self.id) else {
            return false;
        };
        if route.paused {
            return false;
        }
        snapshot.frames.resize(route.outputs.len(), None);
        for (slot, output) in snapshot.frames.iter_mut().zip(&route.outputs) {
            if let Some(frame) = store.rings[output.ring]
                .as_ref()
                .and_then(|ring| ring.frames.back())
                .filter(|frame| frame.processed_through_frame > output.start_frame)
                && !slot.as_ref().is_some_and(|held| Arc::ptr_eq(held, frame))
            {
                *slot = Some(Arc::clone(frame));
            }
        }
        true
    }

    /// Paused subscriptions return the result that was visible when they paused.
    pub fn latest(&self, output: OutputId) -> Option<Arc<Frame>> {
        let store = crate::dataflow::store::lock(&self.store);
        let route = store.routes.get(&self.id)?;
        if route.paused {
            return route.frozen.get(output.index()).cloned().flatten();
        }
        let output = route.outputs.get(output.index())?;
        store.rings[output.ring]
            .as_ref()?
            .frames
            .back()
            .filter(|frame| frame.processed_through_frame > output.start_frame)
            .cloned()
    }

    /// Returns false without changing `view` if the subscription is closed or paused,
    /// or `output` does not exist.
    pub fn sync_history(&self, output: OutputId, view: &mut HistoryView) -> bool {
        let store = crate::dataflow::store::lock(&self.store);
        let Some(route) = store.routes.get(&self.id) else {
            return false;
        };
        if route.paused {
            return false;
        }
        let Some(output) = route.outputs.get(output.index()) else {
            return false;
        };
        if view.reset_epoch != output.reset_epoch {
            view.clear();
            view.last_version = 0;
            view.reset_epoch = output.reset_epoch;
            view.gap_epoch = output.gap_epoch;
        }
        if view.gap_epoch != output.gap_epoch {
            view.gap_epoch = output.gap_epoch;
            view.gap_pending = !view.frames.is_empty();
        }
        view.keep_frames = output.keep_frames.unwrap_or(0);
        if let Some(ring) = store.rings[output.ring].as_ref() {
            let first = ring
                .frames
                .partition_point(|frame| frame.result_version <= view.last_version);
            for frame in ring.frames.range(first..) {
                view.last_version = frame.result_version;
                if frame.processed_through_frame <= output.start_frame {
                    continue;
                }
                let gap_before =
                    frame.discontinuity_before || std::mem::take(&mut view.gap_pending);
                view.frames.push_back(ViewFrame {
                    frame: Arc::clone(frame),
                    gap_before,
                });
            }
        }
        drop(store);
        if let Some(newest) = view.newest_frontier() {
            let window_start = newest.saturating_sub(view.keep_frames);
            while view
                .frames
                .front()
                .is_some_and(|oldest| oldest.frame.processed_through_frame <= window_start)
            {
                view.frames.pop_front();
            }
        }
        true
    }

    pub fn config_revision(&self) -> Option<u64> {
        crate::dataflow::store::lock(&self.store)
            .routes
            .get(&self.id)
            .map(|route| route.revision)
    }

    pub fn descriptor(&self) -> &Arc<AudioDescriptor> {
        &self.descriptor
    }

    pub fn source_status(&self) -> SourceStatus {
        self.telemetry.snapshot()
    }
}
