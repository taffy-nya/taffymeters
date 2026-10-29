"""Summarize a run directory from tools/perf/run.py; reads raw files only.

    python tools/perf/summarize.py <run dir> [--new new] [--old old] [--new-off new-off] [--old-off old-off]

Writes <run dir>/summary.json and prints the threshold table. Statistics follow agent
PERFORMANCE_ACCEPTANCE section 4: per-run nearest-rank percentiles, then the median of
the per-run values; verdicts follow phase-7 decisions D6-D9.
"""
import argparse
import json
import math
from pathlib import Path
import statistics

# Frozen thresholds and historical references (PERFORMANCE_ACCEPTANCE section 2 / 3.1).
THRESHOLDS = {
    "single": {"cpu": 13.58171544588416, "private_mib": 123.5953125, "ui_wall_p95_ms": 0.5,
               "ui_interval_p95_ms": 20.350540000000002, "latency_p95_ms": 17.93571},
    "shared": {"cpu": 16.155657169920545, "private_mib": 124.252734375, "ui_wall_p95_ms": 0.6732499999999999,
               "ui_interval_p95_ms": 20.40961, "latency_p95_ms": 18.22897},
    "different": {"cpu": 18.64633439604814, "private_mib": 124.059375, "ui_wall_p95_ms": 0.68,
                  "ui_interval_p95_ms": 20.57769, "latency_p95_ms": 19.102700000000002},
}
HISTORICAL = {
    "single": {"cpu": 11.437923132621963, "private_mib": 112.359375, "ui_wall_p95_ms": 0.2833,
               "ui_interval_p95_ms": 17.5914, "latency_p95_ms": 15.3961},
    "shared": {"cpu": 13.77787015447322, "private_mib": 112.95703125, "ui_wall_p95_ms": 0.5386,
               "ui_interval_p95_ms": 17.6451, "latency_p95_ms": 15.6627},
    "different": {"cpu": 16.04212217822558, "private_mib": 112.78125, "ui_wall_p95_ms": 0.544,
                  "ui_interval_p95_ms": 17.7979, "latency_p95_ms": 16.457},
}
METRICS = ["cpu", "private_mib", "ui_wall_p95_ms", "ui_interval_p95_ms", "latency_p95_ms"]
MATCHED = ("single", "shared")  # D7: paired-matched loads; `different` is a variant
NEAR = 0.05  # D6: within 5% of a threshold requires a paired retest
SPREAD = 0.20  # D6 / protocol 4.3: round-to-round variation above 20%
MIN_ROUNDS = 3  # protocol 2: medians of at least three independent runs


def nearest_rank(values, p):
    if not values:
        return None
    ordered = sorted(values)
    return ordered[max(math.ceil(p * len(ordered)) - 1, 0)]


def distribution(values):
    if not values:
        return {"n": 0}
    return {"n": len(values), "min": min(values), "p50": nearest_rank(values, 0.5),
            "p95": nearest_rank(values, 0.95), "p99": nearest_rank(values, 0.99), "max": max(values),
            "mean": sum(values) / len(values)}


def process_segments(process, logical_processors):
    """Protocol 4.1 per segment: first/last sample CPU, memory median and peak."""
    frequency = process["qpc_frequency"]
    result = {}
    for segment in dict.fromkeys(row["segment"] for row in process["samples"]):
        rows = [row for row in process["samples"] if row["segment"] == segment]
        if len(rows) < 2:
            continue
        wall = (rows[-1]["qpc"] - rows[0]["qpc"]) / frequency
        cpu = 100 * (rows[-1]["cpu_100ns"] - rows[0]["cpu_100ns"]) / 1e7 / wall
        private = [row["private_bytes"] / 2**20 for row in rows]
        working = [row["working_set_bytes"] / 2**20 for row in rows]
        result[segment] = {
            "samples": len(rows), "sampled_seconds": wall, "start_qpc": rows[0]["qpc"],
            "end_qpc": rows[-1]["qpc"], "cpu": cpu, "cpu_machine": cpu / logical_processors,
            "private_mib": statistics.median(private), "private_peak_mib": max(private),
            "working_set_mib": statistics.median(working), "working_set_peak_mib": max(working),
        }
    return result


