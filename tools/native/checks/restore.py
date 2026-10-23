"""Quit/reopen geometry, offscreen recovery, and real-work-area Bar presets."""
from ..app import App, leaf, session
from .. import win32 as w


def run(c):
    app = App(c)
    work = c.environment["monitors"][0]["work_area_px"]
    w.move(app.hwnd, work[0] + 250, work[1] + 220)
    before = w.stable_rect(app.hwnd)
    app.quit()
    reopened = App(c, "reopened", config=app.config)
    after = w.stable_rect(reopened.hwnd)
    c.expect("quit_restart_frame", w.error_px(before, after) <= 2,
             before=before, after=after, error_px=w.error_px(before, after), threshold_px=2)
    reopened.quit()
    fixture = session(c.environment, [{"frame": [20000, 20000, 20300, 20200], "layout": leaf()}])
    recovered = App(c, "offscreen", fixture=fixture)
    frame = w.rect(recovered.hwnd)
    c.expect("offscreen_recovered", frame[0] >= work[0] - 2 and frame[1] >= work[1] - 2
             and frame[2] <= work[2] + 2 and frame[3] <= work[3] + 2,
             frame=frame, work_area=work, threshold_px=2)
    recovered.open_settings()
    recovered.click("Tab(Workspace)")
    for index, edge in ((1, 1), (2, 3)):
        previous = set(recovered.windows())
        recovered.click(f"BuiltinPreset({index})")
        found = w.wait(lambda: recovered.windows() if set(recovered.windows()) != previous else None,
                       "preset replacement windows")
        frame = w.stable_rect(found[min(found)])
        c.expect("bar_top" if index == 1 else "bar_bottom",
                 abs(frame[edge] - work[edge]) <= 2 and abs(frame[0] - work[0]) <= 2
                 and abs(frame[2] - work[2]) <= 2, frame=frame, work_area=work, threshold_px=2)
