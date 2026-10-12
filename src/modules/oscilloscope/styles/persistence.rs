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
        modules::test_support::RecordingTarget,
        render::Rect,
        snapshot::{AudioChunk, Frame, FrameData},
        theme::Palette,
    };

    fn descriptor() -> AudioDescriptor {
        AudioDescriptor::new(SourceId(1), Generation(1), 48_000, 2, ChannelLayout::Stereo).unwrap()
    }

    fn frame_with(version: u64, samples: Vec<f32>) -> Frame {
        Frame {
            result_version: version,
            processed_through_frame: (samples.len() / 2) as u64,
            source_sequence: 0,
            discontinuity_count: 0,
            discontinuity_before: false,
            data: FrameData::Pcm(AudioChunk {
                start_frame: 0,
                frame_count: samples.len() / 2,
                interleaved: samples,
            }),
        }
    }

    fn chunk(version: u64) -> Frame {
        let samples: Vec<f32> = (0..128)
            .flat_map(|index| {
                let sample = (index as f32 * 0.1).sin() * 0.5;
                [sample, -sample]
            })
            .collect();
        frame_with(version, samples)
    }

    /// Both channels at a constant `level`, so the whole trace is one horizontal
    /// line whose height identifies the frame.
    fn flat_frame(version: u64, level: f32) -> Frame {
        frame_with(version, vec![level; 256])
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

    /// Draw `frame` at `millis` into a fresh target.
    fn draw_at(
        style: &mut dyn OscilloscopeStyle,
        frame: &Frame,
        millis: u64,
    ) -> (RecordingTarget, ModuleResponse) {
        let descriptor = descriptor();
        let palette = Palette::default();
        let mut target = RecordingTarget::default();
        let response = style.draw(
            context(&descriptor, frame, Duration::from_millis(millis), &palette),
            &mut target,
        );
        (target, response)
    }

    /// Height of a `flat_frame` trace with `level` in the 200-point-high test rect.
    fn flat_y(level: f32) -> f32 {
        100.0 - level * 100.0
    }

    #[test]
    fn persistence_instances_keep_and_clear_visual_state_independently() {
        let snapshot = chunk(1);
        let mut first = PersistenceOscilloscopeStyleFactory.create();
        let mut second = PersistenceOscilloscopeStyleFactory.create();
        draw_at(first.as_mut(), &snapshot, 0);
        draw_at(second.as_mut(), &snapshot, 0);

        first.clear();
        let (first_target, _) = draw_at(first.as_mut(), &snapshot, 100);
        let (second_target, _) = draw_at(second.as_mut(), &snapshot, 100);

        assert_eq!(first_target.non_empty_polylines(), 0);
        assert_eq!(second_target.non_empty_polylines(), 1);
    }

    #[test]
    fn traces_expire_by_monotonic_time_not_by_draw_count() {
        let mut style = PersistenceOscilloscopeStyleFactory.create();
        let old = flat_frame(1, 0.2);
        let new = flat_frame(2, 0.4);

        let (target, _) = draw_at(style.as_mut(), &old, 0);
        assert_eq!(target.polylines.len(), 1);
        // Many redraws of the same result in a short time neither add nor evict.
        for millis in 1..=200 {
            let (target, _) = draw_at(style.as_mut(), &old, millis);
            assert_eq!(target.polylines.len(), 1, "redraw at {millis} ms");
        }
        let (target, _) = draw_at(style.as_mut(), &new, 500);
        assert_eq!(target.polylines.len(), 2);

        // Older traces fade more than newer ones.
        let (target, _) = draw_at(style.as_mut(), &new, 600);
        assert_eq!(target.polylines.len(), 2);
        assert!(
            target.polylines[0].stroke.color.alpha < target.polylines[1].stroke.color.alpha,
            "the older trace must be fainter"
        );

        let (target, _) = draw_at(style.as_mut(), &new, 899);
        assert_eq!(
            target.polylines.len(),
            2,
            "the first trace is not yet 900 ms old"
        );

        // The first trace is now older than 900 ms; the second (401 ms) stays.
        let (target, response) = draw_at(style.as_mut(), &new, 901);
        assert_eq!(target.polylines.len(), 1);
        assert!((target.polylines[0].points[0].y - flat_y(0.4)).abs() < 0.001);
        assert!(
            response.repaint_after.is_some(),
            "a live trace keeps animating"
        );

        let (target, response) = draw_at(style.as_mut(), &new, 1_401);
        assert!(target.polylines.is_empty());
        assert_eq!(response.repaint_after, None, "nothing left to animate");
    }

    #[test]
    fn persistence_keeps_only_the_newest_24_traces() {
        let mut style = PersistenceOscilloscopeStyleFactory.create();
        let mut last = RecordingTarget::default();
        // All at one instant, so only the count limit can evict.
        for version in 1..=30_u64 {
            let frame = flat_frame(version, version as f32 * 0.02);
            let (target, _) = draw_at(style.as_mut(), &frame, 0);
            assert_eq!(
                target.polylines.len(),
                (version as usize).min(24),
                "after {version} results"
            );
            last = target;
        }

        // Versions 7..=30 remain, oldest first.
        let heights: Vec<f32> = last
            .polylines
            .iter()
            .map(|polyline| polyline.points[0].y)
            .collect();
        let expected: Vec<f32> = (7..=30)
            .map(|version| flat_y(version as f32 * 0.02))
            .collect();
        assert_eq!(heights.len(), expected.len());
        for (height, want) in heights.iter().zip(&expected) {
            assert!((height - want).abs() < 0.001, "{heights:?} vs {expected:?}");
        }
    }
}
