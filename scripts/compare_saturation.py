#!/usr/bin/env python3
"""Compare two saturation experiments and produce a self-contained HTML report.

Dependencies: matplotlib, jinja2 (pip install matplotlib jinja2).
"""

import base64
import io
import json
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

try:
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
except ImportError:
    sys.exit("matplotlib is required: pip install matplotlib")

try:
    from jinja2 import Environment
    from markupsafe import Markup, escape
except ImportError:
    print("error: jinja2 is required — install with: pip3 install jinja2", file=sys.stderr)
    sys.exit(1)

# ── colours ──────────────────────────────────────────────────────────────────
C_A      = "#4C72B0"   # reth1 (blue)
C_B      = "#DD8452"   # reth2 (orange)
C_GRID   = "#e0e0e0"


# ── helpers ───────────────────────────────────────────────────────────────────

def _fig_to_b64(fig) -> str:
    buf = io.BytesIO()
    fig.savefig(buf, format="png", dpi=130, bbox_inches="tight")
    plt.close(fig)
    buf.seek(0)
    return base64.b64encode(buf.read()).decode()


def _label(image: Optional[str]) -> str:
    if not image:
        return "unknown"
    tag = image.split(":")[-1]
    return tag


def _short(image: Optional[str]) -> str:
    if not image:
        return "unknown"
    return image.split("/")[-1]          # e.g.  arc-execution:reth2


def _leading_alpha_prefix(name: str) -> str:
    chars = []
    for c in name:
        if c.isalpha():
            chars.append(c)
        else:
            break
    return "".join(chars) if chars else name


