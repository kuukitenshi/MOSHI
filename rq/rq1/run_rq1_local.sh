#!/usr/bin/env bash
#
# run_rq1_local.sh: RQ1: End-to-end latency comparison
#
# Compares wall-clock login latency between:
#   - demo_app   (our partitioned broker: AB → IB → Google → tTS → JWT)
#   - hello_playground (Hellō service:   app → wallet.hello.coop → JWT)
#
# Usage:
#   ./rq/run_rq1_local.sh                          # full run (setup + 20 iterations each)
#   ./rq/run_rq1_local.sh --iterations 10          # fewer iterations
#   ./rq/run_rq1_local.sh --no-build               # skip cargo build
#   ./rq/run_rq1_local.sh --only demo              # only measure demo_app
#   ./rq/run_rq1_local.sh --only hello             # only measure hello_playground
#   ./rq/run_rq1_local.sh --headed                 # visible browser (useful for debugging)
#   ./rq/run_rq1_local.sh --skip-setup             # skip setup even if profile is missing
#   ./rq/run_rq1_local.sh --reset-sessions         # delete saved profiles and redo setup
#
# FIRST RUN: the script detects missing browser profiles and runs setup_sessions.js
# (a browser window opens). Log in once. All subsequent runs are fully automated.
# If setup was interrupted, delete rq/.profiles/ and run again.
#
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
RQ_DIR="$ROOT/rq/rq1"
LOG_DIR="$ROOT/logs"
OUT_DIR="$RQ_DIR/out"


# ── Defaults ──────────────────────────────────────────────────────────────
ITERATIONS=20
NO_BUILD=false
ONLY=""          # "" = both, "demo" or "hello"
HEADED_FLAG=""
HELLO_HEADED_FLAG=""
SKIP_SETUP=false
RESET_SESSIONS=false



# hello_playground must run on :3000 to match the redirect_uri registered with Hellō.
# No conflict because demo_app services are fully stopped before Phase 2 starts.

while [[ $# -gt 0 ]]; do
  case "$1" in
    --iterations)  ITERATIONS="$2"; shift 2 ;;
    --no-build)    NO_BUILD=true; shift ;;
    --only)        ONLY="$2"; shift 2 ;;
    --headed)       HEADED_FLAG="--headed"; HELLO_HEADED_FLAG="--headed"; shift ;;
    --hello-headed) HELLO_HEADED_FLAG="--headed"; shift ;;
    --skip-setup)      SKIP_SETUP=true; shift ;;
    --reset-sessions)  RESET_SESSIONS=true; shift ;;
    -h|--help)
      sed -n '3,25p' "$0"
      exit 0
      ;;
    *) echo "[rq1] Unknown argument: $1" >&2; exit 1 ;;
  esac
done

# ── Reset sessions if requested ───────────────────────────────────────────
if [[ "$RESET_SESSIONS" == true ]]; then
  echo "Removing saved browser profiles..."
  rm -rf "$ROOT/rq/rq1/.profiles"
  echo "Profiles deleted. Will run setup on next phase."
  echo ""
fi

# ── Colors ────────────────────────────────────────────────────────────────
RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
CYAN='\033[0;36m'; BOLD='\033[1m'; NC='\033[0m'

DEMO_PIDS=()
HELLO_PID=""

cleanup() {
  echo ""
  echo -e "${YELLOW}[rq1] Stopping services...${NC}"
  for pid in "${DEMO_PIDS[@]}"; do kill "$pid" 2>/dev/null || true; done
  [[ -n "$HELLO_PID" ]] && kill "$HELLO_PID" 2>/dev/null || true
  for port in 3000 3001 4001 4002 4010 4020 5001 8080; do
    lsof -ti:"$port" 2>/dev/null | xargs -r kill 2>/dev/null || true
  done
  wait 2>/dev/null || true
  echo -e "${GREEN}[rq1] Done.${NC}"
}
trap cleanup EXIT

# ── Dependency check ──────────────────────────────────────────────────────
echo -e "${CYAN}[rq1] Checking dependencies...${NC}"

if ! command -v node >/dev/null 2>&1; then
  echo -e "${RED}[rq1] node is required. Install Node.js first.${NC}" >&2
  exit 1
fi

