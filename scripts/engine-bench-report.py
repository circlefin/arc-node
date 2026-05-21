#!/usr/bin/env python3
"""Analyze arc-engine-bench CSV output and emit JSON to stdout.

Usage:

    engine-bench-report.py <results_dir> [--baseline <dir>] [--markdown <path>]

Reads `combined_latency.csv` from `results_dir`, aggregates per-block data into
headline throughput and latency metrics, uses `summary.csv` existence only as
a benchmark-completion marker, resolves a report status
(`normal`/`partial`/`no_data`/`error`), and prints a JSON analysis document to
stdout. Callers (e.g., CI) parse the JSON directly.

With `--baseline <dir>`, averages the headline metrics across prior-run
complete prior runs under `<dir>/<run_id>/{summary,combined_latency}.csv`,
where `summary.csv` is only a completion marker, and emits a Δ table alongside
current values, flagging regressions against the trailing average. Incomplete
baseline runs are skipped.

With `--markdown <path>`, also renders a markdown report at that path.

CSV schema and percentile implementation mirror
`crates/engine-bench/src/bench/output.rs`. All aggregates are computed from
per-block data.

Exit codes:
    0  JSON printed (and markdown written if requested)
    1  results_dir does not exist
    2  bad usage / argparse
    3  markdown write failed after JSON was emitted
"""

import argparse
import csv
import json
import math
import os
import sys
import traceback

WINDOW_SIZE_HIGH = 1500  # ≥ WINDOW_SIZE_HIGH → 10 windows
WINDOW_SIZE_LOW = 600    # ≥ WINDOW_SIZE_LOW  →  5 windows; below → no windowed trend
OUTLIER_MULTIPLIER = 5   # per-block outlier: total_ms > OUTLIER_MULTIPLIER × median(total_ms)
TOP_OUTLIERS_LIMIT = 10
TAIL_LATENCY_RATIO = 3   # p99/p50 threshold for a Flag
PER_BLOCK_OUTLIER_LIST_LIMIT = 3

REPORT_STATUS_NORMAL = "normal"
REPORT_STATUS_PARTIAL = "partial"
REPORT_STATUS_NO_DATA = "no_data"
REPORT_STATUS_ERROR = "error"

SUMMARY_MARKER_PRESENT = "present"
SUMMARY_MARKER_MISSING = "missing"
SUMMARY_MARKER_EMPTY = "empty"

# CombinedLatencyRow — crates/engine-bench/src/bench/output.rs:29-42.
EXPECTED_COMBINED_COLUMNS = [
    "block_number", "block_hash", "tx_count", "gas_used",
    "new_payload_ms", "fcu_ms", "total_ms", "elapsed_ms",
    "mgas_per_s", "tx_per_s",
    "cumulative_mgas_per_s", "cumulative_tx_per_s",
]

# Baseline comparison. All rows render in the benchmark summary table;
# REGRESSION_METRICS is the subset that raises ⚠ on >10% drift.
BASELINE_TARGET_N = 5
REGRESSION_PCT = 10.0
HEADLINE_ROWS = [
    ("avg_mgas_per_s",     "Throughput (avg)", "throughput"),
    ("avg_tx_per_s",       "Tx/s (avg)",       "throughput"),
    ("avg_new_payload_ms", "`new_payload` avg", "latency"),
    ("p50_new_payload_ms", "`new_payload` p50", "latency"),
    ("p95_new_payload_ms", "`new_payload` p95", "latency"),
    ("p99_new_payload_ms", "`new_payload` p99", "latency"),
    ("avg_fcu_ms",         "`fcu` avg",         "latency"),
    ("p50_fcu_ms",         "`fcu` p50",         "latency"),
    ("p95_fcu_ms",         "`fcu` p95",         "latency"),
    ("p99_fcu_ms",         "`fcu` p99",         "latency"),
    ("avg_total_ms",       "`total` avg",       "latency"),
    ("p50_total_ms",       "`total` p50",       "latency"),
    ("p95_total_ms",       "`total` p95",       "latency"),
    ("p99_total_ms",       "`total` p99",       "latency"),
]
REGRESSION_METRICS = {
    "p95_new_payload_ms", "p99_new_payload_ms",
    "p95_fcu_ms", "p99_fcu_ms",
    "avg_mgas_per_s", "avg_tx_per_s",
}


# ----- percentile + stats (pinned to Rust parity — do not modify) -----

def percentile(sorted_values, q):
    n = len(sorted_values)
    if n == 0:
        return 0.0
    if n == 1:
        return sorted_values[0]
    q = max(0.0, min(1.0, q))
    rank = q * (n - 1)
    lo = math.floor(rank)
    hi = math.ceil(rank)
    if lo == hi:
        return sorted_values[lo]
    return sorted_values[lo] + (sorted_values[hi] - sorted_values[lo]) * (rank - lo)


def stats(values):
    if not values:
        return None
    sv = sorted(values)
    return {
        "n": len(values),
        "avg": sum(values) / len(values),
        "p50": percentile(sv, 0.5),
        "p95": percentile(sv, 0.95),
        "p99": percentile(sv, 0.99),
        "max": sv[-1],
    }


