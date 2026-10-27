#!/usr/bin/env bash
# Builds the simworld-godot library for this machine's platform and copies it into addons/simworld/bin/ (not committed:
# release zips carry the binaries). One CI runner per platform runs this; there is no cross-compilation (the disk cache bundles
# SQLite, which needs the target's C toolchain).
#
#   tools/build_addon.sh              debug + release for the host platform
#   tools/build_addon.sh --release    release only
#   tools/build_addon.sh --ios        also build the iOS static library and wrap it in an .xcframework (macOS host, unverified on device)
#
# macOS: universal dylib (aarch64 + x86_64 merged with lipo) when both rust targets are installed
# (`rustup target add x86_64-apple-darwin`), else the host architecture only, with a warning.
# Output names match addons/simworld/simworld.gdextension.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root/rust"
bin="$root/addons/simworld/bin"

profiles=(debug release)
ios=0
for a in "$@"; do
  case "$a" in
    --release) profiles=(release) ;;
    --ios) ios=1 ;;
    *) echo "unknown argument: $a" >&2; exit 2 ;;
  esac
done

cargo_profile_flag() { [[ "$1" == release ]] && echo "--release" || echo ""; }
target_dir() { [[ "$1" == release ]] && echo release || echo debug; }

case "$(uname -s)" in
  Darwin)
    mkdir -p "$bin/macos"
    installed="$(rustup target list --installed 2>/dev/null || true)"
    for p in "${profiles[@]}"; do
      outs=()
      for t in aarch64-apple-darwin x86_64-apple-darwin; do
        if grep -qx "$t" <<<"$installed"; then
          cargo build -p simworld-godot --target "$t" $(cargo_profile_flag "$p")
          outs+=("target/$t/$(target_dir "$p")/libsimworld_godot.dylib")
        else
          echo "note: rust target $t is not installed, skipped"
        fi
      done
      if [[ ${#outs[@]} -eq 0 ]]; then echo "no macOS rust target installed" >&2; exit 1; fi
      [[ ${#outs[@]} -eq 1 ]] && echo "warning: macOS $p library is single-architecture (not universal)"
      lipo -create "${outs[@]}" -output "$bin/macos/libsimworld_godot.$p.dylib"
      echo "wrote $bin/macos/libsimworld_godot.$p.dylib ($(lipo -archs "$bin/macos/libsimworld_godot.$p.dylib"))"
    done
    if [[ $ios -eq 1 ]]; then
      mkdir -p "$bin/ios"
      for p in "${profiles[@]}"; do
        cargo build -p simworld-godot --target aarch64-apple-ios $(cargo_profile_flag "$p")
        out="$bin/ios/simworld_godot.$p.xcframework"
        rm -rf "$out"
        xcodebuild -create-xcframework -library "target/aarch64-apple-ios/$(target_dir "$p")/libsimworld_godot.a" -output "$out"
        echo "wrote $out"
      done
    fi
    ;;
  Linux)
    mkdir -p "$bin/linux"
    for p in "${profiles[@]}"; do
      cargo build -p simworld-godot $(cargo_profile_flag "$p")
      cp "target/$(target_dir "$p")/libsimworld_godot.so" "$bin/linux/libsimworld_godot.$p.so"
      echo "wrote $bin/linux/libsimworld_godot.$p.so"
    done
    ;;
  MINGW*|MSYS*|CYGWIN*)
    mkdir -p "$bin/windows"
    for p in "${profiles[@]}"; do
      cargo build -p simworld-godot $(cargo_profile_flag "$p")
      cp "target/$(target_dir "$p")/simworld_godot.dll" "$bin/windows/simworld_godot.$p.dll"
      echo "wrote $bin/windows/simworld_godot.$p.dll"
    done
    ;;
  *) echo "unsupported host: $(uname -s)" >&2; exit 1 ;;
esac
