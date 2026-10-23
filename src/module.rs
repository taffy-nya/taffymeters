use std::{error::Error, fmt, time::Duration};

use crate::{
    audio::SourceReader,
    dataflow::{
        ChannelError, ChannelSelection, ControlError, DataController, DataSpec, DataflowHandle,
        Keep, OutputId,
    },
    extensions::{Registry, RegistryError},
    render::{Flow, Point, Rect, RenderTarget, Size, TextStyle},
    settings::{Choice, SettingKind, SettingValue, ViewSetting, unit},
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
    /// Scroll distance in logical points.
    pub scroll_delta_y: f32,
    /// Multiplicative zoom factor; 1.0 means no zoom.
    pub zoom_delta: f32,
    pub palette: &'a Palette,
    /// Display gain only: readouts remain in dBFS regardless of the workspace reference.
    pub reference_dbfs: f32,
    pub renderer: &'a mut dyn RenderTarget,
}

/// Reference level of a workspace that sets none: full scale. Loopback of a device with
/// hardware volume (many headphones) peaks there regardless of the system volume;
/// devices with software volume read lower and need a lower reference.
pub const DEFAULT_REFERENCE_DBFS: f32 = 0.0;
/// The top leaves room for float signals above full scale.
pub const REFERENCE_DBFS_RANGE: (f32, f32) = (-36.0, 12.0);
/// Fraction of a linear view's height (half-height for signed views) reached by the reference.
/// dB views put it at the top because tilted bins sit below the sample peak.
pub const REFERENCE_HEIGHT: f32 = 0.9;
/// Wheel zoom scales the view quantity by `exp(k · dy)`.
pub const WHEEL_ZOOM_PER_POINT: f32 = 0.001;
pub const MIN_SCROLL_POINTS: f32 = 0.01;
pub const MIN_ZOOM_CHANGE: f32 = 0.001;

pub fn reference_gain(reference_dbfs: f32) -> f32 {
    REFERENCE_HEIGHT / 10.0_f32.powf(reference_dbfs / 20.0)
}

/// Animating views return this to ask for the next frame; the Workspace frame-rate limit
/// spaces the frames.
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

/// One divider grid step, in logical points.
pub const MIN_EXTENT: f32 = 16.0;

pub trait Module: Send {
    fn draw(&mut self, frame: ModuleFrame<'_>) -> ModuleResponse;

    /// The Workspace respects this logical-point size unless the window is forced smaller.
    /// The default only keeps the leaf reachable; content must scale down to fit.
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

    /// Settings, copying and persistence access the instance's analysis through this controller.
    fn data(&self) -> Option<&DataController> {
        None
    }

    /// Settings use this label for an output of the instance's `DataSpec`.
    fn output_label(&self, _output: OutputId) -> Option<&'static str> {
        None
    }

    /// Must be a subset of `ModuleFactory::color_roles` covering the active style and own parts.
    fn color_roles(&self) -> &[ColorRole] {
        &[]
    }

    /// The Workspace shows these in settings and saves them separately from the `DataSpec`.
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

    /// Accept saved data only if it matches the default graph's shape; preserve its parameters,
    /// durations and bindings. Without saved data, use the default analysis.
    fn create(
        &self,
        services: &ModuleServices,
        registry: &Registry,
        data: Option<&DataSpec>,
    ) -> Result<Box<dyn Module>, RegistryError>;

    /// Settings use this to reset analysis parameters; return `None` if nothing is adjustable.
    fn default_data(&self) -> Option<DataSpec> {
        None
    }

    /// Must cover every role any instance can draw with, across all of its styles.
    fn color_roles(&self) -> &'static [ColorRole] {
        &[]
    }
}

pub fn styled_color_roles(style: &[ColorRole], own: &[ColorRole]) -> Vec<ColorRole> {
    let mut roles = style.to_vec();
    for role in own {
        if !roles.contains(role) {
            roles.push(*role);
        }
    }
    roles
}

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

pub fn flag_setting(key: &str, value: &SettingValue) -> Result<bool, ModuleError> {
    value
        .flag()
        .ok_or_else(|| ModuleError::InvalidSetting(key.to_owned()))
}

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

const FLOW_CHOICES: &[Choice] = &[
    Choice {
        id: Flow::Left.id(),
        label: "Left",
    },
    Choice {
        id: Flow::Right.id(),
        label: "Right",
    },
    Choice {
        id: Flow::Up.id(),
        label: "Up",
    },
    Choice {
        id: Flow::Down.id(),
        label: "Down",
    },
];

pub const FLOW_KEY: &str = "flow";

pub fn flow_setting(flow: Flow) -> ViewSetting {
    ViewSetting {
        key: FLOW_KEY,
        label: "Flow",
        section: "Display",
        kind: SettingKind::Choice(FLOW_CHOICES),
        value: SettingValue::Choice(flow.id().to_owned()),
        default: SettingValue::Choice(Flow::default().id().to_owned()),
    }
}

pub const VISIBLE_SECONDS_KEY: &str = "visible_seconds";

pub fn visible_seconds_setting(value: f32, range: (f32, f32), default: f32) -> ViewSetting {
    ViewSetting {
        key: VISIBLE_SECONDS_KEY,
        label: "Visible time",
        section: "Display",
        kind: SettingKind::Number {
            min: range.0,
            max: range.1,
            unit: unit::SECONDS,
            logarithmic: false,
        },
        value: SettingValue::Number(value),
        default: SettingValue::Number(default),
    }
}

/// The visible span must fit the output's History, even when shorter than `min_seconds`.
/// Use `default_history_seconds` when the output keeps no History.
pub fn visible_range(
    controller: &DataController,
    output: OutputId,
    default_history_seconds: f32,
    min_seconds: f32,
) -> (f32, f32) {
    let history = controller.with_spec(|spec| match spec.outputs().get(output.index()) {
        Some(output) => match output.keep {
            Keep::Seconds(seconds) => seconds,
            Keep::Latest => default_history_seconds,
        },
        None => default_history_seconds,
    });
    (min_seconds.min(history), history)
}

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