# ----- combined_latency.csv parsing -----

def _to_finite_float(s):
    v = float(s)
    if not math.isfinite(v):
        raise ValueError(f"non-finite float: {s!r}")
    return v


def _as_finite_float(value):
    try:
        v = float(value)
    except (TypeError, ValueError):
        return None
    return v if math.isfinite(v) else None


def _parse_combined_row(r):
    # csv::Writer in output.rs buffers writes (flushed on Drop); a crashed
    # bench can leave the final row byte-truncated. Missing cells, and
    # non-finite floats from corrupt cells, signal a torn/malformed row —
    # caller drops it so aggregates stay clean.
    for col in EXPECTED_COMBINED_COLUMNS:
        if r.get(col) in (None, ""):
            return None
    try:
        return {
            "block_number": int(r["block_number"]),
            "block_hash": r["block_hash"],
            "tx_count": int(r["tx_count"]),
            "gas_used": int(r["gas_used"]),
            "new_payload_ms": _to_finite_float(r["new_payload_ms"]),
            "fcu_ms": _to_finite_float(r["fcu_ms"]),
            "total_ms": _to_finite_float(r["total_ms"]),
            "elapsed_ms": _to_finite_float(r["elapsed_ms"]),
            "mgas_per_s": _to_finite_float(r["mgas_per_s"]),
            "tx_per_s": _to_finite_float(r["tx_per_s"]),
            "cumulative_mgas_per_s": _to_finite_float(r["cumulative_mgas_per_s"]),
            "cumulative_tx_per_s": _to_finite_float(r["cumulative_tx_per_s"]),
        }
    except ValueError:
        return None


def _load_combined_rows(path):
    with open(path, newline="", encoding="utf-8") as f:
        reader = csv.DictReader(f)
        rows = []
        try:
            for row in reader:
                rows.append(row)
        except csv.Error as e:
            raise csv.Error(f"line {reader.line_num}: {e}") from e
        return rows


# ----- summary.csv completion marker -----

def summary_marker_status(results_dir):
    path = os.path.join(results_dir, "summary.csv")
    if not os.path.exists(path):
        return SUMMARY_MARKER_MISSING
    return SUMMARY_MARKER_PRESENT


def _compute_windows(n, block_num, new_payload_ms):
    if n >= WINDOW_SIZE_HIGH:
        n_windows = 10
    elif n >= WINDOW_SIZE_LOW:
        n_windows = 5
    else:
        return 0, []

    window_size = n // n_windows
    windows = []
    for i in range(n_windows):
        lo = i * window_size
        hi = (i + 1) * window_size if i < n_windows - 1 else n
        slice_np = new_payload_ms[lo:hi]
        sv = sorted(slice_np)
        windows.append({
            "first_block": block_num[lo],
            "last_block": block_num[hi - 1],
            "avg": sum(slice_np) / len(slice_np),
            "p50": percentile(sv, 0.5),
            "p95": percentile(sv, 0.95),
        })
    return n_windows, windows


def _compute_per_block_outliers(block_num, total_ms, median_total):
    if median_total <= 0:
        return []
    threshold = OUTLIER_MULTIPLIER * median_total
    return [
        {"block": blk, "total_ms": v, "median": median_total}
        for blk, v in zip(block_num, total_ms)
        if v > threshold
    ]


def _compute_throughput(cum_mgas_last, cum_tx_last, total_gas, total_txs, last_elapsed_ms):
    # cumulative_* in output.rs are denominator-normalised over run elapsed,
    # so the final row's cumulative is the run-wide average. Fall back to
    # totals when the bench crashed before that denominator became non-zero.
    last_elapsed_s = last_elapsed_ms / 1000.0 if last_elapsed_ms > 0 else 0.0
    if cum_mgas_last <= 0.0 and last_elapsed_s > 0.0:
        return (
            total_gas / last_elapsed_s / 1_000_000.0,
            total_txs / last_elapsed_s,
            "recomputed",
        )
    return cum_mgas_last, cum_tx_last, "cumulative"


