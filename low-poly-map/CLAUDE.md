# CLAUDE.md (test bench)

Test-bench notes only, kept short because this file loads whenever work touches `low-poly-map/`. Project status, commands, architecture and subagent rules: repo-root [`../CLAUDE.md`](../CLAUDE.md). Contract: [`../docs/contract.md`](../docs/contract.md) (read only the sections you need). This folder is the Godot 4.7 .NET project (renderer Mobile, Jolt) that hosts the `WorldStreamer` GDExtension from `../rust/` and flies a camera over the streamed city.

Detail, read on demand:
- `../docs/bench/keys-and-args.md`: keys, F1-F4 overlays and menus, every launch argument (`--tile-url`, `--world`, `--seed-spot`, `--set`, `--view`, `--terrain*`, `--hole-*`, `--benchmark`, ...) and what each self-test checks.
- `../docs/bench/diagnostics.md`: performance measurement (markers, frame split, GC analysis, benchmark JSON, baselines).
- `../docs/bench/notes.md`: bench scripts, night windows, light sprites, landmark layer, world seed host notes.
- `../docs/addon-notes.md`: addon internals (`WorldView`, pools, fades, LOD host side, terrain host side, seed host side).

## Run it

- `G=~/Downloads/Godot_mono.app/Contents/MacOS/Godot`; build Rust first (`cd ../rust && cargo build`), **sync the addon** (`../tools/sync_addon.sh`: `addons/simworld/` here is a generated, git-ignored copy of `../addons/simworld/`; edit the source there) and C# (`dotnet build`). The plugin `res://addons/simworld/plugin.cfg` is enabled in `project.godot`.
- Play: `$G --path .` (or F5 in the editor; main scene `scenes/CitySpike.tscn`). User arguments go after `--`, e.g. `$G --path . -- --time=13:00 --date=06-21`.
- Tile source: `http://127.0.0.1:8787/vilnius.pmtiles` by default (start `../data/serve.sh`; tiles already in the disk cache at `user://tile_cache` render offline). `--tile-url=` (empty) switches to the synthetic city.
- Headless smoke test: `$G --headless --path . --quit-after 120`. Expect `[WorldView] tile source`, `[WorldView] library api <major>.<minor>` (a `predates api_version` warning means a stale dylib; the facade expects major 1 (`ExpectedApiMajor`) and refuses every 0.x library with an error naming the fix, i.e. rebuild `../rust`; a library minor above the facade's `LatestKnownMinor` = 11 prints one `[WorldStreamerFacade] ... is newer than the facade's 1.11` info line, which is fine), `poll_fades available`, `WorldStreamer configured ... [simworld-godot <ver> (build ...)]`, the disk cache line, `attribution`, `effective edges` (tau_eff, quality, library view, live edges), `status: ok` and a `first tile` line, with no errors. On exit `[WorldView] shutdown` shows the cache lock was released. `slow upload` lines are informational.


## Self-tests (headless, from this folder; `/bench-check` runs them through `selftest-runner`)

| Test | Command after `$G --headless --path .` | Verdict line |
|---|---|---|
| smoke | `--quit-after 120 -- --tile-url=` | `first tile`, no errors |
| cover | `-- --tile-url= --cover-selftest` | `COVER LIVE CHECK PASS` |
| fade | `-- --tile-url= --fade-selftest` | `FADE SELFTEST PASS` |
| window | `-- --tile-url= --window-selftest` | `WINDOW SELFTEST PASS` |
| lod | `-- --tile-url= --lod-selftest` | `LOD SELFTEST PASS` |
| perf | `-- --tile-url= --perf-selftest` | `PERF SELFTEST PASS` |
| geodetic | `-- --tile-url= --geodetic-test --geodetic-speed=240` | `GEODETIC TEST PASS` |
| settings | `-- --tile-url= --settings-selftest` | `SETTINGS SELFTEST PASS` |
| terrain-collision | `-- --tile-url= --terrain-collision-selftest` | `TERRAIN COLLISION SELFTEST PASS` |
| terrain | `-- --tile-url= --terrain-selftest` | `TERRAIN SELFTEST PASS` |
| seed | `res://scenes/SeedTwoCities.tscn --quit-after 3000 -- --seed-selftest` | `SEED SELFTEST PASS` |

Windowed only (need a visible window and an idle machine): `--benchmark=<name>` (`/bench-measure`), `--hole-check --hole-pixels` (`tools/hole_pixels.sh`).

Seed world by hand: `$G --path . res://scenes/SeedTwoCities.tscn -- --seed-spot=eyvi --time=22:00` (spots: vilnius, kaunas, eyvi, eyka, eyvi-field, eyka-field, a1, cruise, coast, lake).

## Gotchas

- Hand-written `.tscn`: node-typed `[Export]`s need `node_paths=PackedStringArray("Prop", ...)` in the `[node ...]` header or they stay null at runtime. Verify by loading the real scene.
- Never `QueueFree` + `new` tiles; use `TilePool`. Never call Rust from other threads.
- Rust cwd rules: run cargo from `../rust/`, never from this folder.
