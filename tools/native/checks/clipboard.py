"""System CF_UNICODETEXT reaches the real color Hex field via Ctrl+V/Enter."""
from ..app import App
from .. import win32 as w


def run(c):
    app = App(c)
    instance = app.state()["modules"][0]["id"]
    app.open_settings()
    picker = f"ColorPicker {{ scope: Instance(ModuleInstanceId({instance})), role: Primary, part: "
    app.click(picker + "Button }")
    app.click(picker + "Hex }")
    expected = "#a6e3a1"
    w.clipboard(expected)
    w.keys(0x11, 0x41)
    w.keys(0x11, 0x56)
    w.keys(0x0D)

    def saved_color():
        module = app.read_session()["windows"][0]["layout"]["module"]
        color = module.get("colors", {}).get("primary")
        return color if isinstance(color, str) and color.lower() == expected else None
    value = w.wait(saved_color, "pasted session color", 8)
    c.expect("clipboard_hex_persisted", value.lower() == expected, expected=expected, saved=value)
