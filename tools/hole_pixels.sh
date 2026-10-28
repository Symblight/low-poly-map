#!/usr/bin/env bash
# Windowed pixel hole test (bench --hole-check --hole-pixels): flies a seeded flight over the Vilnius v2 archive for N seconds and fails (exit 1) when a drawn frame has
# magenta where the host holds a drawn ground triangle (or no tile), when a structural check fires (root-dither-gap, engine-culled-visible-ground, ...), or when fewer than
# 100 frames were drawn (a minimized / occluded window is not drawn by macOS: keep the window visible).
#   tools/hole_pixels.sh [plain|zoom|look] [seed] [seconds] [extra bench args...]
# Needs the tile server (data/serve.sh, http://127.0.0.1:8787/vilnius-v2.pmtiles), a built Rust library, a synced addon and `dotnet build`.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
G="${GODOT:-$HOME/Downloads/Godot_mono.app/Contents/MacOS/Godot}"
mode="${1:-plain}"; seed="${2:-1}"; secs="${3:-150}"; shift $(( $# > 3 ? 3 : $# )) || true
case "$mode" in
  plain) flight=(--hole-flight="$seed") ;;
  zoom)  flight=(--hole-flight="$seed" --hole-zoom=6) ;;
  look)  flight=(--hole-flight="$seed" --hole-look) ;;
  *) echo "mode: plain | zoom | look" >&2; exit 2 ;;
esac
cd "$ROOT/low-poly-map"
exec caffeinate -dimsu "$G" --path . --resolution 1920x1080 -- --tile-url=http://127.0.0.1:8787/vilnius-v2.pmtiles \
  --cache-dir=user://cache_holes2 --hole-check="$secs" --hole-pixels --no-terrain --hide-fallback --hole-teleports=0 --hole-radius=3500 "${flight[@]}" "$@"
