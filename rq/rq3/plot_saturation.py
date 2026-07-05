#!/usr/bin/env python3
"""
plot_saturation.py: RQ3 rate-based saturation curve (vegeta sweep).

Four stacked panels vs the combined OFFERED rate (req/s) across both clients:
  1. Latency (P85 data point, plus mean/P95/P99)
  2. Achieved throughput vs offered rate: the saturation signal: when the
     achieved line peels away from the ideal diagonal, the server can't keep up.
  3. Error rate (%)
  4. Client CPU (both generators): proof the bottleneck is the server, not the
     load generators: client CPU stays far below the 85% saturation guardrail.

If the input was produced by a ramp (up-then-down) sweep, only the rising leg
(phase != "down") is plotted, so the curve stays a clean left-to-right sweep.

Usage:
  python3 rq/rq3/plot_saturation.py \
    --client1 rq/rq3/out/rq3_vegeta_client1.csv \
    --client2 rq/rq3/out/rq3_vegeta_client2.csv \
    --output  plots/rq3/rq3_saturation_vegeta
"""

import argparse, csv, os, sys

try:
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    import matplotlib.gridspec as gridspec
except ImportError:
    print("pip install matplotlib", file=sys.stderr); sys.exit(0)


def _num(v):
    if v is None or v in ('', 'N/A'):
        return float('nan')
    try:
        return float(v)
    except ValueError:
        return v


def load(path):
    rows = []
    with open(path) as f:
        for r in csv.DictReader(f):
            try:                       # skip any non-data line (stray progress output)
                float(r.get('offered_rate'))
            except (TypeError, ValueError):
                continue
            if r.get('phase') == 'down':   # ramp data: keep only the rising leg
                continue
            rows.append({k: _num(v) for k, v in r.items()})
    return sorted(rows, key=lambda x: x['offered_rate'])


