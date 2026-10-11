use std::time::Duration;

use crate::{
    audio::SourceReader,
    dataflow::{
        ChannelSelection, DataController, DataSpec, DataSubscription, Keep, NodeSpec, OutputId,
        SOURCE,
    },
    extensions::{Registry, RegistryError},
    module::{
        Module, ModuleError, ModuleFactory, ModuleFrame, ModuleResponse, ModuleServices,
        StyleOption, number_setting, spec_from_saved,
    },
    render::{Circle, ColoredRect, Point, Rect, Stroke, TextStyle},
    settings::{SettingKind, SettingValue, ViewSetting},
    snapshot::{DataReader, HistoryView},
    theme::ColorRole,
};

/// Source time the Waveform keeps by default; it shows up to its History.
const WAVEFORM_HISTORY_SECONDS: f32 = 10.0;
/// Shortest Waveform span.
const WAVEFORM_MIN_SECONDS: f32 = 0.25;
const OSCILLOSCOPE_SCALE: (f32, f32) = (0.2, 10.0);
const WAVEFORM_SCALE: (f32, f32) = (0.1, 20.0);
const STEREOMETER_SCALE: (f32, f32) = (0.5, 10.0);

/// A subscription to the merged source PCM (or the saved equivalent).
fn subscribe_pcm(
    services: &ModuleServices,
    keep: Keep,
    saved: Option<&DataSpec>,
) -> Result<(DataSubscription, OutputId), ModuleError> {
    let mut spec = DataSpec::new();
    let output = spec.output(SOURCE, keep);
    let spec = spec_from_saved(spec, saved)?;
    Ok((services.dataflow.subscribe(spec)?, output))
}

fn scale_setting(value: f32, range: (f32, f32)) -> ViewSetting {
    ViewSetting {
        key: "scale",
        label: "Scale",
        section: "Display",
        kind: SettingKind::Number {
            min: range.0,
            max: range.1,
            unit: "×",
            logarithmic: true,
        },
        value: SettingValue::Number(value),
    }
}

fn channel_setting(channel: ChannelSelection) -> ViewSetting {
    ViewSetting {
        key: "channel",
        label: "Channel",
        section: "Display",
        kind: SettingKind::Channel,
        value: SettingValue::Channel(channel),
    }
}

fn channel_value(key: &str, value: &SettingValue) -> Result<ChannelSelection, ModuleError> {
    value
        .channel()
        .ok_or_else(|| ModuleError::InvalidSetting(key.to_owned()))
}

use super::oscilloscope_style::{
    self, LINE_OSCILLOSCOPE_STYLE_ID, OscilloscopeStyle, OscilloscopeStyleContext,
    PERSISTENCE_OSCILLOSCOPE_STYLE_ID,
};

pub const OSCILLOSCOPE_MODULE_ID: &str = "taffymeters.oscilloscope";
pub const WAVEFORM_MODULE_ID: &str = "taffymeters.waveform";
pub const STEREOMETER_MODULE_ID: &str = "taffymeters.stereometer";
pub const LEVEL_METER_MODULE_ID: &str = "taffymeters.level_meter";
pub const EMPTY_MODULE_ID: &str = "taffymeters.empty";

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
    oscilloscope_style::register(registry)?;
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
        data: Option<&DataSpec>,
    ) -> Result<Box<dyn Module>, RegistryError> {
        let (subscription, output) = subscribe_pcm(services, Keep::Latest, data)?;
        Ok(Box::new(OscilloscopeModule {
            source: services.source.clone(),
            controller: subscription.controller(),
            reader: subscription.reader(),
            _subscription: subscription,
            output,
            channel: ChannelSelection::MonoAverage,
            y_scale: 1.0,
            style: registry.create_oscilloscope_style(LINE_OSCILLOSCOPE_STYLE_ID)?,
            style_id: LINE_OSCILLOSCOPE_STYLE_ID,
        }))
    }
}

struct OscilloscopeModule {
    source: SourceReader,
    controller: DataController,
    reader: DataReader,
    _subscription: DataSubscription,
    output: OutputId,
    channel: ChannelSelection,
    y_scale: f32,
    style: Box<dyn OscilloscopeStyle>,
    style_id: &'static str,
}

