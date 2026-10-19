//! Win32 side of window moves. Each Workspace window's native window is found by
//! its unique title and subclassed; the subclass procedure runs the
//! [`WindowTracker`] on the UI thread, inside the messages that move windows:
//!
//! - a body drag (`begin_move`) moves the dragged window and its group with
//!   every `WM_MOUSEMOVE`, all in one `DeferWindowPos` batch, snapped unless Alt
//!   is held; the button's release ends it and Escape puts them back;
//! - any other pure move of a grouped window (another program, the OS) moves the
//!   rest of its group in the same message;
//! - `place` moves windows for the Workspace (restore, rescue) without the group
//!   following;
//! - an edge resize (the OS sizing loop that `BeginResize` starts) snaps the edges
//!   it moves in `WM_SIZING`, unless Alt is held.
//!
//! The tracker lock is never held while windows move: moving one sends its
//! `WM_WINDOWPOSCHANGED` (and the followers') into this procedure again.

use std::sync::{
    LazyLock, Mutex, MutexGuard, PoisonError,
    atomic::{AtomicIsize, Ordering},
};

use eframe::egui::{Pos2, Rect, Vec2, pos2, vec2};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    System::Threading::GetCurrentThreadId,
    UI::{
        HiDpi::GetDpiForWindow,
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, GetKeyState, MOD_ALT, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
            VK_ESCAPE, VK_LBUTTON, VK_MENU,
        },
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{
            BeginDeferWindowPos, DeferWindowPos, EndDeferWindowPos, EnumThreadWindows,
            GetCursorPos, GetMessagePos, GetWindowRect, GetWindowTextW, HWND_TOP, IsIconic,
            IsZoomed, MINMAXINFO, SC_KEYMENU, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER,
            SWP_NOSIZE, SWP_NOZORDER, SendMessageW, SetWindowPos, WINDOWPOS, WM_CAPTURECHANGED,
            WM_CHAR, WM_ENTERSIZEMOVE, WM_EXITSIZEMOVE, WM_GETMINMAXINFO, WM_HOTKEY, WM_KEYDOWN,
            WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MOUSEMOVE, WM_NCDESTROY,
            WM_RBUTTONDOWN, WM_SIZING, WM_SYSCHAR, WM_SYSCOMMAND, WM_SYSKEYDOWN, WM_SYSKEYUP,
            WM_WINDOWPOSCHANGED, WMSZ_BOTTOM, WMSZ_BOTTOMLEFT, WMSZ_BOTTOMRIGHT, WMSZ_LEFT,
            WMSZ_RIGHT, WMSZ_TOP, WMSZ_TOPLEFT, WMSZ_TOPRIGHT,
        },
    },
};

use super::{
    WindowId,
    desktop::{self, DesktopProbe, NativeDesktop, ResizeSides},
    moving::{MoveRequest, WindowTracker},
};

/// Identifies this module's subclass on a window.
const SUBCLASS_ID: usize = 0x7461_6666;
/// Button flags in a mouse message's `wParam`.
const MK_LBUTTON: WPARAM = 0x0001;
const MK_RBUTTON: WPARAM = 0x0002;
const MK_MBUTTON: WPARAM = 0x0010;

#[derive(Default)]
struct Native {
    tracker: WindowTracker,
    /// Native window of each tracked Workspace window (`HWND` as an address).
    handles: Vec<(WindowId, isize)>,
    /// This module is moving windows: their changes are its own.
    applying: bool,
    /// Alt was pressed during a move: its release must not open the window menu.
    swallow_keymenu: bool,
    /// Escape canceled a move: its release belongs to that too.
    swallow_escape_up: bool,
    /// What an edge resize in progress snaps to.
    sizing: Option<Sizing>,
    /// Reused for every batch of moves.
    moves: Vec<(WindowId, Pos2)>,
    batch: Vec<(isize, i32, i32)>,
}

/// An edge resize in progress (the OS sizing loop), from its start.
struct Sizing {
    /// Frame and pointer when the loop started.
    start: Rect,
    cursor: Pos2,
    others: Vec<Rect>,
    work_areas: Vec<Rect>,
    /// Physical pixels at the window's scale.
    threshold: f32,
    /// The smallest frame the window may have (its layout's).
    min: Vec2,
}

