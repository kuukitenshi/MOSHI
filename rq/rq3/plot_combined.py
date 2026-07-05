#!/usr/bin/env python3
"""
plot_combined.py: RQ3 saturation + recovery in ONE figure (third view).

A mix of the two RQ3 figures: the panel layout of the saturation curve, but with
the x-axis extended over the WHOLE ramp (offered rate up THEN back down), so both
saturation and recovery are visible at once. The x-axis is the ramp sequence
(time →) with ticks labelled by the offered rate, which rises to the peak and
then falls back.

Four panels:
  1. Throughput: offered vs achieved (the gap = saturation) + the ceiling
  2. Latency: P85 (+ mean/P95/P99), log
  3. Errors: dropped %, rises under overload then falls on recovery
  4. Client CPU: both generators (log) vs the 85% guardrail

Usage:
  python3 rq/rq3/plot_combined.py \
    --client1 rq/rq3/out/rq3_vegeta_client1.csv \
    --client2 rq/rq3/out/rq3_vegeta_client2.csv \
    --output  plots/rq3/rq3_saturation_recovery
"""

import argparse, csv, os, sys

try:
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    import matplotlib.gridspec as gridspec
except ImportError:
    print("pip install matplotlib", file=sys.stderr); sys.exit(0)


def fnum(v):
    try:
        return float(v)
    except (TypeError, ValueError):
        return float('nan')


def load(path):
    rows = []
    with open(path) as f:
        for r in csv.DictReader(f):
            try:
                int(r.get('step'))
            except (TypeError, ValueError):
                continue
            rows.append(r)
    return sorted(rows, key=lambda x: int(x['step']))


