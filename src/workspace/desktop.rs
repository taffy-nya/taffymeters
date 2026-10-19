//! Desktop geometry shared by all windows. Everything here is in physical pixels;
//! each window converts with its own `pixels_per_point` at the boundary. Saved
//! frames are the exception: logical points of their display, relative to its
//! work area, so a saved arrangement fits displays of another size or scale.

use eframe::egui::{Pos2, Rect, Vec2, pos2, vec2};

use super::{WindowId, moving::MoveRequest};

/// Snap distance in logical points of the window being moved.
pub(crate) const SNAP_DISTANCE_POINTS: f32 = 12.0;
/// Two outer frames closer than this (physical pixels) count as touching.
pub(crate) const TOUCH_TOLERANCE_PX: f32 = 2.0;
/// A window shows at least this much of itself (logical points per axis, or all
/// of a smaller window) on a monitor to count as reachable.
pub(crate) const MIN_VISIBLE_POINTS: f32 = 32.0;

/// The desktop beyond what egui's viewport info carries: monitors, the mouse
/// button, and moving the Workspace windows' native windows. Moves go through
/// here so a whole group moves at once, synchronously with the pointer; a
/// desktop that cannot (`false`) leaves them to egui's viewport commands.
pub(crate) trait DesktopProbe {
    /// Whether the primary mouse button is currently held anywhere on the desktop.
    fn primary_button_down(&self) -> bool;
    /// Work area of the monitor containing `point` (physical pixels).
    fn work_area(&self, point: Pos2) -> Option<Rect>;
    /// Every monitor (empty when the platform does not tell).
    fn displays(&self) -> Vec<Display>;
    /// Track the native window of a Workspace window; `false` while it cannot be
    /// found.
    fn attach(&self, _window: WindowId) -> bool {
        false
    }
    /// Windows of each group (only those that move: not maximized or minimized).
    /// Another program moving one of them moves the others by as much.
    fn set_groups(&self, _groups: &[(WindowId, u64)]) {}
    /// Outer frame (physical pixels) of a tracked window as the desktop has it
    /// now, which may be newer than the window's last pass saw.
    fn frame(&self, _window: WindowId) -> Option<Rect> {
        None
    }
    /// Move `request.members` with the pointer until the button is released;
    /// `false` when the native windows are not tracked.
    fn begin_move(&self, _request: MoveRequest) -> bool {
        false
    }
    /// A move started by `begin_move` is in progress.
    fn moving(&self) -> bool {
        false
    }
    /// Move windows (outer top-left, physical pixels) at once; their groups do
    /// not follow. `false` when one of them is not tracked.
    fn place(&self, _moves: &[(WindowId, Pos2)]) -> bool {
        false
    }
}

/// A monitor: where windows are placed and what they snap to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Display {
    /// Work area (without the taskbar and other app bars), physical pixels.
    pub(crate) work: Rect,
    /// The whole monitor, physical pixels.
    pub(crate) bounds: Rect,
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
    bounds: Rect::from_min_max(pos2(0.0, 0.0), pos2(1920.0, 1080.0)),
    scale: 1.0,
    primary: true,
};

