#!/usr/bin/env python3
"""
Generate a self-contained HTML report from a quake saturation experiment.

Reads <experiment_dir>/experiment.json and per-phase tx_latency_*.csv files,
then writes <experiment_dir>/report.html.

Usage:
    python3 scripts/saturation_report.py <experiment_dir>

Dependencies: matplotlib, jinja2 (pip install matplotlib jinja2).
"""

from __future__ import annotations

import argparse
import base64
import csv
import io
import json
import sys
import tarfile
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

try:
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.patches as mpatches
    import matplotlib.pyplot as plt
except ImportError:
    print("error: matplotlib is required — install with: pip install matplotlib", file=sys.stderr)
    sys.exit(1)

try:
    from jinja2 import Environment
except ImportError:
    print("error: jinja2 is required — install with: pip install jinja2", file=sys.stderr)
    sys.exit(1)

# ── Signal metadata ───────────────────────────────────────────────────────────

SIGNAL_ORDER = [
    "gas_plateaued",
    "tps_plateaued",
    "tps_ratio_drop",
    "latency_spike",
    "mempool_growth",
    "el_cpu_saturated",
]

SIGNAL_LABELS = {
    "gas_plateaued": "Gas Plateaued",
    "tps_plateaued": "TPS Plateaued",
    "tps_ratio_drop": "TPS Ratio Drop",
    "latency_spike": "Latency Spike",
    "mempool_growth": "Mempool Growth",
    "el_cpu_saturated": "EL CPU Saturated",
}

# ── Palette ───────────────────────────────────────────────────────────────────

C_ACTUAL    = "#2196F3"
C_OFFERED   = "#90CAF9"
C_FILL      = "#FF9800"
C_GAS       = "#4CAF50"
C_P50       = "#9C27B0"
C_P95       = "#F44336"
C_MEMPOOL   = "#009688"
C_SAT_LINE  = "#F44336"
C_SIG_ON    = "#F44336"
C_SIG_OFF   = "#E8F5E9"

# ── Data loading ──────────────────────────────────────────────────────────────

def load_experiment(experiment_dir: Path) -> dict:
    path = experiment_dir / "experiment.json"
    if not path.exists():
        print(f"error: experiment.json not found in {experiment_dir}", file=sys.stderr)
        sys.exit(1)
    with open(path) as f:
        return json.load(f)


def find_latest_csv(phase_dir: Path) -> Optional[Path]:
    if not phase_dir.exists():
        return None
    candidates = sorted(
        (p for p in phase_dir.iterdir() if p.name.startswith("tx_latency_") and p.name.endswith(".csv")),
        key=lambda p: p.stat().st_mtime,
    )
    return candidates[-1] if candidates else None


def read_latency_ms(csv_path: Path) -> list[float]:
    """Parse submitted_at and finalized_observed_at columns and return latencies in ms."""
    latencies: list[float] = []
    with open(csv_path, newline="") as f:
        reader = csv.reader(f)
        for i, row in enumerate(reader):
            if i == 0:
                continue
            if len(row) < 3:
                continue
            try:
                t_sub = _parse_rfc3339(row[1].strip())
                t_fin = _parse_rfc3339(row[2].strip())
                if t_sub is None or t_fin is None:
                    continue
                diff_ms = (t_fin - t_sub).total_seconds() * 1000
                if diff_ms >= 0:
                    latencies.append(diff_ms)
            except (ValueError, IndexError):
                continue
    return latencies


def load_metrics_tarball(experiment_dir: Path) -> dict[str, list[dict]]:
    """Load non-empty Prometheus series from metrics.tar.gz.

    Returns a dict mapping metric_name → list of series (each with 'metric' labels and 'values').
    Returns an empty dict if the tarball is absent, unreadable, or all series are empty.
    """
    tar_path = experiment_dir / "metrics.tar.gz"
    if not tar_path.exists():
        return {}
    result: dict[str, list[dict]] = {}
    try:
        with tarfile.open(tar_path, "r:gz") as tar:
            for member in tar.getmembers():
                if not member.name.endswith(".json"):
                    continue
                metric_name = Path(member.name).stem
                f = tar.extractfile(member)
                if f is None:
                    continue
                data = json.load(f)
                series = data.get("data", {}).get("result", [])
                if series:
                    result[metric_name] = series
    except Exception as e:
        print(f"warning: could not read metrics.tar.gz: {e}", file=sys.stderr)
    return result


def _parse_rfc3339(s: str) -> Optional[datetime]:
    if s.endswith("Z"):
        s = s[:-1] + "+00:00"
    try:
        return datetime.fromisoformat(s)
    except ValueError:
        return None

# ── Saturation detection ──────────────────────────────────────────────────────

def find_saturation_phase(phases: list[dict]) -> Optional[dict]:
    """First phase with ≥2 signals; falls back to first phase with a critical signal."""
    for phase in phases:
        if len(phase.get("signals", [])) >= 2:
            return phase
    critical = {"gas_plateaued", "tps_ratio_drop"}
    for phase in phases:
        if any(s in critical for s in phase.get("signals", [])):
            return phase
    return None


def saturation_index(phases: list[dict], sat_phase: Optional[dict]) -> Optional[int]:
    if sat_phase is None:
        return None
    for i, p in enumerate(phases):
        if p is sat_phase:
            return i
    return None

# ── Chart helpers ─────────────────────────────────────────────────────────────

def _fig_to_base64(fig) -> str:
    buf = io.BytesIO()
    fig.savefig(buf, format="png", dpi=130, bbox_inches="tight")
    plt.close(fig)
    return base64.b64encode(buf.getvalue()).decode()


def _phase_labels(phases: list[dict]) -> list[str]:
    return [str(p["offered_tps"]) for p in phases]


def _annotate_saturation(ax, sat_idx: Optional[int]) -> None:
    if sat_idx is not None:
        ax.axvline(x=sat_idx, color=C_SAT_LINE, linestyle="--", linewidth=1.5, alpha=0.7, label="Saturation")

# ── Charts ────────────────────────────────────────────────────────────────────

def chart_throughput(phases: list[dict], sat_idx: Optional[int]) -> str:
    xs = list(range(len(phases)))
    labels = _phase_labels(phases)
    actual = [p["metrics"].get("actual_tps") for p in phases]
    offered = [p["offered_tps"] for p in phases]
    fill = [p["metrics"].get("fill_pct") for p in phases]

    fig, ax1 = plt.subplots(figsize=(9, 4))
    ax2 = ax1.twinx()

    ax1.bar(xs, [v or 0 for v in actual], color=C_ACTUAL, alpha=0.8, label="Actual TPS", zorder=3)
    ax1.plot(xs, offered, color=C_OFFERED, linestyle="--", marker="o", markersize=5, label="Offered TPS", zorder=4)
    ax2.plot(xs, [v or 0 for v in fill], color=C_FILL, linestyle="-", marker="s", markersize=5, label="Fill %", zorder=4)

    _annotate_saturation(ax1, sat_idx)
    ax1.set_xticks(xs)
    ax1.set_xticklabels(labels)
    ax1.set_xlabel("Offered TPS")
    ax1.set_ylabel("TPS")
    ax2.set_ylabel("Fill %")
    ax2.set_ylim(0, 115)
    lines1, labs1 = ax1.get_legend_handles_labels()
    lines2, labs2 = ax2.get_legend_handles_labels()
    ax1.legend(lines1 + lines2, labs1 + labs2, loc="upper left", fontsize=8)
    ax1.set_title("Throughput per Phase")
    ax1.grid(axis="y", alpha=0.3)
    fig.tight_layout()
    return _fig_to_base64(fig)


