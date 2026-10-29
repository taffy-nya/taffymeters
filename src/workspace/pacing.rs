//! Spaces self-scheduled host passes (data wake-ups, animation, timed polls) by the
//! frame-rate limit. Input-driven passes are not paced: input schedules them, vsync bounds them.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

pub(crate) const DEFAULT_FPS_LIMIT: f32 = 60.0;
pub(crate) const FPS_LIMIT_RANGE: (f32, f32) = (10.0, 240.0);

/// Lock-free: the analysis worker's publication waker reads it. Only the UI thread writes.
pub(crate) struct FramePacer {
    epoch: Instant,
    interval_ns: AtomicU64,
    /// Frame-grid time of the latest host pass, since `epoch`.
    tick_ns: AtomicU64,
}

impl FramePacer {
    pub(crate) fn new(fps: f32) -> Self {
        let pacer = Self {
            epoch: Instant::now(),
            interval_ns: AtomicU64::new(0),
            tick_ns: AtomicU64::new(0),
        };
        pacer.set_fps(fps);
        pacer
    }

    pub(crate) fn set_fps(&self, fps: f32) {
        let interval = Duration::from_secs_f64(1.0 / f64::from(fps));
        self.interval_ns.store(nanos(interval), Ordering::Relaxed);
    }

    pub(crate) fn begin_pass(&self, now: Instant) {
        let now = self.since_epoch(now);
        let interval = self.interval_ns.load(Ordering::Relaxed);
        let due = self
            .tick_ns
            .load(Ordering::Relaxed)
            .saturating_add(interval);
        // A pass woken late keeps the grid, so timer latency does not lower the rate; so does
        // one within the slack before its slot. Earlier (input) passes and passes after idle
        // restart it.
        let tick =
            if now.saturating_add(slack(interval)) >= due && now.saturating_sub(due) < interval {
                due
            } else {
                now
            };
        self.tick_ns.store(tick, Ordering::Relaxed);
    }

    /// When the next paced pass may start, at least `at_least` after `now`.
    pub(crate) fn next_pass_at(&self, now: Instant, at_least: Duration) -> Instant {
        let interval = self.interval_ns.load(Ordering::Relaxed);
        let due = self
            .tick_ns
            .load(Ordering::Relaxed)
            .saturating_add(interval)
            .saturating_sub(slack(interval));
        let grid = self.epoch + Duration::from_nanos(due);
        grid.max(now + at_least)
    }

    fn since_epoch(&self, now: Instant) -> u64 {
        nanos(now.saturating_duration_since(self.epoch))
    }
}

/// How early a paced pass may start. A window's present blocks until vsync; on a display
/// at about the limit's rate, starting right after the previous present instead of waiting
/// for a grid slot that falls just before vsync keeps passes one refresh apart.
fn slack(interval: u64) -> u64 {
    interval / 4
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_pass_waits_out_the_interval_less_the_slack_after_the_pass_start() {
        let pacer = FramePacer::new(50.0);
        let start = pacer.epoch + Duration::from_secs(1);
        pacer.begin_pass(start);
        let at = pacer.next_pass_at(start + Duration::from_millis(5), Duration::ZERO);
        assert_eq!(at, start + Duration::from_millis(15));
    }

    #[test]
    fn a_pass_within_the_slack_keeps_the_grid_and_an_earlier_one_restarts_it() {
        let pacer = FramePacer::new(50.0);
        let start = pacer.epoch + Duration::from_secs(1);
        pacer.begin_pass(start);
        pacer.begin_pass(start + Duration::from_millis(16));
        assert_eq!(
            pacer.next_pass_at(start + Duration::from_millis(16), Duration::ZERO),
            start + Duration::from_millis(35)
        );
        let input = start + Duration::from_millis(20);
        pacer.begin_pass(input);
        assert_eq!(
            pacer.next_pass_at(input, Duration::ZERO),
            input + Duration::from_millis(15)
        );
    }

    #[test]
    fn next_pass_after_an_idle_period_is_immediate() {
        let pacer = FramePacer::new(60.0);
        let start = pacer.epoch + Duration::from_secs(1);
        pacer.begin_pass(start);
        let now = start + Duration::from_secs(2);
        assert_eq!(pacer.next_pass_at(now, Duration::ZERO), now);
    }

    #[test]
    fn pass_woken_late_keeps_the_grid_and_one_after_idle_restarts_it() {
        let pacer = FramePacer::new(50.0);
        let start = pacer.epoch + Duration::from_secs(1);
        pacer.begin_pass(start);
        let late = start + Duration::from_millis(21);
        pacer.begin_pass(late);
        assert_eq!(
            pacer.next_pass_at(late, Duration::ZERO),
            start + Duration::from_millis(35)
        );
        let idle = start + Duration::from_secs(3);
        pacer.begin_pass(idle);
        assert_eq!(
            pacer.next_pass_at(idle, Duration::ZERO),
            idle + Duration::from_millis(15)
        );
    }

    #[test]
    fn longer_requested_wait_and_new_limit_apply_to_the_current_interval() {
        let pacer = FramePacer::new(50.0);
        let start = pacer.epoch + Duration::from_secs(1);
        pacer.begin_pass(start);
        let at_least = Duration::from_millis(30);
        assert_eq!(pacer.next_pass_at(start, at_least), start + at_least);
        pacer.set_fps(10.0);
        assert_eq!(
            pacer.next_pass_at(start + Duration::from_millis(40), Duration::ZERO),
            start + Duration::from_millis(75)
        );
    }
}
