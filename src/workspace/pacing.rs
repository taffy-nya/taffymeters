//! Spaces self-scheduled host passes (data wake-ups, animation, timed polls) by the
//! frame-rate limit. Input-driven passes are not paced: eframe schedules them, vsync bounds them.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

pub(crate) const DEFAULT_FPS_LIMIT: f32 = 60.0;
pub(crate) const FPS_LIMIT_RANGE: (f32, f32) = (10.0, 240.0);

/// A zero delay makes egui run two passes.
const MIN_WAIT: Duration = Duration::from_millis(1);

/// Lock-free: the analysis worker's publication waker reads it. Only the UI thread writes.
pub(crate) struct FramePacer {
    epoch: Instant,
    interval_ns: AtomicU64,
    /// Frame-grid time of the latest host pass, since `epoch`.
    tick_ns: AtomicU64,
    predicted_ns: AtomicU64,
}

impl FramePacer {
    pub(crate) fn new(fps: f32) -> Self {
        let pacer = Self {
            epoch: Instant::now(),
            interval_ns: AtomicU64::new(0),
            tick_ns: AtomicU64::new(0),
            predicted_ns: AtomicU64::new(0),
        };
        pacer.set_fps(fps);
        pacer
    }

    pub(crate) fn set_fps(&self, fps: f32) {
        let interval = Duration::from_secs_f64(1.0 / f64::from(fps));
        self.interval_ns.store(nanos(interval), Ordering::Relaxed);
    }

    /// `predicted` is egui's predicted frame time, which it subtracts from every repaint delay.
    pub(crate) fn begin_pass(&self, now: Instant, predicted: Duration) {
        let now = self.since_epoch(now);
        let interval = self.interval_ns.load(Ordering::Relaxed);
        let due = self
            .tick_ns
            .load(Ordering::Relaxed)
            .saturating_add(interval);
        // A pass woken late keeps the grid, so timer latency does not lower the rate;
        // early (input) passes and passes after idle restart it.
        let tick = if now >= due && now - due < interval {
            due
        } else {
            now
        };
        self.tick_ns.store(tick, Ordering::Relaxed);
        self.predicted_ns.store(nanos(predicted), Ordering::Relaxed);
    }

    /// Delay to pass to egui for the next paced pass, waiting at least `at_least`.
    pub(crate) fn request_delay(&self, now: Instant, at_least: Duration) -> Duration {
        let due = self
            .tick_ns
            .load(Ordering::Relaxed)
            .saturating_add(self.interval_ns.load(Ordering::Relaxed));
        let wait = Duration::from_nanos(due.saturating_sub(self.since_epoch(now)));
        let predicted = Duration::from_nanos(self.predicted_ns.load(Ordering::Relaxed));
        wait.max(at_least).max(MIN_WAIT) + predicted
    }

    fn since_epoch(&self, now: Instant) -> u64 {
        nanos(now.saturating_duration_since(self.epoch))
    }
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREDICTED: Duration = Duration::from_micros(16_667);

    #[test]
    fn request_waits_out_the_rest_of_the_interval_after_the_pass_start() {
        let pacer = FramePacer::new(50.0);
        let start = Instant::now();
        pacer.begin_pass(start, PREDICTED);
        let delay = pacer.request_delay(start + Duration::from_millis(5), Duration::ZERO);
        assert_eq!(delay, Duration::from_millis(15) + PREDICTED);
    }

    #[test]
    fn request_after_an_idle_period_waits_the_minimum() {
        let pacer = FramePacer::new(60.0);
        let start = Instant::now();
        pacer.begin_pass(start, PREDICTED);
        let delay = pacer.request_delay(start + Duration::from_secs(2), Duration::ZERO);
        assert_eq!(delay, MIN_WAIT + PREDICTED);
    }

    #[test]
    fn pass_woken_late_keeps_the_grid_and_one_after_idle_restarts_it() {
        let pacer = FramePacer::new(50.0);
        let start = Instant::now();
        pacer.begin_pass(start, Duration::ZERO);
        let late = start + Duration::from_millis(21);
        pacer.begin_pass(late, Duration::ZERO);
        assert_eq!(
            pacer.request_delay(late, Duration::ZERO),
            Duration::from_millis(19)
        );
        let idle = start + Duration::from_secs(3);
        pacer.begin_pass(idle, Duration::ZERO);
        assert_eq!(
            pacer.request_delay(idle, Duration::ZERO),
            Duration::from_millis(20)
        );
    }

    #[test]
    fn longer_requested_wait_and_new_limit_apply_to_the_current_interval() {
        let pacer = FramePacer::new(50.0);
        let start = Instant::now();
        pacer.begin_pass(start, Duration::ZERO);
        let at_least = Duration::from_millis(30);
        assert_eq!(pacer.request_delay(start, at_least), at_least);
        pacer.set_fps(10.0);
        assert_eq!(
            pacer.request_delay(start + Duration::from_millis(40), Duration::ZERO),
            Duration::from_millis(60)
        );
    }
}
