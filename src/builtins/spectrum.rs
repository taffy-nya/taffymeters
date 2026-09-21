use std::{fmt::Write, sync::Arc, time::Duration};

use crate::{
    audio::SourceState,
    module::{
        Module, ModuleFactory, ModuleFrame, ModuleResponse, ModuleServices, Registry,
        RegistryError, SpectrumStyle, SpectrumStyleContext, SpectrumStyleFactory, StyleOption,
    },
    plotting::{FREQUENCY_TICKS_HZ, SpectrumCoordinates, db_ticks},
    render::{Color, ColoredRect, Point, Rect, RenderTarget, Stroke, TextStyle},
    snapshot::{SpectrumReader, SpectrumSnapshotSet},
};

pub const SPECTRUM_MODULE_ID: &str = "taffymeters.spectrum";
pub const DEFAULT_SPECTRUM_STYLE_ID: &str = "taffymeters.spectrum.line";
pub const BAR_SPECTRUM_STYLE_ID: &str = "taffymeters.spectrum.bars";
pub const PIXEL_SPECTRUM_STYLE_ID: &str = "taffymeters.spectrum.pixel";

const STYLE_OPTIONS: &[StyleOption] = &[
    StyleOption {
        id: DEFAULT_SPECTRUM_STYLE_ID,
        label: "Curve",
    },
    StyleOption {
        id: BAR_SPECTRUM_STYLE_ID,
        label: "Bars",
    },
    StyleOption {
        id: PIXEL_SPECTRUM_STYLE_ID,
        label: "Pixel",
    },
];

pub(super) fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register_spectrum_style(LineSpectrumStyleFactory)?;
    registry.register_spectrum_style(BarSpectrumStyleFactory)?;
    registry.register_spectrum_style(PixelSpectrumStyleFactory)?;
    registry.register_module(SpectrumModuleFactory)
}

struct SpectrumModuleFactory;

impl ModuleFactory for SpectrumModuleFactory {
    fn type_id(&self) -> &'static str {
        SPECTRUM_MODULE_ID
    }

    fn display_name(&self) -> &'static str {
        "Spectrum"
    }

    fn create(
        &self,
        services: &ModuleServices,
        registry: &Registry,
    ) -> Result<Box<dyn Module>, RegistryError> {
        Ok(Box::new(SpectrumModule {
            reader: services.spectrum.clone(),
            style: registry.create_spectrum_style(DEFAULT_SPECTRUM_STYLE_ID)?,
            style_id: DEFAULT_SPECTRUM_STYLE_ID,
            coordinates: SpectrumCoordinates {
                min_frequency_hz: 20.0,
                max_frequency_hz: 20_000.0,
                min_dbfs: -100.0,
                max_dbfs: 0.0,
            },
            latest: None,
            label_version: 0,
            label_frequency_range: None,
            label: String::with_capacity(192),
            status_line: String::with_capacity(256),
            hover_line: String::with_capacity(128),
            grid: Vec::with_capacity(24),
        }))
    }
}

struct LineSpectrumStyleFactory;

impl SpectrumStyleFactory for LineSpectrumStyleFactory {
    fn style_id(&self) -> &'static str {
        DEFAULT_SPECTRUM_STYLE_ID
    }

    fn display_name(&self) -> &'static str {
        "Curve"
    }

    fn create(&self) -> Box<dyn SpectrumStyle> {
        Box::new(LineSpectrumStyle {
            points: Vec::with_capacity(2048),
            peak_points: Vec::with_capacity(2048),
        })
    }
}

struct LineSpectrumStyle {
    points: Vec<Point>,
    peak_points: Vec<Point>,
}

impl SpectrumStyle for LineSpectrumStyle {
    fn draw(&mut self, context: SpectrumStyleContext<'_>, renderer: &mut dyn RenderTarget) {
        spectrum_points(
            context.snapshot,
            *context.coordinates,
            context.rect,
            &mut self.points,
            &context.snapshot.spectrum_dbfs,
        );
        renderer.polyline(
            context.rect,
            &self.points,
            Stroke {
                width: 1.6,
                color: Color::rgb(92, 230, 199),
            },
        );
        if let Some(peak) = &context.snapshot.peak_dbfs {
            spectrum_points(
                context.snapshot,
                *context.coordinates,
                context.rect,
                &mut self.peak_points,
                peak,
            );
            renderer.polyline(
                context.rect,
                &self.peak_points,
                Stroke {
                    width: 1.0,
                    color: Color::rgba(255, 210, 100, 180),
                },
            );
        }
    }
}