/// The native desktop, queried through Win32; window moves go through the
/// subclassed native windows (`super::native`).
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
                    bounds: Rect::from_min_max(
                        pos2(bounds.left as f32, bounds.top as f32),
                        pos2(bounds.right as f32, bounds.bottom as f32),
                    ),
                    scale,
                    // The primary monitor holds the desktop origin.
                    primary: bounds.left == 0 && bounds.top == 0,
                })
            })
            .collect()
    }

    fn attach(&self, window: WindowId) -> bool {
        super::native::attach(window)
    }

    fn set_groups(&self, groups: &[(WindowId, u64)]) {
        super::native::set_groups(groups);
    }

    fn frame(&self, window: WindowId) -> Option<Rect> {
        super::native::frame(window)
    }

    fn begin_move(&self, request: MoveRequest) -> bool {
        super::native::begin_move(&request, self.displays())
    }

    fn moving(&self) -> bool {
        super::native::moving()
    }

    fn place(&self, moves: &[(WindowId, Pos2)]) -> bool {
        super::native::place(moves)
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

/// Best offset (per axis) that aligns an edge of one of `members` (windows moving
/// together) with an edge of another window, or with an edge of the work area or
/// of the whole monitor (the work area's edge stops at the taskbar, the monitor's
/// at the screen edge) nearest the member's center, when within `threshold`. Other
/// windows only count for a member within `threshold` of it on both axes.
pub(crate) fn snap_group(
    members: impl Iterator<Item = Rect>,
    others: &[Rect],
    displays: &[Display],
    threshold: f32,
) -> Vec2 {
    let mut best_x: Option<f32> = None;
    let mut best_y: Option<f32> = None;
    let consider = |best: &mut Option<f32>, delta: f32| {
        if delta.abs() <= threshold && best.is_none_or(|current| delta.abs() < current.abs()) {
            *best = Some(delta);
        }
    };
    for moving in members {
        for other in others {
            // Only windows within the threshold on both axes interact: side by side
            // they offer adjacent x edges and aligned y edges, stacked the converse.
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
        if let Some(display) = nearest_display(displays, moving.center()) {
            for area in [display.work, display.bounds] {
                for (edge, target) in [(moving.min.x, area.min.x), (moving.max.x, area.max.x)] {
                    consider(&mut best_x, target - edge);
                }
                for (edge, target) in [(moving.min.y, area.min.y), (moving.max.y, area.max.y)] {
                    consider(&mut best_y, target - edge);
                }
            }
        }
    }
    vec2(best_x.unwrap_or(0.0), best_y.unwrap_or(0.0))
}

/// The sides of a frame a resize moves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ResizeSides {
    pub(crate) left: bool,
    pub(crate) top: bool,
    pub(crate) right: bool,
    pub(crate) bottom: bool,
}

/// `start` with its moving `sides` moved by the pointer's `delta`, each kept far
/// enough from the opposite edge for the frame to be at least `min`.
pub(crate) fn resize_frame(start: Rect, sides: ResizeSides, delta: Vec2, min: Vec2) -> Rect {
    let mut frame = start;
    if sides.left {
        frame.min.x = (start.min.x + delta.x).min(start.max.x - min.x);
    }
    if sides.right {
        frame.max.x = (start.max.x + delta.x).max(start.min.x + min.x);
    }
    if sides.top {
        frame.min.y = (start.min.y + delta.y).min(start.max.y - min.y);
    }
    if sides.bottom {
        frame.max.y = (start.max.y + delta.y).max(start.min.y + min.y);
    }
    frame
}

/// `frame` as a resize of its `sides` leaves it, each moving edge put on the
/// nearest edge (within `threshold`) of another window or of the work area nearest
/// the frame's center: adjacent or aligned, as when moving. Not the monitor's
/// edges: Windows keeps the pointer inside the work area while resizing, and
/// releasing it on the work area's edge maximizes vertically. Other windows only
/// count within `threshold` of the frame on both axes. Snaps that would make the
/// frame smaller than `min` are left out; the other edges stay.
pub(crate) fn snap_resize(
    frame: Rect,
    sides: ResizeSides,
    others: &[Rect],
    displays: &[Display],
    threshold: f32,
    min: Vec2,
) -> Rect {
    let near = |other: &&Rect| {
        frame.min.x <= other.max.x + threshold
            && other.min.x <= frame.max.x + threshold
            && frame.min.y <= other.max.y + threshold
            && other.min.y <= frame.max.y + threshold
    };
    let area = nearest_display(displays, frame.center()).map(|display| display.work);
    let nearest =
        |edge: f32, targets: &mut dyn Iterator<Item = f32>, fits: &dyn Fn(f32) -> bool| {
            targets
                .filter(|target| (target - edge).abs() <= threshold && fits(*target))
                .min_by(|a, b| (a - edge).abs().total_cmp(&(b - edge).abs()))
                .unwrap_or(edge)
        };
    let mut snapped = frame;
    if sides.left {
        let mut targets = others
            .iter()
            .filter(near)
            .flat_map(|other| [other.max.x, other.min.x])
            .chain(area.map(|area| area.min.x));
        snapped.min.x = nearest(frame.min.x, &mut targets, &|x| frame.max.x - x >= min.x);
    }
    if sides.right {
        let mut targets = others
            .iter()
            .filter(near)
            .flat_map(|other| [other.min.x, other.max.x])
            .chain(area.map(|area| area.max.x));
        snapped.max.x = nearest(frame.max.x, &mut targets, &|x| x - frame.min.x >= min.x);
    }
    if sides.top {
        let mut targets = others
            .iter()
            .filter(near)
            .flat_map(|other| [other.max.y, other.min.y])
            .chain(area.map(|area| area.min.y));
        snapped.min.y = nearest(frame.min.y, &mut targets, &|y| frame.max.y - y >= min.y);
    }
    if sides.bottom {
        let mut targets = others
            .iter()
            .filter(near)
            .flat_map(|other| [other.min.y, other.max.y])
            .chain(area.map(|area| area.max.y));
        snapped.max.y = nearest(frame.max.y, &mut targets, &|y| y - frame.min.y >= min.y);
    }
    snapped
}

/// Shift (per axis, as small as possible) after which every one of `members` (a
/// frame and how much of it must show) shows at least that much of itself (or all
/// of a smaller window) inside the work area nearest its center. When they cannot
/// all, earlier members win. Zero when they all do already.
pub(crate) fn keep_reachable(
    members: impl Iterator<Item = (Rect, f32)>,
    displays: &[Display],
) -> Vec2 {
    let mut x = (f32::NEG_INFINITY, f32::INFINITY);
    let mut y = (f32::NEG_INFINITY, f32::INFINITY);
    // Allowed shifts of the span [min, max] so that `visible` of it lies in
    // [low, high], intersected with those of the earlier members.
    let narrow =
        |range: &mut (f32, f32), min_visible: f32, min: f32, max: f32, low: f32, high: f32| {
            let visible = min_visible.min(max - min);
            let allowed = (low + visible - max, high - visible - min);
            let both = (range.0.max(allowed.0), range.1.min(allowed.1));
            if allowed.0 <= allowed.1 && both.0 <= both.1 {
                *range = both;
            }
        };
    for (member, min_visible) in members {
        let Some(work) = nearest_display(displays, member.center()).map(|display| display.work)
        else {
            continue;
        };
        narrow(
            &mut x,
            min_visible,
            member.min.x,
            member.max.x,
            work.min.x,
            work.max.x,
        );
        narrow(
            &mut y,
            min_visible,
            member.min.y,
            member.max.y,
            work.min.y,
            work.max.y,
        );
    }
    vec2(0.0_f32.clamp(x.0, x.1), 0.0_f32.clamp(y.0, y.1))
}

/// The display whose work area contains `point`, else the nearest one.
fn nearest_display(displays: &[Display], point: Pos2) -> Option<&Display> {
    display_at(displays, point).map(|index| &displays[index])
}

/// Index of the area (of `count`) containing `point`, else of the nearest one.
fn nearest_index(count: usize, area: impl Fn(usize) -> Rect, point: Pos2) -> Option<usize> {
    (0..count)
        .find(|index| area(*index).contains(point))
        .or_else(|| {
            (0..count).min_by(|a, b| {
                area(*a)
                    .distance_sq_to_pos(point)
                    .total_cmp(&area(*b).distance_sq_to_pos(point))
            })
        })
}

/// A side of a window frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Edge {
    Left,
    Top,
    Right,
    Bottom,
}

