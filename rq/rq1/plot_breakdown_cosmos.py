#!/usr/bin/env python3
"""
plot_breakdown_cosmos.py: Breakdown (demo cosmos backend vs Hellō), independent scales

Same 3-panel layout as plot_breakdown_scales.py:
  1. Demo App: full scale (Google dominates)
  2. Demo App: broker zoom (A B E F G H, no Google C/D)
  3. Hellō: full scale

Usage:
  python3 rq/plot_breakdown_cosmos.py
  python3 rq/plot_breakdown_cosmos.py \
      --demo  rq/out/breakdown_demo.csv \
      --hello rq/out/breakdown_hello.csv \
      --label "cosmos backend" \
      --output plots/rq1_breakdown
"""

import argparse
import csv
import os
import sys
from pathlib import Path

try:
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    import matplotlib.patches as mpatches
except ImportError:
    print("pip install matplotlib", file=sys.stderr)
    sys.exit(1)

matplotlib.rcParams.update({
    'font.size':        18,
    'axes.titlesize':   20,
    'axes.labelsize':   19,
    'xtick.labelsize':  18,
    'ytick.labelsize':  18,
    'legend.fontsize':  17,
    'figure.titlesize': 23,
})


def mean_col(rows, col):
    vals = []
    for r in rows:
        s = str(r.get(col, '')).strip()
        if s in ('', 'FAILED'):
            continue
        try:
            vals.append(float(s))
        except ValueError:
            pass
    return sum(vals) / len(vals) if vals else 0.0


def load_demo(path):
    with open(path) as f:
        rows = list(csv.DictReader(f))
    return {k: mean_col(rows, f'{k}_ms') for k in ['A', 'B', 'C', 'D', 'E', 'F', 'F1', 'G', 'H']}


def load_hello(path):
    with open(path) as f:
        rows = list(csv.DictReader(f))
    if not rows:
        return {'A': 0, 'B': 0, 'C': 0, 'D': 0}
    def pick(*keys):
        for k in keys:
            v = mean_col(rows, k)
            if v > 0:
                return v
        return 0.0
    return {
        'A': pick('A_appcode_ms', 'A_appcode', 'A_total_ms', 'A_login_total'),
        'B': pick('B_appcode_ms', 'B_appcode', 'B_total_ms', 'B_redirect_total'),
        'C': pick('C_appcode_ms', 'C_appcode', 'C_total_ms', 'C_callback_total'),
        'D': pick('D_total_ms', 'D_final_ms'),
    }


# Each tuple is (data_key, display_letter, label, color). The data_key looks the
# value up in the CSV; the display_letter is the figure label. We use the canonical
# A–H flow letters so the legend shows the WHOLE login sequence (Google C/D included
# in the legend, but excluded from the drawn bars: they are a shared external cost
# measured browser-side in rq1_browser_detail). The "SSD n" suffix cross-references
# the sequence-diagram step numbers so each phase maps onto the protocol flow.
# Tuple: (data_key, display_letter, host, description, ssd_ref, color). host,
# description and ssd_ref are separate columns so the legend renders as an aligned
# monospace table (letter | [host] | description | SSD step | value). Padding is
# done in code (not hand-typed spaces) so every column lines up exactly.
DEMO_PHASES = [
    # Thesis-friendly palette (Tableau-like, distinct and print-safe)
    ('A', 'A', '[demo_app→AB]',   'routing',                 'SSD 1',     '#AF7AA1'),  # purple/mauve
    ('B', 'B', '[AB→IB]',         'blind app_id + route',    'SSD 2–3',   '#EDC949'),  # yellow
    ('C', 'C', '[IB↔Google]',     'Google OAuth round-trip', 'SSD 4',     '#4E79A7'),  # blue (excluded)
    ('D', 'D', '[IB↔Google API]', 'Google token exchange',   'SSD 5',     '#F28E2B'),  # orange (excluded)
    ('E', 'E', '[IB]',            'blind iss/sub, crypto',   'SSD 6',     '#9C755F'),  # brown
    ('F', 'F', '[IB→tTS→AB]',     'tTS + FROST + push to AB','SSD 7–14',  '#E15759'),  # red
    ('G', 'G', '[IB→AB]',         'browser redirect to AB',  'SSD 15',    '#59A14F'),  # green
    ('H', 'H', '[AB→demo_app]',   'code exchange + render',  'SSD 15–16', '#76B7B2'),  # teal
]