def server_cpu_per_level(path, tepoch, duration):
    """Mean server CPU% in each level's window [t_epoch, t_epoch + duration]."""
    samples = []
    with open(path) as f:
        for line in f:
            parts = line.split()
            if len(parts) >= 2:
                try:
                    samples.append((float(parts[0]), float(parts[1])))
                except ValueError:
                    pass
    if not samples:
        return None
    out = []
    for t0 in tepoch:
        if t0 != t0:
            out.append(float('nan')); continue
        vals = [c for (t, c) in samples if t0 <= t <= t0 + duration]
        out.append(sum(vals) / len(vals) if vals else float('nan'))
    return out


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--client1', required=True)
    p.add_argument('--client2', required=True)
    p.add_argument('--output', default='plots/rq3/rq3_saturation_recovery')
    p.add_argument('--server-cpu', default=None, help='server CPU log ("<epoch> <pct>" per line)')
    p.add_argument('--duration', type=float, default=8.0, help='seconds per level (server-CPU windowing)')
    args = p.parse_args()
    for f in (args.client1, args.client2):
        if not os.path.exists(f):
            print(f"Missing: {f}", file=sys.stderr); sys.exit(1)

    d1, d2 = load(args.client1), load(args.client2)
    n = min(len(d1), len(d2))
    if n == 0:
        print("No sweep steps (need ramp data with a 'step' column).", file=sys.stderr); sys.exit(1)
    pairs = list(zip(d1[:n], d2[:n]))

    x        = list(range(len(pairs)))           # ramp sequence (time →)
    phase    = [a.get('phase', 'up') for a, b in pairs]
    offered  = [fnum(a['offered_rate']) + fnum(b['offered_rate']) for a, b in pairs]
    achieved = [fnum(a['achieved_rps']) + fnum(b['achieved_rps']) for a, b in pairs]

    def comb_err(a, b):
        req  = fnum(a['requests']) + fnum(b['requests'])
        fail = fnum(a['failed']) + fnum(b['failed'])
        return 100.0 * fail / req if req > 0 else float('nan')
    error = [comb_err(a, b) for a, b in pairs]

    def lat(a, b, key):
        vs = [fnum(a[key]), fnum(b[key])]
        vs = [v for v in vs if v == v and v > 0]
        return sum(vs) / len(vs) if vs else float('nan')
    p85  = [lat(a, b, 'p85_ms')  for a, b in pairs]
    mean = [lat(a, b, 'mean_ms') for a, b in pairs]
    p95  = [lat(a, b, 'p95_ms')  for a, b in pairs]
    p99  = [lat(a, b, 'p99_ms')  for a, b in pairs]
    cpu1 = [fnum(a['client_cpu_pct']) for a, b in pairs]
    cpu2 = [fnum(b['client_cpu_pct']) for a, b in pairs]
    tepoch = [fnum(a.get('t_epoch')) for a, b in pairs]

    server_cpu = None
    if args.server_cpu and os.path.exists(args.server_cpu):
        server_cpu = server_cpu_per_level(args.server_cpu, tepoch, args.duration)

    ceiling   = max([v for v in achieved if v == v], default=float('nan'))
    has_down  = 'down' in phase
    peak_rate = max(offered)
    peak_x    = x[next(i for i, o in enumerate(offered) if o == peak_rate)]

    GREY, BLUE, GREEN, RED, ORANGE, PURPLE = (
        '#9e9e9e', '#1565c0', '#2e7d32', '#c62828', '#e65100', '#5e35b1')

    plt.style.use('seaborn-v0_8-whitegrid')
    fig = plt.figure(figsize=(13, 15))
    gs = gridspec.GridSpec(4, 1, height_ratios=[2.4, 2.4, 1.2, 2.0], hspace=0.16)
    ax_thr = fig.add_subplot(gs[0])
    ax_lat = fig.add_subplot(gs[1], sharex=ax_thr)
    ax_err = fig.add_subplot(gs[2], sharex=ax_thr)
    ax_cpu = fig.add_subplot(gs[3], sharex=ax_thr)

    def marks(ax):
        if has_down:
            ax.axvspan(peak_x, max(x), color=GREEN, alpha=0.06)   # recovery leg
            ax.axvline(peak_x, color=RED, ls='--', lw=1.4, alpha=0.8)

    # ── Panel 1: throughput: offered vs achieved ──────────────────────────
    ax_thr.fill_between(x, offered, color=GREY, alpha=0.18, zorder=1, label='Offered rate')
    ax_thr.plot(x, offered, color=GREY, lw=1.4, alpha=0.85, zorder=2)
    ax_thr.plot(x, achieved, '-o', color=GREEN, lw=2.5, ms=5, zorder=4,
                label='Achieved (served) throughput')
    if ceiling == ceiling:
        ax_thr.axhline(ceiling, color='#1a237e', ls=':', lw=1.8, alpha=0.9)
        ax_thr.text(0, ceiling, f'  ceiling ≈ {int(ceiling):,} req/s',
                    color='#1a237e', fontsize=9, va='bottom', ha='left', fontweight='bold')
    ax_thr.set_ylabel('Throughput (req/s)', fontsize=11)
    ax_thr.set_title('Saturation & Recovery: POST /login\n(vegeta · sustained rate · keep-alive)',
                     fontsize=11, fontweight='bold')
    ax_thr.legend(fontsize=9, loc='upper left')
    ax_thr.grid(True, ls='--', alpha=0.4); ax_thr.set_axisbelow(True)
    marks(ax_thr); plt.setp(ax_thr.get_xticklabels(), visible=False)
    if has_down:
        ax_thr.text(peak_x, peak_rate, ' peak / release', color=RED,
                    fontsize=9, va='bottom', ha='left', fontweight='bold')
        ax_thr.text((peak_x + max(x)) / 2, peak_rate * 0.9, 'recovery (load falling)',
                    color=GREEN, fontsize=9, fontweight='bold', ha='center')

    # ── Panel 2: latency (log) ─────────────────────────────────────────────
    ax_lat.plot(x, p85,  '-o', color=BLUE,   lw=2.5, ms=5, zorder=5, label='P85 (data point)')
    ax_lat.plot(x, mean, '--D', color=PURPLE, lw=1.3, ms=3, alpha=0.8, label='Mean')
    ax_lat.plot(x, p95,  '--s', color=RED,    lw=1.3, ms=3, alpha=0.7, label='P95')
    ax_lat.plot(x, p99,  ':^',  color=ORANGE, lw=1.3, ms=3, alpha=0.7, label='P99')
    ax_lat.set_yscale('log'); ax_lat.set_ylabel('Latency (ms) [log]', fontsize=11)
    ax_lat.legend(fontsize=8.5, loc='upper left')
    ax_lat.grid(True, which='both', ls='--', alpha=0.4); ax_lat.set_axisbelow(True)
    marks(ax_lat); plt.setp(ax_lat.get_xticklabels(), visible=False)

    # ── Panel 3: errors (dropped %) ────────────────────────────────────────
    ax_err.fill_between(x, 0, error, color=RED, alpha=0.18)
    ax_err.plot(x, error, '-o', color=RED, lw=2, ms=4)
    ax_err.set_ylabel('Errors /\ndropped (%)', fontsize=10)
    emax = max([e for e in error if e == e] + [5])
    ax_err.set_ylim(-2, emax * 1.15)
    ax_err.grid(True, ls='--', alpha=0.4); ax_err.set_axisbelow(True)
    marks(ax_err); plt.setp(ax_err.get_xticklabels(), visible=False)

    # ── Panel 4: client CPU (log) ──────────────────────────────────────────
    c1 = [max(v, 0.05) if v == v else 0.05 for v in cpu1]
    c2 = [max(v, 0.05) if v == v else 0.05 for v in cpu2]
    ax_cpu.plot(x, c1, '-o', color=BLUE,   lw=2, ms=4, label='client1 CPU')
    ax_cpu.plot(x, c2, '-s', color=ORANGE, lw=2, ms=4, label='client2 CPU')
    cpk = max([v for v in cpu1 + cpu2 if v == v] or [0])
    if server_cpu and any(v == v for v in server_cpu):
        sc = [max(v, 0.05) if v == v else 0.05 for v in server_cpu]
        ax_cpu.plot(x, sc, '-^', color=GREEN, lw=2.6, ms=6, zorder=6, label='server CPU')
        spk = max([v for v in server_cpu if v == v] or [0])
        ax_cpu.text(max(x), min(spk * 1.25, 360), f'server peak ≈ {spk:.0f}%', va='bottom', ha='right',
                    fontsize=8.5, color=GREEN, fontweight='bold')
    ax_cpu.axhline(85, color='#6a1b9a', ls='--', lw=1.2, alpha=0.85)
    ax_cpu.text(0, 95, 'saturation guardrail (85%)', va='bottom', ha='left', fontsize=8, color='#6a1b9a')
    ax_cpu.text(max(x), max(cpk * 1.3, 0.06), f'clients peak ≈ {cpk:.0f}%', va='bottom', ha='right',
                fontsize=8.5, color='#333', fontweight='bold')
    ax_cpu.set_yscale('log'); ax_cpu.set_ylim(0.1, 400)
    ax_cpu.set_ylabel('CPU (%) [log]', fontsize=10)
    ax_cpu.grid(True, which='both', ls='--', alpha=0.4); ax_cpu.set_axisbelow(True)
    ax_cpu.legend(fontsize=8.5, loc='lower left')
    marks(ax_cpu)   # peak/release line + recovery shading, like the other panels

    # ── x-axis: ramp sequence (time →) with ticks labelled by OFFERED rate ──
    ax_cpu.set_xticks(x)
    ax_cpu.set_xticklabels([f'{o/1000:.0f}k' for o in offered], rotation=45, fontsize=8)
    ax_cpu.set_xlabel('Combined offered rate (req/s): ramped up, then down (time →)', fontsize=11)

    os.makedirs(os.path.dirname(os.path.abspath(args.output)), exist_ok=True)
    for ext in ('png', 'pdf'):
        fig.savefig(f'{args.output}.{ext}', dpi=150, bbox_inches='tight')
        print(f'Saved: {args.output}.{ext}')


if __name__ == '__main__':
    main()
