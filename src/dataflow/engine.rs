//! Worker-side analysis graph. Subscriptions intern their `DataSpec` into shared
//! nodes (pure nodes by key, stateful nodes per owner or explicit binding), the
//! graph is compiled into a flat per-block plan, and results are staged into
//! pooled frames and pushed to the output rings once per block.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::{
    audio::{AudioBlock, AudioDescriptor, SourceId},
    snapshot::{AudioChunk, Frame, FrameData, LevelsData, SpectrumData},
};

use super::{
    ChannelSelection, ControlError, FftMetadata,
    channel::ResolvedChannel,
    fft::StreamingFft,
    kernels::{PeakState, SmoothState, add_with_floor, offset_with_floor, tilt_table},
    spec::{
        DataSpec, Keep, NodeSpec, PeakConfig, SharedAnalysisId, SmoothConfig,
        TIME_PUBLICATIONS_PER_SECOND,
    },
    store::{Ring, Route, RouteOutput, SharedStore, lock},
    time::LevelAnalyzer,
};

/// Frames kept for reuse per output ring.
const POOL_CAPACITY: usize = 8;

type NodeId = usize;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeMetrics {
    /// Live subscriptions.
    pub subscriptions: u64,
    /// Interned nodes, including the source and inactive nodes.
    pub nodes: u64,
    pub active_channel_nodes: u64,
    pub active_fft_nodes: u64,
    /// Active Tilt / Smooth / Gain / Peak nodes.
    pub active_spectrum_nodes: u64,
    pub active_level_nodes: u64,
    /// Output rings (nodes some subscription reads).
    pub rings: u64,
    pub fft_evaluations: u64,
    /// Runs of spectrum nodes after the FFT.
    pub spectrum_evaluations: u64,
    /// Source frames merged into PCM chunks.
    pub pcm_frames: u64,
    /// Source frames analyzed, summed over Levels nodes.
    pub level_frames: u64,
    /// Frames pushed into output rings.
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

/// Who owns a node's state: nobody (pure, shared by key), one subscription, or an
/// explicit binding.
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
                config.decay_db_per_second.to_bits(),
            ),
            NodeSpec::Levels => Self::Levels,
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
        analyzer: LevelAnalyzer,
        momentary_lufs: Vec<f32>,
        block_peaks: Vec<f32>,
        interval_peaks: Vec<f32>,
        start_frame: u64,
    },
}

impl Kernel {
    fn spectrum(&self) -> &[f32] {
        match self {
            Self::Fft(fft) => fft.bins_dbfs(),
            Self::Tilt { out, .. } | Self::Gain { out, .. } => out,
            Self::Smooth { state, .. } => &state.values,
            Self::Peak { state, .. } => &state.values,
            _ => &[],
        }
    }

    fn is_spectrum_node(&self) -> bool {
        matches!(
            self,
            Self::Tilt { .. } | Self::Smooth { .. } | Self::Gain { .. } | Self::Peak { .. }
        )
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
    /// FFT parameters of the chain (spectrum nodes and the FFT itself).
    fft: Option<FftMetadata>,
    active: bool,
    activated_before: bool,
    /// Some subscription currently wants this node's own output.
    ring_demanded: bool,
    /// Its output was read at some point; a later restart is a gap for readers.
    ring_served: bool,
    /// Spectrum results before this source frame are not accepted (after reset or
    /// reactivation the FFT window still holds older audio).
    accept_from: u64,
    /// Produced a result in the current hop.
    produced: bool,
    ring: Option<RingAttach>,
}

struct BindingState {
    node: NodeId,
    members: Vec<u64>,
}

struct Subscription {
    spec: DataSpec,
    /// Node of each spec node, in spec order.
    nodes: Vec<NodeId>,
    /// Node of each output.
    outputs: Vec<NodeId>,
    starts: Vec<u64>,
    revision: u64,
    visible: bool,
    paused: bool,
    reset_epoch: u64,
    gap_epoch: u64,
}

/// A spec the runtime accepted, as finally applied (bindings may have forked).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Applied {
    pub(super) spec: DataSpec,
    pub(super) revision: u64,
}

#[derive(Default)]
struct Plan {
    source: Option<NodeId>,
    channels: Vec<NodeId>,
    levels: Vec<NodeId>,
    /// Active FFT nodes with their active descendants in pre-order.
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

/// Borrow a parent immutably and its child mutably.
fn parent_and_child(
    nodes: &mut [Option<Node>],
    parent: NodeId,
    child: NodeId,
) -> (&Node, &mut Node) {
    assert_ne!(parent, child);
    if parent < child {
        let (head, tail) = nodes.split_at_mut(child);
        (
            head[parent].as_ref().expect("live parent"),
            tail[0].as_mut().expect("live child"),
        )
    } else {
        let (head, tail) = nodes.split_at_mut(parent);
        (
            tail[0].as_ref().expect("live parent"),
            head[child].as_mut().expect("live child"),
        )
    }
}

/// A binding that two outputs of one spec resolve to different nodes.
fn conflicting_binding(spec: &DataSpec, outputs: &[NodeId]) -> Option<SharedAnalysisId> {
    let mut agreed: HashMap<SharedAnalysisId, NodeId> = HashMap::new();
    spec.outputs()
        .iter()
        .zip(outputs)
        .find_map(|(output, &node)| {
            let binding = output.binding?;
            (*agreed.entry(binding).or_insert(node) != node).then_some(binding)
        })
}

/// A pooled frame no reader holds, or a new one.
fn take_frame(pool: &mut Vec<Arc<Frame>>, empty: impl FnOnce() -> FrameData) -> Arc<Frame> {
    if let Some(index) = pool
        .iter_mut()
        .position(|frame| Arc::get_mut(frame).is_some())
    {
        return pool.swap_remove(index);
    }
    Arc::new(Frame {
        result_version: 0,
        processed_through_frame: 0,
        source_sequence: 0,
        discontinuity_count: 0,
        discontinuity_before: false,
        data: empty(),
    })
}

struct StageContext {
    processed_through_frame: u64,
    source_sequence: u64,
    discontinuity_count: u64,
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
        self.nodes[id].as_ref().expect("live node")
    }