def analyze(raw_rows):
    parsed = [_parse_combined_row(r) for r in raw_rows]
    n_raw = len(parsed)
    # Only the trailing row may be torn (csv::Writer was mid-flush when the
    # bench crashed). Earlier Nones signal schema drift or corruption and
    # must be surfaced distinctly so a systematic mismatch is not masked.
    dropped_torn_rows = 1 if n_raw > 0 and parsed[-1] is None else 0
    dropped_malformed_rows = sum(1 for p in parsed[:-1] if p is None)
    rows = [r for r in parsed if r is not None]
    n = len(rows)

    if n == 0:
        return {
            "samples": 0,
            "n_raw": n_raw,
            "dropped_torn_rows": dropped_torn_rows,
            "dropped_malformed_rows": dropped_malformed_rows,
            "empty": True,
        }

    cols = {k: [r[k] for r in rows] for k in rows[0]}
    block_num = cols["block_number"]
    block_hash = cols["block_hash"]
    tx_count = cols["tx_count"]
    gas_used = cols["gas_used"]
    new_payload_ms = cols["new_payload_ms"]
    fcu_ms = cols["fcu_ms"]
    total_ms = cols["total_ms"]
    elapsed_ms = cols["elapsed_ms"]
    mgas_per_s = cols["mgas_per_s"]
    tx_per_s = cols["tx_per_s"]

    n_tx_gt_0 = sum(1 for c in tx_count if c > 0)
    n_tx_eq_0 = n - n_tx_gt_0

    np_tx_gt_0 = [v for v, c in zip(new_payload_ms, tx_count) if c > 0]
    np_tx_eq_0 = [v for v, c in zip(new_payload_ms, tx_count) if c == 0]

    indexed = sorted(range(n), key=lambda i: (-new_payload_ms[i], block_num[i]))
    top_outliers = [
        {
            "block": block_num[i],
            "new_payload_ms": new_payload_ms[i],
            "tx": tx_count[i],
            "gas": gas_used[i],
        }
        for i in indexed[:TOP_OUTLIERS_LIMIT]
    ]

    n_windows, windows = _compute_windows(n, block_num, new_payload_ms)

    sv_total = sorted(total_ms)
    median_total = percentile(sv_total, 0.5)
    per_block_outliers = _compute_per_block_outliers(block_num, total_ms, median_total)

    total_gas = sum(gas_used)
    total_txs = sum(tx_count)
    last_elapsed_ms = elapsed_ms[-1]
    avg_mgas_per_s, avg_tx_per_s, throughput_source = _compute_throughput(
        cols["cumulative_mgas_per_s"][-1], cols["cumulative_tx_per_s"][-1],
        total_gas, total_txs, last_elapsed_ms,
    )

    sv_np = sorted(new_payload_ms)
    sv_fcu = sorted(fcu_ms)
    headline = {
        "samples": n,
        "total_gas": total_gas,
        "total_txs": total_txs,
        "last_sampled_elapsed_ms": last_elapsed_ms,
        "avg_new_payload_ms": sum(new_payload_ms) / n,
        "avg_fcu_ms": sum(fcu_ms) / n,
        "avg_total_ms": sum(total_ms) / n,
        "avg_mgas_per_s": avg_mgas_per_s,
        "avg_tx_per_s": avg_tx_per_s,
        "throughput_source": throughput_source,
        "p50_new_payload_ms": percentile(sv_np, 0.5),
        "p95_new_payload_ms": percentile(sv_np, 0.95),
        "p99_new_payload_ms": percentile(sv_np, 0.99),
        "p50_fcu_ms": percentile(sv_fcu, 0.5),
        "p95_fcu_ms": percentile(sv_fcu, 0.95),
        "p99_fcu_ms": percentile(sv_fcu, 0.99),
        "p50_total_ms": percentile(sv_total, 0.5),
        "p95_total_ms": percentile(sv_total, 0.95),
        "p99_total_ms": percentile(sv_total, 0.99),
    }
    workload = {
        "samples": n,
        "first_block_hash": block_hash[0],
        "last_block_hash": block_hash[-1],
    }

    throughput_tx_bearing = None
    if n_tx_gt_0 > 0:
        throughput_tx_bearing = {
            "mgas_per_s": stats([v for v, c in zip(mgas_per_s, tx_count) if c > 0]),
            "tx_per_s": stats([v for v, c in zip(tx_per_s, tx_count) if c > 0]),
        }

    return {
        "samples": n,
        "n_raw": n_raw,
        "dropped_torn_rows": dropped_torn_rows,
        "dropped_malformed_rows": dropped_malformed_rows,
        "n_tx_gt_0": n_tx_gt_0,
        "n_tx_eq_0": n_tx_eq_0,
        "per_class": {
            "all": stats(new_payload_ms),
            "tx_gt_0": stats(np_tx_gt_0),
            "tx_eq_0": stats(np_tx_eq_0),
        },
        "top_outliers": top_outliers,
        "top_outliers_count": len(top_outliers),
        "n_windows": n_windows,
        "windows": windows,
        "throughput_tx_bearing": throughput_tx_bearing,
        "per_block_outliers": per_block_outliers,
        "headline": headline,
        "workload": workload,
    }


# ----- baseline loading, averaging, comparison -----

# Reasons a baseline subdir is skipped.
BASELINE_SKIP_MISSING = "missing"
BASELINE_SKIP_EMPTY = "empty"
BASELINE_SKIP_PARSE_FAILED = "parse_failed"
BASELINE_SKIP_PARTIAL = "partial"
BASELINE_SKIP_WORKLOAD_END_MISMATCH = "workload_end_mismatch"
BASELINE_SKIP_WORKLOAD_MISMATCH = "workload_mismatch"


def _summary_marker_incomplete_reason(summary_marker):
    if summary_marker == SUMMARY_MARKER_MISSING:
        return "summary.csv missing"
    if summary_marker == SUMMARY_MARKER_EMPTY:
        return "summary.csv empty"
    if summary_marker != SUMMARY_MARKER_PRESENT:
        return f"summary.csv marker status unknown: {summary_marker}"
    return None


