#!/usr/bin/env python3
"""
plot_rq2.py: RQ2: FROST vs Ed25519 cryptographic overhead

Reads either:
  • a directory of per-execution results (--runs-dir, files results_*.json), or
  • a single execution JSON (--input, back-compat).

With multiple executions it reports the RUN-TO-RUN distribution, because the
sub-40µs Ed25519 baseline sits at the timing-noise floor and a single execution
swings (e.g. ×19 vs ×38). Per scheme we take each execution's MEDIAN latency,
then across executions report:
  • bar      = median of the per-execution medians (robust central value)
  • whiskers = [min, max] of the per-execution medians (full run-to-run range)
  • ×N       = median(FROST medians) / median(Ed25519 medians)
P95/P99 markers are the median-across-runs of each execution's within-run tail.

Usage:
  python3 rq2/plot_rq2.py --runs-dir rq/rq2/out/runs --output plots/rq2/rq2_crypto_overhead
  python3 rq2/plot_rq2.py --input   rq/rq2/out/bench_frost_results.json
"""

import argparse
import glob
import re
import json
import os
import statistics as st
import sys
from pathlib import Path

try:
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    import matplotlib.lines  as mlines
    import numpy as np
except ImportError:
    print("pip install matplotlib numpy", file=sys.stderr); sys.exit(1)


COLORS = {
    'ed':  {'bar': '#1565c0', 'light': '#90caf9'},
    'f32': {'bar': '#c62828', 'light': '#ef9a9a'},
    'f53': {'bar': '#e65100', 'light': '#ffcc80'},
    'f75': {'bar': '#558b2f', 'light': '#c5e1a5'},
}
SCHEME_COLORS = [COLORS['ed'], COLORS['f32'], COLORS['f53'], COLORS['f75']]


def load_runs(runs_dir, single):
    """Return a list of per-execution JSON dicts."""
    if runs_dir:
        files = sorted(glob.glob(os.path.join(runs_dir, 'results_*.json')))
        if not files:
            print(f"No results_*.json in {runs_dir}", file=sys.stderr); sys.exit(1)
        return [json.load(open(f)) for f in files]
    return [json.load(open(single))]


def aggregate(runs):
    """Collapse N executions into across-run stats per scheme."""
    def agg(getter):
        medians = [getter(r)['median_us'] for r in runs]
        return {
            'median_us': st.median(medians),          # bar
            'lo_us':     min(medians),                 # whisker low
            'hi_us':     max(medians),                 # whisker high
            # median-across-runs of per-run mean/p95/p99 (kept so plot_quorum.py,
            # which reads mean_us, still works against this aggregated file).
            'mean_us':   st.median(getter(r)['mean_us'] for r in runs),
            'p95_us':    st.median(getter(r)['p95_us'] for r in runs),
            'p99_us':    st.median(getter(r)['p99_us'] for r in runs),
            'exec_medians_us': medians,
        }
    ed = agg(lambda r: r['ed25519']); ed['label'] = 'Ed25519 (centralized)'
    frost = []
    for i in range(len(runs[0]['frost'])):
        s = agg(lambda r, i=i: r['frost'][i])
        s['label'] = runs[0]['frost'][i]['label']
        frost.append(s)
    return {
        'iterations': runs[0]['iterations'],
        'warmup':     runs[0]['warmup'],
        'repeats':    len(runs),
        'ed25519':    ed,
        'frost':      frost,
    }


def build_schemes(agg):
    schemes = [agg['ed25519']] + agg['frost']
    for i, s in enumerate(schemes):
        s['color'] = SCHEME_COLORS[i]['bar']
        s['light'] = SCHEME_COLORS[i]['light']
        s['x_label'] = s['label'].replace(' (', '\n(')
    return schemes


