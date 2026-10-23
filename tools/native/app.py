"""Isolated v2 fixtures, bounded process lifetime, and read-only JSONL traces."""
import json
import os
from pathlib import Path
import subprocess
import time

from . import win32 as win


class Skip(Exception):
    pass


def leaf(kind="spectrum", **module):
    return {"node": "leaf", "module": {"type_id": "taffymeters." + kind, **module}}


def session(environment, windows=None, **fields):
    monitor = environment["monitors"][0]
    work, scale = monitor["work_area_px"], monitor["scale"]
    return {"format_version": 2, "theme": {"preset": "dark"},
            "displays": [{"work_area": [n / scale for n in win.size(work)],
                          "hint": {"origin_px": work[:2], "scale": scale}}],
            "windows": windows if windows is not None else [
                {"frame": [80, 80, 480, 300], "layout": leaf()}], **fields}


class App:
    def __init__(self, context, name="app", fixture=None, config=None, input_kind="sweep",
                 product=False, args=None, env=None, ready=True):
        self.context = context
        self.directory = context.directory / name
        self.directory.mkdir()
        self.config = Path(config) if config is not None else self.directory / "config"
        self.config.mkdir(exist_ok=True)
        if fixture is not None or config is None:
            self.write_session(fixture if fixture is not None else session(context.environment))
        self.trace = self.directory / "trace.jsonl"
        self.log = (self.directory / "stderr.log").open("w", encoding="utf-8")
        environment = os.environ.copy() if env is None else env.copy()
        if env is None:
            environment["APPDATA"] = str(self.directory / "appdata")
        if args is None:
            args = ["--config-dir", str(self.config)]
            if not product:
                args = ["--automation", "preview", "--input", input_kind,
                        "--trace", str(self.trace), *args]
        self.process = subprocess.Popen([str(context.exe), *args], cwd=context.root,
                                        env=environment, stdout=subprocess.DEVNULL, stderr=self.log)
        context.apps.append(self)
        if ready:
            self.hwnd = win.wait(self.first_window, "Workspace window", 25)
            if not product:
                win.wait(lambda: self.records("state") and self.targets(1), "first traced UI pass")
            win.stable_rect(self.hwnd)

    def all_windows(self):
        return win.windows(self.process.pid)

    def windows(self):
        return {int(info["title"].split("#")[1]): hwnd
                for hwnd, info in self.all_windows().items()
                if info["title"].startswith("Taffymeters #")}

    def first_window(self):
        if self.process.poll() is not None:
            raise AssertionError(f"App exited {self.process.returncode}: {self.stderr()}")
        found = self.windows()
        return found[min(found)] if found else None

    def settings_window(self):
        return next((hwnd for hwnd, info in self.all_windows().items()
                     if info["title"] == "Taffymeters settings"), None)

    def records(self, kind=None):
        if not self.trace.exists():
            return []
        lines = self.trace.read_text(encoding="utf-8").splitlines(keepends=True)
        rows = []
        for index, line in enumerate(lines):
            try:
                row = json.loads(line)
            except json.JSONDecodeError:
                if index == len(lines) - 1 and not line.endswith("\n"):
                    break
                raise
            if kind is None or row["kind"] == kind:
                rows.append(row)
        return rows

    def state(self):
        rows = self.records("state")
        return rows[-1]["body"] if rows else {}

    def targets(self, window="settings"):
        rows = [row for row in self.records("targets") if row["body"]["window"] == window]
        return {key: (rect, enabled) for key, rect, enabled in rows[-1]["body"]["targets"]} if rows else {}

    def target(self, key, window="settings"):
        def find():
            target = self.targets(window).get(key)
            if target and not target[1]:
                raise AssertionError(f"Disabled control: {key}")
            return target[0] if target else None
        return win.wait(find, f"current {window} target {key}")

    def click(self, item, window="settings"):
        hwnd = self.settings_window() if window == "settings" else self.windows()[window]
        if not hwnd:
            raise AssertionError(f"No native window for target {item}")
        win.u.SetForegroundWindow(hwnd)
        key = f"Settings({item})" if window == "settings" else item
        deadline = time.monotonic() + 8
        while True:
            rect = self.target(key, window)
            client = win.w.RECT()
            if not win.u.GetClientRect(hwnd, win.c.byref(client)):
                raise win.c.WinError(win.c.get_last_error())
            x = (rect[0] + rect[2]) / 2 * win.scale(hwnd)
            y = (rect[1] + rect[3]) / 2 * win.scale(hwnd)
            if 0 <= x < client.right and 24 <= y < client.bottom - 8:
                break
            if window != "settings" or time.monotonic() >= deadline:
                raise AssertionError(f"Control outside client area: {key}, rect={rect}")
            win.u.SetCursorPos(*win.center(hwnd))
            win.mouse(0x800, -120 if y >= client.bottom - 8 else 120)
            time.sleep(0.12)
        win.click(win.screen(hwnd, rect))

    def open_settings(self, window=1):
        hwnd = self.windows()[window]
        win.u.SetForegroundWindow(hwnd)
        targets = self.targets(window)
        body = next((key for key in targets if key.startswith("LeafBody(")), "EmptyBody")
        win.click(win.screen(hwnd, self.target(body, window)), right=True)
        settings = win.wait(self.settings_window, "settings window")
        win.wait(lambda: self.state().get("settings_open"), "settings_open trace")
        self.target("Settings(Tab(Window))")
        return settings

    def quit(self):
        if not self.settings_window():
            self.open_settings(min(self.windows()))
        self.click("Tab(Workspace)")
        self.click("Quit")
        code = self.process.wait(timeout=15)
        if code != 0:
            raise AssertionError(f"Quit exit {code}: {self.stderr()}")
        return code

    def read_session(self):
        return json.loads((self.config / "session.json").read_text(encoding="utf-8"))

    def write_session(self, fixture):
        (self.config / "session.json").write_text(json.dumps(fixture, indent=2), encoding="utf-8")

    def stderr(self):
        self.log.flush()
        return (self.directory / "stderr.log").read_text(encoding="utf-8", errors="replace").strip()

    def cleanup(self):
        if self.process.poll() is None:
            self.process.kill()
            self.process.wait(timeout=10)
        self.log.close()


class Context:
    def __init__(self, root, exe, directory, environment):
        self.root, self.exe, self.directory, self.environment = root, exe, directory, environment
        self.apps, self.observed, self.checks = [], {}, []
        directory.mkdir()

    def expect(self, name, condition, **observed):
        self.observed[name] = observed
        row = {"name": name, "status": "PASS" if condition else "FAIL", "observed": observed}
        self.checks.append(row)
        if not condition:
            row["reason"] = f"Assertion failed: {name}"
            raise AssertionError(f"{name}: {observed}")

    def require_playback(self):
        if not self.environment["has_playback_device"]:
            raise Skip("No playback device reported by WinMM")

    def cleanup(self):
        for app in reversed(self.apps):
            app.cleanup()