impl Native {
    fn handle(&self, window: WindowId) -> Option<isize> {
        self.handles
            .iter()
            .find(|(id, _)| *id == window)
            .map(|(_, handle)| *handle)
    }
}

static NATIVE: LazyLock<Mutex<Native>> = LazyLock::new(Mutex::default);

fn native() -> MutexGuard<'static, Native> {
    NATIVE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Find and subclass the native window of `window` (titled `Taffymeters #N`).
pub(super) fn attach(window: WindowId) -> bool {
    if native().handle(window).is_some() {
        return true;
    }
    let Some(hwnd) = find_window(window) else {
        return false;
    };
    // SAFETY: `hwnd` is a window of this thread; the procedure stays valid for the
    // process and removes itself when the window is destroyed.
    if unsafe { SetWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID, window.0 as usize) } == 0 {
        return false;
    }
    let Some(frame) = frame_of(hwnd) else {
        return false;
    };
    let mut native = native();
    native.handles.push((window, hwnd as isize));
    let mut ignored = Vec::new();
    native
        .tracker
        .observed(window, frame, true, Pos2::ZERO, &mut ignored);
    true
}

fn find_window(window: WindowId) -> Option<HWND> {
    struct Search {
        title: Vec<u16>,
        found: Option<HWND>,
    }
    unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> windows_sys::core::BOOL {
        // SAFETY: `data` is the `Search` below, alive for the whole enumeration.
        let search = unsafe { &mut *(data as *mut Search) };
        let mut text = [0_u16; 64];
        // SAFETY: the buffer and its length match.
        let length = unsafe { GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32) };
        if usize::try_from(length).is_ok_and(|length| text[..length] == search.title[..]) {
            search.found = Some(hwnd);
            return 0;
        }
        1
    }
    let mut search = Search {
        title: format!("Taffymeters #{}", window.0)
            .encode_utf16()
            .collect(),
        found: None,
    };
    // SAFETY: the callback only reads window text into a local buffer and writes
    // `search`, which outlives the call.
    unsafe {
        EnumThreadWindows(
            GetCurrentThreadId(),
            Some(each),
            &mut search as *mut Search as LPARAM,
        )
    };
    search.found
}

fn frame_of(hwnd: HWND) -> Option<Rect> {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: valid output pointer.
    (unsafe { GetWindowRect(hwnd, &mut rect) } != 0).then(|| {
        Rect::from_min_max(
            pos2(rect.left as f32, rect.top as f32),
            pos2(rect.right as f32, rect.bottom as f32),
        )
    })
}

fn cursor() -> Pos2 {
    let mut point = POINT { x: 0, y: 0 };
    // SAFETY: valid output pointer.
    unsafe { GetCursorPos(&mut point) };
    pos2(point.x as f32, point.y as f32)
}

/// The pointer (screen, physical) when the message being handled was sent.
fn message_cursor() -> Pos2 {
    // SAFETY: no preconditions.
    let position = unsafe { GetMessagePos() };
    pos2(
        f32::from(position as u16 as i16),
        f32::from((position >> 16) as u16 as i16),
    )
}

fn alt_down() -> bool {
    // SAFETY: no preconditions.
    unsafe { GetKeyState(i32::from(VK_MENU)) < 0 }
}

pub(super) fn set_groups(groups: &[(WindowId, u64)]) {
    native().tracker.set_groups(groups);
}

pub(super) fn frame(window: WindowId) -> Option<Rect> {
    native().tracker.frame(window)
}

pub(super) fn moving() -> bool {
    native().tracker.moving()
}

pub(super) fn begin_move(request: &MoveRequest, work_areas: Vec<Rect>) -> bool {
    // SAFETY: no preconditions.
    if unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON)) } >= 0 {
        // Released before the drag was seen: nothing left to move.
        return true;
    }
    let (raise, leader) = {
        let mut native = native();
        if !native.tracker.begin(request, work_areas) {
            return false;
        }
        // Alt may already be down when the drag is recognized: its release must
        // not open the window menu either.
        native.swallow_keymenu |= alt_down();
        // Followers first, then the dragged window: the group comes to the front.
        let raise: Vec<isize> = request
            .members
            .iter()
            .map(|(id, _)| *id)
            .filter(|id| *id != request.leader)
            .chain(std::iter::once(request.leader))
            .filter_map(|id| native.handle(id))
            .collect();
        (raise, native.handle(request.leader))
    };
    if let Some(leader) = leader {
        grab_alt_escape(leader);
    }
    for handle in raise {
        // SAFETY: a tracked window of this thread; z-order only.
        unsafe {
            SetWindowPos(
                handle as HWND,
                HWND_TOP,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            )
        };
    }
    // The pointer moved past egui's drag threshold already: catch up.
    drive(cursor());
    true
}

