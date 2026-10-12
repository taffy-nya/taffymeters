use super::DEFAULT_SPECTRUM_STYLE_ID;
use crate::{
    modules::spectrum::style::{
        SpectrumPlot, SpectrumStyle, SpectrumStyleContext, SpectrumStyleFactory,
    },
    plotting::SpectrumCoordinates,
    render::{Point, Rect, RenderTarget, Stroke},
    theme::ColorRole,
};

pub(super) struct LineSpectrumStyleFactory;

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
