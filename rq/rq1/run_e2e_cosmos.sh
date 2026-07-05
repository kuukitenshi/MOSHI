#!/usr/bin/env bash
#
# run_e2e_cosmos.sh: E2E latency (Google OAuth): backend on cosmos, client local
#
# Architecture:
#   cosmos  : tTS + mock_idp + IB + AB  (inter-service traffic stays on cosmos)
#   local   : demo_app (:3000) + browser (Playwright) + measurement scripts
#   tunnels : local:4001/4002/4010/4020 → cosmos:localhost:PORT
#
# The SSH tunnels make every "http(s)://localhost:PORT" redirect in the OAuth
# chain transparently hit the cosmos services, so Google's registered
# redirect_uri (http://localhost:4020/callback/google) works without changes.
# Cosmos logs are streamed live to local logs/ so breakdown.js sees them.
#
# Usage:
#   ./rq/rq1/run_e2e_cosmos.sh                          # both demo (cosmos) + hellō (local)
#   ./rq/rq1/run_e2e_cosmos.sh --only demo              # demo on cosmos only
#   ./rq/rq1/run_e2e_cosmos.sh --only hello             # hellō locally only (no cosmos)
#   ./rq/rq1/run_e2e_cosmos.sh --no-deploy              # server already running
#   ./rq/rq1/run_e2e_cosmos.sh --no-build               # skip cargo build
#   ./rq/rq1/run_e2e_cosmos.sh --iterations 20          # runs per system (default: 20)
#   ./rq/rq1/run_e2e_cosmos.sh --server angainor        # different SSH host
#   (netem `lan` = 8 ms one-way is ON BY DEFAULT for the demo's inter-service paths)
#   ./rq/rq1/run_e2e_cosmos.sh --no-netem               # disable network emulation
#   ./rq/rq1/run_e2e_cosmos.sh --netem wifi             # override profile
#   ./rq/rq1/run_e2e_cosmos.sh --netem mobile
#   ./rq/rq1/run_e2e_cosmos.sh --netem custom 120 25 0.5
#   ./rq/rq1/run_e2e_cosmos.sh --headed                 # show browser (debug)
#   ./rq/rq1/run_e2e_cosmos.sh --skip-setup             # skip session setup
#   ./rq/rq1/run_e2e_cosmos.sh --reset-sessions         # delete saved profiles, redo login
#
# Outputs (same format as run_rq1.sh: compatible with existing plot scripts):
#   rq/out/rq1_demo_runs.csv          per-run latency
#   rq/out/rq1_demo_summary.json      stats summary
#   rq/out/breakdown_demo.csv         per-phase breakdown
#   plots/rq1_e2e_cosmos_latency.{png,pdf}
#   plots/rq1_breakdown_cosmos.{png,pdf}
#
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
RQ_DIR="$ROOT/rq/rq1"
OUT_DIR="$RQ_DIR/out"
LOG_DIR="$ROOT/logs"
PLOTS_DIR="$ROOT/plots"
BREAKDOWN_CSV="$OUT_DIR/breakdown_demo.csv"   # demo per-phase breakdown (used by plot step)