    fn node_mut(&mut self, id: NodeId) -> &mut Node {
        self.nodes[id].as_mut().expect("live node")
    }

    // ---- interning -------------------------------------------------------------

    fn intern(&mut self, key: NodeKey, spec: Option<NodeSpec>) -> NodeId {
        if let Some(&id) = self.index.get(&key) {
            self.node_mut(id).refs += 1;
            return id;
        }
        let parent = key.input.map(|input| self.node(input));
        let stateful_chain = spec.is_some_and(|spec| spec.is_stateful())
            || parent.is_some_and(|parent| parent.stateful_chain);
        let sample_rate = self.descriptor.sample_rate_hz;
        let fft = match spec {
            Some(NodeSpec::Fft(config)) => Some(FftMetadata::of(config, sample_rate)),
            _ => parent.and_then(|parent| parent.fft),
        };
        let channels = self.descriptor.channel_count as usize;
        let kernel = match spec {
            None => Kernel::Source {
                pending: Vec::new(),
                pending_start: 0,
                pending_frames: 0,
                pending_sequence: 0,
            },
            Some(NodeSpec::Channel(channel)) => Kernel::Channel {
                resolved: channel
                    .resolve(&self.descriptor)
                    .expect("specs are validated against the descriptor"),
                out: Vec::new(),
            },
            Some(NodeSpec::Fft(config)) => Kernel::Fft(Box::new(StreamingFft::new(config))),
            Some(NodeSpec::Tilt { db_per_octave }) => Kernel::Tilt {
                db_per_octave,
                table: Vec::new(),
                out: Vec::new(),
            },
            Some(NodeSpec::Smooth(config)) => Kernel::Smooth {
                config,
                state: SmoothState::default(),
            },
            Some(NodeSpec::Gain { db }) => Kernel::Gain {
                db,
                out: Vec::new(),
            },
            Some(NodeSpec::Peak(config)) => Kernel::Peak {
                config,
                state: PeakState::default(),
            },
            Some(NodeSpec::Levels) => Kernel::Levels {
                analyzer: LevelAnalyzer::new(sample_rate, channels),
                momentary_lufs: vec![-60.0; channels],
                block_peaks: vec![0.0; channels],
                interval_peaks: vec![0.0; channels],
                start_frame: self.current_frontier,
            },
        };
        let node = Node {
            key,
            refs: 1,
            input: key.input,
            children: Vec::new(),
            kernel,
            stateful_chain,
            fft,
            active: false,
            activated_before: false,
            ring_demanded: false,
            ring_served: false,
            accept_from: 0,
            produced: false,
            ring: None,
        };
        let id = if let Some(id) = self.free_nodes.pop() {
            self.nodes[id] = Some(node);
            id
        } else {
            self.nodes.push(Some(node));
            self.nodes.len() - 1
        };
        if let Some(input) = key.input {
            self.node_mut(input).children.push(id);
        }
        self.index.insert(key, id);
        id
    }

    fn release(&mut self, id: NodeId) {
        let node = self.node_mut(id);
        node.refs -= 1;
        if node.refs > 0 {
            return;
        }
        debug_assert!(node.children.is_empty() && node.ring.is_none());
        let node = self.nodes[id].take().expect("live node");
        self.index.remove(&node.key);
        if let Some(input) = node.input {
            self.node_mut(input).children.retain(|child| *child != id);
        }
        self.free_nodes.push(id);
    }

    /// Intern every node of `spec` for subscription `owner`: the source first, then
    /// one reference per spec node (spec node `i` at index `i + 1`).
    fn intern_spec(&mut self, owner: u64, spec: &DataSpec) -> (Vec<NodeId>, Vec<NodeId>) {
        let scopes = spec
            .binding_scopes()
            .expect("specs are validated before interning");
        let source = self.intern(
            NodeKey {
                spec: SpecKey::Source(self.descriptor.source_id),
                input: None,
                scope: StateScope::Pure,
            },
            None,
        );
        let mut ids = Vec::with_capacity(spec.spec_nodes().len() + 1);
        ids.push(source);
        for (index, node) in spec.spec_nodes().iter().enumerate() {
            let input = node.input.index().map_or(source, |input| ids[input + 1]);
            let scope = if node.spec.is_stateful() {
                scopes[index].map_or(StateScope::Owner(owner), StateScope::Binding)
            } else {
                StateScope::Pure
            };
            let id = self.intern(
                NodeKey {
                    spec: SpecKey::of(node.spec),
                    input: Some(input),
                    scope,
                },
                Some(node.spec),
            );
            ids.push(id);
        }
        let outputs = spec
            .outputs()
            .iter()
            .map(|output| output.node.index().map_or(source, |index| ids[index + 1]))
            .collect();
        (ids, outputs)
    }

    /// Release in reverse so children go before their inputs.
    fn release_all(&mut self, ids: &[NodeId]) {
        for &id in ids.iter().rev() {
            self.release(id);
        }
    }

    fn keep_frames(&self, keep: Keep) -> Option<u64> {
        match keep {
            Keep::Latest => None,
            Keep::Seconds(seconds) => {
                Some((f64::from(seconds) * f64::from(self.descriptor.sample_rate_hz)).ceil() as u64)
            }
        }
    }

