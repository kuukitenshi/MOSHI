#!/usr/bin/env python3
"""
plot_rq2_merged.py: RQ2: one figure replacing plot_rq2.py + plot_quorum.py.

Both of those drew the same three FROST medians. This merges them into a single
panel that keeps what was unique to each:
  • from the overhead plot: the centralized Ed25519 baseline bar (log y, so the
    20µs baseline stays visible next to the millisecond quorums);
  • from the quorum plot:   the 4t protocol-message count, carried on the x tick
    labels instead of a second panel.

The ×N (vs Ed25519) and ×n (vs the (3,2) quorum) ratios are deliberately NOT
drawn: they are derived numbers and belong to the RQ2 table, which plot_rq2.py
emits. Stats come from plot_rq2.aggregate, so bars/whiskers/percentiles are
identical to what the two separate figures showed.

Usage:
  python3 rq/rq2/plot_rq2_merged.py --runs-dir rq/rq2/out/runs \
      --output plots/rq2/rq2_crypto
"""

import argparse, os, re, sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from plot_rq2 import load_runs, aggregate            # noqa: E402

try:
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    import matplotlib.lines as mlines
except ImportError:
    print("pip install matplotlib", file=sys.stderr); sys.exit(1)


ED_COLOR = '#78909c'                                  # neutral grey: the baseline
FROST_COLORS = ['#90caf9', '#1976d2', '#0d47a1']      # light → dark: quorum growth


def main():
    script_dir = Path(__file__).resolve().parent
    repo_root = script_dir.parent.parent
    p = argparse.ArgumentParser()
    p.add_argument('--runs-dir', default=str(repo_root / 'rq' / 'rq2' / 'out' / 'runs'))
    p.add_argument('--input', default=str(repo_root / 'rq' / 'rq2' / 'out' / 'bench_frost_results.json'))
    p.add_argument('--output', default=str(repo_root / 'plots' / 'rq2' / 'rq2_crypto'))
    args = p.parse_args()

    runs = load_runs(args.runs_dir if os.path.isdir(args.runs_dir) else None, args.input)
    agg = aggregate(runs)
    ed, frost = agg['ed25519'], agg['frost']
    ed_med = ed['median_us']

    # Kept narrow on purpose: the figure is included at ~.95\linewidth, so a wider
    # canvas is scaled down further and the in-bar annotations fall below ~7pt.
    fig, ax = plt.subplots(figsize=(8.4, 6.0), constrained_layout=True)
    width = 0.7
    xticks = []

    # ── Ed25519 baseline ──────────────────────────────────────────────────
    bar = ed_med / 1000
    lo, hi = ed['lo_us'] / 1000, ed['hi_us'] / 1000
    ax.bar(0, bar, width, color=ED_COLOR, alpha=0.9, zorder=2)
    ax.errorbar(0, bar, yerr=[[bar - lo], [hi - bar]], fmt='none', color='black',
                capsize=5, linewidth=1.5, zorder=3)
    ax.plot(0, ed['p95_us'] / 1000, 'v', color='black', markersize=6, zorder=4)
    ax.plot(0, ed['p99_us'] / 1000, '^', color='#555', markersize=6, zorder=4)
    ax.text(0, max(ed['p99_us'], ed['hi_us']) / 1000 * 1.5,
            f'{bar:.3f} ms\n[{lo:.3f}–{hi:.3f}]', ha='center', va='bottom',
            fontsize=11, fontweight='bold')
    xticks.append('Ed25519\n(centralized)\nsingle signer')

    # ── FROST quorums ─────────────────────────────────────────────────────
    for i, s in enumerate(frost):
        x = i + 1
        bar = s['median_us'] / 1000
        lo, hi = s['lo_us'] / 1000, s['hi_us'] / 1000
        ax.bar(x, bar, width, color=FROST_COLORS[i], alpha=0.95, zorder=2)
        if hi > lo:
            ax.errorbar(x, bar, yerr=[[bar - lo], [hi - bar]], fmt='none',
                        color='black', capsize=5, linewidth=1.5, zorder=3)
        ax.plot(x, s['p95_us'] / 1000, 'v', color='black', markersize=6, zorder=4)
        ax.plot(x, s['p99_us'] / 1000, '^', color='#555', markersize=6, zorder=4)

        # Only the measurement goes on the bar. The ×N/×n ratios live in the RQ2
        # table: they are derived numbers, and the ×N against Ed25519 in
        # particular is the one the text discounts as noise-dominated, so it has
        # no business being the largest text in the figure.
        # Dark bars take white text; the light (3,2) bar needs dark text.
        txt_color = 'white' if i else '#0d3c61'
        # Offset in points, not data units, which a log axis would distort. The
        # range is spelled out because at this scale the whiskers are only a few
        # pixels tall.
        ax.annotate(f'{bar:.3f} ms\n[{lo:.3f}–{hi:.3f}]', xy=(x, bar),
                    xytext=(0, -24), textcoords='offset points',
                    ha='center', va='top', color=txt_color, fontsize=11.5,
                    fontweight='bold', zorder=5)

        m = re.search(r'n=(\d+),\s*t=(\d+)', s['label'])
        n, t = (int(m.group(1)), int(m.group(2))) if m else (0, 0)
        xticks.append(f'FROST\n(n={n}, t={t})\n{4 * t} protocol msgs')

    ax.set_yscale('log')
    tops = [max(s['p99_us'], s['hi_us']) / 1000 for s in [ed] + frost]
    ax.set_ylim(top=max(tops) * 1.7)
    ax.set_ylabel('Signing latency (ms)  [log scale]', fontsize=13)
    ax.tick_params(axis='y', labelsize=11)
    ax.set_xticks(range(len(xticks)))
    ax.set_xticklabels(xticks, fontsize=11.5)
    ax.yaxis.grid(True, which='both', linestyle='--', alpha=0.4)
    ax.set_axisbelow(True)
    ax.legend(handles=[
        mlines.Line2D([], [], color='black', linewidth=1.5, label='min–max across runs'),
        mlines.Line2D([], [], marker='v', color='black', linestyle='none',
                      markersize=6, label='P95 (within-run)'),
        mlines.Line2D([], [], marker='^', color='#555', linestyle='none',
                      markersize=6, label='P99 (within-run)'),
    ], fontsize=11, loc='upper left')

    fig.suptitle('Cryptographic Signing Cost: Centralized Ed25519 vs FROST Quorums',
                 fontsize=15, fontweight='bold')
    ax.set_title(f'{agg["repeats"]} executions × {agg["iterations"]} iterations  ·  '
                 'bar = median, brackets = run-to-run [min, max]',
                 fontsize=11.5, color='#444', pad=8)

    os.makedirs(os.path.dirname(os.path.abspath(args.output)), exist_ok=True)
    for ext in ('png', 'pdf'):
        fig.savefig(f'{args.output}.{ext}', dpi=150, bbox_inches='tight')
        print(f'Saved: {args.output}.{ext}')


if __name__ == '__main__':
    main()
