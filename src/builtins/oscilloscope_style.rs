use std::{collections::VecDeque, time::Duration};

use crate::{
    dataflow::ChannelSelection,
    extensions::{Registry, RegistryError},
    module::ModuleResponse,
    render::{Color, Point, Rect, RenderTarget, Stroke},
    snapshot::TimeDomainSnapshot,
};

pub const LINE_OSCILLOSCOPE_STYLE_ID: &str = "taffymeters.oscilloscope.line";
pub const PERSISTENCE_OSCILLOSCOPE_STYLE_ID: &str = "taffymeters.oscilloscope.persistence";

pub struct OscilloscopeStyleContext<'a> {
    pub snapshot: &'a TimeDomainSnapshot,
    pub channel: ChannelSelection,
    pub rect: Rect,
    pub y_scale: f32,
    pub monotonic_time: Duration,
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

pub(super) fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register_oscilloscope_style(LineOscilloscopeStyleFactory)?;
    registry.register_oscilloscope_style(PersistenceOscilloscopeStyleFactory)
}

struct LineOscilloscopeStyleFactory;

impl OscilloscopeStyleFactory for LineOscilloscopeStyleFactory {
    fn style_id(&self) -> &'static str {
        LINE_OSCILLOSCOPE_STYLE_ID
    }

    fn display_name(&self) -> &'static str {
        "Line"
    }

    fn create(&self) -> Box<dyn OscilloscopeStyle> {
        Box::new(LineOscilloscopeStyle {
            points: Vec::with_capacity(2048),
        })
    }
}

struct LineOscilloscopeStyle {
    points: Vec<Point>,
}

impl OscilloscopeStyle for LineOscilloscopeStyle {
    fn draw(
        &mut self,
        context: OscilloscopeStyleContext<'_>,
        renderer: &mut dyn RenderTarget,
    ) -> ModuleResponse {
        build_latest_trace(&context, &mut self.points);
        renderer.polyline(
            context.rect,
            &self.points,
            Stroke {
                width: 1.4,
                color: Color::rgb(100, 220, 255),
            },
        );
        animate()
    }
}

struct PersistenceOscilloscopeStyleFactory;

impl OscilloscopeStyleFactory for PersistenceOscilloscopeStyleFactory {
    fn style_id(&self) -> &'static str {
        PERSISTENCE_OSCILLOSCOPE_STYLE_ID
    }

    fn display_name(&self) -> &'static str {
        "Persistence"
    }

    fn create(&self) -> Box<dyn OscilloscopeStyle> {
        Box::new(PersistenceOscilloscopeStyle {
            traces: VecDeque::with_capacity(24),
            latest_version: 0,
            scratch: Vec::with_capacity(2048),
        })
    }
}

struct Trace {
    captured_at: Duration,
    points: Vec<Point>,
}

struct PersistenceOscilloscopeStyle {
    traces: VecDeque<Trace>,
    latest_version: u64,
    scratch: Vec<Point>,
}

impl OscilloscopeStyle for PersistenceOscilloscopeStyle {
    fn draw(
        &mut self,
        context: OscilloscopeStyleContext<'_>,
        renderer: &mut dyn RenderTarget,
    ) -> ModuleResponse {
        const LIFETIME: Duration = Duration::from_millis(900);
        while self.traces.front().is_some_and(|trace| {
            context.monotonic_time.saturating_sub(trace.captured_at) >= LIFETIME
        }) {
            self.traces.pop_front();
        }
        if context.snapshot.result_version != self.latest_version {
            self.latest_version = context.snapshot.result_version;
            build_latest_trace(&context, &mut self.scratch);
            let points = std::mem::take(&mut self.scratch);
            self.traces.push_back(Trace {
                captured_at: context.monotonic_time,
                points,
            });
            if self.traces.len() > 24
                && let Some(mut oldest) = self.traces.pop_front()
            {
                oldest.points.clear();
                self.scratch = oldest.points;
            }
        }
        for trace in &self.traces {
            let age = context
                .monotonic_time
                .saturating_sub(trace.captured_at)
                .as_secs_f32();
            let alpha = ((1.0 - age / LIFETIME.as_secs_f32()).clamp(0.0, 1.0) * 190.0) as u8;
            renderer.polyline(
                context.rect,
                &trace.points,
                Stroke {
                    width: 1.3,
                    color: Color::rgba(100, 230, 255, alpha),
                },
            );
        }
        if self.traces.is_empty() {
            ModuleResponse::default()
        } else {
            animate()
        }
    }

