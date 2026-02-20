#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

echo "[build] Building Rust backend..."
cd "$ROOT"
cargo build --release

echo "[build] Building Next.js frontend..."
cd "$ROOT/web"
npm install
npm run build

echo ""
echo "[build] Done."
echo "  Rust binary: target/release/cli-agent"
echo "  Next.js:     web/.next/"