impl Module for OscilloscopeModule {
    fn channel(&self) -> Option<ChannelSelection> {
        Some(self.channel)
    }

    fn set_channel(&mut self, channel: ChannelSelection) -> Result<(), ModuleError> {
        channel.resolve(self.source.descriptor())?;
        if self.channel == channel {
            return Ok(());
        }
        self.channel = channel;
        self.style.clear();
        Ok(())
    }

    fn set_visible(&mut self, visible: bool) {
        let _ = self.controller.set_visible(visible);
    }

    fn data(&self) -> Option<&DataController> {
        Some(&self.controller)
    }

    fn color_roles(&self) -> &'static [ColorRole] {
        &[ColorRole::Primary, ColorRole::Guide]
    }

    fn view_settings(&self) -> Vec<ViewSetting> {
        vec![
            channel_setting(self.channel),
            scale_setting(self.y_scale, OSCILLOSCOPE_SCALE),
        ]
    }

    fn set_view_setting(&mut self, key: &str, value: &SettingValue) -> Result<(), ModuleError> {
        match key {
            "channel" => self.set_channel(channel_value(key, value)?),
            "scale" => {
                self.y_scale = number_setting(key, value, OSCILLOSCOPE_SCALE)?;
                Ok(())
            }
            _ => Err(ModuleError::UnknownSetting(key.to_owned())),
        }
    }

    fn draw(&mut self, frame: ModuleFrame<'_>) -> ModuleResponse {
        if frame
            .pointer
            .is_some_and(|point| frame.rect.contains(point))
            && frame.scroll_delta_y.abs() > 0.01
        {
            self.y_scale = (self.y_scale * (frame.scroll_delta_y * 0.002).exp())
                .clamp(OSCILLOSCOPE_SCALE.0, OSCILLOSCOPE_SCALE.1);
        }
        let Some(snapshot) = self.reader.latest(self.output) else {
            return ModuleResponse::default();
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
                color: frame.palette.color(ColorRole::Guide),
            },
        );
        self.style.draw(
            OscilloscopeStyleContext {
                descriptor: self.reader.descriptor(),
                frame: &snapshot,
                channel: self.channel,
                rect: frame.rect,
                y_scale: self.y_scale,
                monotonic_time: frame.monotonic_time,
                palette: frame.palette,
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
        data: Option<&DataSpec>,
    ) -> Result<Box<dyn Module>, RegistryError> {
        let (subscription, output) =
            subscribe_pcm(services, Keep::Seconds(WAVEFORM_HISTORY_SECONDS), data)?;
        Ok(Box::new(WaveformModule {
            source: services.source.clone(),
            controller: subscription.controller(),
            reader: subscription.reader(),
            _subscription: subscription,
            output,
            history: HistoryView::new(),
            channel: ChannelSelection::MonoAverage,
            seconds: 4.0,
            y_scale: 1.0,
            bars: Vec::with_capacity(2048),
            envelope: WaveformEnvelope::default(),
        }))
    }
}

struct WaveformModule {
    source: SourceReader,
    controller: DataController,
    reader: DataReader,
    _subscription: DataSubscription,
    output: OutputId,
    history: HistoryView,
    channel: ChannelSelection,
    seconds: f32,
    y_scale: f32,
    bars: Vec<ColoredRect>,
    envelope: WaveformEnvelope,
}

impl WaveformModule {
    /// History length of the output: the most the view can show.
    fn history_seconds(&self) -> f32 {
        self.controller
            .with_spec(|spec| match spec.outputs().get(self.output.index()) {
                Some(output) => match output.keep {
                    Keep::Seconds(seconds) => seconds,
                    Keep::Latest => WAVEFORM_HISTORY_SECONDS,
                },
                None => WAVEFORM_HISTORY_SECONDS,
            })
            .max(WAVEFORM_MIN_SECONDS)
    }
}

/// Display sampling cache: min/max per fixed-size bin of absolute source frames, so
/// each draw only folds in samples published since the previous draw.
#[derive(Default)]
struct WaveformEnvelope {
    key: Option<(
        u64,
        ChannelSelection,
        crate::audio::SourceId,
        crate::audio::Generation,
    )>,
    /// Exclusive end of the source frames already folded in.
    through: u64,
    /// `(bin, min, max)`, bins ascending; bins without samples are absent (gaps).
    bins: std::collections::VecDeque<(u64, f32, f32)>,
}

