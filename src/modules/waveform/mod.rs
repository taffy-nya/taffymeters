use crate::{
    audio::SourceReader,
    dataflow::{ChannelSelection, DataController, DataSpec, DataSubscription, Keep, OutputId},
    extensions::{Registry, RegistryError},
    module::{
        Module, ModuleError, ModuleFactory, ModuleFrame, ModuleResponse, ModuleServices,
        number_setting, reference_gain,
    },
    plotting::ColumnRing,
    render::Color,
    settings::{SettingKind, SettingValue, ViewSetting},
    snapshot::{DataReader, HistoryView},
    theme::ColorRole,
};

use crate::modules::pcm_view::{
    SCALE_RANGE, channel_setting, channel_value, pcm_spec, scale_setting, subscribe_pcm,
    wheel_scale,
};

/// Visible span a new Waveform shows.
const DEFAULT_VISIBLE_SECONDS: f32 = 4.0;

/// Source time the Waveform keeps by default; it shows up to its History.
const WAVEFORM_HISTORY_SECONDS: f32 = 10.0;
/// Shortest Waveform span.
const WAVEFORM_MIN_SECONDS: f32 = 0.25;

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
            envelope: WaveformEnvelope::default(),
            picture: ColumnRing::default(),
            picture_key: None,
            column: Vec::new(),
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
    envelope: WaveformEnvelope,
    /// One column per envelope bin, each rasterized once (the newest again while
    /// it fills).
    picture: ColumnRing,
    /// What the pixels were rasterized for; any change redraws them from the
    /// envelope.
    picture_key: Option<PictureKey>,
    /// Pixels of one column, reused while rasterizing.
    column: Vec<Color>,
}

