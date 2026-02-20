#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Load .env into process environment
if [ -f "$ROOT/.env" ]; then
  set -a
  source "$ROOT/.env"
  set +a
  echo "[start] Loaded .env"
fi

cleanup() {
  echo "Stopping servers..."
  kill $RUST_PID $NEXT_PID 2>/dev/null || true
  wait $RUST_PID $NEXT_PID 2>/dev/null || true
}
trap cleanup EXIT

# Start Rust backend
echo "[start] Starting Rust backend on :8080"
cd "$ROOT"
./target/release/cli-agent serve --host 127.0.0.1 --port 8080 &
RUST_PID=$!

# Start Next.js production server
echo "[start] Starting Next.js on :3000"
cd "$ROOT/web"
npm start &
NEXT_PID=$!

echo ""
echo "  Backend:  http://localhost:8080"
echo "  Frontend: http://localhost:3000"
echo ""

wait
