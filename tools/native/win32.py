"""Physical-pixel Win32 desktop operations used by the native smoke checks."""
import ctypes as c
from ctypes import wintypes as w
import time

u = c.WinDLL("user32", use_last_error=True)
k = c.WinDLL("kernel32", use_last_error=True)
shell = c.WinDLL("shell32", use_last_error=True)
EnumProc = c.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)


def bind(dll, name, restype, *args):
    fn = getattr(dll, name)
    fn.restype, fn.argtypes = restype, args
    return fn


bind(u, "EnumWindows", w.BOOL, EnumProc, w.LPARAM)
bind(u, "GetWindowThreadProcessId", w.DWORD, w.HWND, c.POINTER(w.DWORD))
bind(u, "GetWindowTextW", c.c_int, w.HWND, w.LPWSTR, c.c_int)
bind(u, "GetClassNameW", c.c_int, w.HWND, w.LPWSTR, c.c_int)
for name in ("IsWindowVisible", "IsWindow", "IsIconic", "IsZoomed", "SetForegroundWindow"):
    bind(u, name, w.BOOL, w.HWND)
bind(u, "GetForegroundWindow", w.HWND)
bind(u, "GetWindow", w.HWND, w.HWND, w.UINT)
bind(u, "GetWindowRect", w.BOOL, w.HWND, c.POINTER(w.RECT))
bind(u, "GetClientRect", w.BOOL, w.HWND, c.POINTER(w.RECT))
bind(u, "ClientToScreen", w.BOOL, w.HWND, c.POINTER(w.POINT))
bind(u, "GetDpiForWindow", w.UINT, w.HWND)
bind(u, "GetWindowLongW", w.LONG, w.HWND, c.c_int)
bind(u, "SetWindowPos", w.BOOL, w.HWND, w.HWND, c.c_int, c.c_int, c.c_int, c.c_int, w.UINT)
bind(u, "ShowWindow", w.BOOL, w.HWND, c.c_int)
bind(u, "PostMessageW", w.BOOL, w.HWND, w.UINT, w.WPARAM, w.LPARAM)
bind(u, "SendMessageTimeoutW", c.c_ssize_t, w.HWND, w.UINT, w.WPARAM, w.LPARAM,
     w.UINT, w.UINT, c.POINTER(c.c_size_t))
bind(u, "RedrawWindow", w.BOOL, w.HWND, c.POINTER(w.RECT), w.HANDLE, w.UINT)
bind(u, "SetCursorPos", w.BOOL, c.c_int, c.c_int)
bind(u, "GetCursorPos", w.BOOL, c.POINTER(w.POINT))
bind(u, "MonitorFromWindow", w.HANDLE, w.HWND, w.DWORD)
bind(u, "GetMonitorInfoW", w.BOOL, w.HANDLE, c.c_void_p)
bind(u, "SetProcessDpiAwarenessContext", w.BOOL, c.c_void_p)
u.SetProcessDpiAwarenessContext(c.c_void_p(-4))


class MonitorInfo(c.Structure):
    _fields_ = [("size", w.DWORD), ("monitor", w.RECT), ("work", w.RECT), ("flags", w.DWORD)]


class Mouse(c.Structure):
    _fields_ = [("dx", w.LONG), ("dy", w.LONG), ("data", w.DWORD),
                ("flags", w.DWORD), ("time", w.DWORD), ("extra", c.c_size_t)]


class Keyboard(c.Structure):
    _fields_ = [("vk", w.WORD), ("scan", w.WORD), ("flags", w.DWORD),
                ("time", w.DWORD), ("extra", c.c_size_t)]


class Payload(c.Union):
    _fields_ = [("mouse", Mouse), ("keyboard", Keyboard)]


class Input(c.Structure):
    _fields_ = [("kind", w.DWORD), ("payload", Payload)]


bind(u, "SendInput", w.UINT, w.UINT, c.POINTER(Input), c.c_int)


def wait(predicate, description, timeout=12):
    deadline = time.monotonic() + timeout
    while True:
        value = predicate()
        if value:
            return value
        if time.monotonic() >= deadline:
            raise AssertionError(f"Timed out waiting for {description}")
        time.sleep(0.04)