def chart_gas(phases: list[dict], sat_idx: Optional[int]) -> str:
    xs = list(range(len(phases)))
    labels = _phase_labels(phases)
    gas_m = [(p["metrics"].get("gas_per_sec") or 0) / 1_000_000 for p in phases]

    fig, ax = plt.subplots(figsize=(9, 3.5))
    ax.bar(xs, gas_m, color=C_GAS, alpha=0.8)
    _annotate_saturation(ax, sat_idx)
    ax.set_xticks(xs)
    ax.set_xticklabels(labels)
    ax.set_xlabel("Offered TPS")
    ax.set_ylabel("Gas/s (millions)")
    ax.set_title("Gas Throughput per Phase")
    ax.grid(axis="y", alpha=0.3)
    fig.tight_layout()
    return _fig_to_base64(fig)


def chart_latency(phases: list[dict], sat_idx: Optional[int]) -> str:
    xs = list(range(len(phases)))
    labels = _phase_labels(phases)
    p50 = [p["metrics"].get("latency_p50_ms") for p in phases]
    p95 = [p["metrics"].get("latency_p95_ms") for p in phases]

    fig, ax = plt.subplots(figsize=(9, 3.5))
    xs_50 = [x for x, v in zip(xs, p50) if v is not None]
    xs_95 = [x for x, v in zip(xs, p95) if v is not None]
    ax.plot(xs_50, [v for v in p50 if v is not None], color=C_P50, marker="o", label="p50")
    ax.plot(xs_95, [v for v in p95 if v is not None], color=C_P95, marker="s", label="p95")
    _annotate_saturation(ax, sat_idx)
    ax.set_xticks(xs)
    ax.set_xticklabels(labels)
    ax.set_xlabel("Offered TPS")
    ax.set_ylabel("Latency (ms)")
    ax.set_title("Submit-to-Finalized Latency per Phase")
    ax.legend(fontsize=8)
    ax.grid(alpha=0.3)
    fig.tight_layout()
    return _fig_to_base64(fig)


C_QUEUED    = "#FF5722"
C_BASEFEE   = "#9C27B0"

def chart_mempool(phases: list[dict], sat_idx: Optional[int]) -> str:
    """Stacked sub-pool counts + per-pool size in MB on a paired axis.

    Each sub-pool's peak count gets a bar at the left, the size in MB its
    twin on the right. Two axes share the X (offered-TPS labels) so visual
    ordering of phases stays consistent. Basefee is included because it
    is the sub-pool that silently swallowed legacy txs at saturation in
    earlier 50k-account runs.
    """
    xs = list(range(len(phases)))
    labels = _phase_labels(phases)

    pk_pend = [p["metrics"].get("max_mempool") or 0 for p in phases]
    pk_qued = [p["metrics"].get("max_queued_mempool") or 0 for p in phases]
    pk_base = [p["metrics"].get("max_basefee_mempool") or 0 for p in phases]

    pk_pend_mb = [p["metrics"].get("max_pending_size_mb") or 0 for p in phases]
    pk_qued_mb = [p["metrics"].get("max_queued_size_mb") or 0 for p in phases]
    pk_base_mb = [p["metrics"].get("max_basefee_size_mb") or 0 for p in phases]

    has_qued = any(v > 0 for v in pk_qued)
    has_base = any(v > 0 for v in pk_base)

    fig, (ax_count, ax_size) = plt.subplots(1, 2, figsize=(13, 3.5))

    # Left: peak counts, stacked pending → queued → basefee.
    ax_count.bar(xs, pk_pend, color=C_MEMPOOL, alpha=0.7, label="Pending")
    bottom = list(pk_pend)
    if has_qued:
        ax_count.bar(xs, pk_qued, bottom=bottom, color=C_QUEUED, alpha=0.7, label="Queued (future nonce)")
        bottom = [b + q for b, q in zip(bottom, pk_qued)]
    if has_base:
        ax_count.bar(xs, pk_base, bottom=bottom, color=C_BASEFEE, alpha=0.7, label="Basefee")
    _annotate_saturation(ax_count, sat_idx)
    ax_count.set_xticks(xs); ax_count.set_xticklabels(labels)
    ax_count.set_xlabel("Offered TPS"); ax_count.set_ylabel("Transactions")
    ax_count.set_title("Peak sub-pool depth — count")
    ax_count.legend(fontsize=8)
    ax_count.grid(axis="y", alpha=0.3)

    # Right: peak sizes in MB, same stacking colours so each pool reads as
    # the same "track" across both panels.
    ax_size.bar(xs, pk_pend_mb, color=C_MEMPOOL, alpha=0.7, label="Pending")
    bottom = list(pk_pend_mb)
    if has_qued:
        ax_size.bar(xs, pk_qued_mb, bottom=bottom, color=C_QUEUED, alpha=0.7, label="Queued")
        bottom = [b + q for b, q in zip(bottom, pk_qued_mb)]
    if has_base:
        ax_size.bar(xs, pk_base_mb, bottom=bottom, color=C_BASEFEE, alpha=0.7, label="Basefee")
    _annotate_saturation(ax_size, sat_idx)
    ax_size.set_xticks(xs); ax_size.set_xticklabels(labels)
    ax_size.set_xlabel("Offered TPS"); ax_size.set_ylabel("Cumulative MB")
    ax_size.set_title("Peak sub-pool depth — size (MB)")
    ax_size.legend(fontsize=8)
    ax_size.grid(axis="y", alpha=0.3)

    fig.tight_layout()
    return _fig_to_base64(fig)


def chart_signals(phases: list[dict]) -> str:
    n_phases = len(phases)
    n_sigs = len(SIGNAL_ORDER)

    fig, ax = plt.subplots(figsize=(max(6, n_phases * 1.4), 3))

    for j, phase in enumerate(phases):
        fired = set(phase.get("signals", []))
        for i, sig in enumerate(SIGNAL_ORDER):
            color = C_SIG_ON if sig in fired else C_SIG_OFF
            ax.add_patch(plt.Rectangle((j - 0.5, i - 0.5), 1, 1, color=color, linewidth=0.5, edgecolor="#ccc"))

    ax.set_xlim(-0.5, n_phases - 0.5)
    ax.set_ylim(-0.5, n_sigs - 0.5)
    ax.set_xticks(range(n_phases))
    ax.set_xticklabels(_phase_labels(phases))
    ax.set_yticks(range(n_sigs))
    ax.set_yticklabels([SIGNAL_LABELS[s] for s in SIGNAL_ORDER])
    ax.set_xlabel("Offered TPS")
    ax.set_title("Saturation Signals by Phase")
    ax.invert_yaxis()
    legend = [
        mpatches.Patch(color=C_SIG_ON, label="Fired"),
        mpatches.Patch(color=C_SIG_OFF, label="Clear", edgecolor="#aaa"),
    ]
    ax.legend(handles=legend, loc="upper right", fontsize=8)
    fig.tight_layout()
    return _fig_to_base64(fig)


def chart_memory(phases: list[dict], sat_idx: Optional[int]) -> Optional[str]:
    xs = list(range(len(phases)))
    labels = _phase_labels(phases)
    avg_mb  = [p["metrics"].get("el_mem_avg_mb") for p in phases]
    peak_mb = [p["metrics"].get("el_mem_peak_mb") for p in phases]

    if all(v is None for v in avg_mb) and all(v is None for v in peak_mb):
        return None

    fig, ax = plt.subplots(figsize=(9, 3.5))
    xs_a = [x for x, v in zip(xs, avg_mb) if v is not None]
    xs_p = [x for x, v in zip(xs, peak_mb) if v is not None]
    if xs_a:
        ax.bar(xs_a, [v for v in avg_mb if v is not None], color="#42A5F5", alpha=0.8, label="Avg (MiB)")
    if xs_p:
        ax.plot(xs_p, [v for v in peak_mb if v is not None],
                color="#1565C0", marker="^", linewidth=1.5, linestyle="--", label="Peak (MiB)")
    _annotate_saturation(ax, sat_idx)
    ax.set_xticks(xs)
    ax.set_xticklabels(labels)
    ax.set_xlabel("Offered TPS")
    ax.set_ylabel("Resident Memory (MiB)")
    ax.set_title("EL Resident Memory per Phase")
    ax.legend(fontsize=8)
    ax.grid(axis="y", alpha=0.3)
    fig.tight_layout()
    return _fig_to_base64(fig)


