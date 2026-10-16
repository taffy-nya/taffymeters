use super::PIXEL_SPECTRUM_STYLE_ID;
use crate::{
    modules::spectrum::style::{SpectrumStyle, SpectrumStyleContext, SpectrumStyleFactory},
    plotting::spectrum_dbfs_between,
    render::{ColoredRect, Point, Rect, RenderTarget},
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
        let columns = (context.rect.width() / cell).floor().max(1.0) as usize;
        for column in 0..columns {
            let x = context.rect.min.x + column as f32 * cell;
            let Some(dbfs) = spectrum_dbfs_between(
                context.snapshot.spectrum_dbfs,
                context.snapshot.fft.bin_hz,
                context.coordinates.frequency_at_x(x, context.rect),
                context.coordinates.frequency_at_x(x + cell, context.rect),
            ) else {
                continue;
            };
            let top = context.coordinates.y_for_dbfs(dbfs, context.rect);
            let mut y = context.rect.max.y - cell;
            while y + cell > top {
                let strength = ((context.rect.max.y - y) / context.rect.height()).clamp(0.0, 1.0);
                self.pixels.push(ColoredRect {
                    rect: Rect {
                        min: Point { x, y },
                        max: Point {
                            x: (x + cell - 1.0).min(context.rect.max.x),
                            y: (y + cell - 1.0).min(context.rect.max.y),
                        },
                    },
                    color: context.palette.level(strength),
                });
                if y < context.rect.min.y + cell {
                    break;
                }
                y -= cell;
            }
        }
        renderer.fill_rects(context.rect, &self.pixels);
    }
}