def windows(pid):
    found = {}

    @EnumProc
    def each(hwnd, _):
        owner = w.DWORD()
        u.GetWindowThreadProcessId(hwnd, c.byref(owner))
        if owner.value == pid and u.IsWindowVisible(hwnd):
            title, cls = c.create_unicode_buffer(256), c.create_unicode_buffer(128)
            u.GetWindowTextW(hwnd, title, len(title))
            u.GetClassNameW(hwnd, cls, len(cls))
            found[hwnd] = {"title": title.value, "class": cls.value}
        return True

    u.EnumWindows(each, 0)
    return found


def rect(hwnd):
    result = w.RECT()
    if not u.GetWindowRect(hwnd, c.byref(result)):
        raise c.WinError(c.get_last_error())
    return [result.left, result.top, result.right, result.bottom]


def stable_rect(hwnd, timeout=8):
    previous, since = None, time.monotonic()

    def settled():
        nonlocal previous, since
        current = rect(hwnd)
        if current != previous:
            previous, since = current, time.monotonic()
        return current if time.monotonic() - since >= 0.35 else None

    return wait(settled, "stable window frame", timeout)


def size(frame):
    return [frame[2] - frame[0], frame[3] - frame[1]]


def error_px(a, b):
    return max(abs(x - y) for x, y in zip(a, b))


def scale(hwnd):
    return u.GetDpiForWindow(hwnd) / 96.0


def screen(hwnd, logical_rect):
    point = w.POINT(round((logical_rect[0] + logical_rect[2]) / 2 * scale(hwnd)),
                    round((logical_rect[1] + logical_rect[3]) / 2 * scale(hwnd)))
    if not u.ClientToScreen(hwnd, c.byref(point)):
        raise c.WinError(c.get_last_error())
    return point.x, point.y


def center(hwnd):
    r = rect(hwnd)
    return (r[0] + r[2]) // 2, (r[1] + r[3]) // 2


def send(inp):
    if u.SendInput(1, c.byref(inp), c.sizeof(inp)) != 1:
        raise c.WinError(c.get_last_error())


def mouse(flags, data=0):
    send(Input(0, Payload(mouse=Mouse(0, 0, data & 0xFFFFFFFF, flags, 0, 0))))


def key(vk, up=False):
    send(Input(1, Payload(keyboard=Keyboard(vk, 0, 2 if up else 0, 0, 0))))


def keys(*codes):
    try:
        for code in codes:
            key(code)
    finally:
        for code in reversed(codes):
            key(code, True)
    time.sleep(0.08)


def click(point, right=False):
    u.SetCursorPos(point[0] + 2, point[1] + 2)
    time.sleep(0.04)
    u.SetCursorPos(*point)
    time.sleep(0.16)
    mouse(8 if right else 2)
    time.sleep(0.09)
    mouse(16 if right else 4)
    time.sleep(0.16)


def glide(start, end):
    for step in range(1, 41):
        u.SetCursorPos(round(start[0] + (end[0] - start[0]) * step / 40),
                       round(start[1] + (end[1] - start[1]) * step / 40))
        time.sleep(0.012)


def drag(start, end, alt=False, escape=False):
    u.SetCursorPos(*start)
    time.sleep(0.2)
    mouse(2)
    time.sleep(0.12)
    try:
        if alt:
            key(0x12)
        glide(start, end)
        time.sleep(0.2)
        if escape:
            keys(0x1B)
    finally:
        mouse(4)
        if alt:
            key(0x12, True)


def move(hwnd, x, y):
    if not u.SetWindowPos(hwnd, None, round(x), round(y), 0, 0, 0x15):
        raise c.WinError(c.get_last_error())


def resize(hwnd, width, height):
    if not u.SetWindowPos(hwnd, None, 0, 0, round(width), round(height), 0x16):
        raise c.WinError(c.get_last_error())


def message(hwnd, msg, wp=0, lp=0):
    result = c.c_size_t()
    if not u.SendMessageTimeoutW(hwnd, msg, wp, lp, 2, 5000, c.byref(result)):
        raise c.WinError(c.get_last_error())
    return result.value