/// Identifies the Alt+Esc hotkey registered while a body drag is in progress.
const ALT_ESCAPE_HOTKEY: i32 = 0x7466;
/// The window the Alt+Esc hotkey is registered for (`HWND` as an address).
static ALT_ESCAPE_TARGET: AtomicIsize = AtomicIsize::new(0);

/// With Alt held (snapping off), Escape never reaches the window: Alt+Esc is a
/// system shortcut (send the window to the back). While a drag is in progress the
/// dragged window registers it as its own hotkey, so Escape still cancels.
fn grab_alt_escape(leader: isize) {
    release_alt_escape();
    // SAFETY: a window of this thread; WM_HOTKEY is posted to it.
    if unsafe {
        RegisterHotKey(
            leader as HWND,
            ALT_ESCAPE_HOTKEY,
            MOD_ALT | MOD_NOREPEAT,
            u32::from(VK_ESCAPE),
        )
    } != 0
    {
        ALT_ESCAPE_TARGET.store(leader, Ordering::Relaxed);
    }
}

fn release_alt_escape() {
    let target = ALT_ESCAPE_TARGET.swap(0, Ordering::Relaxed);
    if target != 0 {
        // SAFETY: the hotkey this module registered for that window.
        unsafe { UnregisterHotKey(target as HWND, ALT_ESCAPE_HOTKEY) };
    }
}

/// The button was released (or the pointer captured elsewhere): the members stay.
fn end_move() {
    native().tracker.end();
    release_alt_escape();
}

/// Escape: the members go back to where the drag started.
fn cancel_move() {
    {
        let mut native = native();
        let Native { tracker, moves, .. } = &mut *native;
        moves.clear();
        tracker.cancel(moves);
        native.swallow_escape_up = true;
        native.swallow_keymenu |= alt_down();
    }
    release_alt_escape();
    apply();
}

pub(super) fn place(moves: &[(WindowId, Pos2)]) -> bool {
    {
        let mut native = native();
        if moves.iter().any(|(id, _)| native.handle(*id).is_none()) {
            return false;
        }
        native.moves.clear();
        native.moves.extend_from_slice(moves);
    }
    apply();
    true
}

/// Move the session's members to where the pointer at `cursor` puts them.
fn drive(cursor: Pos2) {
    let free = alt_down();
    {
        let mut native = native();
        let Native { tracker, moves, .. } = &mut *native;
        moves.clear();
        tracker.motion(cursor, free, moves);
    }
    apply();
    // Every member has landed (and changed scale, if it did): re-anchor now.
    native().tracker.settle(cursor);
}

/// Move the windows in `Native::moves` in one batch, without holding the lock.
fn apply() {
    let mut batch = {
        let mut native = native();
        let mut batch = std::mem::take(&mut native.batch);
        batch.clear();
        for (id, position) in &native.moves {
            if let (Some(handle), Some(frame)) = (native.handle(*id), native.tracker.frame(*id))
                && frame.min != *position
            {
                batch.push((handle, position.x.round() as i32, position.y.round() as i32));
            }
        }
        native.applying = true;
        batch
    };
    if !batch.is_empty() {
        let flags = SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER;
        // SAFETY: tracked windows of this thread; positions only.
        unsafe {
            let mut defer = BeginDeferWindowPos(batch.len() as i32);
            for (handle, x, y) in &batch {
                if !defer.is_null() {
                    defer = DeferWindowPos(defer, *handle as HWND, HWND_TOP, *x, *y, 0, 0, flags);
                }
            }
            if defer.is_null() || EndDeferWindowPos(defer) == 0 {
                for (handle, x, y) in &batch {
                    SetWindowPos(*handle as HWND, HWND_TOP, *x, *y, 0, 0, flags);
                }
            }
        }
    }
    let mut native = native();
    native.applying = false;
    native.batch = std::mem::take(&mut batch);
}

