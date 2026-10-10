//! Settings descriptions. The analysis part of a Module's settings is generated
//! from its `DataSpec` (each node listed once, explicit sharing shown as one group);
//! the view part comes from the Module as descriptors whose values are also what
//! persistence saves. The Workspace renders both; Modules never depend on the UI
//! toolkit to describe their settings.

use serde::{Deserialize, Serialize};

use crate::dataflow::{
    ChannelSelection, DataSpec, FftConfig, Keep, NodeRef, NodeSpec, OutputId, SharedAnalysisId,
};

/// Value of one view setting; also its saved form.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingValue {
    Number(f32),
    Choice(String),
    Channel(ChannelSelection),
}

impl SettingValue {
    pub fn number(&self) -> Option<f32> {
        match self {
            Self::Number(value) if value.is_finite() => Some(*value),
            _ => None,
        }
    }

    pub fn channel(&self) -> Option<ChannelSelection> {
        match self {
            Self::Channel(channel) => Some(*channel),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Choice {
    pub id: &'static str,
    pub label: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SettingKind {
    Number {
        min: f32,
        max: f32,
        unit: &'static str,
        logarithmic: bool,
    },
    Choice(&'static [Choice]),
    /// One of the source's channels or the mono mix.
    Channel,
}

/// One view parameter a Module shows in its settings and saves.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewSetting {
    pub key: &'static str,
    pub label: &'static str,
    /// Section title; Modules arrange their own settings area by section.
    pub section: &'static str,
    pub kind: SettingKind,
    pub value: SettingValue,
}

impl ViewSetting {
    /// Check `value` against the kind's range; numbers are clamped.
    pub fn accept(&self, value: &SettingValue) -> Option<SettingValue> {
        match (self.kind, value) {
            (SettingKind::Number { min, max, .. }, SettingValue::Number(number))
                if number.is_finite() =>
            {
                Some(SettingValue::Number(number.clamp(min, max)))
            }
            (SettingKind::Choice(choices), SettingValue::Choice(id))
                if choices.iter().any(|choice| choice.id == id) =>
            {
                Some(value.clone())
            }
            (SettingKind::Channel, SettingValue::Channel(_)) => Some(value.clone()),
            _ => None,
        }
    }
}

/// A parameter of one node kind, as the settings UI edits it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeParam {
    Channel,
    FftWindow,
    FftHop,
    FftFloor,
    Tilt,
    SmoothAttack,
    SmoothRelease,
    Center,
    Gain,
    PeakHold,
    PeakDecay,
}

/// Window sizes the settings offer.
pub const FFT_WINDOWS: [usize; 6] = [512, 1024, 2048, 4096, 8192, 16384];
/// Hop as a fraction of the window: 1/2, 1/4, 1/8.
pub const FFT_HOP_DIVISORS: [usize; 3] = [2, 4, 8];

impl NodeParam {
    pub fn of(node: &NodeSpec) -> &'static [Self] {
        match node {
            NodeSpec::Channel(_) => &[Self::Channel],
            NodeSpec::Fft(_) => &[Self::FftWindow, Self::FftHop, Self::FftFloor],
            NodeSpec::Tilt { .. } => &[Self::Tilt],
            NodeSpec::Smooth(_) => &[Self::SmoothAttack, Self::SmoothRelease],
            NodeSpec::Center { .. } => &[Self::Center],
            NodeSpec::Gain { .. } => &[Self::Gain],
            NodeSpec::Peak(_) => &[Self::PeakHold, Self::PeakDecay],
            NodeSpec::Levels => &[],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Channel => "Channel",
            Self::FftWindow => "FFT size",
            Self::FftHop => "Hop",
            Self::FftFloor => "Floor",
            Self::Tilt => "Tilt",
            Self::SmoothAttack => "Smooth attack",
            Self::SmoothRelease => "Smooth release",
            Self::Center => "Center",
            Self::Gain => "Gain",
            Self::PeakHold => "Peak hold",
            Self::PeakDecay => "Peak decay",
        }
    }

    /// Range and unit of numeric parameters (`None` for choices).
    pub fn range(self) -> Option<(f32, f32, &'static str)> {
        Some(match self {
            Self::FftFloor => (-240.0, -60.0, "dBFS"),
            Self::Tilt => (-12.0, 12.0, "dB/oct"),
            Self::SmoothAttack | Self::SmoothRelease => (0.0, 2_000.0, "ms"),
            Self::Center | Self::Gain => (-60.0, 60.0, "dB"),
            Self::PeakHold => (0.0, 10_000.0, "ms"),
            Self::PeakDecay => (0.0, 240.0, "dB/s"),
            Self::Channel | Self::FftWindow | Self::FftHop => return None,
        })
    }

    pub fn number(self, node: &NodeSpec) -> Option<f32> {
        Some(match (self, node) {
            (Self::FftFloor, NodeSpec::Fft(fft)) => fft.floor_dbfs,
            (Self::Tilt, NodeSpec::Tilt { db_per_octave }) => *db_per_octave,
            (Self::SmoothAttack, NodeSpec::Smooth(smooth)) => smooth.attack_ms,
            (Self::SmoothRelease, NodeSpec::Smooth(smooth)) => smooth.release_ms,
            (Self::Center, NodeSpec::Center { db }) | (Self::Gain, NodeSpec::Gain { db }) => *db,
            (Self::PeakHold, NodeSpec::Peak(peak)) => peak.hold_ms,
            (Self::PeakDecay, NodeSpec::Peak(peak)) => peak.decay_db_per_second,
            _ => return None,
        })
    }

    /// `node` with this numeric parameter set to `value` (clamped to the range).
    pub fn with_number(self, node: NodeSpec, value: f32) -> Option<NodeSpec> {
        let (min, max, _) = self.range()?;
        if !value.is_finite() {
            return None;
        }
        let value = value.clamp(min, max);
        Some(match (self, node) {
            (Self::FftFloor, NodeSpec::Fft(fft)) => NodeSpec::Fft(FftConfig {
                floor_dbfs: value,
                ..fft
            }),
            (Self::Tilt, NodeSpec::Tilt { .. }) => NodeSpec::Tilt {
                db_per_octave: value,
            },
            (Self::SmoothAttack, NodeSpec::Smooth(mut smooth)) => {
                smooth.attack_ms = value;
                NodeSpec::Smooth(smooth)
            }
            (Self::SmoothRelease, NodeSpec::Smooth(mut smooth)) => {
                smooth.release_ms = value;
                NodeSpec::Smooth(smooth)
            }
            (Self::Center, NodeSpec::Center { .. }) => NodeSpec::Center { db: value },
            (Self::Gain, NodeSpec::Gain { .. }) => NodeSpec::Gain { db: value },
            (Self::PeakHold, NodeSpec::Peak(mut peak)) => {
                peak.hold_ms = value;
                NodeSpec::Peak(peak)
            }
            (Self::PeakDecay, NodeSpec::Peak(mut peak)) => {
                peak.decay_db_per_second = value;
                NodeSpec::Peak(peak)
            }
            _ => return None,
        })
    }
}

/// `fft` with a new window size, keeping the hop's fraction of the window.
pub fn with_fft_window(fft: FftConfig, window_size: usize) -> FftConfig {
    let divisor = (fft.window_size / fft.hop_size.max(1)).max(1);
    FftConfig {
        window_size,
        hop_size: (window_size / divisor).max(1),
        ..fft
    }
}

/// Where a data setting lives: this instance alone, or the shared group of one
/// explicit binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataScope {
    Private,
    Shared(SharedAnalysisId),
}

/// One node as the settings show it.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeEntry {
    pub node: NodeRef,
    pub spec: NodeSpec,
    pub scope: DataScope,
}