    fn attach_ring(&mut self, node: NodeId) {
        if let Some(ring) = &mut self.node_mut(node).ring {
            ring.consumers += 1;
            return;
        }
        let index = self.free_rings.pop().unwrap_or_else(|| {
            self.ring_owners.push(None);
            self.ring_owners.len() - 1
        });
        self.ring_owners[index] = Some(node);
        {
            let mut store = lock(&self.store);
            if store.rings.len() <= index {
                store.rings.resize_with(index + 1, || None);
            }
            store.rings[index] = Some(Ring::default());
        }
        self.node_mut(node).ring = Some(RingAttach {
            index,
            consumers: 1,
            keep_frames: None,
            gap_pending: false,
            pool: Vec::with_capacity(POOL_CAPACITY),
        });
    }

    fn detach_ring(&mut self, node: NodeId) {
        let ring = self.node_mut(node).ring.as_mut().expect("attached ring");
        ring.consumers -= 1;
        if ring.consumers > 0 {
            return;
        }
        let index = ring.index;
        self.node_mut(node).ring = None;
        self.ring_owners[index] = None;
        // The slot is freed when routes no longer name it (`write_routes`), so a
        // reader never sees a route pointing at a vanished or reused ring.
        self.retired_rings.push(index);
    }

    // ---- subscription control ---------------------------------------------------

    pub(super) fn subscribe(
        &mut self,
        id: u64,
        spec: DataSpec,
        visible: bool,
    ) -> Result<Applied, ControlError> {
        let (nodes, outputs) = self.intern_spec(id, &spec);
        if let Some(binding) = conflicting_binding(&spec, &outputs) {
            self.release_all(&nodes);
            return Err(ControlError::SharedBindingMismatch(binding));
        }
        for (output, &node) in spec.outputs().iter().zip(&outputs) {
            if let Some(binding) = output.binding
                && self
                    .bindings
                    .get(&binding)
                    .is_some_and(|state| state.node != node)
            {
                self.release_all(&nodes);
                return Err(ControlError::SharedBindingMismatch(binding));
            }
        }
        let mut starts = Vec::with_capacity(outputs.len());
        for (output, &node) in spec.outputs().iter().zip(&outputs) {
            // Members of an existing binding see its current History.
            let joins_existing = output.binding.is_some_and(|binding| {
                self.bindings
                    .get(&binding)
                    .is_some_and(|state| !state.members.is_empty())
            });
            starts.push(if joins_existing {
                0
            } else {
                self.current_frontier
            });
            if let Some(binding) = output.binding {
                self.bindings
                    .entry(binding)
                    .or_insert(BindingState {
                        node,
                        members: Vec::new(),
                    })
                    .members
                    .push(id);
            }
        }
        for &node in &outputs {
            self.attach_ring(node);
        }
        self.subscriptions.insert(
            id,
            Subscription {
                spec: spec.clone(),
                nodes,
                outputs,
                starts,
                revision: 1,
                visible,
                paused: false,
                reset_epoch: 0,
                gap_epoch: 0,
            },
        );
        self.flush_pcm_for(&[id]);
        self.reconcile();
        Ok(Applied { spec, revision: 1 })
    }

    /// Re-intern `spec` for subscription `id`. A binding whose output would change
    /// while other members keep it forks: this subscription continues privately.
    pub(super) fn update(&mut self, id: u64, mut spec: DataSpec) -> Result<Applied, ControlError> {
        if !self.subscriptions.contains_key(&id) {
            return Err(ControlError::SubscriptionClosed);
        }
        let (nodes, outputs) = loop {
            let (nodes, outputs) = self.intern_spec(id, &spec);
            if let Some(binding) = conflicting_binding(&spec, &outputs) {
                self.release_all(&nodes);
                return Err(ControlError::SharedBindingMismatch(binding));
            }
            let forks: Vec<usize> = spec
                .outputs()
                .iter()
                .zip(&outputs)
                .enumerate()
                .filter(|(_, (output, node))| {
                    output.binding.is_some_and(|binding| {
                        self.bindings.get(&binding).is_some_and(|state| {
                            state.node != **node && state.members.iter().any(|member| *member != id)
                        })
                    })
                })
                .map(|(index, _)| index)
                .collect();
            if forks.is_empty() {
                break (nodes, outputs);
            }
            for index in forks {
                spec.bind(super::OutputId::from_index(index), None);
            }
            self.release_all(&nodes);
        };
        self.replace(id, spec.clone(), nodes, outputs);
        let revision = self.subscriptions[&id].revision;
        self.flush_pcm_for(&[id]);
        self.reconcile();
        Ok(Applied { spec, revision })
    }

    /// Apply new specs to every member of `binding` together. Fails without any
    /// change unless all members (and other bindings they use) stay consistent.
    pub(super) fn update_shared(
        &mut self,
        binding: SharedAnalysisId,
        specs: Vec<(u64, DataSpec)>,
    ) -> Result<Vec<Applied>, ControlError> {
        let Some(state) = self.bindings.get(&binding) else {
            return Err(ControlError::SharedBindingMismatch(binding));
        };
        let mut members: Vec<u64> = state.members.clone();
        members.sort_unstable();
        members.dedup();
        let mut updated: Vec<u64> = specs.iter().map(|(id, _)| *id).collect();
        updated.sort_unstable();
        if members != updated {
            return Err(ControlError::SharedBindingMismatch(binding));
        }
        let interned: Vec<_> = specs
            .iter()
            .map(|(id, spec)| self.intern_spec(*id, spec))
            .collect();
        let mut agreed: HashMap<SharedAnalysisId, NodeId> = HashMap::new();
        let mut consistent = true;
        for ((_, spec), (_, outputs)) in specs.iter().zip(&interned) {
            for (output, &node) in spec.outputs().iter().zip(outputs) {
                let Some(bound) = output.binding else {
                    continue;
                };
                let expected = *agreed.entry(bound).or_insert(node);
                let outside = self.bindings.get(&bound).is_some_and(|state| {
                    state.node != node
                        && state.members.iter().any(|member| !updated.contains(member))
                });
                if expected != node || outside {
                    consistent = false;
                }
            }
        }
        if !consistent {
            for (nodes, _) in &interned {
                self.release_all(nodes);
            }
            return Err(ControlError::SharedBindingMismatch(binding));
        }
        let mut applied = Vec::with_capacity(specs.len());
        for ((id, spec), (nodes, outputs)) in specs.into_iter().zip(interned) {
            self.replace(id, spec.clone(), nodes, outputs);
            applied.push(Applied {
                spec,
                revision: self.subscriptions[&id].revision,
            });
        }
        self.flush_pcm_for(&updated);
        self.reconcile();
        Ok(applied)
    }