SERVER="cosmos"
REMOTE_DIR="~/opencode_rust_server"
ITERATIONS=20
ONLY=""          # "" = both, "demo" or "hello"
NO_DEPLOY=false
NO_BUILD=false
HEADED_FLAG=""
COLD_HELLO_FLAG=""
HELLO_PROMPT_FLAG=""
HELLO_TAG_FLAG=""
SKIP_SETUP=false
RESET_SESSIONS=false
# Network emulation on the demo's inter-service paths is ON by default with the
# `lan` profile (8 ms one-way), so a plain run reflects a realistic co-located
# deployment rather than a 0 ms localhost. Override with --netem <profile> or turn
# it off with --no-netem.
NETEM_ENABLED=true
NETEM_ARGS=()
NETEM_EXPLICIT=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --server)          SERVER="$2";           shift 2 ;;
    --no-netem)        NETEM_ENABLED=false;   shift ;;
    --remote-dir)      REMOTE_DIR="$2";       shift 2 ;;
    --iterations)      ITERATIONS="$2";       shift 2 ;;
    --only)            ONLY="$2";             shift 2 ;;
    --no-deploy)       NO_DEPLOY=true;        shift ;;
    --no-build)        NO_BUILD=true;         shift ;;
    --headed)          HEADED_FLAG="--headed"; shift ;;
    --cold-hello)      COLD_HELLO_FLAG="--cold-hello"; shift ;;
    --hello-reauth)    HELLO_PROMPT_FLAG="--hello-prompt login"; shift ;;
    --hello-prompt)    HELLO_PROMPT_FLAG="--hello-prompt $2"; shift 2 ;;
    --hello-tag)       HELLO_TAG_FLAG="--tag $2"; shift 2 ;;
    --skip-setup)      SKIP_SETUP=true;       shift ;;
    --reset-sessions)  RESET_SESSIONS=true;   shift ;;
    --netem)
      NETEM_ENABLED=true; shift
      [[ $# -lt 1 ]] && { echo "--netem requires a profile" >&2; exit 1; }
      NETEM_ARGS+=("$1"); shift
      if [[ "${NETEM_ARGS[0]}" == "custom" ]]; then
        [[ $# -lt 2 ]] && { echo "--netem custom requires <lat> <jit> [loss]" >&2; exit 1; }
        NETEM_ARGS+=("$1" "$2"); shift 2
        [[ $# -gt 0 && "$1" != --* ]] && { NETEM_ARGS+=("$1"); shift; }
      fi
      ;;
    -h|--help) sed -n '3,35p' "$0"; exit 0 ;;
    *) echo "Unknown arg: $1" >&2; exit 1 ;;
  esac
done

# Default profile is `lan` when netem is on but no explicit profile was passed.
if [[ "$NETEM_ENABLED" == true && ${#NETEM_ARGS[@]} -eq 0 ]]; then
  NETEM_ARGS=("lan")
fi

mkdir -p "$OUT_DIR" "$LOG_DIR" "$PLOTS_DIR"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
CYAN='\033[0;36m'; BOLD='\033[1m'; NC='\033[0m'

# ── Session reset ─────────────────────────────────────────────────────────────
if [[ "$RESET_SESSIONS" == true ]]; then
  echo -e "${YELLOW}[e2e] Removing saved browser profile...${NC}"
  rm -rf "$RQ_DIR/.profiles/demo"
  echo -e "${GREEN}[e2e] Profile deleted. Will re-run Google login.${NC}"
  echo ""
fi

# ── SSH ControlMaster ─────────────────────────────────────────────────────────
TMP_DIR="$(mktemp -d /tmp/e2e_cosmos_XXXXXX)"
SSH_CTL="$TMP_DIR/ssh_ctl"

DEMO_APP_PID=""
HELLO_PID=""
LOG_STREAM_PIDS=()

cleanup() {
  echo -e "\n${YELLOW}[e2e] Shutting down...${NC}"
  [[ -n "$DEMO_APP_PID" ]] && kill "$DEMO_APP_PID" 2>/dev/null || true
  [[ -n "$HELLO_PID"    ]] && kill "$HELLO_PID"    2>/dev/null || true
  for pid in "${LOG_STREAM_PIDS[@]}"; do kill "$pid" 2>/dev/null || true; done
  lsof -ti:3000 2>/dev/null | xargs -r kill 2>/dev/null || true

  if [[ "$NETEM_ENABLED" == true ]]; then
    ssh -S "$SSH_CTL" "$SERVER" \
      "cd $REMOTE_DIR && NETEM_BACKEND=toxiproxy ./scripts/netem.sh clear 2>/dev/null || true" \
      2>/dev/null || true
  fi
  ssh -S "$SSH_CTL" -O exit "$SERVER" 2>/dev/null || true
  rm -rf "$TMP_DIR"
  echo -e "${GREEN}[e2e] Done.${NC}"
}
trap cleanup EXIT

# ── Node dependencies (needed for both demo and hellō) ───────────────────────
if [[ ! -d "$RQ_DIR/node_modules" ]]; then
  echo -e "${CYAN}[e2e] Installing npm packages...${NC}"
  cd "$RQ_DIR" && npm install --silent && cd "$ROOT"
fi
# Browser only (no --with-deps → no sudo prompt; OS deps already present).
npx --prefix "$RQ_DIR" playwright install chromium >/dev/null 2>&1 || true

NETEM_LABEL=""
[[ "$NETEM_ENABLED" == true ]] && NETEM_LABEL=" [netem: ${NETEM_ARGS[*]}]"

# ══════════════════════════════════════════════════════════════════════════════
# DEMO: backend on cosmos
# ══════════════════════════════════════════════════════════════════════════════
if [[ -z "$ONLY" || "$ONLY" == "demo" ]]; then

  # ── Record the partitioned system's network condition (shown on the plots) ──
  mkdir -p "$OUT_DIR"
  if [[ "$NETEM_ENABLED" == true ]]; then
    case "${NETEM_ARGS[0]}" in
      lan)    echo "LAN (8 ms inter-service)"      > "$OUT_DIR/rq1_net_label.txt" ;;
      wifi)   echo "WiFi (35 ms inter-service)"    > "$OUT_DIR/rq1_net_label.txt" ;;
      mobile) echo "mobile (90 ms inter-service)"  > "$OUT_DIR/rq1_net_label.txt" ;;
      *)      echo "netem ${NETEM_ARGS[*]}"        > "$OUT_DIR/rq1_net_label.txt" ;;
    esac
  else
    echo "localhost via SSH tunnel (no emulation)" > "$OUT_DIR/rq1_net_label.txt"
  fi

  # ── Deploy ──────────────────────────────────────────────────────────────────
  if [[ "$NO_DEPLOY" != true ]]; then
    echo -e "${CYAN}[e2e] Deploying server stack to ${SERVER}...${NC}"
    BUILD_FLAG=""
    [[ "$NO_BUILD" == true ]] && BUILD_FLAG="--no-build"
    bash "$RQ_DIR/deploy_server.sh" --server "$SERVER" --remote-dir "$REMOTE_DIR" $BUILD_FLAG
    echo ""
  fi

  # ── Network emulation on cosmos (optional) ──────────────────────────────────
  if [[ "$NETEM_ENABLED" == true ]]; then
    echo -e "${CYAN}[e2e] Applying netem on ${SERVER}: ${NETEM_ARGS[*]}${NC}"
    NETEM_ARGS_STR="${NETEM_ARGS[*]}"

    ssh -fNM -S "$SSH_CTL" \
      -L 4001:localhost:4001 \
      -L 4002:localhost:4002 \
      -L 4010:localhost:4010 \
      -L 4020:localhost:4020 \
      "$SERVER"

    ssh -S "$SSH_CTL" "$SERVER" bash << REMOTE
set -euo pipefail
cd $REMOTE_DIR

for port in 3001 4001 4002 4010 4020 5001; do
  lsof -ti:\$port 2>/dev/null | xargs -r kill 2>/dev/null || true
done
pkill -f "bin/(tts|mock_idp|ab|ib)" 2>/dev/null || true
sleep 1

NETEM_BACKEND=toxiproxy ./scripts/netem.sh apply $NETEM_ARGS_STR

RUST_LOG=info nohup ./bin/tts > logs/tts.log 2>&1 &
sleep 3

RUST_LOG=info nohup ./bin/mock_idp > logs/mock_idp.log 2>&1 &
sleep 1

RUST_LOG=info \
  MOCK_IDP_AUTHORIZE_URL="https://localhost:4301/authorize" \
  TTS_ISSUE_URL="https://localhost:4501/issue" \
  nohup ./bin/ib > logs/ib.log 2>&1 &
sleep 1

RUST_LOG=info \
  AB_HTTPS_BIND="0.0.0.0:4001" \
  IB_HTTPS_BASE="https://localhost:4402" \
  nohup ./bin/ab > logs/ab.log 2>&1 &
sleep 2

ALL_UP=true
for port in 3001 4001 4002 5001; do
  lsof -i:\$port -sTCP:LISTEN >/dev/null 2>&1 || { echo "port \$port not up"; ALL_UP=false; }
done
[[ "\$ALL_UP" == true ]] && echo "[netem] Services ready." || exit 1
REMOTE

    echo -e "${GREEN}[e2e] Netem active: ${NETEM_ARGS[*]}${NC}"
    echo ""

  else
    echo -e "${CYAN}[e2e] Opening SSH tunnels  local:{4001,4002,4010,4020} → ${SERVER}:localhost:PORT...${NC}"
    ssh -fNM -S "$SSH_CTL" \
      -L 4001:localhost:4001 \
      -L 4002:localhost:4002 \
      -L 4010:localhost:4010 \
      -L 4020:localhost:4020 \
      "$SERVER"
    echo -e "${GREEN}[e2e] Tunnels active.${NC}"
  fi

  # ── Wait for AB via tunnel ───────────────────────────────────────────────────
  echo -e "${CYAN}[e2e] Waiting for AB to respond on :4001...${NC}"
  for _i in $(seq 1 30); do
    if curl -sk "https://localhost:4001/jwks.json" -o /dev/null 2>/dev/null; then
      echo -e "${GREEN}[e2e] AB ready.${NC}"; break
    fi
    sleep 1
    [[ $_i -eq 30 ]] && { echo -e "${RED}[e2e] Timeout waiting for AB.${NC}" >&2; exit 1; }
  done
  echo ""

  # ── Sync the CA cert from cosmos ─────────────────────────────────────────────
  # tTS regenerates the CA on each startup; the local demo_app must trust THAT CA
  # to complete the HTTPS token exchange with AB. Without this, the exchange fails
  # silently (TLS error → error page), phase H is lost, and the login never
  # actually completes.
  echo -e "${CYAN}[e2e] Fetching CA cert from ${SERVER}...${NC}"
  mkdir -p "$ROOT/certs"
  if ssh -S "$SSH_CTL" "$SERVER" "cat ${REMOTE_DIR}/certs/ca.pem" > "$ROOT/certs/ca.pem" 2>/dev/null \
     && [[ -s "$ROOT/certs/ca.pem" ]]; then
    echo -e "${GREEN}[e2e] CA cert synced ($(openssl x509 -in "$ROOT/certs/ca.pem" -noout -fingerprint -sha256 2>/dev/null | cut -d= -f2 | cut -c1-17)...).${NC}"
  else
    echo -e "${RED}[e2e] Failed to fetch CA cert: demo token exchange will fail.${NC}" >&2
  fi
  echo ""

  # ── Stream cosmos logs locally ───────────────────────────────────────────────
  echo -e "${CYAN}[e2e] Streaming cosmos logs to local logs/...${NC}"
  > "$LOG_DIR/ab.log"; > "$LOG_DIR/ib.log"; > "$LOG_DIR/tts.log"
  ssh -S "$SSH_CTL" "$SERVER" "tail -F $REMOTE_DIR/logs/ab.log  2>/dev/null" >> "$LOG_DIR/ab.log"  &
  LOG_STREAM_PIDS+=($!)
  ssh -S "$SSH_CTL" "$SERVER" "tail -F $REMOTE_DIR/logs/ib.log  2>/dev/null" >> "$LOG_DIR/ib.log"  &
  LOG_STREAM_PIDS+=($!)
  ssh -S "$SSH_CTL" "$SERVER" "tail -F $REMOTE_DIR/logs/tts.log 2>/dev/null" >> "$LOG_DIR/tts.log" &
  LOG_STREAM_PIDS+=($!)
  echo -e "${GREEN}[e2e] Log streaming active (ab, ib, tts).${NC}"
  echo ""

  # ── Build + start demo_app locally ──────────────────────────────────────────
  if [[ "$NO_BUILD" != true ]]; then
    echo -e "${CYAN}[e2e] Building demo_app (release)...${NC}"
    cargo build --release --bin demo_app 2>&1
    echo -e "${GREEN}[e2e] Build OK.${NC}"; echo ""
  fi

  echo -e "${CYAN}[e2e] Starting demo_app locally on :3000 (release)...${NC}"
  # Release build: matches the cosmos server stack (deploy_server.sh builds --release),
  # so the demo_app's JWT/FROST verification (phase H) isn't slowed by a debug build.
  # stdbuf -oL/-eL forces line-buffered output so the LAST log line per run
  # ("[Demo App] JWT received successfully") reaches the file before the process
  # is killed: otherwise phase H is lost (block-buffered stdout → N/A).
  NO_COLOR=1 RUST_LOG=info stdbuf -oL -eL cargo run --release --bin demo_app > "$LOG_DIR/demo_app.log" 2>&1 &
  DEMO_APP_PID=$!
  for _i in $(seq 1 20); do
    if curl -s "http://localhost:3000/api/health" -o /dev/null 2>/dev/null; then
      echo -e "${GREEN}[e2e] demo_app ready.${NC}"; break
    fi
    sleep 1
    [[ $_i -eq 20 ]] && { echo -e "${RED}[e2e] demo_app failed to start.${NC}" >&2; exit 1; }
  done
  echo ""

  # ── Google session setup ─────────────────────────────────────────────────────
  DEMO_PROFILE="$RQ_DIR/.profiles/demo"
  DEMO_MARKER="$DEMO_PROFILE/.setup_done"
  if [[ ! -f "$DEMO_MARKER" && "$SKIP_SETUP" != true ]]; then
    echo -e "${YELLOW}[e2e] No saved Google session: opening browser for one-time login...${NC}"
    rm -rf "$DEMO_PROFILE"
    cd "$RQ_DIR" && node setup_sessions.js --system demo && cd "$ROOT"
    echo ""
  else
    echo -e "${GREEN}[e2e] Using saved Google session ($(cat "$DEMO_MARKER" 2>/dev/null | tr -d '\n')).${NC}"
    echo ""
  fi

  # ── Measure ──────────────────────────────────────────────────────────────────
  echo -e "${BOLD}════════════════════════════════════════════════════════════${NC}"
  echo -e "${BOLD}  Demo App: backend on ${SERVER}  |  N=${ITERATIONS}${NETEM_LABEL}${NC}"
  echo -e "${BOLD}════════════════════════════════════════════════════════════${NC}"
  echo ""
  cd "$RQ_DIR"
  node breakdown.js \
    --system demo \
    --runs "$ITERATIONS" \
    --csv "$OUT_DIR/breakdown_demo.csv" \
    $HEADED_FLAG
  cd "$ROOT"

  # ── Stop demo_app ─────────────────────────────────────────────────────────────
  [[ -n "$DEMO_APP_PID" ]] && kill "$DEMO_APP_PID" 2>/dev/null || true
  DEMO_APP_PID=""
  lsof -ti:3000 2>/dev/null | xargs -r kill 2>/dev/null || true
  sleep 1
  echo ""

fi  # end demo

# ══════════════════════════════════════════════════════════════════════════════
# HELLŌ: local Next.js app
# ══════════════════════════════════════════════════════════════════════════════
if [[ -z "$ONLY" || "$ONLY" == "hello" ]]; then

  HELLO_DIR="$ROOT/app_hello_playground"
  if [[ ! -f "$HELLO_DIR/package.json" ]]; then
    echo -e "${RED}[e2e/hello] app_hello_playground not found at $HELLO_DIR${NC}" >&2
    exit 1
  fi

  echo -e "${CYAN}[e2e/hello] Installing hello_playground dependencies...${NC}"
  cd "$HELLO_DIR" && npm install --silent 2>/dev/null || true && cd "$ROOT"

  # Run the RP in PRODUCTION mode (next build && next start), not dev: Next.js dev
  # compiles each route on first hit (JIT, no minify/cache), which inflates the RP
  # phase by hundreds of ms and is not representative: the same way a Rust debug
  # build would inflate the prototype. Fall back to dev only if the build fails.
  # The app's package.json only defines `dev`, so we invoke Next directly via npx
  # (next is a local dependency): `next build` then `next start`.
  echo -e "${CYAN}[e2e/hello] Building hello_playground (production)...${NC}"
  HELLO_LOG="$LOG_DIR/app_hello_playground.log"
  cd "$HELLO_DIR"
  if npx --no-install next build > "$HELLO_LOG" 2>&1; then
    echo -e "${GREEN}[e2e/hello] Build OK: starting production server on :3000...${NC}"
    npx --no-install next start -p 3000 >> "$HELLO_LOG" 2>&1 &
  else
    echo -e "${YELLOW}[e2e/hello] Production build failed: falling back to dev mode (RP phase will be inflated).${NC}"
    npm run dev -- -p 3000 >> "$HELLO_LOG" 2>&1 &
  fi
  HELLO_PID=$!
  cd "$ROOT"

  for _i in $(seq 1 30); do
    if curl -s http://localhost:3000 -o /dev/null 2>/dev/null; then
      echo -e "${GREEN}[e2e/hello] hello_playground ready.${NC}"; break
    fi
    sleep 1
    [[ $_i -eq 30 ]] && { echo -e "${RED}[e2e/hello] Timeout waiting for hello_playground${NC}" >&2; exit 1; }
  done
  echo ""

  echo -e "${BOLD}════════════════════════════════════════════════════════════${NC}"
  echo -e "${BOLD}  Hellō App: local  |  N=${ITERATIONS}${NC}"
  echo -e "${BOLD}════════════════════════════════════════════════════════════${NC}"
  echo -e "${YELLOW}[e2e/hello] Browser will open: log in to Hellō once, then runs automatically.${NC}"
  echo ""
  cd "$RQ_DIR"
  node breakdown.js \
    --system hello \
    --runs "$ITERATIONS" \
    --base-url "http://localhost:3000" \
    --csv "$OUT_DIR/breakdown_hello.csv" \
    $COLD_HELLO_FLAG $HELLO_PROMPT_FLAG $HELLO_TAG_FLAG
  cd "$ROOT"

  kill "$HELLO_PID" 2>/dev/null || true
  HELLO_PID=""
  lsof -ti:3000 2>/dev/null | xargs -r kill 2>/dev/null || true
  sleep 1
  echo ""

fi  # end hello

# ══════════════════════════════════════════════════════════════════════════════
# PHASE 3: Plots
# ══════════════════════════════════════════════════════════════════════════════
PYTHON_BIN="python3"
[[ -x "$ROOT/venv/bin/python" ]] && PYTHON_BIN="$ROOT/venv/bin/python"

NETEM_SUFFIX=""
NETEM_MS=0
if [[ "$NETEM_ENABLED" == true ]]; then
  NETEM_SUFFIX=" (${NETEM_ARGS[*]})"
  # Resolve one-way latency for the reference line in the breakdown plot
  case "${NETEM_ARGS[0]}" in
    lan)    NETEM_MS=8   ;;
    wifi)   NETEM_MS=35  ;;
    mobile) NETEM_MS=90  ;;
    bad)    NETEM_MS=180 ;;
    custom) NETEM_MS="${NETEM_ARGS[1]:-0}" ;;
  esac
