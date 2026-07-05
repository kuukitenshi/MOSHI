#!/usr/bin/env bash
#
# server_cpu_sampler.sh: sample whole-machine CPU% from /proc/stat every second.
#
# Appends "<epoch> <cpu_pct>" lines to $1. Runs on the SERVER during an RQ3 run
# so each sweep level can be aligned (by timestamp) to the server's CPU load,
# letting the plot show the server CPU alongside the client CPU.
#
# Usage: bash server_cpu_sampler.sh /tmp/server_cpu.log
#
OUT="${1:-/tmp/server_cpu.log}"
: > "$OUT"
prev_total=0; prev_idle=0
while true; do
  read -r _ user nice system idle iowait irq softirq steal _ < /proc/stat
  idle_all=$((idle + iowait))
  total=$((user + nice + system + idle + iowait + irq + softirq + steal))
  if [ "$prev_total" -ne 0 ]; then
    dt=$((total - prev_total)); di=$((idle_all - prev_idle))
    [ "$dt" -gt 0 ] && awk -v e="$(date +%s)" -v di="$di" -v dt="$dt" \
      'BEGIN { printf "%d %.1f\n", e, (1 - di/dt) * 100 }' >> "$OUT"
  fi
  prev_total=$total; prev_idle=$idle_all
  sleep 1
done
