use std::{collections::VecDeque, time::Duration};

use crate::{
    dataflow::ChannelSelection,
    module::{
        Module, ModuleFactory, ModuleFrame, ModuleResponse, ModuleServices, OscilloscopeStyle,
        OscilloscopeStyleContext, OscilloscopeStyleFactory, Registry, RegistryError, StyleOption,
    },
    render::{Circle, Color, ColoredRect, Point, Rect, RenderTarget, Stroke, TextStyle},
    snapshot::TimeDomainReader,
};

pub const OSCILLOSCOPE_MODULE_ID: &str = "taffymeters.oscilloscope";
pub const WAVEFORM_MODULE_ID: &str = "taffymeters.waveform";
pub const STEREOMETER_MODULE_ID: &str = "taffymeters.stereometer";
pub const LEVEL_METER_MODULE_ID: &str = "taffymeters.level_meter";
pub const EMPTY_MODULE_ID: &str = "taffymeters.empty";
pub const LINE_OSCILLOSCOPE_STYLE_ID: &str = "taffymeters.oscilloscope.line";
pub const PERSISTENCE_OSCILLOSCOPE_STYLE_ID: &str = "taffymeters.oscilloscope.persistence";

const OSCILLOSCOPE_STYLES: &[StyleOption] = &[
    StyleOption {
        id: LINE_OSCILLOSCOPE_STYLE_ID,
        label: "Line",
    },
    StyleOption {
        id: PERSISTENCE_OSCILLOSCOPE_STYLE_ID,
        label: "Persistence",
    },
];

pub(super) fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register_oscilloscope_style(LineOscilloscopeStyleFactory)?;
    registry.register_oscilloscope_style(PersistenceOscilloscopeStyleFactory)?;
    registry.register_module(OscilloscopeFactory)?;
    registry.register_module(WaveformFactory)?;
    registry.register_module(StereometerFactory)?;
    registry.register_module(LevelMeterFactory)?;
    registry.register_module(EmptyFactory)
}

struct OscilloscopeFactory;

impl ModuleFactory for OscilloscopeFactory {
    fn type_id(&self) -> &'static str {
        OSCILLOSCOPE_MODULE_ID
    }

    fn display_name(&self) -> &'static str {
        "Oscilloscope"
    }

    fn create(
        &self,
        services: &ModuleServices,
        registry: &Registry,
    ) -> Result<Box<dyn Module>, RegistryError> {
        Ok(Box::new(OscilloscopeModule {
            reader: services.time_domain.clone(),
            channel: ChannelSelection::MonoAverage,
            y_scale: 1.0,
            style: registry.create_oscilloscope_style(LINE_OSCILLOSCOPE_STYLE_ID)?,
            style_id: LINE_OSCILLOSCOPE_STYLE_ID,
        }))
    }
}

struct OscilloscopeModule {
    reader: TimeDomainReader,
    channel: ChannelSelection,
    y_scale: f32,
    style: Box<dyn OscilloscopeStyle>,
    style_id: &'static str,
}

impl Module for OscilloscopeModule {
    fn set_channel(&mut self, channel: ChannelSelection) {
        self.channel = channel;
        self.style.clear();
    }

    fn draw(&mut self, mut frame: ModuleFrame<'_>) -> ModuleResponse {
        if frame
            .pointer
            .is_some_and(|point| frame.rect.contains(point))
            && frame.scroll_delta_y.abs() > 0.01
        {
            self.y_scale = (self.y_scale * (frame.scroll_delta_y * 0.002).exp()).clamp(0.2, 10.0);
        }
        let Some(snapshot) = self.reader.latest() else {
            waiting(&mut frame);
            return repaint(100);
        };
        let center_y = frame.rect.center().y;
        frame.renderer.polyline(
            frame.rect,
            &[
                Point {
                    x: frame.rect.min.x,
                    y: center_y,
                },
                Point {
                    x: frame.rect.max.x,
                    y: center_y,
                },
            ],
            Stroke {
                width: 1.0,
                color: Color::rgba(110, 130, 145, 90),
            },
        );
        self.style.draw(
            OscilloscopeStyleContext {
                snapshot: &snapshot,
                channel: self.channel,
                rect: frame.rect,
                y_scale: self.y_scale,
                monotonic_time: frame.monotonic_time,
            },
            frame.renderer,
        )
    }

