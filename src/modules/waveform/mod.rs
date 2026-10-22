use std::collections::VecDeque;

use crate::{
    audio::{Generation, SourceId, SourceReader, SourceState},
    dataflow::{ChannelSelection, DataController, DataSpec, DataSubscription, Keep, OutputId},
    extensions::{Registry, RegistryError},
    module::{
        FLOW_KEY, MIN_ZOOM_CHANGE, Module, ModuleError, ModuleFactory, ModuleFrame, ModuleResponse,
        ModuleServices, VISIBLE_SECONDS_KEY, flow_setting, flow_value, number_setting,
        reference_gain, visible_range, visible_seconds_setting,
    },
    plotting::{ColumnRing, ViewClock},
    render::{Color, Flow},
    settings::{SettingValue, ViewSetting},
    snapshot::{DataReader, HistoryView},
    theme::ColorRole,
};

use crate::modules::pcm_view::{
    CHANNEL_KEY, SCALE_KEY, SCALE_RANGE, channel_setting, channel_value, pcm_spec, scale_setting,
    subscribe_pcm, wheel_scale,
};

const DEFAULT_VISIBLE_SECONDS: f32 = 4.0;

const WAVEFORM_HISTORY_SECONDS: f32 = 10.0;
const WAVEFORM_MIN_SECONDS: f32 = 0.25;

pub const WAVEFORM_MODULE_ID: &str = "taffymeters.waveform";

const COLOR_ROLES: &[ColorRole] = &[ColorRole::Low, ColorRole::Mid, ColorRole::High];

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
        let mut module = WaveformModule {
            source: services.source.clone(),
            controller: subscription.controller(),
            reader: subscription.reader(),
            _subscription: subscription,
            output,
            history: HistoryView::new(),
            channel: ChannelSelection::MonoAverage,
            seconds: DEFAULT_VISIBLE_SECONDS,
            y_scale: 1.0,
            flow: Flow::default(),
            envelope: WaveformEnvelope::default(),
            picture: ColumnRing::default(),
            picture_key: None,
            column: Vec::new(),
            clock: ViewClock::default(),
        };
        module.seconds = module.seconds.min(module.visible_range().1);
        Ok(Box::new(module))
    }

    fn default_data(&self) -> Option<DataSpec> {
        Some(pcm_spec(Keep::Seconds(WAVEFORM_HISTORY_SECONDS)).0)
    }

    fn color_roles(&self) -> &'static [ColorRole] {
        COLOR_ROLES
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
    flow: Flow,
    envelope: WaveformEnvelope,
    /// Completed bins are rasterized once; the filling bin can change on each publication.
    picture: ColumnRing,
    picture_key: Option<PictureKey>,
    column: Vec<Color>,
    clock: ViewClock,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PictureKey {
    columns: usize,
    rows: usize,
    frames_per_bin: u64,
    gain_bits: u32,
    colors: [Color; 3],
}

impl WaveformModule {
    fn visible_range(&self) -> (f32, f32) {
        visible_range(
            &self.controller,
            self.output,
            WAVEFORM_HISTORY_SECONDS,
            WAVEFORM_MIN_SECONDS,
        )
    }
}

/// Cache absolute source-frame bins so draws only fold in newly published samples.
#[derive(Default)]
struct WaveformEnvelope {
    key: Option<(u64, ChannelSelection, SourceId, Generation)>,
    /// Exclusive source-frame frontier already folded in.
    through: u64,
    /// `(bin, min, max)`, bins ascending; bins without samples are absent (gaps).
    bins: VecDeque<(u64, f32, f32)>,
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

    fn data(&self) -> Option<&DataController> {
        Some(&self.controller)
    }

