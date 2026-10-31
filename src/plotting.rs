use std::time::Duration;

use crate::{
    module::FRAME_INTERVAL,
    render::{Color, Flow, Image, Point, Rect, RenderTarget},
};

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

    /// Bounds are excluded because callers interpolate the curve's edge points separately.
    pub fn interior_bin_range(self, bin_hz: f32, bin_count: usize) -> std::ops::Range<usize> {
        if bin_hz <= 0.0 || bin_count == 0 {
            return 0..0;
        }
        let start = (self.min_frequency_hz / bin_hz).floor().max(0.0) as usize + 1;
        let end = ((self.max_frequency_hz / bin_hz).ceil().max(0.0) as usize).min(bin_count);
        start.min(end)..end
    }
}

pub fn spectrum_dbfs_at(bins: &[f32], bin_hz: f32, frequency_hz: f32) -> Option<f32> {
    if bin_hz <= 0.0 || frequency_hz.is_nan() || frequency_hz < 0.0 {
        return None;
    }
    let position = frequency_hz / bin_hz;
    let index = position.floor() as usize;
    let low = *bins.get(index)?;
    let Some(&high) = bins.get(index + 1) else {
        return (position == index as f32).then_some(low);
    };
    let t = position - index as f32;
    Some(low + (high - low) * t)
}

/// Take the loudest bin in inclusive `[low_hz, high_hz]` so narrow peaks survive.
/// Between bins, interpolate at the log-axis center.
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

/// Absolute source-time columns map to fixed slots, avoiding resampling shimmer; a slot holds
/// the column's data, and its pixels are rasterized when written or when the presentation
/// changes. Unwritten slots stay transparent, with the newest column at the edge opposite the
/// flow.
pub struct ColumnRing<T> {
    data: Vec<Option<T>>,
    /// Slots whose pixels no longer match their data.
    dirty: Vec<usize>,
    marked: Vec<bool>,
    newest: Option<u64>,
    image: Image,
}

impl<T> Default for ColumnRing<T> {
    fn default() -> Self {
        Self {
            data: Vec::new(),
            dirty: Vec::new(),
            marked: Vec::new(),
            newest: None,
            image: Image::default(),
        }
    }
}

impl<T: Clone> ColumnRing<T> {
    pub fn reset(&mut self, columns: usize) {
        let columns = columns.max(1);
        self.data.clear();
        self.data.resize_with(columns, || None);
        self.dirty.clear();
        self.marked.clear();
        self.marked.resize(columns, false);
        self.newest = None;
        self.image.resize(columns, self.image.height().max(1));
    }

    fn mark(&mut self, slot: usize) {
        if !self.marked[slot] {
            self.marked[slot] = true;
            self.dirty.push(slot);
        }
    }

    /// `fill_skipped` repeats a value covering several columns' time; otherwise skipped
    /// columns are cleared.
    pub fn write(&mut self, column: u64, value: T, fill_skipped: bool) {
        let columns = self.data.len() as u64;
        // A column older than the ring holds would overwrite a newer one.
        if self
            .newest
            .is_some_and(|newest| column.saturating_add(columns) <= newest)
        {
            return;
        }
        if let Some(newest) = self.newest
            && column > newest
        {
            let first = (newest + 1).max(column.saturating_sub(columns - 1));
            for skipped in first..column {
                let slot = (skipped % columns) as usize;
                self.data[slot] = fill_skipped.then(|| value.clone());
                self.mark(slot);
            }
        }
        let slot = (column % columns) as usize;
        self.data[slot] = Some(value);
        self.mark(slot);
        self.newest = Some(self.newest.map_or(column, |newest| newest.max(column)));
    }

    /// Clear passed columns when silence advances source time without new data.
    pub fn advance(&mut self, column: u64) {
        let Some(newest) = self.newest.filter(|&newest| column > newest) else {
            return;
        };
        let columns = self.data.len() as u64;
        let first = (newest + 1).max(column.saturating_sub(columns - 1));
        for passed in first..=column {
            let slot = (passed % columns) as usize;
            self.data[slot] = None;
            self.mark(slot);
        }
        self.newest = Some(column);
    }

