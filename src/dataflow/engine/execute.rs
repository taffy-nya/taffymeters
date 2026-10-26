use std::sync::{Arc, atomic::Ordering};

use crate::{
    audio::AudioBlock,
    dataflow::{
        kernels::{add_with_floor, offset_with_floor, tilt_table},
        store::lock,
        time::{accumulate_peaks, peak_dbfs},
    },
    snapshot::{AudioChunk, Frame, FrameData, LevelsData, LoudnessData, SpectrumData},
};

use super::{Engine, Kernel, Node, NodeId, POOL_CAPACITY};

fn parent_and_child(
    nodes: &mut [Option<Node>],
    parent: NodeId,
    child: NodeId,
) -> (&Node, &mut Node) {
    assert_ne!(parent, child);
    if parent < child {
        let (head, tail) = nodes.split_at_mut(child);
        (
            head[parent]
                .as_ref()
                .expect("planned parents refer to live nodes"),
            tail[0]
                .as_mut()
                .expect("planned children refer to live nodes"),
        )
    } else {
        let (head, tail) = nodes.split_at_mut(parent);
        (
            tail[0]
                .as_ref()
                .expect("planned parents refer to live nodes"),
            head[child]
                .as_mut()
                .expect("planned children refer to live nodes"),
        )
    }
}