fi

# Latency distribution plot (reuse plot_comparison.py if hello data exists)
DEMO_SUMMARY="$OUT_DIR/rq1_demo_summary.json"
HELLO_SUMMARY="$OUT_DIR/rq1_hello_summary.json"

if [[ -f "$DEMO_SUMMARY" && -f "$HELLO_SUMMARY" ]]; then
  echo -e "${CYAN}[e2e] Generating comparison plot (demo vs hellō)...${NC}"
  "$PYTHON_BIN" "$RQ_DIR/plot_comparison.py" \
    --demo  "$DEMO_SUMMARY" \
    --hello "$HELLO_SUMMARY" \
    --output "$PLOTS_DIR/rq1/rq1_comparison" && \
    echo -e "${GREEN}[e2e] Saved: plots/rq1/rq1_comparison.{png,pdf}${NC}" || true
fi

# Breakdown stacked bar (demo + hellō, independent scales)
HELLO_BREAKDOWN="$OUT_DIR/breakdown_hello.csv"
if [[ -f "$BREAKDOWN_CSV" && -f "$HELLO_BREAKDOWN" ]]; then
  echo -e "${CYAN}[e2e] Generating breakdown plot (demo + hellō)...${NC}"
  "$PYTHON_BIN" "$RQ_DIR/plot_breakdown_cosmos.py" \
    --demo        "$BREAKDOWN_CSV" \
    --hello       "$HELLO_BREAKDOWN" \
    --label       "cosmos backend${NETEM_SUFFIX}" \
    --netem-label "${NETEM_ARGS[*]:-}" \
    --netem-ms    "$NETEM_MS" \
    --output      "$PLOTS_DIR/rq1/rq1_breakdown_cosmos" && \
    echo -e "${GREEN}[e2e] Saved: plots/rq1/rq1_breakdown_cosmos.{png,pdf}${NC}" || true
