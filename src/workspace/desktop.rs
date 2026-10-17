//! Desktop geometry shared by all windows. Everything here is in physical pixels;
//! each window converts with its own `pixels_per_point` at the boundary. Saved
//! frames are the exception: logical points of their display, relative to its
//! work area, so a saved arrangement fits displays of another size or scale.

use eframe::egui::{Pos2, Rect, Vec2, pos2, vec2};

/// Snap distance in logical points of the window being moved.
pub(crate) const SNAP_DISTANCE_POINTS: f32 = 12.0;
/// Two outer frames closer than this (physical pixels) count as touching.
pub(crate) const TOUCH_TOLERANCE_PX: f32 = 2.0;
/// A window shows at least this much of itself (logical points per axis, or all
/// of a smaller window) on a monitor to count as reachable.
pub(crate) const MIN_VISIBLE_POINTS: f32 = 32.0;

/// Facts about the desktop the egui viewport info does not carry.
pub(crate) trait DesktopProbe {
    /// Whether the primary mouse button is currently held anywhere on the desktop.
    fn primary_button_down(&self) -> bool;
    /// Work area of the monitor containing `point` (physical pixels).
    fn work_area(&self, point: Pos2) -> Option<Rect>;
    /// Every monitor (empty when the platform does not tell).
    fn displays(&self) -> Vec<Display>;
}

/// A monitor as saved windows are placed on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Display {
    /// Work area, physical pixels.
    pub(crate) work: Rect,
    /// Physical pixels per logical point.
    pub(crate) scale: f32,
    pub(crate) primary: bool,
}

impl Display {
    /// Size of the work area in this display's logical points.
    pub(crate) fn logical_size(&self) -> Vec2 {
        self.work.size() / self.scale
    }
}

/// Stands in when the desktop reports no monitor.
pub(crate) const FALLBACK_DISPLAY: Display = Display {
    work: Rect::from_min_max(pos2(0.0, 0.0), pos2(1920.0, 1040.0)),
    scale: 1.0,
    primary: true,
};

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

    fn displays(&self) -> Vec<Display> {
        use windows_sys::{
            Win32::{
                Foundation::{LPARAM, RECT},
                Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO},
                UI::HiDpi::{
                    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor,
                    MDT_EFFECTIVE_DPI, SetProcessDpiAwarenessContext,
                },
            },
            core::BOOL,
        };
        unsafe extern "system" fn collect(
            monitor: HMONITOR,
            _: HDC,
            _: *mut RECT,
            data: LPARAM,
        ) -> BOOL {
            // SAFETY: `data` is the `Vec` passed below, alive for the whole call.
            let monitors = unsafe { &mut *(data as *mut Vec<HMONITOR>) };
            monitors.push(monitor);
            1
        }
        // Monitor geometry and DPI come unscaled only to a DPI-aware process. winit
        // makes the process per-monitor aware when its event loop starts, which is
        // after the session is first restored; this is the same call, made first.
        static DPI_AWARE: std::sync::Once = std::sync::Once::new();
        // SAFETY: takes a constant awareness context; failing leaves it unchanged.
        DPI_AWARE.call_once(|| unsafe {
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        });
        let mut monitors: Vec<HMONITOR> = Vec::new();
        // SAFETY: no device context or clip; the callback only pushes into
        // `monitors`, which outlives the call.
        unsafe {
            EnumDisplayMonitors(
                std::ptr::null_mut(),
                std::ptr::null(),
                Some(collect),
                &mut monitors as *mut Vec<HMONITOR> as LPARAM,
            )
        };
        monitors
            .into_iter()
            .filter_map(|monitor| {
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
                let (mut dpi_x, mut dpi_y) = (96, 96);
                // SAFETY: valid monitor handle and output pointers.
                let scale = if unsafe {
                    GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y)
                } == 0
                {
                    dpi_x as f32 / 96.0
                } else {
                    1.0
                };
                let work = info.rcWork;
                let bounds = info.rcMonitor;
                Some(Display {
                    work: Rect::from_min_max(
                        pos2(work.left as f32, work.top as f32),
                        pos2(work.right as f32, work.bottom as f32),
                    ),
                    scale,
                    // The primary monitor holds the desktop origin.
                    primary: bounds.left == 0 && bounds.top == 0,
                })
            })
            .collect()
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

    fn displays(&self) -> Vec<Display> {
        Vec::new()
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
    touching_within(a, b, TOUCH_TOLERANCE_PX)
}

