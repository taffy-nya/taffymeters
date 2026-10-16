use std::time::Duration;

use crate::{
    dataflow::{DataController, DataSpec, DataSubscription, Keep, NodeSpec, OutputId, SOURCE},
    extensions::{Registry, RegistryError},
    module::{
        Module, ModuleError, ModuleFactory, ModuleFrame, ModuleResponse, ModuleServices,
        flag_setting, overlay_setting, spec_from_saved,
    },
    render::{ColoredRect, Point, Rect},
    settings::{SettingValue, ViewSetting},
    snapshot::DataReader,
    theme::ColorRole,
};

pub const LEVEL_METER_MODULE_ID: &str = "taffymeters.level_meter";

const METER_FLOOR_DBFS: f32 = -60.0;
const METER_PEAK_HOLD: Duration = Duration::from_secs(3);
const METER_PEAK_FALL_DB_PER_SECOND: f32 = 18.0;

#[derive(Clone, Copy)]
struct MeterPeakHold {
    value_dbfs: f32,
    hold_until: Duration,
    last_update: Duration,
}

impl Default for MeterPeakHold {
    fn default() -> Self {
        Self {
            value_dbfs: METER_FLOOR_DBFS,
            hold_until: Duration::ZERO,
            last_update: Duration::ZERO,
        }
    }
}

impl MeterPeakHold {
    fn update(&mut self, measurement_dbfs: Option<f32>, now: Duration) -> bool {
        let target = measurement_dbfs.unwrap_or(METER_FLOOR_DBFS);
        let decay_start = self.last_update.max(self.hold_until);
        if now > decay_start && self.value_dbfs > target {
            let elapsed = now.saturating_sub(decay_start).as_secs_f32();
            self.value_dbfs =
                (self.value_dbfs - METER_PEAK_FALL_DB_PER_SECOND * elapsed).max(target);
        }
        if measurement_dbfs.is_some() && target >= self.value_dbfs {
            self.value_dbfs = target;
            self.hold_until = now + METER_PEAK_HOLD;
        }
        self.last_update = now;
        self.value_dbfs > target
    }
}

fn amplitude_to_dbfs(amplitude: f32) -> f32 {
    if amplitude <= 0.001 {
        METER_FLOOR_DBFS
    } else {
        (20.0 * amplitude.log10()).clamp(METER_FLOOR_DBFS, 3.0)
    }
}

pub(super) fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register_module(LevelMeterFactory)
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
        let mut spec = DataSpec::new();
        let levels = spec.add(SOURCE, NodeSpec::Levels);
        let output = spec.output(levels, Keep::Latest);
        let spec = spec_from_saved(spec, data)?;
        let subscription = services
            .dataflow
            .subscribe(spec)
            .map_err(ModuleError::from)?;
        Ok(Box::new(LevelMeterModule {
            controller: subscription.controller(),
            reader: subscription.reader(),
            _subscription: subscription,
            output,
            peak_holds: Vec::new(),
            last_result_version: 0,
            bars: Vec::with_capacity(12),
            show_peak: false,
        }))
    }
}

struct LevelMeterModule {
    controller: DataController,
    reader: DataReader,
    _subscription: DataSubscription,
    output: OutputId,
    peak_holds: Vec<MeterPeakHold>,
    last_result_version: u64,
    bars: Vec<ColoredRect>,
    show_peak: bool,
}

impl Module for LevelMeterModule {
    fn data(&self) -> Option<&DataController> {
        Some(&self.controller)
    }

    fn output_label(&self, output: OutputId) -> Option<&'static str> {
        (output == self.output).then_some("Levels")
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
        let Some(snapshot) = self.reader.latest(self.output) else {
            return ModuleResponse::default();
        };
        let Some(levels) = snapshot.levels() else {
            return ModuleResponse::default();
        };
        let shown_channels = levels
            .momentary_lufs
            .len()
            .min(levels.sample_peaks.len())
            .min(8);
        self.peak_holds
            .resize(shown_channels, MeterPeakHold::default());
        let is_new_snapshot = snapshot.result_version != self.last_result_version;
        self.last_result_version = snapshot.result_version;
        let gap = 5.0;
        let width =
            (frame.rect.width() - gap * (shown_channels + 1) as f32) / shown_channels.max(1) as f32;
        self.bars.clear();
        let mut animating = false;
        for channel in 0..shown_channels {
            let current_lufs = levels.momentary_lufs[channel].clamp(-60.0, 3.0);
            let current_peak_dbfs = amplitude_to_dbfs(levels.sample_peaks[channel]);
            // A hidden marker does not animate; shown again, it falls from where
            // it was to the current level at once.
            if self.show_peak {
                animating |= self.peak_holds[channel].update(
                    is_new_snapshot.then_some(current_peak_dbfs),
                    frame.monotonic_time,
                );
            }
            let left = frame.rect.min.x + gap + channel as f32 * (width + gap);
            let bar = Rect {
                min: Point {
                    x: left,
                    y: frame.rect.min.y + 8.0,
                },
                max: Point {
                    x: left + width,
                    y: frame.rect.max.y - 8.0,
                },
            };
            self.bars.push(ColoredRect {
                rect: bar,
                color: frame.palette.color(ColorRole::Surface),
            });
            let normalized = ((current_lufs + 60.0) / 63.0).clamp(0.0, 1.0);
            self.bars.push(ColoredRect {
                rect: Rect {
                    min: Point {
                        x: bar.min.x,
                        y: bar.max.y - bar.height() * normalized,
                    },
                    max: bar.max,
                },
                color: meter_color(frame.palette, normalized),
            });
            if !self.show_peak {
                continue;
            }
            let peak = ((self.peak_holds[channel].value_dbfs + 60.0) / 63.0).clamp(0.0, 1.0);
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
                color: frame.palette.color(ColorRole::Text),
            });
        }
        frame.renderer.fill_rects(frame.rect, &self.bars);
        if animating {
            animate()
        } else {
            ModuleResponse::default()
        }
    }
    fn clear_visual_state(&mut self) {
        self.peak_holds.clear();
        self.last_result_version = 0;
        self.bars.clear();
    }
}

fn animate() -> ModuleResponse {
    ModuleResponse {
        repaint_after: Some(crate::module::FRAME_INTERVAL),
    }
}

fn meter_color(palette: &crate::theme::Palette, normalized: f32) -> crate::render::Color {
    if normalized < 0.7 {
        palette.color(ColorRole::Low)
    } else if normalized < 0.85 {
        palette.color(ColorRole::Mid)
    } else {
        palette.color(ColorRole::High)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meter_peak_uses_dbfs_and_decays_by_elapsed_time() {
        assert!((amplitude_to_dbfs(0.5) - -6.020_6).abs() < 0.001);

        let mut peak = MeterPeakHold::default();
        assert!(!peak.update(Some(-6.0), Duration::ZERO));
        assert_eq!(peak.value_dbfs, -6.0);
        assert!(peak.update(None, Duration::from_secs(2)));
        assert_eq!(peak.value_dbfs, -6.0);

        assert!(peak.update(None, Duration::from_secs(5)));
        assert!((peak.value_dbfs - -42.0).abs() < 0.001);
        assert!(!peak.update(None, Duration::from_secs(10)));
        assert_eq!(peak.value_dbfs, METER_FLOOR_DBFS);
    }
}
