use crate::{
    audio::SourceReader,
    dataflow::{ChannelSelection, DataController, DataSpec, DataSubscription, Keep, OutputId},
    extensions::{Registry, RegistryError},
    module::{
        Module, ModuleError, ModuleFactory, ModuleFrame, ModuleResponse, ModuleServices,
        number_setting,
    },
    render::{ColoredRect, Point, Rect},
    settings::{SettingKind, SettingValue, ViewSetting},
    snapshot::{DataReader, HistoryView},
    theme::ColorRole,
};

use crate::modules::pcm_view::{channel_setting, channel_value, scale_setting, subscribe_pcm};

/// Source time the Waveform keeps by default; it shows up to its History.
const WAVEFORM_HISTORY_SECONDS: f32 = 10.0;
/// Shortest Waveform span.
const WAVEFORM_MIN_SECONDS: f32 = 0.25;
const WAVEFORM_SCALE: (f32, f32) = (0.1, 20.0);

pub const WAVEFORM_MODULE_ID: &str = "taffymeters.waveform";

pub(super) fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register_module(WaveformFactory)
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
    /// Shortest and longest visible span: the view never shows more than the
    /// output's History, even a History shorter than the usual shortest span.
    fn visible_range(&self) -> (f32, f32) {
        let history =
            self.controller
                .with_spec(|spec| match spec.outputs().get(self.output.index()) {
                    Some(output) => match output.keep {
                        Keep::Seconds(seconds) => seconds,
                        Keep::Latest => WAVEFORM_HISTORY_SECONDS,
                    },
                    None => WAVEFORM_HISTORY_SECONDS,
                });
        (WAVEFORM_MIN_SECONDS.min(history), history)
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
        let (min, max) = self.visible_range();
        vec![
            channel_setting(self.channel),
            ViewSetting {
                key: "visible_seconds",
                label: "Visible time",
                section: "Display",
                kind: SettingKind::Number {
                    min,
                    max,
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
                self.seconds = number_setting(key, value, self.visible_range())?;
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
                let (min, max) = self.visible_range();
                self.seconds = (self.seconds / frame.zoom_delta).clamp(min, max);
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