def _baseline_subdirs(baseline_dir):
    # Degenerate case: baseline_dir itself holds a single combined_latency.csv.
    if os.path.isfile(os.path.join(baseline_dir, "combined_latency.csv")):
        return [("_single", baseline_dir)]
    try:
        names = sorted(os.listdir(baseline_dir))
    except OSError:
        return []
    return [
        (name, os.path.join(baseline_dir, name))
        for name in names
        if os.path.isdir(os.path.join(baseline_dir, name))
    ]


def _workload_diff_fields(current, baseline):
    if current is None or baseline is None:
        return []
    fields = ("samples", "first_block_hash", "last_block_hash")
    return [field for field in fields if current.get(field) != baseline.get(field)]


def _workload_skip_reason(current, baseline, diff_fields):
    same_start = (
        current is not None
        and baseline is not None
        and current.get("first_block_hash") == baseline.get("first_block_hash")
    )
    end_fields = {"samples", "last_block_hash"}
    if same_start and any(field in end_fields for field in diff_fields):
        return BASELINE_SKIP_WORKLOAD_END_MISMATCH
    return BASELINE_SKIP_WORKLOAD_MISMATCH


def _load_baseline_headlines(baseline_dir, current_workload=None):
    """Return (contributing, skipped).

    `contributing` is a list of {"id": str, "headline": dict} where headline
    is derived from combined_latency.csv. `skipped` is a list of
    {"id": str, "reason": BASELINE_SKIP_*, "error": [etype, emsg] | None}.
    """
    contributing = []
    skipped = []
    if not os.path.isdir(baseline_dir):
        return contributing, skipped

    for run_id, subdir in _baseline_subdirs(baseline_dir):
        combined_path = os.path.join(subdir, "combined_latency.csv")
        if not os.path.exists(combined_path):
            skipped.append({"id": run_id, "reason": BASELINE_SKIP_MISSING, "error": None})
            continue
        summary_marker = summary_marker_status(subdir)
        if summary_marker != SUMMARY_MARKER_PRESENT:
            skipped.append({
                "id": run_id,
                "reason": BASELINE_SKIP_PARTIAL,
                "error": _summary_marker_incomplete_reason(summary_marker),
            })
            continue
        try:
            analysis = analyze(_load_combined_rows(combined_path))
        except (ValueError, KeyError) + _CSV_READ_ERRORS as e:
            skipped.append({
                "id": run_id,
                "reason": BASELINE_SKIP_PARSE_FAILED,
                "error": [type(e).__name__, str(e)],
            })
            continue
        if analysis.get("samples", 0) == 0:
            skipped.append({"id": run_id, "reason": BASELINE_SKIP_EMPTY, "error": None})
            continue
        baseline_workload = analysis.get("workload")
        diff_fields = _workload_diff_fields(current_workload, baseline_workload)
        if diff_fields:
            skipped.append({
                "id": run_id,
                "reason": _workload_skip_reason(
                    current_workload,
                    baseline_workload,
                    diff_fields,
                ),
                "error": None,
                "fields": diff_fields,
                "current_workload": current_workload,
                "baseline_workload": baseline_workload,
            })
            continue
        contributing.append({"id": run_id, "headline": analysis["headline"]})
    return contributing, skipped


def _average_baseline_headlines(contributing):
    if not contributing:
        return None
    metrics = {}
    for metric, _label, _direction in HEADLINE_ROWS:
        vals = []
        for entry in contributing:
            v = entry["headline"].get(metric)
            if v is None:
                continue
            fv = _as_finite_float(v)
            if fv is None:
                continue
            vals.append(fv)
        metrics[metric] = sum(vals) / len(vals) if vals else None
    return {
        "n_runs": len(contributing),
        "contributing_ids": [e["id"] for e in contributing],
        "metrics": metrics,
    }


def _compute_comparison(current_headline, averaged_baseline):
    if current_headline is None or averaged_baseline is None:
        return None
    deltas = {}
    regressions = []
    for metric, _label, direction in HEADLINE_ROWS:
        current = current_headline.get(metric)
        baseline = averaged_baseline["metrics"].get(metric)
        if current is None or baseline is None:
            continue
        current_f = _as_finite_float(current)
        baseline_f = _as_finite_float(baseline)
        if current_f is None or baseline_f is None:
            continue
        if baseline_f <= 0:
            continue
        delta_abs = current_f - baseline_f
        delta_pct = (delta_abs / baseline_f) * 100.0
        is_regression = (
            metric in REGRESSION_METRICS
            and (
                (direction == "latency" and delta_pct > REGRESSION_PCT)
                or (direction == "throughput" and delta_pct < -REGRESSION_PCT)
            )
        )
        deltas[metric] = {
            "current": current_f,
            "baseline": baseline_f,
            "delta_abs": delta_abs,
            "delta_pct": delta_pct,
            "direction": direction,
            "regression": is_regression,
        }
        if is_regression:
            regressions.append(metric)
    return {
        "n_runs": averaged_baseline["n_runs"],
        "contributing_ids": list(averaged_baseline["contributing_ids"]),
        "deltas": deltas,
        "regressions": regressions,
    }