impl Module for WaveformModule {
    fn channel(&self) -> Option<ChannelSelection> {
        Some(self.channel)
    }

    fn set_channel(&mut self, channel: ChannelSelection) -> Result<(), ModuleError> {
        channel.resolve(self.source.descriptor())?;
        self.channel = channel;
        Ok(())
    }
    fn clear_visual_state(&mut self) {
        self.bars.clear();
        self.envelope = WaveformEnvelope::default();
    }

    fn data(&self) -> Option<&DataController> {
        Some(&self.controller)
    }

    fn output_label(&self, output: OutputId) -> Option<&'static str> {
        (output == self.output).then_some("History")
    }

    fn color_roles(&self) -> &'static [ColorRole] {
        &[ColorRole::Low, ColorRole::Mid, ColorRole::High]
    }

    fn view_settings(&self) -> Vec<ViewSetting> {
        vec![
            channel_setting(self.channel),
            ViewSetting {
                key: "visible_seconds",
                label: "Visible time",
                section: "Display",
                kind: SettingKind::Number {
                    min: WAVEFORM_MIN_SECONDS,
                    max: self.history_seconds(),
                    unit: "s",
                    logarithmic: false,
                },
                value: SettingValue::Number(self.seconds),
            },
            scale_setting(self.y_scale, WAVEFORM_SCALE),
        ]
    }

    fn set_view_setting(&mut self, key: &str, value: &SettingValue) -> Result<(), ModuleError> {
        match key {
            "channel" => return self.set_channel(channel_value(key, value)?),
            "visible_seconds" => {
                self.seconds =
                    number_setting(key, value, (WAVEFORM_MIN_SECONDS, self.history_seconds()))?;
            }
            "scale" => self.y_scale = number_setting(key, value, WAVEFORM_SCALE)?,
            _ => return Err(ModuleError::UnknownSetting(key.to_owned())),
        }
        Ok(())
    }

    fn draw(&mut self, frame: ModuleFrame<'_>) -> ModuleResponse {
        if frame
            .pointer
            .is_some_and(|point| frame.rect.contains(point))
        {
            if (frame.zoom_delta - 1.0).abs() > 0.001 {
                self.seconds = (self.seconds / frame.zoom_delta)
                    .clamp(WAVEFORM_MIN_SECONDS, self.history_seconds());
            }
            if frame.scroll_delta_y.abs() > 0.01 {
                self.y_scale = (self.y_scale * (frame.scroll_delta_y * 0.002).exp())
                    .clamp(WAVEFORM_SCALE.0, WAVEFORM_SCALE.1);
            }
        }
        self.reader.sync_history(self.output, &mut self.history);
        let Some(newest) = self.history.newest_frontier() else {
            return ModuleResponse::default();
        };
        let descriptor = self.reader.descriptor();
        let Ok(channel) = self.channel.resolve(descriptor) else {
            return ModuleResponse::default();
        };
        let columns = frame.rect.width().floor().max(1.0) as u64;
        let span_frames = ((self.seconds * descriptor.sample_rate_hz as f32) as u64).max(1);
        let frames_per_bin = (span_frames / columns).max(1);
        let key = (
            frames_per_bin,
            self.channel,
            descriptor.source_id,
            descriptor.generation,
        );
        let envelope = &mut self.envelope;
        if envelope.key != Some(key) || newest < envelope.through {
            *envelope = WaveformEnvelope {
                key: Some(key),
                ..WaveformEnvelope::default()
            };
        }
        let last_bin = newest.saturating_sub(1) / frames_per_bin;
        let first_bin = (last_bin + 1).saturating_sub(columns);
        let channels = descriptor.channel_count as usize;
        for chunk in self.history.iter().filter_map(|entry| entry.frame.pcm()) {
            let chunk_end = chunk.start_frame.saturating_add(chunk.frame_count as u64);
            let start = chunk
                .start_frame
                .max(envelope.through)
                .max(first_bin * frames_per_bin);
            if chunk_end <= start {
                continue;
            }
            let skip = (start - chunk.start_frame) as usize;
            for (offset, samples) in chunk
                .interleaved
                .chunks_exact(channels)
                .enumerate()
                .skip(skip)
            {
                let bin = (chunk.start_frame + offset as u64) / frames_per_bin;
                let sample = channel.sample(samples);
                match envelope.bins.back_mut() {
                    Some((last, minimum, maximum)) if *last == bin => {
                        *minimum = minimum.min(sample);
                        *maximum = maximum.max(sample);
                    }
                    _ => envelope.bins.push_back((bin, sample, sample)),
                }
            }
            envelope.through = chunk_end;
        }
        while envelope
            .bins
            .front()
            .is_some_and(|(bin, _, _)| *bin < first_bin)
        {
            envelope.bins.pop_front();
        }
        self.bars.clear();
        let center = frame.rect.center().y;
        let half_height = frame.rect.height() * 0.5;
        for &(bin, minimum, maximum) in &envelope.bins {
            let column = (bin - first_bin) as f32;
            let top = center - (maximum * self.y_scale).clamp(-1.0, 1.0) * half_height;
            let bottom = center - (minimum * self.y_scale).clamp(-1.0, 1.0) * half_height;
            let amplitude = maximum.abs().max(minimum.abs()) * self.y_scale;
            self.bars.push(ColoredRect {
                rect: Rect {
                    min: Point {
                        x: frame.rect.min.x + column,
                        y: top.min(bottom),
                    },
                    max: Point {
                        x: frame.rect.min.x + column + 1.0,
                        y: top.max(bottom).max(top.min(bottom) + 1.0),
                    },
                },
                color: frame.palette.level(amplitude.clamp(0.0, 1.0)),
            });
        }
        frame.renderer.fill_rects(frame.rect, &self.bars);
        ModuleResponse::default()
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
        data: Option<&DataSpec>,
    ) -> Result<Box<dyn Module>, RegistryError> {
        let (subscription, output) = subscribe_pcm(services, Keep::Latest, data)?;
        Ok(Box::new(StereometerModule {
            controller: subscription.controller(),
            reader: subscription.reader(),
            _subscription: subscription,
            output,
            scale: 1.0,
            circles: Vec::with_capacity(512),
        }))
    }
}

