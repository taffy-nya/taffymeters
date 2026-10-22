use crate::{
    audio::{SourceReader, SourceState},
    dataflow::{
        ChannelSelection, DataController, DataSpec, DataSubscription, FftConfig, Keep, NodeRef,
        NodeSpec, OutputId, SOURCE,
    },
    extensions::{Registry, RegistryError},
    module::{
        FLOW_KEY, MIN_SCROLL_POINTS, MIN_ZOOM_CHANGE, Module, ModuleError, ModuleFactory,
        ModuleFrame, ModuleResponse, ModuleServices, VISIBLE_SECONDS_KEY, flow_setting, flow_value,
        number_setting, spec_from_saved, visible_range, visible_seconds_setting,
    },
    plotting::{ColumnRing, ViewClock, spectrum_dbfs_between},
    render::{Color, Flow},
    settings::{SettingKind, SettingValue, ViewSetting, unit},
    snapshot::{DataReader, HistoryView},
    theme::ColorRole,
};

const HISTORY_SECONDS: f32 = 8.0;
const MIN_VISIBLE_SECONDS: f32 = 0.5;
const DEFAULT_VISIBLE_SECONDS: f32 = 6.0;
/// Matches the default Spectrum's 90 dB below the workspace reference.
const LEVEL_SPAN_DB: f32 = 90.0;
const MIN_FREQUENCY_HZ: f32 = 20.0;
const GAIN_DB_RANGE: (f32, f32) = (-24.0, 24.0);
/// Wheel gain per scroll point (a wheel notch scrolls about 36 points): the
/// zoom pace of the other views is a fraction of a dB here, too slow to see.
const WHEEL_GAIN_DB_PER_POINT: f32 = 0.05;
const GAIN_DB_KEY: &str = "gain_db";

pub const SPECTROGRAM_MODULE_ID: &str = "taffymeters.spectrogram";

const COLOR_ROLES: &[ColorRole] = &[ColorRole::Low, ColorRole::Mid, ColorRole::High];

pub(super) fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register_module(SpectrogramFactory)
}

struct SpectrogramFactory;

impl ModuleFactory for SpectrogramFactory {
    fn type_id(&self) -> &'static str {
        SPECTROGRAM_MODULE_ID
    }

    fn display_name(&self) -> &'static str {
        "Spectrogram"
    }

    fn create(
        &self,
        services: &ModuleServices,
        _registry: &Registry,
        data: Option<&DataSpec>,
    ) -> Result<Box<dyn Module>, RegistryError> {
        let (spec, channel, history) = spectrogram_spec();
        let spec = spec_from_saved(spec, data)?;
        let subscription = services
            .dataflow
            .subscribe(spec)
            .map_err(ModuleError::from)?;
        let mut module = SpectrogramModule {
            source: services.source.clone(),
            controller: subscription.controller(),
            reader: subscription.reader(),
            _subscription: subscription,
            channel_node: channel,
            history_output: history,
            history: HistoryView::new(),
            visible_seconds: DEFAULT_VISIBLE_SECONDS,
            gain_db: 0.0,
            flow: Flow::default(),
            picture: ColumnRing::default(),
            picture_key: None,
            rows: Vec::new(),
            column: Vec::new(),
            written_through: 0,
            clock: ViewClock::default(),
        };
        module.visible_seconds = module.visible_seconds.min(module.visible_range().1);
        Ok(Box::new(module))
    }

    fn default_data(&self) -> Option<DataSpec> {
        Some(spectrogram_spec().0)
    }

    fn color_roles(&self) -> &'static [ColorRole] {
        COLOR_ROLES
    }
}

fn spectrogram_spec() -> (DataSpec, NodeRef, OutputId) {
    let mut spec = DataSpec::new();
    let channel = spec.add(SOURCE, NodeSpec::Channel(ChannelSelection::MonoAverage));
    let fft = spec.add(channel, NodeSpec::Fft(FftConfig::default()));
    let tilt = spec.add(
        fft,
        NodeSpec::Tilt {
            db_per_octave: super::DEFAULT_TILT_DB_PER_OCTAVE,
        },
    );
    let history = spec.output(tilt, Keep::Seconds(HISTORY_SECONDS));
    (spec, channel, history)
}