# ----- report status resolution -----

def resolve_report_status(summary_marker, analysis):
    if analysis is not None and analysis.get("samples", 0) > 0:
        if summary_marker != SUMMARY_MARKER_PRESENT:
            return REPORT_STATUS_PARTIAL
        return REPORT_STATUS_NORMAL
    return REPORT_STATUS_NO_DATA


# ----- flag computation -----

def _fmt_metric_value(metric, value):
    # Deferred lookups — formatters are defined after this module block.
    if metric == "avg_mgas_per_s":
        return _fmt_mgas_per_s(value)
    if metric == "avg_tx_per_s":
        return f"{float(value):.1f} tx/s"
    if metric.endswith("_ms"):
        return _fmt_ms(value)
    return _fmt_1dp(value)


def _fmt_delta_pct(pct):
    sign = "+" if pct >= 0 else ""
    return f"{sign}{pct:.1f}%"


def _fmt_metric_or_dash(metric, value):
    if _as_finite_float(value) is None:
        return "—"
    return _fmt_metric_value(metric, value)


def compute_flags(
    analysis,
    analyze_error,
    comparison=None,
    baseline_requested=False,
    baseline_skipped=None,
    report_status=None,
    summary_marker=SUMMARY_MARKER_MISSING,
):
    flags = []

    if report_status == REPORT_STATUS_PARTIAL:
        reason = _summary_marker_incomplete_reason(summary_marker)
        samples = (analysis or {}).get("samples", 0)
        flags.append(
            f"⚠ partial run: {reason}; N={samples} blocks in combined_latency.csv"
        )

    if analysis is not None:
        n_torn = analysis.get("dropped_torn_rows", 0)
        if n_torn > 0:
            s = "" if n_torn == 1 else "s"
            flags.append(f"⚠ dropped {n_torn} torn trailing row{s} from combined_latency.csv")

        n_malformed = analysis.get("dropped_malformed_rows", 0)
        if n_malformed > 0:
            s = "" if n_malformed == 1 else "s"
            flags.append(
                f"⚠ dropped {n_malformed} malformed mid-file row{s} from combined_latency.csv "
                "— possible schema drift"
            )

        if not analyze_error:
            n_raw = analysis.get("n_raw", 0)
            samples = analysis.get("samples", 0)
            if samples == 0 and n_raw > 0:
                flags.append(
                    f"⚠ all {n_raw} row{'s' if n_raw != 1 else ''} in "
                    "combined_latency.csv were malformed — schema drift likely"
                )
            elif samples == 0 and n_raw == 0:
                flags.append("⚠ combined_latency.csv had no data rows (header-only)")

    if analyze_error is not None:
        etype, emsg = analyze_error
        flags.append(f"⚠ combined_latency.csv parse failed: {etype}: {emsg}")

    headline = (analysis or {}).get("headline") or {}
    if headline.get("throughput_source") == "recomputed":
        flags.append("⚠ cumulative throughput column was zero; recomputed from totals")

    if analysis is not None:
        samples = analysis.get("samples", 0)
        if 0 < samples < 10:
            flags.append(
                f"⚠ only {samples} block{'s' if samples != 1 else ''} sampled "
                "— percentile statistics are degenerate"
            )

    p50_total = headline.get("p50_total_ms")
    p99_total = headline.get("p99_total_ms")
    if p50_total is not None and p99_total is not None and p50_total > 0:
        ratio = p99_total / p50_total
        if ratio > TAIL_LATENCY_RATIO:
            flags.append(f"⚠ tail-latency divergence: p99/p50={ratio:.2f}")

    if analysis is not None:
        outliers = analysis.get("per_block_outliers", [])
        shown = outliers[:PER_BLOCK_OUTLIER_LIST_LIMIT]
        for o in shown:
            flags.append(
                f"⚠ per-block outlier: block {o['block']}, "
                f"total_ms={o['total_ms']:.1f}, median={o['median']:.1f}"
            )
        rest = len(outliers) - len(shown)
        if rest > 0:
            flags.append(f"…and {rest} more.")

    if baseline_requested:
        skipped = baseline_skipped or []
        if comparison is not None:
            for metric in comparison["regressions"]:
                d = comparison["deltas"][metric]
                flags.append(
                    f"⚠ regression vs. baseline: {metric} "
                    f"{_fmt_delta_pct(d['delta_pct'])} "
                    f"({_fmt_metric_value(metric, d['current'])} vs "
                    f"{_fmt_metric_value(metric, d['baseline'])}, "
                    f"n={comparison['n_runs']})"
                )
            if comparison["n_runs"] < BASELINE_TARGET_N:
                flags.append(
                    f"⚠ baseline averaged over only {comparison['n_runs']} of "
                    f"{BASELINE_TARGET_N} expected nightly runs"
                )
        elif analysis is not None and analysis.get("samples", 0) > 0:
            # Current had data but baseline dir had no usable contributors.
            flags.append(
                "⚠ no valid nightly baselines found — comparison skipped"
            )
        for entry in skipped:
            reason = entry.get("reason", "unknown")
            err = entry.get("error")
            suffix = ""
            if isinstance(err, list) and len(err) == 2:
                suffix = f": {err[0]}: {err[1]}"
            elif err:
                suffix = f": {err}"
            if reason in (
                BASELINE_SKIP_WORKLOAD_END_MISMATCH,
                BASELINE_SKIP_WORKLOAD_MISMATCH,
            ):
                diff_fields = entry.get("fields", [])
                fields = ", ".join(diff_fields)
                if reason == BASELINE_SKIP_WORKLOAD_END_MISMATCH:
                    reason = "workload ended differently; possible incomplete run"
                else:
                    reason = "workload mismatch"
                verb = "differs" if len(diff_fields) == 1 else "differ"
                suffix = f": {fields} {verb}" if fields else ""
            elif reason == BASELINE_SKIP_PARTIAL:
                reason = "partial"
            flags.append(
                f"⚠ baseline run {entry['id']} skipped: {reason}{suffix}"
            )

    return flags