impl Edge {
    pub(crate) const ALL: [Self; 4] = [Self::Left, Self::Top, Self::Right, Self::Bottom];

    /// The span of `frame` along this edge.
    pub(crate) fn span(self, frame: Rect) -> (f32, f32) {
        match self {
            Self::Left | Self::Right => (frame.min.y, frame.max.y),
            Self::Top | Self::Bottom => (frame.min.x, frame.max.x),
        }
    }
}

/// Where `other` touches `own` (edges at most the touch tolerance apart, sharing
/// more than a corner): the edge of `own` and the shared span along it.
pub(crate) fn shared_edge(own: Rect, other: Rect) -> Option<(Edge, f32, f32)> {
    shared_edge_within(own, other, TOUCH_TOLERANCE_PX)
}

fn shared_edge_within(own: Rect, other: Rect, tolerance: f32) -> Option<(Edge, f32, f32)> {
    let t = tolerance;
    let along_y = (own.min.y.max(other.min.y), own.max.y.min(other.max.y));
    let along_x = (own.min.x.max(other.min.x), own.max.x.min(other.max.x));
    if along_y.1 - along_y.0 > t {
        if (own.max.x - other.min.x).abs() <= t {
            return Some((Edge::Right, along_y.0, along_y.1));
        }
        if (other.max.x - own.min.x).abs() <= t {
            return Some((Edge::Left, along_y.0, along_y.1));
        }
    }
    if along_x.1 - along_x.0 > t {
        if (own.max.y - other.min.y).abs() <= t {
            return Some((Edge::Bottom, along_x.0, along_x.1));
        }
        if (other.max.y - own.min.y).abs() <= t {
            return Some((Edge::Top, along_x.0, along_x.1));
        }
    }
    None
}

