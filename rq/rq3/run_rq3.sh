#!/usr/bin/env bash
#
# run_rq3.sh: RQ3 experiment in one shot (saturation + recovery ramp).
#
# Two clients ramp the offered REQUEST RATE UP past saturation and then back
# DOWN, producing the single RQ3 figure (vs sweep step = time):
#   plots/rq3/rq3_saturation_vegeta.{png,pdf}
#     panel 1 throughput (offered vs achieved + ceiling) · panel 2 latency ·
#     panel 3 errors (rise under overload, fall on recovery) ·
#     panel 4 client CPU (proof the bottleneck is the server, not the generators)
#
# The peak rate is capped (default 7000/client) so the clients stay well below
# saturation (CPU < ~45%): pushing far past the knee forces each client to juggle
# a huge backlog of connections, which contaminates the measurement.
#
# Usage:
#   ./rq/rq3/run_rq3.sh --server cosmos --client1 ngstorage --client2 vitamina01
#   ./rq/rq3/run_rq3.sh --server cosmos --client1 ngstorage --client2 vitamina01 \
#       --duration 8 --rate-start 1000 --rate-end 7000 --rate-step 1000
#
set -euo pipefail

RQ_DIR="$(cd "$(dirname "$0")" && pwd)"

SERVER=""; C1=""; C2=""
DURATION=8
RATE_START=1000; RATE_END=7000; RATE_STEP=1000    # per client (combined = ×2)
REMOTE_DIR="~/opencode_rust_server"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --server)      SERVER="$2";      shift 2 ;;
    --client1)     C1="$2";          shift 2 ;;
    --client2)     C2="$2";          shift 2 ;;
    --duration)    DURATION="$2";    shift 2 ;;
    --rate-start)  RATE_START="$2";  shift 2 ;;
    --rate-end)    RATE_END="$2";    shift 2 ;;
    --rate-step)   RATE_STEP="$2";   shift 2 ;;
    --remote-dir)  REMOTE_DIR="$2";  shift 2 ;;
    -h|--help)     sed -n '3,20p' "$0"; exit 0 ;;
    *) echo "Unknown: $1" >&2; exit 1 ;;
  esac
done

[[ -z "$SERVER" || -z "$C1" || -z "$C2" ]] && {
  echo "Usage: $0 --server H --client1 H --client2 H [--duration S]" >&2
  echo "          [--rate-start N --rate-end N --rate-step N] [--remote-dir P]" >&2
  exit 1
}

BOLD='\033[1m'; CYAN='\033[0;36m'; GREEN='\033[0;32m'; NC='\033[0m'

echo -e "${BOLD}════════════════════════════════════════════════════════════${NC}"
echo -e "${BOLD}  RQ3: rate-based saturation sweep${NC}"
echo -e "${BOLD}  server=${SERVER}  clients=${C1}+${C2}  ${DURATION}s/level${NC}"
echo -e "${BOLD}════════════════════════════════════════════════════════════${NC}\n"

echo -e "${CYAN}[rq3] Saturation + recovery ramp (offered rate up past saturation, then down) …${NC}"
"$RQ_DIR/run_vegeta_sweep.sh" \
  --server "$SERVER" --client1 "$C1" --client2 "$C2" \
  --start "$RATE_START" --end "$RATE_END" --step "$RATE_STEP" \
  --duration "$DURATION" --remote-dir "$REMOTE_DIR"

echo -e "\n${GREEN}[rq3] Done.${NC}"
echo "  plots/rq3/rq3_saturation_vegeta.{png,pdf}  (saturation + recovery, one figure)"
echo "  data:  rq/rq3/out/rq3_vegeta_client{1,2}.csv"