COUNTER_COVERAGE = 0.8  # counter rates spanning less of a segment are marked partial
UNKNOWN_CPU = 2**64 - 1  # perf.rs writes u64::MAX when the worker's thread CPU is unknown


def counter_rates(perf, start, end):
    """Rates and deltas from the worker's ~4 Hz counter samples inside [start, end]."""
    frequency = perf["ticks_per_second"]
    names = perf["counter_columns"]
    samples = [dict(zip(names, row)) for row in perf["counters"] if start <= row[0] <= end]
    window = (end - start) / frequency
    if len(samples) < 2:
        return {"counter_samples": len(samples), "counter_span_seconds": 0.0, "counter_coverage": 0.0,
                "counters_partial": True}
    first, last = samples[0], samples[-1]
    span = (last["qpc"] - first["qpc"]) / frequency
    delta = lambda key: last[key] - first[key]
    rate = lambda key: delta(key) / span
    cpu_known = first["worker_cpu_ns"] != UNKNOWN_CPU and last["worker_cpu_ns"] != UNKNOWN_CPU
    gauge = lambda key: sorted({sample[key] for sample in samples})
    return {
        "counter_samples": len(samples), "counter_span_seconds": span,
        "counter_coverage": span / window if window else 0.0,
        "counters_partial": span < COUNTER_COVERAGE * window,
        "fft_per_second": rate("fft_evaluations"), "spectrum_evaluations_per_second": rate("spectrum_evaluations"),
        "publications_per_second": rate("publications"), "pcm_frames_per_second": rate("pcm_frames"),
        "level_frames_per_second": rate("level_frames"), "callbacks_per_second": rate("callbacks"),
        "steady_dropped_blocks": delta("dropped_blocks"),
        "steady_callback_allocations": delta("callback_allocations"),
        "steady_worker_allocations": delta("worker_allocations"),
        "worker_allocations_per_second": rate("worker_allocations"),
        "worker_cpu_ms_per_second": delta("worker_cpu_ns") / 1e6 / span if cpu_known else None,
        "subscriptions": gauge("subscriptions"), "active_channel_nodes": gauge("active_channel_nodes"),
        "active_fft_nodes": gauge("active_fft_nodes"), "active_spectrum_nodes": gauge("active_spectrum_nodes"),
        "active_level_nodes": gauge("active_level_nodes"),
    }


def new_trace(perf, start, end):
    """Rows, pops and counters inside [start, end] QPC, the external measurement window."""
    frequency = perf["ticks_per_second"]
    ms = 1000.0 / frequency
    rows = perf["rows"]
    inside = [row for row in rows if start <= row["start"] <= end]
    drawn = [row for row in inside if row["windows_drawn"] > 0]
    drawn_all = [row for row in rows if row["windows_drawn"] > 0]
    wall = [(row["logic_ticks"] + row["ui_ticks"]) * ms for row in drawn]
    render = [row["render_ticks"] * ms for row in drawn]
    # D9: consecutive drawing host-pass starts; the first may reach back before the window.
    intervals = []
    for previous, row in zip(drawn_all, drawn_all[1:]):
        if start <= row["start"] <= end:
            intervals.append((row["start"] - previous["start"]) * ms)
    shown = [row for row in inside if row["newly_shown_frames"] > 0]
    # D8: the oldest new frame's age when a reader first handed out a result covering it.
    oldest = [(row["oldest_read_at"] - row["oldest_callback"]) * ms
              for row in shown if row["oldest_callback"] and row["oldest_read_at"]]
    newest = [(row["read_at"] - row["newest_callback"]) * ms for row in shown if row["newest_callback"]]
    pops = [p for p in perf["pops"] if start <= p[3] <= end]
    pop_age = [(p[3] - p[2]) * ms for p in pops if p[2]]
    seconds = (end - start) / frequency
    plot_counts = sorted({row["plots"] for row in drawn})
    plot_sizes = sorted({tuple(tuple(round(v, 1) for v in p) for p in row["plots_points"][:row["plots"]])
                         for row in drawn})
    return {
        "rows": len(inside), "drawn_rows": len(drawn), "passes_per_second": len(drawn) / seconds,
        # Host passes, drawing or not, with the time of each relative to the window start.
        "host_passes_per_second": len(inside) / seconds,
        "host_pass_offsets_seconds": [(row["start"] - start) / frequency for row in inside if not row["windows_drawn"]][:20],
        "render_excluded_all_rows_ms": distribution([row["render_ticks"] * ms for row in inside]),
        "ui_wall_ms": distribution(wall), "ui_interval_ms": distribution(intervals),
        "render_excluded_ms": distribution(render),
        "latency_ms": distribution(oldest), "latency_newest_ms": distribution(newest),
        "latency_coverage": {"passes_with_new_frames": len(shown), "correlated": len(oldest),
                             "unavailable": len(shown) - len(oldest)},
        "callback_to_worker_pop_ms": distribution(pop_age),
        "pops": len(pops), "pops_without_callback": sum(1 for p in pops if not p[2]),
        **counter_rates(perf, start, end),
        "plots_per_pass": plot_counts, "plot_sizes_points": [list(s) for s in plot_sizes][:6],
        "content_points": sorted({tuple(row["content_points"]) for row in drawn})[:6],
        "pixels_per_point": sorted({row["pixels_per_point"] for row in drawn}),
    }


