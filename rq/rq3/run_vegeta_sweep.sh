#!/usr/bin/env bash
#
# run_vegeta_sweep.sh: RQ3 saturation sweep with vegeta (rate-based, keep-alive)
#
# Drives two client nodes hammering one server at a progressively higher REQUEST
# RATE (req/s) until the server saturates (achieved throughput plateaus below the
# offered rate, latency climbs, errors appear). This replaces the curl+xargs
# generator, which could not exceed ~25 real concurrent connections.
#
# Assumes the server binaries are already built/deployed on the server
# (~/opencode_rust_server/bin). By default it (re)starts the server stack.
#
# Usage:
#   ./rq/rq3/run_vegeta_sweep.sh \
#     --server cosmos --client1 ngstorage --client2 vitamina01 \
#     --start 500 --end 8000 --step 500 --duration 10
#
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
RQ_DIR="$ROOT/rq/rq3"
OUT_DIR="$RQ_DIR/out"
mkdir -p "$OUT_DIR"

SERVER_SSH=""; CLIENT1_SSH=""; CLIENT2_SSH=""
R_START=500; R_END=8000; R_STEP=500; DURATION=10
CLIENT2_MULT=1
NO_START=false
REMOTE_DIR="~/opencode_rust_server"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --server)       SERVER_SSH="$2";   shift 2 ;;
    --client1)      CLIENT1_SSH="$2";  shift 2 ;;
    --client2)      CLIENT2_SSH="$2";  shift 2 ;;
    --start)        R_START="$2";      shift 2 ;;
    --end)          R_END="$2";        shift 2 ;;
    --step)         R_STEP="$2";       shift 2 ;;
    --duration)     DURATION="$2";     shift 2 ;;
    --client2-mult) CLIENT2_MULT="$2"; shift 2 ;;
    --no-start)     NO_START=true;     shift ;;
    --remote-dir)   REMOTE_DIR="$2";   shift 2 ;;
    *) echo "Unknown: $1" >&2; exit 1 ;;
  esac
done

[[ -z "$SERVER_SSH" || -z "$CLIENT1_SSH" || -z "$CLIENT2_SSH" ]] && {
  echo "Usage: $0 --server H --client1 H --client2 H [--start N --end N --step N --duration S --client2-mult M --no-start]" >&2
  exit 1
}

RED='\033[0;31m'; GREEN='\033[0;32m'; CYAN='\033[0;36m'; BOLD='\033[1m'; NC='\033[0m'

echo -e "${BOLD}════════════════════════════════════════════════════════════${NC}"
echo -e "${BOLD}  RQ3: Vegeta Saturation Sweep (rate-based)${NC}"
echo -e "${BOLD}  Server: ${SERVER_SSH}  Clients: ${CLIENT1_SSH} + ${CLIENT2_SSH}${NC}"
echo -e "${BOLD}  Rate/client: ${R_START}→${R_END} step ${R_STEP} req/s  (client2 ×${CLIENT2_MULT})  ${DURATION}s/level${NC}"
echo -e "${BOLD}════════════════════════════════════════════════════════════${NC}\n"

# ── Start server stack ─────────────────────────────────────────────────────
if [[ "$NO_START" != true ]]; then
  echo -e "${CYAN}[vegeta] Starting server stack on ${SERVER_SSH}...${NC}"
  ssh "$SERVER_SSH" bash <<REMOTE
set -uo pipefail
cd ${REMOTE_DIR}
# Raise the open-file limit so the broker can hold thousands of concurrent
# connections instead of wedging at the default 1024 soft limit. The hard limit
# is ~1M, so this needs no sudo. All binaries launched below inherit it.
ulimit -n 65535 2>/dev/null || true
for port in 3001 4001 4002 4010 4020 5001; do
  lsof -ti:\$port 2>/dev/null | xargs -r kill 2>/dev/null || true
