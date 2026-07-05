#!/usr/bin/env bash
# plot_rq1.sh: Regenerate all RQ1 plots from existing data (no measurements)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
RQ1_DIR="$ROOT/rq/rq1"
OUT_DIR="$RQ1_DIR/out"
PLOTS_DIR="$ROOT/plots/rq1"

PYTHON="${ROOT}/venv/bin/python"
[[ -x "$PYTHON" ]] || PYTHON=python3

mkdir -p "$PLOTS_DIR"

echo "── Breakdown (moshi server-side) ────────────────────────"
"$PYTHON" "$RQ1_DIR/plot_breakdown_cosmos.py" \
    --demo  "$OUT_DIR/breakdown_demo.csv" \
    --hello "$OUT_DIR/breakdown_hello.csv" \
    --output "$PLOTS_DIR/rq1_breakdown_moshi"

echo "── Breakdown (Hellō wallet, HAR · RTT-subtracted) ───────"
# Parse the measured RTT so the wallet breakdown shows server-side time only
# (TTFB minus 1 RTT per request); falls back to raw TTFB if the file is absent.
RTT_ARGS=()
if [[ -f "$OUT_DIR/rq1_rtt.txt" ]]; then
    RW=$(grep -oE 'wallet=[0-9.]+' "$OUT_DIR/rq1_rtt.txt" | grep -oE '[0-9.]+' || true)
    RG=$(grep -oE 'google=[0-9.]+' "$OUT_DIR/rq1_rtt.txt" | grep -oE '[0-9.]+' || true)
    [[ -n "${RW:-}" ]] && RTT_ARGS+=(--rtt-wallet "$RW")
    [[ -n "${RG:-}" ]] && RTT_ARGS+=(--rtt-google "$RG")
fi
"$PYTHON" "$RQ1_DIR/plot_hello_breakdown_har.py" \
    --har "$OUT_DIR/har_hello.har" --runs 50 \
    ${RTT_ARGS[@]+"${RTT_ARGS[@]}"} \
    --output "$PLOTS_DIR/rq1_breakdown_hello_har"

echo "── CDF of E2E latency (demo vs hellō) ───────────────────"
"$PYTHON" "$RQ1_DIR/plot_cdf.py" \
    --demo  "$OUT_DIR/rq1_demo_runs.csv" \
    --hello "$OUT_DIR/rq1_hello_runs.csv" \
    --output "$PLOTS_DIR/rq1_cdf"

echo "── Comparison (demo vs hellō) ───────────────────────────"
"$PYTHON" "$RQ1_DIR/plot_comparison.py" \
    --demo  "$OUT_DIR/rq1_demo_summary.json" \
    --hello "$OUT_DIR/rq1_hello_summary.json" \
    --output "$PLOTS_DIR/rq1_comparison"

echo "── Browser-side breakdown: detailed (broker split) ─────"
"$PYTHON" "$RQ1_DIR/plot_browser_detail.py" \
    --demo  "$OUT_DIR/browser_timeline_demo.csv" \
    --hello "$OUT_DIR/browser_timeline_hello.csv" \
    --output "$PLOTS_DIR/rq1_browser_detail"

echo ""
echo "Done. Plots saved to $PLOTS_DIR"
