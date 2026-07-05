#!/usr/bin/env bash
#
# load_vegeta_client.sh: Rate-based saturation sweep using vegeta (keep-alive).
#
# Unlike the curl+xargs generator (capped at ~25 real concurrency by per-request
# process spawning), vegeta holds persistent keep-alive connections and paces
# requests at a fixed RATE (req/s). When the offered rate exceeds the server's
# capacity, achieved throughput peels away from the offered rate, latency climbs,
# and errors appear: the real saturation signal.
#
# Sweeps offered rate from --start to --end in steps. At each level it attacks
# for --duration seconds and records latency percentiles, achieved throughput,
# success rate, and client CPU.
#
# Run simultaneously on two clients pointing at the same server.
#
# Usage:
#   bash /tmp/load_vegeta_client.sh \
#     --server SERVER_IP --id client1 --out /tmp/vegeta_client1.csv \
#     --start 500 --end 8000 --step 500 --duration 10
#
set -euo pipefail

SERVER=""
CLIENT_ID="client"
OUT="/tmp/vegeta_${CLIENT_ID}.csv"
R_START=500
R_END=8000
R_STEP=500
DURATION=10
PORT=4001
APP_ID="app_example_456"
VEGETA="${VEGETA_BIN:-/tmp/vegeta}"
MAX_WORKERS=200000   # cap on concurrent connections vegeta may open
RECOVERY=false       # if true: ramp UP to the peak then back DOWN (recovery test)
COOLDOWN=5           # idle seconds between levels so the server drains the
                     # previous level's backlog before the next measurement
                     # (otherwise a saturated level bleeds into the next, giving
                     # spurious alternating good/bad levels in the curve)

while [[ $# -gt 0 ]]; do
  case "$1" in
    --server)   SERVER="$2";    shift 2 ;;
    --id)       CLIENT_ID="$2"; shift 2 ;;
    --out)      OUT="$2";       shift 2 ;;
    --start)    R_START="$2";   shift 2 ;;
    --end)      R_END="$2";     shift 2 ;;
    --step)     R_STEP="$2";    shift 2 ;;
    --duration) DURATION="$2";  shift 2 ;;
    --port)     PORT="$2";      shift 2 ;;
    --recovery) RECOVERY=true;  shift ;;
    --cooldown) COOLDOWN="$2";  shift 2 ;;
    *) echo "Unknown: $1" >&2; exit 1 ;;
  esac
done

[[ -z "$SERVER" ]] && { echo "Usage: $0 --server IP [opts]" >&2; exit 1; }
[[ -x "$VEGETA" ]] || { echo "vegeta not found at $VEGETA" >&2; exit 1; }

URL="https://${SERVER}:${PORT}/login"
BODY_FILE="/tmp/vegeta_body_${CLIENT_ID}.json"
TARGETS="/tmp/vegeta_targets_${CLIENT_ID}.txt"
printf '{"app_id":"%s"}' "$APP_ID" > "$BODY_FILE"
{
  echo "POST ${URL}"
  echo "Content-Type: application/json"
  echo "@${BODY_FILE}"
} > "$TARGETS"

NCPU=$(nproc 2>/dev/null || echo 1)
CLIENT_SAT_PCT=85

read_cpu() {
  awk '/^cpu /{idle=$5+$6; total=0; for(i=2;i<=NF;i++) total+=$i; print idle, total; exit}' /proc/stat
}

echo "step,phase,offered_rate,requests,ok,failed,achieved_rps,success_pct,mean_ms,median_ms,p85_ms,p95_ms,p99_ms,max_ms,client_cpu_pct,load1,ncpu,client_saturated,t_epoch" > "$OUT"

# Build the rate schedule. Normal: ascending only. Recovery: ascending then
# descending back to the start, so we can watch latency/success come back down.
RATES=$(seq "$R_START" "$R_STEP" "$R_END")
if [[ "$RECOVERY" == true ]]; then
  DOWN=$(seq "$(( R_END - R_STEP ))" "-${R_STEP}" "$R_START")
  RATES="$RATES $DOWN"
  echo "[${CLIENT_ID}] RECOVERY mode: ramp ${R_START}→${R_END}→${R_START} req/s, ${DURATION}s/level"
else
  echo "[${CLIENT_ID}] Vegeta rate sweep ${R_START}→${R_END} step ${R_STEP} req/s, ${DURATION}s/level"
fi
echo "[${CLIENT_ID}] Target: ${URL}   (client cores: ${NCPU})"
echo ""
printf "  %5s %5s %8s %10s %9s %9s %8s\n" "step" "phase" "rate" "achieved" "p85" "succ%" "cli_cpu"
printf "  %s\n" "$(printf '%0.s─' {1..60})"

