#!/usr/bin/env bash
#
# run.sh: Launches all microservices and runs the E2E flow
#
# Usage:
#   ./run.sh              # build, launch servers, run mock_app, stop all
#   ./run.sh --no-build   # skip compilation (use existing binaries)
#   ./run.sh --netem wifi # emulate latency on inter-service links
#   ./run.sh --netem custom 120 25 0.5
#
# For the thesis experiments (RQ1-RQ3) see the scripts under rq/ and the
# top-level README.
#
set -euo pipefail

usage() {
    cat <<'EOF'
Usage:
  ./run.sh [--no-build] [--netem <profile>]

Options:
  --no-build                     Skip compilation
  --netem lan|wifi|mobile|bad   Apply predefined network emulation profile
  --netem custom <lat> <jit> [loss]
                                 Apply custom profile in ms and %
  --netem-backend tc|toxiproxy   Force backend (default: auto)
  -h, --help                     Show this help
EOF
}

NO_BUILD=false
NETEM_ENABLED=false
NETEM_ARGS=()
NETEM_BACKEND=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-build)
            NO_BUILD=true
            shift
            ;;
        --netem)
            shift
            if [[ $# -lt 1 ]]; then
                echo "[run.sh] ERROR: --netem requires a profile" >&2
                usage
                exit 1
            fi

            NETEM_ENABLED=true
            NETEM_ARGS+=("$1")
            shift

            if [[ "${NETEM_ARGS[0]}" == "custom" ]]; then
                if [[ $# -lt 2 ]]; then
                    echo "[run.sh] ERROR: --netem custom requires <latency_ms> <jitter_ms> [loss_pct]" >&2
                    usage
                    exit 1
                fi
                NETEM_ARGS+=("$1" "$2")
                shift 2

                if [[ $# -gt 0 && "$1" != --* ]]; then
                    NETEM_ARGS+=("$1")
                    shift
                fi
            fi
            ;;
        --netem-backend)
            shift
            if [[ $# -lt 1 ]]; then
                echo "[run.sh] ERROR: --netem-backend requires tc|toxiproxy" >&2
                usage
                exit 1
            fi
            NETEM_BACKEND="$1"
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "[run.sh] ERROR: unknown argument '$1'" >&2
            usage
            exit 1
            ;;
    esac
done

# ── Colors ────────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m' # No Color

# ── Kill stale processes from previous runs ───────────────────────────────
echo -e "${YELLOW}[run.sh] Killing old processes on prototype ports...${NC}"
for port in 3000 3001 4001 4002 4010 4020 5001 8080; do
    lsof -ti:"$port" 2>/dev/null | xargs -r kill 2>/dev/null || true
done
# Also kill any leftover cargo run / binary processes by name
pkill -f "target/debug/(tts|mock_idp|ab|ib|web_ui|demo_app|mock_app)" 2>/dev/null || true
sleep 1
echo -e "${GREEN}[run.sh] Old processes cleaned up.${NC}"
echo ""

PIDS=()
LOG_DIR="logs"

mkdir -p "$LOG_DIR"

cleanup() {
    echo ""
    echo -e "${YELLOW}[run.sh] Stopping servers...${NC}"
    for pid in "${PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
    done
    wait 2>/dev/null || true

    if [[ "$NETEM_ENABLED" == true ]]; then
        echo -e "${YELLOW}[run.sh] Clearing network emulation...${NC}"
        ./scripts/netem.sh clear >/dev/null 2>&1 || true
    fi

    echo -e "${GREEN}[run.sh] All servers stopped.${NC}"
}

trap cleanup EXIT

# ── Build ─────────────────────────────────────────────────────────────────
if [[ "$NO_BUILD" != true ]]; then
    echo -e "${CYAN}[run.sh] Building workspace...${NC}"
    cargo build 2>&1
    echo -e "${GREEN}[run.sh] Build OK${NC}"
    echo ""
fi

# ── Optional network emulation ─────────────────────────────────────────────
if [[ "$NETEM_ENABLED" == true ]]; then
    if [[ -n "$NETEM_BACKEND" ]]; then
        export NETEM_BACKEND
    fi
    NETEM_CURRENT_BACKEND="$(./scripts/netem.sh backend)"
    echo -e "${CYAN}[run.sh] Applying network emulation (${NETEM_CURRENT_BACKEND}): ${NETEM_ARGS[*]}${NC}"
    ./scripts/netem.sh apply "${NETEM_ARGS[@]}"
    ./scripts/netem.sh status || true
    if [[ "$NETEM_CURRENT_BACKEND" == "toxiproxy" ]]; then
        export AB_IB_HTTPS_BASE="https://localhost:4402"
        export IB_TTS_ISSUE_URL="https://localhost:4501/issue"
        export IB_MOCK_IDP_AUTHORIZE_URL="https://localhost:4301/authorize"
        echo -e "${YELLOW}[run.sh] Using toxiproxy upstreams: AB->4402, IB->4501/4301${NC}"
    fi
    echo ""
fi

# ── Launch servers ────────────────────────────────────────────────────────
echo -e "${BOLD}════════════════════════════════════════════════════${NC}"
echo -e "${BOLD}  MOSHI: E2E Flow${NC}"
echo -e "${BOLD}════════════════════════════════════════════════════${NC}"
echo ""

echo -e "${CYAN}[1/6] Launching tTS          (https://localhost:5001) ...${NC}"
NO_COLOR=1 RUST_LOG=info cargo run --bin tts > "$LOG_DIR/tts.log" 2>&1 &
PIDS+=($!)
sleep 2

echo -e "${CYAN}[2/6] Launching Mock IdP     (https://localhost:3001) ...${NC}"
NO_COLOR=1 RUST_LOG=info cargo run --bin mock_idp > "$LOG_DIR/mock_idp.log" 2>&1 &
PIDS+=($!)
sleep 1

echo -e "${CYAN}[3/6] Launching IB           (https://localhost:4002 + http://localhost:4020) ...${NC}"
NO_COLOR=1 RUST_LOG=info \
MOCK_IDP_AUTHORIZE_URL="${IB_MOCK_IDP_AUTHORIZE_URL:-https://localhost:3001/authorize}" \
TTS_ISSUE_URL="${IB_TTS_ISSUE_URL:-https://localhost:5001/issue}" \
cargo run --bin ib > "$LOG_DIR/ib.log" 2>&1 &
PIDS+=($!)
sleep 1

echo -e "${CYAN}[4/6] Launching AB           (https://localhost:4001 + http://localhost:4010) ...${NC}"
NO_COLOR=1 RUST_LOG=info \
IB_HTTPS_BASE="${AB_IB_HTTPS_BASE:-https://localhost:4002}" \
cargo run --bin ab > "$LOG_DIR/ab.log" 2>&1 &
PIDS+=($!)
sleep 2

echo -e "${CYAN}[5/6] Launching Web UI       (http://localhost:8080)  ...${NC}"
NO_COLOR=1 RUST_LOG=info cargo run --bin web_ui > "$LOG_DIR/web_ui.log" 2>&1 &
PIDS+=($!)
sleep 1

echo -e "${CYAN}[6/6] Launching Demo App     (http://localhost:3000)  ...${NC}"
NO_COLOR=1 RUST_LOG=info cargo run --bin demo_app > "$LOG_DIR/demo_app.log" 2>&1 &
PIDS+=($!)
sleep 1

# Check that all are listening
ALL_UP=true
for port in 3000 3001 4001 4002 4010 4020 5001 8080; do
    if ! lsof -i:"$port" -sTCP:LISTEN >/dev/null 2>&1; then
        echo -e "${RED}[run.sh] ERROR: port $port is not listening${NC}"
        ALL_UP=false
    fi
done

if [ "$ALL_UP" = false ]; then
    echo -e "${RED}[run.sh] Not all servers started. Check logs in $LOG_DIR/*.log${NC}"
    exit 1
fi

echo -e "${GREEN}[run.sh] All servers ready.${NC}"
echo ""

# ── Run mock_app ──────────────────────────────────────────────────────────
echo -e "${BOLD}────────────────────────────────────────────────────${NC}"
echo -e "${BOLD}  Running Mock App (E2E client: direct flow)${NC}"
echo -e "${BOLD}────────────────────────────────────────────────────${NC}"
echo ""

RUST_LOG=info cargo run --bin mock_app 2>&1
APP_EXIT=$?

echo ""

if [ $APP_EXIT -eq 0 ]; then
    echo -e "${GREEN}${BOLD}════════════════════════════════════════════════════${NC}"
    echo -e "${GREEN}${BOLD}  DIRECT E2E FLOW COMPLETED SUCCESSFULLY${NC}"
    echo -e "${GREEN}${BOLD}════════════════════════════════════════════════════${NC}"
else
    echo -e "${RED}${BOLD}  E2E FLOW FAILED (exit code: $APP_EXIT)${NC}"
    echo -e "${RED}  Check logs: $LOG_DIR/tts.log $LOG_DIR/mock_idp.log $LOG_DIR/ib.log $LOG_DIR/ab.log${NC}"
fi

echo ""
echo -e "${YELLOW}[run.sh] Server logs:${NC}"
echo "  tTS:      $LOG_DIR/tts.log"
echo "  Mock IdP: $LOG_DIR/mock_idp.log"
echo "  IB:       $LOG_DIR/ib.log"
echo "  AB:       $LOG_DIR/ab.log"
echo "  Web UI:   $LOG_DIR/web_ui.log"
echo "  Demo App: $LOG_DIR/demo_app.log"
echo ""

# ── Keep servers alive for browser interaction ────────────────────────
echo -e "${CYAN}${BOLD}  Available services:${NC}"
echo -e "${CYAN}    tTS:          https://localhost:5001 (FROST signing)${NC}"
echo -e "${CYAN}    Mock IdP:     https://localhost:3001 (mock identity provider)${NC}"
echo -e "${CYAN}    AB HTTPS:     https://localhost:4001 (inter-service + OIDC discovery)${NC}"
echo -e "${CYAN}    AB HTTP:      http://localhost:4010  (browser-facing redirects)${NC}"
echo -e "${CYAN}    IB HTTPS:     https://localhost:4002 (inter-service + relay)${NC}"
echo -e "${CYAN}    IB HTTP:      http://localhost:4020  (browser-facing Real IdP auth)${NC}"
echo -e "${CYAN}    Web UI:       http://localhost:8080  (3-tab SPA)${NC}"
echo -e "${CYAN}    Demo App:     http://localhost:3000  (relying party + Real IdP sign-in)${NC}"
echo ""
echo -e "${GREEN}${BOLD}  To test Real IdP sign-in:${NC}"
echo -e "${GREEN}    1. Open http://localhost:3000 in your browser${NC}"
echo -e "${GREEN}    2. Click 'Sign in with Google', 'Sign in with GitHub', or 'Sign in with Discord'${NC}"
echo -e "${GREEN}    3. Authenticate with your chosen IdP account${NC}"
echo -e "${GREEN}    4. The JWT appears on the result page${NC}"
echo -e "${GREEN}    Note: GitHub/Discord require valid OAuth credentials in .env${NC}"
echo ""
echo -e "${CYAN}  Press Ctrl+C to stop all servers.${NC}"
echo ""

# Wait forever (until Ctrl+C triggers the cleanup trap)
wait