struct BarSpectrumStyleFactory;

impl SpectrumStyleFactory for BarSpectrumStyleFactory {
    fn style_id(&self) -> &'static str {
        BAR_SPECTRUM_STYLE_ID
    }

    fn display_name(&self) -> &'static str {
        "Bars"
    }

    fn create(&self) -> Box<dyn SpectrumStyle> {
        Box::new(BarSpectrumStyle {
            bars: Vec::with_capacity(512),
        })
    }
}

struct BarSpectrumStyle {
    bars: Vec<ColoredRect>,
}

impl SpectrumStyle for BarSpectrumStyle {
    fn draw(&mut self, context: SpectrumStyleContext<'_>, renderer: &mut dyn RenderTarget) {
        self.bars.clear();
        let width = context.rect.width().max(1.0);
        let bar_count = (width / 6.0).floor().max(1.0) as usize;
        let range = context.coordinates.visible_bin_range(
            context.snapshot.fft.bin_hz,
            context.snapshot.spectrum_dbfs.len(),
        );
        for bar in 0..bar_count {
            let x0 = context.rect.min.x + bar as f32 * width / bar_count as f32;
            let x1 = context.rect.min.x + (bar + 1) as f32 * width / bar_count as f32;
            let f0 = context.coordinates.frequency_at_x(x0, context.rect);
            let f1 = context.coordinates.frequency_at_x(x1, context.rect);
            let first = ((f0 / context.snapshot.fft.bin_hz).floor() as usize).max(range.start);
            let end = ((f1 / context.snapshot.fft.bin_hz).ceil() as usize)
                .saturating_add(1)
                .min(range.end);
            let Some(dbfs) = context.snapshot.spectrum_dbfs[first.min(end)..end]
                .iter()
                .copied()
                .max_by(f32::total_cmp)
            else {
                continue;
            };
            self.bars.push(ColoredRect {
                rect: Rect {
                    min: Point {
                        x: x0 + 0.75,
                        y: context.coordinates.y_for_dbfs(dbfs, context.rect),
                    },
                    max: Point {
                        x: (x1 - 0.75).max(x0 + 1.0),
                        y: context.rect.max.y,
                    },
                },
                color: Color::rgb(100, 190, 255),
            });
        }
        renderer.fill_rects(context.rect, &self.bars);
    }
}

struct PixelSpectrumStyleFactory;

impl SpectrumStyleFactory for PixelSpectrumStyleFactory {
    fn style_id(&self) -> &'static str {
        PIXEL_SPECTRUM_STYLE_ID
    }

    fn display_name(&self) -> &'static str {
        "Pixel"
    }

    fn create(&self) -> Box<dyn SpectrumStyle> {
        Box::new(PixelSpectrumStyle {
            pixels: Vec::with_capacity(2048),
        })
    }
}

struct PixelSpectrumStyle {
    pixels: Vec<ColoredRect>,
}

impl SpectrumStyle for PixelSpectrumStyle {
    fn draw(&mut self, context: SpectrumStyleContext<'_>, renderer: &mut dyn RenderTarget) {
        self.pixels.clear();
        let cell = 7.0;
        let columns = (context.rect.width() / cell).floor().max(1.0) as usize;
        for column in 0..columns {
            let x = context.rect.min.x + column as f32 * cell;
            let frequency = context
                .coordinates
                .frequency_at_x(x + cell * 0.5, context.rect);
            let bin = (frequency / context.snapshot.fft.bin_hz).round() as usize;
            let Some(&dbfs) = context.snapshot.spectrum_dbfs.get(bin) else {
                continue;
            };
            let top = context.coordinates.y_for_dbfs(dbfs, context.rect);
            let mut y = context.rect.max.y - cell;
            while y + cell > top {
                let strength = ((context.rect.max.y - y) / context.rect.height()).clamp(0.0, 1.0);
                self.pixels.push(ColoredRect {
                    rect: Rect {
                        min: Point { x, y },
                        max: Point {
                            x: (x + cell - 1.0).min(context.rect.max.x),
                            y: (y + cell - 1.0).min(context.rect.max.y),
                        },
                    },
                    color: pixel_color(strength),
                });
                if y < context.rect.min.y + cell {
                    break;
                }
                y -= cell;
            }
        }
        renderer.fill_rects(context.rect, &self.pixels);
    }
}

