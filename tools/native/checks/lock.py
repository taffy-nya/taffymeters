"""Lock removes native resize style and blocks shell snapping; unlock restores it."""
import time
from ..app import App
from .. import win32 as w


def run(c):
    app = App(c)
    hwnd = app.hwnd
    app.open_settings()
    app.click("Tab(Window)")
    app.click("Lock")
    w.wait(lambda: app.state()["windows"][0]["locked"] and not w.style(hwnd) & 0x40000, "locked native style")
    c.expect("locked_removes_sizebox", not w.style(hwnd) & 0x40000, style=w.style(hwnd))
    before = w.stable_rect(hwnd)
    settings = app.settings_window()
    w.close(settings)
    w.wait(lambda: not app.settings_window(), "settings closed")
    w.click(w.center(hwnd))
    w.wait(lambda: w.u.GetForegroundWindow() == hwnd, "locked window foreground")
    w.keys(0x5B, 0x25)
    # The shell shortcut is asynchronous; establish an observation interval, not just one immediate read.
    deadline = time.monotonic() + 1.0
    observed = []
    while time.monotonic() < deadline:
        observed.append(w.rect(hwnd))
        time.sleep(0.04)
    c.expect("locked_win_left_unchanged", all(frame == before for frame in observed),
             before=before, after=observed[-1], style=w.style(hwnd), samples=len(observed))
    app.open_settings()
    app.click("Tab(Window)")
    app.click("Lock")
    w.wait(lambda: not app.state()["windows"][0]["locked"] and w.style(hwnd) & 0x40000, "unlocked native style")
    c.expect("unlock_restores_sizebox", bool(w.style(hwnd) & 0x40000), style=w.style(hwnd))