def _topology_from_manifest(manifest: dict) -> dict:
    """Reconstruct the legacy `Topology` summary fields from an embedded manifest.

    The saturation runner now embeds the full parsed manifest under
    `parameters.manifest` instead of a hand-picked Topology struct. Mirrors
    `_topology_from_manifest` in `saturation_report.py` so both scripts share
    the same fallback behavior.
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


def _fmt_node_disk(topology: dict) -> str:
    """`<size> GiB / <type> / <iops> IOPS`, skipping unset components."""
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


def _fmt_container_resources(cpu, mem_gb) -> str:
    """`<cpu> CPU / <mem> GiB`, skipping unset components."""
    parts = []
    if cpu is not None:
        parts.append(f"{cpu:g} CPU")
    if mem_gb is not None:
        parts.append(f"{mem_gb:g} GiB")
    return " / ".join(parts) if parts else "—"


def _fmt_usdc(v) -> str:
    return f"{v:,} USDC" if v is not None else "—"


def _fmt_node_types(by_type: dict) -> str:
    """`13 sentry, 36 full, 21 validator` — sorted by count descending."""
    if not by_type:
        return "—"
    items = sorted(by_type.items(), key=lambda kv: (-kv[1], kv[0]))
    return ", ".join(f"{count} {name}" for name, count in items)


def _fmt(v, spec=".0f", suffix=""):
    return f"{v:{spec}}{suffix}" if v is not None else "—"


def _fmt_avg_sd(avg, sd):
    if avg is None:
        return "—"
    if sd is None:
        return f"{avg:.0f} ms"
    return f"{avg:.0f}±{sd:.0f} ms"


def _fmt_count_mb(count, size_mb):
    """Render a sub-pool depth cell as ``count (MB)``.

    Reth caps each sub-pool on count and size in MB independently; show both
    so the reader can see which dimension is the binding constraint. Either
    half renders as ``—`` when the experiment.json lacks the field (e.g. older
    runs without the size_bytes Prometheus query).
    """
    if count is None and size_mb is None:
        return "—"
    c = f"{count:.0f}" if count is not None else "—"
    s = f"{size_mb:.1f}" if size_mb is not None else "—"
    return f"{c} ({s})"


def _fmt_gas(v):
    if v is None:
        return "—"
    if v >= 1e6:
        return f"{v/1e6:.1f}M"
    if v >= 1e3:
        return f"{v/1e3:.1f}K"
    return f"{v:.0f}"


SIGNAL_LABELS = {
    "gas_plateaued":    "Gas Plateaued",
    "tps_plateaued":    "TPS Plateaued",
    "tps_ratio_drop":   "TPS Ratio Drop",
    "latency_spike":    "Latency Spike",
    "mempool_growth":   "Mempool Growth",
    "el_cpu_saturated": "EL CPU Saturated",
}

# ── chart helpers ─────────────────────────────────────────────────────────────

def _xs(n):
    return list(range(n))


def _bar_pair(ax, xs, vals_a, vals_b, label_a, label_b, width=0.35):
    xs_a = [x - width/2 for x in xs]
    xs_b = [x + width/2 for x in xs]
    ax.bar(xs_a, vals_a, width, color=C_A, alpha=0.85, label=label_a)
    ax.bar(xs_b, vals_b, width, color=C_B, alpha=0.85, label=label_b)


def _line_pair(ax, xs, vals_a, vals_b, label_a, label_b, marker="o"):
    ax.plot(xs, vals_a, color=C_A, marker=marker, linewidth=2, label=label_a)
    ax.plot(xs, vals_b, color=C_B, marker=marker, linewidth=2, label=label_b)


def _finish(ax, fig, x_labels, title, ylabel, legend=True):
    ax.set_xticks(_xs(len(x_labels)))
    ax.set_xticklabels(x_labels)
    ax.set_xlabel("Offered TPS")
    ax.set_ylabel(ylabel)
    ax.set_title(title)
    ax.grid(axis="y", color=C_GRID)
    if legend:
        ax.legend(fontsize=8)
    fig.tight_layout()


# ── individual charts ─────────────────────────────────────────────────────────

def chart_throughput(phases_a, phases_b, labels, la, lb) -> str:
    xs = _xs(len(phases_a))
    offered = [p["offered_tps"] for p in phases_a]
    tps_a   = [p["metrics"].get("actual_tps") or 0 for p in phases_a]
    tps_b   = [p["metrics"].get("actual_tps") or 0 for p in phases_b]

    fig, ax = plt.subplots(figsize=(8, 3.5))
    _bar_pair(ax, xs, tps_a, tps_b, la, lb)
    ax.plot(xs, offered, color="black", linestyle="--", linewidth=1.2,
            label="Offered TPS", zorder=5)
    _finish(ax, fig, labels, "Actual TPS vs Offered", "TPS")
    return _fig_to_b64(fig)


def chart_gas(phases_a, phases_b, labels, la, lb) -> str:
    xs    = _xs(len(phases_a))
    gas_a = [p["metrics"].get("gas_per_sec") or 0 for p in phases_a]
    gas_b = [p["metrics"].get("gas_per_sec") or 0 for p in phases_b]

    fig, ax = plt.subplots(figsize=(8, 3.5))
    _bar_pair(ax, xs, [g/1e6 for g in gas_a], [g/1e6 for g in gas_b], la, lb)
    _finish(ax, fig, labels, "Gas Throughput", "Gas/s (M)")
    return _fig_to_b64(fig)


def chart_fill(phases_a, phases_b, labels, la, lb) -> str:
    xs     = _xs(len(phases_a))
    fill_a = [p["metrics"].get("fill_pct") or 0 for p in phases_a]
    fill_b = [p["metrics"].get("fill_pct") or 0 for p in phases_b]

    fig, ax = plt.subplots(figsize=(8, 3.5))
    _bar_pair(ax, xs, fill_a, fill_b, la, lb)
    ax.axhline(100, color="red", linestyle="--", linewidth=1, alpha=0.6, label="100% (full blocks)")
    _finish(ax, fig, labels, "Block Fill %", "Fill %")
    return _fig_to_b64(fig)


def chart_latency(phases_a, phases_b, labels, la, lb) -> str:
    xs    = _xs(len(phases_a))
    p50_a = [p["metrics"].get("latency_p50_ms") or 0 for p in phases_a]
    p95_a = [p["metrics"].get("latency_p95_ms") or 0 for p in phases_a]
    p50_b = [p["metrics"].get("latency_p50_ms") or 0 for p in phases_b]
    p95_b = [p["metrics"].get("latency_p95_ms") or 0 for p in phases_b]

    fig, ax = plt.subplots(figsize=(8, 3.5))
    ax.plot(xs, [v/1000 for v in p50_a], color=C_A, marker="o", linewidth=2, label=f"{la} p50")
    ax.plot(xs, [v/1000 for v in p95_a], color=C_A, marker="s", linewidth=2, linestyle="--", label=f"{la} p95")
    ax.plot(xs, [v/1000 for v in p50_b], color=C_B, marker="o", linewidth=2, label=f"{lb} p50")
    ax.plot(xs, [v/1000 for v in p95_b], color=C_B, marker="s", linewidth=2, linestyle="--", label=f"{lb} p95")
    _finish(ax, fig, labels, "Transaction Latency", "Latency (s)")
    return _fig_to_b64(fig)


def chart_mempool(phases_a, phases_b, labels, la, lb) -> str:
    """Compare per-sub-pool peak depth (count) between two experiments.

    Reth caps each sub-pool independently on both count and size — this chart
    surfaces count (the dimension easier to read across phases); the
    accompanying phase-summary table carries the matching MB values so the
    size dimension stays available without doubling the chart count.
    """
    xs = _xs(len(phases_a))

    def get(phases, key):
        return [p["metrics"].get(key) or 0 for p in phases]

    series = [
        ("Pending Pool", "max_mempool", "avg_pending_mempool"),
        ("Queued Pool",  "max_queued_mempool", "avg_queued_mempool"),
        ("Basefee Pool", "max_basefee_mempool", "avg_basefee_mempool"),
    ]
    # Drop sub-pools that stay at zero in both runs — keeps the figure tight.
    active = [
        (title, peak_key, avg_key)
        for title, peak_key, avg_key in series
        if any(v > 0 for v in get(phases_a, peak_key) + get(phases_b, peak_key))
    ] or [series[0]]

    fig, axes = plt.subplots(
        1, len(active), figsize=(4.5 * len(active), 3.5), sharey=False
    )
    if len(active) == 1:
        axes = [axes]

    for ax, (title, peak_key, avg_key) in zip(axes, active):
        pk_a = get(phases_a, peak_key)
        av_a = get(phases_a, avg_key)
        pk_b = get(phases_b, peak_key)
        av_b = get(phases_b, avg_key)
        _bar_pair(ax, xs, pk_a, pk_b, f"{la} peak", f"{lb} peak")
        ax.plot(xs, av_a, color=C_A, marker="o", linewidth=1.5, linestyle=":", label=f"{la} avg")
        ax.plot(xs, av_b, color=C_B, marker="o", linewidth=1.5, linestyle=":", label=f"{lb} avg")
        ax.set_xticks(xs)
        ax.set_xticklabels(labels)
        ax.set_xlabel("Offered TPS")
        ax.set_ylabel("Transactions")
        ax.set_title(title)
        ax.grid(axis="y", color=C_GRID)
        ax.legend(fontsize=7)

    fig.tight_layout()
    return _fig_to_b64(fig)


def chart_cpu(phases_a, phases_b, labels, la, lb) -> str:
    xs        = _xs(len(phases_a))
    avg_cpu_a = [p["metrics"].get("el_cpu_avg_pct") or 0 for p in phases_a]
    max_cpu_a = [p["metrics"].get("el_cpu_max_pct") or 0 for p in phases_a]
    avg_cpu_b = [p["metrics"].get("el_cpu_avg_pct") or 0 for p in phases_b]
    max_cpu_b = [p["metrics"].get("el_cpu_max_pct") or 0 for p in phases_b]

    fig, ax = plt.subplots(figsize=(8, 3.5))
    _bar_pair(ax, xs, avg_cpu_a, avg_cpu_b, f"{la} avg", f"{lb} avg")
    ax.plot(xs, max_cpu_a, color=C_A, marker="^", linewidth=1.5, linestyle="--", label=f"{la} max")
    ax.plot(xs, max_cpu_b, color=C_B, marker="^", linewidth=1.5, linestyle="--", label=f"{lb} max")
    ax.axhline(200, color="grey", linestyle=":", linewidth=1, alpha=0.7, label="200% (2 cores)")
    _finish(ax, fig, labels, "EL CPU Utilisation", "CPU %")
    return _fig_to_b64(fig)


def chart_memory(phases_a, phases_b, labels, la, lb) -> str:
    xs      = _xs(len(phases_a))
    avg_a   = [p["metrics"].get("el_mem_avg_mb") or 0 for p in phases_a]
    avg_b   = [p["metrics"].get("el_mem_avg_mb") or 0 for p in phases_b]
    peak_a  = [p["metrics"].get("el_mem_peak_mb") or 0 for p in phases_a]
    peak_b  = [p["metrics"].get("el_mem_peak_mb") or 0 for p in phases_b]

    fig, axes = plt.subplots(1, 2, figsize=(12, 3.5))
    for ax, vals_a, vals_b, title in [
        (axes[0], avg_a,  avg_b,  "EL Avg Resident Memory (MiB)"),
        (axes[1], peak_a, peak_b, "EL Peak Resident Memory (MiB)"),
    ]:
        _line_pair(ax, xs, vals_a, vals_b, la, lb)
        ax.set_xticks(xs)
        ax.set_xticklabels(labels)
        ax.set_xlabel("Offered TPS")
        ax.set_ylabel("Resident Memory (MiB)")
        ax.set_title(title)
        ax.grid(axis="y", color=C_GRID)
        ax.legend(fontsize=8)
    fig.tight_layout()
    return _fig_to_b64(fig)


def chart_block_timing(phases_a, phases_b, labels, la, lb) -> str:
    xs      = _xs(len(phases_a))
    build_a = [p["metrics"].get("avg_block_build_time_ms") or 0 for p in phases_a]
    build_b = [p["metrics"].get("avg_block_build_time_ms") or 0 for p in phases_b]
    final_a = [p["metrics"].get("avg_block_finalize_time_ms") or 0 for p in phases_a]
    final_b = [p["metrics"].get("avg_block_finalize_time_ms") or 0 for p in phases_b]

    fig, axes = plt.subplots(1, 2, figsize=(12, 3.5))
    for ax, vals_a, vals_b, title in [
        (axes[0], build_a,  build_b,  "Block Build Time (ms)"),
        (axes[1], final_a,  final_b,  "Block Finalize Time (ms)"),
    ]:
        _line_pair(ax, xs, vals_a, vals_b, la, lb)
        ax.set_xticks(xs)
        ax.set_xticklabels(labels)
        ax.set_xlabel("Offered TPS")
        ax.set_ylabel("Time (ms)")
        ax.set_title(title)
        ax.grid(axis="y", color=C_GRID)
        ax.legend(fontsize=8)
    fig.tight_layout()
    return _fig_to_b64(fig)


def chart_block_time(phases_a, phases_b, labels, la, lb) -> str:
    xs  = _xs(len(phases_a))
    bt_a = [p["metrics"].get("avg_block_time_s") or 0 for p in phases_a]
    bt_b = [p["metrics"].get("avg_block_time_s") or 0 for p in phases_b]

    fig, ax = plt.subplots(figsize=(8, 3.5))
    _line_pair(ax, xs, bt_a, bt_b, la, lb)
    _finish(ax, fig, labels, "Average Block Time", "Seconds")
    return _fig_to_b64(fig)


def chart_signals(phases_a, phases_b, labels, la, lb) -> str:
    signal_order = list(SIGNAL_LABELS.keys())
    n_phases = len(phases_a)
    n_sigs   = len(signal_order)

    fig, axes = plt.subplots(1, 2, figsize=(max(8, n_phases*2), 3), sharey=True)

    for ax, phases, title in [(axes[0], phases_a, la), (axes[1], phases_b, lb)]:
        grid = [[1 if s in phase.get("signals", []) else 0
                 for phase in phases]
                for s in signal_order]
        ax.imshow(grid, cmap="YlOrRd", vmin=0, vmax=1, aspect="auto")
        ax.set_xticks(_xs(n_phases))
        ax.set_xticklabels(labels, fontsize=8)
        ax.set_yticks(_xs(n_sigs))
        ax.set_yticklabels([SIGNAL_LABELS[s] for s in signal_order], fontsize=8)
        ax.set_xlabel("Offered TPS")
        ax.set_title(title)

    fig.tight_layout()
    return _fig_to_b64(fig)


# ── phase table ───────────────────────────────────────────────────────────────

def phase_table_rows(phases_a, phases_b) -> "Markup":
    rows = []
    for pa, pb in zip(phases_a, phases_b):
        rate = pa["offered_tps"]
        ma, mb = pa.get("metrics", {}), pb.get("metrics", {})
        sigs_a = " ".join(f'<span class="tag">{SIGNAL_LABELS.get(s, escape(s))}</span>' for s in pa.get("signals", [])) or "—"
        sigs_b = " ".join(f'<span class="tag">{SIGNAL_LABELS.get(s, escape(s))}</span>' for s in pb.get("signals", [])) or "—"

        def row(label, a_raw, b_raw, a_fmt, b_fmt, delta_fn=None):
            delta = ""
            if delta_fn and a_raw is not None and b_raw is not None:
                d = delta_fn(a_raw, b_raw)
                cls = "better" if d > 0 else ("worse" if d < 0 else "")
                sign = "+" if d > 0 else ""
                delta = f'<span class="{cls}">{sign}{d:.1f}%</span>'
            return f"<tr><td>{label}</td><td>{a_fmt}</td><td>{b_fmt}</td><td>{delta}</td></tr>"

        def pct_better_higher(a, b):
            return (b - a) / a * 100 if a else 0
        def pct_better_lower(a, b):
            return (a - b) / a * 100 if a else 0

        otps_a = _fmt(ma.get("actual_offered_tps"), ".0f")
        otps_b = _fmt(mb.get("actual_offered_tps"), ".0f")
        obps_a = _fmt_gas(ma.get("actual_offered_bytes_per_sec"))
        obps_b = _fmt_gas(mb.get("actual_offered_bytes_per_sec"))
        tps_a  = _fmt(ma.get("actual_tps"), ".0f")
        tps_b  = _fmt(mb.get("actual_tps"), ".0f")
        gas_a  = _fmt_gas(ma.get("gas_per_sec"))
        gas_b  = _fmt_gas(mb.get("gas_per_sec"))
        fill_a = _fmt(ma.get("fill_pct"), ".1f", "%")
        fill_b = _fmt(mb.get("fill_pct"), ".1f", "%")
        bld_a  = _fmt(ma.get("avg_block_build_time_ms"), ".0f", " ms")
        bld_b  = _fmt(mb.get("avg_block_build_time_ms"), ".0f", " ms")
        fin_a  = _fmt(ma.get("avg_block_finalize_time_ms"), ".0f", " ms")
        fin_b  = _fmt(mb.get("avg_block_finalize_time_ms"), ".0f", " ms")
        mema_a = _fmt(ma.get("el_mem_avg_mb"), ".0f", " MiB")
        mema_b = _fmt(mb.get("el_mem_avg_mb"), ".0f", " MiB")
        memp_a = _fmt(ma.get("el_mem_peak_mb"), ".0f", " MiB")
        memp_b = _fmt(mb.get("el_mem_peak_mb"), ".0f", " MiB")
        avgsd_a = _fmt_avg_sd(ma.get("latency_avg_ms"), ma.get("latency_stddev_ms"))
        avgsd_b = _fmt_avg_sd(mb.get("latency_avg_ms"), mb.get("latency_stddev_ms"))
        p50_a  = _fmt(ma.get("latency_p50_ms"), ".0f", " ms")
        p50_b  = _fmt(mb.get("latency_p50_ms"), ".0f", " ms")
        p95_a  = _fmt(ma.get("latency_p95_ms"), ".0f", " ms")
        p95_b  = _fmt(mb.get("latency_p95_ms"), ".0f", " ms")
        pkp_a  = _fmt_count_mb(ma.get("max_mempool"),         ma.get("max_pending_size_mb"))
        pkp_b  = _fmt_count_mb(mb.get("max_mempool"),         mb.get("max_pending_size_mb"))
        avp_a  = _fmt_count_mb(ma.get("avg_pending_mempool"), ma.get("avg_pending_size_mb"))
        avp_b  = _fmt_count_mb(mb.get("avg_pending_mempool"), mb.get("avg_pending_size_mb"))
        pkq_a  = _fmt_count_mb(ma.get("max_queued_mempool"),  ma.get("max_queued_size_mb"))
        pkq_b  = _fmt_count_mb(mb.get("max_queued_mempool"),  mb.get("max_queued_size_mb"))
        avq_a  = _fmt_count_mb(ma.get("avg_queued_mempool"),  ma.get("avg_queued_size_mb"))
        avq_b  = _fmt_count_mb(mb.get("avg_queued_mempool"),  mb.get("avg_queued_size_mb"))
        pkbf_a = _fmt_count_mb(ma.get("max_basefee_mempool"), ma.get("max_basefee_size_mb"))
        pkbf_b = _fmt_count_mb(mb.get("max_basefee_mempool"), mb.get("max_basefee_size_mb"))
        avbf_a = _fmt_count_mb(ma.get("avg_basefee_mempool"), ma.get("avg_basefee_size_mb"))
        avbf_b = _fmt_count_mb(mb.get("avg_basefee_mempool"), mb.get("avg_basefee_size_mb"))
        ca_a   = _fmt(ma.get("el_cpu_avg_pct"), ".0f", "%")
        ca_b   = _fmt(mb.get("el_cpu_avg_pct"), ".0f", "%")
        cm_a   = _fmt(ma.get("el_cpu_max_pct"), ".0f", "%")
        cm_b   = _fmt(mb.get("el_cpu_max_pct"), ".0f", "%")
        cca_a  = _fmt(ma.get("cl_cpu_avg_pct"), ".0f", "%")
        cca_b  = _fmt(mb.get("cl_cpu_avg_pct"), ".0f", "%")
        ccm_a  = _fmt(ma.get("cl_cpu_max_pct"), ".0f", "%")
        ccm_b  = _fmt(mb.get("cl_cpu_max_pct"), ".0f", "%")

        rows.append(f"""