def chart_pool_evictions_prom(
    prom_metrics: dict,
    exp_start: Optional[datetime],
    phase_starts: list[tuple[float, int]],
) -> Optional[str]:
    """Rate of evictions per sub-pool (pending, basefee, blob, queued), summed across nodes."""
    any_data = any(prom_metrics.get(m) for m, _, _ in _EVICTION_METRICS)
    if not any_data:
        return None

    fig, ax = plt.subplots(figsize=(9, 3.5))
    for metric_name, label, color in _EVICTION_METRICS:
        series = prom_metrics.get(metric_name)
        if not series:
            continue
        summed: dict[float, float] = {}
        for s in series:
            xs, ys = _series_to_xy(s, exp_start)
            rx, ry = _compute_rate(xs, ys)
            for t, r in zip(rx, ry):
                k = round(t, 3)
                summed[k] = summed.get(k, 0.0) + r
        if summed:
            pts = sorted(summed.items())
            ax.plot([p[0] for p in pts], [p[1] for p in pts],
                    marker=".", markersize=2, linewidth=1, label=label, color=color)

    for t_min, rate in phase_starts:
        ax.axvline(x=t_min, color="#bbb", linestyle=":", linewidth=1)
        ax.text(t_min + 0.05, 0.98, f"{rate}", fontsize=6, color="#666", va="top",
                transform=ax.get_xaxis_transform())

    ax.set_xlabel("Time (min from start)" if exp_start is not None else "Time (min)")
    ax.set_ylabel("Evictions/s")
    ax.set_title("Pool Evictions by Sub-pool (rate, summed across nodes)")
    ax.legend(fontsize=8)
    ax.grid(alpha=0.3)
    fig.tight_layout()
    return _fig_to_base64(fig)


def chart_memory_prom(
    prom_metrics: dict,
    exp_start: Optional[datetime],
    phase_starts: list[tuple[float, int]],
    node_colors: Optional[dict[str, tuple]] = None,
) -> Optional[str]:
    """EL resident memory over time from the Prometheus tarball (MiB, per node)."""
    series = prom_metrics.get("reth_process_resident_memory_bytes")
    if not series:
        return None

    mib = 1_048_576.0
    fig, ax = plt.subplots(figsize=(9, 3.5))
    for s in series:
        node = _extract_node_label(s.get("metric", {}))
        xs, ys = _series_to_xy(s, exp_start)
        ys_mib = [y / mib for y in ys]
        if xs:
            color = node_colors.get(node) if node_colors else None
            ax.plot(xs, ys_mib, marker=".", markersize=2, linewidth=1, label=node, color=color)

    for t_min, rate in phase_starts:
        ax.axvline(x=t_min, color="#bbb", linestyle=":", linewidth=1)
        ax.text(t_min + 0.05, 0.98, f"{rate}", fontsize=6, color="#666", va="top",
                transform=ax.get_xaxis_transform())

    ax.set_xlabel("Time (min from start)" if exp_start is not None else "Time (min)")
    ax.set_ylabel("Resident Memory (MiB)")
    ax.set_title("EL Resident Memory (MiB)")
    if node_colors is None and len(series) <= 10:
        ax.legend(fontsize=7, loc="upper right")
    ax.grid(alpha=0.3)
    fig.tight_layout()
    return _fig_to_base64(fig)


def chart_block_timing(phases: list[dict], sat_idx: Optional[int]) -> str:
    xs = list(range(len(phases)))
    labels = _phase_labels(phases)
    build    = [p["metrics"].get("avg_block_build_time_ms") for p in phases]
    finalize = [p["metrics"].get("avg_block_finalize_time_ms") for p in phases]

    fig, ax = plt.subplots(figsize=(9, 3.5))
    xs_b = [x for x, v in zip(xs, build) if v is not None]
    xs_f = [x for x, v in zip(xs, finalize) if v is not None]
    if xs_b:
        ax.plot(xs_b, [v for v in build if v is not None],
                color="#1976D2", marker="o", label="Build time (ms)")
    if xs_f:
        ax.plot(xs_f, [v for v in finalize if v is not None],
                color="#E65100", marker="s", linestyle="--", label="Finalize time (ms)")
    _annotate_saturation(ax, sat_idx)
    ax.set_xticks(xs)
    ax.set_xticklabels(labels)
    ax.set_xlabel("Offered TPS")
    ax.set_ylabel("Time (ms)")
    ax.set_title("Block Build & Finalize Time per Phase")
    ax.legend(fontsize=8)
    ax.grid(alpha=0.3)
    fig.tight_layout()
    return _fig_to_base64(fig)


def chart_latency_dist(experiment_dir: Path, phases: list[dict]) -> list[tuple[int, str]]:
    """Per-phase latency histograms, one standalone image per phase.

    Rendered separately (rather than as a single row of subplots) so a
    reader can click any histogram in the report and enlarge it on its
    own instead of blowing up all phases at once. Returns an ordered list
    of ``(offered_tps, base64_png)`` pairs; empty list if no CSV data.
    """
    charts: list[tuple[int, str]] = []
    for phase in phases:
        rate = phase["offered_tps"]
        csv_path = find_latest_csv(experiment_dir / f"phase_{rate}")
        if not csv_path:
            continue
        lats = read_latency_ms(csv_path)
        if not lats:
            continue

        fig, ax = plt.subplots(figsize=(4.5, 3.2))
        ax.hist(lats, bins=40, color=C_ACTUAL, alpha=0.8, edgecolor="white", linewidth=0.3)
        ax.set_title(f"{rate} TPS")
        ax.set_xlabel("Latency (ms)")
        ax.set_ylabel("Transactions")
        ax.grid(alpha=0.3, axis="y")
        fig.tight_layout()
        charts.append((rate, _fig_to_base64(fig)))

    return charts

# Prometheus chart config.
#
# Each entry is (metric_name, title, ylabel, mode) where mode is:
#   "rate"     — compute per-second rate from a cumulative counter/histogram _sum
#   "avg"      — compute per-block average: Δsum / Δcount, requires a paired
#                "<metric>_count" series in the tarball
#   "raw"      — plot the raw cumulative value as-is
_PROM_CHART_CONFIG = [
    ("arc_malachite_app_block_time_sum",                    "Block Time (avg)",            "Seconds/block",  "avg"),
    ("arc_malachite_app_block_build_time_sum",              "Block Build Time (avg)",      "Seconds/block",  "avg"),
    ("arc_malachite_app_block_finalize_time_sum",           "Block Finalize Time (avg)",   "Seconds/block",  "avg"),
    ("arc_malachite_app_block_gas_used_sum",                "Gas Used (rate)",             "Gas/s",          "rate"),
    ("arc_malachite_app_block_transactions_count_sum",      "Block Tx Count (rate)",       "Tx/s",           "rate"),
    ("malachitebft_core_consensus_consensus_round_sum",     "Consensus Round (avg)",       "Round/block",    "avg"),
    ("malachitebft_core_consensus_consensus_time_sum",      "Consensus Time (avg)",        "Seconds/block",  "avg"),
    ("arc_malachite_app_height_restart_count_total",        "Height Restart Count",        "Count",          "raw"),
    ("arc_malachite_app_sync_fell_behind_count_total",      "Sync Fell Behind Count",      "Count",          "raw"),
    ("reth_process_cpu_seconds_total",                      "EL CPU Usage (rate)",         "CPU cores",      "rate"),
    # Txpool gauges — pending = executable now, queued = nonce-gapped
    ("reth_transaction_pool_pending_pool_transactions",     "Txpool Pending",              "Transactions",   "raw"),
    ("reth_transaction_pool_queued_pool_transactions",      "Txpool Queued (nonce gap)",   "Transactions",   "raw"),
]