/// Whether two frames share an edge segment, edges `tolerance` apart counting as
/// one.
fn touching_within(a: Rect, b: Rect, tolerance: f32) -> bool {
    let t = tolerance;
    let overlap_y = a.min.y < b.max.y - t && b.min.y < a.max.y - t;
    let overlap_x = a.min.x < b.max.x - t && b.min.x < a.max.x - t;
    let side_by_side =
        overlap_y && ((a.max.x - b.min.x).abs() <= t || (b.max.x - a.min.x).abs() <= t);
    let stacked = overlap_x && ((a.max.y - b.min.y).abs() <= t || (b.max.y - a.min.y).abs() <= t);
    side_by_side || stacked
}

/// New top-left for `outer` inside `work` when less than `min_visible` (or the
/// whole window, if smaller) of it is inside `work` on either axis; `None` when
/// it is reachable where it is. Windows larger than `work` keep their top-left
/// edge inside it.
pub(crate) fn reachable_position(outer: Rect, work: Rect, min_visible: f32) -> Option<Pos2> {
    let visible = outer.intersect(work);
    let visible_x = visible.width().max(0.0);
    let visible_y = visible.height().max(0.0);
    if visible_x + 0.5 >= min_visible.min(outer.width())
        && visible_y + 0.5 >= min_visible.min(outer.height())
    {
        return None;
    }
    Some(pos2(
        outer
            .min
            .x
            .clamp(work.min.x, (work.max.x - outer.width()).max(work.min.x)),
        outer
            .min
            .y
            .clamp(work.min.y, (work.max.y - outer.height()).max(work.min.y)),
    ))
}

/// Index of the display whose work area contains `point`, else the nearest one.
pub(crate) fn display_at(displays: &[Display], point: Pos2) -> Option<usize> {
    displays
        .iter()
        .position(|display| display.work.contains(point))
        .or_else(|| {
            (0..displays.len()).min_by(|a, b| {
                let distance = |index: usize| displays[index].work.distance_sq_to_pos(point);
                distance(*a).total_cmp(&distance(*b))
            })
        })
}

/// The saved frame of `outer` (physical pixels) on `display`: logical points from
/// the top-left of its work area. An edge resting on a work-area edge (within the
/// touch tolerance inside, or beyond it by less than the snap distance) is saved
/// exactly on it, so the window is recognized as attached there.
pub(crate) fn saved_frame(display: &Display, outer: Rect) -> Rect {
    let size = display.logical_size();
    let inside = TOUCH_TOLERANCE_PX / display.scale;
    let start = |edge: f32| {
        if edge > -SNAP_DISTANCE_POINTS && edge <= inside {
            0.0
        } else {
            edge
        }
    };
    let end = |edge: f32, end: f32| {
        if edge >= end - inside && edge < end + SNAP_DISTANCE_POINTS {
            end
        } else {
            edge
        }
    };
    let min = (outer.min - display.work.min) / display.scale;
    let max = (outer.max - display.work.min) / display.scale;
    Rect::from_min_max(
        pos2(start(min.x), start(min.y)),
        pos2(end(max.x, size.x), end(max.y, size.y)),
    )
}

/// A display as saved: its work area size (logical points) and, when known, the
/// physical origin of that work area and its scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SavedDisplay {
    pub(crate) size: Vec2,
    pub(crate) hint: Option<(Pos2, f32)>,
}

/// A window as saved: its display (index into the saved displays), frame, group
/// number and the minimum size of its layout (logical points).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SavedWindow {
    pub(crate) display: usize,
    pub(crate) frame: Rect,
    pub(crate) group: Option<u32>,
    pub(crate) min: Vec2,
}