struct SpectrogramModule {
    source: SourceReader,
    controller: DataController,
    reader: DataReader,
    _subscription: DataSubscription,
    channel_node: NodeRef,
    history_output: OutputId,
    history: HistoryView,
    visible_seconds: f32,
    gain_db: f32,
    flow: Flow,
    picture: ColumnRing,
    picture_key: Option<PictureKey>,
    /// Inclusive frequency bounds in Hz for each row, highest frequencies first.
    rows: Vec<(f32, f32)>,
    column: Vec<Color>,
    /// Exclusive source-frame frontier of the newest result in the picture.
    written_through: u64,
    clock: ViewClock,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PictureKey {
    columns: usize,
    rows: usize,
    hop: u64,
    max_frequency_bits: u32,
    /// Top of the color scale in dBFS (reference minus gain), stored as float bits.
    top_bits: u32,
    colors: [Color; 3],
}

impl SpectrogramModule {
    fn visible_range(&self) -> (f32, f32) {
        visible_range(
            &self.controller,
            self.history_output,
            HISTORY_SECONDS,
            MIN_VISIBLE_SECONDS,
        )
    }
}

impl Module for SpectrogramModule {
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
        (output == self.history_output).then_some("History")
    }

    fn color_roles(&self) -> &[ColorRole] {
        COLOR_ROLES
    }

    fn view_settings(&self) -> Vec<ViewSetting> {
        vec![
            visible_seconds_setting(
                self.visible_seconds,
                self.visible_range(),
                DEFAULT_VISIBLE_SECONDS,
            ),
            ViewSetting {
                key: GAIN_DB_KEY,
                label: "Gain",
                section: "Display",
                kind: SettingKind::Number {
                    min: GAIN_DB_RANGE.0,
                    max: GAIN_DB_RANGE.1,
                    unit: unit::DB,
                    logarithmic: false,
                },
                value: SettingValue::Number(self.gain_db),
                default: SettingValue::Number(0.0),
            },
            flow_setting(self.flow),
        ]
    }

