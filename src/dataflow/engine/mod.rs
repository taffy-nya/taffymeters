//! Pure nodes share keys; stateful nodes are private per subscription unless explicitly bound.
//! Block processing reuses a compiled plan and pooled frames; control changes rebuild the plan.

mod execute;
mod graph;
mod plan;

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::{
    audio::{AudioDescriptor, SourceId},
    snapshot::Frame,
};

use super::{
    ChannelSelection, FftMetadata,
    channel::ResolvedChannel,
    fft::StreamingFft,
    kernels::{PeakState, SmoothState},
    spec::{
        DataSpec, NodeSpec, PeakConfig, SharedAnalysisId, SmoothConfig,
        TIME_PUBLICATIONS_PER_SECOND,
    },
    store::SharedStore,
    time::{LEVELS_FLOOR_DBFS, LoudnessAnalyzer},
};

/// Frames kept for reuse per output ring.
const POOL_CAPACITY: usize = 8;

type NodeId = usize;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeMetrics {
    pub subscriptions: u64,
    /// Interned nodes, including the source and inactive nodes.
    pub nodes: u64,
    pub active_channel_nodes: u64,
    pub active_fft_nodes: u64,
    /// Active Tilt / Smooth / Gain / Peak nodes after an FFT.
    pub active_spectrum_nodes: u64,
    /// Active Levels and Loudness nodes and the Peak nodes after Levels.
    pub active_level_nodes: u64,
    pub rings: u64,
    pub fft_evaluations: u64,
    /// Runs of spectrum nodes after the FFT.
    pub spectrum_evaluations: u64,
    /// Source frames merged into PCM chunks.
    pub pcm_frames: u64,
    /// Source frames analyzed, summed over Levels and Loudness nodes.
    pub level_frames: u64,
    pub publications: u64,
}

#[derive(Default)]
pub(super) struct Metrics {
    subscriptions: AtomicU64,
    nodes: AtomicU64,
    active_channel_nodes: AtomicU64,
    active_fft_nodes: AtomicU64,
    active_spectrum_nodes: AtomicU64,
    active_level_nodes: AtomicU64,
    rings: AtomicU64,
    fft_evaluations: AtomicU64,
    spectrum_evaluations: AtomicU64,
    pcm_frames: AtomicU64,
    level_frames: AtomicU64,
    pub(super) publications: AtomicU64,
}

