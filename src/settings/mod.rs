//! Settings descriptions. The analysis part of a Module's settings is generated
//! from its `DataSpec` (each node listed once); the view part comes from the Module
//! as descriptors whose values are also what persistence saves. The Workspace
//! renders both; Modules never depend on the UI toolkit to describe their settings.

use serde::{Deserialize, Serialize};

use crate::dataflow::{ChannelSelection, DataSpec, FftConfig, Keep, NodeRef, NodeSpec, OutputId};

/// Value of one view setting; also its saved form.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingValue {
    Number(f32),
    Choice(String),
    Channel(ChannelSelection),
    Bool(bool),
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

    pub fn flag(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Choice {
    pub id: &'static str,
    pub label: &'static str,
}

/// Units of numeric settings; the settings window formats and parses values by them.
pub mod unit {
    pub const HZ: &str = "Hz";
    pub const MS: &str = "ms";
    pub const SECONDS: &str = "s";
    pub const DB: &str = "dB";
    pub const DBFS: &str = "dBFS";
    pub const DB_PER_OCTAVE: &str = "dB/oct";
    pub const DB_PER_SECOND_SQUARED: &str = "dB/s²";
    pub const TIMES: &str = "×";
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
    /// Shown or hidden: a Module's optional overlay (peak curve, guides, readout).
    Toggle,
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
    /// What a new instance shows; the settings offer to reset to it.
    pub default: SettingValue,
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
            (SettingKind::Channel, SettingValue::Channel(_))
            | (SettingKind::Toggle, SettingValue::Bool(_)) => Some(value.clone()),
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
    Gain,
    PeakHold,
    PeakFall,
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
            NodeSpec::Gain { .. } => &[Self::Gain],
            NodeSpec::Peak(_) => &[Self::PeakHold, Self::PeakFall],
            NodeSpec::Levels | NodeSpec::Loudness => &[],
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
            Self::Gain => "Gain",
            Self::PeakHold => "Peak hold",
            Self::PeakFall => "Peak fall",
        }
    }

    /// Range and unit of numeric parameters (`None` for choices).
    pub fn range(self) -> Option<(f32, f32, &'static str)> {
        Some(match self {
            Self::FftFloor => (-200.0, -120.0, unit::DBFS),
            // Centered on the 4.5 dB/oct default: music already falls with
            // frequency, so negative tilts are of little use.
            Self::Tilt => (0.0, 9.0, unit::DB_PER_OCTAVE),
            Self::SmoothAttack | Self::SmoothRelease => (0.0, 2_000.0, unit::MS),
            Self::Gain => (-60.0, 60.0, unit::DB),
            Self::PeakHold => (0.0, 3_000.0, unit::MS),
            // Centered on the 30 dB/s² default: 60 dB falls in 2 s.
            Self::PeakFall => (0.0, 60.0, unit::DB_PER_SECOND_SQUARED),
            Self::Channel | Self::FftWindow | Self::FftHop => return None,
        })
    }

    pub fn number(self, node: &NodeSpec) -> Option<f32> {
        Some(match (self, node) {
            (Self::FftFloor, NodeSpec::Fft(fft)) => fft.floor_dbfs,
            (Self::Tilt, NodeSpec::Tilt { db_per_octave }) => *db_per_octave,
            (Self::SmoothAttack, NodeSpec::Smooth(smooth)) => smooth.attack_ms,
            (Self::SmoothRelease, NodeSpec::Smooth(smooth)) => smooth.release_ms,
            (Self::Gain, NodeSpec::Gain { db }) => *db,
            (Self::PeakHold, NodeSpec::Peak(peak)) => peak.hold_ms,
            (Self::PeakFall, NodeSpec::Peak(peak)) => peak.fall_db_per_second_squared,
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
            (Self::Gain, NodeSpec::Gain { .. }) => NodeSpec::Gain { db: value },
            (Self::PeakHold, NodeSpec::Peak(mut peak)) => {
                peak.hold_ms = value;
                NodeSpec::Peak(peak)
            }
            (Self::PeakFall, NodeSpec::Peak(mut peak)) => {
                peak.fall_db_per_second_squared = value;
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

/// One node as the settings show it.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeEntry {
    pub node: NodeRef,
    pub spec: NodeSpec,
}

/// One output as the settings show it.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputEntry {
    pub output: OutputId,
    pub keep: Keep,
}

/// The analysis settings of one `DataSpec`. Sharing is not part of them: every
/// instance edits its own analysis and the dataflow shares what stays identical.
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
            .map(|(node, _, spec)| NodeEntry { node, spec })
            .collect();
        let outputs = spec
            .outputs()
            .iter()
            .enumerate()
            .map(|(index, output)| OutputEntry {
                output: OutputId::from_index(index),
                keep: output.keep,
            })
            .collect();
        Self { nodes, outputs }
    }
}

