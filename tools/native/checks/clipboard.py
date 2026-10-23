"""System CF_UNICODETEXT reaches the real color Hex field via Ctrl+V/Enter."""
import time
from ..app import App
from .. import win32 as w


def find(app, part):
    return next((key for key in app.targets() if "ColorPicker" in key and "Instance" in key
                 and "Primary" in key and f"part: {part}" in key), None)


def run(c):
    app = App(c)
    settings = app.open_settings()
    # Color controls are below the analysis section. Scroll the actual settings body.
    w.u.SetForegroundWindow(settings)
    w.u.SetCursorPos(*w.center(settings))
    deadline = time.monotonic() + 10
    button = None
    while time.monotonic() < deadline:
        button = find(app, "Button")
        if button:
            rect = app.targets()[button][0]
            client = w.w.RECT()
            w.u.GetClientRect(settings, w.c.byref(client))
            y = (rect[1] + rect[3]) / 2 * w.scale(settings)
            if 30 < y < client.bottom - 30:
                break
        w.mouse(0x800, -120)
        time.sleep(0.12)
    else:
        raise AssertionError("Primary color picker not visible after scrolling")
    win_rect = app.target(button)
    w.click(w.screen(settings, win_rect))
    key = w.wait(lambda: find(app, "Hex"), "open picker Hex target")
    rect = app.target(key)
    w.click(w.screen(settings, rect))
    w.clipboard("#a6e3a1")
    w.keys(0x11, 0x41)
    w.keys(0x11, 0x56)
    w.keys(0x0D)
    expected = "#a6e3a1"
    def saved_color():
        module = app.read_session()["windows"][0]["layout"]["module"]
        color = module.get("colors", {}).get("primary")
        return color if isinstance(color, str) and color.lower() == expected else None
    value = w.wait(saved_color, "pasted session color", 8)
    c.expect("clipboard_hex_persisted", value.lower() == expected, expected=expected, saved=value)
