# CLAUDE.md (test bench)

Test-bench notes only. Project status, layout, commands, architecture, constraints and the library plan are in the repo-root [`../CLAUDE.md`](../CLAUDE.md); the contract and specs are in `../docs/`. This folder is the Godot 4.7 .NET project (renderer Mobile, Jolt) that hosts the `WorldStreamer` GDExtension from `../rust/` (workspace crate `simworld-godot`, library `libsimworld_godot.*`, wrapping `simworld-core`) and flies a camera over the streamed city.

## Run it

- `G=~/Downloads/Godot_mono.app/Contents/MacOS/Godot`; build Rust first (`cd ../rust && cargo build`) and C# (`dotnet build`).
- Play: `$G --path .` (or F5 in the editor; main scene `scenes/CitySpike.tscn`). User arguments go after `--`, e.g. `$G --path . -- --time=13:00 --date=06-21`.
- Tile source: `http://127.0.0.1:8787/vilnius.pmtiles` by default (start `../data/serve.sh`; tiles already in the disk cache at `user://tile_cache` render offline). `--tile-url=` (empty) switches to the synthetic city.
- Headless smoke test: `$G --headless --path . --quit-after 120`. Expect `[StreamerBridge] tile source`, `poll_fades available`, `WorldStreamer configured ... [simworld-godot <ver> (build ...)]`, the disk cache line and a `first tile` line, with no errors. `slow upload` lines are informational.

## Keys and menus

- Fly: hold the right mouse button to look, WASD, Q/E down/up, Shift x3, mouse wheel changes speed (0-250 m/s cap; altitude and speed in the overlay), F toggles the autopilot, PgUp/PgDn double/halve altitude.
- F2: daylight menu (bottom right). Time slider 0-24 h, date slider (day of year), clock speed (paused, 1x, 1 min/s, 10 min/s, 1 h/s), presets for the chosen date (Sunrise, Noon, Dusk, Night), a night-factor override, and a readout of sun elevation, azimuth, night factor, origin and UTC offset. Controls never take keyboard focus, so flying keeps working.
- F3: debug overlay (FPS, draw calls, per-ring stats from `debug_stats`, upload time, tile rects). H: outline hidden tiles. M: minimap zoom.
- T: tone map Filmic/AgX. X: external view (auto exposure) on/off.

## Launch arguments (after `--`)

| Argument | Effect |
|---|---|
| `--tile-url=<url>` | PMTiles URL; empty = synthetic city |
| `--time=HH:MM`, `--date=MM-DD`, `--time-speed=<x>` | start local time, date (non-leap 2026), simulated seconds per second |
| `--night=<0..1>` | pin the night factor (turns the override on) |
| `--autopilot=<m/s>`, `--autopilot-climb=<m/s>` | level flight north at 300 m AGL / climb-descend cycle 100..8000 m |
| `--cam-alt=<m>`, `--cam-pitch=<deg>` | camera start altitude and pitch |
| `--tonemap=filmic\|agx`, `--no-exposure`, `--no-glow`, `--glow-blend=<mode>` | night-look tests |
| `--shot=<png>[,<frames>]`, `--shot-hud` | save the viewport after N frames (default 90) and quit; HUD hidden unless `--shot-hud` |
| `--light-alpha=<x>`, `--fake-fade=<x>` | debug: sprite alpha, fixed fade when Rust has no `poll_fades` |
| `--cover-selftest` | run the visibility-rule self-test (`CoverSelfTest`) at startup; prints PASS/FAIL |

## Scripts (`scripts/`, namespace `LowPolyMap`)

Library-grade (move to the addon in P5): `WorldStreamerFacade` (only place with method-name strings), `StreamerBridge` (owns the node, uploads tiles within the 2 ms budget, origin shifts), `TilePool`, `TileCover`, `TileKey`.
Host-only (stay here): `FreeFlyCamera`, `DebugOverlay`, `TimeOfDay` (date, clock, NOAA sun position, night factor, aims the `Sun` light; moon at night; UTC offset = round(longitude / 15) unless set), `DaylightMenu`, `NightRenderController` (glow, fog, tone map, exposure; mixes library look and host policy, see audit A12), `CoverSelfTest`.
Shaders (`shaders/`): `building_day` (placeholder, no windows yet), `ground`, `light_sprite`, `lod_dither.gdshaderinc`.

## Gotchas

- Hand-written `.tscn`: node-typed `[Export]`s need `node_paths=PackedStringArray("Prop", ...)` in the `[node ...]` header or they stay null at runtime. Verify by loading the real scene.
- Never `QueueFree` + `new` tiles; use `TilePool`. Never call Rust from other threads.
- Rust cwd rules: run cargo from `../rust/`, never from this folder.
