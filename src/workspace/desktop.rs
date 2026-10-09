//! Desktop geometry shared by all windows. Everything here is in physical pixels;
//! each window converts with its own `pixels_per_point` at the boundary.

use eframe::egui::{Pos2, Rect, Vec2, pos2, vec2};

/// Snap distance in logical points of the window being moved.
pub(crate) const SNAP_DISTANCE_POINTS: f32 = 12.0;
/// Two outer frames closer than this (physical pixels) count as touching.
pub(crate) const TOUCH_TOLERANCE_PX: f32 = 2.0;

/// Facts about the desktop the egui viewport info does not carry.
pub(crate) trait DesktopProbe {
    /// Whether the primary mouse button is currently held anywhere on the desktop.
    fn primary_button_down(&self) -> bool;
    /// Work area of the monitor containing `point` (physical pixels).
    fn work_area(&self, point: Pos2) -> Option<Rect>;
}

/// The native desktop, queried through Win32.
pub(crate) struct NativeDesktop;

#[cfg(windows)]
impl DesktopProbe for NativeDesktop {
    fn primary_button_down(&self) -> bool {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
        // SAFETY: GetAsyncKeyState has no preconditions.
        let state = unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON)) };
        state < 0
    }

    fn work_area(&self, point: Pos2) -> Option<Rect> {
        use windows_sys::Win32::{
            Foundation::POINT,
            Graphics::Gdi::{
                GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
            },
        };
        let point = POINT {
            x: point.x.round() as i32,
            y: point.y.round() as i32,
        };
        // SAFETY: MonitorFromPoint takes a value and returns a handle or null.
        let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };
        if monitor.is_null() {
            return None;
        }
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            rcMonitor: unsafe { std::mem::zeroed() },
            rcWork: unsafe { std::mem::zeroed() },
            dwFlags: 0,
        };
        // SAFETY: `info` is a valid MONITORINFO with cbSize set.
        if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
            return None;
        }
        let work = info.rcWork;
        Some(Rect::from_min_max(
            pos2(work.left as f32, work.top as f32),
            pos2(work.right as f32, work.bottom as f32),
        ))
    }
}

#[cfg(not(windows))]
impl DesktopProbe for NativeDesktop {
    fn primary_button_down(&self) -> bool {
        false
    }

    fn work_area(&self, _point: Pos2) -> Option<Rect> {
        None
    }
}

/// Best offset (per axis) that aligns `moving` with an edge of another window or of
/// the work area, when within `threshold`. Other windows only count when they are
/// within `threshold` of the moving window on both axes.
pub(crate) fn snap_offset(
    moving: Rect,
    others: &[Rect],
    work_area: Option<Rect>,
    threshold: f32,
) -> Vec2 {
    let mut best_x: Option<f32> = None;
    let mut best_y: Option<f32> = None;
    let consider = |best: &mut Option<f32>, delta: f32| {
        if delta.abs() <= threshold && best.is_none_or(|current| delta.abs() < current.abs()) {
            *best = Some(delta);
        }
    };
    for other in others {
        // Only windows within the threshold on both axes interact: side by side they
        // offer adjacent x edges and aligned y edges, stacked the converse.
        let near = moving.min.x <= other.max.x + threshold
            && other.min.x <= moving.max.x + threshold
            && moving.min.y <= other.max.y + threshold
            && other.min.y <= moving.max.y + threshold;
        if !near {
            continue;
        }
        for (edge, target) in [
            (moving.min.x, other.max.x),
            (moving.max.x, other.min.x),
            (moving.min.x, other.min.x),
            (moving.max.x, other.max.x),
        ] {
            consider(&mut best_x, target - edge);
        }
        for (edge, target) in [
            (moving.min.y, other.max.y),
            (moving.max.y, other.min.y),
            (moving.min.y, other.min.y),
            (moving.max.y, other.max.y),
        ] {
            consider(&mut best_y, target - edge);
        }
    }
    if let Some(area) = work_area {
        for (edge, target) in [(moving.min.x, area.min.x), (moving.max.x, area.max.x)] {
            consider(&mut best_x, target - edge);
        }
        for (edge, target) in [(moving.min.y, area.min.y), (moving.max.y, area.max.y)] {
            consider(&mut best_y, target - edge);
        }
    }
    vec2(best_x.unwrap_or(0.0), best_y.unwrap_or(0.0))
}

/// Whether two outer frames share an edge segment.
pub(crate) fn touching(a: Rect, b: Rect) -> bool {
    let t = TOUCH_TOLERANCE_PX;
    let overlap_y = a.min.y < b.max.y - t && b.min.y < a.max.y - t;
    let overlap_x = a.min.x < b.max.x - t && b.min.x < a.max.x - t;
    let side_by_side =
        overlap_y && ((a.max.x - b.min.x).abs() <= t || (b.max.x - a.min.x).abs() <= t);
    let stacked = overlap_x && ((a.max.y - b.min.y).abs() <= t || (b.max.y - a.min.y).abs() <= t);
    side_by_side || stacked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(x, y), vec2(w, h))
    }

    #[test]
    fn snaps_to_nearest_window_edge_and_work_area_independently_per_axis() {
        let other = rect(0.0, 0.0, 400.0, 300.0);
        let moving = rect(410.0, 7.0, 200.0, 100.0);
        assert_eq!(snap_offset(moving, &[other], None, 24.0), vec2(-10.0, -7.0));
        // Beyond the threshold: no snap.
        let far = rect(450.0, 40.0, 200.0, 100.0);
        assert_eq!(snap_offset(far, &[other], None, 24.0), Vec2::ZERO);
        // Not overlapping vertically: the other's right edge is not a target.
        let below = rect(410.0, 500.0, 200.0, 100.0);
        assert_eq!(snap_offset(below, &[other], None, 24.0), Vec2::ZERO);
        // Side by side just beyond the x threshold: nothing to align the tops with.
        let beside = rect(429.0, 9.0, 200.0, 100.0);
        assert_eq!(snap_offset(beside, &[other], None, 24.0), Vec2::ZERO);
        // Side by side within it: adjacent x edge and aligned top.
        let beside = rect(415.0, 9.0, 200.0, 100.0);
        assert_eq!(snap_offset(beside, &[other], None, 24.0), vec2(-15.0, -9.0));
        let work = rect(-1920.0, 0.0, 1920.0, 1040.0);
        let near_corner = rect(-1910.0, 935.0, 200.0, 100.0);
        assert_eq!(
            snap_offset(near_corner, &[], Some(work), 24.0),
            vec2(-10.0, 5.0)
        );
    }

    #[test]
    fn touching_requires_shared_edge_segment() {
        let a = rect(0.0, 0.0, 100.0, 100.0);
        assert!(touching(a, rect(101.0, 20.0, 50.0, 50.0)));
        assert!(touching(a, rect(10.0, 100.0, 50.0, 50.0)));
        assert!(!touching(a, rect(105.0, 20.0, 50.0, 50.0)));
        assert!(!touching(a, rect(100.0, 100.0, 50.0, 50.0)), "corner only");
    }
}