def old_trace(instrument):
    """The probe's own measurement window (from its first UI pass; protocol 3.2 note)."""
    columns = instrument["sample_columns"]
    index = {name: i for i, name in enumerate(columns)}
    rows = instrument["samples"]
    ns = 1e-6

    def column(name, keep=lambda v: v is not None):
        return [row[index[name]] * ns for row in rows if keep(row[index[name]])]

    plots = sorted({len(row[index["plots_id_width_height"]]) + row[index["plots_overflow"]] for row in rows})
    consuming = [row for row in rows if row[index["consumed_frames_max_per_channel"]]]
    correlated = [row for row in consuming if row[index["oldest_sample_callback_to_pop_ns"]] is not None]
    return {
        "rows": len(rows), "measured_ui_calls": instrument["measured_ui_calls"],
        "ui_wall_ms": distribution(column("ui_wall_ns")),
        "ui_interval_ms": distribution(column("ui_interval_ns", lambda v: v)),
        "latency_ms": distribution(column("oldest_sample_callback_to_pop_ns")),
        "latency_newest_ms": distribution(column("newest_sample_callback_to_pop_ns")),
        "latency_coverage": {"passes_with_new_frames": len(consuming), "correlated": len(correlated),
                             "unavailable": len(consuming) - len(correlated)},
        "fft_per_second": instrument["measured_fft_calls"] / instrument["duration_seconds"],
        "samples_dropped_capacity": instrument["samples_dropped_capacity"],
        "measured_dropped_scalars": instrument["sample_correlation"]["measured_dropped_scalars"],
        "plots_per_pass": plots,
        "content_points": sorted({(row[index["content_width_points"]], row[index["content_height_points"]])
                                  for row in rows})[:6],
        "exit_reason": instrument["exit_reason"],
    }


