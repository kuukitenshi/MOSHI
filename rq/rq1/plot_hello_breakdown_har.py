#!/usr/bin/env python3
"""
plot_hello_breakdown_har.py: Hellō breakdown from the HAR, in the SAME visual
style as the demo breakdown (plot_breakdown_cosmos.py): one stacked vertical bar,
Tableau palette, in-bar value/percent, monospace legend table below.

Hellō is a black box (no server logs), so the per-phase split is reconstructed
from the HAR captured by breakdown.js. The value plotted is the SERVER-SIDE time:
  - if --rtt-* is given:  server = sum(max(0, wait - RTT))   (network removed)
  - otherwise:            server = sum(wait) = TTFB           (still has 1 RTT in it)
Network (transport) and Google are external/shared costs and are excluded from the
drawn bar (Google is shown in the legend), mirroring the demo plot.

Usage:
  python3 rq/rq1/plot_hello_breakdown_har.py
  python3 rq/rq1/plot_hello_breakdown_har.py --rtt-wallet 110 --rtt-google 20
"""
import argparse
import json
import re
import sys
from pathlib import Path
from urllib.parse import urlparse

try:
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    import matplotlib.patches as mpatches
except ImportError:
    print("pip install matplotlib", file=sys.stderr)
    sys.exit(1)

matplotlib.rcParams.update({
    'font.size': 18, 'axes.titlesize': 20, 'axes.labelsize': 19,
    'xtick.labelsize': 18, 'ytick.labelsize': 18, 'legend.fontsize': 17,
})

ASSET = re.compile(r'\.(js|css|png|jpe?g|gif|svg|woff2?|ico|map|json|txt)(\?|$)'
                   r'|data:|_next/static|/favicon', re.I)
AUTHISH = re.compile(r'hellocoop|authorize|callback|/oauth|/token|oauth2|op=login|/login|wallet|consent|/api/event', re.I)

# (data_key, letter, host, description, ssd_ref, color): same tuple shape and
# Tableau palette as DEMO_PHASES so the two figures read as a matching pair.
HELLO_PHASES = [
    ('app',       'A', '[app]',          'SDK state + glue',       '', '#AF7AA1'),  # purple
    ('authorize', 'B', '[wallet]',       'authorize endpoint',     '', '#EDC949'),  # yellow
    ('consent',   'C', '[wallet]',       'consent / account UI',   '', '#9C755F'),  # brown
    ('idp_redir', 'D', '[wallet↔Google]','IdP redirect handoff',   '', '#59A14F'),  # green
]
# Excluded from the bar (shown in the legend, like Google C/D in the demo plot):
#   - telemetry (/api/event) is a fire-and-forget analytics beacon, off the login
#     critical path, so it must not be counted in the user-perceived breakdown;
#   - Google is the shared external IdP round-trip (server-side inside the wallet).
EXCLUDED_PHASES = [
    ('telemetry', 'E', '[wallet]',       'telemetry (off-path)',   '', '#E15759'),  # red
    ('google',    'F', '[Google]',       'Google OAuth round-trip','', '#4E79A7'),  # blue
]


def classify(url):
    """Map a request URL to a Hellō phase key (or None to drop)."""
    try:
        u = urlparse(url)
        host, path = u.netloc, u.path
    except Exception:
        return None
    if 'google.com' in host:
        return 'google'
    if ':3000' in host:
        return 'app'
    if 'hello.coop' in host or 'hello.dev' in host:
        if '/api/event' in path:
            return 'telemetry'
        if 'consent' in path:
            return 'consent'
        if '/authorize' in path:
            return 'authorize'
        if '/oauth/' in path or '/login/redirect' in path or '/callback' in path:
            return 'idp_redir'
        return 'wallet_etc'
    return None


def pos(v):
    try:
        return v if (v is not None and v > 0) else 0.0
    except TypeError:
        return 0.0


def load(har_path, runs, rtt):
    har = json.loads(Path(har_path).read_text())
    entries = har.get('log', {}).get('entries', [])
    server = {}   # phase -> summed server-side ms (per run later)
    nreq = {}
    for e in entries:
        url = e.get('request', {}).get('url', '')
        status = (e.get('response', {}) or {}).get('status', 0) or 0
        if ASSET.search(url):
            continue
        if not ((300 <= status < 400) or AUTHISH.search(url)):
            continue
        key = classify(url)
        if key is None:
            continue
        wait = pos((e.get('timings', {}) or {}).get('wait'))
        r = rtt.get('google' if key == 'google' else ('app' if key == 'app' else 'wallet'))
        val = max(0.0, wait - r) if r is not None else wait
        server[key] = server.get(key, 0.0) + val
        nreq[key] = nreq.get(key, 0) + 1
    for k in server:
        server[k] /= max(1, runs)
    return server, nreq