    fn style_options(&self) -> &'static [StyleOption] {
        OSCILLOSCOPE_STYLES
    }

    fn active_style(&self) -> Option<&'static str> {
        Some(self.style_id)
    }

    fn set_style(&mut self, style_id: &str, registry: &Registry) -> Result<(), RegistryError> {
        let option = OSCILLOSCOPE_STYLES
            .iter()
            .find(|option| option.id == style_id)
            .ok_or_else(|| RegistryError::UnknownStyle(style_id.to_owned()))?;
        self.style = registry.create_oscilloscope_style(option.id)?;
        self.style_id = option.id;
        Ok(())
    }

    fn clear_visual_state(&mut self) {
        self.style.clear();
    }
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
        repaint(33)
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
            repaint(16)
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

struct WaveformFactory;

impl ModuleFactory for WaveformFactory {
    fn type_id(&self) -> &'static str {
        WAVEFORM_MODULE_ID
    }

    fn display_name(&self) -> &'static str {
        "Waveform"
    }

    fn create(
        &self,
        services: &ModuleServices,
        _registry: &Registry,
    ) -> Result<Box<dyn Module>, RegistryError> {
        Ok(Box::new(WaveformModule {
            reader: services.time_domain.clone(),
            channel: ChannelSelection::MonoAverage,
            seconds: 4.0,
            y_scale: 1.0,
            bars: Vec::with_capacity(2048),
            minima: Vec::new(),
            maxima: Vec::new(),
        }))
    }
}

struct WaveformModule {
    reader: TimeDomainReader,
    channel: ChannelSelection,
    seconds: f32,
    y_scale: f32,
    bars: Vec<ColoredRect>,
    minima: Vec<f32>,
    maxima: Vec<f32>,
}

impl Module for WaveformModule {
    fn set_channel(&mut self, channel: ChannelSelection) {
        self.channel = channel;
    }

    fn draw(&mut self, mut frame: ModuleFrame<'_>) -> ModuleResponse {
        if frame
            .pointer
            .is_some_and(|point| frame.rect.contains(point))
        {
            if (frame.zoom_delta - 1.0).abs() > 0.001 {
                self.seconds = (self.seconds / frame.zoom_delta).clamp(0.25, 10.0);
            }
            if frame.scroll_delta_y.abs() > 0.01 {
                self.y_scale =
                    (self.y_scale * (frame.scroll_delta_y * 0.002).exp()).clamp(0.1, 20.0);
            }
        }
        let Some(snapshot) = self.reader.latest() else {
            waiting(&mut frame);
            return repaint(100);
        };
        let Ok(channel) = self.channel.resolve(&snapshot.descriptor) else {
            return ModuleResponse::default();
        };
        let columns = frame.rect.width().floor().max(1.0) as usize;
        self.minima.clear();
        self.maxima.clear();
        self.minima.resize(columns, f32::INFINITY);
        self.maxima.resize(columns, -f32::INFINITY);
        let span_frames = (self.seconds * snapshot.descriptor.sample_rate_hz as f32) as u64;
        let first_frame = snapshot
            .processed_through_frame
            .saturating_sub(span_frames.max(1));
        let channels = snapshot.descriptor.channel_count as usize;
        for chunk in snapshot.chunks.iter() {
            if chunk.start_frame.saturating_add(chunk.frame_count as u64) <= first_frame {
                continue;
            }
            for (offset, samples) in chunk.interleaved.chunks_exact(channels).enumerate() {
                let source_frame = chunk.start_frame + offset as u64;
                if source_frame < first_frame {
                    continue;
                }
                let relative = source_frame.saturating_sub(first_frame);
                let column = ((relative as u128 * columns as u128) / span_frames.max(1) as u128)
                    .min(columns.saturating_sub(1) as u128) as usize;
                let sample = channel.sample(samples);
                self.minima[column] = self.minima[column].min(sample);
                self.maxima[column] = self.maxima[column].max(sample);
            }
        }
        self.bars.clear();
        for column in 0..columns {
            if !self.minima[column].is_finite() {
                continue;
            }
            let center = frame.rect.center().y;
            let half_height = frame.rect.height() * 0.5;
            let top = center - (self.maxima[column] * self.y_scale).clamp(-1.0, 1.0) * half_height;
            let bottom =
                center - (self.minima[column] * self.y_scale).clamp(-1.0, 1.0) * half_height;
            let amplitude = self.maxima[column].abs().max(self.minima[column].abs()) * self.y_scale;
            self.bars.push(ColoredRect {
                rect: Rect {
                    min: Point {
                        x: frame.rect.min.x + column as f32,
                        y: top.min(bottom),
                    },
                    max: Point {
                        x: frame.rect.min.x + column as f32 + 1.0,
                        y: top.max(bottom).max(top.min(bottom) + 1.0),
                    },
                },
                color: amplitude_color(amplitude),
            });
        }
        frame.renderer.fill_rects(frame.rect, &self.bars);
        repaint(33)
    }
}

