#!/usr/bin/env bash
# PostToolUse (Edit|Write): after an edit of the addon source, copy it into the bench (tools/sync_addon.sh).
root="$(cd "$(dirname "$0")/../.." && pwd)"
path="$(jq -r '.tool_input.file_path // empty')"
case "$path" in
  "$root"/addons/simworld/*)
    if ! out="$("$root"/tools/sync_addon.sh 2>&1)"; then
      echo "tools/sync_addon.sh failed: $out" >&2; exit 2
    fi
    # Surface pulled .uid files (they need committing); stay quiet otherwise.
    grep '^pulled new uid' <<<"$out" >&2 && exit 2 ;;
esac
exit 0
