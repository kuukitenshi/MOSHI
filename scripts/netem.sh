#!/usr/bin/env bash
#
# scripts/netem.sh: Network emulation for local microservices
#
# Backends:
#   - tc/netem (requires sudo)
#   - toxiproxy (no sudo; default fallback)

set -euo pipefail

DEV="lo"
PORTS=(4002 5001 3001)

TOXI_CONTROL_PORT="8474"
TOXI_BIN_DIR="scripts/.bin"
TOXI_BIN="$TOXI_BIN_DIR/toxiproxy-server"
TOXI_PID_FILE="scripts/.toxiproxy.pid"
TOXI_LOG_FILE="logs/toxiproxy.log"
TOXI_URL="http://127.0.0.1:${TOXI_CONTROL_PORT}"

toxi_proxy_names=(ab_ib ib_tts ib_mock)

require_tools() {
    if ! command -v curl >/dev/null 2>&1; then
        echo "[netem] ERROR: 'curl' not found." >&2
        exit 1
    fi
}

backend() {
    if [[ -n "${NETEM_BACKEND:-}" ]]; then
        case "$NETEM_BACKEND" in
            tc|toxiproxy)
                echo "$NETEM_BACKEND"
                return
                ;;
            *)
                echo "[netem] ERROR: NETEM_BACKEND must be 'tc' or 'toxiproxy'" >&2
                exit 1
                ;;
        esac
    fi

    if command -v tc >/dev/null 2>&1 && command -v sudo >/dev/null 2>&1 && sudo -n true 2>/dev/null; then
        echo "tc"
    else
        echo "toxiproxy"
    fi
}

resolve_profile() {
    local profile="${1:-wifi}"

    case "$profile" in
        lan)
            LATENCY_MS=8
            JITTER_MS=2
            LOSS_PCT=0
            ;;
        wifi)
            LATENCY_MS=35
            JITTER_MS=10
            LOSS_PCT=0.2
            ;;
        mobile)
            LATENCY_MS=90
            JITTER_MS=30
            LOSS_PCT=1.0
            ;;
        bad)
            LATENCY_MS=180
            JITTER_MS=60
            LOSS_PCT=2.5
            ;;
        custom)
            LATENCY_MS="${2:-}"
            JITTER_MS="${3:-}"
            LOSS_PCT="${4:-0}"
            if [[ -z "$LATENCY_MS" || -z "$JITTER_MS" ]]; then
                echo "[netem] ERROR: custom requires latency_ms and jitter_ms." >&2
                exit 1
            fi
            ;;
        *)
            echo "[netem] ERROR: unknown profile '$profile'." >&2
            echo "[netem] Valid: lan | wifi | mobile | bad | custom" >&2
            exit 1
            ;;
    esac
}

clear_netem() {
    local be
    be="$(backend)"

    if [[ "$be" == "tc" ]]; then
        sudo tc qdisc del dev "$DEV" root 2>/dev/null || true
        echo "[netem] Cleared qdisc on $DEV (tc backend)"
        return
    fi

    clear_toxiproxy
    echo "[netem] Cleared toxiproxy emulation"
}

download_toxiproxy_if_needed() {
    if command -v toxiproxy-server >/dev/null 2>&1; then
        TOXI_BIN="$(command -v toxiproxy-server)"
        return
    fi

    mkdir -p "$TOXI_BIN_DIR"
    if [[ ! -x "$TOXI_BIN" ]]; then
        echo "[netem] Downloading toxiproxy-server (no-sudo backend)..."
        curl -fsSL -o "$TOXI_BIN" \
            "https://github.com/Shopify/toxiproxy/releases/download/v2.12.0/toxiproxy-server-linux-amd64"
        chmod +x "$TOXI_BIN"
    fi
}

toxi_api_ok() {
    curl -fsS "$TOXI_URL/version" >/dev/null 2>&1
}

start_toxiproxy_server() {
    mkdir -p logs
    download_toxiproxy_if_needed

    if toxi_api_ok; then
        return
    fi

    nohup "$TOXI_BIN" > "$TOXI_LOG_FILE" 2>&1 &
    echo $! > "$TOXI_PID_FILE"

    local retries=40
    local i
    for i in $(seq 1 "$retries"); do
        if toxi_api_ok; then
            return
        fi
        sleep 0.1
    done

    echo "[netem] ERROR: toxiproxy-server failed to start" >&2
    exit 1
}

toxi_delete_proxy_if_exists() {
    local name="$1"
    curl -fsS -X DELETE "$TOXI_URL/proxies/$name" >/dev/null 2>&1 || true
}

toxi_create_proxy() {
    local name="$1"
    local listen="$2"
    local upstream="$3"

    toxi_delete_proxy_if_exists "$name"
    curl -fsS -X POST "$TOXI_URL/proxies" \
        -H 'Content-Type: application/json' \
        -d "{\"name\":\"$name\",\"listen\":\"$listen\",\"upstream\":\"$upstream\"}" >/dev/null
}

