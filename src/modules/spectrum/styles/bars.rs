use super::BAR_SPECTRUM_STYLE_ID;
use crate::{
    modules::spectrum::style::{SpectrumStyle, SpectrumStyleContext, SpectrumStyleFactory},
    plotting::spectrum_dbfs_between,
    render::{ColoredRect, Point, Rect, RenderTarget},
    theme::ColorRole,
};

pub(super) struct BarSpectrumStyleFactory;

impl SpectrumStyleFactory for BarSpectrumStyleFactory {
    fn style_id(&self) -> &'static str {
        BAR_SPECTRUM_STYLE_ID
    }

    fn display_name(&self) -> &'static str {
        "Bars"
    }

    fn create(&self) -> Box<dyn SpectrumStyle> {
        Box::new(BarSpectrumStyle {
            bars: Vec::with_capacity(512),
        })
    }
}

struct BarSpectrumStyle {
    bars: Vec<ColoredRect>,
}

impl SpectrumStyle for BarSpectrumStyle {
    fn draw(&mut self, context: SpectrumStyleContext<'_>, renderer: &mut dyn RenderTarget) {
        self.bars.clear();
        let width = context.rect.width().max(1.0);
        let bar_count = (width / 6.0).floor().max(1.0) as usize;
        for bar in 0..bar_count {
            let x0 = context.rect.min.x + bar as f32 * width / bar_count as f32;
            let x1 = context.rect.min.x + (bar + 1) as f32 * width / bar_count as f32;
            let Some(dbfs) = spectrum_dbfs_between(
                context.snapshot.spectrum_dbfs,
                context.snapshot.fft.bin_hz,
                context.coordinates.frequency_at_x(x0, context.rect),
                context.coordinates.frequency_at_x(x1, context.rect),
            ) else {
                continue;
            };
            self.bars.push(ColoredRect {
                rect: Rect {
                    min: Point {
                        x: x0 + 0.75,
                        y: context.coordinates.y_for_dbfs(dbfs, context.rect),
                    },
                    max: Point {
                        x: (x1 - 0.75).max(x0 + 1.0),
                        y: context.rect.max.y,
                    },
                },
                color: context.palette.color(ColorRole::Primary),
            });
        }
        renderer.fill_rects(context.rect, &self.bars);
    }
}
