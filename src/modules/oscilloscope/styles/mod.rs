mod line;

use crate::{
    extensions::{Registry, RegistryError},
    modules::oscilloscope::style::OscilloscopeStyleContext,
    render::Point,
};

pub const LINE_OSCILLOSCOPE_STYLE_ID: &str = "taffymeters.oscilloscope.line";

pub(super) fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register_oscilloscope_style(line::LineOscilloscopeStyleFactory)
}

/// Preserve narrow peaks with per-column extrema in sample order, avoiding a false zigzag.
fn build_latest_trace(context: &OscilloscopeStyleContext<'_>, output: &mut Vec<Point>) {
    output.clear();
    let Some(chunk) = context.frame.pcm() else {
        return;
    };
    let channels = context.descriptor.channel_count as usize;
    let Ok(channel) = context.channel.resolve(context.descriptor) else {
        return;
    };
    let frames = chunk.frame_count;
    if frames == 0 {
        return;
    }
    let rect = context.rect;
    let x_per_frame = rect.width() / frames.saturating_sub(1).max(1) as f32;
    let center_y = rect.center().y;
    let y_per_unit = context.y_scale * rect.height() * 0.5;
    let point = |index: usize, sample: f32| Point {
        x: rect.min.x + index as f32 * x_per_frame,
        y: center_y - sample.clamp(-1.0, 1.0) * y_per_unit,
    };
    // Mix before reduction: channel extrema do not determine the mixed signal's extrema.
    let mut samples = chunk
        .interleaved
        .chunks_exact(channels)
        .map(|frame| channel.sample(frame))
        .enumerate();
    let columns = (rect.width().max(1.0) as usize).max(1);
    if frames <= columns {
        output.extend(samples.map(|(index, sample)| point(index, sample)));
        return;
    }

    let mut last = None;
    let mut push = |output: &mut Vec<Point>, (index, sample): (usize, f32)| {
        if last.is_none_or(|last| index > last) {
            output.push(point(index, sample));
            last = Some(index);
        }
    };
    let mut final_sample = (0, 0.0);
    for column in 0..columns {
        // More frames than columns guarantees at least one sample per column.
        let end = (column + 1) * frames / columns;
        let Some(first) = samples.next() else {
            break;
        };
        if column == 0 {
            push(output, first);
        }
        let (mut low, mut high) = (first, first);
        final_sample = first;
        for sample in samples.by_ref().take(end - first.0 - 1) {
            if sample.1 < low.1 {
                low = sample;
            }
            if sample.1 > high.1 {
                high = sample;
            }
            final_sample = sample;
        }
        let (earlier, later) = if low.0 <= high.0 {
            (low, high)
        } else {
            (high, low)
        };
        push(output, earlier);
        push(output, later);
    }
    push(output, final_sample);
}
