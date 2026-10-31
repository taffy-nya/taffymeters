"""Measurement scenarios: product sessions, rebuilt-baseline mapping and timelines.

Load and timeline follow agent PERFORMANCE_ACCEPTANCE section 3 and phase-7
decisions D7/D10. Window frames are logical points relative to the work area.
"""

SPECTRUM = "taffymeters.spectrum"
FRAME = [100.0, 100.0, 900.0, 500.0]  # 800x400 outer; the runner also sets it natively.


def spectrum(channel="mono_average", tilt=4.5, window=2048, hop=512):
    """Default Spectrum (Curve style, FFT 2048/512 unless given, stateful Peak) with a channel and Tilt."""
    return {"type_id": SPECTRUM, "data": {"nodes": [
        {"input": None, "kind": "channel", "params": channel},
        {"input": 0, "kind": "fft", "params": {"window_size": window, "hop_size": hop, "floor_dbfs": -160.0}},
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
    # Oldest-sample latency experiment (7-1 task 8): only an FFT result landing on a
    # capture block's end covers every arrived frame. With 480-frame blocks that needs
    # hop | 480 and window % hop == 0; 2048/480 never lands there.
    "single-w2048h480": {
        "session": session(window(leaf(spectrum(window=2048, hop=480)))),
        "baseline": None, "duration": 10.0, "segments": [(0.0, "visible", None)],
        "plots": 1, "windows": 1,
    },
    "single-w1920h480": {
        "session": session(window(leaf(spectrum(window=1920, hop=480)))),
        "baseline": None, "duration": 10.0, "segments": [(0.0, "visible", None)],
        "plots": 1, "windows": 1,
    },
    "single-w2048h32": {
        "session": session(window(leaf(spectrum(window=2048, hop=32)))),
        "baseline": None, "duration": 10.0, "segments": [(0.0, "visible", None)],
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
    # 7-1 task 3: the settings window closed, open (Module tab) and minimized; the hover
    # segments move the cursor over the Spectrum, as Workspace input also rebuilds settings.
    "settings": {
        "session": session(window(leaf(spectrum()))),
        "baseline": None, "duration": 25.0,
        "segments": [(0.0, "visible", None), (5.0, "hover", "hover"),
                     (10.0, "settings", "open-settings"), (15.0, "settings-hover", "hover"),
                     (20.0, "settings-min", "minimize-settings")],
        "plots": 1, "windows": 1,
    },
    # Paused (Space): no data wakes anything; while the source is not running the open
    # settings window is rebuilt every 250 ms to show its status.
    "settings-paused": {
        "session": session(window(leaf(spectrum()))),
        "baseline": None, "duration": 15.0,
        "segments": [(0.0, "visible", None), (5.0, "paused", "pause"),
                     (10.0, "paused-settings", "open-settings")],
        "plots": 1, "windows": 1,
    },
    # 7-1 task 6: the stimulus stops at 5 s, so loopback delivers nothing; the worker
    # fills silence until the analysis is at rest, then the time views scroll empty.
    "settle-spectrum": {
        "session": session(window(leaf(spectrum()))),
        "baseline": None, "duration": 25.0,
        "segments": [(0.0, "visible", None), (5.0, "silence", "stimulus-stop")],
        "plots": 1, "windows": 1,
    },
    "settle-meter": {
        "session": session(window(split("left_right", leaf(module("level_meter")),
                                        leaf(module("loudness_meter"))))),
        "baseline": None, "duration": 25.0,
        "segments": [(0.0, "visible", None), (5.0, "silence", "stimulus-stop")],
        "plots": 2, "windows": 1,
    },
    "settle-pcm": {
        "session": session(window(leaf(module("oscilloscope")))),
        "baseline": None, "duration": 25.0,
        "segments": [(0.0, "visible", None), (5.0, "silence", "stimulus-stop")],
        "plots": 1, "windows": 1,
    },
    # 7-1 task 5: one scrolling image filling the window; the fast variant shows the
    # shortest time, so new columns arrive at the highest rate.
    "quality-wave": {
        "session": session(window(leaf(module("waveform")))),
        "baseline": None, "duration": 10.0, "segments": [(0.0, "visible", None)],
        "plots": 1, "windows": 1,
    },
    "quality-wave-fast": {
        "session": session(window(leaf(module(
            "waveform", view={"visible_seconds": {"number": 0.25}})))),
        "baseline": None, "duration": 10.0, "segments": [(0.0, "visible", None)],
        "plots": 1, "windows": 1,
    },
    "quality-spec": {
        "session": session(window(leaf(module("spectrogram")))),
        "baseline": None, "duration": 10.0, "segments": [(0.0, "visible", None)],
        "plots": 1, "windows": 1,
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

# Every module type, including the time views that scroll empty after settling.
SCENARIOS["settle-mixed"] = {
    **SCENARIOS["ext-mixed"], "duration": 25.0,
    "segments": [(0.0, "visible", None), (5.0, "silence", "stimulus-stop")],
}


def bar_bottom():
    """The builtin "Bar - Bottom" preset as the user runs it: every module in one full-width
    strip, the two Waveforms keeping 0.5 s, Level and Spectrum showing their Peaks."""
    pcm = lambda seconds: {"nodes": [], "outputs": [
        {"keep": "latest" if seconds is None else {"seconds": seconds}, "node": None}]}
    waveform = lambda channel: module(
        "waveform", data=pcm(0.5), view={
            "channel": {"channel": channel}, "flow": {"choice": "left"},
            "scale": {"number": 1.0}, "visible_seconds": {"number": 0.5}})
    spectrogram = module("spectrogram", data={"nodes": spectrum()["data"]["nodes"][:3],
                                              "outputs": [{"keep": {"seconds": 8.0}, "node": 2}]},
                         view={"flow": {"choice": "up"}, "gain_db": {"number": 0.0},
                               "visible_seconds": {"number": 6.0}})
    level = module("level_meter", data={"nodes": [
        {"input": None, "kind": "levels"},
        {"input": 0, "kind": "peak", "params": {"fall_db_per_second_squared": 30.0, "hold_ms": 2000.0}}],
        "outputs": [{"keep": "latest", "node": 0}, {"keep": "latest", "node": 1}]},
        view={"show_peak": {"bool": True}})
    loudness = module("loudness_meter", data={"nodes": [{"input": None, "kind": "loudness"}],
                                              "outputs": [{"keep": "latest", "node": 0}]})
    curve = {**spectrum(), "style": "taffymeters.spectrum.line", "view": {
        "bottom_db": {"number": -90.0}, "max_frequency_hz": {"number": 20000.0},
        "range_db": {"number": 90.0}, "show_peak": {"bool": True}, "show_readout": {"bool": False}}}
    return split("left_right", leaf(spectrogram), split(
        "left_right", leaf(module("oscilloscope", data=pcm(None), style="taffymeters.oscilloscope.line",
                                  view={"channel": {"channel": "mono_average"}, "scale": {"number": 1.0},
                                        "show_guide": {"bool": False}})),
        split("left_right", split("top_bottom", leaf(waveform("left")), leaf(waveform("right"))),
              split("left_right", leaf(level), split(
                  "left_right", leaf(loudness), split(
                      "left_right", leaf(module("stereometer", data=pcm(None), view={
                          "scale": {"number": 1.0}, "show_guides": {"bool": False}})),
                      leaf(curve), 0.19444443), 0.02702703), 0.05128205), 0.33898306), 0.24358974),
        0.13333333)


BAR_FRAME = [0.0, 800.0, 1440.0, 912.0]
# 7-1 task 10: daily use with music (assets/README.md), restarted per run. daily-bar is the
# user's strip; daily-heavy adds a large 2x2 window with Peaks, then moves the pointer and
# opens settings, so it exceeds ordinary use. Both keep their session frames.
SCENARIOS["daily-bar"] = {
    "session": session(window(bar_bottom(), BAR_FRAME)),
    "baseline": None, "duration": 20.0, "segments": [(0.0, "visible", None)],
    "plots": 8, "windows": 1, "stimulus": "music", "size": None,
}
SCENARIOS["daily-heavy"] = {
    "session": session(
        window(quad({**spectrum(), "style": "taffymeters.spectrum.bars", "view": {"show_peak": {"bool": True}}},
                    module("spectrogram"), module("waveform"),
                    {**spectrum("left"), "style": "taffymeters.spectrum.pixel",
                     "view": {"show_peak": {"bool": True}}}),
               [100.0, 100.0, 1100.0, 660.0]),
        window(bar_bottom(), BAR_FRAME)),
    "baseline": None, "duration": 30.0,
    "segments": [(0.0, "visible", None), (15.0, "hover", "hover"),
                 (20.0, "settings", "open-settings"), (25.0, "settings-hover", "hover")],
    "plots": 12, "windows": 2, "stimulus": "music", "size": None,
}

LOCAL = {"format_version": 1, "input": {"kind": "system_default"}}