# Warm-up: drive the real /login path at a representative (mid-sweep) rate so the
# keep-alive connection pool, TLS session cache, allocator and the server's
# steady state are all hot before the FIRST measured level: otherwise that level
# pays a cold-start spike. Drain afterwards so the warm-up backlog does not bleed
# into level 1.
WARMUP_RATE=$(( (R_START + R_END) / 2 )); [[ "$WARMUP_RATE" -lt 1 ]] && WARMUP_RATE=1
echo "[${CLIENT_ID}] Warm-up: ${WARMUP_RATE}/s for 5s, then ${COOLDOWN}s drain"
"$VEGETA" attack -targets="$TARGETS" -rate="${WARMUP_RATE}/1s" -duration=5s -keepalive -insecure \
  -max-workers="$MAX_WORKERS" -timeout=30s 2>/dev/null | "$VEGETA" report -type=text >/dev/null 2>&1 || true
sleep "$COOLDOWN"

STEP=0
PEAK_SEEN=false
for rate in $RATES; do
  STEP=$(( STEP + 1 ))
  # Drain: idle so the server clears any backlog from the previous (possibly
  # saturated) level, so this measurement starts from a clean baseline. Without
  # it, a saturated level's queue bleeds into the next and produces spurious
  # alternating good/bad levels (the "two collapses" artifact).
  [[ "$STEP" -gt 1 ]] && sleep "$COOLDOWN"
  # Phase = up until we hit the peak, down afterwards.
  [[ "$rate" -ge "$R_END" ]] && PEAK_SEEN=true
  if [[ "$RECOVERY" == true && "$PEAK_SEEN" == true && "$rate" -lt "$R_END" ]]; then
    PHASE="down"
  else
    PHASE="up"
  fi
  BIN="/tmp/vegeta_res_${CLIENT_ID}_${STEP}.bin"
  T_EPOCH=$(date +%s)   # level start: used to align the server CPU samples

  read CPU_IDLE0 CPU_TOTAL0 < <(read_cpu)
  "$VEGETA" attack -targets="$TARGETS" -rate="${rate}/1s" -duration="${DURATION}s" \
    -keepalive -insecure -max-workers="$MAX_WORKERS" -timeout=30s > "$BIN" 2>/dev/null || true
  read CPU_IDLE1 CPU_TOTAL1 < <(read_cpu)

  CLIENT_CPU=$(awk -v i0="$CPU_IDLE0" -v t0="$CPU_TOTAL0" -v i1="$CPU_IDLE1" -v t1="$CPU_TOTAL1" '
    BEGIN { dt=t1-t0; di=i1-i0; if(dt<=0){print "0.0";exit}
            b=100.0*(1.0-di/dt); if(b<0)b=0; printf "%.1f", b }')
  LOAD1=$(awk '{print $1}' /proc/loadavg 2>/dev/null || echo 0)

  # Per-request latencies (ns) + status from the encoded results.
  "$VEGETA" encode -to=csv "$BIN" 2>/dev/null | \
  awk -F',' -v rate="$rate" -v dur="$DURATION" -v cpu="$CLIENT_CPU" \
            -v load1="$LOAD1" -v ncpu="$NCPU" -v sat_pct="$CLIENT_SAT_PCT" \
            -v step="$STEP" -v phase="$PHASE" -v tepoch="$T_EPOCH" '
  {
    total++
    code=$2; lat=$3/1e6      # ns → ms
    if (code==200 && $6=="") { ok++; a[ok]=lat; sum+=lat }
    else { fail++ }
  }
  END {
    total=total+0; ok=ok+0; fail=fail+0
    saturated = (cpu+0 >= sat_pct) ? 1 : 0
    succ = (total>0)? 100.0*ok/total : 0
    ach  = ok/dur
    if (ok==0) {
      printf "%d,%s,%d,%d,0,%d,%.1f,%.1f,0,0,0,0,0,0,%s,%s,%d,%d,%d\n", step, phase, rate, total, fail, ach, succ, cpu, load1, ncpu, saturated, tepoch
      printf "  %5d %5s %8d %10.1f %9s %9.1f %7s%%%s\n", step, phase, rate, ach, "N/A", succ, cpu, (saturated?" !":"") > "/dev/stderr"
      exit
    }
    asort(a)
    mean=sum/ok
    p50=a[int(ok*0.50)+1]; if(p50=="")p50=a[ok]
    p85=a[int(ok*0.85)];   if(p85<1)p85=a[1]
    p95=a[int(ok*0.95)];   if(p95<1)p95=a[1]
    p99=a[int(ok*0.99)];   if(p99<1)p99=a[1]
    printf "%d,%s,%d,%d,%d,%d,%.1f,%.1f,%.2f,%.2f,%.2f,%.2f,%.2f,%.2f,%s,%s,%d,%d,%d\n",
      step, phase, rate, total, ok, fail, ach, succ, mean, p50, p85, p95, p99, a[ok], cpu, load1, ncpu, saturated, tepoch
    printf "  %5d %5s %8d %10.1f %9.1f %9.1f %7s%%%s\n", step, phase, rate, ach, p85, succ, cpu, (saturated?" !":"") > "/dev/stderr"
  }' >> "$OUT"

  rm -f "$BIN"
done

echo ""
echo "[${CLIENT_ID}] Done. CSV: ${OUT}"
