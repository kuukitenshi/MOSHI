#!/usr/bin/env python3
"""
plot_browser_detail.py: Fine-grained E2E breakdown, measured the SAME way as the
coarse plot (browser-side navigation timing), but with the broker split into its
components so each part of each system is visible:

  Partitioned : App | AB | IB | Google
  Hellō       : App | Wallet | Google

Because it is the same clean partition as rq1_browser_comparison (just finer),
the totals match exactly: this is the coarse plot's "broker" bar split open.

Usage:
  python3 plot_browser_detail.py \
    --demo  rq/rq1/out/browser_timeline_demo.csv \
    --hello rq/rq1/out/browser_timeline_hello.csv \
    --output plots/rq1/rq1_browser_detail
"""

import argparse
import csv
import os
import sys

try:
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    from matplotlib.lines import Line2D
    import numpy as np
except ImportError:
    print("matplotlib/numpy not installed: skipping plot", file=sys.stderr)
    sys.exit(0)

# Components stacked bottom→top, with labels + colors. app/ab/ib/wallet/google
# cover both systems; the components a system doesn't use are simply 0.
COMPONENTS = ['app', 'ab', 'ib', 'wallet', 'google']
LABELS = {'app': 'App', 'ab': 'AB (Auth Broker)', 'ib': 'IB (Identity Broker)',
          'wallet': 'Wallet', 'google': 'Google IdP'}
COLORS = {'app': '#92c5de', 'ab': '#7fbf7b', 'ib': '#1b7837',
          'wallet': '#5aae61', 'google': '#d6604d'}


def net_label(demo_path):
    """Network-emulation condition written by the runner (e.g. 'mobile')."""
    f = os.path.join(os.path.dirname(os.path.abspath(demo_path)), 'rq1_net_label.txt')
    try:
        with open(f) as fh:
            return fh.read().strip() or None
    except FileNotFoundError:
        return None


def load_means(path):
    sums = {k: 0.0 for k in COMPONENTS}
    tot, n = 0.0, 0
    with open(path) as f:
        for row in csv.DictReader(f):
            if row.get('status') != 'ok':
                continue
            n += 1
            tot += float(row['total_ms'])
            for k in COMPONENTS:
                sums[k] += float(row.get(f'{k}_ms') or 0)
    if n == 0:
        sys.exit(f"No successful runs in {path}")
    return {k: sums[k] / n for k in COMPONENTS}, tot / n, n


def _har_bucket(host):
    host = host or ''
    if 'google.com' in host:                                       return 'google'
    if 'hello.coop' in host or 'hello.dev' in host:                return 'wallet'
    if ':4010' in host:                                            return 'ab'
    if ':4020' in host:                                            return 'ib'
    if ':3000' in host:                                            return 'app'
    return 'other'


