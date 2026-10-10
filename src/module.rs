use std::{error::Error, fmt, time::Duration};

use crate::{
    audio::SourceReader,
    dataflow::{
        ChannelError, ChannelSelection, ControlError, DataController, DataSpec, DataflowHandle,
        OutputId,
    },
    extensions::{Registry, RegistryError},
    render::{Point, Rect, RenderTarget, Size},
    settings::{SettingValue, ViewSetting},
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
    pub renderer: &'a mut dyn RenderTarget,
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

pub trait Module: Send {
    fn draw(&mut self, frame: ModuleFrame<'_>) -> ModuleResponse;

    /// Smallest area, in logical points, the Module accepts. The Workspace never
    /// shrinks a layout leaf below it unless the window itself is forced smaller. The
    /// default only keeps a leaf large enough to stay reachable (its split entries and
    /// body); content simply scales down.
    fn min_size(&self) -> Size {
        Size {
            width: 40.0,
            height: 32.0,
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

    fn clear_visual_state(&mut self) {}

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

    /// Theme roles the instance draws with; its color overrides offer these.
    fn color_roles(&self) -> &'static [ColorRole] {
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
