#!/usr/bin/env bash
# Copies addons/simworld/ (the library addon at the repo root) into the test bench project, where Godot can load it.
# The copy (low-poly-map/addons/simworld/) is generated and ignored by git: edit the addon at the repo root, then sync.
#
# Godot writes a .uid file next to every new script and shader it sees. Those that exist only in the bench copy are copied back
# to the source first (commit them: scenes reference scripts by uid), so the --delete below never throws a uid away.
#
# Not copied: simworld.gdextension and bin/ (the bench loads the library from rust/target through
# low-poly-map/simplane_world.gdextension, the editor-only dev override).
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
src="$root/addons/simworld"
dst="$root/low-poly-map/addons/simworld"

if [[ -d "$dst" ]]; then
  (cd "$dst" && find . -name '*.uid' -print0) | while IFS= read -r -d '' f; do
    if [[ ! -e "$src/$f" && -e "$src/${f%.uid}" ]]; then
      cp "$dst/$f" "$src/$f"
      echo "pulled new uid: addons/simworld/${f#./}"
    fi
  done
fi

mkdir -p "$dst"
rsync -a --delete \
  --exclude 'simworld.gdextension' --exclude 'bin/' \
  "$src/" "$dst/"
echo "synced $src -> $dst"
