//! One process per config directory, so two instances never overwrite each other's
//! `session.json`; a later launch raises the running instance's windows instead.

use std::{
    io,
    path::{self, Path},
};

use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, INVALID_HANDLE_VALUE, LPARAM,
    },
    System::{
        Memory::{
            CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, MapViewOfFile, PAGE_READWRITE,
            UnmapViewOfFile,
        },
        Threading::GetCurrentProcessId,
    },
    UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextW, GetWindowThreadProcessId, HWND_TOP, IsIconic, IsWindowVisible,
        SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetForegroundWindow,
        SetWindowPos, ShowWindow,
    },
};
use windows_sys::core::BOOL;

/// Workspace windows are "Taffymeters #<id>", the settings window "Taffymeters settings";
/// the off-screen host and error dialogs are titled "Taffymeters" alone.
const WINDOW_TITLE_PREFIX: &str = "Taffymeters ";

/// Holds the config directory until dropped.
pub(crate) struct Instance(HANDLE);

impl Drop for Instance {
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateFileMappingW` and is closed only here.
        unsafe { CloseHandle(self.0) };
    }
}

pub(crate) enum Claim {
    First(Instance),
    /// `pid` is 0 if the running instance had not published it yet.
    Running {
        pid: u32,
    },
}

/// The claim is a named section holding the owner's process id; it lives in the
/// session's `Local\` namespace, so other logon sessions are not seen.
pub(crate) fn claim(config_dir: &Path) -> io::Result<Claim> {
    let name = object_name(config_dir)?;
    let size = size_of::<u32>();
    // SAFETY: `name` is NUL-terminated and outlives the call; a page-file backed
    // section takes `INVALID_HANDLE_VALUE` and no security attributes.
    let mapping = unsafe {
        CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            std::ptr::null(),
            PAGE_READWRITE,
            0,
            size as u32,
            name.as_ptr(),
        )
    };
    if mapping.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: no preconditions; read right after the call that set it.
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    let access = if existed {
        FILE_MAP_READ
    } else {
        FILE_MAP_WRITE
    };
    // SAFETY: `mapping` is a valid section of at least `size` bytes.
    let view = unsafe { MapViewOfFile(mapping, access, 0, 0, size) };
    if view.Value.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: `mapping` is owned here and not used afterwards.
        unsafe { CloseHandle(mapping) };
        return Err(error);
    }
    let slot = view.Value.cast::<u32>();
    let claim = if existed {
        // SAFETY: the view is page aligned and spans `size` readable bytes.
        let pid = unsafe { slot.read_volatile() };
        // SAFETY: `mapping` is owned here and not used afterwards.
        unsafe { CloseHandle(mapping) };
        Claim::Running { pid }
    } else {
        // SAFETY: the view is page aligned and spans `size` writable bytes; the
        // section keeps the value after the view is unmapped.
        unsafe { slot.write_volatile(GetCurrentProcessId()) };
        Claim::First(Instance(mapping))
    };
    // SAFETY: `view` was mapped above and is not used afterwards.
    unsafe { UnmapViewOfFile(view) };
    Ok(claim)
}

/// Restores and raises every window of `pid`, then focuses the frontmost one. The
/// process launched by the user may take the foreground; the running one may not.
pub(crate) fn raise(pid: u32) {
    let windows = windows_of(pid);
    for &hwnd in windows.iter().rev() {
        // SAFETY: a stale handle only makes these calls fail.
        unsafe {
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
            SetWindowPos(
                hwnd,
                HWND_TOP,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
    if let Some(&front) = windows.first() {
        // SAFETY: a stale handle only makes the call fail.
        unsafe { SetForegroundWindow(front) };
    }
}

/// Visible titled windows of `pid`, front to back.
fn windows_of(pid: u32) -> Vec<HWND> {
    struct Search {
        pid: u32,
        prefix: Vec<u16>,
        found: Vec<HWND>,
    }
    unsafe extern "system" fn each(hwnd: HWND, data: LPARAM) -> BOOL {
        // SAFETY: `data` is the `Search` below, alive for the whole enumeration.
        let search = unsafe { &mut *(data as *mut Search) };
        let mut owner = 0_u32;
        // SAFETY: `owner` outlives the call.
        unsafe { GetWindowThreadProcessId(hwnd, &mut owner) };
        // SAFETY: no preconditions.
        if owner != search.pid || unsafe { IsWindowVisible(hwnd) } == 0 {
            return 1;
        }
        let mut text = [0_u16; 64];
        // SAFETY: the buffer and its length match.
        let length = unsafe { GetWindowTextW(hwnd, text.as_mut_ptr(), text.len() as i32) };
        if usize::try_from(length).is_ok_and(|length| text[..length].starts_with(&search.prefix)) {
            search.found.push(hwnd);
        }
        1
    }
    if pid == 0 {
        return Vec::new();
    }
    let mut search = Search {
        pid,
        prefix: WINDOW_TITLE_PREFIX.encode_utf16().collect(),
        found: Vec::new(),
    };
    // SAFETY: the callback only reads window properties and writes `search`, which
    // outlives the call.
    unsafe { EnumWindows(Some(each), &mut search as *mut Search as LPARAM) };
    search.found
}

/// Spellings of one directory (case, separators, relative, trailing separator) share a
/// name. Hashed because object names are limited to `MAX_PATH`; FNV-1a 64 stays stable
/// across builds, unlike `DefaultHasher`.
fn object_name(config_dir: &Path) -> io::Result<Vec<u16>> {
    let absolute = path::absolute(config_dir)?;
    let key = absolute
        .to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .to_lowercase();
    let hash = key
        .encode_utf16()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, unit| {
            (hash ^ u64::from(unit)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    Ok(format!("Local\\taffymeters-{hash:016x}")
        .encode_utf16()
        .chain([0])
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_directory_is_one_instance_until_released() {
        let base =
            std::env::temp_dir().join(format!("taffymeters-instance-{}", std::process::id()));
        let dir = base.join("Config");
        let first = match claim(&dir).unwrap() {
            Claim::First(instance) => instance,
            Claim::Running { .. } => panic!("an unused directory must be free"),
        };
        let respelled = base.join("x").join("..").join("CONFIG\\");
        assert!(matches!(
            claim(&respelled).unwrap(),
            Claim::Running { pid } if pid == std::process::id()
        ));
        assert!(matches!(
            claim(&base.join("other")).unwrap(),
            Claim::First(_)
        ));
        drop(first);
        assert!(matches!(claim(&dir).unwrap(), Claim::First(_)));
    }
}