elif [[ -f "$BREAKDOWN_CSV" ]]; then
  echo -e "${YELLOW}[e2e] No hellō data found: run with --only hello first.${NC}"
  echo -e "${YELLOW}[e2e] Skipping breakdown plot.${NC}"
fi

# Browser-side breakdown: SAME measurement method for both systems
TL_DEMO="$OUT_DIR/browser_timeline_demo.csv"
TL_HELLO="$OUT_DIR/browser_timeline_hello.csv"
if [[ -f "$TL_DEMO" && -f "$TL_HELLO" ]]; then
  echo -e "${CYAN}[e2e] Generating detailed browser-side plot (broker split)...${NC}"
  # Use the per-run browser navigation-hop timeline (NOT HAR-union): its per-host
  # buckets sum to each run's total, so the browser_detail totals match the
  # end-to-end comparison plot exactly. (HAR-union over/under-counts vs wall-clock.)
  "$PYTHON_BIN" "$RQ_DIR/plot_browser_detail.py" \
    --demo   "$TL_DEMO" \
    --hello  "$TL_HELLO" \
    --runs "$ITERATIONS" \
    --output "$PLOTS_DIR/rq1/rq1_browser_detail" && \
    echo -e "${GREEN}[e2e] Saved: plots/rq1/rq1_browser_detail.{png,pdf}${NC}" || true