    /// Swap a subscription to already-interned nodes, then release the old ones.
    fn replace(&mut self, id: u64, spec: DataSpec, nodes: Vec<NodeId>, outputs: Vec<NodeId>) {
        let old = self
            .subscriptions
            .remove(&id)
            .expect("checked live subscription");
        self.unregister_bindings(id, &old.spec);
        for (output, &node) in spec.outputs().iter().zip(&outputs) {
            if let Some(binding) = output.binding {
                let state = self.bindings.entry(binding).or_insert(BindingState {
                    node,
                    members: Vec::new(),
                });
                state.node = node;
                state.members.push(id);
            }
        }
        for &node in &outputs {
            self.attach_ring(node);
        }
        for &node in &old.outputs {
            self.detach_ring(node);
        }
        self.release_all(&old.nodes);
        let starts = outputs
            .iter()
            .enumerate()
            .map(|(index, node)| {
                if old.outputs.get(index) == Some(node) {
                    old.starts[index]
                } else {
                    self.current_frontier
                }
            })
            .collect();
        self.subscriptions.insert(
            id,
            Subscription {
                spec,
                nodes,
                outputs,
                starts,
                revision: old.revision + 1,
                visible: old.visible,
                paused: old.paused,
                reset_epoch: old.reset_epoch,
                gap_epoch: old.gap_epoch,
            },
        );
    }

    fn unregister_bindings(&mut self, id: u64, spec: &DataSpec) {
        for output in spec.outputs() {
            if let Some(binding) = output.binding
                && let Some(state) = self.bindings.get_mut(&binding)
            {
                if let Some(position) = state.members.iter().position(|member| *member == id) {
                    state.members.swap_remove(position);
                }
                if state.members.is_empty() {
                    self.bindings.remove(&binding);
                }
            }
        }
    }

    pub(super) fn remove(&mut self, id: u64) {
        let Some(subscription) = self.subscriptions.remove(&id) else {
            return;
        };
        self.unregister_bindings(id, &subscription.spec);
        for &node in &subscription.outputs {
            self.detach_ring(node);
        }
        self.release_all(&subscription.nodes);
        // `reconcile` drops the route together with its retired rings.
        self.reconcile();
    }

    pub(super) fn set_visible(&mut self, id: u64, visible: bool) {
        if let Some(subscription) = self.subscriptions.get_mut(&id)
            && subscription.visible != visible
        {
            subscription.visible = visible;
            self.reconcile();
        }
    }

    pub(super) fn set_paused(&mut self, id: u64, paused: bool) {
        let frontier = self.current_frontier;
        let Some(subscription) = self.subscriptions.get_mut(&id) else {
            return;
        };
        if subscription.paused == paused {
            return;
        }
        subscription.paused = paused;
        if !paused {
            // Resume reading from now; what was not recorded is not claimed.
            subscription.starts.fill(frontier);
            subscription.gap_epoch += 1;
            self.flush_pcm_for(&[id]);
        }
        self.reconcile();
    }

    /// Reset the stateful nodes in this subscription's scope (shared ones for every
    /// member) and restart reading for this subscription and every other one that
    /// reads a reset node. Pure shared outputs are never cleared for others.
    pub(super) fn reset_state(&mut self, id: u64) {
        let Some(subscription) = self.subscriptions.get(&id) else {
            return;
        };
        let has_history = subscription
            .spec
            .outputs()
            .iter()
            .any(|output| matches!(output.keep, Keep::Seconds(_)));
        let reset_nodes: HashSet<NodeId> = subscription
            .nodes
            .iter()
            .copied()
            .filter(|&node| self.node(node).key.scope != StateScope::Pure)
            .collect();
        let frontier = self.current_frontier;
        for &node in &reset_nodes {
            let node = self.node_mut(node);
            let window = node.fft.map_or(0, |fft| fft.window_size as u64);
            match &mut node.kernel {
                Kernel::Smooth { state, .. } => state.reset(),
                Kernel::Peak { state, .. } => state.reset(),
                Kernel::Levels {
                    analyzer,
                    momentary_lufs,
                    interval_peaks,
                    start_frame,
                    ..
                } => {
                    analyzer.reset();
                    momentary_lufs.fill(-60.0);
                    interval_peaks.fill(0.0);
                    *start_frame = frontier;
                }
                _ => {}
            }
            node.accept_from = frontier + window;
        }
        let mut affected: Vec<u64> = self
            .subscriptions
            .iter()
            .filter(|(other, other_subscription)| {
                (**other == id && (has_history || !reset_nodes.is_empty()))
                    || other_subscription
                        .nodes
                        .iter()
                        .any(|node| reset_nodes.contains(node))
            })
            .map(|(other, _)| *other)
            .collect();
        affected.sort_unstable();
        for other in &affected {
            let subscription = self
                .subscriptions
                .get_mut(other)
                .expect("affected subscription is live");
            subscription.starts.fill(frontier);
            subscription.reset_epoch += 1;
        }
        self.flush_pcm_for(&affected);
        self.write_routes();
    }

