use std::{error::Error, fmt, time::Duration};

use crate::{
    audio::SourceReader,
    dataflow::{
        ChannelError, ChannelSelection, ControlError, DataController, DataSpec, DataflowHandle,
        OutputId,
    },
    extensions::{Registry, RegistryError},
    render::{Flow, Point, Rect, RenderTarget, Size, TextStyle},
    settings::{Choice, SettingKind, SettingValue, ViewSetting},
    theme::{ColorRole, Palette},
};

pub struct ModuleServices {
    pub source: SourceReader,
    pub dataflow: DataflowHandle,
}

pub struct ModuleFrame<'a> {
    pub rect: Rect,
    pub monotonic_time: Duration,
    pub pointer: Option<Point>,
    pub scroll_delta_y: f32,
    pub zoom_delta: f32,
    /// Colors resolved for this instance (theme, type style, instance overrides).
    pub palette: &'a Palette,
    /// The workspace's reference level: where the user's signal usually peaks.
    /// Views place it at `REFERENCE_HEIGHT`, so every Module fills a similar
    /// height whatever the input's loudness. Display only: readouts stay dBFS.
    pub reference_dbfs: f32,
    pub renderer: &'a mut dyn RenderTarget,
}

/// Reference level of a workspace that sets none: measured on the system loopback
/// at the user's usual listening volume (music peaks: p50 −20, p90 −15, p99 −12.7,
/// max −11 dBFS over 60 s), so everyday peaks stay within the views.
pub const DEFAULT_REFERENCE_DBFS: f32 = -12.0;
/// Range of the reference level the settings offer.
pub const REFERENCE_DBFS_RANGE: (f32, f32) = (-60.0, 0.0);
/// Fraction of a linear view's height (of the half height for signed views) at
/// which a sample at the reference level is drawn. dB views (Spectrum,
/// Spectrogram) put the reference at their top instead: a tilted spectrum's
/// loudest bins sit well below the sample peak.
pub const REFERENCE_HEIGHT: f32 = 0.9;
/// Wheel zoom of every view per scroll point: the zoomed quantity scales by
/// `exp(k · dy)`, the pace of the previous implementation's `1 + 0.001 · dy`.
pub const WHEEL_ZOOM_PER_POINT: f32 = 0.001;

/// Linear view gain: a sample at the reference level reaches `REFERENCE_HEIGHT`
/// of the (half) height.
pub fn reference_gain(reference_dbfs: f32) -> f32 {
    REFERENCE_HEIGHT / 10.0_f32.powf(reference_dbfs / 20.0)
}

/// Repaint interval an animating Module requests: the default 60 fps display rate.
pub const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModuleResponse {
    pub repaint_after: Option<Duration>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StyleOption {
    pub id: &'static str,
    pub label: &'static str,
}

/// Default minimum width and height of a Module, in logical points (one divider grid
/// step).
pub const MIN_EXTENT: f32 = 16.0;

pub trait Module: Send {
    fn draw(&mut self, frame: ModuleFrame<'_>) -> ModuleResponse;

    /// Smallest area, in logical points, the Module accepts. The Workspace never
    /// shrinks a layout leaf below it unless the window itself is forced smaller. The
    /// default (`MIN_EXTENT` square) only keeps a leaf reachable; content simply
    /// scales down.
    fn min_size(&self) -> Size {
        Size {
            width: MIN_EXTENT,
            height: MIN_EXTENT,
        }
    }

    fn channel(&self) -> Option<ChannelSelection> {
        None
    }

    fn set_channel(&mut self, _channel: ChannelSelection) -> Result<(), ModuleError> {
        Err(ModuleError::UnsupportedChannel)
    }

    fn style_options(&self) -> &'static [StyleOption] {
        &[]
    }

    fn active_style(&self) -> Option<&'static str> {
        None
    }

    fn set_style(&mut self, _style_id: &str, _registry: &Registry) -> Result<(), RegistryError> {
        Err(RegistryError::StylesUnsupported)
    }

    /// The Workspace reports whether the instance can be seen (its window is not
    /// minimized). Work that only serves what is shown may stop while hidden;
    /// continuous analysis and History must keep running.
    fn set_visible(&mut self, _visible: bool) {}

    /// Control of the instance's analysis subscription, when it reads audio data.
    /// Settings, copying and persistence read and edit its `DataSpec` through it.
    fn data(&self) -> Option<&DataController> {
        None
    }

    /// Name of an output of the instance's `DataSpec` in its settings.
    fn output_label(&self, _output: OutputId) -> Option<&'static str> {
        None
    }

    /// Theme roles the instance draws with now (its own parts and its active
    /// style's); its color overrides offer these. A subset of its type's
    /// `ModuleFactory::color_roles`.
    fn color_roles(&self) -> &[ColorRole] {
        &[]
    }

    /// View parameters (ranges, scales, display channel): shown in the settings
    /// and saved besides the `DataSpec`.
    fn view_settings(&self) -> Vec<ViewSetting> {
        Vec::new()
    }

    fn set_view_setting(&mut self, key: &str, _value: &SettingValue) -> Result<(), ModuleError> {
        Err(ModuleError::UnknownSetting(key.to_owned()))
    }
}