cd "$RQ_DIR"
if [[ ! -d node_modules ]]; then
  echo -e "${CYAN}[rq1] Installing npm packages...${NC}"
  npm install --silent
fi

if ! node -e "require('playwright')" 2>/dev/null; then
  echo -e "${RED}[rq1] playwright module not found. Run: cd rq && npm install${NC}" >&2
  exit 1
fi

# Install chromium browser if not already installed
if ! npx playwright --version >/dev/null 2>&1; then
  echo -e "${CYAN}[rq1] Installing Playwright chromium...${NC}"
  npx playwright install chromium
else
  # Silently install chromium if the binary is missing
  npx playwright install chromium --with-deps >/dev/null 2>&1 || true
fi

cd "$ROOT"
mkdir -p "$LOG_DIR"

# ── Kill stale processes ──────────────────────────────────────────────────
echo -e "${YELLOW}[rq1] Cleaning up old processes...${NC}"
for port in 3000 3001 4001 4002 4010 4020 5001 8080; do
  lsof -ti:"$port" 2>/dev/null | xargs -r kill 2>/dev/null || true
done
pkill -f "target/debug/(tts|mock_idp|ab|ib|web_ui|demo_app|mock_app)" 2>/dev/null || true
sleep 1

# ── Build ─────────────────────────────────────────────────────────────────
if [[ "$NO_BUILD" != true ]]; then
  echo -e "${CYAN}[rq1] Building workspace (cargo build)...${NC}"
  cargo build 2>&1
  echo -e "${GREEN}[rq1] Build OK${NC}"
fi

echo ""
echo -e "${BOLD}════════════════════════════════════════════════════════════${NC}"
echo -e "${BOLD}  RQ1: End-to-End Latency Comparison (N=${ITERATIONS} per system)  ${NC}"
echo -e "${BOLD}════════════════════════════════════════════════════════════${NC}"
echo ""

# ════════════════════════════════════════════════════════════════
# PHASE 1: demo_app (Partitioned Broker)
# ════════════════════════════════════════════════════════════════
if [[ -z "$ONLY" || "$ONLY" == "demo" ]]; then

  echo -e "${CYAN}[rq1/demo] Starting microservices...${NC}"

  NO_COLOR=1 RUST_LOG=info cargo run --bin tts > "$LOG_DIR/tts.log" 2>&1 &
  DEMO_PIDS+=($!)
  sleep 2

  NO_COLOR=1 RUST_LOG=info cargo run --bin mock_idp > "$LOG_DIR/mock_idp.log" 2>&1 &
  DEMO_PIDS+=($!)
  sleep 1

  NO_COLOR=1 RUST_LOG=info \
  MOCK_IDP_AUTHORIZE_URL="https://localhost:3001/authorize" \
  TTS_ISSUE_URL="https://localhost:5001/issue" \
  cargo run --bin ib > "$LOG_DIR/ib.log" 2>&1 &
  DEMO_PIDS+=($!)
  sleep 1

  NO_COLOR=1 RUST_LOG=info \
  IB_HTTPS_BASE="https://localhost:4002" \
  cargo run --bin ab > "$LOG_DIR/ab.log" 2>&1 &
  DEMO_PIDS+=($!)
  sleep 1

  NO_COLOR=1 RUST_LOG=info cargo run --bin demo_app > "$LOG_DIR/demo_app.log" 2>&1 &
  DEMO_PIDS+=($!)
  sleep 2

  # Verify services
  ALL_UP=true
  for port in 4001 4002 4010 4020 5001 3000; do
    if ! lsof -i:"$port" -sTCP:LISTEN >/dev/null 2>&1; then
      echo -e "${RED}[rq1/demo] Port $port not listening${NC}" >&2
      ALL_UP=false
    fi
  done

  if [[ "$ALL_UP" != true ]]; then
    echo -e "${RED}[rq1/demo] Not all services started. Check logs/*.log${NC}" >&2
    exit 1
  fi
  echo -e "${GREEN}[rq1/demo] All services ready.${NC}"

  # Setup session if missing (.setup_done is only written after a successful login)
  DEMO_PROFILE="$RQ_DIR/.profiles/demo"
  DEMO_MARKER="$DEMO_PROFILE/.setup_done"
  if [[ ! -f "$DEMO_MARKER" ]] && [[ "$SKIP_SETUP" != true ]]; then
    echo ""
    echo -e "${YELLOW}[rq1/demo] No valid session found. Running one-time setup...${NC}"
    echo -e "${YELLOW}[rq1/demo] A browser window will open. Log in with Google.${NC}"
    echo -e "${YELLOW}[rq1/demo] After login succeeds, the browser closes automatically.${NC}"
    echo ""
    rm -rf "$DEMO_PROFILE"   # clean any partial profile from a previous failed attempt
    cd "$RQ_DIR" && node setup_sessions.js --system demo && cd "$ROOT"
    echo ""
  else
    echo -e "${GREEN}[rq1/demo] Reusing saved session ($(cat "$DEMO_MARKER" 2>/dev/null | tr -d '\n')).${NC}"
  fi

  # Measure: breakdown.js captures per-phase timing AND writes runs CSV + summary JSON
  echo -e "${CYAN}[rq1/demo] Running ${ITERATIONS} breakdown iterations...${NC}"
  echo ""
  cd "$RQ_DIR" && node breakdown.js \
    --system demo \
    --runs "$ITERATIONS" \
    --csv "$OUT_DIR/breakdown_demo.csv" \
    $HEADED_FLAG
  cd "$ROOT"

  # Stop demo_app services
  echo -e "${YELLOW}[rq1/demo] Stopping microservices...${NC}"
  for pid in "${DEMO_PIDS[@]}"; do kill "$pid" 2>/dev/null || true; done
  DEMO_PIDS=()
  for port in 3000 4001 4002 4010 4020 5001; do
    lsof -ti:"$port" 2>/dev/null | xargs -r kill 2>/dev/null || true
  done
  sleep 1
  echo ""