fn pixel_color(strength: f32) -> Color {
    if strength < 0.55 {
        Color::rgb(45, 145, 215)
    } else if strength < 0.82 {
        Color::rgb(160, 90, 245)
    } else {
        Color::rgb(255, 70, 145)
    }
}

fn spectrum_points(
    snapshot: &SpectrumSnapshotSet,
    coordinates: SpectrumCoordinates,
    rect: Rect,
    output: &mut Vec<Point>,
    bins: &[f32],
) {
    output.clear();
    for index in coordinates.visible_bin_range(snapshot.fft.bin_hz, bins.len()) {
        output.push(coordinates.point(index as f32 * snapshot.fft.bin_hz, bins[index], rect));
    }
}

struct SpectrumModule {
    reader: SpectrumReader,
    style: Box<dyn SpectrumStyle>,
    style_id: &'static str,
    coordinates: SpectrumCoordinates,
    latest: Option<Arc<SpectrumSnapshotSet>>,
    label_version: u64,
    label_frequency_range: Option<(u32, u32)>,
    label: String,
    status_line: String,
    hover_line: String,
    grid: Vec<ColoredRect>,
}

impl SpectrumModule {
    fn update_label(&mut self, snapshot: &SpectrumSnapshotSet) {
        let frequency_range = (
            self.coordinates.min_frequency_hz.to_bits(),
            self.coordinates.max_frequency_hz.to_bits(),
        );
        if self.label_version == snapshot.result_version
            && self.label_frequency_range == Some(frequency_range)
        {
            return;
        }
        self.label_version = snapshot.result_version;
        self.label_frequency_range = Some(frequency_range);
        self.label.clear();
        let peak = self
            .coordinates
            .visible_bin_range(snapshot.fft.bin_hz, snapshot.spectrum_dbfs.len())
            .map(|index| (index, snapshot.spectrum_dbfs[index]))
            .max_by(|left, right| left.1.total_cmp(&right.1));
        match snapshot.channel {
            crate::dataflow::ChannelSelection::Discrete(index) => {
                let _ = write!(self.label, "Channel {}", index + 1);
            }
            selection => self.label.push_str(selection.label()),
        }
        if let Some((peak_bin, peak_dbfs)) = peak {
            let _ = write!(
                self.label,
                " | peak {:.1} Hz {:.1} dBFS | v{}",
                peak_bin as f32 * snapshot.fft.bin_hz,
                peak_dbfs,
                snapshot.result_version,
            );
        }
    }

    fn plot_rect(&self, rect: Rect) -> Rect {
        Rect {
            min: Point {
                x: rect.min.x + 48.0,
                y: rect.min.y + 30.0,
            },
            max: Point {
                x: rect.max.x - 10.0,
                y: rect.max.y - 24.0,
            },
        }
    }

    fn update_range(&mut self, frame: &ModuleFrame<'_>, plot: Rect, nyquist: f32) {
        if frame.pointer.is_some_and(|point| plot.contains(point)) {
            if (frame.zoom_delta - 1.0).abs() > 0.001 {
                self.coordinates.max_frequency_hz = (self.coordinates.max_frequency_hz
                    / frame.zoom_delta)
                    .clamp(self.coordinates.min_frequency_hz * 4.0, nyquist);
            }
            if frame.scroll_delta_y.abs() > 0.01 {
                let span = self.coordinates.max_dbfs - self.coordinates.min_dbfs;
                let next_span = (span * (-frame.scroll_delta_y * 0.002).exp()).clamp(24.0, 180.0);
                self.coordinates.min_dbfs = self.coordinates.max_dbfs - next_span;
            }
        }
    }