struct StereometerModule {
    controller: DataController,
    reader: DataReader,
    _subscription: DataSubscription,
    output: OutputId,
    scale: f32,
    circles: Vec<Circle>,
}

impl Module for StereometerModule {
    fn set_visible(&mut self, visible: bool) {
        let _ = self.controller.set_visible(visible);
    }

    fn data(&self) -> Option<&DataController> {
        Some(&self.controller)
    }

    fn color_roles(&self) -> &'static [ColorRole] {
        &[ColorRole::Primary, ColorRole::Guide, ColorRole::Muted]
    }

    fn view_settings(&self) -> Vec<ViewSetting> {
        vec![scale_setting(self.scale, STEREOMETER_SCALE)]
    }

    fn set_view_setting(&mut self, key: &str, value: &SettingValue) -> Result<(), ModuleError> {
        if key != "scale" {
            return Err(ModuleError::UnknownSetting(key.to_owned()));
        }
        self.scale = number_setting(key, value, STEREOMETER_SCALE)?;
        Ok(())
    }

    fn draw(&mut self, frame: ModuleFrame<'_>) -> ModuleResponse {
        if frame
            .pointer
            .is_some_and(|point| frame.rect.contains(point))
            && frame.scroll_delta_y.abs() > 0.01
        {
            self.scale = (self.scale * (frame.scroll_delta_y * 0.002).exp())
                .clamp(STEREOMETER_SCALE.0, STEREOMETER_SCALE.1);
        }
        let Some(snapshot) = self.reader.latest(self.output) else {
            return ModuleResponse::default();
        };
        let channels = self.reader.descriptor().channel_count as usize;
        if channels < 2 {
            frame.renderer.text(
                frame.rect,
                frame.rect.center(),
                "Stereo input required",
                TextStyle {
                    size: frame.palette.text_size + 2.0,
                    color: frame.palette.color(ColorRole::Muted),
                },
            );
            return ModuleResponse::default();
        }
        let center = frame.rect.center();
        let radius = frame.rect.width().min(frame.rect.height()) * 0.45;
        let guide = Stroke {
            width: 1.0,
            color: frame.palette.color(ColorRole::Guide),
        };
        let dot = frame.palette.color(ColorRole::Primary);
        let dot = dot.with_alpha((f32::from(dot.alpha) * 180.0 / 255.0) as u8);
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
        if let Some(chunk) = snapshot.pcm() {
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
                    color: dot,
                });
            }
        }
        frame.renderer.circles(frame.rect, &self.circles);
        ModuleResponse::default()
    }
    fn clear_visual_state(&mut self) {
        self.circles.clear();
    }
}

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
            animating |= self.peak_holds[channel].update(
                is_new_snapshot.then_some(current_peak_dbfs),
                frame.monotonic_time,
            );
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
        data: Option<&DataSpec>,
    ) -> Result<Box<dyn Module>, RegistryError> {
        // Empty has no analysis: saved analysis it cannot hold makes the leaf a
        // stand-in that keeps the original, instead of being dropped on save.
        spec_from_saved(DataSpec::new(), data)?;
        Ok(Box::new(EmptyModule))
    }
}

