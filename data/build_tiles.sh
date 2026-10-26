#!/usr/bin/env bash
# Build data/out/vilnius.pmtiles (z13-z16) from the Geofabrik Lithuania extract
# using planetiler's generate-custom with schema/simplane.yml, run in Docker (no Java needed).
set -euo pipefail

DATA_DIR="$(cd "$(dirname "$0")" && pwd)"
PLANETILER_IMAGE="${PLANETILER_IMAGE:-ghcr.io/onthegomap/planetiler:0.10.2}"
PBF_URL="https://download.geofabrik.de/europe/lithuania-latest.osm.pbf"
PBF="$DATA_DIR/sources/lithuania-latest.osm.pbf"
# Central Vilnius + 15-25 km margin for the outer LOD rings: minLon,minLat,maxLon,maxLat
BOUNDS="${BOUNDS:-24.85,54.45,25.70,54.92}"
OUT_NAME="${OUT_NAME:-vilnius.pmtiles}"
JAVA_HEAP="${JAVA_HEAP:-4g}"

mkdir -p "$DATA_DIR/sources" "$DATA_DIR/out"

if [[ -s "$PBF" ]]; then
  echo "Using existing $PBF"
else
  echo "Downloading $PBF_URL"
  curl -fL --retry 3 -o "$PBF.part" "$PBF_URL"
  mv "$PBF.part" "$PBF"
fi

docker run --rm \
  -e JAVA_TOOL_OPTIONS="-Xmx$JAVA_HEAP" \
  -v "$DATA_DIR":/data \
  "$PLANETILER_IMAGE" \
  generate-custom \
  --schema=/data/schema/simplane.yml \
  --osm_path=/data/sources/lithuania-latest.osm.pbf \
  --bounds="$BOUNDS" \
  --minzoom=13 --maxzoom=16 \
  --tmpdir=/data/tmp \
  --output="/data/out/$OUT_NAME" \
  --force

rm -rf "$DATA_DIR/tmp"
ls -lh "$DATA_DIR/out/$OUT_NAME"