    fn draw_axes(&mut self, frame: &mut ModuleFrame<'_>, plot: Rect) {
        self.grid.clear();
        let grid_color = Color::rgba(90, 110, 125, 65);
        for &frequency in FREQUENCY_TICKS_HZ {
            if frequency < self.coordinates.min_frequency_hz
                || frequency > self.coordinates.max_frequency_hz
            {
                continue;
            }
            let x = self.coordinates.x_for_frequency(frequency, plot);
            self.grid.push(ColoredRect {
                rect: Rect {
                    min: Point { x, y: plot.min.y },
                    max: Point {
                        x: x + 1.0,
                        y: plot.max.y,
                    },
                },
                color: grid_color,
            });
            let label = if frequency >= 1_000.0 {
                format!("{:.0}k", frequency / 1_000.0)
            } else {
                format!("{frequency:.0}")
            };
            frame.renderer.text(
                frame.rect,
                Point {
                    x: x - 8.0,
                    y: plot.max.y + 5.0,
                },
                &label,
                TextStyle {
                    size: 9.0,
                    color: Color::rgb(125, 145, 158),
                },
            );
        }
        for dbfs in db_ticks(self.coordinates.min_dbfs, self.coordinates.max_dbfs, 20.0) {
            let y = self.coordinates.y_for_dbfs(dbfs, plot);
            self.grid.push(ColoredRect {
                rect: Rect {
                    min: Point { x: plot.min.x, y },
                    max: Point {
                        x: plot.max.x,
                        y: y + 1.0,
                    },
                },
                color: grid_color,
            });
            let label = format!("{dbfs:.0}");
            frame.renderer.text(
                frame.rect,
                Point {
                    x: frame.rect.min.x + 5.0,
                    y: y - 5.0,
                },
                &label,
                TextStyle {
                    size: 9.0,
                    color: Color::rgb(125, 145, 158),
                },
            );
        }
        frame.renderer.fill_rects(plot, &self.grid);
    }

    fn draw_hover(
        &mut self,
        frame: &mut ModuleFrame<'_>,
        plot: Rect,
        snapshot: &SpectrumSnapshotSet,
    ) {
        let Some(pointer) = frame.pointer.filter(|point| plot.contains(*point)) else {
            return;
        };
        let frequency = self.coordinates.frequency_at_x(pointer.x, plot);
        let axis_dbfs = self.coordinates.dbfs_at_y(pointer.y, plot);
        let bin = (frequency / snapshot.fft.bin_hz).round() as usize;
        let curve_dbfs = snapshot.spectrum_dbfs.get(bin).copied();
        self.hover_line.clear();
        let _ = write!(
            self.hover_line,
            "cursor {:.1} Hz | axis {:.1} dBFS",
            frequency, axis_dbfs
        );
        if let Some(curve_dbfs) = curve_dbfs {
            let _ = write!(self.hover_line, " | curve {:.1} dBFS", curve_dbfs);
        }
        frame.renderer.text(
            frame.rect,
            Point {
                x: (pointer.x + 10.0).min(frame.rect.max.x - 260.0),
                y: (pointer.y + 10.0).min(frame.rect.max.y - 18.0),
            },
            &self.hover_line,
            TextStyle {
                size: 11.0,
                color: Color::rgb(235, 240, 245),
            },
        );
    }
}

