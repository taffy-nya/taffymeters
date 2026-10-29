"""Paired desktop performance runs for the product and the rebuilt baseline.

    python tools/perf/run.py --output target/phase7/runs/<name> \
        --build new=new:<perf-trace exe> --build new-off=new-off:<product exe> \
        --build old=old:<rebuilt probe exe> [--build old-off=old-off:<same exe>] \
        --scenarios single,shared,different,lifecycle --rounds 5 [--warmup 3] [--duration 10]

Plays the protocol's looping -36 dBFS 440/880 Hz WAV on the default playback device
for the whole session, so loopback receives it; never changes the volume. Owns the
desktop: windows are moved, resized, minimized and closed through Win32. Builds of
kind `new` get `--perf-trace`; `new-off` is the plain product; `old`/`old-off` are the
phase-0 probe with PHASE0_TRACE on/off. Each round runs every scenario once per build,
alternating the build order between rounds. Raw files only; see summarize.py.
"""
import argparse
import array
import ctypes as c
from ctypes import wintypes as w
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import time
import wave
import winsound

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from native import win32 as win  # noqa: E402  (sets per-monitor DPI awareness)
from perf.scenarios import LOCAL, SCENARIOS  # noqa: E402

from PIL import ImageGrab  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
k = c.WinDLL("kernel32", use_last_error=True)
psapi = c.WinDLL("psapi", use_last_error=True)
win.bind(k, "OpenProcess", w.HANDLE, w.DWORD, w.BOOL, w.DWORD)
win.bind(k, "CloseHandle", w.BOOL, w.HANDLE)
win.bind(k, "GetProcessTimes", w.BOOL, w.HANDLE, *[c.POINTER(w.FILETIME)] * 4)
win.bind(k, "GetSystemTimes", w.BOOL, *[c.POINTER(w.FILETIME)] * 3)
win.bind(k, "QueryPerformanceCounter", w.BOOL, c.POINTER(c.c_int64))
win.bind(k, "QueryPerformanceFrequency", w.BOOL, c.POINTER(c.c_int64))


class Memory(c.Structure):
    _fields_ = [("cb", w.DWORD), ("PageFaultCount", w.DWORD)] + [
        (name, c.c_size_t) for name in (
            "PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage", "QuotaPagedPoolUsage",
            "QuotaPeakNonPagedPoolUsage", "QuotaNonPagedPoolUsage", "PagefileUsage",
            "PeakPagefileUsage", "PrivateUsage")]


class Power(c.Structure):
    _fields_ = [("ac", c.c_ubyte), ("battery_flag", c.c_ubyte), ("battery_percent", c.c_ubyte),
                ("system_status", c.c_ubyte), ("life", w.DWORD), ("full_life", w.DWORD)]


win.bind(psapi, "GetProcessMemoryInfo", w.BOOL, w.HANDLE, c.POINTER(Memory), w.DWORD)
win.bind(k, "GetSystemPowerStatus", w.BOOL, c.POINTER(Power))

SW_HIDE, SW_SHOWNOACTIVATE, SW_MINIMIZE, SW_RESTORE = 0, 4, 6, 9
SWP_NOZORDER_NOACTIVATE = 0x0014


def qpc():
    value = c.c_int64()
    k.QueryPerformanceCounter(c.byref(value))
    return value.value


def qpc_frequency():
    value = c.c_int64()
    k.QueryPerformanceFrequency(c.byref(value))
    return value.value


def filetime(value):
    return (value.dwHighDateTime << 32) | value.dwLowDateTime


def sample(handle):
    memory = Memory()
    memory.cb = c.sizeof(memory)
    times = [w.FILETIME() for _ in range(4)]
    if not psapi.GetProcessMemoryInfo(handle, c.byref(memory), memory.cb):
        raise c.WinError(c.get_last_error())
    if not k.GetProcessTimes(handle, *(c.byref(t) for t in times)):
        raise c.WinError(c.get_last_error())
    return {"qpc": qpc(), "cpu_100ns": filetime(times[2]) + filetime(times[3]),
            "working_set_bytes": memory.WorkingSetSize, "private_bytes": memory.PrivateUsage}