/// Whether two outer frames share an edge segment.
pub(crate) fn touching(a: Rect, b: Rect) -> bool {
    shared_edge(a, b).is_some()
}

/// Whether two frames share an edge segment, edges `tolerance` apart counting as
/// one.
fn touching_within(a: Rect, b: Rect, tolerance: f32) -> bool {
    shared_edge_within(a, b, tolerance).is_some()
}

/// The parts of `span` that none of `covered` (spans, in any order) covers, in
/// order.
pub(crate) fn uncovered<I>(
    span: (f32, f32),
    covered: impl Fn() -> I,
    mut each: impl FnMut(f32, f32),
) where
    I: Iterator<Item = (f32, f32)>,
{
    let mut at = span.0;
    while at < span.1 {
        let next = covered()
            .filter(|(_, end)| *end > at)
            .min_by(|a, b| a.0.total_cmp(&b.0));
        match next {
            Some((start, end)) => {
                if start > at {
                    each(at, start.min(span.1));
                }
                at = at.max(end);
            }
            None => {
                each(at, span.1);
                break;
            }
        }
    }
}

/// New top-left for `outer` inside `work` when less than `min_visible` (or the
/// whole window, if smaller) of it is inside `work` on either axis; `None` when
/// it is reachable where it is. Windows larger than `work` keep their top-left
/// edge inside it.
pub(crate) fn reachable_position(outer: Rect, work: Rect, min_visible: f32) -> Option<Pos2> {
    (!reachable(outer, work, min_visible)).then(|| fit_inside(outer, work))
}

/// Whether at least `min_visible` of `outer` (or all of a smaller window) is inside
/// `work` on both axes.
pub(crate) fn reachable(outer: Rect, work: Rect, min_visible: f32) -> bool {
    let visible = outer.intersect(work);
    visible.width().max(0.0) + 0.5 >= min_visible.min(outer.width())
        && visible.height().max(0.0) + 0.5 >= min_visible.min(outer.height())
}

/// Top-left that puts `outer` inside `work`, or its top-left edge when it is
/// larger.
pub(crate) fn fit_inside(outer: Rect, work: Rect) -> Pos2 {
    pos2(
        outer
            .min
            .x
            .clamp(work.min.x, (work.max.x - outer.width()).max(work.min.x)),
        outer
            .min
            .y
            .clamp(work.min.y, (work.max.y - outer.height()).max(work.min.y)),
    )
}