/// Readers retaining every pooled frame force a new allocation.
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
    /// Before a reading boundary moves to the current frontier, publish merged PCM
    /// that started earlier, so no chunk straddles the boundary.
    pub(super) fn flush_pcm_for(&mut self, subscriptions: &[u64]) {
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

    pub(crate) fn resume_at(&mut self, continuity: u64) {
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
                kernel @ (Kernel::Levels { .. } | Kernel::Loudness { .. }) => {
                    kernel.reset_time_domain(None);
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

    /// Reuses warm buffers and pooled frames; readers must release frames for allocation-free reuse.
    pub(crate) fn process_block(&mut self, block: &AudioBlock) {
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
            let id = self.plan.levels[index].0;
            if let Kernel::Levels {
                interval_peaks,
                start_frame,
                ..
            } = &mut self.node_mut(id).kernel
            {
                if end <= *start_frame {
                    continue;
                }
                let first = (start.max(*start_frame) - start) as usize;
                accumulate_peaks(&samples[first * channels..], interval_peaks);
                level_frames += (frame_count - first) as u64;
            }
        }
        for index in 0..self.plan.loudness.len() {
            let id = self.plan.loudness[index];
            if let Kernel::Loudness {
                analyzer,
                start_frame,
            } = &mut self.node_mut(id).kernel
            {
                if end <= *start_frame {
                    continue;
                }
                let first = (start.max(*start_frame) - start) as usize;
                analyzer.process(&samples[first * channels..]);
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
                self.run_levels(index, &context);
            }
            for index in 0..self.plan.loudness.len() {
                let id = self.plan.loudness[index];
                self.stage_loudness(id, &context);
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

    fn run_hop(&mut self, fft_index: usize, context: &StageContext) {
        let fft_id = self.plan.ffts[fft_index].0;
        self.node_mut(fft_id).produced = true;
        self.stage_values(fft_id, context);
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
            let input = parent.kernel.values();
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
            self.stage_values(id, context);
        }
        self.metrics
            .spectrum_evaluations
            .fetch_add(evaluations, Ordering::Relaxed);
    }

    fn run_levels(&mut self, levels_index: usize, context: &StageContext) {
        let levels_id = self.plan.levels[levels_index].0;
        let node = self.node_mut(levels_id);
        let Kernel::Levels {
            interval_peaks,
            dbfs,
            start_frame,
        } = &mut node.kernel
        else {
            unreachable!("planned Levels node");
        };
        node.produced = context.processed_through_frame > *start_frame;
        if !node.produced {
            return;
        }
        for (value, peak) in dbfs.iter_mut().zip(interval_peaks.iter()) {
            *value = peak_dbfs(*peak);
        }
        interval_peaks.fill(0.0);
        self.stage_values(levels_id, context);
        let sample_rate = self.descriptor.sample_rate_hz;
        for order_index in 0..self.plan.levels[levels_index].1.len() {
            let id = self.plan.levels[levels_index].1[order_index];
            let parent = self.node(id).input.expect("levels readers have an input");
            let (parent, node) = parent_and_child(&mut self.nodes, parent, id);
            node.produced = parent.produced && context.processed_through_frame >= node.accept_from;
            if !node.produced {
                continue;
            }
            let input = parent.kernel.values();
            match &mut node.kernel {
                Kernel::Peak { config, state } => {
                    state.process(input, context.processed_through_frame, sample_rate, *config);
                }
                _ => unreachable!("only Peak nodes follow Levels"),
            }
            node.accept_from = 0;
            self.stage_values(id, context);
        }
    }

    fn next_version(&mut self) -> u64 {
        self.result_version = self.result_version.wrapping_add(1);
        self.result_version
    }

    fn stage_values(&mut self, id: NodeId, context: &StageContext) {
        if self.node(id).ring.is_none() {
            return;
        }
        let version = self.next_version();
        let node = self.node_mut(id);
        let fft = node.fft;
        let ring = node
            .ring
            .as_mut()
            .expect("nodes without rings return before staging");
        let mut frame = take_frame(&mut ring.pool, || match fft {
            Some(fft) => FrameData::Spectrum(SpectrumData {
                fft,
                bins_dbfs: Vec::new(),
            }),
            None => FrameData::Levels(LevelsData::default()),
        });
        let target = Arc::get_mut(&mut frame).expect("pooled frames are unshared");
        target.result_version = version;
        target.processed_through_frame = context.processed_through_frame;
        target.source_sequence = context.source_sequence;
        target.discontinuity_count = context.discontinuity_count;
        target.discontinuity_before = std::mem::take(&mut ring.gap_pending);
        let values = match &mut target.data {
            FrameData::Spectrum(data) => {
                data.fft = fft.expect("spectrum frames belong to an FFT chain");
                &mut data.bins_dbfs
            }
            FrameData::Levels(data) => &mut data.dbfs,
            _ => unreachable!("a node ring holds one kind of frame"),
        };
        values.clear();
        values.extend_from_slice(node.kernel.values());
        self.staged.push((id, frame));
    }

    fn stage_loudness(&mut self, id: NodeId, context: &StageContext) {
        let node = self.node(id);
        let Kernel::Loudness {
            analyzer,
            start_frame,
        } = &node.kernel
        else {
            unreachable!("planned Loudness node");
        };
        if context.processed_through_frame <= *start_frame || node.ring.is_none() {
            return;
        }
        let momentary_lufs = analyzer.lufs();
        let version = self.next_version();
        let ring = self
            .node_mut(id)
            .ring
            .as_mut()
            .expect("nodes without rings return before staging");
        let mut frame = take_frame(&mut ring.pool, || {
            FrameData::Loudness(LoudnessData::default())
        });
        let target = Arc::get_mut(&mut frame).expect("pooled frames are unshared");
        target.result_version = version;
        target.processed_through_frame = context.processed_through_frame;
        target.source_sequence = context.source_sequence;
        target.discontinuity_count = context.discontinuity_count;
        target.discontinuity_before = std::mem::take(&mut ring.gap_pending);
        target.data = FrameData::Loudness(LoudnessData { momentary_lufs });
        self.staged.push((id, frame));
    }

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
        let ring = node
            .ring
            .as_mut()
            .expect("nodes without rings return before staging");
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
                            let old = ring
                                .frames
                                .pop_front()
                                .expect("the loop only removes frames while more than one remains");
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
                            let old = ring
                                .frames
                                .pop_front()
                                .expect("the loop only removes frames while more than one remains");
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
