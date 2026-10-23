"""Startup error dialogs, stderr/exit status, immediate session-end save and Quit."""
import os
from ..app import App
from .. import win32 as w


def run(c):
    for label, args in (("bad_arguments", ["--bogus"]), ("missing_appdata", [])):
        environment = os.environ.copy()
        environment["APPDATA"] = str(c.directory / "isolated-appdata")
        if label == "missing_appdata":
            environment = {key: value for key, value in environment.items() if key.upper() != "APPDATA"}
        app = App(c, label, product=True, args=args, env=environment, ready=False)
        dialog = w.wait(lambda: next((hwnd for hwnd, info in app.all_windows().items()
                                    if info["class"] == "#32770"), None), "startup MessageBox")
        w.close(dialog)
        code = app.process.wait(timeout=10)
        stderr = app.stderr()
        c.expect(label, bool(dialog) and code == 1 and bool(stderr),
                 dialog_class="#32770", exit_code=code, stderr=stderr)
    c.require_playback()
    app = App(c, "endsession", product=True)
    work = c.environment["monitors"][0]["work_area_px"]
    w.move(app.hwnd, work[0] + 410, work[1] + 310)
    frame = w.stable_rect(app.hwnd)
    scale = w.scale(app.hwnd)
    w.message(app.hwnd, 0x16, 1)
    expected = [(v - work[i % 2]) / scale for i, v in enumerate(frame)]
    saved = app.read_session()["windows"][0]["frame"]
    c.expect("endsession_frame_saved", w.error_px(saved, expected) * scale <= 2,
             frame_px=frame, saved_frame_pt=saved, expected_frame_pt=expected, threshold_px=2)
    app.cleanup()
    # Quit is driven through the traced real control, with a real system source.
    quitting = App(c, "quit", input_kind="system")
    code = quitting.quit()
    c.expect("quit_exit_zero", code == 0, exit_code=code)