struct StereometerFactory;

impl ModuleFactory for StereometerFactory {
    fn type_id(&self) -> &'static str {
        STEREOMETER_MODULE_ID
    }

    fn display_name(&self) -> &'static str {
        "Stereometer"
    }

    fn create(
        &self,
        services: &ModuleServices,
        _registry: &Registry,
    ) -> Result<Box<dyn Module>, RegistryError> {
        Ok(Box::new(StereometerModule {
            reader: services.time_domain.clone(),
            scale: 1.0,
            circles: Vec::with_capacity(512),
        }))
    }
}

struct StereometerModule {
    reader: TimeDomainReader,
    scale: f32,
    circles: Vec<Circle>,
}

impl Module for StereometerModule {
    fn draw(&mut self, mut frame: ModuleFrame<'_>) -> ModuleResponse {
        if frame
            .pointer
            .is_some_and(|point| frame.rect.contains(point))
            && frame.scroll_delta_y.abs() > 0.01
        {
            self.scale = (self.scale * (frame.scroll_delta_y * 0.002).exp()).clamp(0.5, 10.0);
        }
        let Some(snapshot) = self.reader.latest() else {
            waiting(&mut frame);
            return repaint(100);
        };
        if snapshot.descriptor.channel_count < 2 {
            frame.renderer.text(
                frame.rect,
                frame.rect.center(),
                "Stereo input required",
                TextStyle {
                    size: 13.0,
                    color: Color::rgb(150, 160, 170),
                },
            );
            return ModuleResponse::default();
        }
        let center = frame.rect.center();
        let radius = frame.rect.width().min(frame.rect.height()) * 0.45;
        let guide = Stroke {
            width: 1.0,
            color: Color::rgba(110, 125, 135, 100),
        };
        frame.renderer.polyline(
            frame.rect,
            &[
                Point {
                    x: center.x,
                    y: center.y - radius,
                },
                Point {
                    x: center.x,
                    y: center.y + radius,
                },
            ],
            guide,
        );
        frame.renderer.polyline(
            frame.rect,
            &[
                Point {
                    x: center.x - radius,
                    y: center.y,
                },
                Point {
                    x: center.x + radius,
                    y: center.y,
                },
            ],
            guide,
        );
        self.circles.clear();
        if let Some(chunk) = snapshot.chunks.last() {
            let channels = snapshot.descriptor.channel_count as usize;
            let step = chunk.frame_count.div_ceil(512).max(1);
            for samples in chunk.interleaved.chunks_exact(channels).step_by(step) {
                let left = samples[0];
                let right = samples[1];
                self.circles.push(Circle {
                    center: Point {
                        x: center.x + (left - right) * 0.707 * radius * self.scale,
                        y: center.y - (left + right) * 0.707 * radius * self.scale,
                    },
                    radius: 1.4,
                    color: Color::rgba(100, 220, 255, 180),
                });
            }
        }
        frame.renderer.circles(frame.rect, &self.circles);
        repaint(33)
    }
}

struct LevelMeterFactory;

impl ModuleFactory for LevelMeterFactory {
    fn type_id(&self) -> &'static str {
        LEVEL_METER_MODULE_ID
    }

    fn display_name(&self) -> &'static str {
        "Level Meter"
    }

    fn create(
        &self,
        services: &ModuleServices,
        _registry: &Registry,
    ) -> Result<Box<dyn Module>, RegistryError> {
        Ok(Box::new(LevelMeterModule {
            reader: services.time_domain.clone(),
            held_lufs: Vec::new(),
            hold_until: Vec::new(),
            bars: Vec::with_capacity(12),
            text: String::with_capacity(64),
        }))
    }
}

struct LevelMeterModule {
    reader: TimeDomainReader,
    held_lufs: Vec<f32>,
    hold_until: Vec<Duration>,
    bars: Vec<ColoredRect>,
    text: String,
}

