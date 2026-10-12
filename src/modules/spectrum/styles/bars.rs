use super::BAR_SPECTRUM_STYLE_ID;
use crate::{
    modules::spectrum::style::{SpectrumStyle, SpectrumStyleContext, SpectrumStyleFactory},
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
        let range = context.coordinates.visible_bin_range(
            context.snapshot.fft.bin_hz,
            context.snapshot.spectrum_dbfs.len(),
        );
        for bar in 0..bar_count {
            let x0 = context.rect.min.x + bar as f32 * width / bar_count as f32;
            let x1 = context.rect.min.x + (bar + 1) as f32 * width / bar_count as f32;
            let f0 = context.coordinates.frequency_at_x(x0, context.rect);
            let f1 = context.coordinates.frequency_at_x(x1, context.rect);
            let first = ((f0 / context.snapshot.fft.bin_hz).floor() as usize).max(range.start);
            let end = ((f1 / context.snapshot.fft.bin_hz).ceil() as usize)
                .saturating_add(1)
                .min(range.end);
            let Some(dbfs) = context.snapshot.spectrum_dbfs[first.min(end)..end]
                .iter()
                .copied()
                .max_by(f32::total_cmp)
            else {
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