def system_busy(seconds):
    """Whole-machine busy percent over `seconds` (GetSystemTimes)."""
    def read():
        idle, kernel, user = w.FILETIME(), w.FILETIME(), w.FILETIME()
        k.GetSystemTimes(c.byref(idle), c.byref(kernel), c.byref(user))
        return filetime(idle), filetime(kernel) + filetime(user)
    idle0, total0 = read()
    time.sleep(seconds)
    idle1, total1 = read()
    total = total1 - total0
    return 100.0 * (total - (idle1 - idle0)) / total if total else None


def power():
    status = Power()
    k.GetSystemPowerStatus(c.byref(status))
    return {"ac_line_status": status.ac, "battery_flag": status.battery_flag,
            "battery_percent": status.battery_percent}


VOLUME_PS = r"""
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices;
[Guid("5CDF2C82-841E-4546-9722-0CF74078229A"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IAudioEndpointVolume { int f(); int g(); int h(); int i(); int SetMasterVolumeLevelScalar(float a, Guid b);
  int j(); int GetMasterVolumeLevelScalar(out float a); int k(); int l(); int m(); int n();
  int SetMute(bool a, Guid b); int GetMute(out bool a); }
[Guid("D666063F-1587-4E43-81F1-B948E807363F"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IMMDevice { int Activate(ref Guid id, int ctx, IntPtr p, [MarshalAs(UnmanagedType.IUnknown)] out object o);
  int OpenPropertyStore(int a, out IntPtr b); int GetId([MarshalAs(UnmanagedType.LPWStr)] out string id); }
[Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IMMDeviceEnumerator { int f(); int GetDefaultAudioEndpoint(int flow, int role, out IMMDevice d); }
[ComImport, Guid("BCDE0395-E52F-467C-8E3D-C4579291692E")] class MMDeviceEnumerator {}
public static class Vol { public static string Get() {
  var e = (IMMDeviceEnumerator)(new MMDeviceEnumerator()); IMMDevice d; e.GetDefaultAudioEndpoint(0, 1, out d);
  string id; d.GetId(out id); var g = typeof(IAudioEndpointVolume).GUID; object o; d.Activate(ref g, 23, IntPtr.Zero, out o);
  var v = (IAudioEndpointVolume)o; float s; v.GetMasterVolumeLevelScalar(out s); bool m; v.GetMute(out m);
  return id + "|" + s.ToString(System.Globalization.CultureInfo.InvariantCulture) + "|" + m; } }
'@
[Vol]::Get()
"""


def volume():
    """Default render endpoint id, master volume scalar and mute; read only."""
    try:
        out = subprocess.run(["powershell", "-NoProfile", "-Command", VOLUME_PS], capture_output=True,
                             text=True, timeout=60).stdout.strip().splitlines()[-1]
        endpoint, scalar, mute = out.split("|")
        return {"endpoint_id": endpoint, "master_scalar": float(scalar), "muted": mute == "True"}
    except Exception as error:  # recorded, never fatal: the run still states it
        return {"error": repr(error)}


TOP_PS = r"""$a = Get-Process | Select Id,ProcessName,CPU; Start-Sleep 5; $b = Get-Process | Select Id,ProcessName,CPU
$b | ForEach-Object { $p = $_; $o = $a | Where-Object Id -eq $p.Id
  if ($o -and $p.CPU) { [pscustomobject]@{name=$p.ProcessName; pid=$p.Id; percent_one_core=[math]::Round(($p.CPU-$o.CPU)/5*100,1)} } } |
  Sort percent_one_core -Descending | Select -First 10 | ConvertTo-Json -Compress"""


def top_processes():
    """Busiest processes over 5 s, percent of one core; background load is reported, not removed."""
    out = subprocess.run(["powershell", "-NoProfile", "-Command", TOP_PS], capture_output=True,
                         text=True, timeout=60).stdout
    return json.loads(out) if out.strip() else None


