"""Snapping, group following, cancellation, and native sizing-loop edges."""
from ..app import App, leaf, session
from .. import win32 as w


def run(c):
    fixture = session(c.environment, [
        {"frame": [100, 80, 320, 200], "group": 1, "layout": leaf()},
        {"frame": [320, 80, 540, 200], "group": 1, "layout": leaf("oscilloscope")},
        {"frame": [100, 340, 320, 460], "layout": leaf("level_meter")}])
    app = App(c, fixture=fixture)
    windows = w.wait(lambda: app.windows() if len(app.windows()) == 3 else None, "three windows")
    a, b, d = (windows[n] for n in (1, 2, 3))
    work = c.environment["monitors"][0]["work_area_px"]
    before = {n: w.rect(h) for n, h in windows.items()}
    w.move(a, before[1][0] + 60, before[1][1] + 40)
    w.wait(lambda: w.rect(b)[0] == before[2][0] + 60, "external group following")
    c.expect("external_group_follow", w.rect(b) == [v + (60 if i % 2 == 0 else 40)
             for i, v in enumerate(before[2])] and w.rect(d) == before[3], frames={n: w.rect(h) for n, h in windows.items()})
    start = w.center(d)
    ar, dr = w.rect(a), w.rect(d)
    w.drag(start, (start[0] + ar[0] - dr[0] + 6, start[1] + ar[3] - dr[1] + 10))
    end = w.stable_rect(d)
    c.expect("neighbor_snap", abs(end[0] - ar[0]) <= 1 and abs(end[1] - ar[3]) <= 1,
             neighbor=ar, moved=end, threshold_px=1)
    w.move(d, before[3][0], before[3][1])
    w.stable_rect(d)
    start, dr = w.center(d), w.rect(d)
    goal = (start[0] + ar[0] - dr[0] + 6, start[1] + ar[3] - dr[1] + 10)
    w.drag(start, goal, alt=True)
    end = w.stable_rect(d)
    c.expect("alt_disables_snap", w.error_px(end[:2], [ar[0] + 6, ar[3] + 10]) <= 1,
             moved=end, expected_origin=[ar[0] + 6, ar[3] + 10])
    before_escape = {n: w.rect(h) for n, h in windows.items()}
    start = w.center(a)
    w.drag(start, (start[0] + 120, start[1] + 90), escape=True)
    after_escape = {n: w.stable_rect(h) for n, h in windows.items()}
    c.expect("escape_restores_group", before_escape == after_escape,
             before=before_escape, after=after_escape)
    # The group has two windows; use the standalone window for every work-area edge.
    for label, edge in (("left", 0), ("top", 1), ("right", 2), ("bottom", 3)):
        w.move(d, work[0] + 300, work[1] + 500)
        dr = w.stable_rect(d)
        start = w.center(d)
        delta = work[edge] - dr[edge] + (8 if edge < 2 else -8)
        goal = (start[0] + (delta if edge % 2 == 0 else 0),
                start[1] + (delta if edge % 2 else 0))
        w.drag(start, goal)
        end = w.stable_rect(d)
        c.expect("work_area_" + label, abs(end[edge] - work[edge]) <= 1,
                 frame=end, work_area=work, error_px=abs(end[edge] - work[edge]), threshold_px=1)
    # A real corner drag enters WM_SIZING, unlike SetWindowPos.
    w.move(d, work[0] + 200, work[1] + 500)
    w.resize(d, 350, 220)
    dr = w.stable_rect(d)
    start = (dr[2] - 3, dr[3] - 3)
    w.drag(start, (work[2] - 11, dr[3] - 3))
    end = w.stable_rect(d)
    c.expect("wm_sizing_snap", abs(end[2] - work[2]) <= 1,
             frame=end, work_area=work, threshold_px=1)
