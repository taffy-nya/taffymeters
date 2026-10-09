use std::{fmt::Write, sync::Arc, time::Duration};

use crate::{
    audio::{SourceReader, SourceState},
    dataflow::{ChannelSelection, FftConfig, PeakConfig, SpectrumRequest, SpectrumSubscription},
    extensions::{Registry, RegistryError},
    module::{
        Module, ModuleError, ModuleFactory, ModuleFrame, ModuleResponse, ModuleServices,
        StyleOption,
    },
    plotting::{FREQUENCY_TICKS_HZ, SpectrumCoordinates, db_ticks},
    render::{Color, ColoredRect, Point, Rect, Size, TextStyle},
    snapshot::{SpectrumReader, SpectrumSnapshotSet},
};

use super::spectrum_style::{
    self, BAR_SPECTRUM_STYLE_ID, DEFAULT_SPECTRUM_STYLE_ID, PIXEL_SPECTRUM_STYLE_ID, SpectrumStyle,
    SpectrumStyleContext,
};

pub const SPECTRUM_MODULE_ID: &str = "taffymeters.spectrum";

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
    spectrum_style::register(registry)?;
    registry.register_module(SpectrumModuleFactory)
}

struct SpectrumModuleFactory;

impl SpectrumModuleFactory {
    fn create_instance(
        &self,
        services: &ModuleServices,
        registry: &Registry,
    ) -> Result<SpectrumModule, RegistryError> {
        let mut request = SpectrumRequest::new(ChannelSelection::MonoAverage, FftConfig::default());
        request.peak = Some(PeakConfig {
            hold_ms: 1_000.0,
            decay_db_per_second: 18.0,
        });
        let subscription = services
            .dataflow
            .subscribe_spectrum(request)
            .map_err(ModuleError::from)?;
        let reader = subscription.reader();
        Ok(SpectrumModule {
            source: services.source.clone(),
            subscription,
            reader,
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
        })
    }
}

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
        Ok(Box::new(self.create_instance(services, registry)?))
    }
}

struct SpectrumModule {
    source: SourceReader,
    subscription: SpectrumSubscription,
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
    fn min_size(&self) -> Size {
        // Room for the frequency and dB tick labels.
        Size {
            width: 160.0,
            height: 100.0,
        }
    }

    fn channel(&self) -> Option<ChannelSelection> {
        Some(self.reader.requested_channel())
    }