pub trait ModuleFactory: Send + Sync {
    fn type_id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;

    /// Create an instance with its default analysis, or with the saved `data`
    /// (a copy or a restored workspace). `data` must have the shape of the default
    /// `DataSpec`; its parameters, durations and bindings are used as they are.
    fn create(
        &self,
        services: &ModuleServices,
        registry: &Registry,
        data: Option<&DataSpec>,
    ) -> Result<Box<dyn Module>, RegistryError>;

    /// The analysis a new instance starts with, for resetting its parameters in
    /// the settings. `None` when the type has nothing adjustable to reset.
    fn default_data(&self) -> Option<DataSpec> {
        None
    }

    /// Theme roles any instance of the type can draw with, whatever its style: the
    /// colors a Module type style offers.
    fn color_roles(&self) -> &'static [ColorRole] {
        &[]
    }
}

/// What a styled instance lists for its color overrides: its style's roles, then
/// its own not already listed.
pub fn styled_color_roles(style: &[ColorRole], own: &[ColorRole]) -> Vec<ColorRole> {
    let mut roles = style.to_vec();
    for role in own {
        if !roles.contains(role) {
            roles.push(*role);
        }
    }
    roles
}

/// A Module that cannot show its content (an instance this build cannot restore,
/// an input it cannot display) says why in the leaf's top-left corner, in the
/// theme's Muted color.
pub fn draw_notice(frame: &mut ModuleFrame<'_>, message: &str) {
    frame.renderer.text(
        frame.rect,
        Point {
            x: frame.rect.min.x + 8.0,
            y: frame.rect.min.y + 8.0,
        },
        message,
        TextStyle {
            size: frame.palette.text_size,
            color: frame.palette.color(ColorRole::Muted),
        },
    );
}

/// The spec an instance subscribes with: `saved` when it has the default's shape.
pub fn spec_from_saved(
    default: DataSpec,
    saved: Option<&DataSpec>,
) -> Result<DataSpec, ModuleError> {
    match saved {
        None => Ok(default),
        Some(saved) if saved.same_shape(&default) => Ok(saved.clone()),
        Some(_) => Err(ModuleError::IncompatibleData),
    }
}

/// A view setting value as a number inside `range`.
pub fn number_setting(
    key: &str,
    value: &SettingValue,
    range: (f32, f32),
) -> Result<f32, ModuleError> {
    value
        .number()
        .map(|number| number.clamp(range.0, range.1))
        .ok_or_else(|| ModuleError::InvalidSetting(key.to_owned()))
}

/// A view setting value as on/off.
pub fn flag_setting(key: &str, value: &SettingValue) -> Result<bool, ModuleError> {
    value
        .flag()
        .ok_or_else(|| ModuleError::InvalidSetting(key.to_owned()))
}

/// An optional overlay of a Module (peak curve, guide lines, readout): hidden on a
/// new instance, listed under "Overlays".
pub fn overlay_setting(key: &'static str, label: &'static str, shown: bool) -> ViewSetting {
    ViewSetting {
        key,
        label,
        section: "Overlays",
        kind: SettingKind::Toggle,
        value: SettingValue::Bool(shown),
        default: SettingValue::Bool(false),
    }
}

/// Choices of a time view's `flow` setting: the direction the picture moves.
const FLOW_CHOICES: &[Choice] = &[
    Choice {
        id: "left",
        label: "Left",
    },
    Choice {
        id: "right",
        label: "Right",
    },
    Choice {
        id: "up",
        label: "Up",
    },
    Choice {
        id: "down",
        label: "Down",
    },
];

/// The direction a time-scrolling view (Waveform, Spectrogram) moves; new
/// instances move left, newest at the right edge.
pub fn flow_setting(flow: Flow) -> ViewSetting {
    ViewSetting {
        key: "flow",
        label: "Flow",
        section: "Display",
        kind: SettingKind::Choice(FLOW_CHOICES),
        value: SettingValue::Choice(flow.id().to_owned()),
        default: SettingValue::Choice(Flow::default().id().to_owned()),
    }
}

/// A view setting value as a flow direction.
pub fn flow_value(key: &str, value: &SettingValue) -> Result<Flow, ModuleError> {
    match value {
        SettingValue::Choice(id) => Flow::from_id(id),
        _ => None,
    }
    .ok_or_else(|| ModuleError::InvalidSetting(key.to_owned()))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModuleError {
    UnsupportedChannel,
    InvalidChannel(ChannelError),
    Control(ControlError),
    /// Saved analysis whose graph differs from what this Module type reads.
    IncompatibleData,
    UnknownSetting(String),
    InvalidSetting(String),
}

impl fmt::Display for ModuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedChannel => f.write_str("module does not support channel selection"),
            Self::InvalidChannel(error) => write!(f, "invalid channel selection: {error}"),
            Self::Control(error) => write!(f, "could not update module analysis: {error}"),
            Self::IncompatibleData => {
                f.write_str("saved analysis does not match what this module reads")
            }
            Self::UnknownSetting(key) => write!(f, "unknown setting: {key}"),
            Self::InvalidSetting(key) => write!(f, "invalid value for setting: {key}"),
        }
    }
}

