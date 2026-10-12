mod line;
mod persistence;

use crate::{
    extensions::{Registry, RegistryError},
    modules::oscilloscope::style::OscilloscopeStyleContext,
    render::Point,
};

pub const LINE_OSCILLOSCOPE_STYLE_ID: &str = "taffymeters.oscilloscope.line";
pub const PERSISTENCE_OSCILLOSCOPE_STYLE_ID: &str = "taffymeters.oscilloscope.persistence";

pub(super) fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register_oscilloscope_style(line::LineOscilloscopeStyleFactory)?;
    registry.register_oscilloscope_style(persistence::PersistenceOscilloscopeStyleFactory)
}

fn build_latest_trace(context: &OscilloscopeStyleContext<'_>, output: &mut Vec<Point>) {
    output.clear();
    let Some(chunk) = context.frame.pcm() else {
        return;
    };
    let channels = context.descriptor.channel_count as usize;
    let Ok(channel) = context.channel.resolve(context.descriptor) else {
        return;
    };
    let width = context.rect.width().max(1.0) as usize;
    let step = chunk.frame_count.div_ceil(width).max(1);
    let count = chunk.frame_count.div_ceil(step);
    let denominator = count.saturating_sub(1).max(1) as f32;
    for (index, frame) in chunk
        .interleaved
        .chunks_exact(channels)
        .step_by(step)
        .enumerate()
    {
        let fraction = index as f32 / denominator;
        let sample = channel.sample(frame).clamp(-1.0, 1.0) * context.y_scale;
        output.push(Point {
            x: context.rect.min.x + fraction * context.rect.width(),
            y: context.rect.center().y - sample * context.rect.height() * 0.5,
        });
    }
}
