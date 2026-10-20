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
        StyleOption, WHEEL_ZOOM_PER_POINT, flag_setting, number_setting, overlay_setting,
        spec_from_saved, styled_color_roles,
    },
    plotting::{SpectrumCoordinates, spectrum_dbfs_at},
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

/// The Spectrum's data: a shared raw FFT feeding the tilted, displayed curve, and
/// the peak curve held from that displayed curve (so Tilt moves both alike).
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
    let curve = spec.add(
        fft,
        NodeSpec::Tilt {
            db_per_octave: super::DEFAULT_TILT_DB_PER_OCTAVE,
        },
    );
    let peak = spec.add(
        curve,
        NodeSpec::Peak(PeakConfig {
            hold_ms: 1_000.0,
            fall_db_per_second_squared: 30.0,
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

/// The cursor readout; the curve, bars or cells are the style's.
const OWN_COLOR_ROLES: &[ColorRole] = &[ColorRole::Text];
/// Every role a Spectrum can draw with across its styles.
const TYPE_COLOR_ROLES: &[ColorRole] = &[
    ColorRole::Primary,
    ColorRole::Secondary,
    ColorRole::Low,
    ColorRole::Mid,
    ColorRole::High,
    ColorRole::Text,
];

/// Highest frequency shown (the zoom gesture and the setting share these bounds;
/// the source's Nyquist frequency caps it when drawing), the bottom edge relative
/// to the workspace reference level, and the shown dB range above it.
const MAX_FREQUENCY_RANGE: (f32, f32) = (80.0, 96_000.0);
const BOTTOM_DB_RANGE: (f32, f32) = (-150.0, -30.0);
const DB_RANGE: (f32, f32) = (30.0, 150.0);
/// What a new Spectrum shows: up to 20 kHz, the 90 dB below the reference. A sine
/// at the reference touches the top; tilted music, whose loudest bins sit about
/// 20 dB below its sample peak, tops out near the height the linear views reach.
const DEFAULT_MAX_FREQUENCY_HZ: f32 = 20_000.0;
const DEFAULT_RANGE_DB: f32 = 90.0;
const DEFAULT_BOTTOM_DB: f32 = -DEFAULT_RANGE_DB;

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
        let style = registry.create_spectrum_style(DEFAULT_SPECTRUM_STYLE_ID)?;
        Ok(SpectrumModule {
            source: services.source.clone(),
            controller: subscription.controller(),
            reader: subscription.reader(),
            _subscription: subscription,
            channel_node: data.channel,
            main: data.main,
            peak: data.peak,
            snapshot: Snapshot::default(),
            color_roles: styled_color_roles(style.color_roles(), OWN_COLOR_ROLES),
            style,
            style_id: DEFAULT_SPECTRUM_STYLE_ID,
            view: SpectrumCoordinates {
                min_frequency_hz: 20.0,
                max_frequency_hz: DEFAULT_MAX_FREQUENCY_HZ,
                min_dbfs: DEFAULT_BOTTOM_DB,
                max_dbfs: DEFAULT_BOTTOM_DB + DEFAULT_RANGE_DB,
            },
            show_peak: false,
            show_readout: false,
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

    fn default_data(&self) -> Option<DataSpec> {
        Some(spectrum_data().spec)
    }

    fn color_roles(&self) -> &'static [ColorRole] {
        TYPE_COLOR_ROLES
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
    /// The style's roles and the readout's.
    color_roles: Vec<ColorRole>,
    /// Shown frequencies, and levels relative to the workspace reference level.
    view: SpectrumCoordinates,
    show_peak: bool,
    show_readout: bool,
    hover_line: String,
}

impl SpectrumModule {
    /// The wheel scales the shown dB range about the bottom edge: the curve grows
    /// or shrinks upward from it, louder parts leaving through the top. Pinch or
    /// Ctrl+wheel zooms the frequency axis.
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
                let next_span = (span * (-frame.scroll_delta_y * WHEEL_ZOOM_PER_POINT).exp())
                    .clamp(DB_RANGE.0, DB_RANGE.1);
                coordinates.max_dbfs = coordinates.min_dbfs + next_span;
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
        let curve_dbfs = spectrum_dbfs_at(data.spectrum_dbfs, data.fft.bin_hz, frequency);
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

    fn color_roles(&self) -> &[ColorRole] {
        &self.color_roles
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
                value: SettingValue::Number(self.view.max_frequency_hz),
                default: SettingValue::Number(DEFAULT_MAX_FREQUENCY_HZ),
            },
            ViewSetting {
                key: "bottom_db",
                label: "Bottom",
                section: "Display",
                kind: SettingKind::Number {
                    min: BOTTOM_DB_RANGE.0,
                    max: BOTTOM_DB_RANGE.1,
                    unit: "dB",
                    logarithmic: false,
                },
                value: SettingValue::Number(self.view.min_dbfs),
                default: SettingValue::Number(DEFAULT_BOTTOM_DB),
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
                value: SettingValue::Number(self.view.max_dbfs - self.view.min_dbfs),
                default: SettingValue::Number(DEFAULT_RANGE_DB),
            },
            overlay_setting("show_peak", "Peak curve", self.show_peak),
            overlay_setting("show_readout", "Cursor readout", self.show_readout),
        ]
    }

    fn set_view_setting(&mut self, key: &str, value: &SettingValue) -> Result<(), ModuleError> {
        let range = self.view.max_dbfs - self.view.min_dbfs;
        match key {
            "max_frequency_hz" => {
                self.view.max_frequency_hz = number_setting(key, value, MAX_FREQUENCY_RANGE)?;
            }
            // Moving the bottom keeps the range: the whole view shifts.
            "bottom_db" => {
                self.view.min_dbfs = number_setting(key, value, BOTTOM_DB_RANGE)?;
                self.view.max_dbfs = self.view.min_dbfs + range;
            }
            "range_db" => {
                self.view.max_dbfs = self.view.min_dbfs + number_setting(key, value, DB_RANGE)?;
            }
            "show_peak" => self.show_peak = flag_setting(key, value)?,
            "show_readout" => self.show_readout = flag_setting(key, value)?,
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
        // One read sees the outputs of a block together, so main and peak share a
        // hop except right after a change, when either may still be the previous one.
        // A hidden peak curve keeps running so it is current when shown again.
        let peak = self
            .snapshot
            .get(self.peak)
            .and_then(|peak| peak.spectrum())
            .filter(|_| self.show_peak);
        let data = SpectrumPlot {
            fft: spectrum.fft,
            spectrum_dbfs: &spectrum.bins_dbfs,
            peak_dbfs: peak.map(|peak| peak.bins_dbfs.as_slice()),
        };
        let nyquist = self.reader.descriptor().sample_rate_hz as f32 * 0.5;
        self.view.max_frequency_hz = self.view.max_frequency_hz.min(nyquist);
        let plot = frame.rect;
        Self::update_range(&mut self.view, &frame, plot, nyquist);
        let coordinates = SpectrumCoordinates {
            min_dbfs: frame.reference_dbfs + self.view.min_dbfs,
            max_dbfs: frame.reference_dbfs + self.view.max_dbfs,
            ..self.view
        };
        self.style.draw(
            SpectrumStyleContext {
                snapshot: &data,
                coordinates: &coordinates,
                rect: plot,
                monotonic_time: frame.monotonic_time,
                palette: frame.palette,
            },
            frame.renderer,
        );
        if self.show_readout {
            Self::draw_hover(&coordinates, &mut self.hover_line, &mut frame, plot, &data);
        }
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
        self.color_roles = styled_color_roles(self.style.color_roles(), OWN_COLOR_ROLES);
        Ok(())
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
        loop {
            if module.reader.config_revision() == Some(revision)
                && let Some(frame) = module.reader.latest(module.main)
                && frame.processed_through_frame >= frontier
            {
                return frame;
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

        // Bins below 100 are quiet: a column (cells sit on a pitch one point wider
        // than the cell) is loud only when it reaches bin 100, its loudest bin.
        let boundary_x = coordinates.x_for_frequency(100.0 * plot.fft.bin_hz, rect);
        let pitch = cell_width + 1.0;
        for cell in &target.rects {
            assert!(
                cell.rect.min.x + pitch >= boundary_x,
                "column at x {} lies in the quiet region (< {boundary_x})",
                cell.rect.min.x
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

    /// Rectangles `style` draws for `plot` in an 800 × 400 plot spanning −100…0 dBFS.
    fn style_rects(style: &mut dyn SpectrumStyle, plot: &SpectrumPlot<'_>) -> Vec<ColoredRect> {
        let mut target = RecordingTarget::default();
        style.draw(
            SpectrumStyleContext {
                snapshot: plot,
                coordinates: &PEAK_TEST_COORDINATES,
                rect: PEAK_TEST_RECT,
                monotonic_time: Duration::ZERO,
                palette: &crate::theme::Palette::default(),
            },
            &mut target,
        );
        target.rects
    }

    const PEAK_TEST_COORDINATES: SpectrumCoordinates = SpectrumCoordinates {
        min_frequency_hz: 20.0,
        max_frequency_hz: 20_000.0,
        min_dbfs: -100.0,
        max_dbfs: 0.0,
    };
    const PEAK_TEST_RECT: Rect = Rect {
        min: Point { x: 0.0, y: 0.0 },
        max: Point { x: 800.0, y: 400.0 },
    };

    #[test]
    fn bars_and_pixel_mark_the_peak_of_each_band_above_its_level() {
        let level = vec![-50.0_f32; 1025];
        let held = vec![-20.0_f32; 1025];
        let fft = FftMetadata {
            window: WindowFunction::PeriodicHann,
            window_size: 2048,
            hop_size: 512,
            bin_hz: 48_000.0 / 2048.0,
            floor_dbfs: -160.0,
        };
        let plot = |peak_dbfs| SpectrumPlot {
            fft,
            spectrum_dbfs: &level,
            peak_dbfs,
        };
        let secondary = crate::theme::Palette::default().color(ColorRole::Secondary);
        let level_y = PEAK_TEST_COORDINATES.y_for_dbfs(-50.0, PEAK_TEST_RECT);
        let peak_y = PEAK_TEST_COORDINATES.y_for_dbfs(-20.0, PEAK_TEST_RECT);
        let mut registry = Registry::default();
        register(&mut registry).unwrap();
        for style_id in [BAR_SPECTRUM_STYLE_ID, PIXEL_SPECTRUM_STYLE_ID] {
            let mut style = registry.create_spectrum_style(style_id).unwrap();
            let without = style_rects(style.as_mut(), &plot(None));
            assert!(
                without.iter().all(|rect| rect.color != secondary),
                "{style_id}: a mark without a peak"
            );
            let bands: std::collections::BTreeSet<u32> = without
                .iter()
                .map(|rect| rect.rect.min.x.to_bits())
                .collect();

            let with = style_rects(style.as_mut(), &plot(Some(&held)));
            let marks: Vec<&ColoredRect> =
                with.iter().filter(|rect| rect.color == secondary).collect();
            assert_eq!(marks.len(), bands.len(), "{style_id}: one mark per band");
            assert_eq!(
                with.len() - marks.len(),
                without.len(),
                "{style_id}: the level is drawn as without a peak"
            );
            for mark in marks {
                assert!(bands.contains(&mark.rect.min.x.to_bits()), "{style_id}");
                // Bars put the mark's top on the peak; Pixel lights the cell holding it.
                assert!(
                    mark.rect.min.y <= peak_y && peak_y - mark.rect.min.y < 7.0,
                    "{style_id}: mark at y {} for a peak at {peak_y}",
                    mark.rect.min.y
                );
                assert!(
                    mark.rect.max.y < level_y,
                    "{style_id}: mark above the level"
                );
            }
        }
    }

    fn frame_at<'a>(
        target: &'a mut RecordingTarget,
        palette: &'a crate::theme::Palette,
        pointer: Option<Point>,
        scroll_delta_y: f32,
    ) -> ModuleFrame<'a> {
        ModuleFrame {
            rect: Rect {
                min: Point { x: 0.0, y: 0.0 },
                max: Point { x: 800.0, y: 400.0 },
            },
            monotonic_time: Duration::from_secs(1),
            pointer,
            scroll_delta_y,
            zoom_delta: 1.0,
            palette,
            reference_dbfs: crate::module::DEFAULT_REFERENCE_DBFS,
            renderer: target,
        }
    }

    #[test]
    fn curve_reaches_both_edges_even_when_the_first_bin_lies_inside_the_range() {
        // 256-point FFT at 48 kHz: bins every 187.5 Hz, the first above 20 Hz is far
        // into the plot. Levels rise linearly with frequency.
        let bin_hz = 187.5;
        let bins: Vec<f32> = (0..=128).map(|index| -100.0 + index as f32 * 0.5).collect();
        let plot = SpectrumPlot {
            fft: FftMetadata {
                window: WindowFunction::PeriodicHann,
                window_size: 256,
                hop_size: 64,
                bin_hz,
                floor_dbfs: -160.0,
            },
            spectrum_dbfs: &bins,
            peak_dbfs: None,
        };
        let coordinates = SpectrumCoordinates {
            min_frequency_hz: 20.0,
            max_frequency_hz: 20_000.0,
            min_dbfs: -120.0,
            max_dbfs: 0.0,
        };
        let rect = Rect {
            min: Point { x: 10.0, y: 0.0 },
            max: Point { x: 810.0, y: 400.0 },
        };
        let mut registry = Registry::default();
        register(&mut registry).unwrap();
        let mut curve = registry
            .create_spectrum_style(DEFAULT_SPECTRUM_STYLE_ID)
            .unwrap();
        let mut target = RecordingTarget::default();
        curve.draw(
            SpectrumStyleContext {
                snapshot: &plot,
                coordinates: &coordinates,
                rect,
                monotonic_time: Duration::ZERO,
                palette: &crate::theme::Palette::default(),
            },
            &mut target,
        );
        let points = &target.polylines[0].points;
        let (first, last) = (points[0], *points.last().unwrap());
        assert_eq!(first.x, rect.min.x, "the curve starts at the left edge");
        assert_eq!(last.x, rect.max.x, "the curve ends at the right edge");
        // The edge levels are interpolated between the neighboring bins.
        let level = |frequency: f32| -100.0 + frequency / bin_hz * 0.5;
        assert!((first.y - coordinates.y_for_dbfs(level(20.0), rect)).abs() < 0.01);
        assert!((last.y - coordinates.y_for_dbfs(level(20_000.0), rect)).abs() < 0.01);
    }

    #[test]
    fn wheel_scales_the_level_axis_about_the_bottom_edge() {
        let mut coordinates = SpectrumCoordinates {
            min_frequency_hz: 20.0,
            max_frequency_hz: 20_000.0,
            min_dbfs: DEFAULT_BOTTOM_DB,
            max_dbfs: DEFAULT_BOTTOM_DB + DEFAULT_RANGE_DB,
        };
        let palette = crate::theme::Palette::default();
        let mut target = RecordingTarget::default();
        let inside = Some(Point { x: 400.0, y: 200.0 });
        let frame = frame_at(&mut target, &palette, inside, 200.0);
        let plot = frame.rect;
        let before = coordinates.y_for_dbfs(-30.0, plot);
        SpectrumModule::update_range(&mut coordinates, &frame, plot, 24_000.0);
        assert_eq!(coordinates.min_dbfs, DEFAULT_BOTTOM_DB, "the bottom stays");
        assert_eq!(coordinates.y_for_dbfs(DEFAULT_BOTTOM_DB, plot), plot.max.y);
        let range = coordinates.max_dbfs - coordinates.min_dbfs;
        assert!((range - DEFAULT_RANGE_DB * (-0.2_f32).exp()).abs() < 0.01);
        let after = coordinates.y_for_dbfs(-30.0, plot);
        assert!(after < before, "scrolling up makes the curve taller");
        // Linear about the bottom: heights above it scale by one factor.
        let height = |y: f32| plot.max.y - y;
        assert!((height(after) / height(before) - DEFAULT_RANGE_DB / range).abs() < 0.001);

        // Away from the plot the wheel does nothing.
        let frame = frame_at(&mut target, &palette, None, 200.0);
        let unchanged = coordinates;
        SpectrumModule::update_range(&mut coordinates, &frame, plot, 24_000.0);
        assert_eq!(coordinates, unchanged);
    }

    #[test]
    fn peak_curve_follows_tilt_and_overlays_are_hidden_until_enabled() {
        let mut fixture = runtime_fixture();
        let mut registry = Registry::default();
        register(&mut registry).unwrap();
        let mut module = SpectrumModuleFactory
            .create_instance(&fixture.services, &registry, None)
            .unwrap();
        let tilt = module.controller.with_spec(|spec| {
            spec.nodes()
                .find(|(_, _, node)| matches!(node, NodeSpec::Tilt { .. }))
                .expect("the Spectrum has a Tilt")
                .0
        });
        module
            .controller
            .update(|spec| spec.set_node(tilt, NodeSpec::Tilt { db_per_octave: 9.0 }))
            .unwrap();
        let samples: Vec<f32> = (0..8_192)
            .flat_map(|frame| {
                let sample =
                    0.2 * (2.0 * std::f32::consts::PI * 3_000.0 * frame as f32 / 48_000.0).sin();
                [sample, sample]
            })
            .collect();
        fixture.capture.write(&samples);
        let main = main_at(&module, 2, 8_192);
        let deadline = Instant::now() + Duration::from_secs(2);
        let peak = loop {
            if let Some(frame) = module.reader.latest(module.peak)
                && frame.processed_through_frame >= 8_192
            {
                break frame;
            }
            assert!(Instant::now() < deadline, "no peak result");
            std::thread::yield_now();
        };
        // 3 kHz is bin 128 of the 2048-point FFT. A steady tone holds the peak at
        // the displayed (tilted, +14 dB there) level, not at the raw level.
        let main_db = main.spectrum().unwrap().bins_dbfs[128];
        let peak_db = peak.spectrum().unwrap().bins_dbfs[128];
        assert!(
            (peak_db - main_db).abs() < 1.0,
            "peak {peak_db} dBFS, curve {main_db} dBFS"
        );

        let palette = crate::theme::Palette::default();
        let inside = Some(Point { x: 400.0, y: 200.0 });
        let mut target = RecordingTarget::default();
        module.draw(frame_at(&mut target, &palette, inside, 0.0));
        assert_eq!(target.polylines.len(), 1, "only the curve by default");
        assert!(target.texts.is_empty(), "no readout by default");

        for key in ["show_peak", "show_readout"] {
            module
                .set_view_setting(key, &SettingValue::Bool(true))
                .unwrap();
        }
        let mut target = RecordingTarget::default();
        module.draw(frame_at(&mut target, &palette, inside, 0.0));
        assert_eq!(target.polylines.len(), 2, "curve and peak curve");
        assert_eq!(target.texts.len(), 1, "the cursor readout");
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

        // One shared Channel/FFT/Tilt chain, one private Peak each.
        let shared_prefix = fixture.services.dataflow.metrics();
        assert_eq!(shared_prefix.active_fft_nodes, 1);
        assert_eq!(shared_prefix.active_spectrum_nodes, 3);
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
            shared.spectrum_evaluations, 15,
            "a shared Tilt and two private Peaks per hop"
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
                    reference_dbfs: crate::module::DEFAULT_REFERENCE_DBFS,
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
