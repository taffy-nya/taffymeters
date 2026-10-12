use std::{collections::VecDeque, time::Duration};

use super::{PERSISTENCE_OSCILLOSCOPE_STYLE_ID, build_latest_trace};
use crate::{
    module::ModuleResponse,
    modules::oscilloscope::style::{
        OscilloscopeStyle, OscilloscopeStyleContext, OscilloscopeStyleFactory,
    },
    render::{Point, RenderTarget, Stroke},
    theme::ColorRole,
};

pub(super) struct PersistenceOscilloscopeStyleFactory;

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
        if context.frame.result_version != self.latest_version {
            self.latest_version = context.frame.result_version;
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
        let trace_color = context.palette.color(ColorRole::Primary);
        for trace in &self.traces {
            let age = context
                .monotonic_time
                .saturating_sub(trace.captured_at)
                .as_secs_f32();
            let fade = (1.0 - age / LIFETIME.as_secs_f32()).clamp(0.0, 1.0);
            let alpha = (fade * 190.0 * f32::from(trace_color.alpha) / 255.0) as u8;
            renderer.polyline(
                context.rect,
                &trace.points,
                Stroke {
                    width: 1.3,
                    color: trace_color.with_alpha(alpha),
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

fn animate() -> ModuleResponse {
    ModuleResponse {
        repaint_after: Some(crate::module::FRAME_INTERVAL),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        audio::{AudioDescriptor, ChannelLayout, Generation, SourceId},
        dataflow::ChannelSelection,
        render::{Circle, ColoredRect, Rect, TextStyle},
        snapshot::{AudioChunk, Frame, FrameData},
        theme::Palette,
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

    fn descriptor() -> AudioDescriptor {
        AudioDescriptor::new(SourceId(1), Generation(1), 48_000, 2, ChannelLayout::Stereo).unwrap()
    }

    fn chunk(version: u64) -> Frame {
        let samples: Vec<f32> = (0..128)
            .flat_map(|index| {
                let sample = (index as f32 * 0.1).sin() * 0.5;
                [sample, -sample]
            })
            .collect();
        Frame {
            result_version: version,
            processed_through_frame: 128,
            source_sequence: 0,
            discontinuity_count: 0,
            discontinuity_before: false,
            data: FrameData::Pcm(AudioChunk {
                start_frame: 0,
                frame_count: 128,
                interleaved: samples,
            }),
        }
    }

    fn context<'a>(
        descriptor: &'a AudioDescriptor,
        frame: &'a Frame,
        time: Duration,
        palette: &'a Palette,
    ) -> OscilloscopeStyleContext<'a> {
        OscilloscopeStyleContext {
            descriptor,
            frame,
            channel: ChannelSelection::MonoAverage,
            rect: Rect {
                min: Point { x: 0.0, y: 0.0 },
                max: Point { x: 400.0, y: 200.0 },
            },
            y_scale: 1.0,
            monotonic_time: time,
            palette,
        }
    }

    #[test]
    fn persistence_instances_keep_and_clear_visual_state_independently() {
        let descriptor = descriptor();
        let snapshot = chunk(1);
        let palette = Palette::default();
        let mut first = PersistenceOscilloscopeStyleFactory.create();
        let mut second = PersistenceOscilloscopeStyleFactory.create();
        let mut first_target = RecordingTarget::default();
        let mut second_target = RecordingTarget::default();
        first.draw(
            context(&descriptor, &snapshot, Duration::ZERO, &palette),
            &mut first_target,
        );
        second.draw(
            context(&descriptor, &snapshot, Duration::ZERO, &palette),
            &mut second_target,
        );

        first.clear();
        first_target.polylines = 0;
        second_target.polylines = 0;
        first.draw(
            context(&descriptor, &snapshot, Duration::from_millis(100), &palette),
            &mut first_target,
        );
        second.draw(
            context(&descriptor, &snapshot, Duration::from_millis(100), &palette),
            &mut second_target,
        );

        assert_eq!(first_target.polylines, 0);
        assert_eq!(second_target.polylines, 1);
    }
}
