#!/usr/bin/env bash
#
# run_rq2.sh: RQ2: FROST vs Ed25519 cryptographic overhead on cosmos
#
# Builds bench_frost (musl static), deploys to cosmos, runs there,
# fetches JSON results, and generates the plot.
#
# Usage:
#   ./rq/rq2/run_rq2.sh                    # build + run (defaults below)
#   ./rq/rq2/run_rq2.sh --no-build         # skip cargo build
#   ./rq/rq2/run_rq2.sh --server angainor  # different node
#   ./rq/rq2/run_rq2.sh --iterations 2000  # iterations per execution
#   ./rq/rq2/run_rq2.sh --warmup 500       # discarded warm-up iterations
#   ./rq/rq2/run_rq2.sh --repeats 20       # independent executions (run-to-run range)
#
# Defaults: 20 repeats x 2000 iterations, 500 warm-up: the numbers reported in
# the thesis. The repeats are what make the [min,max] range in rq2_table.tex real.
#
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
RQ2_DIR="$ROOT/rq/rq2"
OUT_DIR="$RQ2_DIR/out"
PLOTS_DIR="$ROOT/plots/rq2"

SERVER="cosmos"
REMOTE_DIR="~/bench_frost_rq2"
# Defaults bumped for stability: more iterations tightens the mean, and a long
# warm-up lets the pinned core settle to a steady frequency before measuring.
ITERATIONS=2000
WARMUP=500
# REPEATS independent executions (separate processes) so we can report the
# run-to-run RANGE, not a single fragile execution. Bar = median across runs,
# whiskers = [min, max] across runs (see plot_rq2.py).
REPEATS=20
NO_BUILD=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --server)     SERVER="$2";     shift 2 ;;
    --iterations) ITERATIONS="$2"; shift 2 ;;
    --warmup)     WARMUP="$2";     shift 2 ;;
    --repeats)    REPEATS="$2";    shift 2 ;;
    --no-build)   NO_BUILD=true;   shift ;;
    -h|--help) sed -n '3,17p' "$0"; exit 0 ;;
    *) echo "Unknown: $1" >&2; exit 1 ;;
  esac
done

mkdir -p "$OUT_DIR" "$PLOTS_DIR"

MUSL="x86_64-unknown-linux-musl"
BIN="$ROOT/target/$MUSL/release/bench_frost"

RED='\033[0;31m'; GREEN='\033[0;32m'; CYAN='\033[0;36m'; BOLD='\033[1m'; NC='\033[0m'

# ── Build ─────────────────────────────────────────────────────────────────────
if [[ "$NO_BUILD" != true ]]; then
  echo -e "${CYAN}[rq2] Building bench_frost (musl static)...${NC}"
  cd "$ROOT"
  rustup target add "$MUSL" 2>/dev/null || true
  cargo build --release --target "$MUSL" --bin bench_frost 2>&1
  echo -e "${GREEN}[rq2] Build OK${NC}"
fi

if [[ ! -f "$BIN" ]]; then
  echo -e "${RED}[rq2] Binary not found: $BIN${NC}" >&2; exit 1
fi

# ── Deploy ────────────────────────────────────────────────────────────────────
echo -e "${CYAN}[rq2] Deploying to ${SERVER}...${NC}"
ssh "$SERVER" "mkdir -p $REMOTE_DIR"
scp -q "$BIN" "${SERVER}:${REMOTE_DIR}/bench_frost"
ssh "$SERVER" "chmod +x $REMOTE_DIR/bench_frost"

# ── Run on cosmos ─────────────────────────────────────────────────────────────
echo -e "${BOLD}════════════════════════════════════════════════════${NC}"
echo -e "${BOLD}  RQ2: Cryptographic overhead: running on ${SERVER}${NC}"
echo -e "${BOLD}  iterations=${ITERATIONS}  warmup=${WARMUP}  repeats=${REPEATS}${NC}"
echo -e "${BOLD}════════════════════════════════════════════════════${NC}"
echo ""

# Stabilise the measurement: pin to a single core and (best-effort) lock the CPU
# to a fixed frequency, then run REPEATS independent executions (separate
# processes) so the run-to-run RANGE can be reported. The sub-40µs Ed25519
# baseline sits at the timing-noise floor, so a single execution's ratio swings
# (e.g. 19× vs 38×); the median-across-runs + [min,max] range is robust.
# The sudo steps are non-interactive (sudo -n); if not permitted, the run
# continues with a warning and taskset-only pinning.
CPU_CORE="${CPU_CORE:-2}"
ssh "$SERVER" "
  sudo -n cpupower frequency-set -g performance >/dev/null 2>&1 \
    && echo '[rq2] CPU governor -> performance' \
    || echo '[rq2] WARN: could not set performance governor (no sudo?): frequency may vary';
  sudo -n sh -c 'echo 1 > /sys/devices/system/cpu/intel_pstate/no_turbo' >/dev/null 2>&1 \
    && echo '[rq2] turbo -> disabled' \
    || echo '[rq2] WARN: could not disable turbo: ratio may swing';
  rm -f $REMOTE_DIR/results_*.json;
  for i in \$(seq 1 ${REPEATS}); do
    echo \"[rq2]   execution \$i/${REPEATS}\";
    taskset -c ${CPU_CORE} $REMOTE_DIR/bench_frost --iterations $ITERATIONS --warmup $WARMUP --out $REMOTE_DIR/results_\$i.json >/dev/null;
  done;
  sudo -n sh -c 'echo 0 > /sys/devices/system/cpu/intel_pstate/no_turbo' >/dev/null 2>&1 || true
"

# ── Fetch results ─────────────────────────────────────────────────────────────
echo ""
echo -e "${CYAN}[rq2] Fetching ${REPEATS} run files...${NC}"
RUNS_DIR="$OUT_DIR/runs"
rm -rf "$RUNS_DIR"; mkdir -p "$RUNS_DIR"
scp -q "${SERVER}:${REMOTE_DIR}/results_*.json" "$RUNS_DIR/"
echo -e "${GREEN}[rq2] ${REPEATS} runs saved: $RUNS_DIR/${NC}"

# ── Generate plot (aggregates the runs into bench_frost_results.json) ──────────
PYTHON_BIN="python3"
[[ -x "$ROOT/venv/bin/python" ]] && PYTHON_BIN="$ROOT/venv/bin/python"

echo -e "${CYAN}[rq2] Aggregating runs + generating plots...${NC}"
# plot_rq2.py aggregates the runs and (re)writes $OUT_DIR/bench_frost_results.json.
"$PYTHON_BIN" "$ROOT/rq/rq2/plot_rq2.py" \
  --runs-dir "$RUNS_DIR" \
  --output   "$PLOTS_DIR/rq2_crypto_overhead"
# Quorum-scalability figure reads the aggregated results file written just above.
"$PYTHON_BIN" "$ROOT/rq/rq2/plot_quorum.py" \
  --input  "$OUT_DIR/bench_frost_results.json" \
  --output "$PLOTS_DIR/rq2_quorum_scalability"

echo ""
echo -e "${GREEN}${BOLD}[rq2] Done.${NC}"
echo "  Results: $OUT_DIR/bench_frost_results.json"
echo "  Plots:   $PLOTS_DIR/rq2_crypto_overhead.{png,pdf}"
echo "           $PLOTS_DIR/rq2_quorum_scalability.{png,pdf}"
