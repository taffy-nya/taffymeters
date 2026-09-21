use std::time::Duration;

use crate::{
    module::{
        Module, ModuleFactory, ModuleFrame, ModuleResponse, ModuleServices, Registry, RegistryError,
    },
    plotting::SpectrumCoordinates,
    render::{Color, ColoredRect, Point, Rect},
    snapshot::SpectrumReader,
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
        let first_frontier = snapshot
            .processed_through_frame
            .saturating_sub((self.visible_seconds * sample_rate) as u64);
        let visible_count = snapshot
            .history
            .iter()
            .filter(|entry| entry.processed_through_frame >= first_frontier)
            .count();
        let max_columns = (frame.rect.width() / 3.0).floor().max(1.0) as usize;
        let column_step = visible_count.div_ceil(max_columns).max(1);
        let displayed_columns = visible_count.div_ceil(column_step);
        let column_width = frame.rect.width() / displayed_columns.max(1) as f32;
        let band_height = frame.rect.height() / bands as f32;
        self.cells.clear();
        for (column, entry) in snapshot
            .history
            .iter()
            .filter(|entry| entry.processed_through_frame >= first_frontier)
            .step_by(column_step)
            .enumerate()
        {
            let x0 = frame.rect.min.x + column as f32 * column_width;
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
                        max: Point {
                            x: x0 + column_width + 0.5,
                            y: y1,
                        },
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

fn heat_color(value: f32) -> Color {
    let value = value.clamp(0.0, 1.0);
    let red = ((value * 3.0 - 1.0).clamp(0.0, 1.0) * 255.0) as u8;
    let green = ((value * 3.0 - 2.0).clamp(0.0, 1.0) * 255.0) as u8;
    let blue = ((value * 3.0).clamp(0.0, 1.0) * 255.0) as u8;
    Color::rgba(red, green, blue, (value * 255.0) as u8)
}