def draw(ax, schemes):
    """Median bars (log y) with run-to-run [min,max] whiskers and P95/P99 markers."""
    width = 0.55
    ed_med  = schemes[0]['median_us']
    ed_exec = schemes[0].get('exec_medians_us', [])
    for i, s in enumerate(schemes):
        bar = s['median_us'] / 1000
        lo, hi = s['lo_us'] / 1000, s['hi_us'] / 1000
        p95, p99 = s['p95_us'] / 1000, s['p99_us'] / 1000

        ax.bar(i, bar, width, color=s['color'], alpha=0.85, zorder=2)
        if hi > lo:   # run-to-run range (skip when single execution)
            ax.errorbar(i, bar, yerr=[[bar - lo], [hi - bar]], fmt='none',
                        color='black', capsize=5, linewidth=1.5, zorder=3)
        ax.plot(i, p95, 'v', color='black', markersize=6, zorder=4)
        ax.plot(i, p99, '^', color='#555',  markersize=6, zorder=4)

        if i == 0:
            # Ed25519 bar is too short to hold text; label sits high, above markers.
            # Show the median plus the run-to-run [min,max] range of the baseline.
            ax.text(i, max(p99, hi) * 1.4, f'{bar:.3f}\n[{lo:.3f}–{hi:.3f}]',
                    ha='center', va='bottom', fontsize=11, fontweight='bold')
        else:
            ratio = s['median_us'] / ed_med
            label = f'{bar:.3f}\n×{ratio:.0f}'
            # Run-to-run spread of the overhead ratio: each execution's FROST
            # median over the same execution's Ed25519 median. Wide because the
            # sub-40µs Ed25519 denominator sits at the timing-noise floor.
            fr_exec = s.get('exec_medians_us', [])
            if len(fr_exec) > 1 and len(fr_exec) == len(ed_exec):
                rs = [f / e for f, e in zip(fr_exec, ed_exec)]
                label += f'\n[×{min(rs):.0f}–×{max(rs):.0f}]'
            ax.text(i, bar * 0.5, label, ha='center',
                    va='center', color='white', fontsize=12, fontweight='bold')

    ax.set_yscale('log')
    tops = [max(s['p99_us'], s['hi_us']) / 1000 for s in schemes]
    ax.set_ylim(top=max(tops) * 1.6)
    ax.set_ylabel('Latency (ms)  [log scale]', fontsize=13)
    ax.tick_params(axis='y', labelsize=11)
    ax.set_xticks(range(len(schemes)))
    ax.set_xticklabels([s['x_label'] for s in schemes], fontsize=12)
    ax.yaxis.grid(True, which='both', linestyle='--', alpha=0.4)
    ax.set_axisbelow(True)
    handles = [
        mlines.Line2D([], [], color='black', linewidth=1.5, label='min–max across runs'),
        mlines.Line2D([], [], marker='v', color='black', linestyle='none',
                      markersize=6, label='P95 (within-run)'),
        mlines.Line2D([], [], marker='^', color='#555', linestyle='none',
                      markersize=6, label='P99 (within-run)'),
    ]
    ax.legend(handles=handles, fontsize=11, loc='upper left')