fi

# ── Measure the network (RTT) from THIS client, so the HAR server-side split can
# subtract it (TTFB - RTT ≈ pure server). Running from Japan/VPN/anywhere is fine:
# the RTT is measured from the same place the browser ran, so it cancels out.
# `time_connect` is the TCP handshake ≈ one round-trip. We warm DNS first.
rtt_ms() {  # $1 = url ; prints integer ms, or nothing on failure
  curl -s -o /dev/null -m 8 "$1" 2>/dev/null || true   # warm DNS/route
  local s; s=$(curl -s -o /dev/null -m 8 -w '%{time_connect}' "$1" 2>/dev/null) || return 0
  [[ -n "$s" ]] && awk "BEGIN{printf \"%d\", $s*1000}"
}
echo -e "${CYAN}[e2e] Measuring RTT from this client (for TTFB - RTT)...${NC}"
: "${RTT_WALLET:=$(rtt_ms https://wallet.hello.coop/)}"
: "${RTT_GOOGLE:=$(rtt_ms https://accounts.google.com/)}"
echo "  RTT wallet=${RTT_WALLET:-?}ms  google=${RTT_GOOGLE:-?}ms" \
     | tee "$OUT_DIR/rq1_rtt.txt"
echo ""

# ── Transport vs server-side (TTFB) split, from the HAR captured during the run ──
# breakdown.js records out/har_{demo,hello}.har. This splits each request into
# transport (dns/connect/ssl/send/receive) and server `wait` (TTFB), so we can say
# how much of the latency is network vs server-side: independent of who is faster.
HAR_RTT_ARGS=()
[[ -n "${RTT_WALLET:-}" ]] && HAR_RTT_ARGS+=(--rtt-wallet "$RTT_WALLET")
[[ -n "${RTT_GOOGLE:-}" ]] && HAR_RTT_ARGS+=(--rtt-google "$RTT_GOOGLE")
[[ -n "${RTT_AB:-}"     ]] && HAR_RTT_ARGS+=(--rtt-ab "$RTT_AB")
[[ -n "${RTT_IB:-}"     ]] && HAR_RTT_ARGS+=(--rtt-ib "$RTT_IB")
for sys in demo hello; do
  HAR="$OUT_DIR/har_${sys}.har"
  [[ -f "$HAR" ]] || continue
  echo -e "${CYAN}[e2e] ${sys}: transport vs server (TTFB) split from HAR...${NC}"
  node "$RQ_DIR/analyze_har.js" "$HAR" \
    --csv "$OUT_DIR/har_${sys}_split.csv" ${HAR_RTT_ARGS[@]+"${HAR_RTT_ARGS[@]}"} || true