impl Metrics {
    pub(super) fn snapshot(&self) -> RuntimeMetrics {
        let gauge = |value: &AtomicU64| value.load(Ordering::Acquire);
        let counter = |value: &AtomicU64| value.load(Ordering::Relaxed);
        RuntimeMetrics {
            subscriptions: gauge(&self.subscriptions),
            nodes: gauge(&self.nodes),
            active_channel_nodes: gauge(&self.active_channel_nodes),
            active_fft_nodes: gauge(&self.active_fft_nodes),
            active_spectrum_nodes: gauge(&self.active_spectrum_nodes),
            active_level_nodes: gauge(&self.active_level_nodes),
            rings: gauge(&self.rings),
            fft_evaluations: counter(&self.fft_evaluations),
            spectrum_evaluations: counter(&self.spectrum_evaluations),
            pcm_frames: counter(&self.pcm_frames),
            level_frames: counter(&self.level_frames),
            publications: counter(&self.publications),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum StateScope {
    Pure,
    Owner(u64),
    Binding(SharedAnalysisId),
}

/// Exact parameters; floats compare by canonical bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum SpecKey {
    Source(SourceId),
    Channel(ChannelSelection),
    Fft {
        window: usize,
        hop: usize,
        floor: u32,
    },
    Tilt(u32),
    Smooth(u32, u32),
    Gain(u32),
    Peak(u32, u32),
    Levels,
    Loudness,
}

impl SpecKey {
    fn of(spec: NodeSpec) -> Self {
        match spec {
            NodeSpec::Channel(channel) => Self::Channel(channel),
            NodeSpec::Fft(fft) => Self::Fft {
                window: fft.window_size,
                hop: fft.hop_size,
                floor: fft.floor_dbfs.to_bits(),
            },
            NodeSpec::Tilt { db_per_octave } => Self::Tilt(db_per_octave.to_bits()),
            NodeSpec::Smooth(config) => {
                Self::Smooth(config.attack_ms.to_bits(), config.release_ms.to_bits())
            }
            NodeSpec::Gain { db } => Self::Gain(db.to_bits()),
            NodeSpec::Peak(config) => Self::Peak(
                config.hold_ms.to_bits(),
                config.fall_db_per_second_squared.to_bits(),
            ),
            NodeSpec::Levels => Self::Levels,
            NodeSpec::Loudness => Self::Loudness,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct NodeKey {
    spec: SpecKey,
    input: Option<NodeId>,
    scope: StateScope,
}

enum Kernel {
    Source {
        pending: Vec<f32>,
        pending_start: u64,
        pending_frames: usize,
        pending_sequence: u64,
    },
    Channel {
        resolved: ResolvedChannel,
        out: Vec<f32>,
    },
    Fft(Box<StreamingFft>),
    Tilt {
        db_per_octave: f32,
        table: Vec<f32>,
        out: Vec<f32>,
    },
    Smooth {
        config: SmoothConfig,
        state: SmoothState,
    },
    Gain {
        db: f32,
        out: Vec<f32>,
    },
    Peak {
        config: PeakConfig,
        state: PeakState,
    },
    Levels {
        /// Per-channel sample maxima accumulated since the last publication.
        interval_peaks: Vec<f32>,
        dbfs: Vec<f32>,
        start_frame: u64,
    },
    Loudness {
        analyzer: LoudnessAnalyzer,
        start_frame: u64,
    },
}

impl Kernel {
    fn values(&self) -> &[f32] {
        match self {
            Self::Fft(fft) => fft.bins_dbfs(),
            Self::Tilt { out, .. } | Self::Gain { out, .. } => out,
            Self::Smooth { state, .. } => &state.values,
            Self::Peak { state, .. } => &state.values,
            Self::Levels { dbfs, .. } => dbfs,
            _ => &[],
        }
    }

    fn reset_time_domain(&mut self, start: Option<u64>) {
        let start_frame = match self {
            Self::Levels {
                interval_peaks,
                dbfs,
                start_frame,
            } => {
                interval_peaks.fill(0.0);
                dbfs.fill(LEVELS_FLOOR_DBFS);
                start_frame
            }
            Self::Loudness {
                analyzer,
                start_frame,
            } => {
                analyzer.reset();
                start_frame
            }
            _ => return,
        };
        if let Some(start) = start {
            *start_frame = start;
        }
    }
}

struct RingAttach {
    index: usize,
    consumers: u32,
    /// Longest History of any reader; None when every reader wants Latest.
    keep_frames: Option<u64>,
    gap_pending: bool,
    pool: Vec<Arc<Frame>>,
}

struct Node {
    key: NodeKey,
    refs: u32,
    input: Option<NodeId>,
    children: Vec<NodeId>,
    kernel: Kernel,
    /// This node or an ancestor keeps state.
    stateful_chain: bool,
    fft: Option<FftMetadata>,
    active: bool,
    activated_before: bool,
    ring_demanded: bool,
    /// Its output was read at some point; a later restart is a gap for readers.
    ring_served: bool,
    /// Spectrum results before this source frame are not accepted (after reset or
    /// reactivation the FFT window still holds older audio).
    accept_from: u64,
    produced: bool,
    ring: Option<RingAttach>,
}

struct BindingState {
    node: NodeId,
    members: Vec<u64>,
}

struct Subscription {
    spec: DataSpec,
    /// Source first, then spec nodes in declaration order.
    nodes: Vec<NodeId>,
    outputs: Vec<NodeId>,
    starts: Vec<u64>,
    revision: u64,
    visible: bool,
    paused: bool,
    reset_epoch: u64,
    gap_epoch: u64,
}

/// Bindings may differ from the submitted spec after a private fork.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Applied {
    pub(super) spec: DataSpec,
    pub(super) revision: u64,
}

/// Descendant lists run inputs before their readers.
#[derive(Default)]
struct Plan {
    source: Option<NodeId>,
    channels: Vec<NodeId>,
    levels: Vec<(NodeId, Vec<NodeId>)>,
    loudness: Vec<NodeId>,
    ffts: Vec<(NodeId, Vec<NodeId>)>,
}

pub(super) struct Engine {
    descriptor: Arc<AudioDescriptor>,
    store: SharedStore,
    metrics: Arc<Metrics>,
    nodes: Vec<Option<Node>>,
    free_nodes: Vec<NodeId>,
    index: HashMap<NodeKey, NodeId>,
    ring_owners: Vec<Option<NodeId>>,
    free_rings: Vec<usize>,
    /// Detached ring slots still named by published routes until the next commit.
    retired_rings: Vec<usize>,
    subscriptions: HashMap<u64, Subscription>,
    bindings: HashMap<SharedAnalysisId, BindingState>,
    plan: Plan,
    staged: Vec<(NodeId, Arc<Frame>)>,
    evicted: Vec<(usize, Arc<Frame>)>,
    result_version: u64,
    discontinuity_count: u64,
    expected_sequence: Option<u64>,
    expected_start_frame: Option<u64>,
    minimum_continuity: u64,
    active_continuity: u64,
    current_frontier: u64,
    /// Frames since the last time-domain publication; None without time demand.
    clock: Option<usize>,
    publish_interval_frames: usize,
}

impl Engine {
    pub(super) fn new(
        descriptor: Arc<AudioDescriptor>,
        store: SharedStore,
        metrics: Arc<Metrics>,
    ) -> Self {
        let publish_interval_frames =
            (descriptor.sample_rate_hz as usize).div_ceil(TIME_PUBLICATIONS_PER_SECOND);
        Self {
            descriptor,
            store,
            metrics,
            nodes: Vec::new(),
            free_nodes: Vec::new(),
            index: HashMap::new(),
            ring_owners: Vec::new(),
            free_rings: Vec::new(),
            retired_rings: Vec::new(),
            subscriptions: HashMap::new(),
            bindings: HashMap::new(),
            plan: Plan::default(),
            staged: Vec::new(),
            evicted: Vec::new(),
            result_version: 0,
            discontinuity_count: 0,
            expected_sequence: None,
            expected_start_frame: None,
            minimum_continuity: 0,
            active_continuity: 0,
            current_frontier: 0,
            clock: None,
            publish_interval_frames,
        }
    }

    fn node(&self, id: NodeId) -> &Node {
        self.nodes[id]
            .as_ref()
            .expect("node IDs refer to live interned nodes")
    }

    fn node_mut(&mut self, id: NodeId) -> &mut Node {
        self.nodes[id]
            .as_mut()
            .expect("node IDs refer to live interned nodes")
    }
}
