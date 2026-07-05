#!/usr/bin/env python3
"""
plot_quorum.py: RQ2: FROST quorum scalability (latency vs n,t configuration).

Two panels:
  Left: Absolute latency (ms): bars + P95/P99 markers + trend line
  Right: Relative overhead normalised to the (3,2) baseline + protocol messages

Data source (in order of preference):
  1. rq/rq2/out/bench_frost_results.json  (produced by run_rq2.sh)
  2. Hardcoded fallback values

Usage:
  python3 rq/rq2/plot_quorum.py
  python3 rq/rq2/plot_quorum.py --input rq/rq2/out/bench_frost_results.json
"""

import argparse
import json
import sys
from pathlib import Path
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt
import matplotlib.lines as mlines
import matplotlib.patches as mpatches
import numpy as np

# ── Hardcoded fallback (µs → ms conversion) ──────────────────────────────────
FALLBACK = [
    {"label": "n=3, t=2", "n": 3, "t": 2,
     "mean": 0.5014, "median": 0.4978, "p95": 0.5222, "p99": 0.5328},
    {"label": "n=5, t=3", "n": 5, "t": 3,
     "mean": 0.6942, "median": 0.6919, "p95": 0.7268, "p99": 0.7441},
    {"label": "n=7, t=5", "n": 7, "t": 5,
     "mean": 1.1768, "median": 1.1731, "p95": 1.2206, "p99": 1.2548},
]

def load_from_json(path: str) -> list[dict]:
    """Load FROST configs from bench_frost_results.json (values in µs → convert to ms)."""
    with open(path) as f:
        data = json.load(f)
    configs = []
    for s in data.get("frost", []):
        label = s["label"]
        # Extract n and t from label like "FROST Ed25519 (n=3, t=2)"
        import re
        m = re.search(r'n=(\d+),\s*t=(\d+)', label)
        if not m:
            continue
        n, t = int(m.group(1)), int(m.group(2))
        configs.append({
            "label":  f"n={n}, t={t}",
            "n": n, "t": t,
            "mean":   s["mean_us"]   / 1000,
            "median": s["median_us"] / 1000,
            "p95":    s["p95_us"]    / 1000,
            "p99":    s["p99_us"]    / 1000,
        })
    return configs if configs else FALLBACK

p = argparse.ArgumentParser()
script_dir = Path(__file__).resolve().parent
default_json = str(script_dir / "out" / "bench_frost_results.json")
p.add_argument("--input", default=default_json)
p.add_argument("--output", default=str(script_dir.parent.parent / "plots" / "rq2" / "rq2_quorum_scalability"))
args = p.parse_args()

if Path(args.input).exists():
    CONFIGS = load_from_json(args.input)
    print(f"[plot_quorum] Loaded data from {args.input}")
else:
    CONFIGS = FALLBACK
    print(f"[plot_quorum] Using hardcoded data (run ./rq/rq2/run_rq2.sh to get real data)")

# Iterations / repeats for the subtitle (read straight from the results file).
META = {}
if Path(args.input).exists():
    try:
        META = json.load(open(args.input))
    except Exception:
        META = {}
N_ITER = META.get("iterations", 1000)
N_REPS = META.get("repeats")

# Theoretical messages per signing ceremony: 4×t (2 rounds × 2t)
for c in CONFIGS:
    c["messages"] = 4 * c["t"]

BASE = CONFIGS[0]["median"]
COLORS = ["#90caf9", "#1565c0", "#0d2f6b"]   # light → dark blue

x = np.arange(len(CONFIGS))
labels = [c["label"] for c in CONFIGS]

plt.style.use("seaborn-v0_8-whitegrid")
fig, ax_abs = plt.subplots(figsize=(8.5, 5.5))

# ── Latency bars (median) + P95/P99 markers + trend ────────────────────────────
bar_w = 0.45
medians = np.array([c["median"]   for c in CONFIGS])
p95s   = np.array([c["p95"]      for c in CONFIGS])
p99s   = np.array([c["p99"]      for c in CONFIGS])
msgs   = np.array([c["messages"] for c in CONFIGS])

