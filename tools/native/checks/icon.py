"""Executable icon/version resources and both native windows' WM_GETICON."""
import ctypes as ct
from ctypes import wintypes as wt
import re
from ..app import App
from .. import win32 as w


def run(c):
    extract = w.bind(w.shell, "ExtractIconExW", wt.UINT, wt.LPCWSTR, ct.c_int,
                     ct.c_void_p, ct.c_void_p, wt.UINT)
    count = extract(str(c.exe), -1, None, None, 0)
    c.expect("exe_icon_resource", count > 0, icon_resources=count)
    version = ct.WinDLL("version", use_last_error=True)
    size_fn = w.bind(version, "GetFileVersionInfoSizeW", wt.DWORD, wt.LPCWSTR, ct.POINTER(wt.DWORD))
    read_fn = w.bind(version, "GetFileVersionInfoW", wt.BOOL, wt.LPCWSTR, wt.DWORD, wt.DWORD, ct.c_void_p)
    query = w.bind(version, "VerQueryValueW", wt.BOOL, ct.c_void_p, wt.LPCWSTR,
                   ct.POINTER(ct.c_void_p), ct.POINTER(wt.UINT))
    ignored = wt.DWORD()
    size = size_fn(str(c.exe), ct.byref(ignored))
    c.expect("version_resource_present", size > 0, bytes=size)
    buffer = ct.create_string_buffer(size)
    if not read_fn(str(c.exe), 0, size, buffer):
        raise ct.WinError(ct.get_last_error())
    pointer, length = ct.c_void_p(), wt.UINT()
    if not query(buffer, "\\", ct.byref(pointer), ct.byref(length)):
        raise ct.WinError(ct.get_last_error())
    fixed = ct.cast(pointer, ct.POINTER(wt.DWORD))
    actual = [fixed[2] >> 16, fixed[2] & 65535, fixed[3] >> 16, fixed[3] & 65535]
    package = (c.root / "Cargo.toml").read_text(encoding="utf-8").split("[package]", 1)[1].split("\n[", 1)[0]
    match = re.search(r'^version\s*=\s*"(\d+)\.(\d+)\.(\d+)"', package, re.MULTILINE)
    expected = [int(part) for part in match.groups()] + [0]
    c.expect("cargo_version_resource", actual == expected, actual=actual, expected=expected)
    app = App(c)
    settings = app.open_settings()
    for label, hwnd in (("workspace", app.hwnd), ("settings", settings)):
        icons = {str(kind): w.message(hwnd, 0x7F, kind) for kind in (0, 1)}
        c.expect(label + "_wm_geticon", any(icons.values()), icons=icons)
