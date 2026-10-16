use crate::render::{Point, Rect};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpectrumCoordinates {
    pub min_frequency_hz: f32,
    pub max_frequency_hz: f32,
    pub min_dbfs: f32,
    pub max_dbfs: f32,
}

impl SpectrumCoordinates {
    pub fn point(self, frequency_hz: f32, dbfs: f32, rect: Rect) -> Point {
        Point {
            x: self.x_for_frequency(frequency_hz, rect),
            y: self.y_for_dbfs(dbfs, rect),
        }
    }

    pub fn x_for_frequency(self, frequency_hz: f32, rect: Rect) -> f32 {
        if rect.width() <= 0.0 || self.max_frequency_hz <= self.min_frequency_hz {
            return rect.min.x;
        }
        let frequency = frequency_hz.clamp(self.min_frequency_hz, self.max_frequency_hz);
        let fraction = (frequency / self.min_frequency_hz).ln()
            / (self.max_frequency_hz / self.min_frequency_hz).ln();
        rect.min.x + fraction * rect.width()
    }

    pub fn y_for_dbfs(self, dbfs: f32, rect: Rect) -> f32 {
        if rect.height() <= 0.0 || self.max_dbfs <= self.min_dbfs {
            return rect.max.y;
        }
        let fraction = ((dbfs - self.min_dbfs) / (self.max_dbfs - self.min_dbfs)).clamp(0.0, 1.0);
        rect.max.y - fraction * rect.height()
    }

    pub fn frequency_at_x(self, x: f32, rect: Rect) -> f32 {
        if rect.width() <= 0.0 {
            return self.min_frequency_hz;
        }
        let fraction = ((x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
        self.min_frequency_hz * (self.max_frequency_hz / self.min_frequency_hz).powf(fraction)
    }

    pub fn dbfs_at_y(self, y: f32, rect: Rect) -> f32 {
        if rect.height() <= 0.0 {
            return self.min_dbfs;
        }
        let fraction = ((rect.max.y - y) / rect.height()).clamp(0.0, 1.0);
        self.min_dbfs + fraction * (self.max_dbfs - self.min_dbfs)
    }

    /// Bins strictly inside the shown frequency range; the curve's edge points come
    /// from `spectrum_dbfs_at` at the range bounds.
    pub fn interior_bin_range(self, bin_hz: f32, bin_count: usize) -> std::ops::Range<usize> {
        if bin_hz <= 0.0 || bin_count == 0 {
            return 0..0;
        }
        let start = (self.min_frequency_hz / bin_hz).floor().max(0.0) as usize + 1;
        let end = ((self.max_frequency_hz / bin_hz).ceil().max(0.0) as usize).min(bin_count);
        start.min(end)..end
    }
}

/// A binned spectrum's level at `frequency_hz`, interpolated linearly in frequency
/// between the two nearest bins; `None` outside the bins.
pub fn spectrum_dbfs_at(bins: &[f32], bin_hz: f32, frequency_hz: f32) -> Option<f32> {
    if bin_hz <= 0.0 || frequency_hz.is_nan() || frequency_hz < 0.0 {
        return None;
    }
    let position = frequency_hz / bin_hz;
    let index = position.floor() as usize;
    let low = *bins.get(index)?;
    let Some(&high) = bins.get(index + 1) else {
        // Exactly on the last bin.
        return (position == index as f32).then_some(low);
    };
    let t = position - index as f32;
    Some(low + (high - low) * t)
}

/// A binned spectrum's level over `[low_hz, high_hz]` (one display column): the
/// loudest bin inside it, so narrow peaks survive; a column between two bins takes
/// the level interpolated at its log-axis center.
pub fn spectrum_dbfs_between(bins: &[f32], bin_hz: f32, low_hz: f32, high_hz: f32) -> Option<f32> {
    if bin_hz <= 0.0 || low_hz.is_nan() || high_hz.is_nan() || low_hz < 0.0 || high_hz < low_hz {
        return None;
    }
    let first = (low_hz / bin_hz).ceil() as usize;
    let end = ((high_hz / bin_hz).floor() as usize)
        .saturating_add(1)
        .min(bins.len());
    if first < end {
        return bins[first..end].iter().copied().max_by(f32::total_cmp);
    }
    spectrum_dbfs_at(bins, bin_hz, (low_hz * high_hz).sqrt())
}

pub const FREQUENCY_TICKS_HZ: &[f32] = &[
    20.0, 50.0, 100.0, 200.0, 500.0, 1_000.0, 2_000.0, 5_000.0, 10_000.0, 20_000.0,
];

pub fn db_ticks(min_dbfs: f32, max_dbfs: f32, step: f32) -> impl Iterator<Item = f32> {
    let first = (min_dbfs / step).ceil() as i32;
    let last = (max_dbfs / step).floor() as i32;
    (first..=last).map(move |tick| tick as f32 * step)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spectrum_mapping_round_trips_both_axes_at_boundaries() {
        let coordinates = SpectrumCoordinates {
            min_frequency_hz: 20.0,
            max_frequency_hz: 20_000.0,
            min_dbfs: -120.0,
            max_dbfs: 6.0,
        };
        let rect = Rect {
            min: Point { x: 17.0, y: 23.0 },
            max: Point { x: 917.0, y: 423.0 },
        };
        for frequency in [20.0, 100.0, 1_000.0, 10_000.0, 20_000.0] {
            let x = coordinates.x_for_frequency(frequency, rect);
            assert!((coordinates.frequency_at_x(x, rect) / frequency - 1.0).abs() < 0.000_01);
        }
        for dbfs in [-120.0, -60.0, -12.0, 0.0, 6.0] {
            let y = coordinates.y_for_dbfs(dbfs, rect);
            assert!((coordinates.dbfs_at_y(y, rect) - dbfs).abs() < 0.000_1);
        }

        // Low frequencies sit at the left edge, high ones at the right, on a log scale.
        assert_eq!(coordinates.x_for_frequency(20.0, rect), rect.min.x);
        assert_eq!(coordinates.x_for_frequency(20_000.0, rect), rect.max.x);
        let (x_100, x_1k, x_10k) = (
            coordinates.x_for_frequency(100.0, rect),
            coordinates.x_for_frequency(1_000.0, rect),
            coordinates.x_for_frequency(10_000.0, rect),
        );
        assert!(rect.min.x < x_100 && x_100 < x_1k && x_1k < x_10k && x_10k < rect.max.x);
        let geometric_mean = (20.0_f32 * 20_000.0).sqrt();
        let middle = coordinates.x_for_frequency(geometric_mean, rect);
        assert!((middle - rect.center().x).abs() < 0.01, "log axis midpoint");
        // Out-of-range frequencies are pinned to the edges.
        assert_eq!(coordinates.x_for_frequency(1.0, rect), rect.min.x);
        assert_eq!(coordinates.x_for_frequency(96_000.0, rect), rect.max.x);

        // Quiet levels sit at the bottom (larger y), loud ones at the top.
        assert_eq!(coordinates.y_for_dbfs(-120.0, rect), rect.max.y);
        assert_eq!(coordinates.y_for_dbfs(6.0, rect), rect.min.y);
        assert!(
            coordinates.y_for_dbfs(-60.0, rect) > coordinates.y_for_dbfs(-12.0, rect),
            "louder must be higher on screen"
        );
        assert_eq!(coordinates.y_for_dbfs(-200.0, rect), rect.max.y);
        assert_eq!(coordinates.y_for_dbfs(20.0, rect), rect.min.y);
    }
}
