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

use crate::modules::pcm_view::{
    channel_setting, channel_value, pcm_spec, scale_setting, subscribe_pcm,
};

/// Visible span a new Waveform shows.
const DEFAULT_VISIBLE_SECONDS: f32 = 4.0;

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
            seconds: DEFAULT_VISIBLE_SECONDS,
            y_scale: 1.0,
            bars: Vec::with_capacity(2048),
            envelope: WaveformEnvelope::default(),
        }))
    }

    fn default_data(&self) -> Option<DataSpec> {
        Some(pcm_spec(Keep::Seconds(WAVEFORM_HISTORY_SECONDS)).0)
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
                default: SettingValue::Number(DEFAULT_VISIBLE_SECONDS),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::{
        dataflow::SOURCE,
        modules::test_support::{
            BLOCK_FRAMES, PcmFixture, RecordingTarget, TEST_RECT, draw, repeat_frame,
        },
    };

    fn waveform(fixture: &PcmFixture, data: Option<&DataSpec>) -> Box<dyn Module> {
        WaveformFactory
            .create(&fixture.services, &Registry::default(), data)
            .unwrap()
    }

    /// A saved spec with the Waveform's shape and a History of `seconds`.
    fn history_of(seconds: f32) -> DataSpec {
        let mut spec = DataSpec::new();
        spec.output(SOURCE, Keep::Seconds(seconds));
        spec
    }

    /// `(min, max, value)` of the visible-time setting.
    fn visible_time(module: &dyn Module) -> (f32, f32, f32) {
        let setting = module
            .view_settings()
            .into_iter()
            .find(|setting| setting.key == "visible_seconds")
            .expect("Waveform has a visible-time setting");
        let SettingKind::Number { min, max, .. } = setting.kind else {
            panic!("visible time is a number");
        };
        (min, max, setting.value.number().unwrap())
    }

    fn set_visible_time(module: &mut dyn Module, seconds: f32) {
        module
            .set_view_setting("visible_seconds", &SettingValue::Number(seconds))
            .unwrap();
    }

    /// Vertical extent of a bar of constant sample `level` at full scale: the bar is
    /// one point tall and `level` moves it up (positive) from the center line.
    fn flat_bar_y(level: f32) -> f32 {
        TEST_RECT.center().y - level * TEST_RECT.height() * 0.5
    }

    #[test]
    fn visible_time_is_limited_to_the_available_history() {
        let fixture = PcmFixture::new(2);

        let mut usual = waveform(&fixture, None);
        let (min, max, _) = visible_time(usual.as_ref());
        assert_eq!((min, max), (WAVEFORM_MIN_SECONDS, WAVEFORM_HISTORY_SECONDS));
        set_visible_time(usual.as_mut(), 1_000.0);
        assert_eq!(visible_time(usual.as_ref()).2, WAVEFORM_HISTORY_SECONDS);
        set_visible_time(usual.as_mut(), 0.01);
        assert_eq!(visible_time(usual.as_ref()).2, WAVEFORM_MIN_SECONDS);

        let two_seconds = history_of(2.0);
        let mut short = waveform(&fixture, Some(&two_seconds));
        assert_eq!(visible_time(short.as_ref()).0, WAVEFORM_MIN_SECONDS);
        assert_eq!(visible_time(short.as_ref()).1, 2.0);
        set_visible_time(short.as_mut(), 4.0);
        assert_eq!(
            visible_time(short.as_ref()).2,
            2.0,
            "never beyond the History"
        );

        // A History shorter than the usual shortest span is the only span there is.
        let tiny_history = history_of(0.1);
        let mut tiny = waveform(&fixture, Some(&tiny_history));
        let (min, max, _) = visible_time(tiny.as_ref());
        assert_eq!((min, max), (0.1, 0.1));
        set_visible_time(tiny.as_mut(), 4.0);
        assert_eq!(visible_time(tiny.as_ref()).2, 0.1);
        set_visible_time(tiny.as_mut(), 0.01);
        assert_eq!(visible_time(tiny.as_ref()).2, 0.1);
    }

    #[test]
    fn history_shorter_than_the_visible_span_is_drawn_from_the_left_edge() {
        let mut fixture = PcmFixture::new(2);
        let mut module = waveform(&fixture, None);
        let mut target = RecordingTarget::default();

        // 1/30 s of a constant 0.5 in the default 4 s window: a thin sliver at the
        // left edge, not stretched across the Module.
        fixture.feed(&repeat_frame(&[0.5, 0.5], BLOCK_FRAMES));
        draw(module.as_mut(), &mut target, Duration::ZERO);
        let first = target.rects.clone();
        assert!(!first.is_empty());
        assert_eq!(first[0].rect.min.x, TEST_RECT.min.x);
        assert!(
            first
                .windows(2)
                .all(|pair| pair[1].rect.min.x == pair[0].rect.max.x),
            "bars are contiguous and increasing"
        );
        let covered = first.last().unwrap().rect.max.x - TEST_RECT.min.x;
        assert!(
            covered < TEST_RECT.width() * 0.25,
            "{covered} of {} points covered by 1/30 s of a 4 s window",
            TEST_RECT.width()
        );
        for bar in &first {
            assert_eq!(bar.rect.min.y, flat_bar_y(0.5));
        }

        // The next 1/30 s (constant -0.5) extends the picture to the right without
        // disturbing what was already folded in. The block boundary falls inside a
        // bar, so that bar holds both levels.
        fixture.feed(&repeat_frame(&[-0.5, -0.5], BLOCK_FRAMES));
        target.clear();
        draw(module.as_mut(), &mut target, Duration::from_millis(50));
        let second = target.rects.clone();
        assert!(second.len() > first.len(), "the picture grows to the right");
        assert_eq!(second[0].rect.min.x, TEST_RECT.min.x);
        assert!(
            second
                .windows(2)
                .all(|pair| pair[1].rect.min.x == pair[0].rect.max.x),
            "bars stay contiguous"
        );
        let settled = first.len() - 1;
        assert_eq!(&second[..settled], &first[..settled], "earlier bars kept");
        let both_levels = second
            .iter()
            .filter(|bar| bar.rect.min.y == flat_bar_y(0.5) && bar.rect.max.y == flat_bar_y(-0.5))
            .count();
        assert_eq!(both_levels, 1, "one bar spans both halves");
        assert_eq!(second.last().unwrap().rect.min.y, flat_bar_y(-0.5));
    }

    #[test]
    fn changing_channel_redraws_the_envelope_from_the_new_channel() {
        let mut fixture = PcmFixture::new(2);
        let mut module = waveform(&fixture, None);
        let mut target = RecordingTarget::default();
        fixture.feed(&repeat_frame(&[0.5, -0.5], BLOCK_FRAMES));

        // Mono mix of +0.5 / -0.5 is silence.
        draw(module.as_mut(), &mut target, Duration::ZERO);
        assert_eq!(target.rects.len(), 4);
        for bar in &target.rects {
            assert_eq!(bar.rect.min.y, flat_bar_y(0.0));
        }

        // No new audio arrived: the bars must still be rebuilt for the new channel.
        for (channel, level) in [
            (ChannelSelection::Left, 0.5),
            (ChannelSelection::Right, -0.5),
        ] {
            module.set_channel(channel).unwrap();
            target.clear();
            draw(module.as_mut(), &mut target, Duration::from_millis(50));
            assert_eq!(target.rects.len(), 4, "{channel:?}");
            for bar in &target.rects {
                assert_eq!(bar.rect.min.y, flat_bar_y(level), "{channel:?}");
            }
        }
    }

    #[test]
    fn clearing_visual_state_rebuilds_the_same_bars_from_history() {
        let mut fixture = PcmFixture::new(2);
        let mut module = waveform(&fixture, None);
        let mut target = RecordingTarget::default();
        fixture.feed(&repeat_frame(&[0.5, 0.5], 2 * BLOCK_FRAMES));
        draw(module.as_mut(), &mut target, Duration::ZERO);
        let before = target.rects.clone();
        assert!(!before.is_empty());

        module.clear_visual_state();
        target.clear();
        draw(module.as_mut(), &mut target, Duration::from_millis(50));
        assert_eq!(target.rects, before);
    }
}