    fn set_view_setting(&mut self, key: &str, value: &SettingValue) -> Result<(), ModuleError> {
        match key {
            VISIBLE_SECONDS_KEY => {
                self.visible_seconds = number_setting(key, value, self.visible_range())?;
            }
            GAIN_DB_KEY => self.gain_db = number_setting(key, value, GAIN_DB_RANGE)?,
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
                self.visible_seconds = (self.visible_seconds / frame.zoom_delta).clamp(min, max);
            }
            if frame.scroll_delta_y.abs() > MIN_SCROLL_POINTS {
                self.gain_db = (self.gain_db + frame.scroll_delta_y * WHEEL_GAIN_DB_PER_POINT)
                    .clamp(GAIN_DB_RANGE.0, GAIN_DB_RANGE.1);
            }
        }
        self.reader
            .sync_history(self.history_output, &mut self.history);
        let window_end = self.history.newest_frontier();
        let history = self.history.frames();
        let (Some(newest), Some(window_end)) = (
            history.last().and_then(|entry| entry.frame.spectrum()),
            window_end,
        ) else {
            return ModuleResponse::default();
        };
        let sample_rate = self.reader.descriptor().sample_rate_hz as f32;
        let paused = self.source.source_status().state == SourceState::Paused;
        let now_end = self
            .clock
            .update(window_end, frame.monotonic_time, sample_rate, paused);
        let window_frames = (self.visible_seconds * sample_rate).round().max(1.0) as u64;
        // Use the newest hop; older, wider hops can cover multiple columns after a hop-size change.
        let hop = newest.fft.hop_size.max(1) as u64;
        let max_frequency = (sample_rate * 0.5).min(20_000.0);
        let key = PictureKey {
            columns: window_frames.div_ceil(hop).max(1) as usize,
            rows: self.flow.extents(frame.rect).1.round().max(1.0) as usize,
            hop,
            max_frequency_bits: max_frequency.to_bits(),
            top_bits: (frame.reference_dbfs - self.gain_db).to_bits(),
            colors: [ColorRole::Low, ColorRole::Mid, ColorRole::High]
                .map(|role| frame.palette.color(role)),
        };
        if self.picture_key != Some(key) || window_end < self.written_through {
            self.picture_key = Some(key);
            self.picture.reset(key.columns, key.rows);
            self.rows.clear();
            self.rows.extend((0..key.rows).map(|row| {
                let at = |fraction: f32| {
                    MIN_FREQUENCY_HZ * (max_frequency / MIN_FREQUENCY_HZ).powf(fraction)
                };
                let top = 1.0 - row as f32 / key.rows as f32;
                let bottom = 1.0 - (row + 1) as f32 / key.rows as f32;
                (at(bottom), at(top))
            }));
            self.written_through = window_end.saturating_sub(window_frames + hop);
        }
        let start = history
            .partition_point(|entry| entry.frame.processed_through_frame <= self.written_through);
        let top_dbfs = frame.reference_dbfs - self.gain_db;
        for entry in &history[start..] {
            let Some(data) = entry.frame.spectrum() else {
                continue;
            };
            let column = self
                .clock
                .frontier_position(entry.frame.processed_through_frame)
                / hop;
            self.column.clear();
            self.column.extend(self.rows.iter().map(|&(low, high)| {
                let Some(dbfs) = spectrum_dbfs_between(&data.bins_dbfs, data.fft.bin_hz, low, high)
                else {
                    return Color::rgba(0, 0, 0, 0);
                };
                frame.palette.heat((dbfs - top_dbfs) / LEVEL_SPAN_DB + 1.0)
            }));
            // Repeat wider hops across skipped columns, but leave History gaps blank.
            self.picture.write(column, &self.column, !entry.gap_before);
            self.written_through = entry.frame.processed_through_frame;
        }
        self.picture.advance(now_end / hop);
        self.picture
            .draw(frame.renderer, frame.rect, frame.rect, self.flow);
        ModuleResponse {
            repaint_after: self.clock.repaint_after(
                frame.monotonic_time,
                self.clock.frontier_position(window_end),
                key.columns as u64 * hop,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::modules::test_support::{PcmFixture, RecordingTarget, TEST_RECT, draw};

    fn tone(start: usize, frames: usize) -> Vec<f32> {
        (start..start + frames)
            .map(|frame| 0.25 * (std::f32::consts::TAU * 3_000.0 * frame as f32 / 48_000.0).sin())
            .collect()
    }

    fn row_of(frequency_hz: f32, rows: usize) -> usize {
        let fraction = (frequency_hz / MIN_FREQUENCY_HZ).ln() / (20_000.0 / MIN_FREQUENCY_HZ).ln();
        ((1.0 - fraction) * rows as f32) as usize
    }

    #[test]
    fn the_default_visible_time_starts_within_a_short_history() {
        let fixture = PcmFixture::new(1);
        let (mut spec, _, history) = spectrogram_spec();
        spec.set_keep(history, Keep::Seconds(3.0));
        let module = SpectrogramFactory
            .create(&fixture.services, &Registry::default(), Some(&spec))
            .unwrap();
        let setting = module
            .view_settings()
            .into_iter()
            .find(|setting| setting.key == VISIBLE_SECONDS_KEY)
            .unwrap();
        assert_eq!(setting.value, SettingValue::Number(3.0));
    }

    #[test]
    fn each_result_writes_one_column_once_and_the_picture_only_scrolls() {
        let mut fixture = PcmFixture::new(1);
        let mut module = SpectrogramFactory
            .create(&fixture.services, &Registry::default(), None)
            .unwrap();
        let mut target = RecordingTarget::default();
        // One FFT window plus four hops yields five results, at columns 4..=8.
        let mut fed = 2_048 + 4 * 512;
        fixture.feed(&tone(0, fed));
        draw(module.as_mut(), &mut target, Duration::ZERO);
        let first = target.images.pop().expect("the picture is drawn");
        assert_eq!(first.height, TEST_RECT.height() as usize);
        // 6 s at 48 kHz in 512-frame hops.
        assert_eq!(first.width, 563);
        assert_eq!(first.rect, TEST_RECT);
        assert_eq!(first.changed, vec![4, 5, 6, 7, 8]);
        // The oldest edge starts after newest slot 8, keeping the newest column at the right edge.
        assert_eq!(first.first_column, 9);
        let newest = 8;
        let rows = first.height;
        let tone_row = &first.columns[newest][row_of(3_000.0, rows)];
        let bass_row = &first.columns[newest][row_of(50.0, rows)];
        assert!(tone_row.alpha > 200, "the 3 kHz tone shows: {tone_row:?}");
        assert_eq!(bass_row.alpha, 0, "silence at 50 Hz stays background");

        draw(module.as_mut(), &mut target, Duration::from_millis(16));
        let again = target.images.pop().unwrap();
        assert!(
            again.changed.is_empty(),
            "redrawing rewrote {:?}",
            again.changed
        );
        assert_eq!(again.first_column, first.first_column);
        assert_eq!(again.columns, first.columns);

        fixture.feed(&tone(fed, 512));
        fed += 512;
        draw(module.as_mut(), &mut target, Duration::from_millis(32));
        let scrolled = target.images.pop().unwrap();
        assert_eq!(scrolled.changed, vec![newest + 1]);
        for (x, column) in scrolled.columns.iter().enumerate() {
            if x != newest + 1 {
                assert_eq!(column, &first.columns[x], "column {x} changed");
            }
        }
        assert!(fed > 0);
    }

    #[test]
    fn gain_brightens_the_picture_without_changing_the_analysis() {
        let mut fixture = PcmFixture::new(1);
        let mut module = SpectrogramFactory
            .create(&fixture.services, &Registry::default(), None)
            .unwrap();
        // A faint tone (about -79 dB as shown): dim at 0 dB gain.
        let faint: Vec<f32> = tone(0, 4_096)
            .iter()
            .map(|sample| sample * 0.000_2)
            .collect();
        fixture.feed(&faint);
        let mut target = RecordingTarget::default();
        draw(module.as_mut(), &mut target, Duration::ZERO);
        let dim = target.images.pop().unwrap();
        let row = row_of(3_000.0, dim.height);
        let spec = module.data().unwrap().spec();

        module
            .set_view_setting("gain_db", &SettingValue::Number(24.0))
            .unwrap();
        // Do not feed new audio: a gain edit must also recolor existing History.
        draw(module.as_mut(), &mut target, Duration::from_millis(16));
        let bright = target.images.pop().unwrap();
        let (before, after) = (dim.columns[8][row].alpha, bright.columns[8][row].alpha);
        assert!(before < 200 && after > before, "alpha {before} -> {after}");
        assert_eq!(
            module.data().unwrap().spec(),
            spec,
            "the analysis is untouched"
        );
    }

    #[test]
    fn a_silent_source_keeps_the_picture_moving_until_it_has_scrolled_out() {
        let mut fixture = PcmFixture::new(1);
        let mut module = SpectrogramFactory
            .create(&fixture.services, &Registry::default(), None)
            .unwrap();
        fixture.feed(&tone(0, 2_048 + 4 * 512));
        let mut target = RecordingTarget::default();
        draw(module.as_mut(), &mut target, Duration::ZERO);
        let start = target.images.pop().unwrap();

        // A second without results: about 0.85 s past the grace, 80 hops of 512.
        let wake = draw(module.as_mut(), &mut target, Duration::from_secs(1)).repaint_after;
        let moved = target.images.pop().unwrap();
        let shift = (moved.first_column + moved.width - start.first_column) % moved.width;
        assert!((78..=81).contains(&shift), "moved {shift} columns");
        assert_eq!(wake, Some(crate::module::FRAME_INTERVAL));

        let wake = draw(module.as_mut(), &mut target, Duration::from_secs(8)).repaint_after;
        let gone = target.images.pop().unwrap();
        assert!(gone.columns.iter().flatten().all(|pixel| pixel.alpha == 0));
        assert_eq!(wake, None);
    }
}