/// The current display each saved display goes to. A display the hint identifies
/// (same work-area origin, size and scale) is used again; otherwise the first
/// saved display goes to `current` and the others to the remaining displays from
/// left to right, or to `current` when none is left.
pub(crate) fn match_displays(
    saved: &[SavedDisplay],
    displays: &[Display],
    current: usize,
) -> Vec<usize> {
    let mut matched: Vec<Option<usize>> = saved
        .iter()
        .map(|saved| {
            let (origin, scale) = saved.hint?;
            displays.iter().position(|display| {
                (display.work.min - origin).length() <= 0.5
                    && (display.scale - scale).abs() < 1e-3
                    && (display.logical_size() - saved.size).abs().max_elem() <= 0.5
            })
        })
        .collect();
    // The first saved display takes the current one unless it was recognized.
    if let Some(first) = matched.first_mut() {
        first.get_or_insert(current);
    }
    let mut free: Vec<usize> = (0..displays.len())
        .filter(|index| !matched.contains(&Some(*index)))
        .collect();
    free.sort_by(|a, b| {
        let (a, b) = (displays[*a].work.min, displays[*b].work.min);
        a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y))
    });
    let mut free = free.into_iter();
    for slot in &mut matched {
        if slot.is_none() {
            *slot = Some(free.next().unwrap_or(current));
        }
    }
    matched
        .into_iter()
        .map(|slot| slot.unwrap_or(current))
        .collect()
}