# Pool eviction sub-pools, rendered together in one combined chart.
_EVICTION_METRICS = [
    ("reth_transaction_pool_pending_transactions_evicted_total",  "Pending",        "#2196F3"),
    ("reth_transaction_pool_basefee_transactions_evicted_total",  "Base fee",       "#FF9800"),
    ("reth_transaction_pool_blob_transactions_evicted_total",     "Blob",           "#9C27B0"),
    ("reth_transaction_pool_queued_transactions_evicted_total",   "Queued (nonce)", "#F44336"),
]


def _extract_node_label(metric_labels: dict) -> str:
    """Return a clean node name (e.g. ``validator3``) for legends.

    Prefer the Prometheus ``job`` label, which quake's scrape config sets to
    ``<node>_el`` / ``<node>_cl``; strip that side suffix so every chart keys
    on the raw node name. Fall back to ``node``/``instance`` for older
    setups. ``instance`` is IP-based so it's the last resort — avoid it in
    legends when a name is available.
    """
    for key in ("node", "job"):
        v = metric_labels.get(key)
        if v:
            for suf in ("_el", "_cl"):
                if v.endswith(suf):
                    return v[: -len(suf)]
            return v
    return metric_labels.get("instance") or str(metric_labels)


def _natural_key(name: str) -> list:
    """Sort key that orders ``validator2`` before ``validator10``.

    Splits into alternating text/int chunks so trailing digits compare
    numerically, keeping node groups (validator, sentry, full, …) clustered
    together and ordered by their index within each group.
    """
    import re
    return [int(t) if t.isdigit() else t for t in re.split(r"(\d+)", name)]


def _build_node_color_map(series_lists: list[list[dict]]) -> dict[str, tuple]:
    """Assign a stable RGBA color to every node name observed across the
    Prometheus series. Colors come from ``tab20`` (distinct up to 20 nodes)
    or ``hsv`` (continuous, for larger topologies)."""
    names: set[str] = set()
    for series_list in series_lists:
        for s in series_list:
            names.add(_extract_node_label(s.get("metric", {})))
    ordered = sorted(names, key=_natural_key)
    cmap_name = "tab20" if len(ordered) <= 20 else "hsv"
    cmap = plt.get_cmap(cmap_name)
    n = len(ordered)
    return {name: cmap(i / max(n, 1)) for i, name in enumerate(ordered)}


def chart_prom_legend(node_colors: dict[str, tuple]) -> Optional[str]:
    """Render a shared color→node-name legend for the Prometheus charts.

    Individual per-node charts hide their legend when the node count is
    large (crowded, unreadable); this standalone legend is the reference for
    all of them, so every chart uses the same node→color mapping.
    """
    if not node_colors:
        return None
    n = len(node_colors)
    # Wider row so each label has room; row count grows with node count.
    ncol = min(n, 12)
    nrow = (n + ncol - 1) // ncol
    fig, ax = plt.subplots(figsize=(18, max(0.3 * nrow + 0.4, 1.0)))
    ax.axis("off")
    handles = [mpatches.Patch(color=c, label=name) for name, c in node_colors.items()]
    ax.legend(handles=handles, loc="center", ncol=ncol, fontsize=9, frameon=False)
    fig.tight_layout()
    return _fig_to_base64(fig)


def _series_to_xy(s: dict, exp_start: Optional[datetime]) -> tuple[list[float], list[float]]:
    """Convert a Prometheus series to (x_minutes, y_values) lists."""
    values = s.get("values", [])
    timestamps = [float(v[0]) for v in values]
    ys: list[float] = []
    for v in values:
        try:
            ys.append(float(v[1]))
        except (ValueError, TypeError):
            ys.append(float("nan"))
    if exp_start is not None:
        t0 = exp_start.timestamp()
        xs = [(t - t0) / 60 for t in timestamps]
    else:
        xs = [t / 60 for t in timestamps]
    return xs, ys


def _compute_rate(xs: list[float], ys: list[float]) -> tuple[list[float], list[float]]:
    """Compute per-second rate from a monotonically-increasing cumulative series.

    Skips intervals where the counter went backwards (``ys[i] < ys[i-1]``) —
    those are counter resets from a node restart, and treating them as
    negative deltas produces misleading dips on the chart. PromQL's
    ``rate()`` handles resets natively; this is the python-side equivalent.
    """
    rx, ry = [], []
    for i in range(1, len(xs)):
        dt_min = xs[i] - xs[i - 1]
        if dt_min <= 0:
            continue
        d = ys[i] - ys[i - 1]
        if d < 0:
            # Counter reset (process restart). Drop the point — real Prometheus
            # would treat the new value as the post-reset baseline; for a chart
            # we just want to avoid plotting a spurious negative spike.
            continue
        dt_sec = dt_min * 60
        rx.append((xs[i] + xs[i - 1]) / 2)
        ry.append(d / dt_sec)
    return rx, ry


def _compute_avg(
    xs_sum: list[float], ys_sum: list[float],
    xs_cnt: list[float], ys_cnt: list[float],
) -> tuple[list[float], list[float]]:
    """Compute Δsum / Δcount (per-block average) aligned to the _sum timestamps."""
    # Round timestamps to 3 decimal places (1 ms) before keying so that minor
    # float representation differences between _sum and _count scrapes don't
    # silently drop data points.
    _r = lambda t: round(t, 3)
    cnt_map = {_r(t): v for t, v in zip(xs_cnt, ys_cnt)}
    rx, ry = [], []
    for i in range(1, len(xs_sum)):
        d_sum = ys_sum[i] - ys_sum[i - 1]
        cnt_prev = cnt_map.get(_r(xs_sum[i - 1]))
        cnt_curr = cnt_map.get(_r(xs_sum[i]))
        if cnt_prev is None or cnt_curr is None:
            continue
        d_cnt = cnt_curr - cnt_prev
        if d_cnt <= 0:
            continue
        rx.append((xs_sum[i] + xs_sum[i - 1]) / 2)
        ry.append(d_sum / d_cnt)
    return rx, ry


def chart_prometheus_metric(
    series: list[dict],
    title: str,
    ylabel: str,
    mode: str,
    exp_start: Optional[datetime],
    phase_starts: list[tuple[float, int]],
    count_series: Optional[list[dict]] = None,
    node_colors: Optional[dict[str, tuple]] = None,
) -> str:
    """Time-series line chart for a Prometheus metric, one line per node.

    mode:
      "rate" — per-second rate of a cumulative counter/sum
      "avg"  — Δsum / Δcount per block (requires count_series)
      "raw"  — plot raw cumulative values
    """
    fig, ax = plt.subplots(figsize=(9, 3.5))

    # Build a node → count_series lookup for "avg" mode
    count_by_node: dict[str, list[dict]] = {}
    if mode == "avg" and count_series:
        for cs in count_series:
            node = _extract_node_label(cs.get("metric", {}))
            count_by_node[node] = cs

    for s in series:
        node = _extract_node_label(s.get("metric", {}))
        xs, ys = _series_to_xy(s, exp_start)
        if not xs:
            continue

        if mode == "rate":
            px, py = _compute_rate(xs, ys)
        elif mode == "avg" and node in count_by_node:
            xs_cnt, ys_cnt = _series_to_xy(count_by_node[node], exp_start)
            px, py = _compute_avg(xs, ys, xs_cnt, ys_cnt)
        else:
            px, py = xs, ys

        if not px:
            continue
        color = node_colors.get(node) if node_colors else None
        ax.plot(px, py, marker=".", markersize=2, linewidth=1, label=node, color=color)

    for t_min, rate in phase_starts:
        ax.axvline(x=t_min, color="#bbb", linestyle=":", linewidth=1)
        ax.text(
            t_min + 0.05, 0.98, f"{rate}",
            fontsize=6, color="#666", va="top",
            transform=ax.get_xaxis_transform(),
        )

    ax.set_xlabel("Time (min from start)" if exp_start is not None else "Time (min)")
    ax.set_ylabel(ylabel)
    ax.set_title(title)
    ax.grid(alpha=0.3)
    if node_colors is None and len(series) <= 10:
        ax.legend(fontsize=7, loc="upper right")
    fig.tight_layout()
    return _fig_to_base64(fig)