# ── demo-style single stacked bar + monospace legend below ──────────────────
def draw_stacked(ax, phases, excluded, data, title, xlabel):
    total = sum(data.get(t[0], 0.0) for t in phases)
    allp = phases + excluded
    host_w = max((len(t[2]) for t in allp), default=6)
    desc_w = max((len(t[3]) for t in allp), default=10)

    def row(letter, host, desc, value):
        return f'{letter}  {host:<{host_w}}  {desc:<{desc_w}}  {value}'

    bottom = 0.0
    legend_items = []
    axis_max = total * 1.08 if total else 1.0
    for key, letter, host, desc, ssd, color in phases:
        val = data.get(key, 0.0)
        if val <= 0:
            legend_items.append((letter, mpatches.Patch(color=color,
                label=row(letter, host, desc, '—'))))
            continue
        ax.bar(0, val, bottom=bottom, color=color, width=0.55,
               edgecolor='white', linewidth=0.6)
        pct = (val / total * 100) if total else 0
        if val / axis_max >= 0.06:
            ax.text(0, bottom + val / 2, f'{letter} · {val:.1f} ms\n({pct:.0f}%)',
                    ha='center', va='center', fontsize=17, color='white', fontweight='bold')
        elif val / axis_max >= 0.025:
            ax.text(0, bottom + val / 2, f'{letter} · {val:.1f} ms ({pct:.0f}%)',
                    ha='center', va='center', fontsize=16, color='white', fontweight='bold')
        legend_items.append((letter, mpatches.Patch(color=color,
            label=row(letter, host, desc, f'{val:7.1f} ms ({pct:>2.0f}%)'))))
        bottom += val

    for key, letter, host, desc, ssd, color in excluded:
        val = data.get(key, 0.0)
        legend_items.append((letter, mpatches.Patch(color=color, alpha=0.45,
            label=row(letter, host, desc,
                      f'{val:7.1f} ms  (excluded, browser-side)'))))

    ax.set_xlim(-0.55, 0.55)
    ax.set_ylim(0, axis_max)
    ax.set_xticks([0])
    ax.set_xticklabels([f'{xlabel}\nTotal: {total:.2f} ms ({total/1000:.2f} s) - Google Excluded'],
                       fontsize=18)
    ax.set_ylabel('Server-side latency (ms)', fontsize=18)
    ax.set_title(title, fontsize=20, fontweight='bold', pad=10)
    ax.yaxis.grid(True, linestyle='--', alpha=0.35)
    ax.set_axisbelow(True)
    legend_items.sort(key=lambda t: t[0])
    leg = ax.legend(handles=[p for _, p in legend_items], loc='upper center',
                    framealpha=0.95, bbox_to_anchor=(0.5, -0.12), ncol=1,
                    borderaxespad=0.0, handlelength=1.3, handleheight=1.3,
                    labelspacing=0.55, alignment='left',
                    prop={'family': 'monospace', 'size': 16})
    leg.get_frame().set_edgecolor('#cccccc')


def main():
    ap = argparse.ArgumentParser()
    here = Path(__file__).resolve().parent
    repo = here.parent.parent
    ap.add_argument('--har', default=str(here / 'out' / 'har_hello.har'))
    ap.add_argument('--runs', type=int, default=20)
    ap.add_argument('--rtt-wallet', type=float, default=None)
    ap.add_argument('--rtt-google', type=float, default=None)
    ap.add_argument('--rtt-app', type=float, default=None)
    ap.add_argument('--output', default=str(repo / 'plots' / 'rq1' / 'rq1_breakdown_hello_har'))
    ap.add_argument('--csv', default=str(here / 'out' / 'breakdown_hello_har.csv'))
    args = ap.parse_args()

    if not Path(args.har).exists():
        print(f"HAR not found: {args.har}\nRun the RQ1 e2e first (records out/har_hello.har).", file=sys.stderr)
        sys.exit(1)

    rtt = {k: v for k, v in
           {'wallet': args.rtt_wallet, 'google': args.rtt_google, 'app': args.rtt_app}.items()
           if v is not None}
    data, nreq = load(args.har, args.runs, rtt)

    csv_path = Path(args.csv)
    csv_path.parent.mkdir(parents=True, exist_ok=True)
    with open(csv_path, 'w') as f:
        f.write('phase,reqs_total,server_ms_per_run,rtt_subtracted\n')
        for key, letter, host, desc, ssd, color in HELLO_PHASES + EXCLUDED_PHASES:
            sub = 'yes' if (('wallet' in rtt and key not in ('app', 'google'))
                            or (key == 'app' and 'app' in rtt)
                            or (key == 'google' and 'google' in rtt)) else 'no'
            f.write(f"{key},{nreq.get(key,0)},{data.get(key,0.0):.1f},{sub}\n")
    print(f"  CSV: {csv_path}")

    suffix = '\n(Server-side · returning user · n=50 · RTT-subtracted)' if rtt else '  (TTFB: still includes 1 RTT/req)'
    fig, ax = plt.subplots(figsize=(12, 11))
    draw_stacked(ax, HELLO_PHASES, EXCLUDED_PHASES, data,
                 title=f'Hellō Breakdown{suffix}', xlabel='Hellō App')
    out = Path(args.output)
    out.parent.mkdir(parents=True, exist_ok=True)
    fig.tight_layout()
    fig.savefig(f"{out}.png", dpi=150, bbox_inches='tight')
    fig.savefig(f"{out}.pdf", bbox_inches='tight')
    print(f"  Saved: {out}.png / .pdf")
    if not rtt:
        print("  NOTE: no --rtt-* given → bars are TTFB (server + 1 RTT/req). Pass --rtt-wallet <ms> for pure server.")


if __name__ == '__main__':
    main()