/// Index of the display whose work area contains `point`, else the nearest one.
pub(crate) fn display_at(displays: &[Display], point: Pos2) -> Option<usize> {
    nearest_index(displays.len(), |index| displays[index].work, point)
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

    /// Displays whose monitor is just their work area.
    fn screens(areas: &[Rect]) -> Vec<Display> {
        areas
            .iter()
            .map(|area| Display {
                work: *area,
                bounds: *area,
                scale: 1.0,
                primary: area.min == Pos2::ZERO,
            })
            .collect()
    }

    fn snap(moving: Rect, others: &[Rect], work: &[Rect]) -> Vec2 {
        snap_group(std::iter::once(moving), others, &screens(work), 24.0)
    }

    #[test]
    fn snaps_to_nearest_window_edge_and_work_area_independently_per_axis() {
        let other = rect(0.0, 0.0, 400.0, 300.0);
        let moving = rect(410.0, 7.0, 200.0, 100.0);
        assert_eq!(snap(moving, &[other], &[]), vec2(-10.0, -7.0));
        // Beyond the threshold: no snap.
        let far = rect(450.0, 40.0, 200.0, 100.0);
        assert_eq!(snap(far, &[other], &[]), Vec2::ZERO);
        // Not overlapping vertically: the other's right edge is not a target.
        let below = rect(410.0, 500.0, 200.0, 100.0);
        assert_eq!(snap(below, &[other], &[]), Vec2::ZERO);
        // Side by side just beyond the x threshold: nothing to align the tops with.
        let beside = rect(429.0, 9.0, 200.0, 100.0);
        assert_eq!(snap(beside, &[other], &[]), Vec2::ZERO);
        // Side by side within it: adjacent x edge and aligned top.
        let beside = rect(415.0, 9.0, 200.0, 100.0);
        assert_eq!(snap(beside, &[other], &[]), vec2(-15.0, -9.0));
        // The work area of the window's own monitor, not of another one.
        let work = rect(-1920.0, 0.0, 1920.0, 1040.0);
        let primary = rect(0.0, 0.0, 1920.0, 1040.0);
        let near_corner = rect(-1910.0, 935.0, 200.0, 100.0);
        assert_eq!(snap(near_corner, &[], &[primary, work]), vec2(-10.0, 5.0));
    }

    #[test]
    fn resizing_follows_the_pointer_down_to_the_window_minimum() {
        let start = rect(100.0, 100.0, 400.0, 300.0);
        let min = vec2(32.0, 32.0);
        let corner = ResizeSides {
            right: true,
            bottom: true,
            ..Default::default()
        };
        // Smaller than the system's 262×71 floor for captioned windows.
        assert_eq!(
            resize_frame(start, corner, vec2(-300.0, -250.0), min),
            rect(100.0, 100.0, 100.0, 50.0)
        );
        // Past the minimum: it stops there, the fixed edges stay.
        assert_eq!(
            resize_frame(start, corner, vec2(-600.0, -600.0), min),
            rect(100.0, 100.0, 32.0, 32.0)
        );
        let left = ResizeSides {
            left: true,
            ..Default::default()
        };
        assert_eq!(
            resize_frame(start, left, vec2(390.0, 50.0), min),
            Rect::from_min_max(pos2(468.0, 100.0), pos2(500.0, 400.0))
        );
        assert_eq!(
            resize_frame(start, left, vec2(-50.0, 0.0), min),
            Rect::from_min_max(pos2(50.0, 100.0), pos2(500.0, 400.0))
        );
    }

    #[test]
    fn resizing_snaps_only_the_moving_edges_and_keeps_the_minimum() {
        let work = [rect(0.0, 0.0, 1920.0, 1040.0)];
        // `above` ends 10 px above the frame and 10 px right of its right edge.
        let above = rect(0.0, 100.0, 600.0, 290.0);
        let far = rect(1500.0, 100.0, 300.0, 300.0);
        let frame = rect(100.0, 400.0, 490.0, 200.0);
        let resize = |sides, frame, min| {
            snap_resize(frame, sides, &[above, far], &screens(&work), 12.0, min)
        };
        let top = ResizeSides {
            top: true,
            ..Default::default()
        };
        let top_right = ResizeSides {
            top: true,
            right: true,
            ..Default::default()
        };
        let min = vec2(16.0, 16.0);
        // Top edge to `above`'s bottom (adjacent), nothing else moves.
        assert_eq!(
            resize(top, frame, min),
            Rect::from_min_max(pos2(100.0, 390.0), pos2(590.0, 600.0))
        );
        // A corner snaps both of its edges: the right one aligns with `above`'s.
        assert_eq!(
            resize(top_right, frame, min),
            Rect::from_min_max(pos2(100.0, 390.0), pos2(600.0, 600.0))
        );
        // The right edge alone is not moving: unchanged.
        assert_eq!(resize(top, frame, min).max.x, 590.0);
        // A snap that would make the frame shorter than its minimum is left out.
        let short = rect(100.0, 395.0, 490.0, 20.0);
        assert_eq!(resize(top, short, vec2(16.0, 30.0)), short);
        assert_eq!(resize(top, short, min).min.y, 390.0);
        // Work-area edges; the far window is no target.
        let low = rect(100.0, 800.0, 1395.0, 232.0);
        let bottom_right = ResizeSides {
            bottom: true,
            right: true,
            ..Default::default()
        };
        assert_eq!(
            resize(bottom_right, low, min),
            Rect::from_min_max(pos2(100.0, 800.0), pos2(1495.0, 1040.0))
        );
    }

    #[test]
    fn moves_snap_to_the_taskbar_and_to_the_screen_bottom_resizes_to_the_taskbar() {
        // 1920×1080 with a 48 px taskbar at the bottom.
        let display = [Display {
            work: rect(0.0, 0.0, 1920.0, 1032.0),
            bounds: rect(0.0, 0.0, 1920.0, 1080.0),
            scale: 1.0,
            primary: true,
        }];
        let snap = |moving| snap_group(std::iter::once(moving), &[], &display, 24.0);
        // Bottom 10 px above the taskbar: onto it.
        assert_eq!(snap(rect(500.0, 822.0, 400.0, 200.0)), vec2(0.0, 10.0));
        // Bottom 10 px above the screen's bottom (over the taskbar): onto that.
        assert_eq!(snap(rect(500.0, 870.0, 400.0, 200.0)), vec2(0.0, 10.0));
        // Resizing the bottom edge: only the taskbar.
        let bottom = ResizeSides {
            bottom: true,
            ..Default::default()
        };
        let resize = |frame| snap_resize(frame, bottom, &[], &display, 24.0, vec2(16.0, 16.0));
        assert_eq!(resize(rect(500.0, 800.0, 400.0, 220.0)).max.y, 1032.0);
        assert_eq!(resize(rect(500.0, 800.0, 400.0, 270.0)).max.y, 1070.0);
    }

    #[test]
    fn the_nearest_edge_of_any_member_snaps_the_whole_group() {
        let other = rect(1000.0, 0.0, 300.0, 300.0);
        // The right member is 10 px from the other window, the left one 20 px
        // from the work area's left edge.
        let members = [
            rect(20.0, 50.0, 400.0, 200.0),
            rect(420.0, 50.0, 570.0, 200.0),
        ];
        let work = [rect(0.0, 0.0, 1920.0, 1040.0)];
        assert_eq!(
            snap_group(members.into_iter(), &[other], &screens(&work), 24.0),
            vec2(10.0, 0.0)
        );
    }

    #[test]
    fn reachability_shifts_as_little_as_possible_and_earlier_members_win() {
        let work = [rect(0.0, 0.0, 1920.0, 1040.0)];
        let reach = |members: &[Rect], min: f32| {
            keep_reachable(members.iter().map(|member| (*member, min)), &screens(&work))
        };
        // Inside: no shift.
        assert_eq!(reach(&[rect(100.0, 100.0, 400.0, 300.0)], 32.0), Vec2::ZERO);
        // 10 px left on the right, 5 px below the bottom: up to 32 px shown.
        let corner = rect(1910.0, 1035.0, 400.0, 300.0);
        assert_eq!(reach(&[corner], 32.0), vec2(-22.0, -27.0));
        // A member on a 2x monitor keeps twice as many pixels.
        assert_eq!(reach(&[corner], 64.0), vec2(-54.0, -59.0));
        // A small window shows all of itself.
        assert_eq!(
            reach(&[rect(-30.0, 100.0, 20.0, 20.0)], 32.0),
            vec2(30.0, 0.0)
        );
        // Members 3000 px apart cannot both show: the first one wins.
        let apart = [
            rect(-500.0, 100.0, 400.0, 300.0),
            rect(2500.0, 100.0, 400.0, 300.0),
        ];
        assert_eq!(reach(&apart, 32.0), vec2(132.0, 0.0));
        // Each member's own minimum.
        let pair = [(corner, 32.0), (rect(1500.0, 1035.0, 100.0, 300.0), 64.0)];
        assert_eq!(
            keep_reachable(pair.into_iter(), &screens(&work)),
            vec2(-22.0, -59.0)
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
    fn shared_edges_name_the_side_and_the_common_span() {
        let a = rect(100.0, 100.0, 200.0, 100.0);
        assert_eq!(
            shared_edge(a, rect(301.0, 150.0, 50.0, 200.0)),
            Some((Edge::Right, 150.0, 200.0))
        );
        assert_eq!(
            shared_edge(a, rect(0.0, 0.0, 100.0, 300.0)),
            Some((Edge::Left, 100.0, 200.0))
        );
        assert_eq!(
            shared_edge(a, rect(250.0, 198.0, 100.0, 50.0)),
            Some((Edge::Bottom, 250.0, 300.0))
        );
        assert_eq!(
            shared_edge(a, rect(50.0, 0.0, 100.0, 100.0)),
            Some((Edge::Top, 100.0, 150.0))
        );
        // Two pixels of overlap along the edge are a corner, not an edge.
        assert_eq!(shared_edge(a, rect(300.0, 198.0, 50.0, 50.0)), None);
    }

    #[test]
    fn uncovered_lists_the_gaps_between_covered_spans_in_order() {
        let mut gaps = Vec::new();
        let covered = [(40.0, 60.0), (0.0, 10.0), (55.0, 70.0), (90.0, 120.0)];
        uncovered(
            (0.0, 100.0),
            || covered.into_iter(),
            |a, b| gaps.push((a, b)),
        );
        assert_eq!(gaps, vec![(10.0, 40.0), (70.0, 90.0)]);
        gaps.clear();
        uncovered((0.0, 100.0), std::iter::empty, |a, b| gaps.push((a, b)));
        assert_eq!(gaps, vec![(0.0, 100.0)]);
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
            bounds: rect(x, y, w, h),
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