for i, (cfg, col) in enumerate(zip(CONFIGS, COLORS)):
    # Bar (median)
    ax_abs.bar(x[i], cfg["median"], bar_w, color=col, zorder=3,
               edgecolor="white", linewidth=0.8)

    # P95 / P99 markers
    ax_abs.plot([x[i] - bar_w/2, x[i] + bar_w/2], [cfg["p95"], cfg["p95"]],
                color="black", linewidth=1.3, linestyle="--", zorder=5)
    ax_abs.plot([x[i] - bar_w/2, x[i] + bar_w/2], [cfg["p99"], cfg["p99"]],
                color="black", linewidth=1.3, linestyle=":",  zorder=5)

    # Median value label
    ax_abs.text(x[i], cfg["p99"] + 0.06, f'{cfg["median"]:.3f} ms',
                ha="center", va="bottom", fontsize=11,
                fontweight="bold", color=col)

    # Relative overhead annotation inside bar
    rel = cfg["median"] / BASE
    ax_abs.text(x[i], cfg["median"] / 2,
                f'×{rel:.2f}' if i > 0 else 'baseline',
                ha="center", va="center", fontsize=12,
                color="white", fontweight="bold")

# Latency trend through medians
ax_abs.plot(x, medians, color="#333", linewidth=1.4,
            marker="o", markersize=5, markerfacecolor="white",
            markeredgecolor="#333", markeredgewidth=1.5,
            zorder=6, linestyle="-")

ax_abs.set_xticks(x)
ax_abs.set_xticklabels(labels, fontsize=12)
ax_abs.set_xlabel("Quorum configuration (n total nodes, t threshold)", fontsize=12)
ax_abs.set_ylabel("Signing latency (ms)", fontsize=13)
ax_abs.tick_params(axis='y', labelsize=11)
ylim_top = p99s.max() * 1.55
ax_abs.set_ylim(0, ylim_top)
ax_abs.yaxis.grid(True, alpha=0.35)
ax_abs.set_axisbelow(True)

# ── Protocol-message count per signing, as labels lifted clear of each bar ─────
#    (no line, no extra axis: just the count per configuration, placed well
#    above the bar + latency label so the numbers are never covered)
for xi, cfg, m in zip(x, CONFIGS, msgs):
    ax_abs.text(xi, cfg["p99"] + ylim_top * 0.13, f'{m} msgs',
                ha="center", va="bottom", fontsize=12,
                color="#c0392b", fontweight="bold")

# ── Legend ──────────────────────────────────────────────────────────────────────
leg = [
    mlines.Line2D([], [], color="#555", marker="o", markersize=5,
                  markerfacecolor="white", markeredgecolor="#555",
                  label="Median latency (trend)"),
    mlines.Line2D([], [], color="black", linestyle="--", linewidth=1.2, label="P95"),
    mlines.Line2D([], [], color="black", linestyle=":",  linewidth=1.2, label="P99"),
    mpatches.Patch(color="#c0392b", label="Protocol messages (4 per signer)"),
]
ax_abs.legend(handles=leg, fontsize=11, loc="upper left", framealpha=0.9)

fig.suptitle("FROST Quorum Scalability: CPU Signing Latency",
             fontsize=15, fontweight="bold")
if N_REPS:
    _sub = (f"{N_REPS} runs × {N_ITER} iterations  ·  bar = median, "
            f"P95/P99 markers  ·  ×N = overhead vs (3,2) baseline")
else:
    _sub = (f"n = {N_ITER} iterations  ·  bar = median, "
            f"P95/P99 markers  ·  ×N = overhead vs (3,2) baseline")
ax_abs.set_title(_sub, fontsize=12, color="#444", pad=8)

fig.tight_layout()
out_path = Path(args.output)
out_path.parent.mkdir(parents=True, exist_ok=True)
for ext in ["png", "pdf"]:
    p = Path(f"{args.output}.{ext}")
    fig.savefig(p, dpi=300, bbox_inches="tight")
    print(f"Saved: {p}")