    /// Before a reading boundary moves to the current frontier, publish merged PCM
    /// that started earlier, so no chunk straddles the boundary.
    fn flush_pcm_for(&mut self, subscriptions: &[u64]) {
        let Some(source) = self.plan.source else {
            return;
        };
        let reads_pcm = subscriptions.iter().any(|id| {
            self.subscriptions
                .get(id)
                .is_some_and(|subscription| subscription.outputs.contains(&source))
        });
        if reads_pcm && self.node(source).ring_demanded {
            self.stage_pcm(source);
            self.publish();
        }
    }

    // ---- demand, activation and plan --------------------------------------------

    fn reconcile(&mut self) {
        let previously_demanded: HashSet<NodeId> = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.as_ref().is_some_and(|node| node.ring_demanded))
            .map(|(id, _)| id)
            .collect();
        for node in self.nodes.iter_mut().flatten() {
            node.ring_demanded = false;
        }
        let mut demanded: HashSet<NodeId> = HashSet::new();
        let mut demanded_outputs = Vec::new();
        for subscription in self.subscriptions.values() {
            if subscription.paused {
                continue;
            }
            for (output, &node) in subscription
                .spec
                .outputs()
                .iter()
                .zip(&subscription.outputs)
            {
                let continuous =
                    !matches!(output.keep, Keep::Latest) || self.node(node).stateful_chain;
                if continuous || subscription.visible {
                    demanded_outputs.push(node);
                }
            }
        }
        for node in demanded_outputs {
            self.node_mut(node).ring_demanded = true;
            let mut current = Some(node);
            while let Some(id) = current {
                if !demanded.insert(id) {
                    break;
                }
                current = self.node(id).input;
            }
        }
        let live: Vec<NodeId> = (0..self.nodes.len())
            .filter(|id| self.nodes[*id].is_some())
            .collect();
        for id in live {
            let wanted = demanded.contains(&id);
            let active = self.node(id).active;
            if wanted && !active {
                self.activate(id);
            } else if !wanted && active {
                self.deactivate(id);
            }
        }
        for (id, node) in self.nodes.iter_mut().enumerate() {
            let Some(node) = node else {
                continue;
            };
            let was = previously_demanded.contains(&id);
            if was && !node.ring_demanded {
                // Merged PCM must not span the period nobody read it.
                if let Kernel::Source {
                    pending,
                    pending_frames,
                    ..
                } = &mut node.kernel
                {
                    pending.clear();
                    *pending_frames = 0;
                }
            } else if !was && node.ring_demanded {
                if node.ring_served
                    && let Some(ring) = &mut node.ring
                {
                    ring.gap_pending = true;
                }
                node.ring_served = true;
            }
        }
        self.build_plan();
        let time_demand = !self.plan.levels.is_empty()
            || self
                .plan
                .source
                .is_some_and(|source| self.node(source).ring_demanded);
        match (time_demand, self.clock) {
            (true, None) => self.clock = Some(0),
            (false, Some(_)) => self.clock = None,
            _ => {}
        }
        self.recompute_ring_keeps();
        self.write_routes();
        self.update_gauges();
    }

    fn activate(&mut self, id: NodeId) {
        let frontier = self.current_frontier;
        let store = Arc::clone(&self.store);
        let node = self.node_mut(id);
        node.active = true;
        let reactivated = node.activated_before;
        node.activated_before = true;
        let window = node.fft.map_or(0, |fft| fft.window_size as u64);
        match &mut node.kernel {
            Kernel::Fft(fft) => fft.reset(),
            // A new or reactivated stateful node only accepts FFT windows that lie
            // entirely after its activation, even when a shared FFT is already warm.
            Kernel::Smooth { state, .. } => {
                if reactivated {
                    state.reset();
                }
                node.accept_from = frontier + window;
            }
            Kernel::Peak { state, .. } => {
                if reactivated {
                    state.reset();
                }
                node.accept_from = frontier + window;
            }
            Kernel::Levels {
                analyzer,
                momentary_lufs,
                interval_peaks,
                start_frame,
                ..
            } if reactivated => {
                analyzer.reset();
                momentary_lufs.fill(-60.0);
                interval_peaks.fill(0.0);
                *start_frame = frontier;
            }
            _ => {}
        }
        if reactivated && let Some(ring) = &mut node.ring {
            ring.gap_pending = true;
            if ring.keep_frames.is_none() {
                let index = ring.index;
                let stale = lock(&store).rings[index]
                    .as_mut()
                    .map(|ring| std::mem::take(&mut ring.frames));
                drop(stale);
            }
        }
    }

    fn deactivate(&mut self, id: NodeId) {
        let node = self.node_mut(id);
        node.active = false;
        node.produced = false;
        if let Kernel::Source {
            pending,
            pending_frames,
            ..
        } = &mut node.kernel
        {
            pending.clear();
            *pending_frames = 0;
        }
    }

    fn build_plan(&mut self) {
        let mut plan = Plan::default();
        for (id, node) in self.nodes.iter().enumerate() {
            let Some(node) = node else {
                continue;
            };
            match node.kernel {
                Kernel::Source { .. } => plan.source = Some(id),
                Kernel::Channel { .. } if node.active => plan.channels.push(id),
                Kernel::Levels { .. } if node.active => plan.levels.push(id),
                Kernel::Fft(_) if node.active => {
                    let mut order = Vec::new();
                    let mut stack: Vec<NodeId> = node.children.iter().rev().copied().collect();
                    while let Some(child) = stack.pop() {
                        let child_node = self.node(child);
                        if !child_node.active {
                            continue;
                        }
                        order.push(child);
                        stack.extend(child_node.children.iter().rev().copied());
                    }
                    plan.ffts.push((id, order));
                }
                _ => {}
            }
        }
        self.plan = plan;
    }

    fn recompute_ring_keeps(&mut self) {
        let mut keeps: HashMap<NodeId, Option<u64>> = HashMap::new();
        for subscription in self.subscriptions.values() {
            for (output, &node) in subscription
                .spec
                .outputs()
                .iter()
                .zip(&subscription.outputs)
            {
                let keep = self.keep_frames(output.keep);
                let entry = keeps.entry(node).or_insert(None);
                *entry = (*entry).max(keep);
            }
        }
        for (node, keep) in keeps {
            if let Some(ring) = &mut self.node_mut(node).ring {
                ring.keep_frames = keep;
            }
        }
    }

    /// Publish every subscription's route and free the ring slots no route names any
    /// more, all under one lock, so readers switch from old to new outputs at once.
    fn write_routes(&mut self) {
        let mut stale = Vec::new();
        {
            let mut store = lock(&self.store);
            store
                .routes
                .retain(|id, _| self.subscriptions.contains_key(id));
            for (&id, subscription) in &self.subscriptions {
                let outputs: Vec<RouteOutput> = subscription
                    .spec
                    .outputs()
                    .iter()
                    .zip(&subscription.outputs)
                    .zip(&subscription.starts)
                    .map(|((output, &node), &start_frame)| RouteOutput {
                        ring: self.node(node).ring.as_ref().expect("output ring").index,
                        start_frame,
                        keep_frames: self.keep_frames(output.keep),
                        reset_epoch: subscription.reset_epoch,
                        gap_epoch: subscription.gap_epoch,
                    })
                    .collect();
                // A paused subscription keeps showing what was visible when it paused.
                let frozen = if !subscription.paused {
                    Vec::new()
                } else if let Some(previous) = store.routes.get(&id).filter(|route| route.paused) {
                    previous.frozen.clone()
                } else {
                    outputs
                        .iter()
                        .map(|output| {
                            store.rings[output.ring]
                                .as_ref()
                                .and_then(|ring| ring.frames.back())
                                .filter(|frame| frame.processed_through_frame > output.start_frame)
                                .cloned()
                        })
                        .collect()
                };
                store.routes.insert(
                    id,
                    Route {
                        revision: subscription.revision,
                        paused: subscription.paused,
                        outputs,
                        frozen,
                    },
                );
            }
            for index in self.retired_rings.drain(..) {
                stale.push(store.rings[index].take());
                self.free_rings.push(index);
            }
        }
        // Readers may still hold frames; this only releases our references.
        drop(stale);
    }

    fn update_gauges(&self) {
        let mut nodes = 0;
        let mut channels = 0;
        let mut ffts = 0;
        let mut spectrum = 0;
        let mut levels = 0;
        let mut rings = 0;
        for node in self.nodes.iter().flatten() {
            nodes += 1;
            rings += u64::from(node.ring.is_some());
            if !node.active {
                continue;
            }
            match node.kernel {
                Kernel::Channel { .. } => channels += 1,
                Kernel::Fft(_) => ffts += 1,
                Kernel::Levels { .. } => levels += 1,
                ref kernel if kernel.is_spectrum_node() => spectrum += 1,
                _ => {}
            }
        }
        let metrics = &self.metrics;
        metrics
            .subscriptions
            .store(self.subscriptions.len() as u64, Ordering::Release);
        metrics.nodes.store(nodes, Ordering::Release);
        metrics
            .active_channel_nodes
            .store(channels, Ordering::Release);
        metrics.active_fft_nodes.store(ffts, Ordering::Release);
        metrics
            .active_spectrum_nodes
            .store(spectrum, Ordering::Release);
        metrics.active_level_nodes.store(levels, Ordering::Release);
        metrics.rings.store(rings, Ordering::Release);
    }

    // ---- continuity -------------------------------------------------------------

    pub(super) fn resume_at(&mut self, continuity: u64) {
        self.minimum_continuity = continuity;
        self.active_continuity = continuity;
        self.clear_latest_rings();
        self.reset_continuity();
    }

    fn clear_latest_rings(&mut self) {
        let mut store = lock(&self.store);
        let mut stale = Vec::new();
        for node in self.nodes.iter().flatten() {
            if let Some(ring) = &node.ring
                && ring.keep_frames.is_none()
                && let Some(Some(published)) = store.rings.get_mut(ring.index)
            {
                stale.extend(published.frames.drain(..));
            }
        }
        drop(store);
        drop(stale);
    }

    fn reset_continuity(&mut self) {
        for node in self.nodes.iter_mut().flatten() {
            match &mut node.kernel {
                Kernel::Fft(fft) => fft.reset(),
                Kernel::Smooth { state, .. } => state.reset(),
                Kernel::Peak { state, .. } => state.reset(),
                Kernel::Levels {
                    analyzer,
                    momentary_lufs,
                    interval_peaks,
                    ..
                } => {
                    analyzer.reset();
                    momentary_lufs.fill(-60.0);
                    interval_peaks.fill(0.0);
                }
                Kernel::Source {
                    pending,
                    pending_frames,
                    ..
                } => {
                    pending.clear();
                    *pending_frames = 0;
                }
                _ => {}
            }
            node.accept_from = 0;
            if let Some(ring) = &mut node.ring {
                ring.gap_pending = true;
            }
        }
        if let Some(clock) = &mut self.clock {
            *clock = 0;
        }
        self.expected_sequence = None;
        self.expected_start_frame = None;
        self.discontinuity_count = self.discontinuity_count.wrapping_add(1);
    }

    // ---- per block --------------------------------------------------------------

    pub(super) fn process_block(&mut self, block: &AudioBlock) {
        if block.continuity < self.minimum_continuity {
            return;
        }
        if block.continuity != self.active_continuity {
            self.minimum_continuity = block.continuity;
            self.active_continuity = block.continuity;
            self.clear_latest_rings();
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

        let channels = self.descriptor.channel_count as usize;
        let samples = block.samples(channels);
        let start = block.start_frame;
        let frame_count = block.frame_count;
        let end = start.saturating_add(frame_count as u64);

        if let Some(source) = self.plan.source
            && self.node(source).ring_demanded
            && frame_count > 0
        {
            if let Kernel::Source {
                pending,
                pending_start,
                pending_frames,
                pending_sequence,
            } = &mut self.node_mut(source).kernel
            {
                if *pending_frames == 0 {
                    *pending_start = start;
                    *pending_sequence = block.sequence;
                }
                pending.extend_from_slice(samples);
                *pending_frames += frame_count;
            }
            self.metrics
                .pcm_frames
                .fetch_add(frame_count as u64, Ordering::Relaxed);
        }

        for index in 0..self.plan.channels.len() {
            let id = self.plan.channels[index];
            if let Kernel::Channel { resolved, out } = &mut self.node_mut(id).kernel {
                let resolved = *resolved;
                out.clear();
                out.extend(
                    samples
                        .chunks_exact(channels)
                        .map(|frame| resolved.sample(frame)),
                );
            }
        }

        let mut level_frames = 0;
        for index in 0..self.plan.levels.len() {
            let id = self.plan.levels[index];
            if let Kernel::Levels {
                analyzer,
                momentary_lufs,
                block_peaks,
                interval_peaks,
                start_frame,
            } = &mut self.node_mut(id).kernel
            {
                if end <= *start_frame {
                    continue;
                }
                let first = (start.max(*start_frame) - start) as usize;
                analyzer.process(&samples[first * channels..], momentary_lufs, block_peaks);
                for (interval, block) in interval_peaks.iter_mut().zip(block_peaks.iter()) {
                    *interval = interval.max(*block);
                }
                level_frames += (frame_count - first) as u64;
            }
        }
        if level_frames > 0 {
            self.metrics
                .level_frames
                .fetch_add(level_frames, Ordering::Relaxed);
        }

        let interval = self.publish_interval_frames;
        let tick = match &mut self.clock {
            Some(clock) if frame_count > 0 => {
                *clock += frame_count;
                let due = *clock >= interval;
                if due {
                    *clock = 0;
                }
                due
            }
            _ => false,
        };
        if tick {
            if let Some(source) = self.plan.source
                && self.node(source).ring_demanded
            {
                self.stage_pcm(source);
            }
            let context = StageContext {
                processed_through_frame: end,
                source_sequence: block.sequence,
                discontinuity_count: self.discontinuity_count,
            };
            for index in 0..self.plan.levels.len() {
                let id = self.plan.levels[index];
                self.stage_levels(id, &context);
            }
        }

        for fft_index in 0..self.plan.ffts.len() {
            let fft_id = self.plan.ffts[fft_index].0;
            let channel = self.node(fft_id).input.expect("an FFT reads a channel");
            let mono = match &mut self.node_mut(channel).kernel {
                Kernel::Channel { out, .. } => std::mem::take(out),
                _ => unreachable!("an FFT reads a channel"),
            };
            let mut position = 0;
            while position < mono.len() {
                let Kernel::Fft(fft) = &mut self.node_mut(fft_id).kernel else {
                    unreachable!("planned FFT node");
                };
                let Some(consumed) = fft.push(&mono[position..]) else {
                    break;
                };
                position += consumed;
                self.metrics.fft_evaluations.fetch_add(1, Ordering::Relaxed);
                let context = StageContext {
                    processed_through_frame: start + position as u64,
                    source_sequence: block.sequence,
                    discontinuity_count: self.discontinuity_count,
                };
                self.run_hop(fft_index, &context);
            }
            if let Kernel::Channel { out, .. } = &mut self.node_mut(channel).kernel {
                *out = mono;
            }
        }

        self.current_frontier = end;
        self.publish();
    }

    /// Run one FFT result through its active descendants and stage their outputs.
    fn run_hop(&mut self, fft_index: usize, context: &StageContext) {
        let fft_id = self.plan.ffts[fft_index].0;
        self.node_mut(fft_id).produced = true;
        self.stage_spectrum(fft_id, context);
        let sample_rate = self.descriptor.sample_rate_hz;
        let mut evaluations = 0;
        for order_index in 0..self.plan.ffts[fft_index].1.len() {
            let id = self.plan.ffts[fft_index].1[order_index];
            let parent = self.node(id).input.expect("spectrum nodes have an input");
            let (parent, node) = parent_and_child(&mut self.nodes, parent, id);
            node.produced = parent.produced && context.processed_through_frame >= node.accept_from;
            if !node.produced {
                continue;
            }
            let input = parent.kernel.spectrum();
            let fft = node.fft.expect("spectrum nodes follow an FFT");
            let floor = fft.floor_dbfs;
            match &mut node.kernel {
                Kernel::Tilt {
                    db_per_octave,
                    table,
                    out,
                } => {
                    if table.len() != input.len() {
                        tilt_table(table, input.len(), fft.bin_hz, *db_per_octave);
                    }
                    offset_with_floor(out, input, table, floor);
                }
                Kernel::Smooth { config, state } => {
                    state.process(input, context.processed_through_frame, sample_rate, *config);
                }
                Kernel::Gain { db, out } => add_with_floor(out, input, *db, floor),
                Kernel::Peak { config, state } => {
                    state.process(input, context.processed_through_frame, sample_rate, *config);
                }
                _ => unreachable!("only spectrum nodes follow an FFT"),
            }
            node.accept_from = 0;
            evaluations += 1;
            self.stage_spectrum(id, context);
        }
        self.metrics
            .spectrum_evaluations
            .fetch_add(evaluations, Ordering::Relaxed);
    }

    fn next_version(&mut self) -> u64 {
        self.result_version = self.result_version.wrapping_add(1);
        self.result_version
    }

    fn stage_spectrum(&mut self, id: NodeId, context: &StageContext) {
        if self.node(id).ring.is_none() {
            return;
        }
        let version = self.next_version();
        let node = self.node_mut(id);
        let fft = node.fft.expect("spectrum outputs follow an FFT");
        let ring = node.ring.as_mut().expect("checked ring");
        let mut frame = take_frame(&mut ring.pool, || {
            FrameData::Spectrum(SpectrumData {
                fft,
                bins_dbfs: Vec::new(),
            })
        });
        let target = Arc::get_mut(&mut frame).expect("pooled frames are unshared");
        target.result_version = version;
        target.processed_through_frame = context.processed_through_frame;
        target.source_sequence = context.source_sequence;
        target.discontinuity_count = context.discontinuity_count;
        target.discontinuity_before = std::mem::take(&mut ring.gap_pending);
        if let FrameData::Spectrum(data) = &mut target.data {
            data.fft = fft;
            data.bins_dbfs.clear();
            data.bins_dbfs.extend_from_slice(node.kernel.spectrum());
        }
        self.staged.push((id, frame));
    }

    fn stage_levels(&mut self, id: NodeId, context: &StageContext) {
        let version = self.next_version();
        let node = self.node_mut(id);
        let Kernel::Levels {
            momentary_lufs,
            interval_peaks,
            start_frame,
            ..
        } = &mut node.kernel
        else {
            return;
        };
        if context.processed_through_frame <= *start_frame {
            return;
        }
        let Some(ring) = node.ring.as_mut() else {
            interval_peaks.fill(0.0);
            return;
        };
        let mut frame = take_frame(&mut ring.pool, || FrameData::Levels(LevelsData::default()));
        let target = Arc::get_mut(&mut frame).expect("pooled frames are unshared");
        target.result_version = version;
        target.processed_through_frame = context.processed_through_frame;
        target.source_sequence = context.source_sequence;
        target.discontinuity_count = context.discontinuity_count;
        target.discontinuity_before = std::mem::take(&mut ring.gap_pending);
        if let FrameData::Levels(data) = &mut target.data {
            data.momentary_lufs.clone_from(momentary_lufs);
            data.sample_peaks.clone_from(interval_peaks);
        }
        interval_peaks.fill(0.0);
        self.staged.push((id, frame));
    }

    /// Publish the merged PCM as one chunk, swapping buffers with a pooled frame.
    fn stage_pcm(&mut self, source: NodeId) {
        let pending_frames = match &self.node(source).kernel {
            Kernel::Source { pending_frames, .. } => *pending_frames,
            _ => 0,
        };
        if pending_frames == 0 || self.node(source).ring.is_none() {
            return;
        }
        let version = self.next_version();
        let discontinuity_count = self.discontinuity_count;
        let node = self.node_mut(source);
        let Kernel::Source {
            pending,
            pending_start,
            pending_frames,
            pending_sequence,
        } = &mut node.kernel
        else {
            return;
        };
        let ring = node.ring.as_mut().expect("checked ring");
        let mut frame = take_frame(&mut ring.pool, || FrameData::Pcm(AudioChunk::default()));
        let target = Arc::get_mut(&mut frame).expect("pooled frames are unshared");
        target.result_version = version;
        target.processed_through_frame = *pending_start + *pending_frames as u64;
        target.source_sequence = *pending_sequence;
        target.discontinuity_count = discontinuity_count;
        target.discontinuity_before = std::mem::take(&mut ring.gap_pending);
        if let FrameData::Pcm(chunk) = &mut target.data {
            chunk.start_frame = *pending_start;
            chunk.frame_count = *pending_frames;
            std::mem::swap(&mut chunk.interleaved, pending);
            pending.clear();
        }
        *pending_frames = 0;
        self.staged.push((source, frame));
    }

    /// Push staged frames into their rings under one lock, trim each ring to its
    /// kept duration, and return evicted frames to the pools.
    fn publish(&mut self) {
        if self.staged.is_empty() {
            return;
        }
        let published = self.staged.len() as u64;
        {
            let mut store = lock(&self.store);
            for (node, frame) in self.staged.drain(..) {
                let Some(attach) = self.nodes[node]
                    .as_ref()
                    .and_then(|node| node.ring.as_ref())
                else {
                    continue;
                };
                let Some(ring) = store.rings[attach.index].as_mut() else {
                    continue;
                };
                let newest = frame.processed_through_frame;
                ring.frames.push_back(frame);
                match attach.keep_frames {
                    None => {
                        while ring.frames.len() > 1 {
                            let old = ring.frames.pop_front().expect("non-empty ring");
                            self.evicted.push((attach.index, old));
                        }
                    }
                    Some(keep) => {
                        let window_start = newest.saturating_sub(keep);
                        while ring.frames.len() > 1
                            && ring.frames.front().is_some_and(|oldest| {
                                oldest.processed_through_frame <= window_start
                            })
                        {
                            let old = ring.frames.pop_front().expect("non-empty ring");
                            self.evicted.push((attach.index, old));
                        }
                    }
                }
            }
        }
        for (ring_index, frame) in self.evicted.drain(..) {
            let Some(node) = self.ring_owners[ring_index] else {
                continue;
            };
            if let Some(ring) = self.nodes[node]
                .as_mut()
                .and_then(|node| node.ring.as_mut())
                && ring.pool.len() < POOL_CAPACITY
            {
                ring.pool.push(frame);
            }
        }
        self.metrics
            .publications
            .fetch_add(published, Ordering::Relaxed);
    }
}
