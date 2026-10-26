#!/usr/bin/env bash
# Serve data/out/ on http://127.0.0.1:8787/ with HTTP Range support (needed for PMTiles).
# Local stand-in for the real CDN/static host (still undecided).
set -euo pipefail
DATA_DIR="$(cd "$(dirname "$0")" && pwd)"
PORT="${PORT:-8787}"
# http-server answers Range requests with 206 Partial Content. -c-1 disables caching headers.
exec npx --yes http-server@14.1.1 "$DATA_DIR/out" -a 127.0.0.1 -p "$PORT" -c-1