def cim(query):
    command = f"Get-CimInstance {query} | ConvertTo-Json -Compress"
    out = subprocess.run(["powershell", "-NoProfile", "-Command", command], capture_output=True,
                         text=True, timeout=60).stdout
    return json.loads(out) if out.strip() else None


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def git(*args):
    return subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True).stdout.strip()


def stimulus(path):
    """Protocol 3.1: 1 s stereo PCM16, Left 440 Hz, Right 880 Hz, -36 dBFS peak."""
    pcm = array.array("h")
    for index in range(48000):
        pcm.extend(round(32767 * 10 ** (-36 / 20) * math.sin(2 * math.pi * freq * index / 48000))
                   for freq in (440, 880))
    with wave.open(str(path), "wb") as output:
        output.setparams((2, 2, 48000, 48000, "NONE", "not compressed"))
        output.writeframes(pcm.tobytes())


def app_windows(pid, kind):
    """Workspace windows in id order (product) or the single probe window (baseline)."""
    found = win.windows(pid)
    if kind.startswith("new"):
        rows = [(int(info["title"].split("#")[1]), hwnd) for hwnd, info in found.items()
                if info["title"].startswith("Taffymeters #")]
        return [hwnd for _, hwnd in sorted(rows)]
    return [hwnd for hwnd, info in found.items() if "taffymeters phase0" in info["title"].lower()]


def window_state(hwnd):
    return {"rect": win.rect(hwnd) if win.u.IsWindow(hwnd) else None,
            "visible": bool(win.u.IsWindowVisible(hwnd)), "iconic": bool(win.u.IsIconic(hwnd))}


def set_logical_size(hwnd, width, height):
    scale = win.scale(hwnd)
    win.u.SetWindowPos(hwnd, None, 100, 100, round(width * scale), round(height * scale),
                       SWP_NOZORDER_NOACTIVATE)


def screenshot(hwnd, path):
    started = time.perf_counter()
    try:
        ImageGrab.grab(bbox=tuple(win.rect(hwnd)), all_screens=True).save(path)
        return {"path": path.name, "seconds": time.perf_counter() - started}
    except Exception as error:
        return {"error": repr(error), "seconds": time.perf_counter() - started}