/// Snap targets for an edge resize of `window`: the other Workspace windows that
/// are neither minimized nor maximized, the work areas, and the window's minimum.
fn sizing_targets(hwnd: HWND, window: WindowId) -> Sizing {
    let handles: Vec<isize> = native()
        .handles
        .iter()
        .filter(|(id, _)| *id != window)
        .map(|(_, handle)| *handle)
        .collect();
    let others = handles
        .into_iter()
        .map(|handle| handle as HWND)
        // SAFETY: tracked windows of this thread; state queries only.
        .filter(|other| unsafe { IsIconic(*other) == 0 && IsZoomed(*other) == 0 })
        .filter_map(frame_of)
        .collect();
    let work_areas = NativeDesktop
        .displays()
        .iter()
        .map(|display| display.work)
        .collect();
    // SAFETY: a window of this thread.
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let min = min_track_size(hwnd);
    Sizing {
        start: frame_of(hwnd).unwrap_or(Rect::NOTHING),
        cursor: cursor(),
        others,
        work_areas,
        threshold: desktop::SNAP_DISTANCE_POINTS * dpi as f32 / 96.0,
        min: vec2(min.0 as f32, min.1 as f32),
    }
}

/// The window's minimum frame as winit reports it (`ptMinTrackSize`: the layout's
/// minimum); `WM_GETMINMAXINFO` with a zeroed MINMAXINFO leaves out the system's
/// own default.
fn min_track_size(hwnd: HWND) -> (i32, i32) {
    let mut info = MINMAXINFO::default();
    // SAFETY: WM_GETMINMAXINFO fills the MINMAXINFO it points to.
    unsafe {
        SendMessageW(
            hwnd,
            WM_GETMINMAXINFO,
            0,
            &mut info as *mut MINMAXINFO as LPARAM,
        )
    };
    (info.ptMinTrackSize.x.max(0), info.ptMinTrackSize.y.max(0))
}