    fn output_label(&self, output: OutputId) -> Option<&'static str> {
        (output == self.output).then_some("History")
    }

    fn color_roles(&self) -> &[ColorRole] {
        COLOR_ROLES
    }

    fn view_settings(&self) -> Vec<ViewSetting> {
        vec![
            channel_setting(self.channel),
            visible_seconds_setting(self.seconds, self.visible_range(), DEFAULT_VISIBLE_SECONDS),
            scale_setting(self.y_scale),
            flow_setting(self.flow),
        ]
    }

    fn set_view_setting(&mut self, key: &str, value: &SettingValue) -> Result<(), ModuleError> {
        match key {
            CHANNEL_KEY => return self.set_channel(channel_value(key, value)?),
            VISIBLE_SECONDS_KEY => {
                self.seconds = number_setting(key, value, self.visible_range())?;
            }
            SCALE_KEY => self.y_scale = number_setting(key, value, SCALE_RANGE)?,
            FLOW_KEY => self.flow = flow_value(key, value)?,
            _ => return Err(ModuleError::UnknownSetting(key.to_owned())),
        }
        Ok(())
    }

    fn draw(&mut self, frame: ModuleFrame<'_>) -> ModuleResponse {
        if frame
            .pointer
            .is_some_and(|point| frame.rect.contains(point))
        {
            if (frame.zoom_delta - 1.0).abs() > MIN_ZOOM_CHANGE {
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
        let (along, across) = self.flow.extents(frame.rect);
        let columns = along.floor().max(1.0) as u64;
        let span_frames = ((self.seconds * descriptor.sample_rate_hz as f32) as u64).max(1);
        let frames_per_bin = (span_frames / columns).max(1);
        let paused = self.source.source_status().state == SourceState::Paused;
        let now_end = self.clock.update(
            newest,
            frame.monotonic_time,
            descriptor.sample_rate_hz as f32,
            paused,
        );
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
        let gain = self.y_scale * reference_gain(frame.reference_dbfs);
        let picture_key = PictureKey {
            columns: columns as usize,
            rows: across.round().max(1.0) as usize,
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
                // Quantize inserted silence to whole columns so settled bins do not shimmer.
                let shift = self.clock.position(bin * frames_per_bin) - bin * frames_per_bin;
                self.picture
                    .write(bin + shift / frames_per_bin, &self.column, false);
            }
        }
        let shift = now_end - newest;
        self.picture.advance(last_bin + shift / frames_per_bin);
        self.picture
            .draw(frame.renderer, frame.rect, frame.rect, self.flow);
        ModuleResponse {
            repaint_after: self.clock.repaint_after(
                frame.monotonic_time,
                self.clock.frontier_position(newest),
                columns * frames_per_bin,
            ),
        }
    }
}

