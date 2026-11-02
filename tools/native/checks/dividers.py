"""Real divider drags: other dividers stay, Shift moves aligned ones, Ctrl snaps, Escape restores."""
import time

from ..app import App, leaf, session
from .. import win32 as w

SHIFT, CONTROL, ALT = 0x10, 0x11, 0x12
# Logical points: a free drag lands within half a physical pixel; traced rects round to 0.1.
MOVED, FIXED = 1.0, 0.11
MIN_POINTS = 16.0
GRID_POINTS = 16.0


def split(axis, first, second):
    return {"node": "split", "axis": axis, "ratio": 0.5, "manual": False, "first": first, "second": second}


def run(c):
    # [A | (B | D) / (C | E)] in a 900x420 window: both inner rows split at 675. Empty
    # modules keep the pixels beside the dividers static for the highlight check.
    empty = lambda: leaf("empty")
    layout = split("left_right", empty(), split(
        "top_bottom", split("left_right", empty(), empty()), split("left_right", empty(), empty())))
    app = App(c, fixture=session(c.environment, [{"frame": [80, 80, 980, 500], "layout": layout}]))
    hwnd = app.hwnd
    window = app.state()["windows"][0]
    outer, top, bottom = (window["splits"][i]["id"] for i in (0, 2, 3))
    a, b, _, lower, _ = window["leaves"]
    top_y, bottom_y = 105, 315

    def rects():
        targets = app.settled_targets(1)
        return {leaf_id: targets[f"LeafBody(ModuleInstanceId({leaf_id}))"][0] for leaf_id in (a, b, lower)}

    def edges():
        found = rects()
        return {"outer": found[a][2], "top": found[b][2], "bottom": found[lower][2]}

    def states():
        return {s["id"]: (s["ratio"], s["manual"]) for s in app.state()["windows"][0]["splits"]}

    def drag(x_from, x_to, y, hold=(), escape=False):
        w.u.SetForegroundWindow(hwnd)
        w.drag(w.screen(hwnd, [x_from, y, x_from, y]), w.screen(hwnd, [x_to, y, x_to, y]),
               hold=hold, escape=escape)
        time.sleep(0.3)

    # Press the outer divider in the lower row: the whole line moves, nested lines stay.
    drag(450, 300, bottom_y, hold=(ALT,))
    after, found = edges(), rects()
    c.expect("outer_drag_keeps_nested", abs(after["outer"] - 300) <= MOVED
             and abs(after["top"] - 675) <= FIXED and abs(after["bottom"] - 675) <= FIXED
             and abs(found[a][3] - found[a][1] - 420) <= FIXED,
             edges=after, a_rect=found[a])
    manual = {key: states()[key][1] for key in (outer, top, bottom)}
    c.expect("only_dragged_becomes_custom", manual == {outer: True, top: False, bottom: False}, manual=manual)

    drag(675, 600, top_y, hold=(ALT, SHIFT))
    after = edges()
    custom = [states()[key][1] for key in (top, bottom)]
    c.expect("shift_moves_aligned", abs(after["top"] - 600) <= MOVED and abs(after["bottom"] - after["top"]) <= FIXED
             and custom == [True, True], edges=after, custom=custom)

    drag(600, 712, top_y, hold=(ALT,))
    after = edges()
    c.expect("plain_drag_leaves_aligned", abs(after["top"] - 712) <= MOVED
             and abs(after["bottom"] - 600) <= FIXED, edges=after)

    # Bottom fractions are 450/500/600/700/750; the top divider is nearer, and Ctrl beats Alt.
    target = after["top"]
    drag(600, target + 3, bottom_y, hold=(ALT, CONTROL))
    after = edges()
    c.expect("ctrl_snaps_to_parallel", abs(after["bottom"] - target) <= FIXED, edges=after, target=target)

    # Alt+Esc and Shift+Alt+Esc are Windows shortcuts; the window holds them during the drag.
    for name, hold in (("alt_shift_escape_restores_all", (ALT, SHIFT)), ("alt_escape_restores_all", (ALT,))):
        before = states()
        drag(after["top"], 500, top_y, hold=hold, escape=True)
        restored = states()
        c.expect(name, restored == before and app.state()["windows"][0]["divider_drag"] is None
                 and w.u.GetForegroundWindow() == hwnd, before=before, after=restored)

    # Shift while hovering highlights the aligned bottom divider.
    from PIL import ImageGrab
    line = edges()["bottom"]
    w.u.SetForegroundWindow(hwnd)
    w.u.SetCursorPos(*w.screen(hwnd, [line, top_y, line, top_y]))
    time.sleep(0.4)
    x, y = w.screen(hwnd, [line, bottom_y, line, bottom_y])

    def strip():
        w.redraw(hwnd)
        time.sleep(0.3)
        return list(ImageGrab.grab(bbox=(x - 2, y, x + 3, y + 1), all_screens=True).getdata())

    plain = strip()
    w.key(SHIFT)
    try:
        time.sleep(0.3)
        shifted = strip()
    finally:
        w.key(SHIFT, True)
    difference = max(abs(p - q) for before_px, after_px in zip(plain, shifted) for p, q in zip(before_px, after_px))
    c.expect("shift_hover_highlights_aligned", difference >= 24, plain=plain, shifted=shifted, difference=difference)

    # Without modifiers the grid is anchored at the row's start, the outer divider.
    start = edges()
    drag(start["top"], 641, top_y)
    after = edges()
    steps = (after["top"] - start["outer"]) / GRID_POINTS
    c.expect("grid_from_group_start", abs(steps - round(steps)) * GRID_POINTS <= FIXED
             and abs(after["top"] - 641) <= GRID_POINTS / 2 + MOVED
             and abs(after["outer"] - start["outer"]) <= FIXED, edges=after, steps=steps)

    # The drag stops when B reaches its minimum width; nothing else is pushed.
    drag(after["top"], 200, top_y, hold=(ALT,))
    final = edges()
    c.expect("stops_at_minimum", abs(final["top"] - (start["outer"] + MIN_POINTS)) <= MOVED
             and abs(final["outer"] - start["outer"]) <= FIXED and abs(final["bottom"] - start["bottom"]) <= FIXED,
             edges=final, minimum_pt=MIN_POINTS)
