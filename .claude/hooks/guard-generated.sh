#!/usr/bin/env bash
# PreToolUse (Edit|Write|NotebookEdit): refuse edits to generated folders (CLAUDE.md "Layout", "Commands").
root="$(cd "$(dirname "$0")/../.." && pwd)"
path="$(jq -r '.tool_input.file_path // .tool_input.notebook_path // empty')"
[[ -z "$path" ]] && exit 0
case "$path" in
  "$root"/low-poly-map/addons/simworld/*)
    echo "Blocked: low-poly-map/addons/simworld/ is a generated copy. Edit addons/simworld/ at the repo root (tools/sync_addon.sh copies it)." >&2; exit 2 ;;
  "$root"/low-poly-map/.godot/*|"$root"/rust/target/*|"$root"/data/out/*|"$root"/data/sources/*)
    echo "Blocked: $path is generated (ignored by git); change its source instead." >&2; exit 2 ;;
esac
exit 0