def summarize_run(run_dir, logical_processors, scenario):
    process = json.loads((run_dir / "process.json").read_text(encoding="utf-8"))
    segments = process_segments(process, logical_processors)
    expected_plots = scenario["plots"]
    run = {"dir": run_dir.name, "scenario": process["scenario"], "build": process["build"],
           "kind": process["kind"], "repetition": process["repetition"], "valid": process["valid"],
           "invalid_reason": process["invalid_reason"], "segments": segments,
           "pre_launch_machine_busy_percent": process["pre_launch_machine_busy_percent"]}
    if not run["valid"]:
        return run
    # Protocol 4.3: 10 s segments need 8 s of samples, multi-segment timelines 3.5 s each.
    scheduled = [name for _, name, _ in scenario["segments"]]
    minimum = 3.5 if len(scheduled) > 1 else process["requested_seconds"] * 0.8
    missing = [name for name in scheduled if name not in segments]
    short = [name for name, seg in segments.items() if seg["sampled_seconds"] < minimum]
    if missing or short:
        run.update(valid=False, invalid_reason=f"segments missing {missing} or short {short}")
    trace = None
    if process["kind"] == "new":
        perf_path = run_dir / "perf" / "perf.json"
        if not perf_path.exists():
            run.update(valid=False, invalid_reason="perf.json missing")
            return run
        perf = json.loads(perf_path.read_text(encoding="utf-8"))
        if perf["rows_overflow"] or perf["pops_overflow"] or perf["counters_overflow"] or perf["plots_overflow"]:
            run.update(valid=False, invalid_reason="trace capacity overflow")
        # Schema 3 traces (the archived S4 sessions) named it `timeline_reset`.
        if perf.get("source_replaced", perf.get("timeline_reset")):
            run.update(valid=False, invalid_reason="source replaced during the run")
        run["trace_segments"] = {name: new_trace(perf, seg["start_qpc"], seg["end_qpc"])
                                 for name, seg in segments.items()}
        run["callback_max_ms"] = perf["callback_max_ticks"] * 1000.0 / perf["ticks_per_second"]
        run["preallocated_bytes"] = perf["preallocated_bytes"]
        trace = run["trace_segments"].get("visible")
    elif process["kind"].startswith("old"):
        instrument = json.loads((run_dir / "instrument.json").read_text(encoding="utf-8"))
        # The probe closes itself after its own warmup + duration; anything else was cut short.
        if instrument["exit_reason"] != "duration":
            run.update(valid=False, invalid_reason=f"probe exit reason {instrument['exit_reason']}")
        if instrument["samples_dropped_capacity"]:
            run.update(valid=False, invalid_reason="probe capacity overflow")
        if process["kind"] == "old":
            trace = run["old_trace"] = old_trace(instrument)
    if trace is not None and trace["plots_per_pass"] != [expected_plots] and expected_plots:
        run.update(valid=False, invalid_reason=f"plots {trace['plots_per_pass']} != {expected_plots}")
    active = trace and (trace.get("fft_per_second") or trace.get("pcm_frames_per_second")
                        or trace.get("level_frames_per_second"))
    if trace is not None and expected_plots and not active:
        run.update(valid=False, invalid_reason="no analysis activity in the visible segment")
    visible = segments.get("visible")
    if visible is not None:
        run["metrics"] = {"cpu": visible["cpu"], "private_mib": visible["private_mib"]}
        if trace is not None:
            run["metrics"].update(ui_wall_p95_ms=trace["ui_wall_ms"].get("p95"),
                                  ui_interval_p95_ms=trace["ui_interval_ms"].get("p95"),
                                  latency_p95_ms=trace["latency_ms"].get("p95"),
                                  latency_unavailable=trace["latency_coverage"]["unavailable"])
    return run


def median_of(runs, metric):
    values = [run["metrics"][metric] for run in runs if run.get("metrics", {}).get(metric) is not None]
    if not values:
        return None
    return {"median": statistics.median(values), "min": min(values), "max": max(values),
            "rounds": len(values), "values": values}


def spread_of(values):
    """(max - min) / median of the per-round values."""
    return (values["max"] - values["min"]) / values["median"] if values and values["median"] else 0