fi

# ════════════════════════════════════════════════════════════════
# PHASE 2: hello_playground (Hellō service)
# ════════════════════════════════════════════════════════════════
if [[ -z "$ONLY" || "$ONLY" == "hello" ]]; then

  HELLO_DIR="$ROOT/app_hello_playground"

  if [[ ! -f "$HELLO_DIR/package.json" ]]; then
    echo -e "${RED}[rq1/hello] app_hello_playground not found at $HELLO_DIR${NC}" >&2
    exit 1
  fi

  echo -e "${CYAN}[rq1/hello] Installing hello_playground dependencies...${NC}"
  cd "$HELLO_DIR"
  npm install --silent 2>/dev/null || true
  cd "$ROOT"

  echo -e "${CYAN}[rq1/hello] Starting hello_playground (Next.js dev on :3000)...${NC}"
  cd "$HELLO_DIR"
  # Run Next.js on port 3000 (must match Hellō redirect_uri registration)
  HELLO_LOG="$LOG_DIR/app_hello_playground.log"
  npm run dev -- -p 3000 > "$HELLO_LOG" 2>&1 &
  HELLO_PID=$!
  cd "$ROOT"

  # Wait for Next.js to be ready
  echo -e "${CYAN}[rq1/hello] Waiting for Next.js to start...${NC}"
  for i in $(seq 1 30); do
    if curl -s http://localhost:3000 -o /dev/null 2>/dev/null; then
      echo -e "${GREEN}[rq1/hello] hello_playground ready.${NC}"
      break
    fi
    sleep 1
    if [[ $i -eq 30 ]]; then
      echo -e "${RED}[rq1/hello] Timeout waiting for hello_playground${NC}" >&2
      exit 1
    fi
  done

  # Measure: breakdown.js captures per-phase timing AND writes runs CSV + summary JSON
  echo -e "${CYAN}[rq1/hello] Running ${ITERATIONS} breakdown iterations...${NC}"
  echo -e "${YELLOW}[rq1/hello] Browser will open: log in to Hellō once, then it runs automatically.${NC}"
  echo ""
  cd "$RQ_DIR" && node breakdown.js \
    --system hello \
    --runs "$ITERATIONS" \
    --base-url "http://localhost:3000" \
    --csv "$OUT_DIR/breakdown_hello.csv"
  cd "$ROOT"

  # Stop hello_playground
  echo -e "${YELLOW}[rq1/hello] Stopping hello_playground...${NC}"
  kill "$HELLO_PID" 2>/dev/null || true
  HELLO_PID=""
  lsof -ti:3000 2>/dev/null | xargs -r kill 2>/dev/null || true
  sleep 1
  echo ""