/// Inputs of the picture besides the envelope.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PictureKey {
    columns: usize,
    rows: usize,
    frames_per_bin: u64,
    gain_bits: u32,
    colors: [Color; 3],
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
        self.envelope = WaveformEnvelope::default();
        self.picture_key = None;
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
            scale_setting(self.y_scale),
        ]
    }

    fn set_view_setting(&mut self, key: &str, value: &SettingValue) -> Result<(), ModuleError> {
        match key {
            "channel" => return self.set_channel(channel_value(key, value)?),
            "visible_seconds" => {
                self.seconds = number_setting(key, value, self.visible_range())?;
            }
            "scale" => self.y_scale = number_setting(key, value, SCALE_RANGE)?,
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
            self.y_scale = wheel_scale(self.y_scale, &frame);
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
        let mut rebuild = false;
        if envelope.key != Some(key) || newest < envelope.through {
            *envelope = WaveformEnvelope {
                key: Some(key),
                ..WaveformEnvelope::default()
            };
            rebuild = true;
        }
        let last_bin = newest.saturating_sub(1) / frames_per_bin;
        let first_bin = (last_bin + 1).saturating_sub(columns);
        let channels = descriptor.channel_count as usize;
        // First bin that received samples in this draw.
        let mut changed_from = None;
        for chunk in self.history.iter().filter_map(|entry| entry.frame.pcm()) {
            let chunk_end = chunk.start_frame.saturating_add(chunk.frame_count as u64);
            let start = chunk
                .start_frame
                .max(envelope.through)
                .max(first_bin * frames_per_bin);
            if chunk_end <= start {
                continue;
            }
            changed_from.get_or_insert(start / frames_per_bin);
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
        // The Scale setting is relative to the workspace reference level.
        let gain = self.y_scale * reference_gain(frame.reference_dbfs);
        let picture_key = PictureKey {
            columns: columns as usize,
            rows: frame.rect.height().round().max(1.0) as usize,
            frames_per_bin,
            gain_bits: gain.to_bits(),
            colors: [ColorRole::Low, ColorRole::Mid, ColorRole::High]
                .map(|role| frame.palette.color(role)),
        };
        if rebuild || self.picture_key != Some(picture_key) {
            self.picture_key = Some(picture_key);
            self.picture.reset(picture_key.columns, picture_key.rows);
            changed_from = Some(0);
        }
        if let Some(from) = changed_from {
            let start = envelope.bins.partition_point(|(bin, _, _)| *bin < from);
            for &(bin, minimum, maximum) in envelope.bins.range(start..) {
                rasterize_column(
                    &mut self.column,
                    picture_key.rows,
                    minimum * gain,
                    maximum * gain,
                    frame.palette,
                );
                // Bins without samples (gaps) stay blank.
                self.picture.write(bin, &self.column, false);
            }
        }
        self.picture.draw(frame.renderer, frame.rect, frame.rect);
        ModuleResponse::default()
    }
}

/// One Waveform column: the bar from `maximum` down to `minimum` (scaled samples,
/// ±1 at the edges), at least one row tall, edge rows by their coverage so the
/// smooth filtering shows no steps.
fn rasterize_column(
    pixels: &mut Vec<Color>,
    rows: usize,
    minimum: f32,
    maximum: f32,
    palette: &crate::theme::Palette,
) {
    let half = rows as f32 * 0.5;
    let top = half - maximum.clamp(-1.0, 1.0) * half;
    let bottom = (half - minimum.clamp(-1.0, 1.0) * half).max(top + 1.0);
    let color = palette.level(maximum.abs().max(minimum.abs()).clamp(0.0, 1.0));
    pixels.clear();
    pixels.extend((0..rows).map(|row| {
        let coverage = (bottom.min(row as f32 + 1.0) - top.max(row as f32)).clamp(0.0, 1.0);
        color.with_alpha((f32::from(color.alpha) * coverage).round() as u8)
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::{
        dataflow::SOURCE,
        modules::test_support::{
            BLOCK_FRAMES, PcmFixture, RecordedImage, RecordingTarget, TEST_RECT, draw, repeat_frame,
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

    /// Picture row of the top of a bar of constant sample `level` (the test frames'
    /// full-scale reference draws 1.0 at `REFERENCE_HEIGHT` of the half height).
    fn level_row(level: f32) -> usize {
        let half = TEST_RECT.height() * 0.5;
        (half - level * crate::module::REFERENCE_HEIGHT * half).round() as usize
    }

    /// `(column, first row, last row)` of every column that shows a bar, by column.
    fn bars(image: &RecordedImage) -> Vec<(usize, usize, usize)> {
        image
            .columns
            .iter()
            .enumerate()
            .filter_map(|(x, column)| {
                let first = column.iter().position(|pixel| pixel.alpha > 0)?;
                let last = column.iter().rposition(|pixel| pixel.alpha > 0)?;
                Some((x, first, last))
            })
            .collect()
    }

    fn drawn(module: &mut dyn Module, millis: u64) -> RecordedImage {
        let mut target = RecordingTarget::default();
        draw(module, &mut target, Duration::from_millis(millis));
        target.images.pop().expect("the picture is drawn")
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

        // 1/30 s of a constant 0.5 in the default 4 s window: a thin sliver at the
        // left edge, not stretched across the Module.
        fixture.feed(&repeat_frame(&[0.5, 0.5], BLOCK_FRAMES));
        let first = drawn(module.as_mut(), 0);
        assert_eq!(first.rect, TEST_RECT);
        assert_eq!(first.first_column, 0);
        let first_bars = bars(&first);
        assert!(!first_bars.is_empty());
        assert!(
            first_bars.iter().enumerate().all(|(i, bar)| bar.0 == i),
            "bars are contiguous from the left edge: {first_bars:?}"
        );
        assert!(
            first_bars.len() < first.width / 4,
            "{} of {} columns covered by 1/30 s of a 4 s window",
            first_bars.len(),
            first.width
        );
        for bar in &first_bars {
            assert_eq!((bar.1, bar.2), (level_row(0.5), level_row(0.5)));
        }

        // The next 1/30 s (constant -0.5) extends the picture to the right without
        // touching what was already drawn. The block boundary falls inside a bar,
        // so that bar holds both levels and is drawn again; nothing else is.
        fixture.feed(&repeat_frame(&[-0.5, -0.5], BLOCK_FRAMES));
        let second = drawn(module.as_mut(), 50);
        let second_bars = bars(&second);
        assert!(
            second_bars.len() > first_bars.len(),
            "the picture grows to the right"
        );
        assert!(second_bars.iter().enumerate().all(|(i, bar)| bar.0 == i));
        let settled = first_bars.len() - 1;
        assert_eq!(
            &second_bars[..settled],
            &first_bars[..settled],
            "earlier bars kept"
        );
        assert!(
            second.changed.iter().all(|&x| x >= settled),
            "{:?}",
            second.changed
        );
        let both_levels = second_bars
            .iter()
            .filter(|bar| bar.1 == level_row(0.5) && bar.2 == level_row(-0.5) - 1)
            .count();
        assert_eq!(both_levels, 1, "one bar spans both halves");
        let last = second_bars.last().unwrap();
        assert_eq!((last.1, last.2), (level_row(-0.5), level_row(-0.5)));

        // Nothing new: nothing is drawn again.
        assert!(drawn(module.as_mut(), 66).changed.is_empty());
    }

    #[test]
    fn changing_channel_redraws_the_envelope_from_the_new_channel() {
        let mut fixture = PcmFixture::new(2);
        let mut module = waveform(&fixture, None);
        fixture.feed(&repeat_frame(&[0.5, -0.5], BLOCK_FRAMES));

        // Mono mix of +0.5 / -0.5 is silence: a flat line at the center.
        let silent = bars(&drawn(module.as_mut(), 0));
        assert_eq!(silent.len(), 4);
        for bar in &silent {
            assert_eq!((bar.1, bar.2), (level_row(0.0), level_row(0.0)));
        }

        // No new audio arrived: the bars must still be rebuilt for the new channel.
        for (channel, level) in [
            (ChannelSelection::Left, 0.5),
            (ChannelSelection::Right, -0.5),
        ] {
            module.set_channel(channel).unwrap();
            let shown = bars(&drawn(module.as_mut(), 50));
            assert_eq!(shown.len(), 4, "{channel:?}");
            for bar in &shown {
                assert_eq!(
                    (bar.1, bar.2),
                    (level_row(level), level_row(level)),
                    "{channel:?}"
                );
            }
        }
    }

    #[test]
    fn clearing_visual_state_rebuilds_the_same_picture_from_history() {
        let mut fixture = PcmFixture::new(2);
        let mut module = waveform(&fixture, None);
        fixture.feed(&repeat_frame(&[0.5, 0.5], 2 * BLOCK_FRAMES));
        let before = drawn(module.as_mut(), 0);
        assert!(!bars(&before).is_empty());

        module.clear_visual_state();
        let after = drawn(module.as_mut(), 50);
        assert_eq!(after.columns, before.columns);
        assert_eq!(after.first_column, before.first_column);
    }
}