/// Outer frames (physical pixels) of saved windows on the displays `targets`
/// (one per saved display). Windows of one saved display that touch (within the
/// touch tolerance of that display), or share a group, form a cluster that is
/// mapped as a whole, per axis: attached to both work-area edges it stretches
/// proportionally; attached to one edge it keeps its distance to that edge and
/// its size; attached to neither it keeps its size and the relative position of
/// its center. A cluster larger than the target shrinks by one factor on its
/// unstretched axes (keeping its proportions), but never below a member's
/// minimum size; then it moves inside (its top-left first). Every edge is rounded
/// to a physical pixel on its own, so edges that coincided still coincide.
pub(crate) fn place(
    saved: &[SavedDisplay],
    windows: &[SavedWindow],
    targets: &[Display],
) -> Vec<Rect> {
    /// A cluster edge this close (logical points) to a work-area edge rests on it;
    /// saved frames put such edges exactly there (`saved_frame`).
    const ATTACHED: f32 = 0.5;
    // Union-find over the windows.
    let mut parent: Vec<usize> = (0..windows.len()).collect();
    fn root(parent: &mut [usize], mut index: usize) -> usize {
        while parent[index] != index {
            parent[index] = parent[parent[index]];
            index = parent[index];
        }
        index
    }
    for a in 0..windows.len() {
        for b in a + 1..windows.len() {
            let (wa, wb) = (&windows[a], &windows[b]);
            // Windows touch at runtime within a few physical pixels.
            let tolerance = saved[wa.display].hint.map_or(ATTACHED, |(_, scale)| {
                (TOUCH_TOLERANCE_PX / scale).max(ATTACHED)
            });
            if wa.display == wb.display
                && (touching_within(wa.frame, wb.frame, tolerance)
                    || (wa.group.is_some() && wa.group == wb.group))
            {
                let (ra, rb) = (root(&mut parent, a), root(&mut parent, b));
                parent[ra] = rb;
            }
        }
    }
    /// x' = scale·x + offset on one axis.
    #[derive(Clone, Copy)]
    struct AxisMap {
        scale: f32,
        offset: f32,
        stretched: bool,
    }
    let attach = |low: f32, high: f32, from: f32, to: f32| -> AxisMap {
        let (start, end) = (low <= ATTACHED, high >= from - ATTACHED);
        let (scale, offset) = match (start, end) {
            (true, true) => (to / from, 0.0),
            (true, false) => (1.0, 0.0),
            (false, true) => (1.0, to - from),
            (false, false) => {
                let center = (low + high) / 2.0;
                (1.0, center * to / from - center)
            }
        };
        AxisMap {
            scale,
            offset,
            stretched: start && end,
        }
    };
    let mut placed = vec![Rect::NOTHING; windows.len()];
    for cluster in 0..windows.len() {
        let members: Vec<usize> = (0..windows.len())
            .filter(|index| root(&mut parent, *index) == cluster)
            .collect();
        let Some(first) = members.first() else {
            continue;
        };
        let display = windows[*first].display;
        let from = saved[display].size;
        let target = &targets[display];
        let to = target.logical_size();
        let bounds = members.iter().fold(Rect::NOTHING, |bounds, index| {
            bounds.union(windows[*index].frame)
        });
        let mut axes = [
            attach(bounds.min.x, bounds.max.x, from.x, to.x),
            attach(bounds.min.y, bounds.max.y, from.y, to.y),
        ];
        let (low, high, to) = (
            [bounds.min.x, bounds.min.y],
            [bounds.max.x, bounds.max.y],
            [to.x, to.y],
        );
        // One shrink factor keeps the proportions of what is not stretched.
        let fit = (0..2)
            .filter(|axis| !axes[*axis].stretched)
            .map(|axis| to[axis] / (axes[axis].scale * (high[axis] - low[axis])))
            .fold(1.0_f32, f32::min);
        for (axis, map) in axes.iter_mut().enumerate() {
            if !map.stretched {
                map.scale *= fit;
                map.offset *= fit;
            }
            // No member below its layout's minimum: the cluster may then exceed the
            // target, but its windows keep touching.
            let floor = members
                .iter()
                .map(|index| {
                    let window = &windows[*index];
                    let (size, min) = match axis {
                        0 => (window.frame.width(), window.min.x),
                        _ => (window.frame.height(), window.min.y),
                    };
                    min / size
                })
                .fold(0.0_f32, f32::max);
            if map.scale < floor {
                let start = map.scale * low[axis] + map.offset;
                map.scale = floor;
                map.offset = start - floor * low[axis];
            }
            let (start, end) = (
                map.scale * low[axis] + map.offset,
                map.scale * high[axis] + map.offset,
            );
            if start < 0.0 {
                map.offset -= start;
            } else if end > to[axis] {
                map.offset -= (end - to[axis]).min(start);
            }
        }
        let [ax, ay] = axes;
        let x = |v: f32| (target.work.min.x + (ax.scale * v + ax.offset) * target.scale).round();
        let y = |v: f32| (target.work.min.y + (ay.scale * v + ay.offset) * target.scale).round();
        for index in members {
            let frame = windows[index].frame;
            placed[index] = Rect::from_min_max(
                pos2(x(frame.min.x), y(frame.min.y)),
                pos2(x(frame.max.x), y(frame.max.y)),
            );
        }
    }
    placed
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

    #[test]
    fn windows_off_every_monitor_come_back_but_partly_visible_ones_stay() {
        let work = rect(0.0, 0.0, 1920.0, 1040.0);
        // Disconnected monitor on the left: entirely outside.
        assert_eq!(
            reachable_position(rect(-1500.0, 200.0, 400.0, 300.0), work, 32.0),
            Some(pos2(0.0, 200.0))
        );
        // Only a sliver on screen.
        assert_eq!(
            reachable_position(rect(1910.0, 1030.0, 400.0, 300.0), work, 32.0),
            Some(pos2(1520.0, 740.0))
        );
        // Deliberately half off screen: left alone.
        assert_eq!(
            reachable_position(rect(1700.0, 100.0, 400.0, 300.0), work, 32.0),
            None
        );
        // Larger than the work area: top-left inside.
        assert_eq!(
            reachable_position(rect(-3000.0, -50.0, 2500.0, 1200.0), work, 32.0),
            Some(pos2(0.0, 0.0))
        );
    }

    fn display(x: f32, y: f32, w: f32, h: f32, scale: f32) -> Display {
        Display {
            work: rect(x, y, w, h),
            scale,
            primary: x == 0.0 && y == 0.0,
        }
    }

    fn one(size: Vec2) -> [SavedDisplay; 1] {
        [SavedDisplay { size, hint: None }]
    }

    fn window(left: f32, top: f32, right: f32, bottom: f32) -> SavedWindow {
        SavedWindow {
            display: 0,
            frame: Rect::from_min_max(pos2(left, top), pos2(right, bottom)),
            group: None,
            min: vec2(16.0, 16.0),
        }
    }

    #[test]
    fn a_bar_spans_any_work_area_and_keeps_its_logical_height() {
        let saved = one(vec2(1920.0, 1032.0));
        let top = window(0.0, 0.0, 1920.0, 112.0);
        let bottom = window(0.0, 920.0, 1920.0, 1032.0);
        // 2880×1800 at 200% with a 64 px taskbar at the bottom.
        let laptop = display(0.0, 0.0, 2880.0, 1736.0, 2.0);
        assert_eq!(
            place(&saved, &[top], &[laptop]),
            vec![rect(0.0, 0.0, 2880.0, 224.0)]
        );
        assert_eq!(
            place(&saved, &[bottom], &[laptop]),
            vec![rect(0.0, 1512.0, 2880.0, 224.0)]
        );
        // A second monitor left of the primary one, 125%.
        let left = display(-1920.0, 0.0, 1920.0, 1080.0, 1.25);
        assert_eq!(
            place(&saved, &[bottom], &[left]),
            vec![rect(-1920.0, 940.0, 1920.0, 140.0)]
        );
    }

    #[test]
    fn touching_windows_stay_touching_at_a_fractional_scale() {
        let saved = one(vec2(1920.0, 1040.0));
        let windows = [
            window(101.3, 100.0, 400.7, 300.0),
            window(400.7, 100.0, 700.1, 300.0),
            window(101.3, 300.0, 700.1, 420.5),
        ];
        let placed = place(&saved, &windows, &[display(0.0, 0.0, 2560.0, 1400.0, 1.5)]);
        assert_eq!(placed[0].max.x, placed[1].min.x);
        assert_eq!(placed[0].max.y, placed[2].min.y);
        assert_eq!(placed[0].min.x, placed[2].min.x);
        assert_eq!(placed[1].max.x, placed[2].max.x);
        // Floating: sizes kept in logical points.
        assert!((placed[2].width() - 598.8 * 1.5).abs() <= 1.0, "{placed:?}");
    }

    #[test]
    fn a_row_attached_to_both_sides_stretches_and_keeps_its_proportions() {
        let saved = one(vec2(1920.0, 1040.0));
        let windows = [
            window(0.0, 900.0, 640.0, 1040.0),
            window(640.0, 900.0, 1280.0, 1040.0),
            window(1280.0, 900.0, 1920.0, 1040.0),
        ];
        let placed = place(&saved, &windows, &[display(0.0, 0.0, 1280.0, 680.0, 1.0)]);
        assert_eq!(
            placed,
            vec![
                rect(0.0, 540.0, 427.0, 140.0),
                rect(427.0, 540.0, 426.0, 140.0),
                rect(853.0, 540.0, 427.0, 140.0),
            ]
        );
    }

    #[test]
    fn floating_windows_keep_their_size_and_relative_place() {
        let saved = one(vec2(1920.0, 1032.0));
        let centered = window(720.0, 396.0, 1200.0, 636.0);
        let placed = place(
            &saved,
            &[centered],
            &[display(0.0, 0.0, 1366.0, 728.0, 1.0)],
        );
        assert_eq!(placed, vec![rect(443.0, 244.0, 480.0, 240.0)]);
        // A window in the right third stays in the right third.
        let right = window(1400.0, 100.0, 1600.0, 300.0);
        let placed = place(&saved, &[right], &[display(0.0, 0.0, 960.0, 516.0, 1.0)]);
        assert_eq!(placed, vec![rect(650.0, 0.0, 200.0, 200.0)]);
    }

    #[test]
    fn a_cluster_larger_than_the_target_shrinks_inside_it() {
        let saved = one(vec2(1920.0, 1040.0));
        let windows = [
            window(100.0, 100.0, 1100.0, 900.0),
            window(1100.0, 100.0, 1800.0, 900.0),
        ];
        let placed = place(&saved, &windows, &[display(0.0, 0.0, 1280.0, 720.0, 1.0)]);
        assert_eq!(placed[0].max.x, placed[1].min.x);
        assert_eq!(placed[0].min.x, 0.0);
        assert_eq!(placed[1].max.x, 1280.0);
        assert!(placed.iter().all(|r| r.min.y >= 0.0 && r.max.y <= 720.0));
    }

    #[test]
    fn an_oversized_floating_window_shrinks_keeping_its_proportions() {
        let saved = one(vec2(1920.0, 1040.0));
        let placed = place(
            &saved,
            &[window(100.0, 100.0, 1100.0, 600.0)],
            &[display(0.0, 0.0, 800.0, 600.0, 1.0)],
        );
        assert_eq!(placed[0].size(), vec2(800.0, 400.0), "{placed:?}");
        assert_eq!(placed[0].min.x, 0.0);
    }

    #[test]
    fn shrinking_stops_at_minimum_sizes_so_windows_keep_touching() {
        let saved = one(vec2(1920.0, 1040.0));
        // A 16-point window beside a wide one, shrunk to half.
        let windows = [
            window(100.0, 100.0, 116.0, 300.0),
            window(116.0, 100.0, 1700.0, 300.0),
        ];
        let placed = place(&saved, &windows, &[display(0.0, 0.0, 800.0, 600.0, 1.0)]);
        assert!(placed[0].width() >= 16.0, "{placed:?}");
        assert_eq!(placed[0].max.x, placed[1].min.x);
        assert_eq!(placed[0].min.x, 0.0);
    }

    #[test]
    fn windows_touching_within_the_runtime_tolerance_form_one_cluster() {
        // 1 px apart at 100%: touching for snapping, so moved as one.
        let saved = [SavedDisplay {
            size: vec2(1920.0, 1040.0),
            hint: Some((Pos2::ZERO, 1.0)),
        }];
        let windows = [
            window(100.0, 100.0, 500.0, 400.0),
            window(501.0, 100.0, 901.0, 400.0),
        ];
        let placed = place(&saved, &windows, &[display(0.0, 0.0, 960.0, 520.0, 1.0)]);
        assert_eq!(placed[1].min.x - placed[0].max.x, 1.0, "{placed:?}");
    }

    #[test]
    fn same_work_area_restores_exactly() {
        let target = display(-1280.0, 0.0, 2560.0, 1400.0, 2.0);
        let saved = [SavedDisplay {
            size: target.logical_size(),
            hint: Some((target.work.min, 2.0)),
        }];
        let outers = [
            rect(-1280.0, 0.0, 600.0, 400.0),
            rect(-680.0, 0.0, 300.0, 400.0),
            rect(0.0, 700.0, 330.0, 222.0),
        ];
        let windows: Vec<SavedWindow> = outers
            .iter()
            .map(|outer| SavedWindow {
                display: 0,
                frame: saved_frame(&target, *outer),
                group: None,
                min: vec2(16.0, 16.0),
            })
            .collect();
        assert_eq!(place(&saved, &windows, &[target]), outers.to_vec());
    }

    #[test]
    fn saved_frames_rest_exactly_on_nearby_work_area_edges() {
        let laptop = display(0.0, 0.0, 2880.0, 1736.0, 2.0);
        // 9 px past the left edge, flush right: the hand-dragged bar.
        assert_eq!(
            saved_frame(&laptop, rect(-9.0, 0.0, 2889.0, 234.0)),
            Rect::from_min_max(pos2(0.0, 0.0), pos2(1440.0, 117.0))
        );
        // Clearly off the edge: kept as it is.
        assert_eq!(
            saved_frame(&laptop, rect(-100.0, 20.0, 200.0, 100.0)),
            Rect::from_min_max(pos2(-50.0, 10.0), pos2(50.0, 60.0))
        );
    }

    #[test]
    fn displays_match_by_hint_then_current_then_left_to_right() {
        let primary = display(0.0, 0.0, 1920.0, 1040.0, 1.0);
        let left = display(-2560.0, 0.0, 2560.0, 1400.0, 1.5);
        let right = display(1920.0, 0.0, 1920.0, 1040.0, 1.0);
        let displays = [primary, right, left];
        let unknown = SavedDisplay {
            size: vec2(1000.0, 800.0),
            hint: None,
        };
        let known_left = SavedDisplay {
            size: left.logical_size(),
            hint: Some((left.work.min, 1.5)),
        };
        // A built-in preset goes to the current display.
        assert_eq!(match_displays(&[unknown], &displays, 1), vec![1]);
        // The saved display is still there: back to it, whatever is current.
        assert_eq!(match_displays(&[known_left], &displays, 0), vec![2]);
        // Others take the free displays from left to right, then the current one.
        assert_eq!(
            match_displays(&[unknown, unknown, unknown, unknown], &displays, 0),
            vec![0, 2, 1, 0]
        );
        assert_eq!(
            match_displays(&[unknown, known_left], &displays, 2),
            vec![2, 2]
        );
        // Recognized elsewhere, the first saved display leaves the current one
        // free for the others.
        assert_eq!(
            match_displays(&[known_left, unknown], &displays, 0),
            vec![2, 0]
        );
    }
}