struct EmptyModule;

impl Module for EmptyModule {
    fn draw(&mut self, _frame: ModuleFrame<'_>) -> ModuleResponse {
        ModuleResponse::default()
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
    use std::{sync::Arc, time::Instant};

    use crate::{
        audio::{
            AudioDescriptor, ChannelLayout, Generation, SourceId, SourceReader, SourceTelemetry,
            create_block_transport,
        },
        dataflow::Dataflow,
        render::{RenderTarget, Stroke},
    };

    #[derive(Default)]
    struct RecordingTarget {
        polylines: usize,
    }

    impl RenderTarget for RecordingTarget {
        fn polyline(&mut self, _clip: Rect, points: &[Point], _stroke: Stroke) {
            self.polylines += usize::from(!points.is_empty());
        }

        fn fill_rects(&mut self, _clip: Rect, _rects: &[ColoredRect]) {}

        fn circles(&mut self, _clip: Rect, _circles: &[Circle]) {}

        fn text(&mut self, _clip: Rect, _position: Point, _text: &str, _style: TextStyle) {}
    }

    #[test]
    fn reselecting_oscilloscope_channel_keeps_persistence_traces() {
        let descriptor = Arc::new(
            AudioDescriptor::new(SourceId(1), Generation(1), 48_000, 2, ChannelLayout::Stereo)
                .unwrap(),
        );
        let telemetry = Arc::new(SourceTelemetry::new());
        let (mut capture, endpoint) =
            create_block_transport(2, 8, 1_600, Arc::clone(&telemetry)).unwrap();
        let (dataflow, handle) =
            Dataflow::spawn(Arc::clone(&descriptor), endpoint, Arc::clone(&telemetry));
        capture.set_worker(dataflow.worker_thread());
        let services = ModuleServices {
            source: SourceReader::new(descriptor, telemetry),
            dataflow: handle,
        };
        let mut registry = Registry::default();
        register(&mut registry).unwrap();
        let mut module = OscilloscopeFactory
            .create(&services, &registry, None)
            .unwrap();
        let palette = crate::theme::Palette::default();
        module
            .set_style(PERSISTENCE_OSCILLOSCOPE_STYLE_ID, &registry)
            .unwrap();
        let samples: Vec<f32> = (0..1_600).flat_map(|_| [0.2, -0.1]).collect();
        capture.write(&samples);

        let rect = Rect {
            min: Point { x: 0.0, y: 0.0 },
            max: Point { x: 400.0, y: 200.0 },
        };
        let mut target = RecordingTarget::default();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            target.polylines = 0;
            module.draw(ModuleFrame {
                rect,
                monotonic_time: Duration::ZERO,
                pointer: None,
                scroll_delta_y: 0.0,
                zoom_delta: 1.0,
                palette: &palette,
                renderer: &mut target,
            });
            if target.polylines == 2 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Persistence never received audio"
            );
            std::thread::yield_now();
        }

        module.set_channel(ChannelSelection::MonoAverage).unwrap();
        target.polylines = 0;
        module.draw(ModuleFrame {
            rect,
            monotonic_time: Duration::from_millis(50),
            pointer: None,
            scroll_delta_y: 0.0,
            zoom_delta: 1.0,
            palette: &palette,
            renderer: &mut target,
        });
        assert_eq!(target.polylines, 2, "reselecting cleared a live trace");
    }

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