/// Scaled samples ±1 map to the edges; the bar spans at least one row before clipping.
/// Partial row coverage avoids steps under bilinear filtering.
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
        settings::SettingKind,
    };

    fn waveform(fixture: &PcmFixture, data: Option<&DataSpec>) -> Box<dyn Module> {
        WaveformFactory
            .create(&fixture.services, &Registry::default(), data)
            .unwrap()
    }

    fn history_of(seconds: f32) -> DataSpec {
        let mut spec = DataSpec::new();
        spec.output(SOURCE, Keep::Seconds(seconds));
        spec
    }

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
        assert_eq!(
            visible_time(short.as_ref()),
            (WAVEFORM_MIN_SECONDS, 2.0, 2.0),
            "the default span starts within the History"
        );
        set_visible_time(short.as_mut(), 4.0);
        assert_eq!(
            visible_time(short.as_ref()).2,
            2.0,
            "never beyond the History"
        );

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
    fn history_shorter_than_the_visible_span_enters_at_the_right_edge() {
        let mut fixture = PcmFixture::new(2);
        let mut module = waveform(&fixture, None);
        let screen =
            |image: &RecordedImage, x: usize| (x + image.width - image.first_column) % image.width;

        // A single block occupies only 1/30 s of the default 4 s window.
        fixture.feed(&repeat_frame(&[0.5, 0.5], BLOCK_FRAMES));
        let first = drawn(module.as_mut(), 0);
        assert_eq!(first.rect, TEST_RECT);
        let first_bars = bars(&first);
        assert!(!first_bars.is_empty());
        let positions: Vec<usize> = first_bars.iter().map(|bar| screen(&first, bar.0)).collect();
        let expected: Vec<usize> = (first.width - first_bars.len()..first.width).collect();
        assert_eq!(
            positions, expected,
            "bars are contiguous up to the right edge"
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

        // The block boundary falls inside one bin, so only that existing column must be redrawn.
        fixture.feed(&repeat_frame(&[-0.5, -0.5], BLOCK_FRAMES));
        let second = drawn(module.as_mut(), 50);
        let second_bars = bars(&second);
        assert!(
            second_bars.len() > first_bars.len(),
            "the picture grows, newest on the right"
        );
        assert!(second_bars.iter().enumerate().all(|(i, bar)| bar.0 == i));
        assert_eq!(
            screen(&second, second_bars.last().unwrap().0),
            second.width - 1
        );
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

        assert!(drawn(module.as_mut(), 66).changed.is_empty());
    }

    #[test]
    fn changing_channel_redraws_the_envelope_from_the_new_channel() {
        let mut fixture = PcmFixture::new(2);
        let mut module = waveform(&fixture, None);
        fixture.feed(&repeat_frame(&[0.5, -0.5], BLOCK_FRAMES));

        let silent = bars(&drawn(module.as_mut(), 0));
        assert_eq!(silent.len(), 4);
        for bar in &silent {
            assert_eq!((bar.1, bar.2), (level_row(0.0), level_row(0.0)));
        }

        // Do not feed new audio: the channel edit must also rebuild existing History.
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
    fn a_vertical_flow_lays_time_along_the_height_and_the_bar_across_the_width() {
        let mut fixture = PcmFixture::new(2);
        let mut module = waveform(&fixture, None);
        module
            .set_view_setting("flow", &SettingValue::Choice("up".to_owned()))
            .unwrap();
        fixture.feed(&repeat_frame(&[0.5, 0.5], BLOCK_FRAMES));
        let picture = drawn(module.as_mut(), 0);
        assert_eq!(picture.flow, Flow::Up);
        assert_eq!(
            (picture.width, picture.height),
            (TEST_RECT.height() as usize, TEST_RECT.width() as usize),
            "one column per point of height, a row per point of width"
        );
        // 1/30 s in a 4 s window over 200 columns occupies two columns.
        let half = TEST_RECT.width() * 0.5;
        let row = (half - 0.5 * crate::module::REFERENCE_HEIGHT * half).round() as usize;
        let shown = bars(&picture);
        assert_eq!(
            shown.iter().map(|bar| bar.0).collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert!(
            shown.iter().all(|bar| (bar.1, bar.2) == (row, row)),
            "{shown:?}"
        );
        assert!(
            module
                .set_view_setting("flow", &SettingValue::Choice("sideways".to_owned()))
                .is_err()
        );
    }

    #[test]
    fn a_silent_source_keeps_the_picture_moving_until_it_has_scrolled_out() {
        let mut fixture = PcmFixture::new(2);
        let mut module = waveform(&fixture, None);
        let screen =
            |image: &RecordedImage, x: usize| (x + image.width - image.first_column) % image.width;
        let at = |module: &mut dyn Module, millis: u64| {
            let mut target = RecordingTarget::default();
            let response = draw(module, &mut target, Duration::from_millis(millis));
            (target.images.pop().unwrap(), response.repaint_after)
        };
        fixture.feed(&repeat_frame(&[0.5, 0.5], BLOCK_FRAMES));
        let (start, wake) = at(module.as_mut(), 0);
        let newest = bars(&start).last().unwrap().0;
        assert_eq!(screen(&start, newest), start.width - 1);
        assert!(wake.is_some_and(|wake| wake <= crate::plotting::SILENCE_GRACE));

        // 0.85 s past the grace at 100 columns/s should shift the picture by 85 columns.
        let (moved, wake) = at(module.as_mut(), 1_000);
        let shift = start.width - 1 - screen(&moved, newest);
        assert!((84..=86).contains(&shift), "moved {shift} columns");
        assert_eq!(wake, Some(crate::module::FRAME_INTERVAL));

        let (gone, wake) = at(module.as_mut(), 5_000);
        assert!(bars(&gone).is_empty());
        assert_eq!(wake, None);

        fixture.feed(&repeat_frame(&[-0.5, -0.5], BLOCK_FRAMES));
        let (back, _) = at(module.as_mut(), 5_050);
        let shown = bars(&back);
        assert!(!shown.is_empty());
        assert_eq!(screen(&back, shown.last().unwrap().0), back.width - 1);
        assert!(
            shown
                .iter()
                .all(|bar| (bar.1, bar.2) == (level_row(-0.5), level_row(-0.5)))
        );
    }
}