def run_one(args, build, scenario_name, repetition, out):
    name, kind, exe = build
    scenario = SCENARIOS[scenario_name]
    run_dir = out / f"{scenario_name}_{name}_{repetition}"
    run_dir.mkdir()
    duration = scenario["duration"]
    if duration == 10.0 and args.duration is not None:
        duration = args.duration
    env = dict(os.environ)
    argv = [str(exe)]
    if kind.startswith("new"):
        config = run_dir / "config"
        config.mkdir()
        (config / "session.json").write_text(json.dumps(scenario["session"], indent=2), encoding="utf-8")
        (config / "local.json").write_text(json.dumps(LOCAL, indent=2), encoding="utf-8")
        argv += ["--config-dir", str(config)]
        if kind == "new":
            argv += ["--perf-trace", str(run_dir / "perf")]
    else:
        if scenario["baseline"] is None:
            raise SystemExit(f"{scenario_name} has no baseline equivalent")
        env.update(PHASE0_SCENARIO=scenario["baseline"], PHASE0_DURATION=str(duration),
                   PHASE0_WARMUP=str(args.warmup), PHASE0_OUTPUT=str(run_dir / "instrument.json"),
                   PHASE0_MAX_SAMPLES="8192", PHASE0_TRACE="off" if kind == "old-off" else "on")
    busy = system_busy(1.0)
    launched_qpc = qpc()
    stdout, stderr = open(run_dir / "stdout.log", "wb"), open(run_dir / "stderr.log", "wb")
    proc = subprocess.Popen(argv, env=env, cwd=Path(exe).parent, stdout=stdout, stderr=stderr)
    result = {"schema_version": 1, "scenario": scenario_name, "build": name, "kind": kind,
              "repetition": repetition, "argv": argv, "pre_launch_machine_busy_percent": busy,
              "launched_qpc": launched_qpc, "qpc_frequency": qpc_frequency(), "events": [],
              "samples": [], "screenshots": [], "warmup_seconds": args.warmup,
              "requested_seconds": duration, "valid": False, "invalid_reason": None}
    handle = None
    try:
        deadline = time.perf_counter() + 30
        hwnds = []
        while time.perf_counter() < deadline:
            if proc.poll() is not None:
                raise RuntimeError(f"exited before readiness with {proc.returncode}")
            hwnds = app_windows(proc.pid, kind)
            if len(hwnds) >= scenario["windows"] and all(win.u.IsWindowVisible(h) for h in hwnds):
                break
            time.sleep(0.05)
        else:
            raise RuntimeError("windows did not become ready within 30 s")
        result["ready_qpc"] = qpc()
        primary = hwnds[0]
        result["dpi"] = win.u.GetDpiForWindow(primary)
        set_logical_size(primary, 800, 400)
        win.u.SetCursorPos(20, 20)
        result["initial_windows"] = [window_state(h) for h in hwnds]
        time.sleep(args.warmup)
        handle = k.OpenProcess(0x0400 | 0x0010, False, proc.pid)
        if not handle:
            raise c.WinError(c.get_last_error())
        start = time.perf_counter()
        result["measurement_start_qpc"] = qpc()
        segments = list(scenario["segments"])
        current, last_resize, resizes = None, -1, 0
        while proc.poll() is None:
            elapsed = time.perf_counter() - start
            if elapsed >= duration:
                break
            while segments and elapsed >= segments[0][0]:
                _, current, action = segments.pop(0)
                before = qpc()
                if action == "minimize":
                    win.u.ShowWindow(primary, SW_MINIMIZE)
                elif action == "restore":
                    win.u.ShowWindow(primary, SW_RESTORE)
                    set_logical_size(primary, 800, 400)
                elif action == "hide":
                    win.u.ShowWindow(primary, SW_HIDE)
                elif action == "show":
                    win.u.ShowWindow(primary, SW_SHOWNOACTIVATE)
                    set_logical_size(primary, 800, 400)
                elif action == "close":
                    win.close(primary)
                result["events"].append({"qpc": before, "done_qpc": qpc(), "elapsed": elapsed,
                                         "segment": current, "action": action,
                                         "window": window_state(primary)})
                if current in ("visible", "restored"):
                    if current == "restored":
                        time.sleep(0.3)  # let the compositor present the restored frame
                    result["screenshots"].append(
                        screenshot(primary, run_dir / f"{current}.png") | {"segment": current})
            step = int(elapsed * 4)
            if current == "resize" and step != last_resize:
                # Alternate by actions issued: a slow loop can skip a quarter-second slot.
                width, height = (1000, 500) if resizes % 2 == 0 else (600, 300)
                resizes += 1
                before = qpc()
                set_logical_size(primary, width, height)
                result["events"].append({"qpc": before, "done_qpc": qpc(), "elapsed": elapsed,
                                         "segment": current, "action": f"resize {width}x{height}",
                                         "window": window_state(primary)})
                last_resize = step
            row = sample(handle)
            row["segment"] = current
            result["samples"].append(row)
            time.sleep(0.2)
        result["measurement_end_qpc"] = qpc()
        # The product only closes when told to; the probe closes on its own clock.
        exited_early = kind.startswith("new") and proc.poll() is not None
        if kind.startswith("new"):
            for hwnd in app_windows(proc.pid, kind):
                win.close(hwnd)
        elif proc.poll() is None:
            for hwnd in app_windows(proc.pid, kind):
                win.u.ShowWindow(hwnd, SW_RESTORE)
                win.close(hwnd)
        proc.wait(timeout=20)
        result["exit_code"] = proc.returncode
        result["valid"] = proc.returncode == 0 and not exited_early
        if proc.returncode != 0:
            result["invalid_reason"] = f"exit code {proc.returncode}"
        elif exited_early:
            result["invalid_reason"] = "exited before the timeline ended"
    except Exception as error:
        result["invalid_reason"] = repr(error)
        if proc.poll() is None:
            for hwnd in app_windows(proc.pid, kind):
                win.u.ShowWindow(hwnd, SW_RESTORE)
                win.close(hwnd)
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=10)
        result["exit_code"] = proc.returncode
    finally:
        if handle:
            k.CloseHandle(handle)
        stdout.close()
        stderr.close()
    result["exited_qpc"] = qpc()
    (run_dir / "process.json").write_text(json.dumps(result, indent=1), encoding="utf-8")
    status = "ok" if result["valid"] else f"INVALID {result['invalid_reason']}"
    print(f"{run_dir.name}: {status}", flush=True)
    return {"dir": run_dir.name, "valid": result["valid"]}


