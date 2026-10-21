use std::time::Duration;

use crate::{
    audio::AudioDescriptor,
    dataflow::ChannelSelection,
    module::ModuleResponse,
    render::{Rect, RenderTarget},
    snapshot::Frame,
    theme::{ColorRole, Palette},
};

pub struct OscilloscopeStyleContext<'a> {
    pub descriptor: &'a AudioDescriptor,
    /// Latest merged source PCM chunk.
    pub frame: &'a Frame,
    pub channel: ChannelSelection,
    pub rect: Rect,
    /// Includes the workspace reference gain; styles must not apply it again.
    pub y_scale: f32,
    pub monotonic_time: Duration,
    pub palette: &'a Palette,
}

pub trait OscilloscopeStyle: Send {
    fn draw(
        &mut self,
        context: OscilloscopeStyleContext<'_>,
        renderer: &mut dyn RenderTarget,
    ) -> ModuleResponse;
    /// Report every theme role this style draws with for the instance's color overrides.
    fn color_roles(&self) -> &'static [ColorRole];
}

pub trait OscilloscopeStyleFactory: Send + Sync {
    fn style_id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn create(&self) -> Box<dyn OscilloscopeStyle>;
}