impl Module for SpectrumModule {
    fn draw(&mut self, mut frame: ModuleFrame<'_>) -> ModuleResponse {
        let requested_channel = self.reader.requested_channel();
        let requested_revision = self.reader.requested_revision();
        self.latest = self.reader.latest().filter(|snapshot| {
            snapshot.channel == requested_channel && snapshot.config_revision == requested_revision
        });
        if let Some(error) = self.reader.analysis_error() {
            let message = format!("Channel error: {error}");
            frame.renderer.text(
                frame.rect,
                Point {
                    x: frame.rect.min.x + 8.0,
                    y: frame.rect.min.y + 8.0,
                },
                &message,
                TextStyle {
                    size: 12.0,
                    color: Color::rgb(255, 110, 110),
                },
            );
            return ModuleResponse {
                repaint_after: Some(Duration::from_millis(100)),
            };
        }

        if let Some(snapshot) = self.latest.clone() {
            let nyquist = snapshot.descriptor.sample_rate_hz as f32 * 0.5;
            self.coordinates.max_frequency_hz = self.coordinates.max_frequency_hz.min(nyquist);
            let plot = self.plot_rect(frame.rect);
            self.update_range(&frame, plot, nyquist);
            self.update_label(&snapshot);
            self.draw_axes(&mut frame, plot);
            self.style.draw(
                SpectrumStyleContext {
                    snapshot: &snapshot,
                    coordinates: &self.coordinates,
                    rect: plot,
                    monotonic_time: frame.monotonic_time,
                },
                frame.renderer,
            );
            self.draw_hover(&mut frame, plot, &snapshot);
            let status = self.reader.source_status();
            let color = match status.state {
                SourceState::Running | SourceState::Starting => Color::rgb(190, 205, 215),
                SourceState::Paused => Color::rgb(255, 210, 100),
                SourceState::DeviceLost | SourceState::Stopped => Color::rgb(255, 110, 110),
            };
            self.status_line.clear();
            let _ = write!(
                self.status_line,
                "{} | {} Hz | {} ch | {}",
                source_state_label(status.state),
                snapshot.descriptor.sample_rate_hz,
                snapshot.descriptor.channel_count,
                self.label,
            );
            frame.renderer.text(
                frame.rect,
                Point {
                    x: frame.rect.min.x + 8.0,
                    y: frame.rect.min.y + 7.0,
                },
                &self.status_line,
                TextStyle { size: 11.0, color },
            );
        } else {
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

        ModuleResponse {
            repaint_after: Some(Duration::from_millis(33)),
        }
    }

    fn style_options(&self) -> &'static [StyleOption] {
        STYLE_OPTIONS
    }

    fn active_style(&self) -> Option<&'static str> {
        Some(self.style_id)
    }

    fn set_style(&mut self, style_id: &str, registry: &Registry) -> Result<(), RegistryError> {
        let option = STYLE_OPTIONS
            .iter()
            .find(|option| option.id == style_id)
            .ok_or_else(|| RegistryError::UnknownStyle(style_id.to_owned()))?;
        self.style = registry.create_spectrum_style(option.id)?;
        self.style_id = option.id;
        Ok(())
    }

    fn clear_visual_state(&mut self) {
        self.style.clear();
    }
}