def redraw(hwnd):
    if not u.RedrawWindow(hwnd, None, None, 0x101):
        raise c.WinError(c.get_last_error())


def close(hwnd):
    u.PostMessageW(hwnd, 0x10, 0, 0)


def minimize(hwnd):
    u.PostMessageW(hwnd, 0x112, 0xF020, 0)
    wait(lambda: u.IsIconic(hwnd), "window minimized")


def restore(hwnd):
    u.ShowWindow(hwnd, 9)
    wait(lambda: not u.IsIconic(hwnd) and not u.IsZoomed(hwnd), "window restored")
    return stable_rect(hwnd)


def style(hwnd, extended=False):
    return u.GetWindowLongW(hwnd, -20 if extended else -16) & 0xFFFFFFFF


def monitors():
    result = []
    callback_type = c.WINFUNCTYPE(w.BOOL, w.HANDLE, w.HDC, c.POINTER(w.RECT), w.LPARAM)
    bind(u, "EnumDisplayMonitors", w.BOOL, w.HDC, c.POINTER(w.RECT), callback_type, w.LPARAM)
    get_dpi = bind(c.WinDLL("shcore"), "GetDpiForMonitor", c.c_long,
                   w.HANDLE, c.c_int, c.POINTER(w.UINT), c.POINTER(w.UINT))

    @callback_type
    def each(handle, _dc, _rect, _data):
        info = MonitorInfo(c.sizeof(MonitorInfo))
        if not u.GetMonitorInfoW(handle, c.byref(info)):
            raise c.WinError(c.get_last_error())
        x, y = w.UINT(), w.UINT()
        hr = get_dpi(handle, 0, c.byref(x), c.byref(y))
        coords = lambda r: [r.left, r.top, r.right, r.bottom]
        result.append({"primary": bool(info.flags & 1), "rect_px": coords(info.monitor),
                       "work_area_px": coords(info.work), "dpi": [x.value, y.value] if hr == 0 else None,
                       "scale": x.value / 96 if hr == 0 else None})
        return True

    if not u.EnumDisplayMonitors(None, None, each, 0):
        raise c.WinError(c.get_last_error())
    return sorted(result, key=lambda item: not item["primary"])


def environment():
    winmm = c.WinDLL("winmm")
    playback, capture = winmm.waveOutGetNumDevs(), winmm.waveInGetNumDevs()
    displays = monitors()
    return {"monitor_count": len(displays), "monitors": displays,
            "playback_device_count": playback, "capture_device_count": capture,
            "has_playback_device": playback > 0, "has_capture_device": capture > 0,
            "audio_detection": "WinMM waveOutGetNumDevs/waveInGetNumDevs"}


def clipboard(text):
    bind(u, "OpenClipboard", w.BOOL, w.HWND)
    bind(u, "SetClipboardData", w.HANDLE, w.UINT, w.HANDLE)
    bind(k, "GlobalAlloc", w.HGLOBAL, w.UINT, c.c_size_t)
    bind(k, "GlobalLock", c.c_void_p, w.HGLOBAL)
    bind(k, "GlobalUnlock", w.BOOL, w.HGLOBAL)
    bind(k, "GlobalFree", w.HGLOBAL, w.HGLOBAL)
    wait(lambda: u.OpenClipboard(None), "clipboard available")
    handle = None
    try:
        if not u.EmptyClipboard():
            raise c.WinError(c.get_last_error())
        data = (text + "\0").encode("utf-16-le")
        handle = k.GlobalAlloc(0x42, len(data))
        if not handle:
            raise c.WinError(c.get_last_error())
        pointer = k.GlobalLock(handle)
        if not pointer:
            raise c.WinError(c.get_last_error())
        c.memmove(pointer, data, len(data))
        k.GlobalUnlock(handle)
        if not u.SetClipboardData(13, handle):
            raise c.WinError(c.get_last_error())
        handle = None
    finally:
        u.CloseClipboard()
        if handle:
            k.GlobalFree(handle)
