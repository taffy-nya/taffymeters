"""Measurement scenarios: product sessions, rebuilt-baseline mapping and timelines.

Load and timeline follow agent PERFORMANCE_ACCEPTANCE section 3 and phase-7
decisions D7/D10. Window frames are logical points relative to the work area.
"""

SPECTRUM = "taffymeters.spectrum"
FRAME = [100.0, 100.0, 900.0, 500.0]  # 800x400 outer; the runner also sets it natively.


def spectrum(channel="mono_average", tilt=4.5):
    """Default Spectrum (Curve style, FFT 2048/512, stateful Peak) with a channel and Tilt."""
    return {"type_id": SPECTRUM, "data": {"nodes": [
        {"input": None, "kind": "channel", "params": channel},
        {"input": 0, "kind": "fft", "params": {"window_size": 2048, "hop_size": 512, "floor_dbfs": -160.0}},
        {"input": 1, "kind": "tilt", "params": {"db_per_octave": tilt}},
        {"input": 2, "kind": "peak", "params": {"hold_ms": 1000.0, "fall_db_per_second_squared": 30.0}}],
        "outputs": [{"node": 2, "keep": "latest"}, {"node": 3, "keep": "latest"}]}}


def module(kind, **fields):
    return {"type_id": "taffymeters." + kind, **fields}


def leaf(module_file):
    return {"node": "leaf", "module": module_file}


def split(axis, first, second, ratio=0.5):
    return {"node": "split", "axis": axis, "ratio": ratio, "manual": False, "first": first, "second": second}


def quad(top_left, bottom_left, top_right, bottom_right):
    """Equal 2x2: left/right halves, each split top/bottom (the baseline's layout)."""
    return split("left_right",
                 split("top_bottom", leaf(top_left), leaf(bottom_left)),
                 split("top_bottom", leaf(top_right), leaf(bottom_right)))


def window(layout, frame=FRAME):
    return {"display": 0, "frame": frame, "layout": layout}


def session(*windows):
    return {"format_version": 2, "theme": {"preset": "dark"},
            "displays": [{"work_area": [1440.0, 912.0]}], "windows": list(windows)}


# Segment actions: visible (none), resize (alternate 600x300 / 1000x500 at ~4 Hz),
# minimize / restore (D10), hide / show (SW_HIDE supplement), close (WM_CLOSE window 0).
SCENARIOS = {
    "single": {
        "session": session(window(leaf(spectrum()))),
        "baseline": "single", "duration": 10.0, "segments": [(0.0, "visible", None)],
        "plots": 1, "windows": 1,
    },
    "shared": {
        "session": session(window(quad(spectrum(), spectrum(), spectrum(), spectrum()))),
        "baseline": "shared", "duration": 10.0, "segments": [(0.0, "visible", None)],
        "plots": 4, "windows": 1,
    },
    # D7: the baseline varies bands/Tilt, which the product has no setting for; this
    # variant varies Tilt and channel and is paired only for a ratio.
    "different": {
        "session": session(window(quad(spectrum("left", -6.0), spectrum("right", 0.0),
                                       spectrum("mono_average", 4.5), spectrum("left", 9.0)))),
        "baseline": "different", "duration": 10.0, "segments": [(0.0, "visible", None)],
        "plots": 4, "windows": 1,
    },
    "lifecycle": {
        "session": session(window(quad(spectrum(), spectrum(), spectrum(), spectrum()))),
        "baseline": "shared", "duration": 20.0,
        "segments": [(0.0, "visible", None), (5.0, "resize", None), (10.0, "hidden", "minimize"),
                     (15.0, "restored", "restore")],
        "plots": 4, "windows": 1,
    },
    # Threshold 4: no stateful node and no History (Keep::Latest PCM only).
    "osc-hidden": {
        "session": session(window(leaf(module("oscilloscope")))),
        "baseline": None, "duration": 20.0,
        "segments": [(0.0, "visible", None), (10.0, "hidden", "minimize")],
        "plots": 1, "windows": 1,
    },
    # Hidden with continuous state: every Spectrum keeps its Peak running.
    "spectrum-hidden": {
        "session": session(window(leaf(spectrum()))),
        "baseline": None, "duration": 20.0,
        "segments": [(0.0, "visible", None), (10.0, "hidden", "minimize")],
        "plots": 1, "windows": 1,
    },
    "sw-hide": {
        "session": session(window(quad(spectrum(), spectrum(), spectrum(), spectrum()))),
        "baseline": None, "duration": 15.0,
        "segments": [(0.0, "visible", None), (5.0, "hidden", "hide"), (10.0, "restored", "show")],
        "plots": 4, "windows": 1,
    },
    # Last related subscription closed: window 0 holds the only Spectrum; window 1 keeps
    # an Oscilloscope so the source and the app stay alive.
    "close-last": {
        "session": session(window(leaf(spectrum())),
                           window(leaf(module("oscilloscope")), [950.0, 100.0, 1350.0, 300.0])),
        "baseline": None, "duration": 20.0,
        "segments": [(0.0, "visible", None), (10.0, "closed", "close")],
        "plots": 2, "windows": 2,
    },
    # Source alive, zero analysis: one empty window.
    "zero-analysis": {
        "session": session({"display": 0, "frame": FRAME}),
        "baseline": None, "duration": 10.0, "segments": [(0.0, "visible", None)],
        "plots": 0, "windows": 1,
    },
    "ext-mixed": {
        "session": session(window(split(
            "left_right",
            quad(spectrum(), module("spectrogram"), module("oscilloscope"), module("waveform")),
            quad(module("stereometer"), module("level_meter"), module("loudness_meter"),
                 {**spectrum(), "style": "taffymeters.spectrum.bars"})))),
        "baseline": None, "duration": 10.0, "segments": [(0.0, "visible", None)],
        "plots": 8, "windows": 1,
    },
    "ext-windows": {
        "session": session(
            window(split("left_right", leaf(spectrum()), leaf(module("spectrogram")))),
            window(split("left_right", leaf(module("oscilloscope")), leaf(module("waveform"))),
                   [950.0, 100.0, 1350.0, 300.0]),
            window(split("left_right", leaf(module("stereometer")), leaf(module("level_meter"))),
                   [100.0, 550.0, 500.0, 750.0]),
            window(split("left_right", leaf(module("loudness_meter")),
                         leaf({**spectrum("left"), "style": "taffymeters.spectrum.pixel"})),
                   [950.0, 550.0, 1350.0, 750.0])),
        "baseline": None, "duration": 10.0, "segments": [(0.0, "visible", None)],
        "plots": 8, "windows": 4,
    },
}

LOCAL = {"format_version": 1, "input": {"kind": "system_default"}}