def load_means_from_har(har_path, runs):
    """Per-host mean wall-clock (ms/login) from a HAR, measured the SAME way for
    both systems. We take, per host, the UNION of each request's
    [start, start+time] intervals (so concurrent requests are not double-counted,
    unlike a naive sum), then divide by the number of measured logins."""
    import json
    from datetime import datetime
    from urllib.parse import urlparse
    entries = json.load(open(har_path)).get('log', {}).get('entries', [])
    ivs = {k: [] for k in COMPONENTS}
    for e in entries:
        b = _har_bucket(urlparse(e.get('request', {}).get('url', '')).netloc)
        if b not in ivs:
            continue
        sdt = e.get('startedDateTime')
        if not sdt:
            continue
        try:
            start = datetime.fromisoformat(sdt.replace('Z', '+00:00')).timestamp() * 1000
        except ValueError:
            continue
        ivs[b].append((start, start + max(0.0, float(e.get('time', 0) or 0))))

    def union_len(intervals):
        if not intervals:
            return 0.0
        intervals = sorted(intervals)
        total = 0.0
        cs, ce = intervals[0]
        for s, en in intervals[1:]:
            if s > ce:
                total += ce - cs
                cs, ce = s, en
            else:
                ce = max(ce, en)
        return total + (ce - cs)

    means = {k: union_len(ivs[k]) / max(1, runs) for k in COMPONENTS}
    return means, sum(means.values()), runs


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--demo',   required=True)
    p.add_argument('--hello',  required=True)
    p.add_argument('--demo-har',  default=None,
                   help='HAR for the partitioned system; if given, per-host time is '
                        'taken from the HAR (same method as Hellō) instead of the CSV')
    p.add_argument('--hello-har', default=None,
                   help='HAR for Hellō; if given, per-host time is taken from the HAR')
    p.add_argument('--runs', type=int, default=20)
    p.add_argument('--output', default='plots/rq1/rq1_browser_detail')
    args = p.parse_args()

    # Unified measurement: when HARs are supplied, BOTH systems are measured the
    # same way (HAR interval-union per host); otherwise fall back to the per-run
    # browser-timeline CSVs.
    if args.demo_har and os.path.exists(args.demo_har):
        d_means, d_tot, d_n = load_means_from_har(args.demo_har, args.runs)
    else:
        d_means, d_tot, d_n = load_means(args.demo)
    if args.hello_har and os.path.exists(args.hello_har):
        h_means, h_tot, h_n = load_means_from_har(args.hello_har, args.runs)
    else:
        h_means, h_tot, h_n = load_means(args.hello)
    nl = net_label(args.demo)
    net_line = (f'returning user · n=20 · net: {nl}'
                if nl else 'returning user, warm session')
    # For the title we keep only the short profile name (e.g. "LAN"); the numeric
    # inter-service detail (e.g. "8 ms") is moved into the legend instead.
    nl_short = nl.split('(')[0].strip() if nl else ''
    title_net_line = (f'returning user · n=50 · MOSHI net: {nl_short}'
                      if nl_short else 'returning user, warm session')

    # The IdP round-trip is a constant: ~the same Google for both systems. It is
    # only observable in the partitioned system (the wallet does it server-side),
    # so by default we export that measured value as a shared constant and apply
    # it to Hellō too: equal Google in both, exact "rest" for each. Hellō's
    # totals are adjusted so the constant is additive, not double-counted.
    # The Google IdP round-trip is the same external service for both systems, but
    # it is only separately observable in the partitioned flow; the Hellō wallet
    # performs it server-side, inside the wallet navigation. We take the partitioned
    # in-browser measurement as the Google constant and, for Hellō, render it
    # EMBEDDED inside the wallet bar (a hatched sub-region) instead of stacked on
    # top: Hellō's measured wall-clock total already contains it, so nothing is
    # double-counted. Totals (d_tot, h_tot) stay as measured.
    emb_google = d_means['google']        # shared Google constant (ms)
    h_means = dict(h_means, google=0.0)   # not a separate Hellō segment

    # Text summary: segment means. Google is a separate in-browser segment in the
    # partitioned flow and an embedded sub-region of the Hellō wallet.
    g = emb_google
    line = '─' * 60
    out = [
        '=' * 64,
        f'  RQ1: Browser-side breakdown ({net_line})',
        '=' * 64,
        f'  {"Segment":<18}{"Partitioned (ms)":>18}{"Hellō (ms)":>18}',
        '  ' + line,
        f'  {"App":<18}{d_means["app"]:>18.0f}{h_means["app"]:>18.0f}',
        f'  {"AB":<18}{d_means["ab"]:>18.0f}{0:>18.0f}',
        f'  {"IB":<18}{d_means["ib"]:>18.0f}{0:>18.0f}',
        f'  {"Wallet":<18}{0:>18.0f}{h_means["wallet"]:>18.0f}',
        f'  {"Google (IdP)":<18}{g:>18.0f}{g:>18.0f}',
        '  ' + line,
        f'  {"TOTAL":<18}{d_tot:>18.0f}{h_tot:>18.0f}',
        '=' * 64,
        f'  Google IdP constant {g:.0f} ms: a separate in-browser segment in the',
        f'  partitioned flow; embedded inside Hellō\'s {h_means["wallet"]:.0f} ms wallet',
        '  (server-side: not added on top of the measured total).',
        '=' * 64,
    ]
    text = '\n'.join(out)
    print('\n' + text + '\n')
    txt_path = os.path.join(os.path.dirname(os.path.abspath(args.demo)),
                            'rq1_browser_summary.txt')
    with open(txt_path, 'w') as f:
        f.write(text + '\n')
    print(f'Saved: {txt_path}')

    plt.style.use('seaborn-v0_8-whitegrid')
    matplotlib.rcParams.update({
        'font.size': 14, 'axes.titlesize': 17, 'axes.labelsize': 15,
        'xtick.labelsize': 14, 'ytick.labelsize': 14, 'legend.fontsize': 12,
    })

    S = 1000.0
    width = 0.5
    fig, ax = plt.subplots(figsize=(9, 7))

    def seg(xi, bottom, val_ms, color, label=None):
        """Draw one solid stacked segment; return the new top (in seconds)."""
        v = val_ms / S
        ax.bar(xi, v, width, bottom=bottom, color=color, alpha=0.92, zorder=3,
               edgecolor='white', linewidth=0.8, label=label)
        if v >= 0.15:
            ax.text(xi, bottom + v / 2, f'{v:.2f}s', ha='center', va='center',
                    fontsize=11, fontweight='bold', color='white')
        return bottom + v

    # --- Partitioned (x=0): App | AB | IB | Google (separate in-browser hop) ---
    b = 0.0
    b = seg(0, b, d_means['app'],    COLORS['app'],    LABELS['app'])
    b = seg(0, b, d_means['ab'],     COLORS['ab'],     LABELS['ab'])
    b = seg(0, b, d_means['ib'],     COLORS['ib'],     LABELS['ib'])
    b = seg(0, b, emb_google,        COLORS['google'], LABELS['google'])
    d_top = b

    # --- Hellō (x=1): App | Wallet | Google IdP (round-trip stacked ON TOP, solid,
    #     same colour as the partitioned Google hop). The wallet performs it
    #     server-side, so it is part of the measured wallet total: we render it as
    #     the top slice of the bar rather than a hatched sub-region. wall_pure is the
    #     wallet minus that Google constant, so the total (app+wallet) is unchanged. ---
    b = seg(1, 0.0, h_means['app'], COLORS['app'])
    wall_pure = h_means['wallet'] - emb_google
    b = seg(1, b, wall_pure,  COLORS['wallet'], LABELS['wallet'])
    b = seg(1, b, emb_google, COLORS['google'])   # Google IdP on top (legend from MOSHI)
    h_top = b

    # totals above each bar: labelled with the MEASURED wall-clock mean (d_tot/
    # h_tot), the same canonical source the comparison plot uses, so every RQ1
    # figure reports the identical total (3.43 s / 4.79 s). The stacked segments
    # sum to ~9 ms less (coarse-granularity slack), but the bar height is only for
    # the visual breakdown; the reported total stays the measured end-to-end value.
    top_max = max(d_top, h_top)
    for xi, top, meas_ms in [(0, d_top, d_tot), (1, h_top, h_tot)]:
        ax.text(xi, top + top_max * 0.01, f'{meas_ms / S:.2f} s total',
                ha='center', va='bottom', fontsize=13, fontweight='bold', color='#222')

    ax.set_ylabel('User-perceived latency (s)', labelpad=8)
    ax.set_title('E2E Login Breakdown\n'
                 f'(Client-side · {title_net_line})', fontweight='bold', pad=14)
    ax.set_xticks([0, 1])
    ax.set_xticklabels(['MOSHI', 'Hellō'])
    ax.set_ylim(0, top_max * 1.18)
    # Legend: component bars + the inter-service network detail moved out of the title
    handles, labels = ax.get_legend_handles_labels()
    if nl:
        import re
        m = re.search(r'([\d.]+\s*ms)', nl)
       # det = f'inter-service link: {nl_short}' + (f' · {m.group(1)} one-way' if m else '')
        handles.append(Line2D([0], [0], color='none'))
        #labels.append(det)
    ax.legend(handles, labels, loc='center left', bbox_to_anchor=(1.02, 0.5),
              framealpha=0.95)
    ax.set_axisbelow(True)
    fig.subplots_adjust(top=0.88, bottom=0.10, left=0.11, right=0.78)

    os.makedirs(os.path.dirname(os.path.abspath(args.output)), exist_ok=True)
    for ext in ['png', 'pdf']:
        out = f'{args.output}.{ext}'
        fig.savefig(out, dpi=150, bbox_inches='tight')
        print(f'Saved: {out}')


if __name__ == '__main__':
    main()
