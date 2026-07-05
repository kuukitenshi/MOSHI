#!/usr/bin/env python3
"""
plot_cdf.py: Empirical CDF of E2E login latency: demo_app (MOSHI) vs Hellō.

Replaces the mean/median/p95 bar chart: instead of three summary points, the
full distribution of the n runs is shown, with the p95 and the mean marked.

Usage:
  python3 plot_cdf.py --demo  rq/rq1/out/rq1_demo_runs.csv \
                      --hello rq/rq1/out/rq1_hello_runs.csv \
                      --output plots/rq1/rq1_cdf
"""

import argparse
import csv
import math
import os
import sys

try:
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    import matplotlib.ticker as mticker
    import numpy as np
except ImportError:
    print("matplotlib/numpy not installed: skipping plot", file=sys.stderr)
    sys.exit(0)


def load_runs(path):
    """Read elapsed_ms of the successful runs, in seconds."""
    vals = []
    with open(path) as f:
        for row in csv.DictReader(f):
            if row.get('status', 'ok') != 'ok':
                continue
            vals.append(float(row['elapsed_ms']) / 1000.0)
    if not vals:
        sys.exit(f'no successful runs in {path}')
    return np.array(sorted(vals))


def percentile(sorted_vals, q):
    """Nearest-rank percentile: same convention as the *_summary.json files."""
    idx = max(0, math.ceil(q * len(sorted_vals)) - 1)
    return sorted_vals[idx]


def ecdf(sorted_vals):
    """Step coordinates for the empirical CDF, in percent."""
    y = np.arange(1, len(sorted_vals) + 1) / len(sorted_vals) * 100.0
    return sorted_vals, y


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--demo',  required=True)
    p.add_argument('--hello', required=True)
    p.add_argument('--output', default='plots/rq1/rq1_cdf')
    args = p.parse_args()

    d = load_runs(args.demo)
    h = load_runs(args.hello)

    plt.style.use('seaborn-v0_8-whitegrid')
    matplotlib.rcParams.update({
        'font.size':        14,
        'axes.titlesize':   17,
        'axes.labelsize':   15,
        'xtick.labelsize':  14,
        'ytick.labelsize':  14,
        'legend.fontsize':  13,
    })

    COLOR_D = '#2166ac'
    COLOR_H = '#d6604d'

    fig, ax = plt.subplots(figsize=(10, 7))

    for vals, color, name in ((d, COLOR_D, 'MOSHI'), (h, COLOR_H, 'Hellō Broker')):
        x, y = ecdf(vals)
        mean = vals.mean()
        p95  = percentile(vals, 0.95)

        # The curve starts at 0% just below the minimum so the step is closed.
        ax.step(np.concatenate(([x[0]], x)), np.concatenate(([0.0], y)),
                where='post', color=color, linewidth=2.6, zorder=3,
                label=f'{name}  (mean {mean:.2f} s · p95 {p95:.2f} s)')

        # Mean: vertical dashed line.
        ax.axvline(mean, color=color, linestyle=':', linewidth=1.8,
                   alpha=0.85, zorder=2)
        # p95: marker on the curve.
        ax.plot([p95], [95.0], marker='o', markersize=9, color=color,
                markeredgecolor='white', markeredgewidth=1.5, zorder=5)
        ax.annotate(f'p95 = {p95:.2f} s',
                    xy=(p95, 95.0), xytext=(6, -20), textcoords='offset points',
                    fontsize=12, fontweight='bold', color=color)

    # 95% reference line.
    ax.axhline(95.0, color='#666', linestyle='--', linewidth=1.4,
               alpha=0.8, zorder=1)
    ax.annotate('95%', xy=(0.005, 95.0), xycoords=('axes fraction', 'data'),
                xytext=(0, 5), textcoords='offset points',
                fontsize=12, color='#666', fontweight='bold')

    x_lo = min(d[0], h[0])
    x_hi = max(d[-1], h[-1])
    pad  = (x_hi - x_lo) * 0.05
    ax.set_xlim(x_lo - pad, x_hi + pad)
    ax.set_ylim(0, 103)

    ax.set_xlabel('Latency (s)', fontsize=15, labelpad=8)
    ax.set_ylabel('Runs completed (%)', fontsize=15, labelpad=8)
    ax.set_title(f'CDF of E2E Login Latency\n'
                 f'(Client-side · returning user · n={len(d)} · MOSHI net: lan)',
                 fontsize=17, fontweight='bold', pad=14)
    ax.yaxis.set_major_formatter(mticker.FuncFormatter(lambda v, _: f'{v:.0f}'))
    ax.set_yticks([0, 25, 50, 75, 95, 100])
    ax.legend(fontsize=13, framealpha=0.95, loc='lower right')
    ax.set_axisbelow(True)

    fig.text(0.01, -0.03,
             'Dotted vertical lines mark the mean of each distribution.',
             fontsize=16, color='#555', style='italic')

    fig.subplots_adjust(top=0.90, bottom=0.10, left=0.10, right=0.97)

    os.makedirs(os.path.dirname(os.path.abspath(args.output)), exist_ok=True)
    for ext in ['png', 'pdf']:
        out = f'{args.output}.{ext}'
        fig.savefig(out, dpi=150, bbox_inches='tight')
        print(f'Saved: {out}')


if __name__ == '__main__':
    main()