HELLO_PHASES = [
    ('C', 'C', '[app↔Hellō]', 'Hellō token exchange', '', '#4E79A7'),
    ('B', 'B', '[app→Hellō]', 'wallet auth URL',      '', '#F28E2B'),
    ('A', 'A', '[app]',       'SDK state setup',      '', '#E15759'),
    ('D', 'D', '[app]',       'final redirect',       '', '#76B7B2'),
]

HELLO_LABEL = 'Hellō App'


def draw_stacked(ax, phases, data, title, xlabel, show_ylabel=True, show_legend=True,
                 excluded=None):
    # data values are in ms; display in ms
    drawn = list(phases)
    excl  = list(excluded or [])
    total_ms = sum(data.get(t[0], 0) for t in drawn)

    # Column widths so the legend renders as an aligned monospace table:
    #   <L>  [host]  <description>  <SSD step>  <value>
    all_p  = drawn + excl
    host_w = max((len(t[2]) for t in all_p), default=6)
    desc_w = max((len(t[3]) for t in all_p), default=10)
    ssd_w  = max((len(t[4]) for t in all_p), default=4)

    def row(letter, host, desc, ssd, value):
        return f'{letter}  {host:<{host_w}}  {desc:<{desc_w}}  {ssd:<{ssd_w}}  {value}'

    bottom = 0.0
    legend_items = []   # (letter, patch): sorted by letter for the legend
    for key, letter, host, desc, ssd, color in drawn:
        val_ms = data.get(key, 0.0)
        if val_ms <= 0:
            legend_items.append((letter, mpatches.Patch(color=color,
                label=row(letter, host, desc, ssd, '—'))))
            continue
        ax.bar(0, val_ms, bottom=bottom, color=color, width=0.55,
               edgecolor='white', linewidth=0.6)
        pct = (val_ms / total_ms * 100) if total_ms else 0
        y_mid = bottom + val_ms / 2
        axis_max = total_ms * 1.08
        # in-bar value shown with one decimal (proper rounding), matching the legend
        if val_ms / axis_max >= 0.06:
            ax.text(0, y_mid, f'{letter} · {val_ms:.1f} ms\n({pct:.0f}%)',
                ha='center', va='center', fontsize=17,
                color='white', fontweight='bold')
        elif val_ms / axis_max >= 0.025:
            ax.text(0, y_mid, f'{letter} · {val_ms:.1f} ms ({pct:.0f}%)',
                    ha='center', va='center', fontsize=16,
                    color='white', fontweight='bold')
        # legend shows the segment value and %, right-aligned in fixed columns
        legend_items.append((letter, mpatches.Patch(color=color,
            label=row(letter, host, desc, ssd, f'{val_ms:7.1f} ms ({pct:>2.0f}%)'))))
        bottom += val_ms

    # Excluded phases (Google C/D): shown in the legend to display the whole login
    # sequence, but NOT drawn as bars and NOT counted in the internal total.
    for key, letter, host, desc, ssd, color in excl:
        val_ms = data.get(key, 0.0)
        legend_items.append((letter, mpatches.Patch(color=color, alpha=0.45,
            label=row(letter, host, desc, ssd,
                      f'{val_ms:7.1f} ms  (excluded, browser-side)'))))

    ax.set_xlim(-0.55, 0.55)
    ax.set_ylim(0, total_ms * 1.08)
    ax.set_xticks([0])
    ax.set_xticklabels([
        f'{xlabel}\nTotal: {total_ms:.2f} ms ({total_ms/1000:.2f} s) - Google Excluded'
    ], fontsize=18)
    if show_ylabel:
        ax.set_ylabel('Latency (ms)', fontsize=18)
    ax.set_title(title, fontsize=20, fontweight='bold', pad=10)
    ax.yaxis.grid(True, linestyle='--', alpha=0.35)
    ax.set_axisbelow(True)
    legend_items.sort(key=lambda t: t[0])   # alphabetical by phase letter (A→F)
    patches = [p for _, p in legend_items]
    if show_legend:
        # place the legend BELOW the plot, one entry per row so every label is
        # left-aligned and reads as a clean A→H sequence.
        leg = ax.legend(handles=patches, loc='upper center',
                        framealpha=0.95, bbox_to_anchor=(0.5, -0.12), ncol=1,
                        borderaxespad=0.0, handlelength=1.3, handleheight=1.3,
                        labelspacing=0.55, alignment='left',
                        prop={'family': 'monospace', 'size': 16})
        leg.get_frame().set_edgecolor('#cccccc')