fi

# ════════════════════════════════════════════════════════════════
# PHASE 3: Comparison Table
# ════════════════════════════════════════════════════════════════

OUT_DIR="$RQ_DIR/out"
mkdir -p "$OUT_DIR"
DEMO_SUMMARY="$OUT_DIR/rq1_demo_summary.json"
HELLO_SUMMARY="$OUT_DIR/rq1_hello_summary.json"
COMPARISON_OUT="$OUT_DIR/rq1_comparison.txt"

if [[ -f "$DEMO_SUMMARY" && -f "$HELLO_SUMMARY" ]]; then
  {
  echo ""
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "  RQ1: End-to-End Latency Comparison (Google IdP)"
  echo "  Generated: $(date '+%Y-%m-%d %H:%M:%S')"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo ""
  node - "$DEMO_SUMMARY" "$HELLO_SUMMARY" <<'JSEOF'
const fs = require('fs');
const d = JSON.parse(fs.readFileSync(process.argv[2]));
const h = JSON.parse(fs.readFileSync(process.argv[3]));

const fmt  = v => v != null ? v.toFixed(2) : 'N/A';
const pct  = (a, b) => b && a ? `${((a / b - 1) * 100).toFixed(1)}%` : 'N/A';

const COL1 = 24, COL2 = 24, COL3 = 24;
const row = (label, dv, hv, unit='ms') =>
  `  | ${label.padEnd(COL1)} | ${(fmt(dv)+' '+unit).padStart(COL2)} | ${(fmt(hv)+' '+unit).padStart(COL3)} |`;

console.log(`  ${'Metric'.padEnd(COL1)}   ${'Demo App (Partitioned)'.padStart(COL2)}   ${'Hellō Playground'.padStart(COL3)}`);
console.log('  ' + '─'.repeat(COL1 + COL2 + COL3 + 12));
console.log(row('n (successful runs)',   d.n,      h.n,      ''));
console.log(row('Mean',                 d.mean,   h.mean));
console.log(row('Median',               d.median, h.median));
console.log(row('P95',                  d.p95,    h.p95));
console.log(row('StdDev',               d.stddev, h.stddev));
console.log(row('Min',                  d.min,    h.min));
console.log(row('Max',                  d.max,    h.max));
console.log(row('Failures',             d.failures, h.failures, ''));
console.log('');

if (d.mean && h.mean) {
  const ratio = h.mean / d.mean;
  const faster = ratio > 1 ? 'Demo App is faster' : 'Hellō is faster';
  console.log(`  Mean ratio (Hellō / Demo): ${ratio.toFixed(2)}x  →  ${faster}`);
  console.log(`  Hellō overhead vs Demo:    ${pct(h.mean, d.mean)}`);
}
JSEOF
  echo ""
  echo "  CSVs:"
  [[ -f "$OUT_DIR/rq1_demo_runs.csv"  ]] && echo "    Demo App: $OUT_DIR/rq1_demo_runs.csv"
  [[ -f "$OUT_DIR/rq1_hello_runs.csv" ]] && echo "    Hellō:    $OUT_DIR/rq1_hello_runs.csv"
  echo ""
  } | tee "$COMPARISON_OUT"

  echo -e "${GREEN}[rq1] Comparison saved: $COMPARISON_OUT${NC}"

  # Generate plot if matplotlib is available
  if command -v python3 >/dev/null 2>&1 && python3 -c "import matplotlib" 2>/dev/null; then
    python3 "$RQ_DIR/plot_comparison.py" \
      --demo  "$DEMO_SUMMARY" \
      --hello "$HELLO_SUMMARY" \
      --output "$ROOT/plots/rq1_e2e_comparison.png" 2>/dev/null && \
      echo -e "${GREEN}[rq1] Plot saved: plots/rq1_e2e_comparison.png${NC}" || true
  fi

elif [[ -f "$DEMO_SUMMARY" ]]; then
  echo -e "${YELLOW}[rq1] Only demo_app results available (hello not measured yet).${NC}"
elif [[ -f "$HELLO_SUMMARY" ]]; then
  echo -e "${YELLOW}[rq1] Only hello_playground results available (demo not measured yet).${NC}"
fi