done

# ── Hellō breakdown plot (demo-style stacked bar), server-side with RTT removed ──
if [[ -f "$OUT_DIR/har_hello.har" ]]; then
  echo -e "${CYAN}[e2e] Plotting Hellō breakdown from HAR (RTT-subtracted)...${NC}"
  "$PYTHON_BIN" "$RQ_DIR/plot_hello_breakdown_har.py" \
    --har "$OUT_DIR/har_hello.har" --runs "$ITERATIONS" \
    ${RTT_WALLET:+--rtt-wallet "$RTT_WALLET"} ${RTT_GOOGLE:+--rtt-google "$RTT_GOOGLE"} \
    --output "$PLOTS_DIR/rq1/rq1_breakdown_hello_har" || true
fi

echo ""
echo -e "${GREEN}${BOLD}════════════════════════════════════════════════════════════${NC}"
echo -e "${GREEN}${BOLD}  DONE${NC}"
echo -e "${GREEN}${BOLD}════════════════════════════════════════════════════════════${NC}"
echo ""
echo "  E2E runs CSV:    $OUT_DIR/rq1_demo_runs.csv"
echo "  Summary JSON:    $OUT_DIR/rq1_demo_summary.json"
echo "  Breakdown CSV:   $OUT_DIR/breakdown_demo.csv"
[[ -f "$PLOTS_DIR/rq1/rq1_comparison.png" ]] && \
  echo "  Comparison plot: $PLOTS_DIR/rq1/rq1_comparison.png"
[[ -f "$PLOTS_DIR/rq1/rq1_breakdown_cosmos.png" ]] && \
  echo "  Breakdown plot:  $PLOTS_DIR/rq1/rq1_breakdown_cosmos.{png,pdf}"
echo ""
