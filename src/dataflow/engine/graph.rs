use std::collections::{HashMap, HashSet};

use crate::dataflow::{
    ControlError, FftMetadata, OutputId,
    fft::StreamingFft,
    kernels::{PeakState, SmoothState},
    spec::{DataSpec, Keep, NodeSpec, SharedAnalysisId},
    time::{LEVELS_FLOOR_DBFS, LoudnessAnalyzer},
};

use super::{
    Applied, BindingState, Engine, Kernel, Node, NodeId, NodeKey, SpecKey, StateScope, Subscription,
};

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

impl Engine {
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
                interval_peaks: vec![0.0; channels],
                dbfs: vec![LEVELS_FLOOR_DBFS; channels],
                start_frame: self.current_frontier,
            },
            Some(NodeSpec::Loudness) => Kernel::Loudness {
                analyzer: LoudnessAnalyzer::new(sample_rate, channels),
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
        let node = self.nodes[id]
            .take()
            .expect("the node remains live until its last reference is released");
        self.index.remove(&node.key);
        if let Some(input) = node.input {
            self.node_mut(input).children.retain(|child| *child != id);
        }
        self.free_nodes.push(id);
    }

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

    fn release_all(&mut self, ids: &[NodeId]) {
        for &id in ids.iter().rev() {
            self.release(id);
        }
    }

    pub(crate) fn subscribe(
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

    pub(crate) fn update(&mut self, id: u64, mut spec: DataSpec) -> Result<Applied, ControlError> {
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
                spec.bind(OutputId::from_index(index), None);
            }
            self.release_all(&nodes);
        };
        self.replace(id, spec.clone(), nodes, outputs);
        let revision = self.subscriptions[&id].revision;
        self.flush_pcm_for(&[id]);
        self.reconcile();
        Ok(Applied { spec, revision })
    }

    /// All members switch together; inconsistent bindings leave every member unchanged.
    pub(crate) fn update_shared(
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

    fn replace(&mut self, id: u64, spec: DataSpec, nodes: Vec<NodeId>, outputs: Vec<NodeId>) {
        let old = self
            .subscriptions
            .remove(&id)
            .expect("subscriptions are checked before replacement");
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

    pub(crate) fn remove(&mut self, id: u64) {
        let Some(subscription) = self.subscriptions.remove(&id) else {
            return;
        };
        self.unregister_bindings(id, &subscription.spec);
        for &node in &subscription.outputs {
            self.detach_ring(node);
        }
        self.release_all(&subscription.nodes);
        self.reconcile();
    }

    pub(crate) fn set_visible(&mut self, id: u64, visible: bool) {
        if let Some(subscription) = self.subscriptions.get_mut(&id)
            && subscription.visible != visible
        {
            subscription.visible = visible;
            self.reconcile();
        }
    }

    pub(crate) fn set_paused(&mut self, id: u64, paused: bool) {
        let frontier = self.current_frontier;
        let Some(subscription) = self.subscriptions.get_mut(&id) else {
            return;
        };
        if subscription.paused == paused {
            return;
        }
        subscription.paused = paused;
        if !paused {
            subscription.starts.fill(frontier);
            subscription.gap_epoch += 1;
            self.flush_pcm_for(&[id]);
        }
        self.reconcile();
    }

    /// Shared state resets for all readers of the affected nodes, restarting their histories.
    /// Pure shared outputs are not cleared for other subscriptions.
    pub(crate) fn reset_state(&mut self, id: u64) {
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
                kernel => kernel.reset_time_domain(Some(frontier)),
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
}