done
pkill -f "bin/(tts|mock_idp|ab|ib)" 2>/dev/null || true
sleep 1
chmod +x bin/* scripts/*.sh 2>/dev/null || true
mkdir -p logs
# RQ3 ONLY: MOSHI_LONGPOLL=1 switches the tTS->AB delivery to long-poll (AB pulls
# from tTS /await) instead of the default reentrant push. Every other script
# (deploy_server.sh, run.sh) leaves it unset, so the normal system stays on push.
RUST_LOG=warn MOSHI_LONGPOLL=1 nohup ./bin/tts > logs/tts.log 2>&1 &
sleep 3
RUST_LOG=warn nohup ./bin/mock_idp > logs/mock_idp.log 2>&1 &
sleep 1
RUST_LOG=warn MOCK_IDP_AUTHORIZE_URL="https://localhost:3001/authorize" TTS_ISSUE_URL="https://localhost:5001/issue" nohup ./bin/ib > logs/ib.log 2>&1 &
sleep 1
RUST_LOG=warn MOSHI_LONGPOLL=1 TTS_BASE="https://localhost:5001" AB_HTTPS_BIND="0.0.0.0:4001" IB_HTTPS_BASE="https://localhost:4002" nohup ./bin/ab > logs/ab.log 2>&1 &
sleep 2
for p in 3001 4001 4002 5001; do
  lsof -i:\$p -sTCP:LISTEN >/dev/null 2>&1 || { echo "[remote] port \$p NOT listening" >&2; }
done
echo "[remote] server stack started."
REMOTE
  echo ""
fi

# ── Server IP ──────────────────────────────────────────────────────────────
SERVER_IP=$(ssh "$SERVER_SSH" "hostname -I | awk '{print \$1}'")
echo -e "${GREEN}[vegeta] Server IP: ${SERVER_IP}${NC}"

# ── Verify reachable from client1 ──────────────────────────────────────────
TEST=$(ssh "$CLIENT1_SSH" "curl -sk -o /dev/null -w '%{http_code}' -X POST 'https://${SERVER_IP}:4001/login' -H 'Content-Type: application/json' -d '{\"app_id\":\"app_example_456\"}'" 2>/dev/null || echo 000)
[[ "$TEST" != "200" ]] && { echo -e "${RED}[vegeta] Server not reachable from ${CLIENT1_SSH} (HTTP $TEST)${NC}" >&2; exit 1; }
echo -e "${GREEN}[vegeta] Server OK (HTTP 200 from ${CLIENT1_SSH})${NC}\n"

# ── Distribute vegeta binary + client script ───────────────────────────────
echo -e "${CYAN}[vegeta] Distributing vegeta + client script...${NC}"
for C in "$CLIENT1_SSH" "$CLIENT2_SSH"; do
  ssh "$C" "test -x /tmp/vegeta" 2>/dev/null || {
    [[ -x /tmp/vegeta ]] && scp -q /tmp/vegeta "${C}:/tmp/vegeta" && ssh "$C" "chmod +x /tmp/vegeta" || {
      echo -e "${RED}[vegeta] /tmp/vegeta missing locally and on ${C}.${NC}" >&2; exit 1; }
  }
  scp -q "$RQ_DIR/load_vegeta_client.sh" "${C}:/tmp/load_vegeta_client.sh"
  ssh "$C" "chmod +x /tmp/load_vegeta_client.sh"
done
# server-side CPU sampler (records "<epoch> <cpu%>" each second during the run)
scp -q "$RQ_DIR/server_cpu_sampler.sh" "${SERVER_SSH}:/tmp/server_cpu_sampler.sh"
ssh "$SERVER_SSH" "chmod +x /tmp/server_cpu_sampler.sh"

# ── Run both clients in lockstep ───────────────────────────────────────────
S2_START=$(( R_START * CLIENT2_MULT ))
S2_END=$((   R_END   * CLIENT2_MULT ))
S2_STEP=$((  R_STEP  * CLIENT2_MULT ))
echo -e "${CYAN}[vegeta] client1 ${R_START}→${R_END}/${R_STEP}  |  client2 (×${CLIENT2_MULT}) ${S2_START}→${S2_END}/${S2_STEP}${NC}\n"

# The client ramps the offered rate UP past saturation and back DOWN (--recovery),
# so the single figure shows both saturation and recovery (see plot_saturation.py).
C1_REMOTE="/tmp/vegeta_client1.csv"; C2_REMOTE="/tmp/vegeta_client2.csv"
LOCAL1="$OUT_DIR/rq3_vegeta_client1.csv"; LOCAL2="$OUT_DIR/rq3_vegeta_client2.csv"

# Start the server CPU sampler in the background for the whole run.
SRV_CPU_REMOTE="/tmp/server_cpu.log"
ssh "$SERVER_SSH" "nohup bash /tmp/server_cpu_sampler.sh $SRV_CPU_REMOTE >/dev/null 2>&1 & echo \$! > /tmp/server_cpu.pid"

ssh "$CLIENT1_SSH" \
  "/tmp/load_vegeta_client.sh --server ${SERVER_IP} --id client1 --out ${C1_REMOTE} \
   --start ${R_START} --end ${R_END} --step ${R_STEP} --duration ${DURATION} --recovery" &
PID1=$!
ssh "$CLIENT2_SSH" \
  "/tmp/load_vegeta_client.sh --server ${SERVER_IP} --id client2 --out ${C2_REMOTE} \
   --start ${S2_START} --end ${S2_END} --step ${S2_STEP} --duration ${DURATION} --recovery" &
PID2=$!

wait $PID1; E1=$?
wait $PID2; E2=$?
[[ $E1 -ne 0 || $E2 -ne 0 ]] && echo -e "${RED}[vegeta] a client exited non-zero (c1=$E1 c2=$E2)${NC}" >&2

# Stop the server CPU sampler.
ssh "$SERVER_SSH" "kill \$(cat /tmp/server_cpu.pid) 2>/dev/null; rm -f /tmp/server_cpu.pid" 2>/dev/null || true

# ── Collect ────────────────────────────────────────────────────────────────
scp -q "${CLIENT1_SSH}:${C1_REMOTE}" "$LOCAL1"
scp -q "${CLIENT2_SSH}:${C2_REMOTE}" "$LOCAL2"
scp -q "${SERVER_SSH}:${SRV_CPU_REMOTE}" "$OUT_DIR/rq3_server_cpu.log" 2>/dev/null || true
echo -e "\n${GREEN}[vegeta] CSVs: ${LOCAL1##*/}, ${LOCAL2##*/}  + rq3_server_cpu.log${NC}"

# ── Plot ───────────────────────────────────────────────────────────────────
PYTHON_BIN="python3"; [[ -x "$ROOT/venv/bin/python" ]] && PYTHON_BIN="$ROOT/venv/bin/python"
mkdir -p "$ROOT/plots/rq3"
if "$PYTHON_BIN" -c "import matplotlib" 2>/dev/null; then
  "$PYTHON_BIN" "$RQ_DIR/plot_saturation.py" \
    --client1 "$LOCAL1" --client2 "$LOCAL2" \
    --server-cpu "$OUT_DIR/rq3_server_cpu.log" \
    --output  "$ROOT/plots/rq3/rq3_saturation_vegeta" && \
    echo -e "${GREEN}[vegeta] Plot: plots/rq3/rq3_saturation_vegeta.{png,pdf}${NC}"
  "$PYTHON_BIN" "$RQ_DIR/plot_combined.py" \
    --client1 "$LOCAL1" --client2 "$LOCAL2" \
    --server-cpu "$OUT_DIR/rq3_server_cpu.log" \
    --output "$ROOT/plots/rq3/rq3_saturation_recovery" && \
    echo -e "${GREEN}[vegeta] Plot: plots/rq3/rq3_saturation_recovery.{png,pdf}${NC}"
fi
