"""Settings ownership and topmost/close operations while Workspace is minimized."""
from ..app import App
from .. import win32 as w


def run(c):
    app = App(c)
    hwnd = app.hwnd
    settings = app.open_settings()
    owner = w.u.GetWindow(settings, 4)
    # Independent settings must not be hidden when the selected Workspace minimizes.
    c.expect("settings_owner", owner is None, owner=owner, expected_owner=None, workspace=hwnd)
    app.click("Tab(Window)")
    w.minimize(hwnd)
    app.click("AlwaysOnTop")
    w.wait(lambda: bool(w.style(hwnd, True) & 8), "topmost style")
    w.wait(lambda: app.state()["windows"][0]["always_on_top"], "topmost trace")
    c.expect("topmost_while_minimized", bool(w.u.IsIconic(hwnd)) and bool(w.style(hwnd, True) & 8),
             iconic=bool(w.u.IsIconic(hwnd)), exstyle=w.style(hwnd, True))
    w.close(settings)
    w.wait(lambda: not app.settings_window(), "settings destroyed while owner minimized")
    w.wait(lambda: app.state().get("settings_open") is False, "settings_open false trace")
    c.expect("minimized_close_trace", not app.state()["settings_open"] and bool(w.u.IsIconic(hwnd)),
             settings_open=app.state()["settings_open"], iconic=bool(w.u.IsIconic(hwnd)))
