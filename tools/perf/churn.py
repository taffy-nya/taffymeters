"""Repeated create/destroy in a real native process; checks the return to baseline.

    python tools/perf/churn.py --exe <automation build> --output <new dir> [--cycles 20] [--input synthetic]

Drives the settings window with real OS input (tools/native): each cycle splits the
Spectrum (a new instance with its own subscription and private Peak), closes one of
the two, then opens a new empty window and closes it. Records the trace's live
subscription/node counts and the process's private bytes after every cycle. Owns
the mouse and keyboard.
"""
import argparse
import json
from pathlib import Path
import statistics
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from native import win32 as w  # noqa: E402
from native.app import App, Context, leaf, session  # noqa: E402
from perf.run import k, sample  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]


def settle(app, predicate, description, timeout=12):
    return w.wait(lambda: (lambda s: s if s and predicate(s) else None)(app.state()), description, timeout)


def counts(app):
    state = app.state()
    return {"modules": len(state["modules"]), "windows": len(state["windows"]),
            "subscriptions": state["subscriptions"], "nodes": state["nodes"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--exe", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cycles", type=int, default=20)
    parser.add_argument("--input", default="synthetic")
    args = parser.parse_args()
    args.output = args.output.resolve()
    if args.output.exists():
        parser.error("output must not exist")
    args.output.mkdir(parents=True)
    environment = w.environment()
    context = Context(ROOT, args.exe.resolve(), args.output / "run", environment)
    rows, handle, failure = [], None, None
    try:
        fixture = session(environment, [{"frame": [100, 100, 500, 320], "layout": leaf("spectrum")}])
        app = App(context, fixture=fixture, input_kind=args.input)
        handle = k.OpenProcess(0x0400 | 0x0010, False, app.process.pid)
        time.sleep(3.0)
        rows.append({"cycle": 0, **counts(app), **sample(handle)})
        for cycle in range(1, args.cycles + 1):
            app.open_settings(1)
            app.click("Tab(Module)")
            app.click("SplitRight")
            settle(app, lambda s: len(s["modules"]) == 2, "split created a module")
            peak = counts(app)
            app.click("Tab(Module)")
            app.click("CloseModule")
            settle(app, lambda s: len(s["modules"]) == 1, "module closed")
            app.click("Tab(Window)")
            app.click("NewWindow")
            w.wait(lambda: len(app.windows()) == 2, "new window")
            w.close(app.windows()[max(app.windows())])
            w.wait(lambda: len(app.windows()) == 1, "new window closed")
            settings = app.settings_window()
            if settings:
                w.close(settings)
                w.wait(lambda: not app.settings_window(), "settings closed")
            time.sleep(0.5)
            rows.append({"cycle": cycle, "with_copy": peak, **counts(app), **sample(handle)})
            print(json.dumps({k_: rows[-1][k_] for k_ in ("cycle", "modules", "subscriptions", "nodes",
                                                          "private_bytes")}), flush=True)
        time.sleep(3.0)
        rows.append({"cycle": "settled", **counts(app), **sample(handle)})
    except Exception as error:
        failure = repr(error)
        raise
    finally:
        if handle:
            k.CloseHandle(handle)
        context.cleanup()
        write_report(args, rows, failure)


def write_report(args, rows, failure):
    """Always written, so a failed run keeps the cycles it completed."""
    keys = ("modules", "windows", "subscriptions", "nodes")
    result = {"schema_version": 1, "exe": str(args.exe), "input": args.input, "cycles": args.cycles,
              "completed": failure is None and len(rows) == args.cycles + 2, "failure": failure, "rows": rows}
    if rows:
        baseline, final = rows[0], rows[-1]
        private = [row["private_bytes"] / 2**20 for row in rows]
        result.update(
            baseline={key: baseline[key] for key in keys},
            final={key: final[key] for key in keys},
            objects_return_to_baseline=all(final[key] == baseline[key] for key in keys),
            private_mib={"baseline": private[0], "final": private[-1], "delta": private[-1] - private[0],
                         "max": max(private), "median": statistics.median(private)})
    (args.output / "churn.json").write_text(json.dumps(result, indent=1), encoding="utf-8")
    print(json.dumps({key: result.get(key) for key in ("completed", "failure", "baseline", "final",
                                                       "objects_return_to_baseline", "private_mib")},
                     indent=1))


if __name__ == "__main__":
    main()
