//! Desktop geometry uses physical pixels, converted at each window's DPI boundary.
//! Saved frames instead use display-local logical points relative to the work area.

use eframe::egui::{Pos2, Rect, Vec2, pos2, vec2};

use super::{WindowId, moving::MoveRequest};

pub(crate) const SNAP_DISTANCE_POINTS: f32 = 12.0;
/// Frames within this tolerance count as touching.
pub(crate) const TOUCH_TOLERANCE_PX: f32 = 2.0;
/// Reachability requires this much of each axis, or the whole axis of a smaller window.
pub(crate) const MIN_VISIBLE_POINTS: f32 = 32.0;

/// Workspace calls this on the UI thread. Group moves must land synchronously;
/// returning `false` delegates placement or drag initiation to egui viewport commands.
pub(crate) trait DesktopProbe {
    /// Must report desktop-wide state, including presses outside this application's windows.
    fn primary_button_down(&self) -> bool;
    /// Return the work area containing `point`, or the nearest one if outside all monitors.
    fn work_area(&self, point: Pos2) -> Option<Rect>;
    /// Return an empty list if monitor enumeration is unavailable.
    fn displays(&self) -> Vec<Display>;
    /// Return `false` until the native window can be tracked.
    fn attach(&self, _window: WindowId) -> bool {
        false
    }
    /// Only movable windows are supplied; external moves must carry their group along.
    fn set_groups(&self, _groups: &[(WindowId, u64)]) {}
    /// Return the current native frame, not geometry cached by a UI pass.
    fn frame(&self, _window: WindowId) -> Option<Rect> {
        None
    }
    /// Move `request.members` with the pointer until the button is released;
    /// `false` when the native windows are not tracked.
    fn begin_move(&self, _request: MoveRequest) -> bool {
        false
    }
    /// Report only a move started by `begin_move`, not the OS move loop.
    fn moving(&self) -> bool {
        false
    }
    /// Place outer top-lefts together without group following; `false` if any is untracked.
    fn place(&self, _moves: &[(WindowId, Pos2)]) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Display {
    /// Excludes the taskbar and other app bars.
    pub(crate) work: Rect,
    pub(crate) bounds: Rect,
    /// Physical pixels per logical point.
    pub(crate) scale: f32,
    pub(crate) primary: bool,
}

impl Display {
    pub(crate) fn logical_size(&self) -> Vec2 {
        self.work.size() / self.scale
    }
}

pub(crate) const FALLBACK_DISPLAY: Display = Display {
    work: Rect::from_min_max(pos2(0.0, 0.0), pos2(1920.0, 1040.0)),
    bounds: Rect::from_min_max(pos2(0.0, 0.0), pos2(1920.0, 1080.0)),
    scale: 1.0,
    primary: true,
};

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
            Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromPoint},
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
        super::native::monitor_info(monitor).map(|info| super::native::rect(info.rcWork))
    }

    fn displays(&self) -> Vec<Display> {
        use windows_sys::{
            Win32::{
                Foundation::{LPARAM, RECT},
                Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR},
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
        // Restoration precedes winit's event loop, so enable DPI awareness before querying
        // unscaled monitor geometry; winit will request the same context later.
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
                let info = super::native::monitor_info(monitor)?;
                let (mut dpi_x, mut dpi_y) = (0, 0);
                // SAFETY: valid monitor handle and output pointers.
                let scale = if unsafe {
                    GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y)
                } == 0
                {
                    super::native::dpi_scale(dpi_x)
                } else {
                    1.0
                };
                let bounds = info.rcMonitor;
                Some(Display {
                    work: super::native::rect(info.rcWork),
                    bounds: super::native::rect(bounds),
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

/// Other windows only attract members within `threshold` on both axes.
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ResizeSides {
    pub(crate) left: bool,
    pub(crate) top: bool,
    pub(crate) right: bool,
    pub(crate) bottom: bool,
}

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

/// Snap moving edges to nearby windows and the nearest work area, but not monitor bounds.
/// Windows confines the resize pointer to the work area and vertically maximizes on
/// release at its edge. Other windows count only within `threshold` on both axes;
/// reject snaps below `min` and leave fixed edges unchanged.
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

/// Each member supplies its minimum visible span in physical pixels. When the
/// constraints conflict, earlier members win.
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

fn nearest_display(displays: &[Display], point: Pos2) -> Option<&Display> {
    display_at(displays, point).map(|index| &displays[index])
}

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Edge {
    Left,
    Top,
    Right,
    Bottom,
}

impl Edge {
    pub(crate) const ALL: [Self; 4] = [Self::Left, Self::Top, Self::Right, Self::Bottom];

    pub(crate) fn span(self, frame: Rect) -> (f32, f32) {
        match self {
            Self::Left | Self::Right => (frame.min.y, frame.max.y),
            Self::Top | Self::Bottom => (frame.min.x, frame.max.x),
        }
    }
}

/// Returns the edge of `own` and the shared span; corner-only contact does not count.
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

pub(crate) fn touching(a: Rect, b: Rect) -> bool {
    shared_edge(a, b).is_some()
}

fn touching_within(a: Rect, b: Rect, tolerance: f32) -> bool {
    shared_edge_within(a, b, tolerance).is_some()
}

/// Accepts unordered covered spans; emits gaps in ascending order.
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

pub(crate) fn reachable_position(outer: Rect, work: Rect, min_visible: f32) -> Option<Pos2> {
    (!reachable(outer, work, min_visible)).then(|| fit_inside(outer, work))
}

pub(crate) fn reachable(outer: Rect, work: Rect, min_visible: f32) -> bool {
    let visible = outer.intersect(work);
    visible.width().max(0.0) + 0.5 >= min_visible.min(outer.width())
        && visible.height().max(0.0) + 0.5 >= min_visible.min(outer.height())
}

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

pub(crate) fn display_at(displays: &[Display], point: Pos2) -> Option<usize> {
    nearest_index(displays.len(), |index| displays[index].work, point)
}

/// Returns work-area-relative logical points. Near-attached edges are saved exactly
/// on the work-area edge so restoration recognizes their attachment.
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

/// `size` is in logical points; `hint` is the physical work-area origin and scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SavedDisplay {
    pub(crate) size: Vec2,
    pub(crate) hint: Option<(Pos2, f32)>,
}

/// `frame` is work-area-relative logical points; `min` is a logical size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SavedWindow {
    pub(crate) display: usize,
    pub(crate) frame: Rect,
    pub(crate) group: Option<u32>,
    pub(crate) min: Vec2,
}

/// Reuse displays matching their saved origin, size and scale. Only an unrecognized
/// first saved display takes `current`; later ones take free displays left-to-right,
/// falling back to `current` when none remain.
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

/// Returns physical outer frames. Within each saved display, touching or grouped windows
/// map as one cluster. On each axis, attachment to both work-area edges stretches;
/// one edge preserves size and edge distance; neither preserves size and relative center.
/// Oversized clusters shrink on unstretched axes, subject to member minima, then fit
/// top-left first. Shared edges round identically to avoid gaps.
pub(crate) fn place(
    saved: &[SavedDisplay],
    windows: &[SavedWindow],
    targets: &[Display],
) -> Vec<Rect> {
    /// A cluster edge this close (logical points) to a work-area edge rests on it;
    /// saved frames put such edges exactly there (`saved_frame`).
    const ATTACHED: f32 = 0.5;
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
        // One shrink factor preserves proportions across the unstretched axes.
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
        let far = rect(450.0, 40.0, 200.0, 100.0);
        assert_eq!(snap(far, &[other], &[]), Vec2::ZERO);
        let below = rect(410.0, 500.0, 200.0, 100.0);
        assert_eq!(snap(below, &[other], &[]), Vec2::ZERO);
        // A 29 px horizontal gap exceeds the 24 px threshold, even with tops only 9 px apart.
        let beside = rect(429.0, 9.0, 200.0, 100.0);
        assert_eq!(snap(beside, &[other], &[]), Vec2::ZERO);
        let beside = rect(415.0, 9.0, 200.0, 100.0);
        assert_eq!(snap(beside, &[other], &[]), vec2(-15.0, -9.0));
        // Only the member's nearest monitor offers work-area edges.
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
        assert_eq!(
            resize(top, frame, min),
            Rect::from_min_max(pos2(100.0, 390.0), pos2(590.0, 600.0))
        );
        assert_eq!(
            resize(top_right, frame, min),
            Rect::from_min_max(pos2(100.0, 390.0), pos2(600.0, 600.0))
        );
        assert_eq!(resize(top, frame, min).max.x, 590.0);
        let short = rect(100.0, 395.0, 490.0, 20.0);
        assert_eq!(resize(top, short, vec2(16.0, 30.0)), short);
        assert_eq!(resize(top, short, min).min.y, 390.0);
        // The right edge is near `far`, but their vertical gap excludes that window.
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
        // Bottom 10 px above the taskbar.
        assert_eq!(snap(rect(500.0, 822.0, 400.0, 200.0)), vec2(0.0, 10.0));
        // Bottom 10 px above the screen bottom, overlapping the taskbar.
        assert_eq!(snap(rect(500.0, 870.0, 400.0, 200.0)), vec2(0.0, 10.0));
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
        assert_eq!(reach(&[rect(100.0, 100.0, 400.0, 300.0)], 32.0), Vec2::ZERO);
        // Only 10 px remain visible on the right and 5 px at the bottom.
        let corner = rect(1910.0, 1035.0, 400.0, 300.0);
        assert_eq!(reach(&[corner], 32.0), vec2(-22.0, -27.0));
        // A member on a 2x monitor keeps twice as many pixels.
        assert_eq!(reach(&[corner], 64.0), vec2(-54.0, -59.0));
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
        // Two pixels of overlap along the edge are within the corner tolerance.
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
        // A disconnected monitor to the left leaves this window entirely outside.
        assert_eq!(
            reachable_position(rect(-1500.0, 200.0, 400.0, 300.0), work, 32.0),
            Some(pos2(0.0, 200.0))
        );
        assert_eq!(
            reachable_position(rect(1910.0, 1030.0, 400.0, 300.0), work, 32.0),
            Some(pos2(1520.0, 740.0))
        );
        // Deliberately partly off-screen, but still reachable.
        assert_eq!(
            reachable_position(rect(1700.0, 100.0, 400.0, 300.0), work, 32.0),
            None
        );
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
        // A second monitor left of the primary one, at 125%.
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
        // The floating cluster keeps its 598.8-point width, converted at 150%.
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
        // The center stays at the same relative horizontal position on the smaller display.
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
        // A 1 px gap at 100% counts as touching at runtime.
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
        assert_eq!(match_displays(&[unknown], &displays, 1), vec![1]);
        assert_eq!(match_displays(&[known_left], &displays, 0), vec![2]);
        assert_eq!(
            match_displays(&[unknown, unknown, unknown, unknown], &displays, 0),
            vec![0, 2, 1, 0]
        );
        assert_eq!(
            match_displays(&[unknown, known_left], &displays, 2),
            vec![2, 2]
        );
        // Recognizing the first saved display elsewhere leaves `current` free for the next.
        assert_eq!(
            match_displays(&[known_left, unknown], &displays, 0),
            vec![2, 0]
        );
    }
}
