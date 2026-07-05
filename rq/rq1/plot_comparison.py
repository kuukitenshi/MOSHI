#!/usr/bin/env python3
"""
plot_comparison.py: Bar chart comparing E2E latency of demo_app vs Hellō playground.

Usage:
  python3 plot_comparison.py --demo  rq/rq1/out/rq1_demo_summary.json \
                             --hello rq/rq1/out/rq1_hello_summary.json \
                             --output plots/rq1/rq1_comparison
"""

import argparse
import json
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


def load(path):
    with open(path) as f:
        return json.load(f)


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--demo',   required=True)
    p.add_argument('--hello',  required=True)
    p.add_argument('--idp-ms', type=float, default=0.0,
                   help='Optional IdP (Google) overhead in milliseconds to show as stacked component')
    p.add_argument('--output', default='plots/rq1/rq1_comparison')
    args = p.parse_args()

    d = load(args.demo)
    h = load(args.hello)

    plt.style.use('seaborn-v0_8-whitegrid')
    matplotlib.rcParams.update({
        'font.size':        14,
        'axes.titlesize':   17,
        'axes.labelsize':   15,
        'xtick.labelsize':  14,
        'ytick.labelsize':  14,
        'legend.fontsize':  13,
    })

    metrics    = ['mean', 'median', 'p95']
    labels     = ['Mean', 'Median', 'P95']
    demo_vals  = [d[m] / 1000 for m in metrics]
    hello_vals = [h[m] / 1000 for m in metrics]
    d_std      = d.get('stddev', 0) / 1000
    h_std      = h.get('stddev', 0) / 1000
    idp_s      = float(args.idp_ms) / 1000.0 if args.idp_ms and args.idp_ms > 0 else 0.0

    x     = np.arange(len(labels))
    width = 0.32

    fig, ax = plt.subplots(figsize=(10, 7))

    COLOR_D = '#2166ac'
    COLOR_H = '#d6604d'
    # If an IdP overhead is provided, split Hellō mean into base + idp (stacked)
    if idp_s > 0:
        hello_base_vals = hello_vals.copy()
        hello_idp_vals = [0.0, 0.0, 0.0]
        # apply IdP only to Mean (index 0) for clarity
        hello_idp_vals[0] = min(idp_s, hello_vals[0])
        hello_base_vals[0] = max(0.0, hello_vals[0] - hello_idp_vals[0])

        bars_d = ax.bar(x - width / 2, demo_vals, width,
                        label=f'MOSHI',
                        color=COLOR_D, alpha=0.88, zorder=3,
                        yerr=[d_std, 0, 0], capsize=6,
                        error_kw=dict(ecolor=COLOR_D, elinewidth=2, capthick=2))

        bars_h_base = ax.bar(x + width / 2, hello_base_vals, width,
                              label=f'Hellō App',
                              color=COLOR_H, alpha=0.92, zorder=3,
                              yerr=[0, 0, 0], capsize=6)

        # IdP segment should be red (external Google)
        IDP_COLOR = "#e3a4a5"
        bars_h_idp = ax.bar(x + width / 2, hello_idp_vals, width,
                             bottom=hello_base_vals,
                             label=f'Google IdP (≈{idp_s:.2f}s)',
                             color=IDP_COLOR, alpha=0.95, zorder=4,
                             yerr=[h_std, 0, 0], capsize=6,
                             # error bar for Hello mean should use Hello's color
                             error_kw=dict(ecolor=COLOR_H, elinewidth=2, capthick=2))

        # unify bar containers for later annotation
        bars_demo_containers = (bars_d, None)
        bars_hello_containers = (bars_h_base, bars_h_idp)
    else:
        bars_d = ax.bar(x - width / 2, demo_vals, width,
                        label=f'MOSHI',
                        color=COLOR_D, alpha=0.88, zorder=3,
                        yerr=[d_std, 0, 0], capsize=6,
                        error_kw=dict(ecolor=COLOR_D, elinewidth=2, capthick=2))
        bars_h = ax.bar(x + width / 2, hello_vals, width,
                        label=f'Hellō Broker',
                        color=COLOR_H, alpha=0.88, zorder=3,
                        yerr=[h_std, 0, 0], capsize=6,
                        error_kw=dict(ecolor=COLOR_H, elinewidth=2, capthick=2))

        bars_demo_containers = (bars_d, None)

    # Y-axis top for annotation placement
    # compute y max considering stacked Hello component when present
    total_demo_mean = demo_vals[0]
    total_hello_mean = hello_vals[0]
    if idp_s > 0:
        total_hello_mean = hello_base_vals[0] + hello_idp_vals[0]

    y_max = max(total_demo_mean + d_std, total_hello_mean + h_std,
                max(demo_vals), max(hello_vals)) * 1.18

    # Value labels above each bar (placed above error bar when present)
    # Annotate values above each visible bar segment (use bar.get_y()+height)
    # Build a list of items with explicit indices so we can robustly identify the mean bars
    items = []  # each item: (bar, err, is_demo_mean, is_hello_mean)
    if idp_s > 0:
        # demo bars (indices correspond to metrics order)
        for idx, bar in enumerate(bars_demo_containers[0]):
            is_demo_mean = (idx == 0)
            items.append((bar, d_std if is_demo_mean else 0, is_demo_mean, False))
        # hello idp bars (mean is the first idp segment; skip base bars, only show on IdP)
        for idx, bar in enumerate(bars_hello_containers[1]):
            is_hello_mean = (idx == 0)
            items.append((bar, h_std if is_hello_mean else 0, False, is_hello_mean))
    else:
        for idx, bar in enumerate(bars_demo_containers[0]):
            is_demo_mean = (idx == 0)
            items.append((bar, d_std if is_demo_mean else 0, is_demo_mean, False))
        for idx, bar in enumerate(bars_h):
            is_hello_mean = (idx == 0)
            items.append((bar, h_std if is_hello_mean else 0, False, is_hello_mean))

    for bar, err, is_demo_mean, is_hello_mean in items:
        val = bar.get_y() + bar.get_height()
        y_top = val + err
        if is_demo_mean or is_hello_mean:
            # place the total just above the top of the error bar (both means alike)
            y_offset = y_max * 0.02
        else:
            y_offset = max(y_max * 0.02, err + y_max * 0.005)
        # For Hello mean with IdP, show the original measured value (not the stacked total)
        if is_hello_mean and idp_s > 0:
            display_val = hello_vals[0]
        else:
            display_val = val
        ax.text(bar.get_x() + bar.get_width() / 2,
                y_top + y_offset,
                f'{display_val:.2f} s',
                ha='center', va='bottom', fontsize=13, fontweight='bold',
                color='#222')

    # σ label inside the mean bars
    if idp_s > 0:
        # Hello top segment contains the mean top
        hello_mean_top_bar = bars_hello_containers[1][0]
        hello_mean_top_height = hello_mean_top_bar.get_y() + hello_mean_top_bar.get_height()
        if h_std > 0:
            ax.text(hello_mean_top_bar.get_x() + hello_mean_top_bar.get_width() / 2,
                    hello_mean_top_height * 0.06,
                    f'σ = {h_std:.2f} s',
                    ha='center', va='bottom', fontsize=12,
                    color='white', fontstyle='italic', fontweight='bold')
        demo_mean_bar = bars_demo_containers[0][0]
        if d_std > 0:
            ax.text(demo_mean_bar.get_x() + demo_mean_bar.get_width() / 2,
                    demo_mean_bar.get_height() * 0.06,
                    f'σ = {d_std:.2f} s',
                    ha='center', va='bottom', fontsize=12,
                    color='white', fontstyle='italic', fontweight='bold')
    else:
        demo_mean_bar = bars_demo_containers[0][0]
        if d_std > 0:
            ax.text(demo_mean_bar.get_x() + demo_mean_bar.get_width() / 2,
                    demo_mean_bar.get_height() * 0.06,
                    f'σ = {d_std:.2f} s',
                    ha='center', va='bottom', fontsize=12,
                    color='white', fontstyle='italic', fontweight='bold')
        hello_mean_bar = bars_h[0]
        if h_std > 0:
            ax.text(hello_mean_bar.get_x() + hello_mean_bar.get_width() / 2,
                    hello_mean_bar.get_height() * 0.06,
                    f'σ = {h_std:.2f} s',
                    ha='center', va='bottom', fontsize=12,
                    color='white', fontstyle='italic', fontweight='bold')

    # Secondary label for Hello mean showing total with IdP (when stacking is enabled)
    if idp_s > 0:
        try:
            hello_top_bar = bars_hello_containers[1][0]
            x_center = hello_top_bar.get_x() + hello_top_bar.get_width() / 2
            hello_top = hello_top_bar.get_y() + hello_top_bar.get_height()
            # place secondary label above the primary label to show total+idp
            total_with_idp = hello_vals[0] + hello_idp_vals[0]
            y_secondary = hello_top + h_std + y_max * 0.03
            ax.text(x_center, y_secondary, f'{total_with_idp:.2f} s',
                    ha='center', va='bottom', fontsize=13, fontweight='bold', color='#222')
        except Exception:
            pass

    ax.set_ylabel('Latency (s)', fontsize=15, labelpad=8)
    title_extra = ''
    
    ax.set_title(f'E2E Login Latency\n (Client-side · returning user · n=50 · MOSHI net: lan)', fontsize=17, fontweight='bold', pad=14)
    ax.set_xticks(x)
    ax.set_xticklabels(labels, fontsize=14)
    ax.set_ylim(0, y_max)
    ax.yaxis.set_major_formatter(mticker.FuncFormatter(lambda v, _: f'{v:.1f}'))
    ax.tick_params(axis='y', labelsize=14)
    # place legend outside to the right so it doesn't cover bars
    ax.legend(fontsize=13, framealpha=0.95, loc='upper left', bbox_to_anchor=(1.02, 1.0))
    ax.set_axisbelow(True)

    # Footnote
    fig.text(0.01, -0.05,
             'Error bars show ±1 SD (mean only).\n',
             fontsize=16, color='#555', style='italic')

    fig.subplots_adjust(top=0.92, bottom=0.08, left=0.10, right=0.82)

    os.makedirs(os.path.dirname(os.path.abspath(args.output)), exist_ok=True)
    for ext in ['png', 'pdf']:
        out = f'{args.output}.{ext}'
        fig.savefig(out, dpi=150, bbox_inches='tight')
        print(f'Saved: {out}')


if __name__ == '__main__':
    main()
