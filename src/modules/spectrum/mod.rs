use std::fmt::Write;

use crate::{
    audio::SourceReader,
    dataflow::{
        ChannelSelection, DataController, DataSpec, DataSubscription, FftConfig, Keep, NodeRef,
        NodeSpec, OutputId, PeakConfig, SOURCE,
    },
    extensions::{Registry, RegistryError},
    module::{
        Module, ModuleError, ModuleFactory, ModuleFrame, ModuleResponse, ModuleServices,
        StyleOption, number_setting, spec_from_saved,
    },
    plotting::SpectrumCoordinates,
    render::{Point, Rect, TextStyle},
    settings::{SettingKind, SettingValue, ViewSetting},
    snapshot::{DataReader, Snapshot},
    theme::ColorRole,
};

pub mod style;
mod styles;

use self::style::{SpectrumPlot, SpectrumStyle, SpectrumStyleContext};
pub use self::styles::{BAR_SPECTRUM_STYLE_ID, DEFAULT_SPECTRUM_STYLE_ID, PIXEL_SPECTRUM_STYLE_ID};

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
    styles::register(registry)?;
    registry.register_module(SpectrumModuleFactory)
}

/// The Spectrum's data: a shared raw FFT feeding the displayed curve and the peak
/// curve.
struct SpectrumData {
    spec: DataSpec,
    channel: NodeRef,
    main: OutputId,
    peak: OutputId,
}

fn spectrum_data() -> SpectrumData {
    let mut spec = DataSpec::new();
    let channel = spec.add(SOURCE, NodeSpec::Channel(ChannelSelection::MonoAverage));
    let fft = spec.add(channel, NodeSpec::Fft(FftConfig::default()));
    let curve = spec.chain(
        fft,
        [
            NodeSpec::Tilt { db_per_octave: 0.0 },
            NodeSpec::Center { db: 0.0 },
            NodeSpec::Gain { db: 0.0 },
        ],
    );
    let peak = spec.add(
        fft,
        NodeSpec::Peak(PeakConfig {
            hold_ms: 1_000.0,
            decay_db_per_second: 18.0,
        }),
    );
    let main = spec.output(curve, Keep::Latest);
    let peak = spec.output(peak, Keep::Latest);
    SpectrumData {
        spec,
        channel,
        main,
        peak,
    }
}

const COLOR_ROLES: &[ColorRole] = &[
    ColorRole::Primary,
    ColorRole::Secondary,
    ColorRole::Low,
    ColorRole::Mid,
    ColorRole::High,
    ColorRole::Text,
];

/// Highest frequency shown (the zoom gesture and the setting share these bounds;
/// the source's Nyquist frequency caps it when drawing), and the shown dB range
/// below 0 dBFS.
const MAX_FREQUENCY_RANGE: (f32, f32) = (80.0, 96_000.0);
const DB_RANGE: (f32, f32) = (24.0, 180.0);

struct SpectrumModuleFactory;