# ----- formatters -----

def _fmt_int(n):
    return f"{int(n)}"


def _fmt_int_grouped(n):
    n = int(n)
    return f"{n:,}" if n >= 10_000 else f"{n}"


def _fmt_gas_used(n):
    return f"{int(n):,}"


def _fmt_total_gas(n):
    n = float(n)
    if n >= 1e9:
        return f"{n / 1e9:.2f} Ggas"
    if n >= 1e6:
        return f"{n / 1e6:.1f} Mgas"
    return f"{int(n):,} gas"


def _fmt_ms(v):
    return f"{float(v):.1f} ms"


def _fmt_1dp(v):
    return f"{float(v):.1f}"


def _fmt_mgas_per_s(v):
    return f"{float(v):.1f} Mgas/s"


def _fmt_wall_clock_ms(ms):
    seconds = float(ms) / 1000.0
    if seconds >= 10_000:
        return f"{seconds:,.1f} s"
    return f"{seconds:.1f} s"


# ----- report rendering -----

def _render_minimal_report(flags):
    lines = [
        "# arc-engine-bench: no data",
        "",
        "_No benchmark output was available — EaaS may not have produced "
        "results, the run aborted before the first block, or the CSV was "
        "truncated._",
        "",
    ]
    lines.extend(_render_flags(flags))
    return "\n".join(lines) + "\n"


def _render_partial_banner(analysis):
    samples = (analysis or {}).get("samples", 0)
    return [
        "> ⚠ **Partial results** — `summary.csv` did not confirm benchmark completion.",
        f"> Headline numbers below are derived from `combined_latency.csv` ({samples} blocks).",
        "",
    ]


def _render_title_and_workload(analysis):
    headline = analysis["headline"]
    classes_line = (
        f"tx-bearing: {_fmt_int_grouped(analysis['n_tx_gt_0'])} · "
        f"empty: {_fmt_int_grouped(analysis['n_tx_eq_0'])} · "
    )
    return [
        "# arc-engine-bench",
        "",
        f"Samples: {_fmt_int_grouped(headline['samples'])} blocks · {classes_line}"
        f"total gas: {_fmt_total_gas(headline['total_gas'])} · "
        f"total tx: {_fmt_int_grouped(headline['total_txs'])} · "
        f"elapsed: {_fmt_wall_clock_ms(headline['last_sampled_elapsed_ms'])}.",
        "",
    ]


def _render_headline(analysis, comparison, baseline_requested):
    """Unified headline table. With a valid comparison, renders
    Metric / Current / Baseline (avg) / Δ columns. Otherwise 2-col
    Metric / Value."""
    headline = analysis["headline"]
    with_baseline = comparison is not None

    if with_baseline:
        lines = [
            "## Benchmark summary",
            "",
            "| Metric | Current | Baseline (avg) | Δ |",
            "|---|---:|---:|---:|",
        ]
    else:
        lines = ["## Benchmark summary", "", "| Metric | Value |", "|---|---:|"]

    for metric, label, _direction in HEADLINE_ROWS:
        current = headline.get(metric)
        current_cell = _fmt_metric_or_dash(metric, current)
        if not with_baseline:
            lines.append(f"| {label} | {current_cell} |")
            continue
        d = comparison["deltas"].get(metric)
        if d is None:
            lines.append(f"| {label} | {current_cell} | — | — |")
            continue
        marker = " ⚠" if d["regression"] else ""
        lines.append(
            f"| {label} | "
            f"{_fmt_metric_value(metric, d['current'])} | "
            f"{_fmt_metric_value(metric, d['baseline'])} | "
            f"{_fmt_delta_pct(d['delta_pct'])}{marker} |"
        )
    lines.append("")

    if with_baseline:
        n = comparison["n_runs"]
        lines.append(
            f"_Baseline averages the last {n} nightly run{'s' if n != 1 else ''}._"
        )
        lines.append("")
    elif baseline_requested:
        lines.append(
            "_No recent nightly baseline available._"
        )
        lines.append("")
    return lines


