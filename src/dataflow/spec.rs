//! `DataSpec`: the value a Module declares for its audio data. It names the
//! processing chain (nodes and their parameters), the outputs the Module reads and
//! the explicit sharing of outputs. The same value is the source of the Module's
//! analysis settings and of what persistence will save; the runtime interns it into
//! the shared node graph.

use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};

use crate::audio::AudioDescriptor;

use super::{ChannelError, ChannelSelection, FftConfig};

/// Explicit sharing of one output and the stateful nodes it depends on. A logical
/// value: persistence renumbers it per file, it is never a runtime handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SharedAnalysisId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SmoothConfig {
    pub attack_ms: f32,
    pub release_ms: f32,
}

impl SmoothConfig {
    pub fn new(attack_ms: f32, release_ms: f32) -> Result<Self, SpecError> {
        let value = Self {
            attack_ms,
            release_ms,
        };
        value.validated()
    }

    fn validated(self) -> Result<Self, SpecError> {
        if !self.attack_ms.is_finite()
            || !self.release_ms.is_finite()
            || self.attack_ms < 0.0
            || self.release_ms < 0.0
        {
            return Err(SpecError::InvalidSmoothTime);
        }
        Ok(Self {
            attack_ms: canonical_zero(self.attack_ms),
            release_ms: canonical_zero(self.release_ms),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PeakConfig {
    pub hold_ms: f32,
    pub decay_db_per_second: f32,
}

impl PeakConfig {
    pub fn new(hold_ms: f32, decay_db_per_second: f32) -> Result<Self, SpecError> {
        let value = Self {
            hold_ms,
            decay_db_per_second,
        };
        value.validated()
    }

    fn validated(self) -> Result<Self, SpecError> {
        if !self.hold_ms.is_finite()
            || !self.decay_db_per_second.is_finite()
            || self.hold_ms < 0.0
            || self.decay_db_per_second < 0.0
        {
            return Err(SpecError::InvalidPeakTime);
        }
        Ok(Self {
            hold_ms: canonical_zero(self.hold_ms),
            decay_db_per_second: canonical_zero(self.decay_db_per_second),
        })
    }
}

/// One processing step and its parameters.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "params", rename_all = "snake_case")]
pub enum NodeSpec {
    /// Source PCM → one channel (Left/Right by layout, a discrete index, or the
    /// average of all channels).
    Channel(ChannelSelection),
    /// One channel → magnitude spectrum in dBFS, periodic Hann, every hop.
    Fft(FftConfig),
    /// Spectrum + `db_per_octave · log2(f / 1 kHz)`.
    Tilt { db_per_octave: f32 },
    /// Per-bin attack/release exponential smoothing in source time (stateful).
    Smooth(SmoothConfig),
    /// Spectrum + `db`.
    Gain { db: f32 },
    /// Per-bin peak hold, then linear decay in dB/s (stateful).
    Peak(PeakConfig),
    /// Source PCM → per channel 400 ms K-weighted loudness and the sample peak of
    /// each publication interval (stateful).
    Levels,
}

/// Kind of data a node produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SignalKind {
    Pcm,
    Mono,
    Spectrum,
    Levels,
}

impl NodeSpec {
    pub(crate) fn input_kind(&self) -> SignalKind {
        match self {
            Self::Channel(_) | Self::Levels => SignalKind::Pcm,
            Self::Fft(_) => SignalKind::Mono,
            Self::Tilt { .. } | Self::Smooth(_) | Self::Gain { .. } | Self::Peak(_) => {
                SignalKind::Spectrum
            }
        }
    }

    pub(crate) fn output_kind(&self) -> SignalKind {
        match self {
            Self::Channel(_) => SignalKind::Mono,
            Self::Levels => SignalKind::Levels,
            _ => SignalKind::Spectrum,
        }
    }

    /// Stateful nodes are private per subscription unless an explicit binding
    /// covers them; pure nodes are shared whenever their key matches.
    pub fn is_stateful(&self) -> bool {
        matches!(self, Self::Smooth(_) | Self::Peak(_) | Self::Levels)
    }

    fn validated(self, descriptor: &AudioDescriptor) -> Result<Self, SpecError> {
        let finite = |value: f32, error| {
            if value.is_finite() {
                Ok(canonical_zero(value))
            } else {
                Err(error)
            }
        };
        Ok(match self {
            Self::Channel(channel) => {
                channel
                    .resolve(descriptor)
                    .map_err(SpecError::InvalidChannel)?;
                self
            }
            Self::Fft(fft) => Self::Fft(
                FftConfig::new(fft.window_size, fft.hop_size, fft.floor_dbfs)
                    .map_err(|_| SpecError::InvalidFft)?,
            ),
            Self::Tilt { db_per_octave } => Self::Tilt {
                db_per_octave: finite(db_per_octave, SpecError::InvalidTilt)?,
            },
            Self::Smooth(config) => Self::Smooth(config.validated()?),
            Self::Gain { db } => Self::Gain {
                db: finite(db, SpecError::InvalidGain)?,
            },
            Self::Peak(config) => Self::Peak(config.validated()?),
            Self::Levels => self,
        })
    }
}

/// A node of a `DataSpec`, or the source itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeRef(u32);

/// The interleaved source PCM every chain starts from.
pub const SOURCE: NodeRef = NodeRef(u32::MAX);

impl NodeRef {
    pub(crate) fn index(self) -> Option<usize> {
        (self != SOURCE).then_some(self.0 as usize)
    }
}

/// How much of an output a reader keeps available.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Keep {
    /// Only the most recent result.
    Latest,
    /// Every result of the last `seconds` of source time (History).
    Seconds(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OutputId(u32);
impl OutputId {
    pub(crate) fn index(self) -> usize {
        self.0 as usize
    }

    pub(crate) fn from_index(index: usize) -> Self {
        Self(index as u32)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutputSpec {
    pub node: NodeRef,
    pub keep: Keep,
    pub binding: Option<SharedAnalysisId>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SpecNode {
    pub(crate) input: NodeRef,
    pub(crate) spec: NodeSpec,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(into = "SpecFile", try_from = "SpecFile")]
pub struct DataSpec {
    nodes: Vec<SpecNode>,
    outputs: Vec<OutputSpec>,
}

/// Saved form of a `DataSpec`: node inputs and output nodes are indices into
/// `nodes`, `null` meaning the source.
#[derive(Clone, Serialize, Deserialize)]
struct SpecFile {
    nodes: Vec<NodeFile>,
    outputs: Vec<OutputFile>,
}

#[derive(Clone, Serialize, Deserialize)]
struct NodeFile {
    input: Option<u32>,
    #[serde(flatten)]
    spec: NodeSpec,
}

#[derive(Clone, Serialize, Deserialize)]
struct OutputFile {
    node: Option<u32>,
    keep: Keep,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    binding: Option<SharedAnalysisId>,
}

fn file_ref(node: NodeRef) -> Option<u32> {
    (node != SOURCE).then_some(node.0)
}

impl From<DataSpec> for SpecFile {
    fn from(spec: DataSpec) -> Self {
        Self {
            nodes: spec
                .nodes
                .iter()
                .map(|node| NodeFile {
                    input: file_ref(node.input),
                    spec: node.spec,
                })
                .collect(),
            outputs: spec
                .outputs
                .iter()
                .map(|output| OutputFile {
                    node: file_ref(output.node),
                    keep: output.keep,
                    binding: output.binding,
                })
                .collect(),
        }
    }
}

impl TryFrom<SpecFile> for DataSpec {
    type Error = String;

    /// Only the graph structure is checked here (inputs come before their nodes,
    /// outputs name existing nodes); parameters are validated on subscribe.
    fn try_from(file: SpecFile) -> Result<Self, Self::Error> {
        let mut spec = Self::new();
        for (index, node) in file.nodes.into_iter().enumerate() {
            let input = match node.input {
                None => SOURCE,
                Some(input) if (input as usize) < index => NodeRef(input),
                Some(input) => return Err(format!("node {index} reads node {input}")),
            };
            spec.nodes.push(SpecNode {
                input,
                spec: node.spec,
            });
        }
        for output in file.outputs {
            let node = match output.node {
                None => SOURCE,
                Some(node) if (node as usize) < spec.nodes.len() => NodeRef(node),
                Some(node) => return Err(format!("an output names missing node {node}")),
            };
            spec.outputs.push(OutputSpec {
                node,
                keep: output.keep,
                binding: output.binding,
            });
        }
        Ok(spec)
    }
}

impl DataSpec {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `node` reading from `input` (an earlier node or `SOURCE`).
    pub fn add(&mut self, input: NodeRef, node: NodeSpec) -> NodeRef {
        assert!(
            input == SOURCE || (input.0 as usize) < self.nodes.len(),
            "a node reads from the source or an earlier node"
        );
        self.nodes.push(SpecNode { input, spec: node });
        NodeRef(self.nodes.len() as u32 - 1)
    }

    /// Append `nodes` one after another, starting from `input`; returns the last.
    pub fn chain(&mut self, input: NodeRef, nodes: impl IntoIterator<Item = NodeSpec>) -> NodeRef {
        nodes
            .into_iter()
            .fold(input, |input, node| self.add(input, node))
    }

    pub fn output(&mut self, node: NodeRef, keep: Keep) -> OutputId {
        self.outputs.push(OutputSpec {
            node,
            keep,
            binding: None,
        });
        OutputId(self.outputs.len() as u32 - 1)
    }

    /// Share `output` (and the stateful nodes it depends on) under `binding`, or
    /// make it private again with `None`.
    pub fn bind(&mut self, output: OutputId, binding: Option<SharedAnalysisId>) {
        self.outputs[output.index()].binding = binding;
    }

    pub fn node(&self, node: NodeRef) -> Option<NodeSpec> {
        node.index()
            .and_then(|index| self.nodes.get(index))
            .map(|node| node.spec)
    }

    /// Replace the parameters of `node`; the edit is checked when submitted.
    pub fn set_node(&mut self, node: NodeRef, spec: NodeSpec) {
        if let Some(entry) = node.index().and_then(|index| self.nodes.get_mut(index)) {
            entry.spec = spec;
        }
    }

    /// `(node, input, parameters)` in declaration order (inputs come first).
    pub fn nodes(&self) -> impl Iterator<Item = (NodeRef, NodeRef, NodeSpec)> + '_ {
        self.nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (NodeRef(index as u32), node.input, node.spec))
    }

    pub fn outputs(&self) -> &[OutputSpec] {
        &self.outputs
    }

    pub(crate) fn spec_nodes(&self) -> &[SpecNode] {
        &self.nodes
    }

    /// Same graph: node kinds and inputs, output nodes and kinds of retention.
    /// Parameters, durations and bindings may differ.
    pub fn same_shape(&self, other: &Self) -> bool {
        self.nodes.len() == other.nodes.len()
            && self.outputs.len() == other.outputs.len()
            && self.nodes.iter().zip(&other.nodes).all(|(a, b)| {
                a.input == b.input
                    && std::mem::discriminant(&a.spec) == std::mem::discriminant(&b.spec)
            })
            && self.outputs.iter().zip(&other.outputs).all(|(a, b)| {
                a.node == b.node
                    && std::mem::discriminant(&a.keep) == std::mem::discriminant(&b.keep)
            })
    }

    /// Replace every binding through `map`.
    pub fn rebind(&mut self, mut map: impl FnMut(SharedAnalysisId) -> SharedAnalysisId) {
        for output in &mut self.outputs {
            output.binding = output.binding.map(&mut map);
        }
    }

    /// Set how long `output` is kept.
    pub fn set_keep(&mut self, output: OutputId, keep: Keep) {
        if let Some(entry) = self.outputs.get_mut(output.index()) {
            entry.keep = keep;
        }
    }

    /// Canonical copy checked against the source: node kinds connect, parameters
    /// are finite and in range, channels exist, histories fit the memory budget, and
    /// no stateful node is claimed by two different bindings.
    pub(crate) fn validated(&self, descriptor: &AudioDescriptor) -> Result<Self, SpecError> {
        if self.outputs.is_empty() {
            return Err(SpecError::NoOutputs);
        }
        let mut nodes = Vec::with_capacity(self.nodes.len());
        for (index, node) in self.nodes.iter().enumerate() {
            let input_kind = match node.input.index() {
                None => SignalKind::Pcm,
                Some(input) => self.nodes[input].spec.output_kind(),
            };
            if node.spec.input_kind() != input_kind {
                return Err(SpecError::KindMismatch { node: index });
            }
            nodes.push(SpecNode {
                input: node.input,
                spec: node.spec.validated(descriptor)?,
            });
        }
        let validated = Self {
            nodes,
            outputs: self.outputs.clone(),
        };
        for output in &validated.outputs {
            let Some(kind) = (match output.node.index() {
                None => Some(SignalKind::Pcm),
                Some(index) => validated
                    .nodes
                    .get(index)
                    .map(|node| node.spec.output_kind()),
            }) else {
                return Err(SpecError::UnknownNode);
            };
            if kind == SignalKind::Mono {
                return Err(SpecError::UnsupportedOutput);
            }
            if let Keep::Seconds(seconds) = output.keep {
                if !seconds.is_finite() || seconds <= 0.0 {
                    return Err(SpecError::InvalidKeep);
                }
                if validated.history_bytes(output.node, seconds, descriptor)
                    > HISTORY_BUDGET_BYTES as f64
                {
                    return Err(SpecError::HistoryTooLarge);
                }
            }
        }
        validated.binding_scopes()?;
        Ok(validated)
    }

    /// Binding covering each node: an output's binding claims every stateful node
    /// the output depends on (including the output node itself). Pure nodes stay
    /// shared by key and are never claimed.
    pub(crate) fn binding_scopes(&self) -> Result<Vec<Option<SharedAnalysisId>>, SpecError> {
        let mut scopes: Vec<Option<SharedAnalysisId>> = vec![None; self.nodes.len()];
        for output in &self.outputs {
            let Some(binding) = output.binding else {
                continue;
            };
            let mut current = output.node.index();
            while let Some(index) = current {
                if self.nodes[index].spec.is_stateful() {
                    match scopes[index] {
                        Some(existing) if existing != binding => {
                            return Err(SpecError::ConflictingBindings);
                        }
                        _ => scopes[index] = Some(binding),
                    }
                }
                current = self.nodes[index].input.index();
            }
        }
        Ok(scopes)
    }

    /// Upper estimate of the memory one History keeps: per retained result its
    /// payload plus the frame allocation, metadata and ring entry, with one extra
    /// result kept at the window boundary.
    fn history_bytes(&self, node: NodeRef, seconds: f32, descriptor: &AudioDescriptor) -> f64 {
        let sample_rate = f64::from(descriptor.sample_rate_hz);
        let channels = f64::from(descriptor.channel_count);
        let float = std::mem::size_of::<f32>() as f64;
        let time_rate = TIME_PUBLICATIONS_PER_SECOND as f64;
        let mut fft = None;
        let mut current = node.index();
        while let Some(index) = current {
            if let NodeSpec::Fft(config) = self.nodes[index].spec {
                fft = Some(config);
                break;
            }
            current = self.nodes[index].input.index();
        }
        let kind = node.index().map_or(SignalKind::Pcm, |index| {
            self.nodes[index].spec.output_kind()
        });
        let (results_per_second, payload) = match (kind, fft) {
            (SignalKind::Spectrum, Some(fft)) => (
                sample_rate / fft.hop_size as f64,
                (fft.window_size / 2 + 1) as f64 * float,
            ),
            (SignalKind::Levels, _) => (time_rate, 2.0 * channels * float),
            _ => (
                time_rate,
                (sample_rate / time_rate).ceil() * channels * float,
            ),
        };
        let per_result = payload + FRAME_OVERHEAD_BYTES;
        (results_per_second * f64::from(seconds) + 1.0) * per_result
    }
}

/// Time-domain results (PCM chunks, Levels) are published at most this often.
pub(crate) const TIME_PUBLICATIONS_PER_SECOND: usize = 30;

/// Storage of one retained result besides its payload: the frame and its
/// metadata, the `Arc` counts, the ring entry and allocator bookkeeping.
const FRAME_OVERHEAD_BYTES: f64 = (std::mem::size_of::<crate::snapshot::Frame>()
    + 2 * std::mem::size_of::<usize>()
    + std::mem::size_of::<std::sync::Arc<crate::snapshot::Frame>>()
    + 32) as f64;

/// Largest History a single output may keep.
pub const HISTORY_BUDGET_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn canonical_zero(value: f32) -> f32 {
    if value == 0.0 { 0.0 } else { value }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpecError {
    NoOutputs,
    UnknownNode,
    /// The output's data kind is not published (a Channel node's mono samples).
    UnsupportedOutput,
    KindMismatch {
        node: usize,
    },
    InvalidChannel(ChannelError),
    InvalidFft,
    InvalidTilt,
    InvalidSmoothTime,
    InvalidGain,
    InvalidPeakTime,
    InvalidKeep,
    HistoryTooLarge,
    ConflictingBindings,
}

impl fmt::Display for SpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoOutputs => f.write_str("a data spec needs at least one output"),
            Self::UnknownNode => f.write_str("an output refers to an unknown node"),
            Self::UnsupportedOutput => {
                f.write_str("an output must be the source, a spectrum or Levels node")
            }
            Self::KindMismatch { node } => {
                write!(f, "node {node} cannot read the data of its input")
            }
            Self::InvalidChannel(error) => write!(f, "{error}"),
            Self::InvalidFft => f.write_str("invalid FFT configuration"),
            Self::InvalidTilt => f.write_str("tilt must be finite"),
            Self::InvalidSmoothTime => {
                f.write_str("smooth attack/release must be finite and non-negative")
            }
            Self::InvalidGain => f.write_str("gain must be finite"),
            Self::InvalidPeakTime => f.write_str("peak hold/decay must be finite and non-negative"),
            Self::InvalidKeep => f.write_str("history duration must be finite and positive"),
            Self::HistoryTooLarge => write!(
                f,
                "history exceeds the {} MiB per-output budget",
                HISTORY_BUDGET_BYTES / (1024 * 1024)
            ),
            Self::ConflictingBindings => {
                f.write_str("a stateful node is claimed by two different bindings")
            }
        }
    }
}

impl Error for SpecError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{ChannelLayout, Generation, SourceId};

    fn stereo_48k() -> AudioDescriptor {
        AudioDescriptor::new(SourceId(1), Generation(1), 48_000, 2, ChannelLayout::Stereo).unwrap()
    }

    /// Checks the History budget boundary of `node`. The frame model is
    /// `bytes = (results_per_second * seconds + 1) *
    /// (payload + FRAME_OVERHEAD_BYTES)`, i.e. one extra result at the window
    /// boundary. `max_results` is the largest whole number of retained results
    /// whose bytes stay within `HISTORY_BUDGET_BYTES`; the two probes sit half a
    /// result inside and 1.5 results beyond that limit, so f32 rounding of the
    /// seconds cannot flip either side.
    fn assert_history_boundary(
        mut spec: DataSpec,
        node: NodeRef,
        descriptor: &AudioDescriptor,
        results_per_second: f64,
        payload_bytes: f64,
    ) {
        let per_result = payload_bytes + FRAME_OVERHEAD_BYTES;
        let max_results = (HISTORY_BUDGET_BYTES as f64 / per_result).floor() - 1.0;
        let fits = ((max_results - 0.5) / results_per_second) as f32;
        let too_long = ((max_results + 1.5) / results_per_second) as f32;
        let output = spec.output(node, Keep::Seconds(fits));
        assert_eq!(
            spec.validated(descriptor).map(|_| ()),
            Ok(()),
            "a history of {fits} s fits the budget and must be accepted"
        );
        spec.set_keep(output, Keep::Seconds(too_long));
        assert_eq!(
            spec.validated(descriptor).map(|_| ()),
            Err(SpecError::HistoryTooLarge),
            "one more output frame ({too_long} s) exceeds the budget"
        );
    }

    #[test]
    fn spectrum_history_is_accepted_up_to_the_budget_and_rejected_one_frame_beyond() {
        let descriptor = stereo_48k();
        let fft = FftConfig::new(2048, 512, -160.0).unwrap();
        let mut spec = DataSpec::new();
        let channel = spec.add(SOURCE, NodeSpec::Channel(ChannelSelection::Left));
        let node = spec.add(channel, NodeSpec::Fft(fft));
        assert_history_boundary(
            spec,
            node,
            &descriptor,
            f64::from(descriptor.sample_rate_hz) / fft.hop_size as f64,
            (fft.window_size / 2 + 1) as f64 * 4.0,
        );
    }

    #[test]
    fn levels_history_is_accepted_up_to_the_budget_and_rejected_one_frame_beyond() {
        let descriptor = stereo_48k();
        let mut spec = DataSpec::new();
        let node = spec.add(SOURCE, NodeSpec::Levels);
        assert_history_boundary(
            spec,
            node,
            &descriptor,
            TIME_PUBLICATIONS_PER_SECOND as f64,
            2.0 * f64::from(descriptor.channel_count) * 4.0,
        );
    }

    #[test]
    fn history_duration_rejects_negative_infinity_and_accepts_tiny_positive() {
        let descriptor = stereo_48k();
        let mut spec = DataSpec::new();
        let levels = spec.add(SOURCE, NodeSpec::Levels);
        let output = spec.output(levels, Keep::Seconds(f32::NEG_INFINITY));
        assert_eq!(
            spec.validated(&descriptor).map(|_| ()),
            Err(SpecError::InvalidKeep)
        );
        spec.set_keep(output, Keep::Seconds(f32::MIN_POSITIVE));
        assert!(spec.validated(&descriptor).is_ok());
    }
}
