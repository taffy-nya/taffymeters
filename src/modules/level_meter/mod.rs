//! Level Meter (per channel sample peak) and Loudness Meter (programme momentary
//! loudness). Both draw vertical bars on the same absolute −60…+3 scale; every
//! ballistic (the bar's fall, the held peak) is computed by the dataflow in source
//! time, so the Modules only draw the latest results.

mod loudness;

use crate::{
    dataflow::{
        DataController, DataSpec, DataSubscription, Keep, NodeRef, NodeSpec, OutputId, PeakConfig,
        SOURCE,
    },
    extensions::{Registry, RegistryError},
    module::{
        Module, ModuleError, ModuleFactory, ModuleFrame, ModuleResponse, ModuleServices,
        flag_setting, overlay_setting, spec_from_saved,
    },
    render::{Color, ColoredRect, Point, Rect},
    settings::{SettingValue, ViewSetting},
    snapshot::{DataReader, Snapshot},
    theme::{ColorRole, Palette},
};

pub use loudness::LOUDNESS_METER_MODULE_ID;

pub const LEVEL_METER_MODULE_ID: &str = "taffymeters.level_meter";

/// Scale of both meters (dBFS or LUFS): headroom above full scale, as in DAWs.
const METER_BOTTOM: f32 = -60.0;
const METER_TOP: f32 = 3.0;
const BAR_GAP: f32 = 5.0;
const BAR_INSET: f32 = 8.0;
const MAX_BARS: usize = 8;
const PEAK_MARKER_HEIGHT: f32 = 2.0;

/// The bar: instant rise, no hold, a quick fall gathering speed.
const BAR_PEAK: PeakConfig = PeakConfig {
    hold_ms: 0.0,
    fall_db_per_second_squared: 120.0,
};
/// The peak hold marker.
const HELD_PEAK: PeakConfig = PeakConfig {
    hold_ms: 2_000.0,
    fall_db_per_second_squared: 30.0,
};

pub(super) fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register_module(LevelMeterFactory)?;
    registry.register_module(loudness::LoudnessMeterFactory)
}

/// `value` on the meter scale, 0 at the bottom and 1 at the top.
fn normalized(value: f32) -> f32 {
    ((value - METER_BOTTOM) / (METER_TOP - METER_BOTTOM)).clamp(0.0, 1.0)
}

/// Track of bar `index` of `count` side by side in `rect`.
fn bar_track(rect: Rect, index: usize, count: usize) -> Rect {
    let width = (rect.width() - BAR_GAP * (count + 1) as f32) / count.max(1) as f32;
    let left = rect.min.x + BAR_GAP + index as f32 * (width + BAR_GAP);
    Rect {
        min: Point {
            x: left,
            y: rect.min.y + BAR_INSET,
        },
        max: Point {
            x: left + width,
            y: rect.max.y - BAR_INSET,
        },
    }
}

/// The track and its fill up to `level`.
fn push_bar(rects: &mut Vec<ColoredRect>, palette: &Palette, track: Rect, level: f32) {
    rects.push(ColoredRect {
        rect: track,
        color: palette.color(ColorRole::Surface),
    });
    let fill = normalized(level);
    rects.push(ColoredRect {
        rect: Rect {
            min: Point {
                x: track.min.x,
                y: track.max.y - track.height() * fill,
            },
            max: track.max,
        },
        color: meter_color(palette, fill),
    });
}

fn meter_color(palette: &Palette, normalized: f32) -> Color {
    if normalized < 0.7 {
        palette.color(ColorRole::Low)
    } else if normalized < 0.85 {
        palette.color(ColorRole::Mid)
    } else {
        palette.color(ColorRole::High)
    }
}

/// The Level Meter's data: per channel sample peaks, held twice — quickly for the
/// bar and long for the peak marker.
struct LevelMeterData {
    spec: DataSpec,
    bar_node: NodeRef,
    bar: OutputId,
    peak: OutputId,
}

fn level_meter_data() -> LevelMeterData {
    let mut spec = DataSpec::new();
    let levels = spec.add(SOURCE, NodeSpec::Levels);
    let bar_node = spec.add(levels, NodeSpec::Peak(BAR_PEAK));
    let peak_node = spec.add(levels, NodeSpec::Peak(HELD_PEAK));
    let bar = spec.output(bar_node, Keep::Latest);
    let peak = spec.output(peak_node, Keep::Latest);
    LevelMeterData {
        spec,
        bar_node,
        bar,
        peak,
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
        data: Option<&DataSpec>,
    ) -> Result<Box<dyn Module>, RegistryError> {
        let defaults = level_meter_data();
        let spec = spec_from_saved(defaults.spec, data)?;
        let subscription = services
            .dataflow
            .subscribe(spec)
            .map_err(ModuleError::from)?;
        Ok(Box::new(LevelMeterModule {
            controller: subscription.controller(),
            reader: subscription.reader(),
            _subscription: subscription,
            bar_node: defaults.bar_node,
            bar: defaults.bar,
            peak: defaults.peak,
            snapshot: Snapshot::default(),
            rects: Vec::with_capacity(3 * MAX_BARS),
            show_peak: false,
        }))
    }

    fn default_data(&self) -> Option<DataSpec> {
        Some(level_meter_data().spec)
    }
}

struct LevelMeterModule {
    controller: DataController,
    reader: DataReader,
    _subscription: DataSubscription,
    bar_node: NodeRef,
    bar: OutputId,
    peak: OutputId,
    snapshot: Snapshot,
    rects: Vec<ColoredRect>,
    show_peak: bool,
}