impl SpectrumModuleFactory {
    fn create_instance(
        &self,
        services: &ModuleServices,
        registry: &Registry,
        saved: Option<&DataSpec>,
    ) -> Result<SpectrumModule, RegistryError> {
        let data = spectrum_data();
        let spec = spec_from_saved(data.spec, saved)?;
        let subscription = services
            .dataflow
            .subscribe(spec)
            .map_err(ModuleError::from)?;
        Ok(SpectrumModule {
            source: services.source.clone(),
            controller: subscription.controller(),
            reader: subscription.reader(),
            _subscription: subscription,
            channel_node: data.channel,
            main: data.main,
            peak: data.peak,
            snapshot: Snapshot::default(),
            style: registry.create_spectrum_style(DEFAULT_SPECTRUM_STYLE_ID)?,
            style_id: DEFAULT_SPECTRUM_STYLE_ID,
            coordinates: SpectrumCoordinates {
                min_frequency_hz: 20.0,
                max_frequency_hz: 20_000.0,
                min_dbfs: -100.0,
                max_dbfs: 0.0,
            },
            hover_line: String::with_capacity(128),
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
        data: Option<&DataSpec>,
    ) -> Result<Box<dyn Module>, RegistryError> {
        Ok(Box::new(self.create_instance(services, registry, data)?))
    }
}

struct SpectrumModule {
    source: SourceReader,
    controller: DataController,
    reader: DataReader,
    _subscription: DataSubscription,
    channel_node: NodeRef,
    main: OutputId,
    peak: OutputId,
    snapshot: Snapshot,
    style: Box<dyn SpectrumStyle>,
    style_id: &'static str,
    coordinates: SpectrumCoordinates,
    hover_line: String,
}

impl SpectrumModule {
    fn update_range(
        coordinates: &mut SpectrumCoordinates,
        frame: &ModuleFrame<'_>,
        plot: Rect,
        nyquist: f32,
    ) {
        if frame.pointer.is_some_and(|point| plot.contains(point)) {
            if (frame.zoom_delta - 1.0).abs() > 0.001 {
                coordinates.max_frequency_hz = (coordinates.max_frequency_hz / frame.zoom_delta)
                    .clamp(MAX_FREQUENCY_RANGE.0, nyquist.max(MAX_FREQUENCY_RANGE.0));
            }
            if frame.scroll_delta_y.abs() > 0.01 {
                let span = coordinates.max_dbfs - coordinates.min_dbfs;
                let next_span =
                    (span * (-frame.scroll_delta_y * 0.002).exp()).clamp(DB_RANGE.0, DB_RANGE.1);
                coordinates.min_dbfs = coordinates.max_dbfs - next_span;
            }
        }
    }

    fn draw_hover(
        coordinates: &SpectrumCoordinates,
        hover_line: &mut String,
        frame: &mut ModuleFrame<'_>,
        plot: Rect,
        data: &SpectrumPlot<'_>,
    ) {
        let Some(pointer) = frame.pointer.filter(|point| plot.contains(*point)) else {
            return;
        };
        let frequency = coordinates.frequency_at_x(pointer.x, plot);
        let axis_dbfs = coordinates.dbfs_at_y(pointer.y, plot);
        let bin = (frequency / data.fft.bin_hz).round() as usize;
        let curve_dbfs = data.spectrum_dbfs.get(bin).copied();
        hover_line.clear();
        let _ = write!(
            hover_line,
            "cursor {:.1} Hz | axis {:.1} dBFS",
            frequency, axis_dbfs
        );
        if let Some(curve_dbfs) = curve_dbfs {
            let _ = write!(hover_line, " | curve {:.1} dBFS", curve_dbfs);
        }
        frame.renderer.text(
            frame.rect,
            Point {
                x: (pointer.x + 10.0).min(frame.rect.max.x - 260.0),
                y: (pointer.y + 10.0).min(frame.rect.max.y - 18.0),
            },
            hover_line,
            TextStyle {
                size: frame.palette.text_size,
                color: frame.palette.color(ColorRole::Text),
            },
        );
    }
}

impl Module for SpectrumModule {
    fn channel(&self) -> Option<ChannelSelection> {
        self.controller
            .with_spec(|spec| match spec.node(self.channel_node) {
                Some(NodeSpec::Channel(channel)) => Some(channel),
                _ => None,
            })
    }

    fn set_channel(&mut self, channel: ChannelSelection) -> Result<(), ModuleError> {
        channel.resolve(self.source.descriptor())?;
        let node = self.channel_node;
        self.controller
            .update(|spec| spec.set_node(node, NodeSpec::Channel(channel)))?;
        Ok(())
    }

    fn set_visible(&mut self, visible: bool) {
        let _ = self.controller.set_visible(visible);
    }

    fn data(&self) -> Option<&DataController> {
        Some(&self.controller)
    }

    fn output_label(&self, output: OutputId) -> Option<&'static str> {
        if output == self.main {
            Some("Curve")
        } else if output == self.peak {
            Some("Peak curve")
        } else {
            None
        }
    }

    fn color_roles(&self) -> &'static [ColorRole] {
        COLOR_ROLES
    }

    fn view_settings(&self) -> Vec<ViewSetting> {
        vec![
            ViewSetting {
                key: "max_frequency_hz",
                label: "Max frequency",
                section: "Display",
                kind: SettingKind::Number {
                    min: MAX_FREQUENCY_RANGE.0,
                    max: MAX_FREQUENCY_RANGE.1,
                    unit: "Hz",
                    logarithmic: true,
                },
                value: SettingValue::Number(self.coordinates.max_frequency_hz),
            },
            ViewSetting {
                key: "range_db",
                label: "Range",
                section: "Display",
                kind: SettingKind::Number {
                    min: DB_RANGE.0,
                    max: DB_RANGE.1,
                    unit: "dB",
                    logarithmic: false,
                },
                value: SettingValue::Number(self.coordinates.max_dbfs - self.coordinates.min_dbfs),
            },
        ]
    }