    /// Rasterizes changed columns into `rows` top-first pixels; every column when `rows` or
    /// `presentation_changed` says the pixels of unchanged data are stale.
    pub fn rasterize(
        &mut self,
        rows: usize,
        presentation_changed: bool,
        mut rasterize: impl FnMut(&T, &mut [Color]),
    ) {
        let rows = rows.max(1);
        let resized = self.image.height() != rows;
        if resized {
            self.image.resize(self.data.len(), rows);
        }
        if resized || presentation_changed {
            for slot in 0..self.data.len() {
                if self.data[slot].is_some() {
                    self.mark(slot);
                }
            }
        }
        for &slot in &self.dirty {
            let pixels = self.image.column_mut(slot);
            match &self.data[slot] {
                Some(value) => rasterize(value, pixels),
                None => pixels.fill(Color::rgba(0, 0, 0, 0)),
            }
            self.marked[slot] = false;
        }
        self.dirty.clear();
    }

    pub fn draw(&mut self, renderer: &mut dyn RenderTarget, clip: Rect, rect: Rect, flow: Flow) {
        let Some(newest) = self.newest else {
            return;
        };
        renderer.scrolling_image(
            clip,
            rect,
            &mut self.image,
            ((newest + 1) % self.data.len() as u64) as usize,
            flow,
        );
    }
}

/// Results normally arrive every 10–35 ms; allow jitter before treating the source as silent.
pub const SILENCE_GRACE: Duration = Duration::from_millis(150);

/// Loopback sends no samples during silence, so source frames alone cannot keep a view moving.
/// After `SILENCE_GRACE`, insert display-clock silence; later source frames follow those gaps.
#[derive(Default)]
pub struct ViewClock {
    /// Source frontier and display time last seen; pauses advance the time to exclude held time.
    newest: Option<(u64, Duration)>,
    last_update: Duration,
    paused: bool,
    /// Frames of silence shown after `newest`.
    pending: u64,
    /// `(frame, total)`: frames from `frame` on are placed `total` frames later.
    gaps: Vec<(u64, u64)>,
}

impl ViewClock {
    /// Returns the exclusive end in source frames plus inserted silence.
    pub fn update(&mut self, frontier: u64, now: Duration, sample_rate: f32, paused: bool) -> u64 {
        match self.newest {
            Some((seen, _)) if frontier > seen => {
                if self.pending > 0 {
                    self.gaps.push((seen, self.total() + self.pending));
                    self.pending = 0;
                }
                self.newest = Some((frontier, now));
            }
            Some((seen, at)) if frontier == seen => {
                if paused {
                    let held = now.saturating_sub(self.last_update);
                    self.newest = Some((seen, at + held));
                } else {
                    let silent = now.saturating_sub(at).saturating_sub(SILENCE_GRACE);
                    self.pending = (silent.as_secs_f64() * f64::from(sample_rate)) as u64;
                }
            }
            _ => {
                *self = Self {
                    newest: Some((frontier, now)),
                    ..Self::default()
                }
            }
        }
        self.last_update = now;
        self.paused = paused;
        self.frontier_position(frontier) + self.pending
    }

    /// Schedule grace expiry, then repaint each frame until silence pushes data out of `span`
    /// positions (source frames plus inserted silence).
    /// A paused source holds the picture still.
    pub fn repaint_after(&self, now: Duration, data_end: u64, span: u64) -> Option<Duration> {
        let (frontier, at) = self.newest?;
        if self.paused {
            return None;
        }
        if self.pending == 0 {
            let wait = (at + SILENCE_GRACE).saturating_sub(now);
            return Some(wait.max(Duration::from_millis(1)));
        }
        let end = self.frontier_position(frontier) + self.pending;
        (end.saturating_sub(data_end) < span).then_some(FRAME_INTERVAL)
    }

    pub fn position(&self, frame: u64) -> u64 {
        frame + self.offset(|gap| frame >= gap)
    }

    /// Unlike a frame at a gap boundary, an exclusive frontier there precedes the gap.
    pub fn frontier_position(&self, frontier: u64) -> u64 {
        frontier + self.offset(|gap| frontier > gap)
    }

    fn offset(&self, after: impl Fn(u64) -> bool) -> u64 {
        self.gaps
            .iter()
            .rev()
            .find(|(gap, _)| after(*gap))
            .map_or(0, |(_, total)| *total)
    }