def write_table(agg, out_dir):
    """Median, within-run P95/P99, and the two overhead ratios, in ms.

    RQ2 carries no figure, so this table is the only place the numbers appear.
    Every central value is followed by its own run-to-run [min,max] in brackets
    rather than getting a separate column: the baseline's own spread is what
    makes ×N meaningless (it swings ~1.85× while FROST stays within 7%), so it
    has to sit next to the number a reader would otherwise trust.
    """
    ed = agg['ed25519']
    ed_exec = ed['exec_medians_us']
    base = agg['frost'][0]
    base_exec = base['exec_medians_us']

    def ratio_cell(s, ref_med, ref_exec, digits):
        if s is None:
            return '---'
        rs = [a / b for a, b in zip(s['exec_medians_us'], ref_exec)]
        return (f'$\\times${s["median_us"] / ref_med:.{digits}f} '
                f'[{min(rs):.{digits}f}--{max(rs):.{digits}f}]')

    def row(label, s, msgs='---', vs_ed=True, vs_base=True):
        m = s['median_us'] / 1000
        lo, hi = s['lo_us'] / 1000, s['hi_us'] / 1000
        p95 = s['p95_us'] / 1000
        p99 = s['p99_us'] / 1000
        c_ed = ratio_cell(s if vs_ed else None, ed['median_us'], ed_exec, 0)
        c_bs = ratio_cell(s if vs_base else None, base['median_us'], base_exec, 2)
        return (f'{label}  &  {msgs}  &  {m:.3f} [{lo:.3f}--{hi:.3f}]'
                f'  &  {p95:.3f}  &  {p99:.3f}  &  {c_ed}  &  {c_bs} \\\\')

    lines = [row('Ed25519 (centralized)', ed, vs_ed=False, vs_base=False)]
    for s in agg['frost']:
        lbl = s['label'].replace('FROST Ed25519 ', '').replace('(', '').replace(')', '').strip()
        n_t = lbl.replace('n=', 'n{=}').replace('t=', 't{=}')
        t = int(re.search(r't=(\d+)', s['label']).group(1))
        lines.append(row(f'FROST Ed25519 ($\\protect{{{n_t}}}$)', s, msgs=str(4 * t),
                         vs_base=(s is not agg['frost'][0])))
    print('\nLaTeX table rows:'); print('─' * 72)
    for r in lines:
        print(r)
    print('─' * 72)
    path = os.path.join(out_dir, 'rq2_table.tex')
    with open(path, 'w') as f:
        f.write('% RQ2: auto-generated by plot_rq2.py\n')
        f.write('% Columns: Method & Protocol msgs (4t) & Median [min-max] & P95 '
                '& P99 (ms) & xN vs Ed25519 & xn vs (3,2)\n')
        f.write('\\midrule\n')
        f.write('\n'.join(lines) + '\n')
    print(f'Table saved: {path}')


def main():
    p = argparse.ArgumentParser()
    script_dir = Path(__file__).resolve().parent
    repo_root  = script_dir.parent.parent
    p.add_argument('--runs-dir', default=None,
                   help='directory with per-execution results_*.json (multi-run)')
    p.add_argument('--input',  default=str(repo_root / 'rq' / 'rq2' / 'out' / 'bench_frost_results.json'),
                   help='single-execution JSON (used only if --runs-dir is absent)')
    p.add_argument('--output', default=str(repo_root / 'plots' / 'rq2' / 'rq2_crypto_overhead'))
    args = p.parse_args()

    if not args.runs_dir and not os.path.exists(args.input):
        print(f"Missing: {args.input}", file=sys.stderr)
        print("Run first: ./rq/rq2/run_rq2.sh", file=sys.stderr)
        sys.exit(1)

    runs    = load_runs(args.runs_dir, args.input)
    agg     = aggregate(runs)
    schemes = build_schemes(agg)
    n, reps = agg['iterations'], agg['repeats']

    fig, ax = plt.subplots(figsize=(8.5, 6), constrained_layout=True)
    draw(ax, schemes)

    fig.suptitle('Cryptographic Signing Overhead: FROST vs Centralized Ed25519',
                 fontsize=15, fontweight='bold')
    if reps > 1:
        sub = (f'{reps} executions × {n} iterations  ·  bar = median, '
               f'whiskers = min–max across runs  ·  ×N = median overhead vs Ed25519')
    else:
        sub = (f'n = {n} iterations  ·  median (log scale)  ·  '
               f'×N = median overhead vs Ed25519 baseline')
    ax.set_title(sub, fontsize=12, color='#444', pad=8)

    os.makedirs(os.path.dirname(os.path.abspath(args.output)), exist_ok=True)
    for ext in ['png', 'pdf']:
        out = f'{args.output}.{ext}'
        fig.savefig(out, dpi=150, bbox_inches='tight')
        print(f'Saved: {out}')

    # Persist the aggregated result + the LaTeX table next to the inputs.
    out_dir = args.runs_dir or os.path.dirname(args.input)
    if args.runs_dir:
        out_dir = os.path.dirname(args.runs_dir.rstrip('/'))   # .../out
    with open(os.path.join(out_dir, 'bench_frost_results.json'), 'w') as f:
        json.dump(agg, f, indent=2)
    write_table(agg, out_dir)


if __name__ == '__main__':
    main()
