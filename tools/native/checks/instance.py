"""A second launch on the same config dir raises the running instance and exits 0;
another config dir runs alongside."""
import time

from ..app import App
from .. import win32 as w


def settle(predicate, timeout=8):
    deadline = time.monotonic() + timeout
    while not predicate() and time.monotonic() < deadline:
        time.sleep(0.04)
    return predicate()


def run(c):
    c.require_playback()
    first = App(c, "first", product=True)
    w.minimize(first.hwnd)
    second = App(c, "second", config=first.config, product=True, ready=False)
    code = second.process.wait(timeout=15)
    raised = settle(lambda: not w.u.IsIconic(first.hwnd)
                    and w.u.GetForegroundWindow() == first.hwnd)
    c.expect("same_dir_raises_and_exits", code == 0 and raised and first.process.poll() is None,
             exit_code=code, iconic=bool(w.u.IsIconic(first.hwnd)),
             foreground_is_first=w.u.GetForegroundWindow() == first.hwnd, stderr=second.stderr())
    other = App(c, "other", product=True)
    c.expect("other_dir_runs_alongside",
             other.process.poll() is None and first.process.poll() is None,
             first_windows=len(first.windows()), other_windows=len(other.windows()))
