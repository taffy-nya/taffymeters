"""A one-second system playback tone activates analysis, then silence reaches idle."""
import math
import struct
import threading
import time
import wave
import winsound
from ..app import App
from .. import win32 as w


def run(c):
    c.require_playback()
    path = c.directory / "tone.wav"
    with wave.open(str(path), "wb") as stream:
        stream.setparams((2, 2, 48000, 0, "NONE", "not compressed"))
        stream.writeframes(b"".join(struct.pack("<hh", round(9000 * math.sin(2 * math.pi * 1000 * n / 48000)),
                                             round(4500 * math.sin(2 * math.pi * 1000 * n / 48000)))
                                    for n in range(48000)))
    app = App(c, input_kind="system")
    def rates():
        # Trace rates are emitted by real UI passes; idle audio schedules none.
        # Request native paint without changing the source or analysis demand.
        w.redraw(app.hwnd)
        return app.records("rate")

    idle_before = w.wait(lambda: next((row for row in rates()[::-1]
                                    if row["body"]["fft_evaluations_per_second"] == 0), None),
                         "quiet baseline before tone", 12)
    c.expect("quiet_baseline", idle_before["body"]["fft_evaluations_per_second"] == 0,
             sample=idle_before)
    baseline = len(app.records("rate"))
    error = []
    def play():
        try:
            winsound.PlaySound(str(path), winsound.SND_FILENAME)
        except Exception as exc:
            error.append(str(exc))
    thread = threading.Thread(target=play, daemon=True)
    thread.start()
    try:
        active = w.wait(lambda: next((row for row in rates()[baseline:]
                                    if row["body"]["fft_evaluations_per_second"] > 0), None),
                        "tone activates FFT analysis", 5)
        thread.join(timeout=4)
        if thread.is_alive() or error:
            raise AssertionError(f"Tone playback did not complete: {error}")
        c.expect("tone_analysis_active", active["body"]["fft_evaluations_per_second"] > 0,
                 sample=active, tone_duration_s=1)
        # Six seconds of compensation plus two complete rate-sample intervals.
        start = time.monotonic()
        baseline = len(app.records("rate"))
        idle = w.wait(lambda: next((row for row in rates()[baseline:]
                                  if row["body"]["fft_evaluations_per_second"] == 0), None),
                      "silence reaches FFT idle", 9)
        c.expect("silence_analysis_idle", idle["body"]["fft_evaluations_per_second"] == 0,
                 sample=idle, elapsed_after_tone_s=time.monotonic() - start, deadline_s=9)
    finally:
        winsound.PlaySound(None, 0)
        thread.join(timeout=3)