    fn set_view_setting(&mut self, key: &str, value: &SettingValue) -> Result<(), ModuleError> {
        match key {
            "max_frequency_hz" => {
                self.coordinates.max_frequency_hz =
                    number_setting(key, value, MAX_FREQUENCY_RANGE)?;
            }
            "range_db" => {
                self.coordinates.min_dbfs =
                    self.coordinates.max_dbfs - number_setting(key, value, DB_RANGE)?;
            }
            _ => return Err(ModuleError::UnknownSetting(key.to_owned())),
        }
        Ok(())
    }

    fn draw(&mut self, mut frame: ModuleFrame<'_>) -> ModuleResponse {
        self.reader.read(&mut self.snapshot);
        let Some(main) = self.snapshot.get(self.main) else {
            // No audio yet.
            return ModuleResponse::default();
        };
        let Some(spectrum) = main.spectrum() else {
            return ModuleResponse::default();
        };
        // The peak curve is drawn only when it belongs to the same analysis hop.
        let peak = self
            .snapshot
            .get(self.peak)
            .filter(|peak| peak.processed_through_frame == main.processed_through_frame)
            .and_then(|peak| peak.spectrum());
        let data = SpectrumPlot {
            fft: spectrum.fft,
            spectrum_dbfs: &spectrum.bins_dbfs,
            peak_dbfs: peak.map(|peak| peak.bins_dbfs.as_slice()),
        };
        let nyquist = self.reader.descriptor().sample_rate_hz as f32 * 0.5;
        self.coordinates.max_frequency_hz = self.coordinates.max_frequency_hz.min(nyquist);
        let plot = frame.rect;
        Self::update_range(&mut self.coordinates, &frame, plot, nyquist);
        self.style.draw(
            SpectrumStyleContext {
                snapshot: &data,
                coordinates: &self.coordinates,
                rect: plot,
                monotonic_time: frame.monotonic_time,
                palette: frame.palette,
            },
            frame.renderer,
        );
        Self::draw_hover(
            &self.coordinates,
            &mut self.hover_line,
            &mut frame,
            plot,
            &data,
        );
        ModuleResponse::default()
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

#[cfg(test)]
mod tests {
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };

    use super::*;
    use crate::{
        audio::{
            AudioDescriptor, CaptureEndpoint, ChannelLayout, Generation, SourceId, SourceReader,
            SourceTelemetry, create_block_transport,
        },
        dataflow::{Dataflow, DataflowHandle, FftMetadata, WindowFunction},
        modules::test_support::RecordingTarget,
        render::ColoredRect,
        snapshot::Frame,
    };

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
        let deadline = Instant::now() + Duration::from_secs(2);
        while handle.metrics().subscriptions != expected {
            assert!(
                Instant::now() < deadline,
                "dataflow did not reach {expected} subscriptions: {:?}",
                handle.metrics()
            );
            std::thread::yield_now();
        }
    }