impl Module for LevelMeterModule {
    fn draw(&mut self, mut frame: ModuleFrame<'_>) -> ModuleResponse {
        let Some(snapshot) = self.reader.latest() else {
            waiting(&mut frame);
            return repaint(100);
        };
        let shown_channels = snapshot.momentary_lufs.len().min(8);
        self.held_lufs.resize(shown_channels, -60.0);
        self.hold_until.resize(shown_channels, Duration::ZERO);
        let gap = 5.0;
        let width =
            (frame.rect.width() - gap * (shown_channels + 1) as f32) / shown_channels.max(1) as f32;
        self.bars.clear();
        let mut animating = false;
        for channel in 0..shown_channels {
            let current = snapshot.momentary_lufs[channel].clamp(-60.0, 3.0);
            if current >= self.held_lufs[channel] {
                self.held_lufs[channel] = current;
                self.hold_until[channel] = frame.monotonic_time + Duration::from_secs(3);
            } else if frame.monotonic_time >= self.hold_until[channel] {
                self.held_lufs[channel] = (self.held_lufs[channel] - 0.5).max(current);
                animating |= self.held_lufs[channel] > current;
            }
            let left = frame.rect.min.x + gap + channel as f32 * (width + gap);
            let bar = Rect {
                min: Point {
                    x: left,
                    y: frame.rect.min.y + 8.0,
                },
                max: Point {
                    x: left + width,
                    y: frame.rect.max.y - 24.0,
                },
            };
            self.bars.push(ColoredRect {
                rect: bar,
                color: Color::rgba(20, 28, 34, 220),
            });
            let normalized = ((current + 60.0) / 63.0).clamp(0.0, 1.0);
            self.bars.push(ColoredRect {
                rect: Rect {
                    min: Point {
                        x: bar.min.x,
                        y: bar.max.y - bar.height() * normalized,
                    },
                    max: bar.max,
                },
                color: meter_color(normalized),
            });
            let peak = ((self.held_lufs[channel] + 60.0) / 63.0).clamp(0.0, 1.0);
            let peak_y = bar.max.y - bar.height() * peak;
            self.bars.push(ColoredRect {
                rect: Rect {
                    min: Point {
                        x: bar.min.x,
                        y: peak_y,
                    },
                    max: Point {
                        x: bar.max.x,
                        y: peak_y + 2.0,
                    },
                },
                color: Color::rgb(245, 245, 245),
            });
            self.text.clear();
            use std::fmt::Write as _;
            let _ = write!(self.text, "{} {:.1}", channel + 1, current);
            frame.renderer.text(
                frame.rect,
                Point {
                    x: bar.min.x,
                    y: bar.max.y + 5.0,
                },
                &self.text,
                TextStyle {
                    size: 9.0,
                    color: Color::rgb(160, 175, 185),
                },
            );
        }
        frame.renderer.fill_rects(frame.rect, &self.bars);
        if animating { repaint(33) } else { repaint(100) }
    }
}

struct EmptyFactory;

impl ModuleFactory for EmptyFactory {
    fn type_id(&self) -> &'static str {
        EMPTY_MODULE_ID
    }

    fn display_name(&self) -> &'static str {
        "Empty"
    }

    fn create(
        &self,
        _services: &ModuleServices,
        _registry: &Registry,
    ) -> Result<Box<dyn Module>, RegistryError> {
        Ok(Box::new(EmptyModule))
    }
}

struct EmptyModule;

impl Module for EmptyModule {
    fn draw(&mut self, _frame: ModuleFrame<'_>) -> ModuleResponse {
        ModuleResponse::default()
    }
}

fn waiting(frame: &mut ModuleFrame<'_>) {
    frame.renderer.text(
        frame.rect,
        Point {
            x: frame.rect.min.x + 8.0,
            y: frame.rect.min.y + 8.0,
        },
        "WAITING FOR SYSTEM AUDIO",
        TextStyle {
            size: 12.0,
            color: Color::rgb(190, 205, 215),
        },
    );
}

fn repaint(milliseconds: u64) -> ModuleResponse {
    ModuleResponse {
        repaint_after: Some(Duration::from_millis(milliseconds)),
    }
}

fn amplitude_color(amplitude: f32) -> Color {
    let amplitude = amplitude.clamp(0.0, 1.0);
    if amplitude < 0.45 {
        Color::rgb(80, 180, 240)
    } else if amplitude < 0.8 {
        Color::rgb(165, 90, 245)
    } else {
        Color::rgb(255, 70, 120)
    }
}

fn meter_color(normalized: f32) -> Color {
    if normalized < 0.7 {
        Color::rgb(80, 180, 240)
    } else if normalized < 0.85 {
        Color::rgb(80, 205, 105)
    } else {
        Color::rgb(235, 80, 55)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        audio::{AudioDescriptor, ChannelLayout, Generation, SourceId},
        render::Circle,
        snapshot::{AudioChunk, TimeDomainSnapshot},
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
            momentary_lufs: vec![-20.0, -20.0].into(),
            sample_peaks: vec![0.5, 0.5].into(),
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