def _render_per_window(analysis):
    heading = "## Per-window trend (`new_payload_ms`)"
    lines = [heading, ""]
    if analysis.get("n_windows", 0) == 0:
        lines.append("_Run too short for windowed trend._")
        lines.append("")
        return lines

    lines.append("| Blocks | avg | p50 | p95 |")
    lines.append("|---|---:|---:|---:|")
    for w in analysis["windows"]:
        lines.append(
            f"| {_fmt_int(w['first_block'])}–{_fmt_int(w['last_block'])} "
            f"| {_fmt_1dp(w['avg'])} "
            f"| {_fmt_1dp(w['p50'])} "
            f"| {_fmt_1dp(w['p95'])} |"
        )
    lines.append("")
    return lines


def _render_top_outliers(analysis):
    count = analysis.get("top_outliers_count", 0)
    lines = [f"## Top `new_payload` outliers (N={count})", ""]
    if count == 0:
        lines.append("_No samples to rank._")
        lines.append("")
        return lines
    lines.append("| Block | new_payload_ms | tx | gas |")
    lines.append("|---:|---:|---:|---:|")
    for o in analysis["top_outliers"]:
        lines.append(
            f"| {_fmt_int(o['block'])} "
            f"| {_fmt_1dp(o['new_payload_ms'])} "
            f"| {_fmt_int_grouped(o['tx'])} "
            f"| {_fmt_gas_used(o['gas'])} |"
        )
    lines.append("")
    return lines


def _render_per_class_entry(label, cls):
    if cls is None:
        return f"| {label} | — | — | — | — | — | — |"
    return (
        f"| {label} | {_fmt_int_grouped(cls['n'])} "
        f"| {_fmt_1dp(cls['avg'])} "
        f"| {_fmt_1dp(cls['p50'])} "
        f"| {_fmt_1dp(cls['p95'])} "
        f"| {_fmt_1dp(cls['p99'])} "
        f"| {_fmt_1dp(cls['max'])} |"
    )


def _render_per_class(analysis):
    pc = analysis["per_class"]
    lines = [
        "## Per-class breakdown (`new_payload_ms`)",
        "",
        "| Class | n | avg | p50 | p95 | p99 | max |",
        "|---|---:|---:|---:|---:|---:|---:|",
        _render_per_class_entry("all", pc["all"]),
        _render_per_class_entry("tx > 0", pc["tx_gt_0"]),
        _render_per_class_entry("tx = 0", pc["tx_eq_0"]),
        "",
    ]

    tb = analysis.get("throughput_tx_bearing")
    lines.append("**Throughput on tx-bearing blocks**")
    lines.append("")
    if tb is None:
        lines.append("_No tx-bearing blocks in run._")
        lines.append("")
    else:
        lines.append("| Metric | avg | p50 | p95 |")
        lines.append("|---|---:|---:|---:|")
        for metric_label, key in (("`mgas_per_s`", "mgas_per_s"), ("`tx_per_s`", "tx_per_s")):
            s = tb[key]
            lines.append(
                f"| {metric_label} | {_fmt_1dp(s['avg'])} "
                f"| {_fmt_1dp(s['p50'])} "
                f"| {_fmt_1dp(s['p95'])} |"
            )
        lines.append("")

    lines.append(
        "> The per-class `mgas_per_s` / `tx_per_s` are "
        "**per-block instantaneous** (gas_used ÷ per-block latency), "
        "while the Headline `Throughput (avg)` is "
        "**bench-elapsed averaged** (total gas ÷ last-sampled elapsed). "
        "The two series are not directly comparable."
    )
    lines.append("")
    return lines


def _render_flags(flags):
    lines = ["## Flags", ""]
    if not flags:
        lines.append("_No flags._")
    else:
        for f in flags:
            lines.append(f"- {f}")
    lines.append("")
    return lines


def render_report(report_status, analysis, flags, comparison=None, baseline_requested=False):
    if report_status == REPORT_STATUS_NO_DATA:
        return _render_minimal_report(flags)

    lines = []
    if report_status == REPORT_STATUS_PARTIAL:
        lines.extend(_render_partial_banner(analysis))
    lines.extend(_render_title_and_workload(analysis))
    lines.extend(_render_headline(analysis, comparison, baseline_requested))
    lines.extend(_render_per_window(analysis))
    lines.extend(_render_top_outliers(analysis))
    lines.extend(_render_per_class(analysis))
    lines.extend(_render_flags(flags))
    return "\n".join(lines) + "\n"


# ----- orchestration -----

# Read-side errors worth surfacing as "parse failed" flags rather than
# crashing the whole run — callers keep whatever data did parse.
_CSV_READ_ERRORS = (OSError, UnicodeDecodeError, csv.Error)


def _atomic_write(path, text):
    tmp = f"{path}.tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        f.write(text)
    os.replace(tmp, path)


