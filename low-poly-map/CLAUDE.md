# CLAUDE.md (test bench)

Test-bench notes only. Project status, layout, commands, architecture, constraints and the library plan are in the repo-root [`../CLAUDE.md`](../CLAUDE.md); the single contract document is [`../docs/contract.md`](../docs/contract.md) (read it before changing either side; do not copy it here) and the specs are in `../docs/`. This folder is the Godot 4.7 .NET project (renderer Mobile, Jolt) that hosts the `WorldStreamer` GDExtension from `../rust/` (workspace crate `simworld-godot`, library `libsimworld_godot.*`, wrapping `simworld-core`) and flies a camera over the streamed city.

## Run it

- `G=~/Downloads/Godot_mono.app/Contents/MacOS/Godot`; build Rust first (`cd ../rust && cargo build`), **sync the addon** (`../tools/sync_addon.sh`: `addons/simworld/` here is a generated, git-ignored copy of `../addons/simworld/`; edit the source there) and C# (`dotnet build`). The plugin `res://addons/simworld/plugin.cfg` is enabled in `project.godot`.
- Play: `$G --path .` (or F5 in the editor; main scene `scenes/CitySpike.tscn`). User arguments go after `--`, e.g. `$G --path . -- --time=13:00 --date=06-21`.
- Tile source: `http://127.0.0.1:8787/vilnius.pmtiles` by default (start `../data/serve.sh`; tiles already in the disk cache at `user://tile_cache` render offline). `--tile-url=` (empty) switches to the synthetic city.
- Headless smoke test: `$G --headless --path . --quit-after 120`. Expect `[WorldView] tile source`, `[WorldView] library api <major>.<minor>` (a `predates api_version` warning means a stale dylib; a library minor above the facade's `LatestKnownMinor` = 7 prints one `[WorldStreamerFacade] ... is newer than the facade's 0.7` info line, which is fine; the facade still runs against 0.5, `MinApiMinor`), `poll_fades available`, `WorldStreamer configured ... [simworld-godot <ver> (build ...)]`, the disk cache line, `attribution`, `effective bands`, `status: ok` and a `first tile` line, with no errors. On exit `[WorldView] shutdown` shows the cache lock was released. `slow upload` lines are informational.

## Keys and menus

- Fly: hold the right mouse button to look, WASD, Q/E down/up, Shift x4, Ctrl x10 (turbo), mouse wheel changes speed (default 80 m/s, no speed cap; altitude and speed in the overlay), F toggles the autopilot, PgUp/PgDn double/halve altitude.
- F2: daylight menu (bottom right). Time slider 0-24 h, date slider (day of year), clock speed (paused, 1x, 1 min/s, 10 min/s, 1 h/s), presets for the chosen date (Sunrise, Noon, Dusk, Night), a night-factor override, and a readout of sun elevation, azimuth, night factor, origin and UTC offset. Controls never take keyboard focus, so flying keeps working.
- F1: detailed debug overlay (FPS, draw calls, per-ring stats from `debug_stats`, upload time, tile rects, status / attribution lines). Off at startup: only the FPS label is on screen by default. H: outline hidden tiles, M: minimap zoom (both only while F1 is on).
- F3: settings panel (bottom-left, hidden at startup; `SettingsMenu`). Controls never take keyboard focus (only the speed number box while typing; Enter or a click outside releases it).
  - Movement: speed slider (log scale 1..2000 m/s) and number box (no cap), two-way with `FreeFlyCamera.Speed`, so the mouse wheel moves the slider. Live.
  - 3D building range: the four band edges (`bands.r0_edge_m`..`r3_edge_m`, 50..100000 m; R0 detailed buildings, R1 boxes, R2 merged, R3 lights only), kept strictly increasing (a neighbour that would cross is pushed along).
  - Hide buildings lower than: `buildings.r0/r1/r2.min_height_m`, 0..100 m (profile 2; rows are disabled on a library without the keys).
  - Light sprites (no windows yet): "Lights from band" `R0..R3` = `lights.min_ring` (profile 3, default R2): the finest band that gets light sprites. R1 lights the 0.6-2.5 km ring, R0 everywhere; a stopgap for the dark near field until the window shader exists (the sprites sit on the roofs). Launch form: `--set=lights.min_ring=1`.
  - Edges and heights are profile values read at configure time, so they apply by restarting the world: Apply button, or auto-apply 0.6 s after the last change (checkbox, default on). Each section has a Reset (library defaults; the initial slider values are the effective ones the bridge runs with, incl. `--area`/`--set`/`--profile`). The status line shows the effective values after every apply and tiles loaded / queued.
  - The panel's values are merged over the launch-arg profile (panel wins). A rejected profile keeps the previous world running, shows the library's error in red at the bottom and in the console, and reverts the sliders. The bottom line carries the attribution.
  - Origin on apply (`WorldView.ApplyProfileOverrides`, queued and run at the start of the next `_Process`, before `update_camera`): `configure_world` is a teleport (new origin, every delivered tile comes back through `poll_unload`), so the bridge configures with the camera's current geodetic position (`local_to_geodetic`, f32: up to about 0.4 m of jump) as the new origin, takes the unload batch immediately (before any new delivery can reuse a key; stragglers are released too and warned about), puts the camera at local x/z = 0 (altitude and orientation kept), clears its shift bookkeeping and fade list, and keeps the same `cache_dir` (lock kept, no `shutdown()`). Tiles are back within a few seconds. `WorldView.OriginLat/Lon` stay the startup position (the clock reads them).
- T: tone map Filmic/AgX. X: external view (auto exposure) on/off.

## Launch arguments (after `--`)

| Argument | Effect |
|---|---|
| `--tile-url=<url>` | PMTiles URL; empty = synthetic city |
| `--time=HH:MM`, `--date=MM-DD`, `--time-speed=<x>` | start local time, date (non-leap 2026), simulated seconds per second |
| `--night=<0..1>` | pin the night factor (turns the override on) |
| `--autopilot=<m/s>`, `--autopilot-climb=<m/s>` | level flight north at 300 m AGL / climb-descend cycle 100..8000 m |
| `--max-speed=<m/s>` | optional speed cap for the free-fly camera and the autopilot (no cap by default; the game sets its own; streaming budgets are tested up to 250 m/s, faster flight can show holes) |
| `--cam-alt=<m>`, `--cam-pitch=<deg>` | camera start altitude and pitch |
| `--tonemap=filmic\|agx`, `--no-exposure`, `--no-glow`, `--glow-blend=<mode>` | night-look tests |
| `--shot=<png>[,<frames>]`, `--shot-hud` | save the viewport after N frames (default 90) and quit; HUD hidden unless `--shot-hud` |
| `--light-alpha=<x>`, `--fake-fade=<x>` | debug: sprite alpha, fixed fade when Rust has no `poll_fades` |
| `--curvature=<x>` | visual earth curvature: 0 flat, 1 real Earth (default), e.g. 50 to see it clearly |
| `--settings-open` | start with the settings panel (F3) visible, e.g. for screenshots |
| `--overlay-detail` | start with the detailed overlay (F1) on |
| `--settings-selftest` | `SettingsMenu` self-check without input: after the first tiles plus 3 s it drives the panel widgets (speed slider/box/wheel mirror, auto-apply), then applies config A (R2 edge 3000, R3 edge 6000, R1 min height 8) and B (defaults) alternately 11 times with the camera moved between applies and flown off the origin first, checks per apply that the camera is back at x/z 0 with the same geodetic position, no tile is held when the reconfigure finishes, the effective profile matches, the pool in use equals the cover and the host holds what the library delivered; then a rejected out-of-range profile must leave the world untouched and revert the panel. Prints before/after queued / loaded per ring, tile root counts, the node count, and `SETTINGS SELFTEST PASS/FAIL: ...` with exit code 0/1. `$G --headless --path . -- --tile-url= --settings-selftest` (about 30 s; also works against the real archive) |
| `--cover-selftest` | run the visibility-rule self-test (`CoverSelfTest`) at startup; prints PASS/FAIL |
| `--profile=<path.json>` | world profile file (`res://`, `user://` or an OS path, relative to the cwd; globalized before it is passed as `profile_path`; the library reads it; api 0.6+) |
| `--set=<dotted.key>=<number>` (repeatable) | profile override, nested into `profile_overrides`, e.g. `--set=bands.r3_edge_m=10000 --set=bands.r2_edge_m=4000 --set=fades.cap_s=0.5 --set=buildings.r1.min_height_m=6` (profile 2: hide buildings lower than 6 m in R1 meshes). Numbers go in as floats. Wins over `--area`. Rejections (range, unknown key) are logged with the library's error text, then the world starts with the default profile |
| `--area=<0.05..1>` | scale of the loaded 3D area: multiplies the four band edges (defaults read from `world_profile()`: 600/2500/7000/25000 m) before any `--set`. Scales the library defaults, not the edges of a `--profile` file. The overlay's source line shows the effective `bands 0.6/2.5/7/25 km` |
| `--geodetic-test` | `GeodeticFlightTest`: flies a scripted path (north 7.2 km at 120 m/s, east 2.4 km, climb 600 m) through `update_camera_geodetic` only, places the camera from `geodetic_to_local`, checks origin shifts, quits with `GEODETIC TEST PASS/FAIL: ...` and exit code 0/1. Options `--geodetic-speed=<m/s>` (default 120), `--geodetic-dt=<s>` (simulated step, default 0.1), `--geodetic-fps=<n>` (frame cap, default 60), `--geodetic-timeout=<s>` (real-time failsafe, default 300). CI form: `$G --headless --path . -- --tile-url= --geodetic-test --geodetic-speed=240` (about 9 s) |

## Scripts (`scripts/`, namespace `LowPolyMap`)

Library code lives in the addon (`../addons/simworld/runtime/`, namespace `SimWorld`; the bench copy is generated): `WorldView` (the old `WorldView`; scene node `WorldView`, `AutoStart` off), `WorldStreamerFacade`, `TilePool`, `TileCover`, `TileKey`, `IWorldCameraSource`, `WorldRenderSettings`, `NightLook`, `ShaderGlobals`. See the root CLAUDE.md "Addon". `FreeFlyCamera` implements `IWorldCameraSource`; `WorldView.Camera` is only the `Camera3D` for fov / far plane.
Host-only (`scripts/`, namespace `LowPolyMap`): `BenchArgs` (reads every launch argument below that touches the world: `--tile-url`, `--curvature`, `--light-alpha`, `--fake-fade`, `--profile`, `--area`, `--set`, `--cover-selftest`; applies it to the `WorldView`, then calls `Start()`; the Vilnius origin and `user://tile_cache` are scene values of the `WorldView` node), `GeodeticFlightTest` (P4 acceptance driver, inert without `--geodetic-test`; it sets `WorldView.ExternalCameraUpdate` to replace `update_camera`), `FreeFlyCamera`, `DebugOverlay`, `TimeOfDay` (date, clock, NOAA sun position, night factor, aims the `Sun` light; moon at night; UTC offset = round(longitude / 15) unless set; calls `WorldView.SetTime`), `DaylightMenu`, `SettingsMenu` (F3 panel, reconfigure UI and `--settings-selftest`), `NightRenderController` (host policy: exposure, tone map, sun / sky / ambient colours; bloom and fog come from the addon's `NightLook`, see audit A12), `CoverSelfTest`.
Shaders: addon `building_day` (placeholder, no windows yet), `ground`, `light_sprite`, `lod_dither.gdshaderinc`, `earth_curve.gdshaderinc`; bench `shaders/fallback_ground` (the scene's big plane, includes the addon's `earth_curve`).
Earth curvature is visual only: every world shader drops vertices by d²/2R from the camera (globals `simworld_curve_center`, `simworld_curve_inv_2r`, registered by the plugin / `WorldView`, set each frame by `WorldView`). Rust, physics and AGL stay flat. Any new world mesh shader must include `earth_curve.gdshaderinc` (relative `#include` inside the addon), or it will float above the curved ground at range.

## Gotchas

- Hand-written `.tscn`: node-typed `[Export]`s need `node_paths=PackedStringArray("Prop", ...)` in the `[node ...]` header or they stay null at runtime. Verify by loading the real scene.
- Never `QueueFree` + `new` tiles; use `TilePool`. Never call Rust from other threads.
- Rust cwd rules: run cargo from `../rust/`, never from this folder.