<tr class="rate-header"><td colspan="4"><strong>{rate} TPS offered</strong></td></tr>
{row("Offered TPS (spammer)", ma.get("actual_offered_tps"), mb.get("actual_offered_tps"), otps_a, otps_b, pct_better_higher)}
{row("Offered B/s (spammer)", ma.get("actual_offered_bytes_per_sec"), mb.get("actual_offered_bytes_per_sec"), obps_a, obps_b, pct_better_higher)}
{row("Actual TPS",   ma.get("actual_tps"),        mb.get("actual_tps"),        tps_a,  tps_b,  pct_better_higher)}
{row("Gas/s",        ma.get("gas_per_sec"),        mb.get("gas_per_sec"),        gas_a,  gas_b)}
{row("Fill %",        ma.get("fill_pct"),                   mb.get("fill_pct"),                   fill_a, fill_b)}
{row("Build time",    ma.get("avg_block_build_time_ms"),    mb.get("avg_block_build_time_ms"),    bld_a,  bld_b,  pct_better_lower)}
{row("Finalize time", ma.get("avg_block_finalize_time_ms"), mb.get("avg_block_finalize_time_ms"), fin_a,  fin_b,  pct_better_lower)}
{row("Avg±SD latency", ma.get("latency_avg_ms"),   mb.get("latency_avg_ms"),     avgsd_a, avgsd_b, pct_better_lower)}
{row("p50 latency",  ma.get("latency_p50_ms"),     mb.get("latency_p50_ms"),     p50_a,  p50_b,  pct_better_lower)}
{row("p95 latency",  ma.get("latency_p95_ms"),     mb.get("latency_p95_ms"),     p95_a,  p95_b,  pct_better_lower)}
{row("Peak pending (count, MB)", ma.get("max_mempool"),         mb.get("max_mempool"),         pkp_a,  pkp_b)}
{row("Avg pending (count, MB)",  ma.get("avg_pending_mempool"), mb.get("avg_pending_mempool"), avp_a,  avp_b)}
{row("Peak queued (count, MB)",  ma.get("max_queued_mempool"),  mb.get("max_queued_mempool"),  pkq_a,  pkq_b)}
{row("Avg queued (count, MB)",   ma.get("avg_queued_mempool"),  mb.get("avg_queued_mempool"),  avq_a,  avq_b)}
{row("Peak basefee (count, MB)", ma.get("max_basefee_mempool"), mb.get("max_basefee_mempool"), pkbf_a, pkbf_b)}
{row("Avg basefee (count, MB)",  ma.get("avg_basefee_mempool"), mb.get("avg_basefee_mempool"), avbf_a, avbf_b)}
{row("EL CPU avg",   ma.get("el_cpu_avg_pct"),     mb.get("el_cpu_avg_pct"),     ca_a,   ca_b)}
{row("EL CPU max",   ma.get("el_cpu_max_pct"),     mb.get("el_cpu_max_pct"),     cm_a,   cm_b)}
{row("CL CPU avg",   ma.get("cl_cpu_avg_pct"),     mb.get("cl_cpu_avg_pct"),     cca_a,  cca_b)}
{row("CL CPU max",   ma.get("cl_cpu_max_pct"),     mb.get("cl_cpu_max_pct"),     ccm_a,  ccm_b)}
{row("EL mem avg",   ma.get("el_mem_avg_mb"),      mb.get("el_mem_avg_mb"),      mema_a, mema_b, pct_better_lower)}
{row("EL mem peak",  ma.get("el_mem_peak_mb"),     mb.get("el_mem_peak_mb"),     memp_a, memp_b, pct_better_lower)}
<tr><td>Signals</td><td>{sigs_a}</td><td>{sigs_b}</td><td></td></tr>
""")
    return Markup("\n".join(rows))


# ── HTML template ─────────────────────────────────────────────────────────────

_TEMPLATE = """\
<!DOCTYPE html>
<html lang="en">
<head><meta charset="utf-8"><title>{{ title }}</title>
<style>
body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
       max-width: 1100px; margin: 40px auto; padding: 0 20px; color: #222; }
