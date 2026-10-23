"""Run headless automation and opt-in desktop regression checks.

Usage: python tools/check.py [--headless | --native] [--only snap,lock]
                            [--audio] [--ci] [--out EMPTY_DIRECTORY]

No selector runs headless then native, even when headless fails. Native checks
own mouse/keyboard after a cancellable five-second warning. Audio playback is
never requested unless --audio is present. Output directories are never reused.
Exit codes: 0 all pass, 1 failure/cancellation, 2 arguments, 3 hardware skips.
"""
import argparse
from datetime import datetime
import hashlib
import importlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import time
import traceback

sys.dont_write_bytecode = True

from native.checks import GROUPS

ROOT = Path(__file__).resolve().parent.parent


def arguments():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--headless", action="store_true", help="only cargo automation tests")
    mode.add_argument("--native", action="store_true", help="only Windows native smoke checks")
    parser.add_argument("--only", help="comma-separated native groups: " + ",".join(GROUPS))
    parser.add_argument("--audio", action="store_true", help="include audible system-loopback check")
    parser.add_argument("--ci", action="store_true", help="run CI fmt and both clippy commands before tests")
    parser.add_argument("--out", type=Path, help="nonexistent or empty evidence directory")
    args = parser.parse_args()
    if args.only is not None:
        selected = args.only.split(",")
        if any(name not in GROUPS for name in selected) or len(selected) != len(set(selected)):
            parser.error("--only requires distinct known group names")
        if args.headless:
            parser.error("--only requires native checks")
        if "audio" in selected and not args.audio:
            parser.error("the audio group requires --audio")
    else:
        selected = [group for group in GROUPS if group != "audio" or args.audio]
    if args.headless and args.audio:
        parser.error("--audio requires native checks")
    args.groups = selected
    args.out = (args.out if args.out is not None else ROOT / "target" / "checks" /
                datetime.now().strftime("%Y%m%d-%H%M%S-%f")).resolve()
    if args.out.exists() and (not args.out.is_dir() or any(args.out.iterdir())):
        parser.error("--out must be a nonexistent or empty directory")
    try:
        args.out.mkdir(parents=True, exist_ok=True)
    except OSError as exc:
        parser.error(str(exc))
    return args


def command(args, log_path, timeout=1800):
    with log_path.open("w", encoding="utf-8") as log:
        log.write("$ " + subprocess.list2cmdline(args) + "\n")
        log.flush()
        try:
            process = subprocess.Popen(args, cwd=ROOT, stdout=log, stderr=subprocess.STDOUT)
            try:
                code = process.wait(timeout=timeout)
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait(timeout=10)
        except (OSError, subprocess.TimeoutExpired) as exc:
            log.write(str(exc) + "\n")
            return {"status": "FAIL", "reason": str(exc), "command": args, "log": log_path.name}
    return {"status": "PASS" if code == 0 else "FAIL", "exit_code": code,
            "command": args, "log": log_path.name,
            "reason": "" if code == 0 else f"command exited {code}"}


def run_headless(out):
    result = command(["cargo", "test", "--locked", "--features", "automation"], out / "headless.log")
    text = (out / "headless.log").read_text(encoding="utf-8", errors="replace")
    names = ("passed", "failed", "ignored", "measured", "filtered_out")
    counts = re.findall(r"test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out", text)
    result["test_counts"] = {name: sum(int(row[i]) for row in counts) for i, name in enumerate(names)}
    result["test_binaries"] = len(counts)
    if result["status"] == "PASS" and not counts:
        result.update(status="FAIL", reason="cargo succeeded without recognizable test summaries")
    return result


def executable(out):
    args = ["cargo", "build", "--locked", "--features", "automation", "--message-format=json"]
    result = command(args, out / "native-build.log")
    if result["status"] == "FAIL":
        return result, None
    candidates = []
    for line in (out / "native-build.log").read_text(encoding="utf-8", errors="replace").splitlines():
        try:
            row = json.loads(line)
        except json.JSONDecodeError:
            continue
        if row.get("reason") == "compiler-artifact" and row.get("target", {}).get("name") == "taffymeters" \
                and "bin" in row.get("target", {}).get("kind", []) and row.get("executable"):
            candidates.append(Path(row["executable"]))
    if not candidates or not candidates[-1].is_file():
        result.update(status="FAIL", reason="cargo supplied no usable taffymeters compiler-artifact executable")
        return result, None
    exe = candidates[-1].resolve()
    digest = hashlib.sha256()
    with exe.open("rb") as file:
        for block in iter(lambda: file.read(1024 * 1024), b""):
            digest.update(block)
    return result, {"path": str(exe), "sha256": digest.hexdigest()}


def line(name, result):
    reason = result.get("reason")
    counts = result.get("test_counts")
    detail = f" ({counts['passed']} passed, {counts['failed']} failed, {counts['ignored']} ignored)" if counts else ""
    print(f"{name}: {result['status']}{detail}" + (f" — {reason}" if reason else ""), flush=True)