def main():
    p = argparse.ArgumentParser()
    script_dir = Path(__file__).resolve().parent
    repo_root = script_dir.parent.parent
    p.add_argument('--demo',   default=str(repo_root / 'rq' / 'rq1' / 'out' / 'breakdown_demo.csv'))
    p.add_argument('--hello',  default=str(repo_root / 'rq' / 'rq1' / 'out' / 'breakdown_hello.csv'))
    p.add_argument('--label',  default='cosmos backend',
                   help='Extra label for demo panel title (e.g. "cosmos backend, netem wifi")')
    p.add_argument('--netem-label', default='lan',
                   help='Netem profile name shown on plot (e.g. "wifi")')
    p.add_argument('--netem-ms', type=float, default=8.0,
                   help='Netem one-way latency in ms: drawn as reference line on broker panel')
    p.add_argument('--output', default=str(repo_root / 'plots/rq1' / 'rq1_breakdown_cosmos'))
    args = p.parse_args()

    missing = [f for f in [args.demo, args.hello] if not os.path.exists(f)]
    if missing:
        print(f"Missing files: {missing}", file=sys.stderr)
        print("Run first:", file=sys.stderr)
        for f in missing:
            if 'demo' in f:
                print("  node rq/breakdown.js --system demo --runs 5", file=sys.stderr)
            else:
                print("  node rq/breakdown.js --system hello --runs 5", file=sys.stderr)
        sys.exit(1)

    demo  = load_demo(args.demo)
    hello = load_hello(args.hello)
    netem_label = args.netem_label.strip()
    netem_ms    = args.netem_ms

    broker_phases   = [ph for ph in DEMO_PHASES if ph[0] not in ('C', 'D')]
    excluded_phases = [ph for ph in DEMO_PHASES if ph[0] in ('C', 'D')]

    demo_title  = f'Demo App full scale\n({args.label})'
    if netem_label:
        demo_title = f'Demo App full scale\n({args.label}, netem: {netem_label})'

    # ── Single panel: the partitioned broker's INTERNAL stages (server-side).
    # Google (C, D) is excluded on purpose: it is a shared external cost shown
    # browser-side in rq1_browser_detail, and the server-side view of it (IB→
    # Google→IB) would conflict with that (different vantage point). This plot's
    # value is the broker's own anatomy: blinding (E), FROST (F/F1), relay (G).
    # The Hellō wallet has no logs, so no equivalent breakdown exists for it.
    title = f'MOSHI Breakdown: Internal Stages\n(Server-side · returning user · n=50 · net: lan)'
   # title += f', netem: {netem_label})' if netem_label else ')'

    fig, ax = plt.subplots(figsize=(12, 11))
    draw_stacked(ax, broker_phases, demo,
                 title=title,
                 xlabel='Broker Internal Stages',
                 show_ylabel=True, show_legend=True,
                 excluded=excluded_phases)

    # ── Netem reference line (1 RTT on the inter-service links) ────────────────
    if netem_ms > 0 and demo.get('F', 0.0) > 0:
        rtt_ms = netem_ms * 2
        ax.axhline(rtt_ms, color='#ff6f00', linewidth=1.4, linestyle='--', alpha=0.85)
        ax.annotate(
            f'netem 1 RTT = {rtt_ms:.1f} ms ({netem_label})',
            xy=(0.28, rtt_ms), xytext=(0.30, rtt_ms + max(rtt_ms * 0.4, 0.5)),
            fontsize=13, color='#ff6f00', fontweight='bold',
            arrowprops=dict(arrowstyle='->', color='#ff6f00', lw=1.2),
        )

    f1 = demo.get('F1', 0.0)
    if f1 > 0:
        # show the "only" part in ms (as requested), keep the parenthetical in s
        f1_ms = f1
        f_ms = demo.get('F', 0) #/ 1000.0
        ax.set_xlabel(
                      f'FROST signing only: {f1_ms:.1f} ms  (within F phase: {f_ms:.2f} ms)',
                      fontsize=16, style='italic', color='#555')

    os.makedirs(os.path.dirname(os.path.abspath(args.output)), exist_ok=True)
    for ext in ['png', 'pdf']:
        out = f'{args.output}.{ext}'
        fig.savefig(out, dpi=180, bbox_inches='tight')
        print(f'Saved: {out}')


if __name__ == '__main__':
    main()