def avg(r1, r2, key):
    # ignore NaN and exact-0 latencies (0 = a collapsed client with no successful
    # requests; averaging it in would drag the curve down misleadingly).
    vals = [r.get(key, float('nan')) for r in (r1, r2)]
    vals = [v for v in vals if v == v and v > 0]
    return sum(vals) / len(vals) if vals else float('nan')


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
    p.add_argument('--output', default='plots/rq3/rq3_saturation_vegeta')
    p.add_argument('--server-cpu', default=None, help='server CPU log ("<epoch> <pct>" per line)')
    p.add_argument('--duration', type=float, default=8.0, help='seconds per level (server-CPU windowing)')
    args = p.parse_args()
    for f in (args.client1, args.client2):
        if not os.path.exists(f):
            print(f"Missing: {f}", file=sys.stderr); sys.exit(1)

    d1, d2 = load(args.client1), load(args.client2)
    n = min(len(d1), len(d2))
    if n == 0:
        print("No sweep levels.", file=sys.stderr); sys.exit(1)
    pairs = list(zip(d1[:n], d2[:n]))

    offered  = [r1['offered_rate'] + r2['offered_rate'] for r1, r2 in pairs]
    achieved = [r1.get('achieved_rps', float('nan')) + r2.get('achieved_rps', float('nan'))
                for r1, r2 in pairs]
    p85  = [avg(r1, r2, 'p85_ms')  for r1, r2 in pairs]
    mean = [avg(r1, r2, 'mean_ms') for r1, r2 in pairs]
    p95  = [avg(r1, r2, 'p95_ms')  for r1, r2 in pairs]
    p99  = [avg(r1, r2, 'p99_ms')  for r1, r2 in pairs]
    err  = [(r1.get('failed', 0) + r2.get('failed', 0)) /
            max(r1.get('requests', 0) + r2.get('requests', 0), 1) * 100
            for r1, r2 in pairs]
    cli_sat = [bool(r1.get('client_saturated', 0)) or bool(r2.get('client_saturated', 0))
               for r1, r2 in pairs]
    cpu1 = [r1.get('client_cpu_pct', float('nan')) for r1, r2 in pairs]
    cpu2 = [r2.get('client_cpu_pct', float('nan')) for r1, r2 in pairs]
    tepoch = [r1.get('t_epoch', float('nan')) for r1, r2 in pairs]
    server_cpu = None
    if args.server_cpu and os.path.exists(args.server_cpu):
        server_cpu = server_cpu_per_level(args.server_cpu, tepoch, args.duration)

    succ = [100.0 * (r1.get('ok', 0) + r2.get('ok', 0)) /
            max(r1.get('requests', 0) + r2.get('requests', 0), 1)
            for r1, r2 in pairs]
    def mask(series):
        return [v if (s > 0 and v == v and v > 0) else float('nan')
                for v, s in zip(series, succ)]
    p85, mean, p95, p99 = mask(p85), mask(mean), mask(p95), mask(p99)

    # Peak achieved throughput and the offered rate at which it occurs. The new
    # flow peaks then collapses under congestion, so this is a true peak rather
    # than a sustained plateau; we mark the point, not a flat asymptote.
    pts = [(o, a) for o, a in zip(offered, achieved) if a == a]
    ceiling = max((a for _, a in pts), default=float('nan'))
    peak_x = next((o for o, a in pts if a == ceiling), float('nan'))

    # Saturation = first offered rate where the server stops keeping up, i.e.
    # achieved falls below 95% of offered (departs the ideal diagonal). Robust
    # to a single noisy level, unlike a chord-elbow on a rise-then-collapse curve.
    knee = None
    for o, a in zip(offered, achieved):
        if a == a and o > 0 and a < 0.95 * o:
            knee = o
            break

    # Max sustainable (error-free) rate: the HIGHEST offered rate still at
    # <=1% errors, taken as a max (not a contiguous run from the start) so a
    # single noisy level does not collapse it to the very first rate.
    safe_candidates = [o for o, e in zip(offered, err) if e == e and e <= 1.0]
    safe = max(safe_candidates) if safe_candidates else None

    fig = plt.figure(figsize=(12, 14))
    gs = gridspec.GridSpec(4, 1, height_ratios=[3, 2.2, 1, 2.0], hspace=0.16)
    ax_lat = fig.add_subplot(gs[0])
    ax_thr = fig.add_subplot(gs[1], sharex=ax_lat)
    ax_err = fig.add_subplot(gs[2], sharex=ax_lat)
    ax_cpu = fig.add_subplot(gs[3], sharex=ax_lat)

    # ── Panel 1: latency ───────────────────────────────────────────────
    ax_lat.plot(offered, p85,  '-o', color='#1565c0', lw=2.5, ms=5, label='P85 latency (data point)', zorder=4)
    ax_lat.plot(offered, mean, '--D', color='#5e35b1', lw=1.5, ms=4, alpha=0.8, label='Mean latency', zorder=3)
    ax_lat.plot(offered, p95,  '--s', color='#e53935', lw=1.5, ms=4, alpha=0.8, label='P95 latency', zorder=3)
    ax_lat.plot(offered, p99,  ':^',  color='#ff8f00', lw=1.5, ms=4, alpha=0.7, label='P99 latency', zorder=3)
    sat_x = [o for o, s in zip(offered, cli_sat) if s]
    sat_y = [y for y, s in zip(p85, cli_sat) if s]
    if sat_x:
        ax_lat.scatter(sat_x, sat_y, s=160, facecolors='none', edgecolors='k',
                       linewidths=1.6, zorder=6, label='Client CPU saturated: discard')
    ax_lat.set_yscale('log')
    ax_lat.set_ylabel('Latency (ms) [log]', fontsize=11)
    ax_lat.yaxis.grid(True, which='both', ls='--', alpha=0.4); ax_lat.set_axisbelow(True)
    ax_lat.set_title('Saturation Curve: POST /login\n(vegeta · sustained rate · keep-alive)',
                     fontsize=11, fontweight='bold')
    plt.setp(ax_lat.get_xticklabels(), visible=False)

    # ── Panel 2: achieved vs offered throughput ────────────────────────
    lo, hi = min(offered), max(offered)
    ax_thr.plot([lo, hi], [lo, hi], '-', color='#9e9e9e', lw=1.2, alpha=0.8,
                label='Ideal (achieved = offered)')
    ax_thr.plot(offered, achieved, '-o', color='#2e7d32', lw=2.5, ms=5,
                label='Achieved throughput', zorder=4)
    if ceiling == ceiling and peak_x == peak_x:
        # Once errors appear (offered >= knee) the achieved throughput stops
        # tracking offered and holds roughly flat: that is a ceiling/plateau, not
        # "still rising". We only call it "still rising" if the peak is at the top
        # of the sweep AND clearly above that saturated-region level (>15%).
        sat_region = [a for o, a in zip(offered, achieved)
                      if a == a and knee is not None and o >= knee]
        plateau = sum(sat_region) / len(sat_region) if sat_region else ceiling
        rising = (peak_x >= hi) and (ceiling > 1.15 * plateau)
        ax_thr.plot([peak_x], [ceiling], marker='*', color='#1a237e', ms=15, zorder=6)
        if rising:
            ax_thr.annotate(f'max ≈ {int(ceiling):,} req/s\n(still rising: not saturated)',
                            xy=(peak_x, ceiling), xytext=(-10, 10), textcoords='offset points',
                            color='#1a237e', fontsize=9, ha='right', va='bottom',
                            fontweight='bold')
        else:
            ax_thr.axhline(ceiling, color='#1a237e', ls=':', lw=1.5, alpha=0.6)
            # Anchor the label away from whichever edge the star sits on: centring
            # it on a peak at either end of the sweep spills past the frame.
            frac = (peak_x - lo) / (hi - lo) if hi > lo else 0.5
            align, dx = (('right', -12) if frac > 0.66 else
                         ('left', 12) if frac < 0.34 else ('center', 0))
            ax_thr.annotate(f'ceiling ≈ {int(ceiling):,} req/s (plateau)',
                            xy=(peak_x, ceiling), xytext=(dx, 9), textcoords='offset points',
                            color='#1a237e', fontsize=9, ha=align, va='bottom',
                            fontweight='bold')
    ax_thr.set_ylabel('Throughput (req/s)', fontsize=11)
    ax_thr.yaxis.grid(True, ls='--', alpha=0.4); ax_thr.set_axisbelow(True)
    ax_thr.legend(fontsize=9, loc='lower right')
    plt.setp(ax_thr.get_xticklabels(), visible=False)

    # ── Markers across panels ──────────────────────────────────────────
    # Add headroom above the latency plateau so the vertical marker labels sit in
    # clear whitespace at the top instead of overlapping the curves. Labels are
    # anchored in axis-fraction y (via get_xaxis_transform: x=data, y=axes) so they
    # always hang from just under the top edge regardless of the data range.
    if safe or knee:
        ax_lat.set_ylim(top=ax_lat.get_ylim()[1] * 40)
    lat_lbl_tf = ax_lat.get_xaxis_transform()
    if safe:
        for ax in (ax_lat, ax_thr, ax_err, ax_cpu):
            ax.axvline(safe, color='#00838f', ls='--', lw=1.5, alpha=0.85)
        ax_lat.text(safe, 0.97, f'  max sustainable ≈ {int(safe):,} req/s (0 errors)',
                    transform=lat_lbl_tf, color='#00838f', fontsize=9, va='top',
                    rotation=90, fontweight='bold')
    if knee:
        for ax in (ax_lat, ax_thr, ax_err, ax_cpu):
            ax.axvline(knee, color='#d81b60', ls='--', lw=1.9, alpha=0.9)
        ax_lat.text(knee, 0.97, f'  saturation ≈ {int(knee):,} req/s',
                    transform=lat_lbl_tf, color='#d81b60', fontsize=9, va='top',
                    rotation=90, fontweight='bold')
    ax_lat.legend(fontsize=8.5, loc='center right')

    # ── Panel 3: error rate ────────────────────────────────────────────
    ax_err.fill_between(offered, err, alpha=0.4, color='#e53935')
    ax_err.plot(offered, err, '-o', color='#c62828', lw=2, ms=4)
    ax_err.set_ylabel('Error %', fontsize=10)
    ax_err.yaxis.grid(True, ls='--', alpha=0.4); ax_err.set_axisbelow(True)
    ax_err.set_ylim(bottom=0)
    if max(err) < 1:
        ax_err.set_ylim(0, 5)
    plt.setp(ax_err.get_xticklabels(), visible=False)

    # ── Panel 4: client CPU (log) ──────────────────────────────────────
    c1 = [max(v, 0.05) if v == v else 0.05 for v in cpu1]
    c2 = [max(v, 0.05) if v == v else 0.05 for v in cpu2]
    ax_cpu.plot(offered, c1, '-o', color='#1565c0', lw=2, ms=4, label='client1 CPU')
    ax_cpu.plot(offered, c2, '-s', color='#e65100', lw=2, ms=4, label='client2 CPU')
    cpk = max([v for v in cpu1 + cpu2 if v == v] or [0])
    if server_cpu and any(v == v for v in server_cpu):
        sc = [max(v, 0.05) if v == v else 0.05 for v in server_cpu]
        ax_cpu.plot(offered, sc, '-^', color='#2e7d32', lw=2.6, ms=5, zorder=6, label='server CPU')
        spk = max([v for v in server_cpu if v == v] or [0])
        ax_cpu.text(hi, min(spk * 1.25, 360), f'server peak ≈ {spk:.0f}%', va='bottom', ha='right',
                    fontsize=8.5, color='#2e7d32', fontweight='bold')
    ax_cpu.axhline(85, color='#6a1b9a', ls='--', lw=1.2, alpha=0.85)
    ax_cpu.text(lo, 95, 'saturation guardrail (85%)', va='bottom', ha='left', fontsize=8, color='#6a1b9a')
    ax_cpu.text(hi, max(cpk * 1.3, 0.06), f'clients peak ≈ {cpk:.0f}%', va='bottom', ha='right',
                fontsize=8.5, color='#333', fontweight='bold')
    ax_cpu.set_yscale('log')
    ax_cpu.set_ylim(0.1, 400)
    ax_cpu.set_ylabel('CPU (%) [log]', fontsize=10)
    ax_cpu.set_xlabel('Combined offered rate across both clients (req/s)', fontsize=11)
    ax_cpu.yaxis.grid(True, which='both', ls='--', alpha=0.4); ax_cpu.set_axisbelow(True)
    ax_cpu.legend(fontsize=8.5, loc='lower right')

    os.makedirs(os.path.dirname(os.path.abspath(args.output)), exist_ok=True)
    for ext in ('png', 'pdf'):
        fig.savefig(f'{args.output}.{ext}', dpi=150, bbox_inches='tight')
        print(f'Saved: {args.output}.{ext}')


if __name__ == '__main__':
    main()