def _render_error_report(exc_info):
    etype, emsg = exc_info
    lines = [
        "# arc-engine-bench: renderer error",
        "",
        "_The renderer hit an unexpected error while processing the benchmark "
        "output. The CI logs contain the full traceback._",
        "",
        "## Error",
        "",
        f"`{etype}: {emsg}`",
        "",
        "## Flags",
        "",
        "- ⚠ renderer exited with an unexpected error; see CI logs for details.",
        "",
    ]
    return "\n".join(lines) + "\n"


def _run_analysis(results_dir, baseline_dir=None):
    """Parse inputs and return the full analysis document as a dict.

    Raises FileNotFoundError if results_dir does not exist. All other
    combined_latency.csv parse errors are captured into `analyze_error`
    on the returned dict so the caller can still emit JSON.
    """
    if not os.path.isdir(results_dir):
        raise FileNotFoundError(f"results_dir not found: {results_dir}")

    combined_path = os.path.join(results_dir, "combined_latency.csv")

    summary_marker = summary_marker_status(results_dir)

    analysis = None
    analyze_error = None
    if os.path.exists(combined_path):
        try:
            analysis = analyze(_load_combined_rows(combined_path))
        except (ValueError, KeyError) + _CSV_READ_ERRORS as e:
            analyze_error = [type(e).__name__, str(e)]

    report_status = resolve_report_status(summary_marker, analysis)
    baseline_requested = baseline_dir is not None
    baseline_contributing = []
    baseline_skipped = []
    averaged_baseline = None
    comparison = None
    current_headline = (analysis or {}).get("headline")
    current_workload = (analysis or {}).get("workload")
    if baseline_requested and report_status == REPORT_STATUS_NORMAL:
        baseline_contributing, baseline_skipped = _load_baseline_headlines(
            baseline_dir,
            current_workload=current_workload,
        )
        averaged_baseline = _average_baseline_headlines(baseline_contributing)
        comparison = _compute_comparison(current_headline, averaged_baseline)

    flags = compute_flags(
        analysis, analyze_error,
        comparison=comparison,
        baseline_requested=baseline_requested and report_status == REPORT_STATUS_NORMAL,
        baseline_skipped=baseline_skipped,
        report_status=report_status,
        summary_marker=summary_marker,
    )

    return {
        "report_status": report_status,
        "summary_marker": summary_marker,
        "analysis": analysis,
        "analyze_error": analyze_error,
        "flags": flags,
        "baseline_requested": baseline_requested,
        "baseline_contributing_ids": [e["id"] for e in baseline_contributing],
        "baseline_skipped": baseline_skipped,
        "averaged_baseline": averaged_baseline,
        "comparison": comparison,
    }


def _render_markdown(data):
    if data["report_status"] == REPORT_STATUS_ERROR:
        return _render_error_report(data["error"])
    return render_report(
        data["report_status"], data["analysis"], data["flags"],
        comparison=data.get("comparison"),
        baseline_requested=data.get("baseline_requested", False),
    )


def _error_document(exc):
    etype, emsg = type(exc).__name__, str(exc)
    return {
        "report_status": REPORT_STATUS_ERROR,
        "error": [etype, emsg],
        "summary_marker": SUMMARY_MARKER_MISSING,
        "analysis": None,
        "analyze_error": None,
        "flags": [f"⚠ renderer error: {etype}: {emsg}"],
        "baseline_requested": False,
        "baseline_contributing_ids": [],
        "baseline_skipped": [],
        "averaged_baseline": None,
        "comparison": None,
    }


def main(argv):
    parser = argparse.ArgumentParser(
        description="Analyze arc-engine-bench combined_latency.csv; print JSON to stdout.",
    )
    parser.add_argument(
        "results_dir",
        help=(
            "Directory containing combined_latency.csv and optional "
            "summary.csv completion marker."
        ),
    )
    parser.add_argument(
        "--markdown",
        metavar="PATH",
        help="Also render a markdown report to PATH.",
    )
    parser.add_argument(
        "--baseline",
        metavar="DIR",
        help=(
            "Directory of prior-run baselines. Layout: "
            "DIR/<ID>/{summary,combined_latency}.csv (one subdirectory per "
            "contributing run; summary.csv is only a completion marker)."
        ),
    )
    args = parser.parse_args(argv[1:])

    try:
        data = _run_analysis(args.results_dir, baseline_dir=args.baseline)
    except FileNotFoundError as e:
        print(f"::error::{e}", file=sys.stderr)
        return 1
    except Exception as e:
        # Emit an error-status document so callers parsing stdout as JSON
        # still get a well-formed payload.
        traceback.print_exc(file=sys.stderr)
        data = _error_document(e)

    json.dump(data, sys.stdout, indent=2)
    sys.stdout.write("\n")

    if args.markdown:
        try:
            _atomic_write(args.markdown, _render_markdown(data))
        except OSError as e:
            print(
                f"::error::could not write markdown to {args.markdown}: {e}",
                file=sys.stderr,
            )
            return 3

    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
