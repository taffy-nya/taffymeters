"""One minimized Workspace stops passes while its visible peer keeps analysis alive."""
from ..app import App, leaf, session
from .. import win32 as w


def rate_for(row, window):
    return dict(row["body"]["passes_per_second"]).get(window, 0)


def run(c):
    app = App(c, fixture=session(c.environment, [
        {"frame": [70, 80, 350, 270], "layout": leaf()},
        {"frame": [390, 80, 670, 270], "layout": leaf("oscilloscope")}]))
    found = w.wait(lambda: app.windows() if len(app.windows()) == 2 else None, "two windows")
    w.wait(lambda: any(rate_for(row, 1) > 0 and rate_for(row, 2) > 0 for row in app.records("rate")), "both windows rendering")
    w.minimize(found[1])
    baseline = len(app.records("rate"))
    rows = w.wait(lambda: app.records("rate")[baseline + 1:] if len(app.records("rate")) >= baseline + 3 else None,
                  "two full minimized rate samples", 8)
    c.expect("minimized_zero_passes_analysis_continues", all(rate_for(row, 1) == 0 and rate_for(row, 2) > 0
             and row["body"]["fft_evaluations_per_second"] > 0 for row in rows), samples=rows)
    baseline = len(app.records("rate"))
    w.restore(found[1])
    row = w.wait(lambda: next((row for row in app.records("rate")[baseline:]
                             if rate_for(row, 1) > 0), None), "restored rendering", 8)
    c.expect("restore_resumes_passes", rate_for(row, 1) > 0, sample=row)
