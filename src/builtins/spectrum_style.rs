use crate::{
    dataflow::FftMetadata,
    extensions::{Registry, RegistryError},
    plotting::SpectrumCoordinates,
    render::{ColoredRect, Point, Rect, RenderTarget, Stroke},
    theme::{ColorRole, Palette},
};

pub const DEFAULT_SPECTRUM_STYLE_ID: &str = "taffymeters.spectrum.line";
pub const BAR_SPECTRUM_STYLE_ID: &str = "taffymeters.spectrum.bars";
pub const PIXEL_SPECTRUM_STYLE_ID: &str = "taffymeters.spectrum.pixel";

/// One Spectrum result: the main curve and, when available, the peak curve of the
/// same analysis hop.
#[derive(Clone, Copy, Debug)]
pub struct SpectrumPlot<'a> {
    pub fft: FftMetadata,
    pub spectrum_dbfs: &'a [f32],
    pub peak_dbfs: Option<&'a [f32]>,
}

pub struct SpectrumStyleContext<'a> {
    pub snapshot: &'a SpectrumPlot<'a>,
    pub coordinates: &'a SpectrumCoordinates,
    pub rect: Rect,
    pub monotonic_time: std::time::Duration,
    pub palette: &'a Palette,
}

pub trait SpectrumStyle: Send {
    fn draw(&mut self, context: SpectrumStyleContext<'_>, renderer: &mut dyn RenderTarget);
    fn clear(&mut self) {}
}

pub trait SpectrumStyleFactory: Send + Sync {
    fn style_id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn create(&self) -> Box<dyn SpectrumStyle>;
}

pub(super) fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register_spectrum_style(LineSpectrumStyleFactory)?;
    registry.register_spectrum_style(BarSpectrumStyleFactory)?;
    registry.register_spectrum_style(PixelSpectrumStyleFactory)
}

struct LineSpectrumStyleFactory;

impl SpectrumStyleFactory for LineSpectrumStyleFactory {
    fn style_id(&self) -> &'static str {
        DEFAULT_SPECTRUM_STYLE_ID
    }

    fn display_name(&self) -> &'static str {
        "Curve"
    }

    fn create(&self) -> Box<dyn SpectrumStyle> {
        Box::new(LineSpectrumStyle {
            points: Vec::with_capacity(2048),
            peak_points: Vec::with_capacity(2048),
        })
    }
}

struct LineSpectrumStyle {
    points: Vec<Point>,
    peak_points: Vec<Point>,
}

impl SpectrumStyle for LineSpectrumStyle {
    fn draw(&mut self, context: SpectrumStyleContext<'_>, renderer: &mut dyn RenderTarget) {
        spectrum_points(
            context.snapshot,
            *context.coordinates,
            context.rect,
            &mut self.points,
            context.snapshot.spectrum_dbfs,
        );
        renderer.polyline(
            context.rect,
            &self.points,
            Stroke {
                width: 1.6,
                color: context.palette.color(ColorRole::Primary),
            },
        );
        if let Some(peak) = context.snapshot.peak_dbfs {
            spectrum_points(
                context.snapshot,
                *context.coordinates,
                context.rect,
                &mut self.peak_points,
                peak,
            );
            renderer.polyline(
                context.rect,
                &self.peak_points,
                Stroke {
                    width: 1.0,
                    color: context.palette.color(ColorRole::Secondary),
                },
            );
        }
    }
}

struct BarSpectrumStyleFactory;

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

struct PixelSpectrumStyleFactory;

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
            let frequency = context
                .coordinates
                .frequency_at_x(x + cell * 0.5, context.rect);
            let bin = (frequency / context.snapshot.fft.bin_hz).round() as usize;
            let Some(&dbfs) = context.snapshot.spectrum_dbfs.get(bin) else {
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

fn spectrum_points(
    snapshot: &SpectrumPlot<'_>,
    coordinates: SpectrumCoordinates,
    rect: Rect,
    output: &mut Vec<Point>,
    bins: &[f32],
) {
    // Display sampling: on the log axis many high bins share one point-wide column;
    // keep only the loudest bin of each column so peaks survive and the path stays
    // proportional to the drawn width instead of the FFT size.
    output.clear();
    let mut column: Option<(i32, Point)> = None;
    for index in coordinates.visible_bin_range(snapshot.fft.bin_hz, bins.len()) {
        let point = coordinates.point(index as f32 * snapshot.fft.bin_hz, bins[index], rect);
        let x = point.x.floor() as i32;
        match &mut column {
            Some((current, loudest)) if *current == x => {
                if point.y < loudest.y {
                    *loudest = point;
                }
            }
            _ => {
                if let Some((_, loudest)) = column.replace((x, point)) {
                    output.push(loudest);
                }
            }
        }
    }
    if let Some((_, loudest)) = column {
        output.push(loudest);
    }
}
