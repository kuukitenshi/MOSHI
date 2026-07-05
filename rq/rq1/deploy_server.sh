#!/usr/bin/env bash
#
# deploy_server.sh: Build locally and deploy server stack to remote SSH host
#
# Copies release binaries + certs + scripts to the server and starts all services.
# Uses the direct flow (Mock IdP): no Google OAuth needed on the server side.
#
# Usage:
#   ./rq/deploy_server.sh --server USER@HOST [--remote-dir ~/opencode_rust] [--no-build]
#
# Requirements:
#   - SSH access to server (key-based auth recommended)
#   - Server: Linux x86_64, no other requirements
#   - Local: cargo installed (for building)
#
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SERVER=""
REMOTE_DIR="~/opencode_rust_server"
NO_BUILD=false
REMOTE_PORT_BASE=4001  # AB HTTPS

while [[ $# -gt 0 ]]; do
  case "$1" in
    --server)     SERVER="$2";     shift 2 ;;
    --remote-dir) REMOTE_DIR="$2"; shift 2 ;;
    --no-build)   NO_BUILD=true;   shift ;;
    *) echo "Unknown: $1" >&2; exit 1 ;;
  esac
done

if [[ -z "$SERVER" ]]; then
  echo "Usage: $0 --server USER@HOST [--remote-dir PATH] [--no-build]" >&2
  exit 1
fi

RED='\033[0;31m'; GREEN='\033[0;32m'; CYAN='\033[0;36m'; BOLD='\033[1m'; NC='\033[0m'

# ── Build ──────────────────────────────────────────────────────────────────
# Use musl target for fully static binaries: works on any Linux regardless
# of glibc version (avoids "GLIBC_X.XX not found" errors on older servers).
MUSL_TARGET="x86_64-unknown-linux-musl"
BIN_DIR="$ROOT/target/${MUSL_TARGET}/release"

if [[ "$NO_BUILD" != true ]]; then
  echo -e "${CYAN}[deploy] Building static binaries (musl)...${NC}"
  cd "$ROOT"

  # Check musl target is installed
  if ! rustup target list --installed 2>/dev/null | grep -q "$MUSL_TARGET"; then
    echo -e "${CYAN}[deploy] Installing musl target...${NC}"
    rustup target add "$MUSL_TARGET"
  fi

  cargo build --release --target "$MUSL_TARGET" \
    --bin tts --bin mock_idp --bin ib --bin ab --bin demo_app --bin mock_app 2>&1
  echo -e "${GREEN}[deploy] Build OK (static musl)${NC}"
fi

# ── Bundle ─────────────────────────────────────────────────────────────────
BUNDLE_DIR="$(mktemp -d)/server_bundle"
mkdir -p "$BUNDLE_DIR/bin" "$BUNDLE_DIR/scripts" "$BUNDLE_DIR/logs" "$BUNDLE_DIR/certs"

echo -e "${CYAN}[deploy] Bundling...${NC}"
for bin in tts mock_idp ib ab demo_app mock_app; do
  cp "${BIN_DIR}/$bin" "$BUNDLE_DIR/bin/"
done
# netem.sh is only needed for --netem runs; the inline remote start block below
# does not depend on it. Bundled only if present.
[[ -f "$ROOT/scripts/netem.sh" ]] && cp "$ROOT/scripts/netem.sh" "$BUNDLE_DIR/scripts/"

# Copy .env if it exists (Google OAuth creds: only needed for Google flow)
[[ -f "$ROOT/.env" ]] && cp "$ROOT/.env" "$BUNDLE_DIR/"

# Note: certs/ is empty: they are generated at first startup by the tTS service
touch "$BUNDLE_DIR/certs/.gitkeep"

echo -e "${CYAN}[deploy] Copying to ${SERVER}:${REMOTE_DIR}...${NC}"
ssh "$SERVER" "mkdir -p ${REMOTE_DIR}/bin ${REMOTE_DIR}/scripts ${REMOTE_DIR}/logs ${REMOTE_DIR}/certs"
if command -v rsync >/dev/null 2>&1; then
  rsync -az --progress "$BUNDLE_DIR/" "${SERVER}:${REMOTE_DIR}/"
else
  # rsync not installed locally: fall back to tar-over-ssh (tar+gzip are universal).
  echo -e "${CYAN}[deploy] rsync not found: using tar over ssh...${NC}"
  tar -C "$BUNDLE_DIR" -czf - . | ssh "$SERVER" "tar -C ${REMOTE_DIR} -xzf -"
fi

# ── Start services on server ───────────────────────────────────────────────
echo -e "${CYAN}[deploy] Starting services on server...${NC}"

# Get the server's IP as seen from itself (for BIND_HOST)
SERVER_IP=$(ssh "$SERVER" "hostname -I | awk '{print \$1}'")
echo -e "${CYAN}[deploy] Server IP: ${SERVER_IP}${NC}"

ssh "$SERVER" bash <<REMOTE
set -euo pipefail
cd ${REMOTE_DIR}

# Kill any existing services
for port in 3001 4001 4002 4010 4020 5001; do
  lsof -ti:\$port 2>/dev/null | xargs -r kill 2>/dev/null || true
done
pkill -f "bin/(tts|mock_idp|ab|ib|demo_app)" 2>/dev/null || true
sleep 1

chmod +x bin/* 2>/dev/null || true
chmod +x scripts/*.sh 2>/dev/null || true   # scripts/ may be empty (no netem.sh)

mkdir -p logs

echo "[remote] Starting tTS..."
RUST_LOG=info nohup ./bin/tts > logs/tts.log 2>&1 &
sleep 3

echo "[remote] Starting Mock IdP..."
RUST_LOG=info nohup ./bin/mock_idp > logs/mock_idp.log 2>&1 &
sleep 1

echo "[remote] Starting IB..."
RUST_LOG=info \
MOCK_IDP_AUTHORIZE_URL="https://localhost:3001/authorize" \
TTS_ISSUE_URL="https://localhost:5001/issue" \
nohup ./bin/ib > logs/ib.log 2>&1 &
sleep 1

echo "[remote] Starting AB..."
# AB binds HTTPS on 0.0.0.0:4001 so cluster clients can reach it.
# All other services stay on localhost (inter-service only).
RUST_LOG=info \
AB_HTTPS_BIND="0.0.0.0:4001" \
IB_HTTPS_BASE="https://localhost:4002" \
nohup ./bin/ab > logs/ab.log 2>&1 &
sleep 2

# Verify ports
ALL_UP=true
for port in 3001 4001 4002 5001; do
  if ! lsof -i:\$port -sTCP:LISTEN >/dev/null 2>&1; then
    echo "[remote] ERROR: port \$port not listening" >&2
    ALL_UP=false
  fi
done

if [[ "\$ALL_UP" == true ]]; then
  echo "[remote] All services ready."
else
  echo "[remote] Some services failed. Check logs:" >&2
  cat logs/tts.log | tail -5
  exit 1
fi
REMOTE

echo -e "${GREEN}${BOLD}[deploy] Server ready at ${SERVER_IP}${NC}"
echo ""
echo "  AB HTTPS (inter-service): https://${SERVER_IP}:4001"
echo "  Test:  curl -sk -X POST https://${SERVER_IP}:4001/login -H 'Content-Type: application/json' -d '{\"app_id\":\"app_example_456\"}' | head -c 100"
echo ""
echo "  To stop:  ssh ${SERVER} 'pkill -f \"bin/(tts|mock_idp|ab|ib)\"; echo done'"
echo "  Logs:     ssh ${SERVER} 'tail -f ${REMOTE_DIR}/logs/ab.log'"
echo ""
