use super::BARS_SPECTRUM_STYLE_ID;
use crate::{
    modules::spectrum::style::{SpectrumStyle, SpectrumStyleContext, SpectrumStyleFactory},
    plotting::spectrum_dbfs_between,
    render::{ColoredRect, Point, Rect, RenderTarget},
    theme::ColorRole,
};

pub(super) struct BarSpectrumStyleFactory;

impl SpectrumStyleFactory for BarSpectrumStyleFactory {
    fn style_id(&self) -> &'static str {
        BARS_SPECTRUM_STYLE_ID
    }

    fn display_name(&self) -> &'static str {
        "Bars"
    }

    fn create(&self) -> Box<dyn SpectrumStyle> {
        Box::new(BarSpectrumStyle {
            bars: Vec::with_capacity(1024),
        })
    }
}

struct BarSpectrumStyle {
    bars: Vec<ColoredRect>,
}

/// Height of the peak mark above each bar, in points.
const PEAK_MARK_HEIGHT: f32 = 2.0;

impl SpectrumStyle for BarSpectrumStyle {
    fn draw(&mut self, context: SpectrumStyleContext<'_>, renderer: &mut dyn RenderTarget) {
        self.bars.clear();
        let width = context.rect.width().max(1.0);
        let bar_count = (width / 6.0).floor().max(1.0) as usize;
        let bin_hz = context.snapshot.fft.bin_hz;
        for bar in 0..bar_count {
            let x0 = context.rect.min.x + bar as f32 * width / bar_count as f32;
            let x1 = context.rect.min.x + (bar + 1) as f32 * width / bar_count as f32;
            let low_hz = context.coordinates.frequency_at_x(x0, context.rect);
            let high_hz = context.coordinates.frequency_at_x(x1, context.rect);
            let (left, right) = (x0 + 0.75, (x1 - 0.75).max(x0 + 1.0));
            if let Some(dbfs) =
                spectrum_dbfs_between(context.snapshot.spectrum_dbfs, bin_hz, low_hz, high_hz)
            {
                self.bars.push(ColoredRect {
                    rect: Rect {
                        min: Point {
                            x: left,
                            y: context.coordinates.y_for_dbfs(dbfs, context.rect),
                        },
                        max: Point {
                            x: right,
                            y: context.rect.max.y,
                        },
                    },
                    color: context.palette.color(ColorRole::Primary),
                });
            }
            // The peak of the same band: a short mark whose top edge is the level.
            if let Some(peak) = context
                .snapshot
                .peak_dbfs
                .and_then(|peak| spectrum_dbfs_between(peak, bin_hz, low_hz, high_hz))
            {
                let y = context.coordinates.y_for_dbfs(peak, context.rect);
                self.bars.push(ColoredRect {
                    rect: Rect {
                        min: Point { x: left, y },
                        max: Point {
                            x: right,
                            y: y + PEAK_MARK_HEIGHT,
                        },
                    },
                    color: context.palette.color(ColorRole::Secondary),
                });
            }
        }
        renderer.fill_rects(context.rect, &self.bars);
    }

    fn color_roles(&self) -> &'static [ColorRole] {
        &[ColorRole::Primary, ColorRole::Secondary]
    }
}
