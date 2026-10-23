"""Real corner resizing and maximize/minimize restoration."""
from ..app import App, session
from .. import win32 as w


def run(c):
    fixture = session(c.environment, [{"frame": [150, 100, 350, 240]}])
    app = App(c, fixture=fixture)
    hwnd = app.hwnd
    minimum = app.state()["windows"][0]["min_size"]
    before = w.rect(hwnd)
    start = (before[2] - 3, before[3] - 3)
    w.drag(start, (before[0] + 4, before[1] + 4))
    frame = w.stable_rect(hwnd)
    actual = w.size(frame)
    c.expect("corner_enforces_minimum", all(v >= n * w.scale(hwnd) - 1 for v, n in zip(actual, minimum))
             and actual[0] < w.size(before)[0] and actual[1] < w.size(before)[1],
             before=before, frame=frame, minimum_pt=minimum, scale=w.scale(hwnd))
    w.u.ShowWindow(hwnd, 3)
    w.wait(lambda: w.u.IsZoomed(hwnd), "maximized")
    maximized = w.rect(hwnd)
    restored = w.restore(hwnd)
    c.expect("maximize_restore_unchanged", restored == frame, before=frame, maximized=maximized, restored=restored)
    w.minimize(hwnd)
    restored = w.restore(hwnd)
    c.expect("minimize_restore_unchanged", restored == frame, before=frame, restored=restored)