/// Label of a node kind for the settings.
pub fn node_label(node: &NodeSpec) -> &'static str {
    match node {
        NodeSpec::Channel(_) => "Channel",
        NodeSpec::Fft(_) => "FFT",
        NodeSpec::Tilt { .. } => "Tilt",
        NodeSpec::Smooth(_) => "Smooth",
        NodeSpec::Gain { .. } => "Gain",
        NodeSpec::Peak(_) => "Peak",
        NodeSpec::Levels => "Levels",
        NodeSpec::Loudness => "Loudness",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataflow::{PeakConfig, SmoothConfig};

    const CHOICES: &[Choice] = &[
        Choice {
            id: "line",
            label: "Line",
        },
        Choice {
            id: "bars",
            label: "Bars",
        },
    ];

    fn setting(kind: SettingKind, value: SettingValue) -> ViewSetting {
        ViewSetting {
            key: "test",
            label: "Test",
            section: "Test",
            kind,
            default: value.clone(),
            value,
        }
    }

    fn number_setting(min: f32, max: f32) -> ViewSetting {
        setting(
            SettingKind::Number {
                min,
                max,
                unit: "",
                logarithmic: false,
            },
            SettingValue::Number(min),
        )
    }

    #[test]
    fn number_setting_accept_clamps_into_the_range() {
        let setting = number_setting(-10.0, 20.0);

        assert_eq!(
            setting.accept(&SettingValue::Number(5.0)),
            Some(SettingValue::Number(5.0))
        );
        assert_eq!(
            setting.accept(&SettingValue::Number(50.0)),
            Some(SettingValue::Number(20.0))
        );
        assert_eq!(
            setting.accept(&SettingValue::Number(-50.0)),
            Some(SettingValue::Number(-10.0))
        );
    }

    #[test]
    fn number_setting_accept_rejects_non_finite_values() {
        let setting = number_setting(-10.0, 20.0);

        assert_eq!(setting.accept(&SettingValue::Number(f32::NAN)), None);
        assert_eq!(setting.accept(&SettingValue::Number(f32::INFINITY)), None);
        assert_eq!(
            setting.accept(&SettingValue::Number(f32::NEG_INFINITY)),
            None
        );
    }

    #[test]
    fn number_setting_accept_rejects_other_value_types() {
        let setting = number_setting(0.0, 1.0);

        assert_eq!(
            setting.accept(&SettingValue::Choice("line".to_owned())),
            None
        );
        assert_eq!(
            setting.accept(&SettingValue::Channel(ChannelSelection::Left)),
            None
        );
    }

    #[test]
    fn choice_setting_accepts_only_declared_ids() {
        let setting = setting(
            SettingKind::Choice(CHOICES),
            SettingValue::Choice("line".to_owned()),
        );

        assert_eq!(
            setting.accept(&SettingValue::Choice("bars".to_owned())),
            Some(SettingValue::Choice("bars".to_owned()))
        );
        assert_eq!(
            setting.accept(&SettingValue::Choice("pixels".to_owned())),
            None
        );
        // Ids match exactly: the label of a choice is not its id.
        assert_eq!(
            setting.accept(&SettingValue::Choice("Bars".to_owned())),
            None
        );
        assert_eq!(setting.accept(&SettingValue::Number(0.0)), None);
    }

    #[test]
    fn channel_setting_accepts_channels_only() {
        let setting = setting(
            SettingKind::Channel,
            SettingValue::Channel(ChannelSelection::Left),
        );

        assert_eq!(
            setting.accept(&SettingValue::Channel(ChannelSelection::Discrete(3))),
            Some(SettingValue::Channel(ChannelSelection::Discrete(3)))
        );
        assert_eq!(setting.accept(&SettingValue::Number(1.0)), None);
        assert_eq!(
            setting.accept(&SettingValue::Choice("left".to_owned())),
            None
        );
    }

    #[test]
    fn node_param_reads_nothing_from_a_node_of_another_kind() {
        let gain = NodeSpec::Gain { db: 6.0 };
        let peak = NodeSpec::Peak(PeakConfig::new(1_000.0, 18.0).unwrap());
        let tilt = NodeSpec::Tilt { db_per_octave: 2.0 };

        assert_eq!(NodeParam::Gain.number(&gain), Some(6.0));
        assert_eq!(NodeParam::PeakHold.number(&peak), Some(1_000.0));
        assert_eq!(NodeParam::Gain.number(&peak), None);
        assert_eq!(NodeParam::PeakHold.number(&gain), None);
        assert_eq!(NodeParam::FftFloor.number(&tilt), None);
        assert_eq!(NodeParam::Tilt.number(&gain), None);
        assert_eq!(NodeParam::PeakHold.number(&NodeSpec::Levels), None);
        // Choices have no number even on the node kind they belong to.
        let fft = NodeSpec::Fft(FftConfig::default());
        assert_eq!(NodeParam::FftWindow.number(&fft), None);
    }

    #[test]
    fn node_param_with_number_leaves_a_node_of_another_kind_unchanged() {
        let tilt = NodeSpec::Tilt { db_per_octave: 3.0 };

        assert_eq!(NodeParam::Gain.with_number(tilt, 1.0), None);
        assert_eq!(NodeParam::PeakHold.with_number(tilt, 1.0), None);
        assert_eq!(
            NodeParam::Channel.with_number(NodeSpec::Channel(ChannelSelection::Left), 1.0),
            None
        );
    }

    #[test]
    fn node_param_with_number_clamps_and_rejects_non_finite_values() {
        let gain = NodeSpec::Gain { db: 0.0 };

        assert_eq!(
            NodeParam::Gain.with_number(gain, 1_000.0),
            Some(NodeSpec::Gain { db: 60.0 })
        );
        assert_eq!(
            NodeParam::Gain.with_number(gain, -1_000.0),
            Some(NodeSpec::Gain { db: -60.0 })
        );
        assert_eq!(NodeParam::Gain.with_number(gain, f32::NAN), None);
        assert_eq!(NodeParam::Gain.with_number(gain, f32::INFINITY), None);
    }

    #[test]
    fn node_param_edits_only_the_named_parameter_of_a_multi_parameter_node() {
        let smooth = NodeSpec::Smooth(SmoothConfig::new(10.0, 200.0).unwrap());
        let peak = NodeSpec::Peak(PeakConfig::new(500.0, 20.0).unwrap());

        assert_eq!(NodeParam::SmoothAttack.number(&smooth), Some(10.0));
        assert_eq!(NodeParam::SmoothRelease.number(&smooth), Some(200.0));
        assert_eq!(
            NodeParam::SmoothRelease.with_number(smooth, 99_999.0),
            Some(NodeSpec::Smooth(SmoothConfig::new(10.0, 2_000.0).unwrap()))
        );
        assert_eq!(NodeParam::PeakHold.number(&peak), Some(500.0));
        assert_eq!(NodeParam::PeakFall.number(&peak), Some(20.0));
        assert_eq!(
            NodeParam::PeakFall.with_number(peak, 30.0),
            Some(NodeSpec::Peak(PeakConfig::new(500.0, 30.0).unwrap()))
        );
        let fft = NodeSpec::Fft(FftConfig::default());
        assert_eq!(
            NodeParam::FftFloor.with_number(fft, -140.0),
            Some(NodeSpec::Fft(FftConfig {
                floor_dbfs: -140.0,
                ..FftConfig::default()
            }))
        );
    }

    #[test]
    fn with_fft_window_keeps_the_hop_fraction_of_the_window() {
        for window in FFT_WINDOWS {
            for divisor in FFT_HOP_DIVISORS {
                let fft = FftConfig {
                    window_size: 2048,
                    hop_size: 2048 / divisor,
                    floor_dbfs: -100.0,
                };

                let resized = with_fft_window(fft, window);

                assert_eq!(resized.window_size, window);
                assert_eq!(
                    resized.hop_size,
                    window / divisor,
                    "window {window} / {divisor}"
                );
                assert_eq!(resized.floor_dbfs, -100.0);
            }
        }
    }

    #[test]
    fn with_fft_window_never_produces_a_zero_hop() {
        let fft = FftConfig {
            window_size: 2048,
            hop_size: 256,
            floor_dbfs: -100.0,
        };

        let resized = with_fft_window(fft, 4);

        assert_eq!(resized.window_size, 4);
        assert_eq!(resized.hop_size, 1);
    }
}
