use super::DEFAULT_SPECTRUM_STYLE_ID;
use crate::{
    modules::spectrum::style::{
        SpectrumPlot, SpectrumStyle, SpectrumStyleContext, SpectrumStyleFactory,
    },
    plotting::{SpectrumCoordinates, spectrum_dbfs_at},
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

    fn color_roles(&self) -> &'static [ColorRole] {
        &[ColorRole::Primary, ColorRole::Secondary]
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
    // proportional to the drawn width instead of the FFT size. The ends are the
    // levels interpolated at the range bounds, so the curve reaches both edges even
    // when the first bin lies well inside the range (small FFTs).
    output.clear();
    let bin_hz = snapshot.fft.bin_hz;
    let mut column: Option<(i32, Point)> = None;
    let mut add = |frequency_hz: f32, dbfs: f32| {
        let point = coordinates.point(frequency_hz, dbfs, rect);
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
    };
    let edge = |frequency_hz: f32| spectrum_dbfs_at(bins, bin_hz, frequency_hz);
    if let Some(dbfs) = edge(coordinates.min_frequency_hz) {
        add(coordinates.min_frequency_hz, dbfs);
    }
    for index in coordinates.interior_bin_range(bin_hz, bins.len()) {
        add(index as f32 * bin_hz, bins[index]);
    }
    if let Some(dbfs) = edge(coordinates.max_frequency_hz) {
        add(coordinates.max_frequency_hz, dbfs);
    }
    if let Some((_, loudest)) = column {
        output.push(loudest);
    }
}