    fn total(&self) -> u64 {
        self.gaps.last().map_or(0, |(_, total)| *total)
    }
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
    fn presentation_changes_reuse_data_and_silence_cannot_resurrect_it() {
        let mut data = ColumnRing::default();
        data.reset(4);
        data.write(1, 17_u8, false);
        data.write(2, 29, false);
        let mut calls = 0;
        data.rasterize(2, false, |&value, pixels| {
            calls += 1;
            pixels.fill(Color::rgb(value, 0, 0));
        });
        assert_eq!(calls, 2);
        data.rasterize(2, false, |_, _| {
            panic!("unchanged columns are not rasterized")
        });
        data.rasterize(4, false, |&value, pixels| {
            pixels.fill(Color::rgb(0, value, 0));
        });
        assert_eq!(data.image.height(), 4);
        assert_eq!(data.image.column(1), [Color::rgb(0, 17, 0); 4]);
        data.advance(3);
        data.rasterize(4, true, |&value, pixels| {
            pixels.fill(Color::rgb(0, 0, value));
        });
        assert_eq!(data.image.column(3), [Color::rgba(0, 0, 0, 0); 4]);
        data.advance(8);
        data.rasterize(8, true, |_, _| panic!("silence cleared retained payloads"));
        assert!((0..4).all(|slot| data.image.column(slot).iter().all(|pixel| pixel.alpha == 0)));
    }

    #[test]
    fn clearing_or_resetting_data_ring_releases_shared_payloads() {
        use std::sync::Arc;
        let value = Arc::new(3_u8);
        let weak = Arc::downgrade(&value);
        let mut data = ColumnRing::default();
        data.reset(4);
        data.write(1, Arc::clone(&value), false);
        data.write(3, Arc::clone(&value), true);
        drop(value);
        data.advance(7);
        assert!(weak.upgrade().is_none());
        data.write(8, Arc::new(7), false);
        data.reset(2);
        assert!(data.data.iter().all(Option::is_none));
    }

    #[test]
    fn column_ring_keeps_the_newest_column_at_its_edge_and_fills_or_clears_skipped_columns() {
        use crate::modules::test_support::RecordingTarget;
        let color = |value: u8| [Color::rgb(value, 0, 0); 2];
        let rect = Rect {
            min: Point { x: 0.0, y: 0.0 },
            max: Point { x: 40.0, y: 20.0 },
        };
        let mut ring = ColumnRing::default();
        ring.reset(4);
        let mut target = RecordingTarget::default();
        let mut drawn = |ring: &mut ColumnRing<[Color; 2]>| {
            ring.rasterize(2, false, |value, pixels| pixels.copy_from_slice(value));
            ring.draw(&mut target, rect, rect, Flow::Left);
            target.images.pop().unwrap()
        };

        // Start before the ring fills to catch content incorrectly anchored at the oldest edge.
        ring.write(1, color(1), true);
        ring.write(2, color(2), true);
        let started = drawn(&mut ring);
        assert_eq!(started.first_column, 3);
        assert_eq!(started.changed, vec![1, 2]);
        assert_eq!(started.columns[3], [Color::rgba(0, 0, 0, 0); 2]);
        assert_eq!(started.columns[0], [Color::rgba(0, 0, 0, 0); 2]);

        // One result spans three columns, crossing the ring boundary.
        ring.write(5, color(5), true);
        let full = drawn(&mut ring);
        assert_eq!(full.first_column, 2);
        assert_eq!(full.columns[3], color(5));
        assert_eq!(full.columns[0], color(5));
        assert_eq!(full.columns[1], color(5));
        assert_eq!(full.columns[2], color(2));

        ring.write(8, color(8), false);
        let gapped = drawn(&mut ring);
        assert_eq!(gapped.first_column, 1);
        assert_eq!(gapped.columns[2], [Color::rgba(0, 0, 0, 0); 2]);
        assert_eq!(gapped.columns[3], [Color::rgba(0, 0, 0, 0); 2]);
        assert_eq!(gapped.columns[0], color(8));
        assert_eq!(gapped.columns[1], color(5));
        let mut changed = gapped.changed.clone();
        changed.sort_unstable();
        assert_eq!(changed, vec![0, 2, 3]);

        let again = drawn(&mut ring);
        assert!(again.changed.is_empty());
        assert_eq!(again.first_column, 1);
    }

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
        assert_eq!(coordinates.x_for_frequency(1.0, rect), rect.min.x);
        assert_eq!(coordinates.x_for_frequency(96_000.0, rect), rect.max.x);

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