/// One output as the settings show it.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputEntry {
    pub output: OutputId,
    pub keep: Keep,
    pub binding: Option<SharedAnalysisId>,
    /// The output depends on stateful nodes, so sharing it shares that state.
    pub shareable: bool,
}

/// The analysis settings of one `DataSpec`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DataSettings {
    /// Every node with parameters, once, in declaration order.
    pub nodes: Vec<NodeEntry>,
    pub outputs: Vec<OutputEntry>,
}

impl DataSettings {
    pub fn describe(spec: &DataSpec) -> Self {
        let nodes = spec
            .nodes()
            .filter(|(_, _, node)| !NodeParam::of(node).is_empty())
            .map(|(node, _, spec_node)| NodeEntry {
                node,
                spec: spec_node,
                scope: spec
                    .binding_covering(node)
                    .map_or(DataScope::Private, |(binding, _)| {
                        DataScope::Shared(binding)
                    }),
            })
            .collect();
        let outputs = spec
            .outputs()
            .iter()
            .enumerate()
            .map(|(index, output)| OutputEntry {
                output: OutputId::from_index(index),
                keep: output.keep,
                binding: output.binding,
                shareable: spec.depends_on_state(output.node),
            })
            .collect();
        Self { nodes, outputs }
    }

    /// Bindings in first-appearance order.
    pub fn bindings(&self) -> Vec<SharedAnalysisId> {
        let mut bindings = Vec::new();
        for binding in self.outputs.iter().filter_map(|output| output.binding) {
            if !bindings.contains(&binding) {
                bindings.push(binding);
            }
        }
        bindings
    }
}

/// Label of a node kind for the settings.
pub fn node_label(node: &NodeSpec) -> &'static str {
    match node {
        NodeSpec::Channel(_) => "Channel",
        NodeSpec::Fft(_) => "FFT",
        NodeSpec::Tilt { .. } => "Tilt",
        NodeSpec::Smooth(_) => "Smooth",
        NodeSpec::Center { .. } => "Center",
        NodeSpec::Gain { .. } => "Gain",
        NodeSpec::Peak(_) => "Peak",
        NodeSpec::Levels => "Levels",
    }
}