fn source_state_label(state: SourceState) -> &'static str {
    match state {
        SourceState::Starting => "STARTING",
        SourceState::Running => "RUNNING",
        SourceState::Paused => "PAUSED",
        SourceState::DeviceLost => "DEVICE LOST",
        SourceState::Stopped => "STOPPED",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        audio::{
            AudioDescriptor, ChannelLayout, Generation, SourceId, SourceTelemetry,
            create_block_transport,
        },
        dataflow::{
            ChannelSelection, FftConfig, FftMetadata, SpectrumDataflow, SpectrumRequest,
            WindowFunction,
        },
        module::ModuleServices,
        render::Circle,
    };

    #[derive(Default)]
    struct RecordingTarget {
        polylines: usize,
        rectangles: usize,
    }

    impl RenderTarget for RecordingTarget {
        fn polyline(&mut self, _clip: Rect, _points: &[Point], _stroke: Stroke) {
            self.polylines += 1;
        }

        fn fill_rects(&mut self, _clip: Rect, rects: &[ColoredRect]) {
            self.rectangles += rects.len();
        }

        fn circles(&mut self, _clip: Rect, _circles: &[Circle]) {}

        fn text(&mut self, _clip: Rect, _position: Point, _text: &str, _style: TextStyle) {}
    }

    fn snapshot() -> SpectrumSnapshotSet {
        SpectrumSnapshotSet {
            descriptor: Arc::new(
                AudioDescriptor::new(SourceId(1), Generation(1), 48_000, 2, ChannelLayout::Stereo)
                    .unwrap(),
            ),
            channel: ChannelSelection::MonoAverage,
            config_revision: 1,
            result_version: 1,
            processed_through_frame: 2048,
            source_sequence: 1,
            discontinuity_count: 0,
            fft: FftMetadata {
                window: WindowFunction::PeriodicHann,
                window_size: 2048,
                hop_size: 512,
                bin_hz: 48_000.0 / 2048.0,
                floor_dbfs: -160.0,
            },
            spectrum_dbfs: (0..=1024)
                .map(|index| -100.0 + index as f32 * 0.08)
                .collect::<Vec<_>>()
                .into(),
            peak_dbfs: None,
            history: Arc::from([]),
        }
    }

    #[test]
    fn pixel_style_generates_cells_instead_of_recoloring_the_curve() {
        let snapshot = snapshot();
        let coordinates = SpectrumCoordinates {
            min_frequency_hz: 20.0,
            max_frequency_hz: 20_000.0,
            min_dbfs: -100.0,
            max_dbfs: 0.0,
        };
        let rect = Rect {
            min: Point { x: 0.0, y: 0.0 },
            max: Point { x: 800.0, y: 400.0 },
        };
        let context = SpectrumStyleContext {
            snapshot: &snapshot,
            coordinates: &coordinates,
            rect,
            monotonic_time: Duration::ZERO,
        };
        let mut target = RecordingTarget::default();
        let mut pixel = PixelSpectrumStyleFactory.create();
        pixel.draw(context, &mut target);
        assert_eq!(target.polylines, 0);
        assert!(target.rectangles > 100);
    }

    #[test]
    fn peak_label_recomputes_when_frequency_range_changes_without_new_audio() {
        let descriptor = Arc::new(
            AudioDescriptor::new(SourceId(1), Generation(1), 48_000, 2, ChannelLayout::Stereo)
                .unwrap(),
        );
        let telemetry = Arc::new(SourceTelemetry::new());
        let (_capture, processing) =
            create_block_transport(2, 4, 64, Arc::clone(&telemetry)).unwrap();
        let dataflow = SpectrumDataflow::spawn(Arc::clone(&descriptor), processing, telemetry);
        let subscription = dataflow
            .subscribe(SpectrumRequest::new(
                ChannelSelection::MonoAverage,
                FftConfig::new(32, 16, -160.0).unwrap(),
            ))
            .unwrap();
        let mut module = SpectrumModule {
            reader: subscription.reader(),
            style: LineSpectrumStyleFactory.create(),
            style_id: DEFAULT_SPECTRUM_STYLE_ID,
            coordinates: SpectrumCoordinates {
                min_frequency_hz: 20.0,
                max_frequency_hz: 20_000.0,
                min_dbfs: -100.0,
                max_dbfs: 0.0,
            },
            latest: None,
            label_version: 0,
            label_frequency_range: None,
            label: String::new(),
            status_line: String::new(),
            hover_line: String::new(),
            grid: Vec::new(),
        };
        let snapshot = snapshot();

        module.update_label(&snapshot);
        let full_range_label = module.label.clone();
        module.coordinates.max_frequency_hz = 5_000.0;
        module.update_label(&snapshot);

        assert_ne!(module.label, full_range_label);
    }

    #[test]
    fn switching_spectrum_style_does_not_rebuild_audio_analysis() {
        let descriptor = Arc::new(
            AudioDescriptor::new(SourceId(1), Generation(1), 48_000, 2, ChannelLayout::Stereo)
                .unwrap(),
        );
        let telemetry = Arc::new(SourceTelemetry::new());
        let (_capture, processing) =
            create_block_transport(2, 4, 64, Arc::clone(&telemetry)).unwrap();
        let dataflow = SpectrumDataflow::spawn(Arc::clone(&descriptor), processing, telemetry);
        let subscription = dataflow
            .subscribe(SpectrumRequest::new(
                ChannelSelection::MonoAverage,
                FftConfig::new(32, 16, -160.0).unwrap(),
            ))
            .unwrap();
        let services = ModuleServices {
            spectrum: subscription.reader(),
            time_domain: dataflow.time_domain_reader(),
        };
        let mut registry = Registry::default();
        register(&mut registry).unwrap();
        let mut module = registry
            .create_module(SPECTRUM_MODULE_ID, &services)
            .unwrap();
        let before = dataflow.metrics();

        module
            .set_style(PIXEL_SPECTRUM_STYLE_ID, &registry)
            .unwrap();

        assert_eq!(dataflow.metrics(), before);
    }
}