toxi_add_latency() {
    local name="$1"
    local latency="$2"
    local jitter="$3"

    curl -fsS -X POST "$TOXI_URL/proxies/$name/toxics" \
        -H 'Content-Type: application/json' \
        -d "{\"name\":\"lat_up\",\"type\":\"latency\",\"stream\":\"upstream\",\"attributes\":{\"latency\":$latency,\"jitter\":$jitter}}" >/dev/null

    curl -fsS -X POST "$TOXI_URL/proxies/$name/toxics" \
        -H 'Content-Type: application/json' \
        -d "{\"name\":\"lat_down\",\"type\":\"latency\",\"stream\":\"downstream\",\"attributes\":{\"latency\":$latency,\"jitter\":$jitter}}" >/dev/null
}

apply_toxiproxy() {
    local profile="${1:-wifi}"
    resolve_profile "$profile" "${2:-}" "${3:-}" "${4:-}"

    start_toxiproxy_server

    toxi_create_proxy "ab_ib"  "0.0.0.0:4402" "127.0.0.1:4002"
    toxi_create_proxy "ib_tts" "0.0.0.0:4501" "127.0.0.1:5001"
    toxi_create_proxy "ib_mock" "0.0.0.0:4301" "127.0.0.1:3001"

    local name
    for name in "${toxi_proxy_names[@]}"; do
        toxi_add_latency "$name" "$LATENCY_MS" "$JITTER_MS"
    done

    echo "[netem] Applied profile '$profile' via toxiproxy"
    echo "[netem] Params: delay=${LATENCY_MS}ms jitter=${JITTER_MS}ms"
    if [[ "$LOSS_PCT" != "0" && "$LOSS_PCT" != "0.0" ]]; then
        echo "[netem] Note: loss=${LOSS_PCT}% requested, ignored by toxiproxy backend"
    fi
    echo "[netem] Proxy ports: 4402->4002, 4501->5001, 4301->3001"
}

clear_toxiproxy() {
    local name
    for name in "${toxi_proxy_names[@]}"; do
        toxi_delete_proxy_if_exists "$name"
    done

    if [[ -f "$TOXI_PID_FILE" ]]; then
        kill "$(cat "$TOXI_PID_FILE")" 2>/dev/null || true
        rm -f "$TOXI_PID_FILE"
    fi
}

apply_netem() {
    local profile="${1:-wifi}"
    local be
    be="$(backend)"

    if [[ "$be" == "tc" ]]; then
        resolve_profile "$profile" "${2:-}" "${3:-}" "${4:-}"

        sudo tc qdisc del dev "$DEV" root 2>/dev/null || true
        sudo tc qdisc add dev "$DEV" root handle 1: prio bands 4
        sudo tc qdisc add dev "$DEV" parent 1:2 handle 20: netem \
            delay "${LATENCY_MS}ms" "${JITTER_MS}ms" \
            loss "${LOSS_PCT}%"

        local prio=10
        local port
        for port in "${PORTS[@]}"; do
            sudo tc filter add dev "$DEV" protocol ip parent 1:0 prio "$prio" u32 \
                match ip dport "$port" 0xffff flowid 1:2
            prio=$((prio + 1))

            sudo tc filter add dev "$DEV" protocol ip parent 1:0 prio "$prio" u32 \
                match ip sport "$port" 0xffff flowid 1:2
            prio=$((prio + 1))
        done

        echo "[netem] Applied profile '$profile' on $DEV (tc backend)"
        echo "[netem] Params: delay=${LATENCY_MS}ms jitter=${JITTER_MS}ms loss=${LOSS_PCT}%"
        echo "[netem] Ports: ${PORTS[*]}"
        return
    fi

    apply_toxiproxy "$@"
}

status_netem() {
    local be
    be="$(backend)"

    if [[ "$be" == "tc" ]]; then
        echo "[netem] backend: tc"
        echo "[netem] qdisc status ($DEV):"
        tc qdisc show dev "$DEV"
        echo ""
        echo "[netem] filter status ($DEV):"
        tc filter show dev "$DEV" parent 1:0 || true
        return
    fi

    echo "[netem] backend: toxiproxy"
    if toxi_api_ok; then
        curl -fsS "$TOXI_URL/proxies" || true
        echo ""
    else
        echo "[netem] toxiproxy is not running"
    fi
}

usage() {
    cat <<'EOF'
Usage:
  scripts/netem.sh apply [lan|wifi|mobile|bad]
  scripts/netem.sh apply custom <latency_ms> <jitter_ms> [loss_pct]
  scripts/netem.sh clear
  scripts/netem.sh status
  scripts/netem.sh backend

Examples:
  scripts/netem.sh apply wifi
  NETEM_BACKEND=toxiproxy scripts/netem.sh apply wifi
  scripts/netem.sh apply custom 120 25 0.5
  scripts/netem.sh clear
EOF
}

main() {
    require_tools

    local cmd="${1:-}"
    case "$cmd" in
        apply)
            shift
            apply_netem "$@"
            ;;
        clear)
            clear_netem
            ;;
        status)
            status_netem
            ;;
        backend)
            backend
            ;;
        -h|--help|help|"")
            usage
            ;;
        *)
            echo "[netem] ERROR: unknown command '$cmd'" >&2
            usage
            exit 1
            ;;
    esac
}

main "$@"
