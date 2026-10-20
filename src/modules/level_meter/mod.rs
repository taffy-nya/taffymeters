//! Level Meter (per channel sample peak) and Loudness Meter (programme momentary
//! loudness). Both draw vertical bars on the same absolute −60…+3 scale. The Level
//! Meter's bar is each publication interval's sample peak as is; its peak hold is
//! computed by the dataflow in source time, so the Modules only draw the latest
//! results.

mod loudness;

use crate::{
    dataflow::{
        DataController, DataSpec, DataSubscription, Keep, NodeSpec, OutputId, PeakConfig, SOURCE,
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
/// The trough and the zone colors; the peak hold mark is Secondary, like
/// Spectrum's peak curve.
const COLOR_ROLES: &[ColorRole] = &[
    ColorRole::Surface,
    ColorRole::Low,
    ColorRole::Mid,
    ColorRole::High,
    ColorRole::Secondary,
];
const PEAK_MARKER_HEIGHT: f32 = 2.0;

/// Level Meter colors: Low below −6 dBFS, Mid from −6 to 0 dBFS, High above full
/// scale.
const SAMPLE_PEAK_ZONES: Zones = Zones {
    mid_from: -6.0,
    high_above: 0.0,
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

/// Where a meter's color changes, on its own scale (dBFS or LUFS).
#[derive(Clone, Copy)]
struct Zones {
    /// Mid from this level up, inclusive.
    mid_from: f32,
    /// High strictly above this level.
    high_above: f32,
}

impl Zones {
    fn color(self, palette: &Palette, level: f32) -> Color {
        if level > self.high_above {
            palette.color(ColorRole::High)
        } else if level >= self.mid_from {
            palette.color(ColorRole::Mid)
        } else {
            palette.color(ColorRole::Low)
        }
    }
}

/// The track and its fill up to `level`, colored by the zone `level` is in.
fn push_bar(
    rects: &mut Vec<ColoredRect>,
    palette: &Palette,
    zones: Zones,
    track: Rect,
    level: f32,
) {
    rects.push(ColoredRect {
        rect: track,
        color: palette.color(ColorRole::Surface),
    });
    rects.push(ColoredRect {
        rect: Rect {
            min: Point {
                x: track.min.x,
                y: track.max.y - track.height() * normalized(level),
            },
            max: track.max,
        },
        color: zones.color(palette, level),
    });
}

/// The Level Meter's data: per channel sample peaks shown as they are by the bar,
/// and held by a Peak for the marker.
struct LevelMeterData {
    spec: DataSpec,
    bar: OutputId,
    peak: OutputId,
}

fn level_meter_data() -> LevelMeterData {
    let mut spec = DataSpec::new();
    let levels = spec.add(SOURCE, NodeSpec::Levels);
    let peak_node = spec.add(levels, NodeSpec::Peak(HELD_PEAK));
    let bar = spec.output(levels, Keep::Latest);
    let peak = spec.output(peak_node, Keep::Latest);
    LevelMeterData { spec, bar, peak }
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

    fn color_roles(&self) -> &'static [ColorRole] {
        COLOR_ROLES
    }
}

struct LevelMeterModule {
    controller: DataController,
    reader: DataReader,
    _subscription: DataSubscription,
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

    fn color_roles(&self) -> &[ColorRole] {
        COLOR_ROLES
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
            push_bar(
                &mut self.rects,
                frame.palette,
                SAMPLE_PEAK_ZONES,
                track,
                level,
            );
            let Some(&held) = peaks.and_then(|peaks| peaks.dbfs.get(channel)) else {
                continue;
            };
            // Never under the bar: right after an edit the held result may lag.
            let y = track.max.y - track.height() * normalized(held.max(level));
            self.rects.push(ColoredRect {
                rect: Rect {
                    min: Point { x: track.min.x, y },
                    max: Point {
                        x: track.max.x,
                        y: y + PEAK_MARKER_HEIGHT,
                    },
                },
                color: frame.palette.color(ColorRole::Secondary),
            });
        }
        frame.renderer.fill_rects(frame.rect, &self.rects);
        ModuleResponse::default()
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
    fn stereo_bars_follow_each_interval_at_once_and_the_marker_holds() {
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

        // One silent interval: the bars drop to the bottom at once while the
        // markers are still held.
        fixture.feed(&vec![0.0; 2 * BLOCK_FRAMES]);
        target.clear();
        draw(meter.as_mut(), &mut target, Duration::ZERO);
        let fell = tops(&target, &palette);
        assert!((fell[0] - height_of(METER_BOTTOM)).abs() < 0.5, "{fell:?}");
        assert!((fell[2] - height_of(METER_BOTTOM)).abs() < 0.5, "{fell:?}");
        assert!(
            (fell[1] - rose[1]).abs() < 0.5,
            "held marker moved: {fell:?}"
        );
        assert!(
            (fell[3] - rose[3]).abs() < 0.5,
            "held marker moved: {fell:?}"
        );
    }

    #[test]
    fn sample_peak_colors_change_at_minus_6_and_above_full_scale() {
        let palette = Palette::default();
        let color = |dbfs| SAMPLE_PEAK_ZONES.color(&palette, dbfs);
        let (low, mid, high) = (
            palette.color(ColorRole::Low),
            palette.color(ColorRole::Mid),
            palette.color(ColorRole::High),
        );
        assert_eq!(color(-6.1), low);
        assert_eq!(color(-6.0), mid);
        assert_eq!(color(0.0), mid, "full scale is not over");
        assert_eq!(color(0.1), high);
    }
}
