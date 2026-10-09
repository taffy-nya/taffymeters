//! Immutable analysis results and the readers a Module uses to see them.
//!
//! The worker pushes `Arc<Frame>`s into output rings; a reader copies `Arc`s out
//! under one short lock. A Module keeps a `Snapshot` (latest result of every
//! output, read together) and, for histories, a `HistoryView` that only takes the
//! frames it has not seen yet.

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
    /// Per channel, 400 ms K-weighted loudness.
    pub momentary_lufs: Vec<f32>,
    /// Per channel, the largest absolute sample of the publication interval.
    pub sample_peaks: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum FrameData {
    Spectrum(SpectrumData),
    Pcm(AudioChunk),
    Levels(LevelsData),
}

/// One result of one output.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    /// Unique and increasing across every output of the worker.
    pub result_version: u64,
    /// Exclusive end of the source frames this result accounts for.
    pub processed_through_frame: u64,
    pub source_sequence: u64,
    /// Source discontinuities seen by the worker when this result was produced.
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
}

/// The latest result of every output of one subscription, read under one lock so
/// the outputs of one processed block are seen together.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    config_revision: u64,
    frames: Vec<Option<Arc<Frame>>>,
}

impl Snapshot {
    pub fn get(&self, output: OutputId) -> Option<&Arc<Frame>> {
        self.frames.get(output.index()).and_then(Option::as_ref)
    }

    /// Revision of the spec these results belong to.
    pub fn config_revision(&self) -> u64 {
        self.config_revision
    }
}

#[derive(Clone, Debug)]
pub struct ViewFrame {
    pub frame: Arc<Frame>,
    /// Source discontinuity, reactivation or resumed reading before this frame.
    pub gap_before: bool,
}

/// A Module-held History: the frames of one output within its kept duration,
/// updated incrementally.
#[derive(Clone, Debug, Default)]
pub struct HistoryView {
    frames: VecDeque<ViewFrame>,
    last_version: u64,
    revision: u64,
    reset_epoch: u64,
    gap_epoch: u64,
    gap_pending: bool,
    keep_frames: u64,
}

impl HistoryView {
    pub fn new() -> Self {
        Self::default()
    }

    /// Frames oldest first, as one slice.
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

    /// First source frame of the kept window. The oldest PCM chunk may start
    /// before it; readers clip it themselves.
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

    /// Refresh `snapshot` with the latest visible result of every output. Returns
    /// false, leaving `snapshot` untouched, when the subscription is closed or its
    /// analysis is paused.
    pub fn read(&self, snapshot: &mut Snapshot) -> bool {
        let store = crate::dataflow::store::lock(&self.store);
        let Some(route) = store.routes.get(&self.id) else {
            return false;
        };
        if route.paused {
            return false;
        }
        snapshot.config_revision = route.revision;
        snapshot.frames.resize(route.outputs.len(), None);
        for (slot, output) in snapshot.frames.iter_mut().zip(&route.outputs) {
            let latest = store.rings[output.ring]
                .as_ref()
                .and_then(|ring| ring.frames.back())
                .filter(|frame| frame.processed_through_frame > output.start_frame);
            match latest {
                Some(frame) if slot.as_ref().is_some_and(|held| Arc::ptr_eq(held, frame)) => {}
                Some(frame) => *slot = Some(Arc::clone(frame)),
                None => *slot = None,
            }
        }
        true
    }

    /// Latest visible result of one output.
    pub fn latest(&self, output: OutputId) -> Option<Arc<Frame>> {
        let store = crate::dataflow::store::lock(&self.store);
        let route = store.routes.get(&self.id)?;
        let output = route.outputs.get(output.index())?;
        store.rings[output.ring]
            .as_ref()?
            .frames
            .back()
            .filter(|frame| frame.processed_through_frame > output.start_frame)
            .cloned()
    }

    /// Append the frames of `output` that `view` has not seen and drop those older
    /// than the kept duration. Returns false when closed or paused.
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
        if view.revision != route.revision || view.reset_epoch != output.reset_epoch {
            view.clear();
            view.last_version = 0;
            view.revision = route.revision;
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

    /// Revision of the spec the runtime has applied, or None once closed.
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