h1   { font-size: 1.4rem; border-bottom: 2px solid #ddd; padding-bottom: 8px; }
h2   { font-size: 1.1rem; margin-top: 2rem; color: #444; }
table { border-collapse: collapse; width: 100%; font-size: 0.85rem; margin-top: 0.5rem; }
th, td { padding: 6px 10px; text-align: left; border-bottom: 1px solid #eee; }
th { background: #f5f5f5; font-weight: 600; }
tr.rate-header td { background: #e8f0fe; font-weight: 700; padding-top: 12px; }
.tag { background: #fee2b3; border-radius: 3px; padding: 1px 5px; font-size: 0.78rem;
       margin-right: 3px; white-space: nowrap; }
.better { color: #2a7a2a; font-weight: 600; }
.worse  { color: #c0392b; font-weight: 600; }
.legend-a { color: #4C72B0; font-weight: 700; }
.legend-b { color: #DD8452; font-weight: 700; }
.chart-row { display: flex; flex-wrap: wrap; gap: 16px; margin-top: 8px; }
.chart-box { flex: 1 1 45%; min-width: 300px; }
img { border: 1px solid #eee; border-radius: 4px; max-width: 100%; }
</style>
</head>
<body>
<h1>{{ title }}</h1>

<table>
  <tr><th></th><th class="legend-a">A — {{ label_a }}</th><th class="legend-b">B — {{ label_b }}</th></tr>
  <tr><td>EL image</td><td><code>{{ image_a }}</code></td><td><code>{{ image_b }}</code></td></tr>
  <tr><td>Node types</td><td>{{ node_types_a }}</td><td>{{ node_types_b }}</td></tr>
  <tr><td>Node size</td><td><code>{{ node_size_a }}</code></td><td><code>{{ node_size_b }}</code></td></tr>
  <tr><td>Node disk</td><td>{{ node_disk_a }}</td><td>{{ node_disk_b }}</td></tr>
  <tr><td>EL resources</td><td>{{ el_resources_a }}</td><td>{{ el_resources_b }}</td></tr>
  <tr><td>CL resources</td><td>{{ cl_resources_a }}</td><td>{{ cl_resources_b }}</td></tr>
  <tr><td>Prefund / account</td><td>{{ prefund_a }}</td><td>{{ prefund_b }}</td></tr>
  <tr><td>Block gas limit</td><td>{{ block_gas_a }}</td><td>{{ block_gas_b }}</td></tr>
  <tr><td>Experiment</td><td>{{ exp_id_a }}</td><td>{{ exp_id_b }}</td></tr>
  <tr><td>Duration</td><td>{{ duration_a }}</td><td>{{ duration_b }}</td></tr>
  <tr><td>Phases</td><td>{{ rates_a }}</td><td>{{ rates_b }}</td></tr>
  <tr><td>Hold / Ramp / Cool</td><td>{{ hold_a }}s / {{ ramp_a }}s / {{ cool_a }}s</td><td>{{ hold_b }}s / {{ ramp_b }}s / {{ cool_b }}s</td></tr>
  <tr><td>Generators</td><td>{{ gen_a }}</td><td>{{ gen_b }}</td></tr>
  <tr><td>TX mix</td><td><code>{{ mix_a }}</code></td><td><code>{{ mix_b }}</code></td></tr>
  <tr><td>EL storage</td><td>{{ storage_a }}</td><td>{{ storage_b }}</td></tr>
</table>

<h2>Phase Summary</h2>
<table>
  <thead>
    <tr><th>Metric</th><th class="legend-a">A — {{ label_a }}</th><th class="legend-b">B — {{ label_b }}</th><th>Δ (B vs A)</th></tr>
  </thead>
  <tbody>
    {{ phase_rows }}
  </tbody>
</table>

<h2>Charts</h2>
<div class="chart-row">
  <div class="chart-box"><img src="data:image/png;base64,{{ charts.throughput }}" alt="Throughput"></div>
  <div class="chart-box"><img src="data:image/png;base64,{{ charts.gas }}" alt="Gas throughput"></div>
</div>
<div class="chart-row">
  <div class="chart-box"><img src="data:image/png;base64,{{ charts.fill }}" alt="Block fill"></div>
  <div class="chart-box"><img src="data:image/png;base64,{{ charts.block_time }}" alt="Block time"></div>
</div>
<div class="chart-row">
  <div class="chart-box"><img src="data:image/png;base64,{{ charts.latency }}" alt="Latency"></div>
  <div class="chart-box"><img src="data:image/png;base64,{{ charts.cpu }}" alt="CPU"></div>
</div>
<div class="chart-row">
  <div class="chart-box" style="flex:1 1 90%;"><img src="data:image/png;base64,{{ charts.mempool }}" alt="Mempool"></div>
</div>
<div class="chart-row">
  <div class="chart-box" style="flex:1 1 90%;"><img src="data:image/png;base64,{{ charts.block_timing }}" alt="Block build &amp; finalize time"></div>
</div>
<div class="chart-row">
  <div class="chart-box" style="flex:1 1 90%;"><img src="data:image/png;base64,{{ charts.memory }}" alt="EL resident memory"></div>
</div>
<div class="chart-row">
  <div class="chart-box" style="flex:1 1 90%;"><img src="data:image/png;base64,{{ charts.signals }}" alt="Signals"></div>
</div>

</body>
</html>
"""


# ── main ──────────────────────────────────────────────────────────────────────

def _duration(exp):
    fmt = "%Y-%m-%dT%H:%M:%S.%fZ"
    try:
        s = datetime.strptime(exp["started_at"], fmt).replace(tzinfo=timezone.utc)
        e = datetime.strptime(exp["ended_at"],   fmt).replace(tzinfo=timezone.utc)
        secs = int((e - s).total_seconds())
        return f"{secs//60}m {secs%60}s"
    except Exception:
        return "—"


def main():
    import argparse

    parser = argparse.ArgumentParser(
        description="Compare two saturation experiments and write a self-contained HTML report.",
    )
    parser.add_argument("exp_dir_a", type=Path, help="First experiment directory")
    parser.add_argument("exp_dir_b", type=Path, help="Second experiment directory")
    parser.add_argument("output", type=Path, help="Output HTML path")
    parser.add_argument(
        "--label-a",
        help="Display label for experiment A (defaults to its EL image tag)",
    )
    parser.add_argument(
        "--label-b",
        help="Display label for experiment B (defaults to its EL image tag)",
    )
    args = parser.parse_args()

    dir_a = args.exp_dir_a
    dir_b = args.exp_dir_b
    out = args.output

    with open(dir_a / "experiment.json") as f:
        exp_a = json.load(f)
    with open(dir_b / "experiment.json") as f:
        exp_b = json.load(f)

    def _topo(params: dict) -> dict:
        t = params.get("topology")
        if t:
            return t
        return _topology_from_manifest(params.get("manifest") or {})

    topo_a = _topo(exp_a["parameters"])
    topo_b = _topo(exp_b["parameters"])
    p_a    = exp_a["parameters"]
    p_b    = exp_b["parameters"]

    label_a = args.label_a or _label(topo_a.get("image_el"))
    label_b = args.label_b or _label(topo_b.get("image_el"))

    # Align phases by offered_tps so reports work across runs that swept
    # different rate sets (e.g. baseline included 2000 TPS, hardware-varied
    # runs stopped at 1800).
    rates_a = {p["offered_tps"]: p for p in exp_a["phases"]}
    rates_b = {p["offered_tps"]: p for p in exp_b["phases"]}
    common  = sorted(set(rates_a) & set(rates_b))
    if len(common) != len(rates_a) or len(common) != len(rates_b):
        print(f"  note: aligning on common rates {common}; "
              f"a had {sorted(rates_a)}, b had {sorted(rates_b)}", flush=True)
    phases_a = [rates_a[r] for r in common]
    phases_b = [rates_b[r] for r in common]
    x_labels = [str(r) for r in common]

    print("Rendering charts...", flush=True)
    charts = {
        "throughput": chart_throughput(phases_a, phases_b, x_labels, label_a, label_b),
        "gas":        chart_gas(phases_a, phases_b, x_labels, label_a, label_b),
        "fill":       chart_fill(phases_a, phases_b, x_labels, label_a, label_b),
        "latency":    chart_latency(phases_a, phases_b, x_labels, label_a, label_b),
        "mempool":    chart_mempool(phases_a, phases_b, x_labels, label_a, label_b),
        "cpu":        chart_cpu(phases_a, phases_b, x_labels, label_a, label_b),
        "block_time":   chart_block_time(phases_a, phases_b, x_labels, label_a, label_b),
        "block_timing": chart_block_timing(phases_a, phases_b, x_labels, label_a, label_b),
        "memory":       chart_memory(phases_a, phases_b, x_labels, label_a, label_b),
        "signals":      chart_signals(phases_a, phases_b, x_labels, label_a, label_b),
    }

    ctx = dict(
        title=f"Saturation Experiment Comparison: {label_a} vs {label_b}",
        label_a=label_a, label_b=label_b,
        image_a=_short(topo_a.get("image_el")), image_b=_short(topo_b.get("image_el")),
        node_size_a=topo_a.get("node_size") or "—", node_size_b=topo_b.get("node_size") or "—",
        node_types_a=_fmt_node_types(topo_a.get("nodes_by_type") or {}),
        node_types_b=_fmt_node_types(topo_b.get("nodes_by_type") or {}),
        node_disk_a=_fmt_node_disk(topo_a), node_disk_b=_fmt_node_disk(topo_b),
        el_resources_a=_fmt_container_resources(topo_a.get("el_cpu_limit"), topo_a.get("el_memory_limit_gb")),
        el_resources_b=_fmt_container_resources(topo_b.get("el_cpu_limit"), topo_b.get("el_memory_limit_gb")),
        cl_resources_a=_fmt_container_resources(topo_a.get("cl_cpu_limit"), topo_a.get("cl_memory_limit_gb")),
        cl_resources_b=_fmt_container_resources(topo_b.get("cl_cpu_limit"), topo_b.get("cl_memory_limit_gb")),
        prefund_a=_fmt_usdc(topo_a.get("extra_account_balance_usdc")),
        prefund_b=_fmt_usdc(topo_b.get("extra_account_balance_usdc")),
        block_gas_a=topo_a.get("block_gas_limit") or "—",
        block_gas_b=topo_b.get("block_gas_limit") or "—",
        exp_id_a=exp_a["experiment_id"], exp_id_b=exp_b["experiment_id"],
        duration_a=_duration(exp_a), duration_b=_duration(exp_b),
        rates_a=", ".join(str(p["offered_tps"]) for p in phases_a),
        rates_b=", ".join(str(p["offered_tps"]) for p in phases_b),
        hold_a=p_a.get("hold_secs", "—"), hold_b=p_b.get("hold_secs", "—"),
        ramp_a=p_a.get("rampup_secs", "—"), ramp_b=p_b.get("rampup_secs", "—"),
        cool_a=p_a.get("cooldown_secs", "—"), cool_b=p_b.get("cooldown_secs", "—"),
        gen_a=p_a.get("generators", "—"), gen_b=p_b.get("generators", "—"),
        mix_a=p_a.get("tx_mix", "—"), mix_b=p_b.get("tx_mix", "—"),
        storage_a="V2" if topo_a.get("el_storage_v2", True) else "V1",
        storage_b="V2" if topo_b.get("el_storage_v2", True) else "V1",
        phase_rows=phase_table_rows(phases_a, phases_b),
        charts=charts,
    )
    env = Environment(autoescape=True)
    html = env.from_string(_TEMPLATE).render(**ctx)

    out.write_text(html)
    print(f"Written: {out}")


if __name__ == "__main__":
    main()
