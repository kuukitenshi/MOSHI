#!/usr/bin/env bash
#
# run.sh: RQ3 ready-to-run wrapper with the cluster flags baked in.
#
# Runs the saturation + recovery ramp against the standard cluster:
#   server = cosmos   clients = ngstorage + vitamina01
#
#   → plots/rq3/rq3_saturation_vegeta.{png,pdf}
#     one figure (vs time): throughput offered-vs-achieved + ceiling, latency,
#     errors (up under overload, back down on recovery), client-CPU proof
#
# Usage:
#   ./rq/rq3/run.sh                 # full ramp + figure
#   ./rq/rq3/run.sh --duration 12   # any run_rq3.sh flag is passed straight through
#   ./rq/rq3/run.sh --rate-end 9000 # push the peak further (watch client CPU!)
#
set -euo pipefail

RQ_DIR="$(cd "$(dirname "$0")" && pwd)"

# ── Cluster + sweep settings (edit here if the hosts change) ──────────────────
SERVER="cosmos"
CLIENT1="ngstorage"
CLIENT2="vitamina01"
DURATION=8
# Per client → combined ×2. Capped at 7000/client (14000 combined): far enough
# past the knee to show the plateau + drops, but not so far that the clients
# saturate and become the bottleneck themselves.
RATE_START=1000; RATE_END=7000; RATE_STEP=1000

exec "$RQ_DIR/run_rq3.sh" \
  --server "$SERVER" --client1 "$CLIENT1" --client2 "$CLIENT2" \
  --duration "$DURATION" \
  --rate-start "$RATE_START" --rate-end "$RATE_END" --rate-step "$RATE_STEP" \
  "$@"
