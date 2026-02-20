#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

cleanup() {
  echo "Stopping servers..."
  kill $RUST_PID $NEXT_PID 2>/dev/null || true
  wait $RUST_PID $NEXT_PID 2>/dev/null || true
}
trap cleanup EXIT

# Start Rust backend
echo "[dev] Starting Rust backend on :8080"
cd "$ROOT"
cargo run -- serve --host 127.0.0.1 --port 8080 &
RUST_PID=$!

# Start Next.js frontend
echo "[dev] Starting Next.js frontend on :3000"
cd "$ROOT/web"
if [ ! -d "node_modules" ]; then
  echo "[dev] Installing npm dependencies..."
  npm install
fi
npm run dev &
NEXT_PID=$!

echo ""
echo "  Backend:  http://localhost:8080"
echo "  Frontend: http://localhost:3000"
echo ""

wait
