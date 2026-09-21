use std::time::Duration;

use crate::{
    module::{
        Module, ModuleFactory, ModuleFrame, ModuleResponse, ModuleServices, Registry, RegistryError,
    },
    plotting::SpectrumCoordinates,
    render::{Color, ColoredRect, Point, Rect},
    snapshot::{SpectrumHistoryFrame, SpectrumReader},
};

pub const SPECTROGRAM_MODULE_ID: &str = "taffymeters.spectrogram";

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
    ) -> Result<Box<dyn Module>, RegistryError> {
        Ok(Box::new(SpectrogramModule {
            reader: services.spectrum.clone(),
            visible_seconds: 6.0,
            cells: Vec::with_capacity(16_384),
        }))
    }
}

struct SpectrogramModule {
    reader: SpectrumReader,
    visible_seconds: f32,
    cells: Vec<ColoredRect>,
}

impl Module for SpectrogramModule {
    fn draw(&mut self, frame: ModuleFrame<'_>) -> ModuleResponse {
        if frame
            .pointer
            .is_some_and(|point| frame.rect.contains(point))
            && (frame.zoom_delta - 1.0).abs() > 0.001
        {
            self.visible_seconds = (self.visible_seconds / frame.zoom_delta).clamp(0.5, 8.0);
        }
        let requested_channel = self.reader.requested_channel();
        let requested_revision = self.reader.requested_revision();
        let Some(snapshot) = self.reader.latest().filter(|snapshot| {
            snapshot.channel == requested_channel && snapshot.config_revision == requested_revision
        }) else {
            return ModuleResponse {
                repaint_after: Some(Duration::from_millis(100)),
            };
        };
        if snapshot.history.is_empty() {
            return ModuleResponse {
                repaint_after: Some(Duration::from_millis(100)),
            };
        }

        let bands = (frame.rect.height() / 5.0).floor().clamp(16.0, 96.0) as usize;
        let coordinates = SpectrumCoordinates {
            min_frequency_hz: 20.0,
            max_frequency_hz: (snapshot.descriptor.sample_rate_hz as f32 * 0.5).min(20_000.0),
            min_dbfs: snapshot.fft.floor_dbfs,
            max_dbfs: 0.0,
        };
        let sample_rate = snapshot.descriptor.sample_rate_hz as f32;
        let window_frames = (self.visible_seconds * sample_rate).round().max(1.0) as u64;
        let window_end = snapshot.processed_through_frame;
        let first_frontier = window_end.saturating_sub(window_frames);
        let first_visible = snapshot
            .history
            .partition_point(|entry| entry.processed_through_frame < first_frontier);
        let visible = &snapshot.history[first_visible..];
        let max_columns = (frame.rect.width() / 3.0).floor().max(1.0) as usize;
        let column_step = visible.len().div_ceil(max_columns).max(1);
        let band_height = frame.rect.height() / bands as f32;
        self.cells.clear();
        for group in visible.chunks(column_step) {
            let Some((x0, x1, entry)) = history_column(
                group,
                frame.rect,
                window_end,
                window_frames,
                snapshot.fft.hop_size,
            ) else {
                continue;
            };
            for band in 0..bands {
                let y0 = frame.rect.max.y - (band + 1) as f32 * band_height;
                let y1 = y0 + band_height + 0.5;
                let fraction = (band as f32 + 0.5) / bands as f32;
                let x = frame.rect.min.x + fraction * frame.rect.width();
                let frequency = coordinates.frequency_at_x(x, frame.rect);
                let bin = (frequency / snapshot.fft.bin_hz).round() as usize;
                let Some(&dbfs) = entry.bins_dbfs.get(bin) else {
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
                    color: heat_color(normalized),
                });
            }
        }
        frame.renderer.fill_rects(frame.rect, &self.cells);
        ModuleResponse {
            repaint_after: Some(Duration::from_millis(33)),
        }
    }
}

fn history_column(
    group: &[SpectrumHistoryFrame],
    rect: Rect,
    window_end: u64,
    window_frames: u64,
    hop_size: usize,
) -> Option<(f32, f32, &SpectrumHistoryFrame)> {
    if group.is_empty() || group.iter().any(|entry| entry.discontinuity_before) {
        return None;
    }
    let entry = group.last()?;
    let represented_frames = (hop_size as u64).saturating_mul(group.len() as u64);
    let column_start = entry
        .processed_through_frame
        .saturating_sub(represented_frames);
    let x0 = x_for_frontier(rect, window_end, window_frames, column_start);
    let x1 = x_for_frontier(
        rect,
        window_end,
        window_frames,
        entry.processed_through_frame,
    );
    Some((x0, (x1 + 0.5).max(x0 + 1.0).min(rect.max.x), entry))
}

fn x_for_frontier(rect: Rect, window_end: u64, window_frames: u64, frontier: u64) -> f32 {
    let age = window_end.saturating_sub(frontier).min(window_frames) as f32;
    rect.max.x - age / window_frames.max(1) as f32 * rect.width()
}

fn heat_color(value: f32) -> Color {
    let value = value.clamp(0.0, 1.0);
    let red = ((value * 3.0 - 1.0).clamp(0.0, 1.0) * 255.0) as u8;
    let green = ((value * 3.0 - 2.0).clamp(0.0, 1.0) * 255.0) as u8;
    let blue = ((value * 3.0).clamp(0.0, 1.0) * 255.0) as u8;
    Color::rgba(red, green, blue, (value * 255.0) as u8)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn history_frame(frontier: u64, discontinuity_before: bool) -> SpectrumHistoryFrame {
        SpectrumHistoryFrame {
            processed_through_frame: frontier,
            discontinuity_before,
            bins_dbfs: Arc::from([-20.0]),
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