    fn clear(&mut self) {
        self.traces.clear();
        self.scratch.clear();
    }
}

fn build_latest_trace(context: &OscilloscopeStyleContext<'_>, output: &mut Vec<Point>) {
    output.clear();
    let Some(chunk) = context.snapshot.chunks.last() else {
        return;
    };
    let channels = context.snapshot.descriptor.channel_count as usize;
    let Ok(channel) = context.channel.resolve(&context.snapshot.descriptor) else {
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

fn animate() -> ModuleResponse {
    ModuleResponse {
        repaint_after: Some(crate::module::FRAME_INTERVAL),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        audio::{AudioDescriptor, ChannelLayout, Generation, SourceId},
        render::{Circle, ColoredRect, TextStyle},
        snapshot::AudioChunk,
    };

    #[derive(Default)]
    struct RecordingTarget {
        polylines: usize,
    }

    impl RenderTarget for RecordingTarget {
        fn polyline(&mut self, _clip: Rect, points: &[Point], _stroke: Stroke) {
            if !points.is_empty() {
                self.polylines += 1;
            }
        }

        fn fill_rects(&mut self, _clip: Rect, _rects: &[ColoredRect]) {}

        fn circles(&mut self, _clip: Rect, _circles: &[Circle]) {}

        fn text(&mut self, _clip: Rect, _position: Point, _text: &str, _style: TextStyle) {}
    }

    fn snapshot(version: u64) -> TimeDomainSnapshot {
        let descriptor = Arc::new(
            AudioDescriptor::new(SourceId(1), Generation(1), 48_000, 2, ChannelLayout::Stereo)
                .unwrap(),
        );
        let samples: Vec<f32> = (0..128)
            .flat_map(|index| {
                let sample = (index as f32 * 0.1).sin() * 0.5;
                [sample, -sample]
            })
            .collect();
        TimeDomainSnapshot {
            descriptor,
            result_version: version,
            processed_through_frame: 128,
            discontinuity_count: 0,
            chunks: vec![Arc::new(AudioChunk {
                start_frame: 0,
                frame_count: 128,
                source_sequence: 0,
                discontinuity_before: false,
                interleaved: samples.into(),
            })]
            .into(),
            levels: None,
        }
    }

    fn context(snapshot: &TimeDomainSnapshot, time: Duration) -> OscilloscopeStyleContext<'_> {
        OscilloscopeStyleContext {
            snapshot,
            channel: ChannelSelection::MonoAverage,
            rect: Rect {
                min: Point { x: 0.0, y: 0.0 },
                max: Point { x: 400.0, y: 200.0 },
            },
            y_scale: 1.0,
            monotonic_time: time,
        }
    }

    #[test]
    fn persistence_instances_keep_and_clear_visual_state_independently() {
        let snapshot = snapshot(1);
        let mut first = PersistenceOscilloscopeStyleFactory.create();
        let mut second = PersistenceOscilloscopeStyleFactory.create();
        let mut first_target = RecordingTarget::default();
        let mut second_target = RecordingTarget::default();
        first.draw(context(&snapshot, Duration::ZERO), &mut first_target);
        second.draw(context(&snapshot, Duration::ZERO), &mut second_target);

        first.clear();
        first_target.polylines = 0;
        second_target.polylines = 0;
        first.draw(
            context(&snapshot, Duration::from_millis(100)),
            &mut first_target,
        );
        second.draw(
            context(&snapshot, Duration::from_millis(100)),
            &mut second_target,
        );

        assert_eq!(first_target.polylines, 0);
        assert_eq!(second_target.polylines, 1);
    }
}