impl Module for LevelMeterModule {
    fn data(&self) -> Option<&DataController> {
        Some(&self.controller)
    }

    fn output_label(&self, output: OutputId) -> Option<&'static str> {
        if output == self.bar {
            Some("Bar")
        } else if output == self.peak {
            Some("Peak")
        } else {
            None
        }
    }

    fn node_label(&self, node: NodeRef) -> Option<&'static str> {
        (node == self.bar_node).then_some("Bar")
    }

    fn color_roles(&self) -> &'static [ColorRole] {
        &[
            ColorRole::Surface,
            ColorRole::Low,
            ColorRole::Mid,
            ColorRole::High,
            ColorRole::Text,
        ]
    }

    fn view_settings(&self) -> Vec<ViewSetting> {
        vec![overlay_setting("show_peak", "Peak hold", self.show_peak)]
    }

    fn set_view_setting(&mut self, key: &str, value: &SettingValue) -> Result<(), ModuleError> {
        if key != "show_peak" {
            return Err(ModuleError::UnknownSetting(key.to_owned()));
        }
        self.show_peak = flag_setting(key, value)?;
        Ok(())
    }

    fn draw(&mut self, frame: ModuleFrame<'_>) -> ModuleResponse {
        self.reader.read(&mut self.snapshot);
        let Some(bars) = self.snapshot.get(self.bar).and_then(|bar| bar.levels()) else {
            return ModuleResponse::default();
        };
        let peaks = self
            .show_peak
            .then(|| self.snapshot.get(self.peak).and_then(|peak| peak.levels()))
            .flatten();
        let count = bars.dbfs.len().min(MAX_BARS);
        self.rects.clear();
        for (channel, &level) in bars.dbfs.iter().take(count).enumerate() {
            let track = bar_track(frame.rect, channel, count);
            push_bar(&mut self.rects, frame.palette, track, level);
            let Some(&held) = peaks.and_then(|peaks| peaks.dbfs.get(channel)) else {
                continue;
            };
            // Never under the bar, whatever the two falls are set to.
            let y = track.max.y - track.height() * normalized(held.max(level));
            self.rects.push(ColoredRect {
                rect: Rect {
                    min: Point { x: track.min.x, y },
                    max: Point {
                        x: track.max.x,
                        y: y + PEAK_MARKER_HEIGHT,
                    },
                },
                color: frame.palette.color(ColorRole::Text),
            });
        }
        frame.renderer.fill_rects(frame.rect, &self.rects);
        ModuleResponse::default()
    }

    fn clear_visual_state(&mut self) {
        self.snapshot = Snapshot::default();
        self.rects.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::test_support::{
        BLOCK_FRAMES, PcmFixture, RecordingTarget, TEST_RECT, draw, repeat_frame,
    };
    use std::time::Duration;

    fn create(fixture: &PcmFixture) -> Box<dyn Module> {
        LevelMeterFactory
            .create(&fixture.services, &Registry::default(), None)
            .unwrap()
    }

    /// Top edge of every drawn rect after the background tracks: per channel the
    /// fill, then the marker when shown.
    fn tops(target: &RecordingTarget, palette: &Palette) -> Vec<f32> {
        target
            .rects
            .iter()
            .filter(|rect| rect.color != palette.color(ColorRole::Surface))
            .map(|rect| rect.rect.min.y)
            .collect()
    }

    fn height_of(dbfs: f32) -> f32 {
        let track = bar_track(TEST_RECT, 0, 2);
        track.max.y - track.height() * normalized(dbfs)
    }

    #[test]
    fn stereo_bars_rise_at_once_and_fall_under_the_held_peak() {
        let palette = Palette::default();
        let mut fixture = PcmFixture::new(2);
        let mut meter = create(&fixture);
        meter
            .set_view_setting("show_peak", &SettingValue::Bool(true))
            .unwrap();
        let mut target = RecordingTarget::default();

        // Left at −6 dBFS, right at −12 dBFS: two bars, each up to its own peak
        // within the first interval, with the markers at the same height.
        fixture.feed(&repeat_frame(&[0.5, -0.25], BLOCK_FRAMES));
        draw(meter.as_mut(), &mut target, Duration::ZERO);
        let rose = tops(&target, &palette);
        assert_eq!(rose.len(), 4, "fill and marker per channel: {rose:?}");
        assert!((rose[0] - height_of(-6.02)).abs() < 0.5, "{rose:?}");
        assert!((rose[2] - height_of(-12.04)).abs() < 0.5, "{rose:?}");
        assert!((rose[1] - rose[0]).abs() < 0.5 && (rose[3] - rose[2]).abs() < 0.5);

        // Half a second of silence: the bars fall (½·120·0.5² = 15 dB) while the
        // markers are still held.
        fixture.feed(&vec![0.0; 2 * 15 * BLOCK_FRAMES]);
        target.clear();
        draw(meter.as_mut(), &mut target, Duration::ZERO);
        let fell = tops(&target, &palette);
        assert!((fell[0] - height_of(-6.02 - 15.0)).abs() < 1.0, "{fell:?}");
        assert!(
            (fell[1] - rose[1]).abs() < 0.5,
            "held marker moved: {fell:?}"
        );
        assert!(
            (fell[3] - rose[3]).abs() < 0.5,
            "held marker moved: {fell:?}"
        );
    }
}
