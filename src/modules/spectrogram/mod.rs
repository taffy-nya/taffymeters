use crate::{
    audio::SourceReader,
    dataflow::{
        ChannelSelection, DataController, DataSpec, DataSubscription, FftConfig, Keep, NodeRef,
        NodeSpec, OutputId, SOURCE,
    },
    extensions::{Registry, RegistryError},
    module::{
        Module, ModuleError, ModuleFactory, ModuleFrame, ModuleResponse, ModuleServices,
        number_setting, spec_from_saved,
    },
    plotting::SpectrumCoordinates,
    render::{ColoredRect, Point, Rect},
    settings::{SettingKind, SettingValue, ViewSetting},
    snapshot::{DataReader, HistoryView, ViewFrame},
    theme::ColorRole,
};

/// Source time the Spectrogram keeps by default; it shows up to its History.
const HISTORY_SECONDS: f32 = 8.0;
/// Shortest visible span.
const MIN_VISIBLE_SECONDS: f32 = 0.5;

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
        let mut spec = DataSpec::new();
        let channel = spec.add(SOURCE, NodeSpec::Channel(ChannelSelection::MonoAverage));
        let fft = spec.add(channel, NodeSpec::Fft(FftConfig::default()));
        let history = spec.output(fft, Keep::Seconds(HISTORY_SECONDS));
        let spec = spec_from_saved(spec, data)?;
        let subscription = services
            .dataflow
            .subscribe(spec)
            .map_err(ModuleError::from)?;
        Ok(Box::new(SpectrogramModule {
            source: services.source.clone(),
            controller: subscription.controller(),
            reader: subscription.reader(),
            _subscription: subscription,
            channel_node: channel,
            history_output: history,
            history: HistoryView::new(),
            visible_seconds: 6.0,
            cells: Vec::with_capacity(16_384),
            band_bins: Vec::with_capacity(96),
        }))
    }
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
    cells: Vec<ColoredRect>,
    /// FFT bin range `[first, end)` of each band, rebuilt per draw.
    band_bins: Vec<(usize, usize)>,
}

impl SpectrogramModule {
    /// Shortest and longest visible span: the view never shows more than the
    /// output's History, even a History shorter than the usual shortest span.
    fn visible_range(&self) -> (f32, f32) {
        let history = self.controller.with_spec(|spec| {
            match spec.outputs().get(self.history_output.index()) {
                Some(output) => match output.keep {
                    Keep::Seconds(seconds) => seconds,
                    Keep::Latest => HISTORY_SECONDS,
                },
                None => HISTORY_SECONDS,
            }
        });
        (MIN_VISIBLE_SECONDS.min(history), history)
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

    fn clear_visual_state(&mut self) {
        self.cells.clear();
    }

    fn data(&self) -> Option<&DataController> {
        Some(&self.controller)
    }

    fn output_label(&self, output: OutputId) -> Option<&'static str> {
        (output == self.history_output).then_some("History")
    }