def verdicts(scenario, new, old):
    """D6: baseline self-check band, absolute verdict, paired ratio; D7 for `different`."""
    rows = {}
    for metric in METRICS:
        candidate = median_of(new, metric)
        baseline = median_of(old, metric)
        row = {"candidate": candidate, "baseline": baseline}
        threshold = THRESHOLDS.get(scenario, {}).get(metric)
        reference = HISTORICAL.get(scenario, {}).get(metric)
        if threshold is not None:
            band = [reference - (threshold - reference), threshold]
            row.update(threshold=threshold, historical=reference, baseline_band=band)
        pairs = []
        for run in new:
            twin = next((o for o in old if o["repetition"] == run["repetition"]), None)
            a = run.get("metrics", {}).get(metric)
            b = twin.get("metrics", {}).get(metric) if twin else None
            if a is not None and b:
                pairs.append(a / b)
        row["paired_ratio"] = {"median": statistics.median(pairs), "values": pairs} if pairs else None
        unavailable = None
        if metric == "latency_p95_ms":
            count = lambda runs: sum(r.get("metrics", {}).get("latency_unavailable") or 0 for r in runs)
            unavailable = {"candidate": count(new), "baseline": count(old)}
            row["latency_unavailable"] = unavailable
        if candidate is None:
            row["verdict"] = "UNMEASURED"
        elif threshold is None:
            row["verdict"] = "NOT_COMPARABLE"
            row["reason"] = "no frozen threshold for this scenario"
        elif scenario not in MATCHED:
            row["verdict"] = "NOT_COMPARABLE"
            row["reason"] = "D7: Tilt/channel variant, not the historical bands/Tilt load"
        elif baseline is None:
            row["verdict"] = "NOT_COMPARABLE"
            row["reason"] = "no paired baseline in this session"
        elif min(candidate["rounds"], baseline["rounds"], len(pairs)) < MIN_ROUNDS:
            row["verdict"] = "NOT_COMPARABLE"
            row["reason"] = f"fewer than {MIN_ROUNDS} valid paired rounds"
        elif not band[0] <= baseline["median"] <= band[1]:
            row["verdict"] = "NOT_COMPARABLE"
            row["reason"] = "D6: rebuilt baseline outside its historical band (environment)"
        elif candidate["median"] > threshold:
            row["verdict"] = "FAIL"
        elif unavailable and unavailable["candidate"]:
            # Protocol 4.2: a pass cannot rest on the correlated subset alone.
            row["verdict"] = "NOT_COMPARABLE"
            row["reason"] = "candidate latency correlation incomplete"
        else:
            row["verdict"] = "PASS"
        if threshold is not None and candidate is not None:
            row["near_threshold"] = abs(candidate["median"] - threshold) <= NEAR * threshold
            row["round_spread"] = {"candidate": spread_of(candidate), "baseline": spread_of(baseline)}
            row["retest_required"] = row["near_threshold"] or max(row["round_spread"].values()) > SPREAD
        rows[metric] = row
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("run_dir", type=Path)
    parser.add_argument("--new", default="new")
    parser.add_argument("--old", default="old")
    args = parser.parse_args()
    manifest = json.loads((args.run_dir / "manifest.json").read_text(encoding="utf-8"))
    processors = manifest["environment"]["logical_processors"]
    scenarios_by_name = manifest["scenarios"]
    runs = [summarize_run(path.parent, processors,
                          scenarios_by_name[json.loads(path.read_text(encoding="utf-8"))["scenario"]])
            for path in sorted(args.run_dir.glob("*/process.json"))]
    by = {}
    for run in runs:
        by.setdefault((run["scenario"], run["build"]), []).append(run)
    scenarios = list(dict.fromkeys(run["scenario"] for run in runs))
    summary = {"schema_version": 1, "run_dir": str(args.run_dir), "runs": runs, "scenarios": {}}
    for scenario in scenarios:
        builds = {build: [r for r in rs if r["valid"]] for (s, build), rs in by.items() if s == scenario}
        invalid = [r["dir"] for (s, _), rs in by.items() if s == scenario for r in rs if not r["valid"]]
        entry = {"valid_rounds": {b: len(rs) for b, rs in builds.items()}, "invalid_runs": invalid,
                 "by_build": {b: {m: median_of(rs, m) for m in METRICS} for b, rs in builds.items()}}
        if args.new in builds:
            entry["verdicts"] = verdicts(scenario, builds[args.new], builds.get(args.old, []))
        summary["scenarios"][scenario] = entry
    (args.run_dir / "summary.json").write_text(json.dumps(summary, indent=1), encoding="utf-8")
    for scenario, entry in summary["scenarios"].items():
        print(f"== {scenario}: valid {entry['valid_rounds']} invalid {entry['invalid_runs']}")
        for metric, row in entry.get("verdicts", {}).items():
            cand = row["candidate"]["median"] if row["candidate"] else None
            base = row["baseline"]["median"] if row["baseline"] else None
            ratio = row["paired_ratio"]["median"] if row["paired_ratio"] else None
            fmt = lambda v: "-" if v is None else f"{v:.4g}"
            print(f"  {metric:20} new {fmt(cand):>9} old {fmt(base):>9} ratio {fmt(ratio):>7} "
                  f"thr {fmt(row.get('threshold')):>9} {row['verdict']}"
                  f"{' RETEST' if row.get('retest_required') else ''}")


if __name__ == "__main__":
    main()
