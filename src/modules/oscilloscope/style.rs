use std::time::Duration;

use crate::{
    audio::AudioDescriptor,
    dataflow::ChannelSelection,
    module::ModuleResponse,
    render::{Rect, RenderTarget},
    snapshot::Frame,
    theme::Palette,
};

pub struct OscilloscopeStyleContext<'a> {
    pub descriptor: &'a AudioDescriptor,
    /// The latest merged PCM chunk.
    pub frame: &'a Frame,
    pub channel: ChannelSelection,
    pub rect: Rect,
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
    fn clear(&mut self) {}
}

pub trait OscilloscopeStyleFactory: Send + Sync {
    fn style_id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn create(&self) -> Box<dyn OscilloscopeStyle>;
}
