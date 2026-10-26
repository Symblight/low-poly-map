# CLAUDE.md (test bench)

Test-bench notes only. Project status, layout, commands, architecture, constraints and the library plan are in the repo-root [`../CLAUDE.md`](../CLAUDE.md); the single contract document is [`../docs/contract.md`](../docs/contract.md) (read it before changing either side; do not copy it here) and the specs are in `../docs/`. This folder is the Godot 4.7 .NET project (renderer Mobile, Jolt) that hosts the `WorldStreamer` GDExtension from `../rust/` (workspace crate `simworld-godot`, library `libsimworld_godot.*`, wrapping `simworld-core`) and flies a camera over the streamed city.

## Run it

- `G=~/Downloads/Godot_mono.app/Contents/MacOS/Godot`; build Rust first (`cd ../rust && cargo build`) and C# (`dotnet build`).
- Play: `$G --path .` (or F5 in the editor; main scene `scenes/CitySpike.tscn`). User arguments go after `--`, e.g. `$G --path . -- --time=13:00 --date=06-21`.
- Tile source: `http://127.0.0.1:8787/vilnius.pmtiles` by default (start `../data/serve.sh`; tiles already in the disk cache at `user://tile_cache` render offline). `--tile-url=` (empty) switches to the synthetic city.
- Headless smoke test: `$G --headless --path . --quit-after 120`. Expect `[StreamerBridge] tile source`, `[StreamerBridge] library api <major>.<minor>` (a `predates api_version` warning means a stale dylib; a library minor above the facade's `LatestKnownMinor` = 7 prints one `[WorldStreamerFacade] ... is newer than the facade's 0.7` info line, which is fine; the facade still runs against 0.5, `MinApiMinor`), `poll_fades available`, `WorldStreamer configured ... [simworld-godot <ver> (build ...)]`, the disk cache line, `attribution`, `effective bands`, `status: ok` and a `first tile` line, with no errors. On exit `[StreamerBridge] shutdown` shows the cache lock was released. `slow upload` lines are informational.

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
| `--curvature=<x>` | visual earth curvature: 0 flat, 1 real Earth (default), e.g. 50 to see it clearly |
| `--cover-selftest` | run the visibility-rule self-test (`CoverSelfTest`) at startup; prints PASS/FAIL |
| `--profile=<path.json>` | world profile file (`res://`, `user://` or an OS path, relative to the cwd; globalized before it is passed as `profile_path`; the library reads it; api 0.6+) |
| `--set=<dotted.key>=<number>` (repeatable) | profile override, nested into `profile_overrides`, e.g. `--set=bands.r3_edge_m=10000 --set=bands.r2_edge_m=4000 --set=fades.cap_s=0.5`. Numbers go in as floats. Wins over `--area`. Rejections (range, unknown key) are logged with the library's error text, then the world starts with the default profile |
| `--area=<0.05..1>` | scale of the loaded 3D area: multiplies the four band edges (defaults read from `world_profile()`: 600/2500/7000/25000 m) before any `--set`. Scales the library defaults, not the edges of a `--profile` file. The overlay's source line shows the effective `bands 0.6/2.5/7/25 km` |
| `--geodetic-test` | `GeodeticFlightTest`: flies a scripted path (north 7.2 km at 120 m/s, east 2.4 km, climb 600 m) through `update_camera_geodetic` only, places the camera from `geodetic_to_local`, checks origin shifts, quits with `GEODETIC TEST PASS/FAIL: ...` and exit code 0/1. Options `--geodetic-speed=<m/s>` (default 120), `--geodetic-dt=<s>` (simulated step, default 0.1), `--geodetic-fps=<n>` (frame cap, default 60), `--geodetic-timeout=<s>` (real-time failsafe, default 300). CI form: `$G --headless --path . -- --tile-url= --geodetic-test --geodetic-speed=240` (about 9 s) |

## Scripts (`scripts/`, namespace `LowPolyMap`)

Library-grade (move to the addon in P5): `WorldStreamerFacade` (only place with method-name strings; checks `api_version()` once on attach: major mismatch = facade unavailable, low minor = warning naming the missing features, unknown = method probing as before), `StreamerBridge` (owns the node, configures it with `configure_world` on api 0.6+ and the profile args, uploads tiles within the 2 ms budget, origin shifts through `take_origin_shift`, logs `status_changed` once per change, calls `shutdown()` on `_ExitTree`; the `origin_shifted` signal is only counted, never used for shifting), `TilePool`, `TileCover`, `TileKey`.
Host-only (stay here): `GeodeticFlightTest` (P4 acceptance driver, inert without `--geodetic-test`; it sets `StreamerBridge.ExternalCameraUpdate` to replace `update_camera`), `FreeFlyCamera`, `DebugOverlay`, `TimeOfDay` (date, clock, NOAA sun position, night factor, aims the `Sun` light; moon at night; UTC offset = round(longitude / 15) unless set), `DaylightMenu`, `NightRenderController` (glow, fog, tone map, exposure; mixes library look and host policy, see audit A12), `CoverSelfTest`.
Shaders (`shaders/`): `building_day` (placeholder, no windows yet), `ground`, `light_sprite`, `fallback_ground` (the scene's big plane), `lod_dither.gdshaderinc`, `earth_curve.gdshaderinc`.
Earth curvature is visual only: every world shader drops vertices by d²/2R from the camera (globals `curve_center`, `curve_inv_2r` in `project.godot`, set each frame by `StreamerBridge`). Rust, physics and AGL stay flat. Any new world mesh shader must include `earth_curve.gdshaderinc`, or it will float above the curved ground at range.

## Gotchas

- Hand-written `.tscn`: node-typed `[Export]`s need `node_paths=PackedStringArray("Prop", ...)` in the `[node ...]` header or they stay null at runtime. Verify by loading the real scene.
- Never `QueueFree` + `new` tiles; use `TilePool`. Never call Rust from other threads.
- Rust cwd rules: run cargo from `../rust/`, never from this folder.
