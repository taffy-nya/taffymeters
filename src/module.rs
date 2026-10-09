use std::{error::Error, fmt, time::Duration};

use crate::{
    audio::SourceReader,
    dataflow::{ChannelError, ChannelSelection, ControlError, DataflowHandle},
    extensions::{Registry, RegistryError},
    render::{Point, Rect, RenderTarget, Size},
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
}

pub trait ModuleFactory: Send + Sync {
    fn type_id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;

    fn create(
        &self,
        services: &ModuleServices,
        registry: &Registry,
    ) -> Result<Box<dyn Module>, RegistryError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModuleError {
    UnsupportedChannel,
    InvalidChannel(ChannelError),
    Control(ControlError),
}

impl fmt::Display for ModuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedChannel => f.write_str("module does not support channel selection"),
            Self::InvalidChannel(error) => write!(f, "invalid channel selection: {error}"),
            Self::Control(error) => write!(f, "could not update module analysis: {error}"),
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