# ── Formatting helpers ────────────────────────────────────────────────────────

def _fmt(v: Optional[float], fmt: str = ".1f", suffix: str = "") -> str:
    return f"{v:{fmt}}{suffix}" if v is not None else "—"


def _fmt_avg_sd(avg: Optional[float], sd: Optional[float]) -> str:
    if avg is None:
        return "—"
    if sd is None:
        return f"{avg:.0f}"
    return f"{avg:.0f}±{sd:.0f}"


def _fmt_count_mb(count: Optional[float], size_mb: Optional[float]) -> str:
    """Render a sub-pool depth cell as ``count(MB)``.

    Reth caps each transaction sub-pool on both transaction count and cumulative
    wire size, so the experiment runner emits both. Show them in the same cell
    so a reader can spot which dimension is binding without cross-referencing
    columns. Either side renders as ``—`` when missing, keeping the layout
    legible against older ``experiment.json`` files that lack the size fields.
    """
    if count is None and size_mb is None:
        return "—"
    c = f"{count:.0f}" if count is not None else "—"
    s = f"{size_mb:.1f}" if size_mb is not None else "—"
    return f"{c} ({s})"


def _fmt_gas(v: Optional[float]) -> str:
    if v is None:
        return "—"
    if v >= 1_000_000:
        return f"{v / 1_000_000:.1f}M"
    if v >= 1_000:
        return f"{v / 1_000:.1f}K"
    return f"{v:.0f}"