impl Error for ModuleError {}

impl From<ChannelError> for ModuleError {
    fn from(error: ChannelError) -> Self {
        Self::InvalidChannel(error)
    }
}

impl From<ControlError> for ModuleError {
    fn from(error: ControlError) -> Self {
        Self::Control(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataflow::{FftConfig, Keep, NodeSpec, SOURCE, SharedAnalysisId};

    /// Source → Channel → Fft, with the Fft output kept as the latest result.
    fn default_spec() -> DataSpec {
        let mut spec = DataSpec::new();
        let channel = spec.add(SOURCE, NodeSpec::Channel(ChannelSelection::Left));
        let fft = spec.add(channel, NodeSpec::Fft(FftConfig::default()));
        spec.output(fft, Keep::Latest);
        spec
    }

    #[test]
    fn spec_from_saved_without_saved_data_uses_the_default() {
        assert_eq!(spec_from_saved(default_spec(), None), Ok(default_spec()));
    }

    #[test]
    fn spec_from_saved_adopts_saved_parameters_and_bindings_of_the_same_shape() {
        let mut saved = DataSpec::new();
        let channel = saved.add(SOURCE, NodeSpec::Channel(ChannelSelection::Right));
        let fft = saved.add(
            channel,
            NodeSpec::Fft(FftConfig {
                window_size: 1024,
                hop_size: 256,
                floor_dbfs: -120.0,
            }),
        );
        let output = saved.output(fft, Keep::Latest);
        saved.bind(output, Some(SharedAnalysisId(7)));
        assert!(saved.same_shape(&default_spec()));
        assert_ne!(saved, default_spec());

        let spec = spec_from_saved(default_spec(), Some(&saved));

        assert_eq!(spec, Ok(saved));
    }

    #[test]
    fn spec_from_saved_rejects_a_different_node_graph_instead_of_replacing_it() {
        let mut saved = default_spec();
        let tilt = saved.add(SOURCE, NodeSpec::Tilt { db_per_octave: 3.0 });
        saved.output(tilt, Keep::Latest);

        assert_eq!(
            spec_from_saved(default_spec(), Some(&saved)),
            Err(ModuleError::IncompatibleData)
        );
    }

    #[test]
    fn spec_from_saved_rejects_saved_data_that_keeps_a_different_kind_of_history() {
        let mut saved = default_spec();
        saved.set_keep(OutputId::from_index(0), Keep::Seconds(10.0));

        assert_eq!(
            spec_from_saved(default_spec(), Some(&saved)),
            Err(ModuleError::IncompatibleData)
        );
    }

    #[test]
    fn spec_from_saved_rejects_an_empty_saved_spec() {
        assert_eq!(
            spec_from_saved(default_spec(), Some(&DataSpec::new())),
            Err(ModuleError::IncompatibleData)
        );
    }

    #[test]
    fn number_setting_clamps_into_the_range() {
        let range = (-10.0, 20.0);

        assert_eq!(
            number_setting("k", &SettingValue::Number(5.0), range),
            Ok(5.0)
        );
        assert_eq!(
            number_setting("k", &SettingValue::Number(99.0), range),
            Ok(20.0)
        );
        assert_eq!(
            number_setting("k", &SettingValue::Number(-99.0), range),
            Ok(-10.0)
        );
        assert_eq!(
            number_setting("k", &SettingValue::Number(20.0), range),
            Ok(20.0)
        );
        assert_eq!(
            number_setting("k", &SettingValue::Number(-10.0), range),
            Ok(-10.0)
        );
    }

    #[test]
    fn number_setting_rejects_values_that_are_not_finite_numbers_and_names_the_key() {
        let range = (0.0, 1.0);
        let invalid = || Err(ModuleError::InvalidSetting("gain".to_owned()));

        assert_eq!(
            number_setting("gain", &SettingValue::Choice("0.5".to_owned()), range),
            invalid()
        );
        assert_eq!(
            number_setting(
                "gain",
                &SettingValue::Channel(ChannelSelection::Left),
                range
            ),
            invalid()
        );
        assert_eq!(
            number_setting("gain", &SettingValue::Number(f32::NAN), range),
            invalid()
        );
        assert_eq!(
            number_setting("gain", &SettingValue::Number(f32::INFINITY), range),
            invalid()
        );
        assert_eq!(
            number_setting("gain", &SettingValue::Number(f32::NEG_INFINITY), range),
            invalid()
        );
    }
}
