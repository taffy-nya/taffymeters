use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, atomic::Ordering},
};

use crate::dataflow::{
    spec::Keep,
    store::{Ring, Route, RouteOutput, lock},
};

use super::{Engine, Kernel, Node, NodeId, POOL_CAPACITY, Plan, RingAttach};

impl Engine {
    fn keep_frames(&self, keep: Keep) -> Option<u64> {
        match keep {
            Keep::Latest => None,
            Keep::Seconds(seconds) => {
                Some((f64::from(seconds) * f64::from(self.descriptor.sample_rate_hz)).ceil() as u64)
            }
        }
    }

    pub(super) fn attach_ring(&mut self, node: NodeId) {
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

    pub(super) fn detach_ring(&mut self, node: NodeId) {
        let ring = self
            .node_mut(node)
            .ring
            .as_mut()
            .expect("subscription outputs have attached rings");
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

    pub(super) fn reconcile(&mut self) {
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
            || !self.plan.loudness.is_empty()
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
            kernel if reactivated => kernel.reset_time_domain(Some(frontier)),
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
                Kernel::Levels { .. } if node.active => {
                    plan.levels.push((id, self.active_descendants(node)));
                }
                Kernel::Loudness { .. } if node.active => plan.loudness.push(id),
                Kernel::Fft(_) if node.active => {
                    plan.ffts.push((id, self.active_descendants(node)));
                }
                _ => {}
            }
        }
        self.plan = plan;
    }

    fn active_descendants(&self, node: &Node) -> Vec<NodeId> {
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
        order
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
    pub(super) fn write_routes(&mut self) {
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
                        ring: self
                            .node(node)
                            .ring
                            .as_ref()
                            .expect("subscription outputs have attached rings")
                            .index,
                        start_frame,
                        keep_frames: self.keep_frames(output.keep),
                        reset_epoch: subscription.reset_epoch,
                        gap_epoch: subscription.gap_epoch,
                    })
                    .collect();
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
                Kernel::Tilt { .. } | Kernel::Smooth { .. } | Kernel::Gain { .. } => spectrum += 1,
                Kernel::Peak { .. } if node.fft.is_some() => spectrum += 1,
                Kernel::Levels { .. } | Kernel::Loudness { .. } | Kernel::Peak { .. } => {
                    levels += 1;
                }
                Kernel::Source { .. } => {}
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
}