def _fmt_duration(seconds: float) -> str:
    h = int(seconds // 3600)
    m = int((seconds % 3600) // 60)
    s = int(seconds % 60)
    if h > 0:
        return f"{h}h {m:02d}m"
    if m > 0:
        return f"{m}m {s:02d}s"
    return f"{s}s"


def _fmt_node_disk(topology: dict) -> str:
    """Render the node disk summary as `<size> GiB / <type> / <iops> IOPS`."""
    size = topology.get("node_disk_gb")
    vol_type = topology.get("node_volume_type")
    iops = topology.get("node_volume_iops")
    parts = []
    if size is not None:
        parts.append(f"{size} GiB")
    if vol_type:
        parts.append(vol_type)
    if iops is not None:
        parts.append(f"{iops} IOPS")
    return " / ".join(parts) if parts else "—"


def _leading_alpha_prefix(name: str) -> str:
    """Strip the trailing non-alphabetic segment of a node name.

    `validator-blue` → `validator`, `sentry-us-east-2-a` → `sentry`,
    `full-1` → `full`. Falls back to the whole name when no alphabetic
    prefix is present.
    """
    chars = []
    for c in name:
        if c.isalpha():
            chars.append(c)
        else:
            break
    return "".join(chars) if chars else name


def _topology_from_manifest(manifest: dict) -> dict:
    """Derive the topology summary fields from the parsed manifest TOML.

    The saturation runner now embeds the full manifest under
    `parameters.manifest` instead of a hand-picked Topology struct; this
    function reconstructs the same summary fields the HTML template expects
    so the topology table keeps rendering without a separate schema.
    """
    nodes = manifest.get("nodes") or {}
    node_names = list(nodes.keys())

    nodes_by_type: dict[str, int] = {}
    for name in node_names:
        t = _leading_alpha_prefix(name)
        nodes_by_type[t] = nodes_by_type.get(t, 0) + 1
    nodes_by_type = dict(sorted(nodes_by_type.items()))

    num_validators = sum(1 for n in node_names if n.startswith("validator"))

    el_storage_v2 = True
    if node_names:
        first = nodes[node_names[0]]
        v2 = (first.get("el") or {}).get("config", {}).get("storage", {}).get("v2")
        if v2 is not None:
            el_storage_v2 = bool(v2)

    return {
        "num_nodes": len(node_names),
        "num_validators": num_validators,
        "nodes_by_type": nodes_by_type,
        "node_size": manifest.get("node_size"),
        "node_disk_gb": manifest.get("node_disk_gb"),
        "node_volume_type": manifest.get("node_volume_type"),
        "node_volume_iops": manifest.get("node_volume_iops"),
        "el_cpu_limit": manifest.get("el_cpu_limit"),
        "el_memory_limit_gb": manifest.get("el_memory_limit_gb"),
        "cl_cpu_limit": manifest.get("cl_cpu_limit"),
        "cl_memory_limit_gb": manifest.get("cl_memory_limit_gb"),
        "extra_account_balance_usdc": manifest.get("extra_account_balance_usdc"),
        "block_gas_limit": manifest.get("block_gas_limit"),
        "cc_size": manifest.get("cc_size"),
        "cc_disk_gb": manifest.get("cc_disk_gb"),
        "image_el": manifest.get("image_el"),
        "image_cl": manifest.get("image_cl"),
        "el_storage_v2": el_storage_v2,
    }


def _fmt_container_resources(cpu: Optional[float], mem_gb: Optional[float]) -> str:
    """Render per-container resource caps as `<cpu> CPU / <mem> GiB`."""
    parts = []
    if cpu is not None:
        parts.append(f"{cpu:g} CPU")
    if mem_gb is not None:
        parts.append(f"{mem_gb:g} GiB")
    return " / ".join(parts) if parts else "—"


def _fmt_usdc(v: Optional[int]) -> str:
    return f"{v:,} USDC" if v is not None else "—"


def _fmt_node_types(by_type: dict) -> str:
    """`13 sentry, 36 full, 21 validator` — sorted by count descending."""
    if not by_type:
        return ""
    items = sorted(by_type.items(), key=lambda kv: (-kv[1], kv[0]))
    return ", ".join(f"{count} {name}" for name, count in items)

# ── HTML template ─────────────────────────────────────────────────────────────

_TEMPLATE = """\
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Saturation Report &mdash; {{ experiment_id }}</title>
  <style>
* { box-sizing: border-box; }
body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; margin: 0; background: #f8f9fa; color: #212529; }
.wrap { max-width: 1100px; margin: 0 auto; padding: 28px 16px; }
h1 { font-size: 1.6rem; margin: 0 0 4px; }
h2 { font-size: 1.1rem; margin: 32px 0 12px; border-bottom: 2px solid #dee2e6; padding-bottom: 6px; }
.meta { color: #6c757d; font-size: 0.85rem; margin-bottom: 24px; }
.badge { display: inline-block; padding: 2px 10px; border-radius: 12px; font-size: 0.78rem; font-weight: 600; text-transform: uppercase; letter-spacing: .03em; }
.badge-completed { background: #d4edda; color: #155724; }
.badge-timed_out { background: #fff3cd; color: #856404; }
.badge-failed { background: #f8d7da; color: #721c24; }
.callout { padding: 12px 16px; border-radius: 4px; margin-bottom: 16px; }
.callout-warn { background: #fff3cd; border-left: 4px solid #ffc107; }
.callout-ok { background: #d4edda; border-left: 4px solid #28a745; }
table { width: 100%; border-collapse: collapse; font-size: 0.87rem; background: #fff; border-radius: 6px; overflow: hidden; box-shadow: 0 1px 3px rgba(0,0,0,.08); margin-bottom: 4px; }
th { background: #343a40; color: #fff; padding: 8px 12px; text-align: right; font-weight: 600; white-space: nowrap; }
th:first-child { text-align: left; }
td { padding: 7px 12px; text-align: right; border-bottom: 1px solid #dee2e6; vertical-align: middle; }
td:first-child { text-align: left; font-weight: 600; }
tr.sat-row td { background: #fff3cd; }
tr:last-child td { border-bottom: none; }
.tag { display: inline-block; background: #f8d7da; color: #721c24; border-radius: 3px; padding: 1px 6px; font-size: 0.75rem; margin: 1px; white-space: nowrap; }
.chart-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; }
.chart-grid-3col { display: grid; grid-template-columns: 1fr 1fr 1fr; gap: 16px; }
.chart-wide { grid-column: 1 / -1; }
.chart-box { background: #fff; border-radius: 6px; box-shadow: 0 1px 3px rgba(0,0,0,.08); padding: 8px; }
.chart-box img { width: 100%; height: auto; display: block; cursor: zoom-in; }
.lightbox { display: none; position: fixed; inset: 0; background: rgba(0,0,0,.75); z-index: 9999; align-items: center; justify-content: center; padding: 24px; }
.lightbox.open { display: flex; }
.lightbox img { max-width: 100%; max-height: 100%; object-fit: contain; border-radius: 4px; box-shadow: 0 8px 32px rgba(0,0,0,.5); cursor: zoom-out; }
details summary { cursor: pointer; font-weight: 600; color: #495057; padding: 6px 0; user-select: none; }
pre { background: #f1f3f5; border-radius: 4px; padding: 12px; font-size: 0.74rem; overflow-x: auto; line-height: 1.4; }
code { font-family: 'SFMono-Regular', Consolas, 'Liberation Mono', Menlo, monospace; }
  </style>
</head>
<body>
<div class="wrap">
<h1>Saturation Experiment Report</h1>
<p class="meta">
  <strong>{{ experiment_id }}</strong> &nbsp;&middot;&nbsp;
  <span class="badge badge-{{ status_type }}">{{ status_type_display }}</span>
  {%- if status_reason %} &mdash; {{ status_reason }}{% endif %}
  &nbsp;&middot;&nbsp; {{ started_str }}
  {%- if ended_str %}&nbsp;&rarr;&nbsp;{{ ended_str }}{% endif %}
  {%- if duration %} &nbsp;&middot;&nbsp; {{ duration }}{% endif %}
</p>

<h2>Parameters</h2>
<table>
  <tr><th>Rates (TPS)</th><td>{{ params_rates }}</td></tr>
  <tr><th>Hold</th><td>{{ params_hold }}</td></tr>
  <tr><th>Ramp-up</th><td>{{ params_warmup }}</td></tr>
  <tr><th>Cooldown</th><td>{{ params_cooldown }}</td></tr>
  <tr><th>Generators</th><td>{{ params_generators }}</td></tr>
  <tr><th>Tx mix</th><td><code>{{ params_tx_mix }}</code></td></tr>
  <tr><th>Guzzler weights</th><td><code>{{ params_guzzler_fn_weights }}</code></td></tr>
  <tr><th>ERC20 weights</th><td><code>{{ params_erc20_fn_weights }}</code></td></tr>
</table>

<h2>Topology &amp; Hardware</h2>
<table>
  <tr><th>Nodes</th><td>{{ topo_num_nodes }}{% if topo_node_types %} ({{ topo_node_types }}){% endif %}</td></tr>
  <tr><th>Validators</th><td>{{ topo_num_validators }}</td></tr>
  <tr><th>Node instance</th><td>{% if topo_node_size %}<code>{{ topo_node_size }}</code>{% else %}—{% endif %}</td></tr>
  <tr><th>Node disk</th><td>{{ topo_node_disk }}</td></tr>
  <tr><th>EL resources</th><td>{{ topo_el_resources }}</td></tr>
  <tr><th>CL resources</th><td>{{ topo_cl_resources }}</td></tr>
  <tr><th>CC instance</th><td>{% if topo_cc_size %}<code>{{ topo_cc_size }}</code>{% else %}—{% endif %}</td></tr>
  <tr><th>CC disk</th><td>{% if topo_cc_disk_gb %}{{ topo_cc_disk_gb }} GiB{% else %}—{% endif %}</td></tr>
  <tr><th>Prefund / account</th><td>{{ topo_account_balance }}</td></tr>
  <tr><th>Block gas limit</th><td>{{ topo_block_gas_limit }}</td></tr>
  <tr><th>EL image</th><td>{% if topo_image_el %}<code>{{ topo_image_el }}</code>{% else %}—{% endif %}</td></tr>
  <tr><th>CL image</th><td>{% if topo_image_cl %}<code>{{ topo_image_cl }}</code>{% else %}—{% endif %}</td></tr>
  <tr><th>EL storage</th><td>{{ topo_el_storage }}</td></tr>
</table>

{% if manifest_json %}
<h2>Manifest</h2>
<details><summary>manifest.json</summary>
<pre>{{ manifest_json }}</pre>
</details>
{% endif %}

<h2>Saturation Point</h2>
{% if sat_tps is not none %}
<div class="callout callout-warn">
  <strong>Saturation detected at {{ sat_tps }} TPS</strong><br>
  Signals: {% for label in sat_signal_labels %}<span class="tag">{{ label }}</span> {% endfor %}
</div>
{% else %}
<div class="callout callout-ok">
  <strong>No saturation detected</strong> within the tested rate range.
  Consider extending <code>--rates</code> to higher values.
</div>
{% endif %}

<h2>Phase Summary</h2>
<div style="overflow-x: auto;">
<table>
  <thead>
    <tr>
      <th>Rate (TPS)</th><th>Offered TPS<br><span style="font-weight:normal;font-size:0.75rem">(spammer local)</span></th><th>Offered B/s<br><span style="font-weight:normal;font-size:0.75rem">(spammer local)</span></th><th>Actual TPS<br><span style="font-weight:normal;font-size:0.75rem">(chain)</span></th><th>Gas/s</th><th>Fill %</th>
      <th>Blk Time</th><th>Build (ms)</th><th>Finalize (ms)</th><th>Avg±SD (ms)</th><th>P50 (ms)</th><th>P95 (ms)</th><th>MemAvg (MiB)</th><th>MemPk (MiB)</th>
      <th>Peak Pend<br><span style="font-weight:normal;font-size:0.75rem">count (MB)</span></th>
      <th>Avg Pend<br><span style="font-weight:normal;font-size:0.75rem">count (MB)</span></th>
      <th>Peak Qued<br><span style="font-weight:normal;font-size:0.75rem">count (MB)</span></th>
      <th>Avg Qued<br><span style="font-weight:normal;font-size:0.75rem">count (MB)</span></th>
      <th>Peak BaseF<br><span style="font-weight:normal;font-size:0.75rem">count (MB)</span></th>
      <th>Avg BaseF<br><span style="font-weight:normal;font-size:0.75rem">count (MB)</span></th>
      <th>PoolEvct</th>
      <th>EL CPU avg</th><th>EL CPU max</th>
      <th>CL CPU avg</th><th>CL CPU max</th><th>Signals</th>
    </tr>
  </thead>
  <tbody>
  {% for phase in phases %}
    <tr{% if phase.is_sat %} class="sat-row"{% endif %}>
      <td>{{ phase.offered_tps }}</td>
      <td>{{ phase.actual_offered_tps }}</td>
      <td>{{ phase.actual_offered_bytes_per_sec }}</td>
      <td>{{ phase.actual_tps }}</td>
      <td>{{ phase.gas_per_sec }}</td>
      <td>{{ phase.fill_pct }}</td>
      <td>{{ phase.avg_block_time_s }}</td>
      <td>{{ phase.avg_block_build_time_ms }}</td>
      <td>{{ phase.avg_block_finalize_time_ms }}</td>
      <td>{{ phase.latency_avg_sd_ms }}</td>
      <td>{{ phase.latency_p50_ms }}</td>
      <td>{{ phase.latency_p95_ms }}</td>
      <td>{{ phase.el_mem_avg_mb }}</td>
      <td>{{ phase.el_mem_peak_mb }}</td>
      <td>{{ phase.peak_pending }}</td>
      <td>{{ phase.avg_pending }}</td>
      <td>{{ phase.peak_queued }}</td>
      <td>{{ phase.avg_queued }}</td>
      <td>{{ phase.peak_basefee }}</td>
      <td>{{ phase.avg_basefee }}</td>
      <td>{{ phase.pool_evictions }}</td>
      <td>{{ phase.el_cpu_avg_pct }}</td>
      <td>{{ phase.el_cpu_max_pct }}</td>
      <td>{{ phase.cl_cpu_avg_pct }}</td>
      <td>{{ phase.cl_cpu_max_pct }}</td>
      <td>
        {%- if phase.signal_labels %}
          {%- for label in phase.signal_labels %}<span class="tag">{{ label }}</span>{% endfor %}
        {%- else %}—{% endif %}
      </td>
    </tr>
  {% endfor %}
  </tbody>
</table>
</div>

{% set phases_with_errors = phases | selectattr("rpc_errors") | list %}
{% if phases_with_errors %}
<details style="margin-top:12px"><summary>RPC Errors by Phase</summary>
<div style="overflow-x:auto; margin-top:8px;">
<table>
  <thead>
    <tr><th>Rate (TPS)</th><th style="text-align:left">Error</th><th>Count</th></tr>
  </thead>
  <tbody>
  {% for phase in phases %}{% for err, count in phase.rpc_errors %}
    <tr{% if phase.is_sat %} class="sat-row"{% endif %}>
      {% if loop.first %}<td rowspan="{{ phase.rpc_errors | length }}" style="vertical-align:top">{{ phase.offered_tps }}</td>{% endif %}
      <td style="text-align:left;font-weight:normal;font-family:monospace;font-size:0.82rem">{{ err }}</td>
      <td>{{ count }}</td>
    </tr>
  {% endfor %}{% endfor %}
  </tbody>
</table>
</div>
</details>
{% endif %}

<h2>Charts</h2>
<div class="chart-grid">
  <div class="chart-box chart-wide"><img src="data:image/png;base64,{{ charts.throughput }}" alt="Throughput"></div>
  <div class="chart-box"><img src="data:image/png;base64,{{ charts.latency }}" alt="Latency"></div>
  <div class="chart-box"><img src="data:image/png;base64,{{ charts.mempool }}" alt="Mempool depth"></div>
  <div class="chart-box"><img src="data:image/png;base64,{{ charts.gas }}" alt="Gas throughput"></div>
  <div class="chart-box"><img src="data:image/png;base64,{{ charts.block_timing }}" alt="Block timing"></div>
  {% if charts.memory %}
  <div class="chart-box"><img src="data:image/png;base64,{{ charts.memory }}" alt="EL memory"></div>
  {% endif %}
  <div class="chart-box chart-wide"><img src="data:image/png;base64,{{ charts.signals }}" alt="Signals heatmap"></div>
</div>

{% if charts.latency_dists %}
<h3>Latency distributions by phase</h3>
<div class="chart-grid-3col">
  {% for rate, b64 in charts.latency_dists %}
  <div class="chart-box"><img src="data:image/png;base64,{{ b64 }}" alt="Latency distribution — {{ rate }} TPS"></div>
  {% endfor %}
</div>
{% endif %}

{% if prom_charts %}
<h2>Prometheus Metrics</h2>
<div class="chart-grid">
  {% for title, b64 in prom_charts.items() %}
  <div class="chart-box{% if title == 'Node color legend' %} chart-wide{% endif %}"><img src="data:image/png;base64,{{ b64 }}" alt="{{ title }}"></div>
  {% endfor %}
</div>
{% endif %}

<h2>Raw Data</h2>
<details><summary>experiment.json</summary>
<pre>{{ raw_json }}</pre>
</details>

</div>
<div class="lightbox" id="lightbox" role="dialog" aria-modal="true">
  <img id="lightbox-img" src="" alt="Enlarged chart">
</div>
<script>
(function () {
  var lb = document.getElementById('lightbox');
  var lbImg = document.getElementById('lightbox-img');
  document.querySelectorAll('.chart-box img').forEach(function (img) {
    img.addEventListener('click', function () {
      lbImg.src = img.src;
      lbImg.alt = img.alt;
      lb.classList.add('open');
    });
  });
  lb.addEventListener('click', function () { lb.classList.remove('open'); });
  document.addEventListener('keydown', function (e) {
    if (e.key === 'Escape') lb.classList.remove('open');
  });
})();
</script>
</body>
</html>
"""


def _build_html(exp: dict, charts: dict, prom_charts: dict[str, str]) -> str:
    phases    = exp.get("phases", [])
    params    = exp.get("parameters", {})
    # New schema (saturation runner ≥ 2026-06-25): manifest TOML embedded as
    # a JSON object under `parameters.manifest`. Old schema: a hand-picked
    # `Topology` struct at `parameters.topology`. Fall back to the old field
    # when the new one is absent so historical experiment.json files still render.
    manifest  = params.get("manifest") or {}
    topology  = params.get("topology") or (_topology_from_manifest(manifest) if manifest else {})
    status    = exp.get("status", {})
    status_t  = status.get("type", "unknown")
    sat_phase = find_saturation_phase(phases)

    started = _parse_rfc3339(exp.get("started_at") or "")
    ended   = _parse_rfc3339(exp.get("ended_at") or "")

    prepared_phases = []
    for phase in phases:
        m = phase.get("metrics", {})
        evictions = m.get("pool_evictions")
        # Reth caps each sub-pool on both count and size; render the cell as
        # ``count(MB)`` so a reader sees both dimensions at a glance and can
        # spot which one is binding when "txpool is full" fires.
        as_f = lambda x: float(x) if x is not None else None  # noqa: E731
        prepared_phases.append({
            "offered_tps":           phase["offered_tps"],
            "actual_offered_tps":    _fmt(m.get("actual_offered_tps"), ".0f"),
            "actual_offered_bytes_per_sec": _fmt_gas(m.get("actual_offered_bytes_per_sec")),
            "actual_tps":            _fmt(m.get("actual_tps"), ".0f"),
            "gas_per_sec":           _fmt_gas(m.get("gas_per_sec")),
            "fill_pct":              _fmt(m.get("fill_pct"), ".1f", "%"),
            "avg_block_time_s":              _fmt(m.get("avg_block_time_s"), ".2f", "s"),
            "avg_block_build_time_ms":       _fmt(m.get("avg_block_build_time_ms"), ".0f"),
            "avg_block_finalize_time_ms":    _fmt(m.get("avg_block_finalize_time_ms"), ".0f"),
            "el_mem_avg_mb":                 _fmt(m.get("el_mem_avg_mb"), ".0f"),
            "el_mem_peak_mb":                _fmt(m.get("el_mem_peak_mb"), ".0f"),
            "latency_avg_sd_ms":     _fmt_avg_sd(m.get("latency_avg_ms"), m.get("latency_stddev_ms")),
            "latency_p50_ms":        _fmt(m.get("latency_p50_ms"), ".0f"),
            "latency_p95_ms":        _fmt(m.get("latency_p95_ms"), ".0f"),
            "peak_pending":   _fmt_count_mb(as_f(m.get("max_mempool")),         m.get("max_pending_size_mb")),
            "avg_pending":    _fmt_count_mb(m.get("avg_pending_mempool"),       m.get("avg_pending_size_mb")),
            "peak_queued":    _fmt_count_mb(as_f(m.get("max_queued_mempool")),  m.get("max_queued_size_mb")),
            "avg_queued":     _fmt_count_mb(m.get("avg_queued_mempool"),        m.get("avg_queued_size_mb")),
            "peak_basefee":   _fmt_count_mb(m.get("max_basefee_mempool"),       m.get("max_basefee_size_mb")),
            "avg_basefee":    _fmt_count_mb(m.get("avg_basefee_mempool"),       m.get("avg_basefee_size_mb")),
            "pool_evictions":        _fmt(evictions, ".0f") if evictions is not None else "—",
            "el_cpu_avg_pct":      _fmt(m.get("el_cpu_avg_pct"), ".0f", "%"),
            "el_cpu_max_pct":      _fmt(m.get("el_cpu_max_pct"), ".0f", "%"),
            "cl_cpu_avg_pct":      _fmt(m.get("cl_cpu_avg_pct"), ".0f", "%"),
            "cl_cpu_max_pct":      _fmt(m.get("cl_cpu_max_pct"), ".0f", "%"),
            "signal_labels":       [SIGNAL_LABELS.get(s, s) for s in phase.get("signals", [])],
            "is_sat":              sat_phase is not None and phase["offered_tps"] == sat_phase["offered_tps"],
            "rpc_errors":          sorted(m.get("rpc_errors", {}).items(), key=lambda kv: -kv[1]),
        })

    ctx = {
        "experiment_id":             exp.get("experiment_id", ""),
        "status_type":               status_t,
        "status_type_display":       status_t.replace("_", " "),
        "status_reason":             status.get("reason", "") if status_t == "failed" else "",
        "started_str":               started.strftime("%Y-%m-%d %H:%M UTC") if started else "",
        "ended_str":                 ended.strftime("%H:%M UTC") if ended else "",
        "duration":                  _fmt_duration((ended - started).total_seconds()) if started and ended else "",
        "params_rates":              ", ".join(str(r) for r in params.get("rates", [])),
        "params_hold":               _fmt_duration(params.get("hold_secs", 0)),
        "params_warmup":             _fmt_duration(params.get("rampup_secs", 0)),
        "params_cooldown":           _fmt_duration(params.get("cooldown_secs", 0)),
        "params_generators":         str(params.get("generators", "")),
        "params_tx_mix":             params.get("tx_mix", ""),
        "params_guzzler_fn_weights": params.get("guzzler_fn_weights", ""),
        "params_erc20_fn_weights":   params.get("erc20_fn_weights", ""),
        "topo_num_nodes":            topology.get("num_nodes", ""),
        "topo_num_validators":       topology.get("num_validators", ""),
        "topo_node_types":           _fmt_node_types(topology.get("nodes_by_type") or {}),
        "topo_node_size":            topology.get("node_size"),
        "topo_node_disk":            _fmt_node_disk(topology),
        "topo_el_resources":         _fmt_container_resources(
                                         topology.get("el_cpu_limit"),
                                         topology.get("el_memory_limit_gb"),
                                     ),
        "topo_cl_resources":         _fmt_container_resources(
                                         topology.get("cl_cpu_limit"),
                                         topology.get("cl_memory_limit_gb"),
                                     ),
        "topo_account_balance":      _fmt_usdc(topology.get("extra_account_balance_usdc")),
        "topo_block_gas_limit":      f"{topology.get('block_gas_limit'):,}" if topology.get("block_gas_limit") else "—",
        "topo_cc_size":              topology.get("cc_size"),
        "topo_cc_disk_gb":           topology.get("cc_disk_gb"),
        "topo_image_el":             topology.get("image_el"),
        "topo_image_cl":             topology.get("image_cl"),
        "topo_el_storage":           "V2" if topology.get("el_storage_v2", True) else "V1",
        "manifest_json":             json.dumps(manifest, indent=2) if manifest else None,
        "sat_tps":                   sat_phase["offered_tps"] if sat_phase else None,
        "sat_signal_labels":         [SIGNAL_LABELS.get(s, s) for s in (sat_phase.get("signals", []) if sat_phase else [])],
        "phases":                    prepared_phases,
        "charts":                    charts,
        "prom_charts":               prom_charts,
        "raw_json":                  json.dumps(exp, indent=2),
    }

    env = Environment(autoescape=True)
    return env.from_string(_TEMPLATE).render(**ctx)

# ── Entry point ───────────────────────────────────────────────────────────────

def parse_args(argv: list[str]) -> argparse.Namespace:
    p = argparse.ArgumentParser(
        description="Generate a self-contained HTML report from a quake saturation experiment."
    )
    p.add_argument("experiment_dir", type=Path, help="Path to the experiment output directory.")
    return p.parse_args(argv)


def main(argv: list[str] = sys.argv[1:]) -> None:
    args = parse_args(argv)
    experiment_dir = args.experiment_dir.resolve()

    exp = load_experiment(experiment_dir)
    phases = exp.get("phases", [])

    if not phases:
        print("warning: no phases in experiment.json — report will be sparse", file=sys.stderr)

    sat_phase = find_saturation_phase(phases)
    sat_idx   = saturation_index(phases, sat_phase)
    exp_start = _parse_rfc3339(exp.get("started_at") or "")

    plt.rcParams.update({"figure.facecolor": "white", "axes.facecolor": "white"})

    charts: dict[str, str] = {
        "throughput": chart_throughput(phases, sat_idx),
        "gas":        chart_gas(phases, sat_idx),
        "latency":    chart_latency(phases, sat_idx),
        "mempool":    chart_mempool(phases, sat_idx),
        "signals":       chart_signals(phases),
        "block_timing":  chart_block_timing(phases, sat_idx),
        "memory":        chart_memory(phases, sat_idx),
    }

    charts["latency_dists"] = chart_latency_dist(experiment_dir, phases)

    # Phase start annotations for Prometheus time-series charts (offset from exp_start)
    phase_starts: list[tuple[float, int]] = []
    if exp_start is not None:
        t0 = exp_start.timestamp()
        for p in phases:
            ps = _parse_rfc3339(p.get("started_at") or "")
            if ps is not None:
                phase_starts.append(((ps.timestamp() - t0) / 60, p["offered_tps"]))

    prom_metrics = load_metrics_tarball(experiment_dir)
    prom_charts: dict[str, str] = {}

    # Stable per-node color assignment used across every Prometheus chart so
    # the standalone legend below matches every line in every graph.
    per_node_series = [
        prom_metrics.get(m) or []
        for m in [
            "reth_process_resident_memory_bytes",
            *[cfg[0] for cfg in _PROM_CHART_CONFIG],
        ]
    ]
    node_colors = _build_node_color_map(per_node_series)

    legend_chart = chart_prom_legend(node_colors)
    if legend_chart:
        prom_charts["Node color legend"] = legend_chart

    eviction_chart = chart_pool_evictions_prom(prom_metrics, exp_start, phase_starts)
    if eviction_chart:
        prom_charts["Pool Evictions by Sub-pool"] = eviction_chart

    memory_prom_chart = chart_memory_prom(prom_metrics, exp_start, phase_starts, node_colors)
    if memory_prom_chart:
        prom_charts["EL Resident Memory (MiB)"] = memory_prom_chart

    for metric_name, title, ylabel, mode in _PROM_CHART_CONFIG:
        series = prom_metrics.get(metric_name)
        if not series:
            continue
        count_series = None
        if mode == "avg":
            # Pair _sum metric with its corresponding _count series
            count_name = metric_name.replace("_sum", "_count")
            count_series = prom_metrics.get(count_name)
            if not count_series:
                # Fall back to block time count as a proxy denominator
                count_series = prom_metrics.get("arc_malachite_app_block_time_count")
        prom_charts[title] = chart_prometheus_metric(
            series, title, ylabel, mode, exp_start, phase_starts, count_series,
            node_colors=node_colors,
        )

    report = _build_html(exp, charts, prom_charts)

    out_path = experiment_dir / "report.html"
    out_path.write_text(report, encoding="utf-8")
    print(f"Report written to {out_path}")


if __name__ == "__main__":
    main()