    fn color_roles(&self) -> &'static [ColorRole] {
        COLOR_ROLES
    }

    fn view_settings(&self) -> Vec<ViewSetting> {
        let (min, max) = self.visible_range();
        vec![ViewSetting {
            key: "visible_seconds",
            label: "Visible time",
            section: "Display",
            kind: SettingKind::Number {
                min,
                max,
                unit: "s",
                logarithmic: false,
            },
            value: SettingValue::Number(self.visible_seconds),
        }]
    }

    fn set_view_setting(&mut self, key: &str, value: &SettingValue) -> Result<(), ModuleError> {
        if key != "visible_seconds" {
            return Err(ModuleError::UnknownSetting(key.to_owned()));
        }
        self.visible_seconds = number_setting(key, value, self.visible_range())?;
        Ok(())
    }

    fn draw(&mut self, frame: ModuleFrame<'_>) -> ModuleResponse {
        if frame
            .pointer
            .is_some_and(|point| frame.rect.contains(point))
            && (frame.zoom_delta - 1.0).abs() > 0.001
        {
            let (min, max) = self.visible_range();
            self.visible_seconds = (self.visible_seconds / frame.zoom_delta).clamp(min, max);
        }
        self.reader
            .sync_history(self.history_output, &mut self.history);
        let Some(window_end) = self.history.newest_frontier() else {
            return ModuleResponse::default();
        };
        let Some(fft) = self
            .history
            .iter()
            .find_map(|entry| entry.frame.spectrum())
            .map(|data| data.fft)
        else {
            return ModuleResponse::default();
        };

        let bands = (frame.rect.height() / 5.0).floor().clamp(16.0, 96.0) as usize;
        let sample_rate = self.reader.descriptor().sample_rate_hz as f32;
        let coordinates = SpectrumCoordinates {
            min_frequency_hz: 20.0,
            max_frequency_hz: (sample_rate * 0.5).min(20_000.0),
            min_dbfs: fft.floor_dbfs,
            max_dbfs: 0.0,
        };
        let window_frames = (self.visible_seconds * sample_rate).round().max(1.0) as u64;
        let first_frontier = window_end.saturating_sub(window_frames);
        let history = self.history.frames();
        let first_visible =
            history.partition_point(|entry| entry.frame.processed_through_frame < first_frontier);
        let visible = &history[first_visible..];
        let max_columns = (frame.rect.width() / 3.0).floor().max(1.0) as usize;
        let column_step = visible.len().div_ceil(max_columns).max(1);
        let band_height = frame.rect.height() / bands as f32;
        // Each band shows the loudest bin it covers (sampling one bin per band missed
        // narrow tones on the log axis). The ranges are the same for every column.
        let edge = |fraction: f32| {
            let x = frame.rect.min.x + fraction * frame.rect.width();
            coordinates.frequency_at_x(x, frame.rect) / fft.bin_hz
        };
        self.band_bins.clear();
        self.band_bins.extend((0..bands).map(|band| {
            let first = edge(band as f32 / bands as f32).floor() as usize;
            let end = (edge((band + 1) as f32 / bands as f32).ceil() as usize).max(first + 1);
            (first, end)
        }));
        self.cells.clear();
        for group in visible.chunks(column_step) {
            let Some((x0, x1, entry)) =
                history_column(group, frame.rect, window_end, window_frames, fft.hop_size)
            else {
                continue;
            };
            let Some(bins) = entry.frame.spectrum().map(|data| &data.bins_dbfs) else {
                continue;
            };
            for (band, &(first_bin, end_bin)) in self.band_bins.iter().enumerate() {
                let y0 = frame.rect.max.y - (band + 1) as f32 * band_height;
                let y1 = y0 + band_height + 0.5;
                let end_bin = end_bin.min(bins.len());
                let Some(dbfs) = bins
                    .get(first_bin..end_bin)
                    .and_then(|bins| bins.iter().copied().reduce(f32::max))
                else {
                    continue;
                };
                let normalized = ((dbfs - coordinates.min_dbfs)
                    / (coordinates.max_dbfs - coordinates.min_dbfs))
                    .clamp(0.0, 1.0);
                if normalized <= 0.015 {
                    continue;
                }
                self.cells.push(ColoredRect {
                    rect: Rect {
                        min: Point { x: x0, y: y0 },
                        max: Point { x: x1, y: y1 },
                    },
                    color: frame.palette.heat(normalized),
                });
            }
        }
        frame.renderer.fill_rects(frame.rect, &self.cells);
        ModuleResponse::default()
    }
}

fn history_column(
    group: &[ViewFrame],
    rect: Rect,
    window_end: u64,
    window_frames: u64,
    hop_size: usize,
) -> Option<(f32, f32, &ViewFrame)> {
    if group.is_empty() || group.iter().any(|entry| entry.gap_before) {
        return None;
    }
    let entry = group.last()?;
    let represented_frames = (hop_size as u64).saturating_mul(group.len() as u64);
    let column_start = entry
        .frame
        .processed_through_frame
        .saturating_sub(represented_frames);
    let x0 = x_for_frontier(rect, window_end, window_frames, column_start);
    let x1 = x_for_frontier(
        rect,
        window_end,
        window_frames,
        entry.frame.processed_through_frame,
    );
    Some((x0, (x1 + 0.5).max(x0 + 1.0).min(rect.max.x), entry))
}

fn x_for_frontier(rect: Rect, window_end: u64, window_frames: u64, frontier: u64) -> f32 {
    let age = window_end.saturating_sub(frontier).min(window_frames) as f32;
    rect.max.x - age / window_frames.max(1) as f32 * rect.width()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        dataflow::{FftMetadata, WindowFunction},
        snapshot::{Frame, FrameData, SpectrumData},
    };

    fn history_frame(frontier: u64, gap_before: bool) -> ViewFrame {
        ViewFrame {
            frame: Arc::new(Frame {
                result_version: frontier,
                processed_through_frame: frontier,
                source_sequence: 0,
                discontinuity_count: 0,
                discontinuity_before: gap_before,
                data: FrameData::Spectrum(SpectrumData {
                    fft: FftMetadata {
                        window: WindowFunction::PeriodicHann,
                        window_size: 2048,
                        hop_size: 512,
                        bin_hz: 48_000.0 / 2048.0,
                        floor_dbfs: -160.0,
                    },
                    bins_dbfs: vec![-20.0],
                }),
            }),
            gap_before,
        }
    }

    #[test]
    fn history_columns_keep_fixed_source_time_positions_and_visible_gaps() {
        let rect = Rect {
            min: Point { x: 0.0, y: 0.0 },
            max: Point { x: 600.0, y: 300.0 },
        };
        let window_end = 288_000;
        let window_frames = 288_000;
        let newest = history_frame(window_end, false);
        let (x0, x1, _) = history_column(&[newest], rect, window_end, window_frames, 512).unwrap();
        assert!(x0 > 598.0, "one hop must not fill the six-second window");
        assert_eq!(x1, rect.max.x);
        assert_eq!(
            x_for_frontier(rect, window_end, window_frames, 144_000),
            300.0
        );

        let after_gap = history_frame(window_end, true);
        assert!(
            history_column(&[after_gap], rect, window_end, window_frames, 512).is_none(),
            "a discontinuity must leave a visible blank column"
        );
    }
}