def native_groups(args, report):
    if sys.platform != "win32":
        report["native"] = [{"name": group, "status": "SKIP", "reason": "Windows desktop required"}
                            for group in args.groups]
        for row in report["native"]:
            line(row["name"], row)
        return
    from native import win32 as win
    from native.app import Context, Skip
    report["environment"].update(win.environment())
    build, exe = executable(args.out)
    report["native_build"], report["exe"] = build, exe
    line("native-build", build)
    if exe is None:
        report["native"] = [{"name": group, "status": "FAIL", "reason": "native build failed"} for group in args.groups]
        for row in report["native"]:
            line(row["name"], row)
        return
    print("WARNING: native checks will take over the mouse and keyboard. Ctrl+C cancels.", flush=True)
    for remaining in range(5, 0, -1):
        print(f"Starting in {remaining} s", flush=True)
        time.sleep(1)
    for group in args.groups:
        context = Context(ROOT, Path(exe["path"]), args.out / group, report["environment"])
        started = time.monotonic()
        row = {"name": group, "status": "FAIL", "reason": "Interrupted before completion"}
        try:
            importlib.import_module("native.checks." + group).run(context)
            row.update(status="PASS", reason="")
        except Skip as exc:
            row.update(status="SKIP", reason=str(exc))
        except Exception as exc:
            row.update(status="FAIL", reason=str(exc))
            (context.directory / "failure.txt").write_text(traceback.format_exc(), encoding="utf-8")
            snapshots = []
            for app in context.apps:
                try:
                    rates = app.records("rate")
                    snapshots.append({"pid": app.process.pid, "exit_code": app.process.poll(),
                                      "frames_px": {str(n): win.rect(hwnd) for n, hwnd in app.windows().items()},
                                      "state": app.state(), "last_rate": rates[-1] if rates else None})
                except Exception as snapshot_error:
                    snapshots.append({"pid": app.process.pid, "error": str(snapshot_error)})
            context.observed["failure_snapshot"] = snapshots
            try:
                from PIL import ImageGrab
                ImageGrab.grab(all_screens=True).save(context.directory / "failure.png")
                row["screenshot"] = f"{group}/failure.png"
            except Exception:
                pass
        finally:
            try:
                context.cleanup()
            except Exception as exc:
                row.update(status="FAIL", reason=f"Process cleanup failed: {exc}")
            row.update(checks=context.checks, observed=context.observed,
                       duration_s=round(time.monotonic() - started, 3))
            report["native"].append(row)
        line(group, row)
        (args.out / "result.json").write_text(json.dumps(report, indent=2, ensure_ascii=False), encoding="utf-8")


def main():
    args = arguments()
    report = {"schema_version": 1, "started": datetime.now().astimezone().isoformat(),
              "environment": {"os": platform.platform(), "python": sys.version,
                              "cargo_target_dir": os.environ.get("CARGO_TARGET_DIR")},
              "requested": {"headless": not args.native, "native": not args.headless,
                            "groups": args.groups if not args.headless else [], "audio": args.audio, "ci": args.ci},
              "ci": [], "headless": None, "native": [], "exe": None}
    try:
        commit = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True, timeout=15)
        report["git_commit"] = commit.stdout.strip() if commit.returncode == 0 else None
        if commit.returncode:
            report["git_commit_error"] = commit.stderr.strip()
        status = subprocess.run(["git", "status", "--porcelain"], cwd=ROOT, capture_output=True, text=True, timeout=15)
        report["git_dirty"] = bool(status.stdout.strip()) if status.returncode == 0 else None
        if args.ci:
            commands = [("fmt", ["cargo", "fmt", "--all", "--check"]),
                        ("clippy", ["cargo", "clippy", "--locked", "--all-targets", "--", "-D", "warnings"]),
                        ("clippy-automation", ["cargo", "clippy", "--locked", "--all-targets", "--features",
                                               "automation", "--", "-D", "warnings"])]
            for name, cmd in commands:
                row = command(cmd, args.out / f"{name}.log")
                row["name"] = name
                report["ci"].append(row)
                line(name, row)
        if not args.native:
            report["headless"] = run_headless(args.out)
            line("headless", report["headless"])
        if not args.headless:
            native_groups(args, report)
    except KeyboardInterrupt:
        report["interrupted"] = {"status": "FAIL", "reason": "Cancelled by Ctrl+C"}
        line("cancelled", report["interrupted"])
    except Exception as exc:
        report["runner"] = {"status": "FAIL", "reason": str(exc)}
        (args.out / "runner-failure.txt").write_text(traceback.format_exc(), encoding="utf-8")
        line("runner", report["runner"])
    results = [*report["ci"], *report["native"]]
    results += [report[key] for key in ("headless", "native_build", "runner", "interrupted") if report.get(key)]
    status = "FAIL" if any(row["status"] == "FAIL" for row in results) else (
        "SKIP" if any(row["status"] == "SKIP" for row in results) else "PASS")
    code = {"PASS": 0, "FAIL": 1, "SKIP": 3}[status]
    report.update(status=status, exit_code=code, finished=datetime.now().astimezone().isoformat())
    (args.out / "result.json").write_text(json.dumps(report, indent=2, ensure_ascii=False), encoding="utf-8")
    print(f"Result: {status} — {args.out}", flush=True)
    return code


if __name__ == "__main__":
    sys.exit(main())
