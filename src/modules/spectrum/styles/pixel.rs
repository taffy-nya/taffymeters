use super::PIXEL_SPECTRUM_STYLE_ID;
use crate::{
    modules::spectrum::style::{SpectrumStyle, SpectrumStyleContext, SpectrumStyleFactory},
    plotting::spectrum_dbfs_between,
    render::{ColoredRect, Point, Rect, RenderTarget},
    theme::ColorRole,
};

pub(super) struct PixelSpectrumStyleFactory;

impl SpectrumStyleFactory for PixelSpectrumStyleFactory {
    fn style_id(&self) -> &'static str {
        PIXEL_SPECTRUM_STYLE_ID
    }

    fn display_name(&self) -> &'static str {
        "Pixel"
    }

    fn create(&self) -> Box<dyn SpectrumStyle> {
        Box::new(PixelSpectrumStyle {
            pixels: Vec::with_capacity(2048),
        })
    }
}

struct PixelSpectrumStyle {
    pixels: Vec<ColoredRect>,
}

impl SpectrumStyle for PixelSpectrumStyle {
    fn draw(&mut self, context: SpectrumStyleContext<'_>, renderer: &mut dyn RenderTarget) {
        self.pixels.clear();
        let cell = 7.0;
        let rect = context.rect;
        let bin_hz = context.snapshot.fft.bin_hz;
        let columns = (rect.width() / cell).floor().max(1.0) as usize;
        for column in 0..columns {
            let x = rect.min.x + column as f32 * cell;
            let low_hz = context.coordinates.frequency_at_x(x, rect);
            let high_hz = context.coordinates.frequency_at_x(x + cell, rect);
            let top_of = |dbfs| context.coordinates.y_for_dbfs(dbfs, rect);
            let top =
                spectrum_dbfs_between(context.snapshot.spectrum_dbfs, bin_hz, low_hz, high_hz)
                    .map(top_of);
            let peak_top = context
                .snapshot
                .peak_dbfs
                .and_then(|peak| spectrum_dbfs_between(peak, bin_hz, low_hz, high_hz))
                .map(top_of);
            let lit = |top: Option<f32>, y: f32| top.is_some_and(|top| y + cell > top);
            let mut y = rect.max.y - cell;
            while lit(top, y) || lit(peak_top, y) {
                let last = y < rect.min.y + cell;
                // Only the highest peak cell is lit; leave the gap above the level dark.
                let color = if lit(peak_top, y) && (last || !lit(peak_top, y - cell)) {
                    Some(context.palette.color(ColorRole::Secondary))
                } else if lit(top, y) {
                    let strength = ((rect.max.y - y) / rect.height()).clamp(0.0, 1.0);
                    Some(context.palette.level(strength))
                } else {
                    None
                };
                if let Some(color) = color {
                    self.pixels.push(ColoredRect {
                        rect: Rect {
                            min: Point { x, y },
                            max: Point {
                                x: (x + cell - 1.0).min(rect.max.x),
                                y: (y + cell - 1.0).min(rect.max.y),
                            },
                        },
                        color,
                    });
                }
                if last {
                    break;
                }
                y -= cell;
            }
        }
        renderer.fill_rects(rect, &self.pixels);
    }

    fn color_roles(&self) -> &'static [ColorRole] {
        &[
            ColorRole::Low,
            ColorRole::Mid,
            ColorRole::High,
            ColorRole::Secondary,
        ]
    }
}