    /// The module's main curve once it reached `frontier` under `revision`.
    fn main_at(module: &SpectrumModule, revision: u64, frontier: u64) -> Arc<Frame> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut snapshot = Snapshot::default();
        loop {
            module.reader.read(&mut snapshot);
            if snapshot.config_revision() == revision
                && let Some(frame) = snapshot.get(module.main)
                && frame.processed_through_frame >= frontier
            {
                return Arc::clone(frame);
            }
            assert!(Instant::now() < deadline, "no result at {frontier}");
            std::thread::yield_now();
        }
    }

    fn fft_node(module: &SpectrumModule) -> NodeRef {
        module.controller.with_spec(|spec| {
            spec.nodes()
                .find(|(_, _, node)| matches!(node, NodeSpec::Fft(_)))
                .expect("the Spectrum has an FFT")
                .0
        })
    }

    #[test]
    fn pixel_style_generates_cells_instead_of_recoloring_the_curve() {
        // Quiet below bin 100 (2.34 kHz), a flat -50 dBFS from there up.
        let bins: Vec<f32> = (0..=1024)
            .map(|index| if index < 100 { -100.0 } else { -50.0 })
            .collect();
        let plot = SpectrumPlot {
            fft: FftMetadata {
                window: WindowFunction::PeriodicHann,
                window_size: 2048,
                hop_size: 512,
                bin_hz: 48_000.0 / 2048.0,
                floor_dbfs: -160.0,
            },
            spectrum_dbfs: &bins,
            peak_dbfs: None,
        };
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
            snapshot: &plot,
            coordinates: &coordinates,
            rect,
            monotonic_time: Duration::ZERO,
            palette: &crate::theme::Palette::default(),
        };
        let mut target = RecordingTarget::default();
        let mut registry = Registry::default();
        register(&mut registry).unwrap();
        let mut pixel = registry
            .create_spectrum_style(PIXEL_SPECTRUM_STYLE_ID)
            .unwrap();
        pixel.draw(context, &mut target);
        assert!(target.polylines.is_empty(), "cells replace the curve");
        assert!(!target.rects.is_empty(), "the loud region produces cells");

        let cell = target.rects[0].rect;
        let (cell_width, cell_height) = (cell.width(), cell.height());
        assert!(cell_width > 0.0 && cell_height > 0.0);
        for cell in &target.rects {
            let size = (cell.rect.width(), cell.rect.height());
            assert_eq!(size, (cell_width, cell_height), "cells are uniform");
            assert!(
                cell.rect.min.x >= rect.min.x
                    && cell.rect.max.x <= rect.max.x
                    && cell.rect.min.y >= rect.min.y
                    && cell.rect.max.y <= rect.max.y,
                "cell {:?} leaves the plot",
                cell.rect
            );
        }
        for (index, a) in target.rects.iter().enumerate() {
            for b in &target.rects[index + 1..] {
                let overlaps = a.rect.min.x < b.rect.max.x
                    && b.rect.min.x < a.rect.max.x
                    && a.rect.min.y < b.rect.max.y
                    && b.rect.min.y < a.rect.max.y;
                assert!(!overlaps, "cells {:?} and {:?} overlap", a.rect, b.rect);
            }
        }

        // Bins below 100 are quiet: no cell may be centered at a frequency that
        // rounds to one of them. Bin 99.5 is the boundary.
        let boundary_x = coordinates.x_for_frequency(99.5 * plot.fft.bin_hz, rect);
        for cell in &target.rects {
            let center_x = (cell.rect.min.x + cell.rect.max.x) * 0.5;
            assert!(
                center_x >= boundary_x,
                "cell at x {center_x} is in the quiet region (< {boundary_x})"
            );
        }

        // Loud columns start right at the boundary and run to the right edge.
        let mut columns: std::collections::BTreeMap<u32, Vec<ColoredRect>> = Default::default();
        for cell in &target.rects {
            columns
                .entry(cell.rect.min.x.to_bits())
                .or_default()
                .push(*cell);
        }
        let first_x = f32::from_bits(*columns.keys().next().unwrap());
        let last_right = columns
            .values()
            .map(|cells| cells[0].rect.max.x)
            .fold(f32::MIN, f32::max);
        assert!(
            first_x - boundary_x <= 2.0 * cell_width,
            "first loud column"
        );
        assert!(
            rect.max.x - last_right <= 2.0 * cell_width,
            "last loud column"
        );

        // Every loud column is a stack from the bottom up to the -50 dBFS level.
        let level_y = coordinates.y_for_dbfs(-50.0, rect);
        let expected_count = columns.values().next().unwrap().len();
        for (key, cells) in &columns {
            let x = f32::from_bits(*key);
            assert_eq!(cells.len(), expected_count, "column at x {x}: equal levels");
            let lowest = cells.iter().map(|c| c.rect.max.y).fold(f32::MIN, f32::max);
            let highest = cells.iter().map(|c| c.rect.min.y).fold(f32::MAX, f32::min);
            assert!(
                rect.max.y - lowest <= cell_height,
                "column at x {x} floats above the bottom"
            );
            assert!(
                (highest - level_y).abs() <= cell_height,
                "column at x {x} reaches y {highest}, level is {level_y}"
            );
        }
    }

    #[test]
    fn factory_instances_keep_private_state_and_release_only_their_own_analysis() {
        let mut fixture = runtime_fixture();
        let mut registry = Registry::default();
        register(&mut registry).unwrap();
        let factory = SpectrumModuleFactory;
        let first = factory
            .create_instance(&fixture.services, &registry, None)
            .unwrap();
        let mut second = factory
            .create_instance(&fixture.services, &registry, None)
            .unwrap();
        wait_for_subscriptions(&fixture.services.dataflow, 2);

        // One shared Channel/FFT/Tilt/Center/Gain chain, one private Peak each.
        let shared_prefix = fixture.services.dataflow.metrics();
        assert_eq!(shared_prefix.active_fft_nodes, 1);
        assert_eq!(shared_prefix.active_spectrum_nodes, 5);
        assert_eq!(
            second.controller.spec().node(fft_node(&second)),
            Some(NodeSpec::Fft(FftConfig::default()))
        );

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

        let samples: Vec<f32> = (0..4096).flat_map(|_| [0.05_f32, -0.05]).collect();
        fixture.capture.write(&samples);
        let first_result = main_at(&first, 1, 4_096);
        let second_result = main_at(&second, 2, 4_096);
        let old_reader = first.reader.clone();

        drop(first);
        wait_for_subscriptions(&fixture.services.dataflow, 1);
        fixture.capture.write(&samples);
        let continued = main_at(&second, 2, 8_192);
        assert!(continued.result_version > second_result.result_version);

        drop(second);
        wait_for_subscriptions(&fixture.services.dataflow, 0);
        let stopped = fixture.services.dataflow.metrics();
        assert_eq!((stopped.nodes, stopped.rings), (0, 0));
        // A result taken earlier stays readable; the closed reader sees nothing new.
        assert_eq!(first_result.processed_through_frame, 4_096);
        assert!(old_reader.latest(OutputId::from_index(0)).is_none());
    }

    #[test]
    fn real_factory_instances_keep_fft_and_visual_state_when_moved() {
        let mut fixture = runtime_fixture();
        let mut registry = Registry::default();
        register(&mut registry).unwrap();
        let factory = SpectrumModuleFactory;
        let first = Box::new(
            factory
                .create_instance(&fixture.services, &registry, None)
                .unwrap(),
        );
        let second = Box::new(
            factory
                .create_instance(&fixture.services, &registry, None)
                .unwrap(),
        );
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
        let first_before = main_at(&first, 1, 4_096);
        main_at(&second, 1, 4_096);
        let shared = fixture.services.dataflow.metrics();
        assert_eq!(shared.fft_evaluations, 5, "one shared FFT");
        assert_eq!(
            shared.spectrum_evaluations, 25,
            "shared Tilt/Center/Gain and two private Peaks per hop"
        );

        let mut source: Vec<Box<SpectrumModule>> = vec![first, second];
        source[1].set_channel(ChannelSelection::Right).unwrap();
        let fft_node = fft_node(&source[1]);
        source[1]
            .controller
            .update(|spec| {
                spec.set_node(
                    fft_node,
                    NodeSpec::Fft(FftConfig::new(1_024, 256, -160.0).unwrap()),
                )
            })
            .unwrap();
        source[1]
            .set_style(PIXEL_SPECTRUM_STYLE_ID, &registry)
            .unwrap();
        let mut destination = vec![source.remove(1)];
        assert_eq!(fixture.services.dataflow.metrics().subscriptions, 2);
        assert_eq!(destination[0].active_style(), Some(PIXEL_SPECTRUM_STYLE_ID));
        assert_eq!(destination[0].channel(), Some(ChannelSelection::Right));

        fixture.capture.write(&samples);
        let first_after = main_at(&source[0], 1, 8_192);
        let second_after = main_at(&destination[0], 3, 8_192);
        let bins = |frame: &Frame| frame.spectrum().unwrap().clone();
        assert_eq!(bins(&first_after).fft.window_size, 2_048);
        assert_eq!(bins(&second_after).fft.window_size, 1_024);
        assert!(first_after.result_version > first_before.result_version);
        assert!(bins(&first_after).bins_dbfs[128] > bins(&first_after).bins_dbfs[100]);
        assert!(bins(&second_after).bins_dbfs[128] > bins(&second_after).bins_dbfs[100]);
        let evaluated = fixture.services.dataflow.metrics().fft_evaluations;
        let rect = Rect {
            min: Point { x: 0.0, y: 0.0 },
            max: Point { x: 800.0, y: 400.0 },
        };
        let mut target = RecordingTarget::default();
        for _ in 0..10 {
            for module in [&mut *source[0], &mut *destination[0]] {
                module.draw(ModuleFrame {
                    rect,
                    monotonic_time: Duration::from_secs(1),
                    pointer: None,
                    scroll_delta_y: 0.0,
                    zoom_delta: 1.0,
                    palette: &crate::theme::Palette::default(),
                    renderer: &mut target,
                });
            }
        }
        assert!(!target.polylines.is_empty() && !target.rects.is_empty());
        assert_eq!(
            fixture.services.dataflow.metrics().fft_evaluations,
            evaluated,
            "drawing does not advance analysis"
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
            match ModuleFactory::create(&SpectrumModuleFactory, &fixture.services, &registry, None)
            {
                Ok(_) => panic!("a Spectrum module requires its registered visual style"),
                Err(error) => error,
            };
        assert!(matches!(error, RegistryError::UnknownStyle(_)));
        wait_for_subscriptions(&fixture.services.dataflow, 0);
    }
}