    fn set_channel(&mut self, channel: ChannelSelection) -> Result<(), ModuleError> {
        channel.resolve(self.source.descriptor())?;
        self.subscription.controller().set_channel(channel)?;
        Ok(())
    }

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
            let status = self.source.source_status();
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
            AudioDescriptor, CaptureEndpoint, ChannelLayout, Generation, SourceId, SourceReader,
            SourceTelemetry, create_block_transport,
        },
        dataflow::{Dataflow, DataflowHandle, FftMetadata, WindowFunction},
        extensions::{Registry, RegistryError},
        render::{Circle, RenderTarget, Stroke},
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

    struct RuntimeFixture {
        capture: CaptureEndpoint,
        _dataflow: Dataflow,
        services: ModuleServices,
    }

    fn runtime_fixture() -> RuntimeFixture {
        let descriptor = Arc::new(
            AudioDescriptor::new(SourceId(1), Generation(1), 48_000, 2, ChannelLayout::Stereo)
                .unwrap(),
        );
        let telemetry = Arc::new(SourceTelemetry::new());
        let (mut capture, endpoint) =
            create_block_transport(2, 8, 4096, Arc::clone(&telemetry)).unwrap();
        let (dataflow, handle) =
            Dataflow::spawn(Arc::clone(&descriptor), endpoint, Arc::clone(&telemetry));
        capture.set_worker(dataflow.worker_thread());
        RuntimeFixture {
            capture,
            _dataflow: dataflow,
            services: ModuleServices {
                source: SourceReader::new(descriptor, telemetry),
                dataflow: handle,
            },
        }
    }

    fn wait_for_subscriptions(handle: &DataflowHandle, expected: u64) {
        for _ in 0..100_000 {
            if handle.metrics().subscriptions == expected {
                return;
            }
            std::thread::yield_now();
        }
        panic!(
            "dataflow did not reach {expected} active Spectrum subscriptions: {:?}",
            handle.metrics()
        );
    }

    fn spectrum_snapshot() -> SpectrumSnapshotSet {
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
        let snapshot = spectrum_snapshot();
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
        let mut registry = Registry::default();
        register(&mut registry).unwrap();
        let mut pixel = registry
            .create_spectrum_style(PIXEL_SPECTRUM_STYLE_ID)
            .unwrap();
        pixel.draw(context, &mut target);
        assert_eq!(target.polylines, 0);
        assert!(target.rectangles > 100);
    }

    #[test]
    fn peak_label_recomputes_when_frequency_range_changes_without_new_audio() {
        let fixture = runtime_fixture();
        let mut registry = Registry::default();
        register(&mut registry).unwrap();
        let mut module = SpectrumModuleFactory
            .create_instance(&fixture.services, &registry)
            .unwrap();
        let snapshot = spectrum_snapshot();

        module.update_label(&snapshot);
        let full_range_label = module.label.clone();
        module.coordinates.max_frequency_hz = 5_000.0;
        module.update_label(&snapshot);

        assert_ne!(module.label, full_range_label);
    }

    #[test]
    fn factory_instances_keep_private_state_and_release_only_their_own_analysis() {
        let mut fixture = runtime_fixture();
        let mut registry = Registry::default();
        register(&mut registry).unwrap();
        let factory = SpectrumModuleFactory;
        let first = factory
            .create_instance(&fixture.services, &registry)
            .unwrap();
        let mut second = factory
            .create_instance(&fixture.services, &registry)
            .unwrap();
        wait_for_subscriptions(&fixture.services.dataflow, 2);

        let shared_prefix = fixture.services.dataflow.metrics();
        assert_eq!(shared_prefix.fft_branches, 1);
        assert_eq!(shared_prefix.pipelines, 2);
        let request = second.subscription.controller().request().unwrap();
        assert_eq!(
            request.peak,
            Some(PeakConfig {
                hold_ms: 1_000.0,
                decay_db_per_second: 18.0,
            })
        );
        assert_eq!(request.history, None);

        second
            .set_channel(ChannelSelection::Right)
            .expect("stereo source supports Right");
        assert_eq!(first.channel(), Some(ChannelSelection::MonoAverage));
        assert_eq!(second.channel(), Some(ChannelSelection::Right));
        assert!(matches!(
            second.set_channel(ChannelSelection::Discrete(2)),
            Err(ModuleError::InvalidChannel(_))
        ));
        assert_eq!(second.channel(), Some(ChannelSelection::Right));

        let before_style_change = fixture.services.dataflow.metrics();
        second.set_style(BAR_SPECTRUM_STYLE_ID, &registry).unwrap();
        second.clear_visual_state();
        assert_eq!(fixture.services.dataflow.metrics(), before_style_change);

        let old_reader = first.reader.clone();
        let second_reader = second.reader.clone();
        let samples: Vec<f32> = (0..4096).flat_map(|_| [0.05_f32, -0.05]).collect();
        fixture.capture.write(&samples);
        let first_revision = old_reader.requested_revision();
        let second_revision = second_reader.requested_revision();
        let mut first_snapshot = None;
        let mut second_snapshot = None;
        for _ in 0..100_000 {
            first_snapshot = old_reader.latest().filter(|snapshot| {
                snapshot.channel == ChannelSelection::MonoAverage
                    && snapshot.config_revision == first_revision
                    && snapshot.processed_through_frame >= 4_096
            });
            second_snapshot = second_reader.latest().filter(|snapshot| {
                snapshot.channel == ChannelSelection::Right
                    && snapshot.config_revision == second_revision
                    && snapshot.processed_through_frame >= 4_096
            });
            if first_snapshot.is_some() && second_snapshot.is_some() {
                break;
            }
            std::thread::yield_now();
        }
        let first_version = first_snapshot
            .expect("first factory instance should receive its own analysis")
            .result_version;
        let second_version = second_snapshot
            .expect("second factory instance should receive its selected analysis")
            .result_version;

        drop(first);
        wait_for_subscriptions(&fixture.services.dataflow, 1);
        fixture.capture.write(&samples);
        let mut continued_snapshot = None;
        for _ in 0..100_000 {
            continued_snapshot = second_reader.latest().filter(|snapshot| {
                snapshot.channel == ChannelSelection::Right
                    && snapshot.config_revision == second_revision
                    && snapshot.result_version > second_version
            });
            if continued_snapshot.is_some() {
                break;
            }
            std::thread::yield_now();
        }
        assert!(
            continued_snapshot.is_some(),
            "remaining factory instance must continue receiving its selected analysis"
        );
        assert_eq!(fixture.services.dataflow.metrics().subscriptions, 1);

        drop(second);
        wait_for_subscriptions(&fixture.services.dataflow, 0);
        let retained_snapshot = old_reader.latest().unwrap();
        assert_eq!(retained_snapshot.result_version, first_version);
        let stopped = fixture.services.dataflow.metrics();
        assert_eq!(stopped.subscriptions, 0);
        assert_eq!(stopped.fft_branches, 0);
        assert_eq!(stopped.pipelines, 0);
        let _ = old_reader.latest();
        assert_eq!(fixture.services.dataflow.metrics().subscriptions, 0);
    }

    #[test]
    fn real_factory_instances_keep_fft_and_visual_state_when_moved() {
        use std::time::Instant;

        fn at_frontier(
            reader: &SpectrumReader,
            revision: u64,
            frontier: u64,
        ) -> Arc<SpectrumSnapshotSet> {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if let Some(snapshot) = reader.latest()
                    && snapshot.config_revision == revision
                    && snapshot.processed_through_frame >= frontier
                {
                    return snapshot;
                }
                assert!(
                    Instant::now() < deadline,
                    "factory instance did not reach {frontier}"
                );
                std::thread::yield_now();
            }
        }

        let mut fixture = runtime_fixture();
        let mut registry = Registry::default();
        register(&mut registry).unwrap();
        let factory = SpectrumModuleFactory;
        let first = Box::new(
            factory
                .create_instance(&fixture.services, &registry)
                .unwrap(),
        );
        let second = Box::new(
            factory
                .create_instance(&fixture.services, &registry)
                .unwrap(),
        );
        let first_reader = first.reader.clone();
        let second_reader = second.reader.clone();
        let mut source = vec![first, second];
        let samples: Vec<f32> = (0..4_096)
            .flat_map(|frame| {
                let time = frame as f32 / 48_000.0;
                [
                    0.2 * (2.0 * std::f32::consts::PI * 3_000.0 * time).sin(),
                    0.2 * (2.0 * std::f32::consts::PI * 6_000.0 * time).sin(),
                ]
            })
            .collect();
        fixture.capture.write(&samples);
        let first_before = at_frontier(&first_reader, 1, 4_096);
        at_frontier(&second_reader, 1, 4_096);
        let shared = fixture.services.dataflow.metrics();
        assert_eq!(shared.fft_evaluations, 5);
        assert_eq!(shared.pipeline_evaluations, 10);

        source[1].set_channel(ChannelSelection::Right).unwrap();
        source[1]
            .subscription
            .controller()
            .set_fft(FftConfig::new(1_024, 256, -160.0).unwrap())
            .unwrap();
        source[1]
            .set_style(PIXEL_SPECTRUM_STYLE_ID, &registry)
            .unwrap();
        let mut destination = vec![source.remove(1)];
        assert_eq!(fixture.services.dataflow.metrics().subscriptions, 2);
        assert_eq!(destination[0].active_style(), Some(PIXEL_SPECTRUM_STYLE_ID));
        assert_eq!(destination[0].channel(), Some(ChannelSelection::Right));

        fixture.capture.write(&samples);
        let first_after = at_frontier(&first_reader, 1, 8_192);
        let second_after = at_frontier(&second_reader, 3, 8_192);
        assert_eq!(first_after.fft.window_size, 2_048);
        assert_eq!(second_after.fft.window_size, 1_024);
        assert!(first_after.result_version > first_before.result_version);
        assert!(first_after.spectrum_dbfs[128] > first_after.spectrum_dbfs[100]);
        assert!(second_after.spectrum_dbfs[128] > second_after.spectrum_dbfs[100]);
        let evaluated = fixture.services.dataflow.metrics().fft_evaluations;
        let rect = Rect {
            min: Point { x: 0.0, y: 0.0 },
            max: Point { x: 800.0, y: 400.0 },
        };
        let mut target = RecordingTarget::default();
        for _ in 0..10 {
            let _ = first_reader.latest();
            let _ = second_reader.latest();
            for module in [&mut *source[0], &mut *destination[0]] {
                module.draw(ModuleFrame {
                    rect,
                    monotonic_time: Duration::from_secs(1),
                    pointer: None,
                    scroll_delta_y: 0.0,
                    zoom_delta: 1.0,
                    renderer: &mut target,
                });
            }
        }
        assert!(target.polylines > 0 && target.rectangles > 0);
        assert_eq!(
            fixture.services.dataflow.metrics().fft_evaluations,
            evaluated
        );
        drop(destination);
        wait_for_subscriptions(&fixture.services.dataflow, 1);
        drop(source);
        wait_for_subscriptions(&fixture.services.dataflow, 0);
    }

    #[test]
    fn factory_failure_after_subscription_releases_the_subscription() {
        let fixture = runtime_fixture();
        let registry = Registry::default();

        let error =
            match ModuleFactory::create(&SpectrumModuleFactory, &fixture.services, &registry) {
                Ok(_) => panic!("a Spectrum module requires its registered visual style"),
                Err(error) => error,
            };
        assert!(matches!(error, RegistryError::UnknownStyle(_)));
        wait_for_subscriptions(&fixture.services.dataflow, 0);
    }
}
