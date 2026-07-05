#!/usr/bin/env bash
# generate_plots.sh: Regenerate ALL thesis plots from the data already in rq*/out/.
# Re-renders plots only; it does NOT run experiments or SSH to the cluster.
#
#   RQ1 → plots/rq1/  (breakdown_moshi, breakdown_hello_har, comparison, browser_detail)
#   RQ2 → plots/rq2/  (crypto_overhead, quorum_scalability)
#   RQ3 → plots/rq3/  (saturation_vegeta)
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")" && pwd)"
PY="python3"; [[ -x "$ROOT_DIR/venv/bin/python" ]] && PY="$ROOT_DIR/venv/bin/python"
echo "[plots] python: $PY"

# ── RQ1: breakdown (internal stages) + comparison + browser detail ───────────
echo "[plots] RQ1"
bash "$ROOT_DIR/rq/rq1/plot_rq1.sh"

# ── RQ2: crypto overhead + quorum scalability ────────────────────────────────
echo "[plots] RQ2"
RQ2_JSON="$ROOT_DIR/rq/rq2/out/bench_frost_results.json"
RQ2_RUNS="$ROOT_DIR/rq/rq2/out/runs"
# RQ2 carries no figure in the thesis: with four configurations the table says
# everything a bar chart would, so plot_rq2.py runs here for rq2_table.tex (paste
# it into cap5). Aggregate the per-execution runs so the table's ratio ranges are
# the true run-to-run spread (the single JSON has repeats=1 → degenerate).
# rq/rq2/plot_rq2_merged.py still draws that figure if it is ever wanted back.
"$PY" "$ROOT_DIR/rq/rq2/plot_rq2.py"    --runs-dir "$RQ2_RUNS" --output "$ROOT_DIR/plots/rq2/rq2_crypto_overhead"
"$PY" "$ROOT_DIR/rq/rq2/plot_quorum.py" --input "$RQ2_JSON" --output "$ROOT_DIR/plots/rq2/rq2_quorum_scalability"

# ── RQ3: rate ramp: saturation + recovery + server/client CPU ────────────────
echo "[plots] RQ3"
RQ3_OUT="$ROOT_DIR/rq/rq3/out"
RQ3_SRVCPU="$RQ3_OUT/rq3_server_cpu.log"
SRVCPU_ARG=(); [[ -f "$RQ3_SRVCPU" ]] && SRVCPU_ARG=(--server-cpu "$RQ3_SRVCPU")
"$PY" "$ROOT_DIR/rq/rq3/plot_saturation.py" \
  --client1 "$RQ3_OUT/rq3_vegeta_client1.csv" \
  --client2 "$RQ3_OUT/rq3_vegeta_client2.csv" \
  "${SRVCPU_ARG[@]}" \
  --output  "$ROOT_DIR/plots/rq3/rq3_saturation_vegeta"
# Combined saturation + recovery (x = offered rate, up then down) + server CPU.
"$PY" "$ROOT_DIR/rq/rq3/plot_combined.py" \
  --client1 "$RQ3_OUT/rq3_vegeta_client1.csv" \
  --client2 "$RQ3_OUT/rq3_vegeta_client2.csv" \
  "${SRVCPU_ARG[@]}" \
  --output  "$ROOT_DIR/plots/rq3/rq3_saturation_recovery" 2>/dev/null || true

echo "[plots] done: see plots/rq1, plots/rq2, plots/rq3"