def parse_build(text):
    name, rest = text.split("=", 1)
    kind, path = rest.split(":", 1)
    if kind not in ("new", "new-off", "old", "old-off"):
        raise argparse.ArgumentTypeError(f"unknown build kind {kind}")
    return name, kind, Path(path).resolve()


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--build", type=parse_build, action="append", required=True)
    parser.add_argument("--scenarios", default="single,shared,different,lifecycle")
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument("--warmup", type=float, default=3.0)
    parser.add_argument("--duration", type=float, default=None, help="override the 10 s scenarios")
    args = parser.parse_args()
    # Builds run with their own directory as cwd, so every path passed on must be absolute.
    args.output = args.output.resolve()
    scenarios = args.scenarios.split(",")
    for scenario in scenarios:
        if scenario not in SCENARIOS:
            parser.error(f"unknown scenario {scenario}")
    if args.output.exists() and any(args.output.iterdir()):
        parser.error("output must be new or empty")
    args.output.mkdir(parents=True, exist_ok=True)
    manifest = {
        "schema_version": 1, "argv": sys.argv, "started": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "git": {"head": git("rev-parse", "HEAD"), "status": git("status", "--short")},
        "toolchain": subprocess.run(["rustc", "-vV"], capture_output=True, text=True).stdout,
        "builds": [{"name": n, "kind": kd, "exe": str(p), "sha256": sha256(p)} for n, kd, p in args.build],
        "scenarios": {s: SCENARIOS[s] for s in scenarios},
        "environment": {
            "platform": platform.platform(), "python": sys.version,
            "logical_processors": os.cpu_count(), "qpc_frequency": qpc_frequency(),
            "cpu": cim("Win32_Processor | Select Name,NumberOfCores,NumberOfLogicalProcessors,MaxClockSpeed"),
            "gpu": cim("Win32_VideoController | Select Name,DriverVersion,CurrentHorizontalResolution,"
                       "CurrentVerticalResolution,CurrentRefreshRate"),
            "monitors": win.monitors(), "power_before": power(), "volume_before": volume(),
            "machine_busy_percent_before": system_busy(5.0), "top_processes_before": top_processes(),
        },
        "stimulus": {"sample_rate": 48000, "channels_hz": [440, 880], "peak_dbfs": -36,
                     "route": "winsound default render endpoint -> WASAPI loopback"},
    }
    wav = args.output / "stimulus.wav"
    stimulus(wav)
    runs = []
    try:
        winsound.PlaySound(str(wav), winsound.SND_FILENAME | winsound.SND_ASYNC | winsound.SND_LOOP)
        time.sleep(1.0)
        for repetition in range(1, args.rounds + 1):
            order = args.build if repetition % 2 else list(reversed(args.build))
            for scenario in scenarios:
                for build in order:
                    if build[1].startswith("old") and SCENARIOS[scenario]["baseline"] is None:
                        continue
                    runs.append(run_one(args, build, scenario, repetition, args.output))
    finally:
        winsound.PlaySound(None, 0)
        manifest.update(runs=runs, finished=time.strftime("%Y-%m-%dT%H:%M:%S%z"),
                        power_after=power(), volume_after=volume(), top_processes_after=top_processes())
        (args.output / "manifest.json").write_text(json.dumps(manifest, indent=1), encoding="utf-8")
    shutil.copy2(Path(__file__).with_name("scenarios.py"), args.output / "scenarios.py")


if __name__ == "__main__":
    main()