/// The edges a `WM_SIZING` side moves.
fn sizing_sides(side: u32) -> ResizeSides {
    ResizeSides {
        left: matches!(side, WMSZ_LEFT | WMSZ_TOPLEFT | WMSZ_BOTTOMLEFT),
        top: matches!(side, WMSZ_TOP | WMSZ_TOPLEFT | WMSZ_TOPRIGHT),
        right: matches!(side, WMSZ_RIGHT | WMSZ_TOPRIGHT | WMSZ_BOTTOMRIGHT),
        bottom: matches!(side, WMSZ_BOTTOM | WMSZ_BOTTOMLEFT | WMSZ_BOTTOMRIGHT),
    }
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    data: usize,
) -> LRESULT {
    let window = WindowId(data as u64);
    let leading = || native().tracker.leader() == Some(window);
    match message {
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            // egui ignores a press until it knows where the pointer is, which it
            // learns from winit's moves; after the pointer left the window, winit
            // reports a move again only to a position other than the last one it
            // saw there. After a body drag the window moved with the pointer, so a
            // later press (a left drag or a right click for the settings) can land
            // exactly where the last release was and be lost. Report the pointer
            // one pixel off and then where it is.
            let x = (lparam & 0xFFFF) as u16;
            let off = (lparam & !0xFFFF) | x.wrapping_add(1) as isize;
            let pressed = match message {
                WM_LBUTTONDOWN => MK_LBUTTON,
                WM_RBUTTONDOWN => MK_RBUTTON,
                _ => MK_MBUTTON,
            };
            let buttons = wparam & !pressed;
            // SAFETY: forwarding moves of this window's own pointer.
            unsafe {
                DefSubclassProc(hwnd, WM_MOUSEMOVE, buttons, off);
                DefSubclassProc(hwnd, WM_MOUSEMOVE, buttons, lparam);
            }
        }
        WM_MOUSEMOVE if leading() => {
            if wparam & MK_LBUTTON == 0 {
                end_move();
            } else {
                drive(message_cursor());
            }
        }
        WM_LBUTTONUP if leading() => {
            // The release may come before the move to where it happened.
            drive(message_cursor());
            end_move();
        }
        WM_CAPTURECHANGED if lparam as HWND != hwnd && leading() => end_move(),
        WM_HOTKEY if wparam == ALT_ESCAPE_HOTKEY as usize => {
            if leading() {
                cancel_move();
            }
            return 0;
        }
        // Escape without Alt (with Alt it is the hotkey above).
        WM_KEYDOWN | WM_SYSKEYDOWN if wparam == usize::from(VK_ESCAPE) && leading() => {
            cancel_move();
            return 0;
        }
        // The character and the release of the Escape that canceled a move.
        WM_CHAR | WM_SYSCHAR if wparam == 0x1B && native().swallow_escape_up => return 0,
        WM_KEYUP | WM_SYSKEYUP if wparam == usize::from(VK_ESCAPE) => {
            let mut native = native();
            if std::mem::take(&mut native.swallow_escape_up) {
                return 0;
            }
        }
        WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP
            if wparam == usize::from(VK_MENU) && leading() =>
        {
            native().swallow_keymenu = true;
            // SAFETY: forwarding this window's message.
            let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
            // Alt toggles snapping without the pointer moving.
            drive(message_cursor());
            return result;
        }
        WM_SYSCOMMAND if (wparam as u32 & 0xFFF0) == SC_KEYMENU => {
            let mut native = native();
            if std::mem::take(&mut native.swallow_keymenu) {
                return 0;
            }
        }
        WM_WINDOWPOSCHANGED => {
            // SAFETY: forwarding this window's message.
            let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
            // SAFETY: WM_WINDOWPOSCHANGED carries a WINDOWPOS.
            let flags = unsafe { (*(lparam as *const WINDOWPOS)).flags };
            if flags & (SWP_NOMOVE | SWP_NOSIZE) != (SWP_NOMOVE | SWP_NOSIZE)
                && let Some(frame) = frame_of(hwnd)
            {
                let follow = {
                    let mut native = native();
                    let ours = native.applying;
                    let cursor = cursor();
                    let Native { tracker, moves, .. } = &mut *native;
                    moves.clear();
                    tracker.observed(window, frame, ours, cursor, moves);
                    !moves.is_empty()
                };
                if follow {
                    apply();
                }
            }
            return result;
        }
        WM_ENTERSIZEMOVE => {
            let sizing = sizing_targets(hwnd, window);
            native().sizing = Some(sizing);
        }
        WM_EXITSIZEMOVE => native().sizing = None,
        WM_SIZING => {
            // The OS proposes the frame for the pointer, but never smaller than its
            // minimum for captioned windows (winit's windows have a caption style,
            // only not drawn: 262×71 px at 200%), whatever the window's own minimum.
            // The frame is computed from the pointer instead, kept at least the
            // window's own minimum, and snapped unless Alt is held; an edge stays on
            // a target until the pointer is past it.
            let free = alt_down();
            let mut native = native();
            if free {
                native.swallow_keymenu = true;
            }
            if let Some(sizing) = &native.sizing
                && sizing.start != Rect::NOTHING
            {
                let sides = sizing_sides(wparam as u32);
                let mut frame = desktop::resize_frame(
                    sizing.start,
                    sides,
                    cursor() - sizing.cursor,
                    sizing.min,
                );
                if !free {
                    frame = desktop::snap_resize(
                        frame,
                        sides,
                        &sizing.others,
                        &sizing.work_areas,
                        sizing.threshold,
                        sizing.min,
                    );
                }
                // SAFETY: WM_SIZING carries the proposed frame, to be adjusted.
                let rect = unsafe { &mut *(lparam as *mut RECT) };
                rect.left = frame.min.x.round() as i32;
                rect.top = frame.min.y.round() as i32;
                rect.right = frame.max.x.round() as i32;
                rect.bottom = frame.max.y.round() as i32;
            }
        }
        WM_NCDESTROY => {
            {
                let mut native = native();
                native.tracker.forget(window);
                native.handles.retain(|(id, _)| *id != window);
            }
            // Forgetting a member ends the move.
            if !native().tracker.moving() {
                release_alt_escape();
            }
            // SAFETY: removing this module's own subclass from the window.
            unsafe { RemoveWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID) };
        }
        _ => {}
    }
    // SAFETY: forwarding this window's message.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}
