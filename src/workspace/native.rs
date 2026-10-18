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
//!   following.
//!
//! The tracker lock is never held while windows move: moving one sends its
//! `WM_WINDOWPOSCHANGED` (and the followers') into this procedure again.

use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use eframe::egui::{Pos2, Rect, pos2};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    System::Threading::GetCurrentThreadId,
    UI::{
        Input::KeyboardAndMouse::{GetAsyncKeyState, GetKeyState, VK_ESCAPE, VK_LBUTTON, VK_MENU},
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::{
            BeginDeferWindowPos, DeferWindowPos, EndDeferWindowPos, EnumThreadWindows,
            GetCursorPos, GetWindowRect, GetWindowTextW, HWND_TOP, SC_KEYMENU, SWP_NOACTIVATE,
            SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos, WINDOWPOS,
            WM_CAPTURECHANGED, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
            WM_NCDESTROY, WM_SYSCOMMAND, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_WINDOWPOSCHANGED,
        },
    },
};

use super::{
    WindowId,
    moving::{MoveRequest, WindowTracker},
};

/// Identifies this module's subclass on a window.
const SUBCLASS_ID: usize = 0x7461_6666;
/// `MK_LBUTTON` in a mouse message's `wParam`.
const MK_LBUTTON: WPARAM = 0x0001;

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
    /// Reused for every batch of moves.
    moves: Vec<(WindowId, Pos2)>,
    batch: Vec<(isize, i32, i32)>,
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
    let raise: Vec<isize> = {
        let mut native = native();
        if !native.tracker.begin(request, work_areas) {
            return false;
        }
        // Followers first, then the dragged window: the group comes to the front.
        request
            .members
            .iter()
            .filter(|id| **id != request.leader)
            .chain(std::iter::once(&request.leader))
            .filter_map(|id| native.handle(*id))
            .collect()
    };
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
    drive();
    true
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

/// Move the session's members to where the pointer puts them now.
fn drive() {
    let (cursor, free) = (cursor(), alt_down());
    {
        let mut native = native();
        let Native { tracker, moves, .. } = &mut *native;
        moves.clear();
        tracker.motion(cursor, free, moves);
    }
    apply();
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
        WM_LBUTTONDOWN => {
            // egui ignores a press until it knows where the pointer is, which it
            // learns from winit's moves; after the pointer left the window, winit
            // reports a move again only to a position other than the last one it
            // saw there. After a body drag the window moved with the pointer, so a
            // later press can land exactly where the last release was and be lost.
            // Report the pointer one pixel off and then where it is.
            let x = (lparam & 0xFFFF) as u16;
            let off = (lparam & !0xFFFF) | x.wrapping_add(1) as isize;
            let buttons = wparam & !MK_LBUTTON;
            // SAFETY: forwarding moves of this window's own pointer.
            unsafe {
                DefSubclassProc(hwnd, WM_MOUSEMOVE, buttons, off);
                DefSubclassProc(hwnd, WM_MOUSEMOVE, buttons, lparam);
            }
        }
        WM_MOUSEMOVE if leading() => {
            if wparam & MK_LBUTTON == 0 {
                native().tracker.end();
            } else {
                drive();
            }
        }
        WM_LBUTTONUP if leading() => native().tracker.end(),
        WM_CAPTURECHANGED if lparam as HWND != hwnd && leading() => native().tracker.end(),
        WM_KEYDOWN if wparam == usize::from(VK_ESCAPE) && leading() => {
            {
                let mut native = native();
                let Native { tracker, moves, .. } = &mut *native;
                moves.clear();
                tracker.cancel(moves);
                native.swallow_escape_up = true;
            }
            apply();
            return 0;
        }
        WM_KEYUP if wparam == usize::from(VK_ESCAPE) => {
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
            drive();
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
        WM_NCDESTROY => {
            {
                let mut native = native();
                native.tracker.forget(window);
                native.handles.retain(|(id, _)| *id != window);
            }
            // SAFETY: removing this module's own subclass from the window.
            unsafe { RemoveWindowSubclass(hwnd, Some(subclass), SUBCLASS_ID) };
        }
        _ => {}
    }
    // SAFETY: forwarding this window's message.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}
