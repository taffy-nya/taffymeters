"""Native New placement, independent close, repeated empty right-click and focus."""
from ..app import App, session
from .. import win32 as w


def run(c):
    app = App(c, fixture=session(c.environment, [{"frame": [100, 80, 420, 260]}]))
    source = app.hwnd
    app.open_settings()
    app.click("Tab(Window)")
    before = w.rect(source)
    app.click("NewWindow")
    found = w.wait(lambda: app.windows() if len(app.windows()) == 2 else None, "new window")
    new = found[2]
    frame = w.stable_rect(new)
    offset = round(32 * w.scale(source))
    c.expect("new_size_and_offset", w.size(frame) == w.size(before) and
             frame[:2] == [before[0] + offset, before[1] + offset],
             source=before, new=frame, expected_offset_px=offset)
    w.close(new)
    w.wait(lambda: len(app.windows()) == 1, "only new window closed")
    c.expect("source_survives_close", app.windows().get(1) == source and w.u.IsWindow(source), windows=app.windows())
    settings = app.settings_window()
    if settings:
        w.close(settings)
        w.wait(lambda: not app.settings_window(), "settings closed")
    rect = app.target("EmptyBody", 1)
    point = w.screen(source, rect)
    for index in range(2):
        w.click(point, right=True)
        settings = w.wait(app.settings_window, "empty-window settings")
        c.expect(f"empty_right_click_{index + 1}", bool(settings), hwnd=settings)
        w.close(settings)
        w.wait(lambda: not app.settings_window(), "settings closed")
    w.click(point)
    w.wait(lambda: w.u.GetForegroundWindow() == source, "selected window foreground")
    c.expect("left_click_foreground", w.u.GetForegroundWindow() == source, foreground=w.u.GetForegroundWindow(), expected=source)
