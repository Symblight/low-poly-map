# Spec: SimPlane integration contract (investigation)

2026-10-29 · Aleksei. **Status: investigation, no code.** Follows `docs/decision-library-direction.md` (decision 5: integrate first). This document lists what both sides must prepare before `WorldView` can replace SimPlane's flat test world, ranked by how much each item blocks a first flight over the seed world.

SimPlane state: JSBSim and the custom flight model are tested; **no map feature was ever wired**. The integration is a new seam, not a replacement of an existing one.

## 1. SimPlane today (facts, `../Simplane/simplane`, 2026-10-29)

| Topic | SimPlane | Library |
| --- | --- | --- |
| Renderer | **Forward+** (`project.godot`), SunshineClouds2 compute clouds | Built and measured on **Mobile**; never run under Forward+ |
| World origin | Fixed constant **47.0 N 8.0 E**, in two copies: `Navigation/WorldGeoReference.cs` and private constants in `Flight/JsbsimFlightModel.cs:34` (`NdChecks` asserts they agree) | Origin = `configure_world(lat, lon)`, f64 ENU, **floating origin** re-centred beyond 5 km, `origin_shifted` signal, `geodetic_to_local` / `local_to_geodetic` |
| Aircraft placement | JSBSim lat/lon linearised at the fixed origin into an f32 node position (`JsbsimFlightModel.cs:678-681`); the custom model keeps local metres only | Everything handed out is relative to the current origin |
| Ground | Flat y = 0. `IFlightModel.GroundHeightAt` is a delegate, default `_ => 0f`; JSBSim gets `position/terrain-elevation-asl-ft` from it every tick; the custom model samples it per wheel | `ground_height_at(x, z)` main thread, f64, the exact height function, valid as soon as the world is configured (procedural, no tile needed) |
| Spawn | `AircraftController._spawnPoint` = the node's position in `Main.tscn` (runway threshold at the origin), `StartHeadingDeg`, missions Take off / Landing / Custom | `airports()`: runways with ends, headings, elevations, at real positions |
| Nav data | `Navigation/Data/*.csv`, OurAirports extract within 600 NM of 47 N 8 E plus the synthetic `SIMP` | `world/two-cities/airports.json`, also OurAirports, Lithuania |
| Cameras | Two `Camera3D` nodes (`ChaseCamera`, `MountedCamera`), `ViewSwitcher` picks one; far 60 000 m, mounted near 0.05 m | `WorldView.Camera` is one `Camera3D`; `IWorldCameraSource` is the general hook (position, velocity, AGL, origin shift) |
| Environment | `SkyController` duplicates the `WorldEnvironment`, is the single writer of fog (density from visibility), tracks a **static** `Sun` (rotation fixed in `TestWorld.tscn`, shadows to 400 m) | `NightLook.ApplyStatic` / `Update` write glow and fog on the host's environment; `SetTime(hour, night)` drives windows and sprites; the clock is host policy (bench `TimeOfDay`) |
| Time of day | None | Needed for night; the Rust `set_clock` / `daylight()` stays unbuilt (planned 1.8, frozen) |
| Lifecycle | `GameSession` static state, Restart reloads `Main.tscn`, loading screen with 10 warm-up frames | One node = one world, `shutdown()`, cache folder lock, material prewarm 24 frames |
| C# project | `net8.0`, **`Nullable` disabled** | Addon written with nullable annotations (`Camera3D?`), compiled into the host assembly |
| Export | macOS only, no iOS preset | `.gdextension` has macOS / Windows / Linux / iOS entries, only macOS built |
| Native addons | `addons/jsbsim/`: `.gdextension` + `bin/` + `dependencies` block | `addons/simworld/`: same shape, `tools/build_addon.sh` fills `bin/` |
| Tests | Headless check scenes, `CheckReport`, `tools/run_checks.sh`; flight checks assume flat ground at 0 | Headless smoke run, `*-selftest` bench modes |

## 2. Blocking items (a first flight does not happen without them)

### B1. One coordinate frame, owned by the library

SimPlane's origin is 1 500 km from Vilnius. An f32 node position from 47 N 8 E is useless there, so the fixed origin has to go regardless of the library. Decision proposed: **the library owns the local frame.**

- `WorldGeoReference` stops being constants. It becomes a thin wrapper over `WorldView.Streamer` (`GeodeticToLocal` / `LocalToGeodetic`), with the origin set at flight start from the chosen airport. The private copy in `JsbsimFlightModel` is removed; the model gets lat/lon from JSBSim natively (`s[5]`, `s[6]`) and asks the wrapper for the node position each tick.
- The custom 6DOF model keeps local metres and must apply the shift itself (subscribe to `OriginShifted`, subtract the vector from its position state). Telemetry lat/lon for it goes through the same wrapper.
- Every other node in local space subscribes to `WorldView.OriginShifted` (C# event) and subtracts the shift in the same frame: windsock, chase camera smoothing state, cloud slab positions in `SkyController`, airport markings (the addon does its own), any particle or marker.
- Camera input: use `update_camera_geodetic` through an `IWorldCameraSource` that reads the **current** camera from `ViewSwitcher.Current`, not one fixed `Camera3D`. `Camera3DSource` is the template.
- Heights: JSBSim's altitude is above the ellipsoid, the library has no geoid (contract 3.3 b). Accepted: one convention everywhere (JSBSim is told terrain elevation = `ground_height_at`, so the aircraft rests on the drawn ground); the displayed altitude differs from true MSL by the local geoid undulation (about 20 to 30 m in the Baltics). Nobody at ETS level notices; written down so it is not rediscovered as a bug.
- `NdChecks` and the ND presenter: they project through `WorldGeoReference`; the test that pins the constants changes to a round-trip test through the library.

### B2. Ground height wired

`model.GroundHeightAt = p => (float)view.Streamer.GroundHeightAt(p.X, p.Z)`. Available right after `Start()`, before any tile, so a ground start needs no wait for the height.

Gap: **the drawn runway is above `ground_height_at`** by the stacking lift (0.06 / 0.18 / 0.45 / 1.5 m per ring; `WorldView.RunwayLift`, contract section 9 "Airports"). A wheel resting on the height function sinks into the drawn runway by that amount. Options: (a) host adds `RunwayLift` for points inside an airport fence (the fence polygon is not exposed, only runways; a runway rectangle plus a margin would do for the strip), (b) the library makes `ground_height_at` return the drawn surface inside airports. (a) needs no contract change and is the proposal; the visible error elsewhere is the drape lift, which is under the gear height everywhere.

### B3. Spawn from `airports()`

`GameSession` gains `WorldPath` (the seed `world.json`), `AirportIdent`, `RunwayDesignator`. `AircraftController` takes the spawn from `WorldView.Airports` (threshold position in local metres, heading from the runway end, elevation from the height function), not from the node's position in the scene. Missions keep working: Take off = threshold, Landing = final at the existing `LandingAimZ` distance along the reciprocal heading, Custom = air start over the airport.

The menu needs a world / airport / runway page (Mission or Flight setup; `docs/menu-map-spec.md` in SimPlane must be updated in the same change).

### B4. A world scene that replaces `TestWorld`

New `World/SeedWorld.tscn`: `WorldView` (seed world path, `EnableTerrain` follows automatically, `LandmarksEnabled` off for a seed world), the existing `WorldEnvironment` + `Sun` + `SkyController` + `Windsock`, no plane mesh, no cone trees. `TestWorld.tscn` **stays** as the flat test environment for the flight checks (fast, deterministic, no streaming), selected by `GameSession` or a `--flat` flag.

Seed data ships in the bundle (`res://world/two-cities/`, 1.5 MB). Cache dir `user://simworld/`. No tile URL. Attribution strings from `attributions()` go to `CREDITS.md` and the credits page (OpenStreetMap for the coast and water, OurAirports for airports).

### B5. Addon packaging into SimPlane

- Vendor `addons/simworld/` with `bin/macos/` from `tools/build_addon.sh --release` (same shape as `addons/jsbsim/`; the dev override `simplane_world.gdextension` is bench only). For iteration, a symlink from SimPlane's `addons/simworld` to this repo's folder plus `build_addon.sh` is enough; no zip until the first release tag.
- `Nullable`: SimPlane disables it, the addon annotates. Either enable nullable in `Simplane.csproj` (warnings in SimPlane's own code, not errors) or wrap the addon in `#nullable enable` per file (one line each). Proposal: enable it project-wide; the warnings are information, not a blocker.
- Shader globals: SimPlane's `project.godot` has none; `WorldView` adds the four at runtime, so nothing to do, but enabling the editor plugin once writes them permanently and is cleaner.
- Name clashes: addon namespace `SimWorld`, SimPlane `Simplane.*`; `Log` exists on both sides in different namespaces. No clash found.

### B6. The library under Forward+

Untested. Expected to work (the shaders use the Mobile subset, dither fades, `_surfaces` upload, global uniforms are all available in Forward+), but the measured budgets (upload 2 ms, pool trim, GC pacer) were taken on Mobile. Action before anything else: run the bench once with `rendering/renderer/rendering_method = forward_plus` through the fade, LOD and seed self-tests. If SimPlane later goes to iOS, it is SunshineClouds2 that decides whether Mobile is possible, not the library.

## 3. Needed for a playable result, not for the first flight

- **N1. Clock and night.** SimPlane has no time of day. The bench's `TimeOfDay` (NOAA sun in C#) moves into SimPlane as host code (it is host policy by the packaging spec), drives the `Sun` rotation, `WorldView.SetTime(hour, nightFactor)` and `NightLook.Update`. The Rust clock stays unbuilt (freeze).
- **N2. Environment ownership.** Confirmed conflict: `NightLook.ApplyStatic` switches the environment to **depth fog** with density 1 and its own begin / end / height haze (`NightLook.cs:85-95`), while `SkyController` writes **exponential** fog density from the visibility (`fog_density = 2.996 / V`). Applied to the same environment, the last writer wins and the other side's weather or night look is gone. Proposal: `SkyController` stays the only fog writer; the addon gets a `NightLook.ApplyFog` switch (default true, no contract change) so the host can take glow and tone settings without the fog. Glow must be enabled on SimPlane's environment for the night windows to read; today it is off.
- **N3. Camera policy.** Far plane: the addon expects `reach x 1.15` (about 29 km for the default `max_distance_m`); SimPlane's 60 km is wasteful but harmless with reverse Z. Keep SimPlane's values until a depth artefact shows.
- **N4. Shadows.** The `Sun` casts to 400 m; R0 building meshes cast (`TilePool.cs:227`). Measure the shadow pass on the apron with an A320 cockpit view before deciding; the cheap option is shadows off for tiles.
- **N5. Settings.** SimPlane's Graphics page (sky tiers) gains a world Detail slider = `WorldView.SetLodQuality` and a memory budget from the tier (`WorldRenderSettings.MemoryBudgetMb`).
- **N6. Loading.** The loading screen waits for `WorldView.Status.IsOk` and the first `poll_ready` delivery (`ReadyTotal > 0`) before the warm-up frames; a ground start can begin before the far cover is complete. If a stricter rule is wanted, "R0 + R1 loaded around the spawn" is readable from `Cover` (`DescribeCover`), no contract change.
- **N7. Restart and exit.** `Restart` reloads `Main.tscn`: the old `WorldView` must `shutdown()` before the new one configures the same cache folder (contract 11a, the lock). Confirmed: `WorldView._ExitTree` calls `Streamer.Shutdown()` (`WorldView.cs:232-242`). Nothing to build; acceptance 5 guards it.
- **N8. Nav data from one source.** Regenerate `Navigation/Data/*.csv` for the Baltic region from the same OurAirports snapshot the seed world uses, or read `WorldView.Airports` into `NavData` at runtime. Same positions on the ND and under the wheels is the acceptance.
- **N9. Diagnostics.** Both projects carry a benchmark harness. SimPlane's debug HUD gets the `WorldView` counters (loaded tiles, upload ms, pool, shift count) and the FPS logger a world marker; the addon exposes `Timings` / `LastUploads`.

## 4. What the library must prepare (allowed under the freeze: integration needs only)

| # | Item | Contract change | Size |
| --- | --- | --- | --- |
| L1 | Forward+ run of the bench self-tests (B6) | none | **done 2026-10-29**: bench switched to `forward_plus` (Metal, M2 Pro); `--fade-selftest`, `--window-selftest`, `--lod-selftest` headless, a windowed smoke run, `--hole-check=40 --hole-pixels` (4740 frames, 2984 scanned, 0 holes, no `Too many instances` line) and `--seed-selftest` (both cities) all PASS, no shader or script error. Budgets were not measured under Forward+ (the game's first measurement does that) |
| L2 | `NightLook.ApplyFog` switch (N2; confirmed conflict with the weather fog) | none | **done 2026-10-29** |
| L3 | A camera source that follows a switching camera: `CameraSwitchSource` + `IWorldCameraTick` / `IWorldCameraProvider`, `WorldView.ViewCamera` (B1) | none | **done 2026-10-29** |
| L4 | Document the runway surface rule for physics hosts: `ground_height_at + RunwayLift` inside the strip (B2) | doc only | **done 2026-10-29** (contract 9 "Airports", physics hosts) |
| L5 | `tools/build_addon.sh` verified to produce a `bin/macos/` that a fresh project loads (B5) | none | **done 2026-10-29**: an empty Godot 4.7.2 .NET project in SimPlane's configuration (Forward+, `net8.0`, `<Nullable>disable</Nullable>`), the addon folder copied in with `bin/` and its own `simworld.gdextension`, a `Camera3D` + `WorldView` (synthetic city) + a 30-line clock script driving `CameraSwitchSource`, `SetTime` and `OriginShifted`: `dotnet build` 0 warnings, library api 1.11 loaded, 362 tiles held, status ok, clean shutdown (`FRESH CHECK PASS`). The scaffold is reusable: `project.godot`, `FreshCheck.csproj`, `Main.tscn`, `Clock.cs` (kept outside the repo; recreate from this description). **Known Godot 4.7.2 quirk, not ours:** the very first `--headless --import` of a fresh project (no `.godot/` yet) that contains any GDExtension exits with signal 11 / code 134 inside `mvk::SPIRVToMSLConverter` after `loading_editor_layout`, i.e. after the import finished; the `.godot/` folder is complete, the next `--import` exits 0 and the project runs. Reproduced with SimPlane's own JSBSim extension in the same bare project, under Mobile and Forward+, with `reloadable` on and off, debug and release libraries; a project without an extension never crashes. Scripts that run `--import` on a fresh checkout should not treat that first exit code as a failure (run it twice, or check for `.godot/imported`) |
| L6 | ~~Shutdown on exit~~ confirmed present (`WorldView._ExitTree`), nothing to do (N7) | none | done |
| L7 | Addon files compile with `Nullable` disabled: every addon file now starts with `#nullable enable` (B5) | none | **done 2026-10-29** |

Nothing here is a new `api_version`. Items that would be one (a geoid, `set_clock`, the airport fence in `airports()`) are recorded as not needed for the first flight.

## 5. What SimPlane must prepare

| # | Item | Blocks |
| --- | --- | --- |
| S1 | Runtime origin: `WorldGeoReference` over the library's conversions, JSBSim duplicate removed, custom model shift-aware, `OriginShifted` subscribers (B1) | first flight |
| S2 | `GroundHeightAt` wired, runway lift applied inside the strip (B2) | **done 2026-10-29** (section 7a) |
| S3 | `GameSession` world / airport / runway, spawn from `Airports`, menu page, `menu-map-spec.md` (B3) | **partly done 2026-10-29**: a **City** dropdown on the start menu's Flight setup page (`StartMenu.Cities`: Vilnius = seed world at EYVI 01, TestDev = flat; `GameSession.World` / `WorldChosen`; Vilnius is picked when the menu opens, a scene started without the menu stays flat; `menu-map-spec.md` updated; `WeatherPickerChecks` pass through it). Remaining: the airport and runway choice inside a city, the Landing mission geometry on a heading other than north |
| S4 | `World/SeedWorld` (built in code under `TestWorld`), `TestWorld` kept for checks, seed data and credits in the bundle (B4) | **done 2026-10-29** (section 7a) |
| S5 | Vendor the addon (symlink for now), nullable pragmas in the addon, shader globals at runtime (B5) | **done 2026-10-29** |
| S6 | `TimeOfDay` host clock, sun, `SetTime`, glow on the environment (N1, N2) | night |
| S7 | Settings tier mapping, loading rule, restart shutdown (N5, N6, N7) | release |
| S8 | Nav data regenerated for the Baltics (N8) | ND correctness |
| S9 | `WorldChecks` headless scene (section 6) | regression safety |

## 6. Acceptance (a `WorldChecks` scene in SimPlane's check style)

1. Seed world configures headless, `Status.IsOk`, a first tile arrives within 10 s, `Attributions` non-empty.
2. Ground height at the EYVI runway 01 threshold equals the airport's elevation from `Airports` within 0.5 m; the runway profile along the centreline has a slope under 1 %.
3. A320 ground start on that threshold: after 10 s parked, AGL within 0.1 m of the gear height, no watchdog trip (below-ground watchdog uses the same delegate).
4. A 12 km straight flight crosses one origin shift: aircraft lat/lon continuous (no jump above 1 m between ticks), ND symbols continuous, the camera and windsock stay in place relative to the aircraft, `OriginShiftCount` = 1.
5. Restart twice: no `cache_dir in use` error, no tile-root leak (`TileRootCount` = `PoolCreated` after each start), memory back within 10 % of the first run.
6. `TestWorld` flight checks unchanged (flat ground remains the physics regression baseline).
7. Frame cost on the apron in the cockpit view and at 3 000 ft over Vilnius, recorded with SimPlane's benchmark harness, against the current `TestWorld` numbers: this is the first measurement of the library in its real host and the number the next optimisation work starts from.

## 7. Open decisions

- Origin ownership (B1): library frame proposed; the alternative (SimPlane frame, library origin pinned, no re-centre) breaks f32 precision past a few km and is rejected unless a measurement says otherwise.
- Runway surface (B2): host-side lift (proposed) or a contract change later.
- Nullable (B5): project-wide enable (proposed) or per-file pragmas in the addon.
- Clouds versus night fog (N2): `SkyController` is the only fog writer (proposed).
- Nav data (N8): regenerate the CSVs (proposed, keeps the ND independent of the world node) or read `Airports` at runtime.

## 7a. Step 2 result (2026-10-29)

Built in SimPlane (uncommitted there): `addons/simworld` is a symlink to this repo's addon (ignored in SimPlane's git; the release vendors a copy), `WorldData/two-cities/` copied in (1.5 MB; not `world/`: SimPlane's code folder is `World/` and the macOS file system merges the two, which would break a case-sensitive export), `CREDITS.md` rows for SimWorld, OpenStreetMap and OurAirports. `World/WorldKind.cs` (`WorldKind` Flat / Seed, `WorldSelect.Effective()`: `--world=seed|flat` after `--`, else `GameSession.World`), `World/IGroundSurface.cs` (group `ground_surface`, `HeightAt`), `World/SeedWorld.cs` (a `WorldView` built in code over `WorldData/two-cities/world.json`, origin = the EYVI runway 01 end from OurAirports, `CameraSwitchSource(() => GetViewport().GetCamera3D())`, `AutoStart` off and `Start()` after the source is set, `LandmarksEnabled` off; `HeightAt` = `ground_height_at` + `RunwayLift.X` inside a runway pavement from `airports()`; spawn = threshold + 60 m along the runway heading; an origin shift logs a warning and rebuilds the strips; `--world-shot=<png>` saves the viewport 180 frames after the first tile; a log line every 5 s for the first minute). `TestWorld` keeps every node path (`TestWorld/WorldEnvironment`, `SkyController`, `Windsock`) in both worlds: in the seed world it frees the flat `Ground` plane, builds no runway or trees, adds `SeedWorld`, lifts the windsock onto the terrain and writes the aircraft's `Position` / `StartHeadingDeg` before `AircraftController._Ready`. `AircraftController`: `Model.GroundHeightAt` from the `ground_surface` group right after the model is created (before the first reset), and SimPlane's own rebase is off when the world does not move with the origin (`TestWorld.MovesWithOrigin` false), so the aircraft node sits at true local metres. `GameSession.World`.

Measured: headless and windowed (Metal, Forward+) runs of `Main.tscn -- --world=seed`: 28 airports loaded in 287 ms, spawn (22.5, 330.8, -55.6) heading 22 at EYVI 01, the A320 (JSBSim) parked with a steady CG height of 8.4 ft over 10 s, 497 / 605 tiles held, no error, the screenshot shows the aircraft on the drawn runway with markings, the windsock on the grass and Vilnius on the horizon; `WindsockChecks` and `SkyChecks` (flat world) unchanged. The material prewarm is skipped in this host because no camera exists when the view starts (the cameras are later siblings); the first fade therefore compiles its shaders. Not yet: the telemetry lat/lon still come from the 47 N 8 E origin (step 3), the ND shows the wrong place, Landing mission geometry assumes the flat runway heading north, `set_view` starts one frame late.

**Relief lowered (same day, host side):** SimPlane passes `terrain.amplitude_m` 120 and `seed.airport.max_slope` 0.003 as profile overrides; EYVI is now at 88.3 m with the runway level within 1.2 m over 2 507 m (slope 0.046 %, the "Seed world runway profile" log line). `terrain.height_offset_m` was not used: it would lift the coast with the land, and the water rule holds the sea at 0, so Palanga would get a 110 m cliff.

**New finding, needs a decision:** the procedural relief put EYVI at 330.8 m with the default amplitude (88 m at 120) while the real field elevation is 198 m (648 ft); the altimeter read 1094 ft on the ground (290 ft now). The height function treats `elevation_ft` as informational (spec-world-seed 5.1 step 9). Options: (a) the seed height function passes through the airport elevation at every strip (the runway profile anchored to `elevation_ft` instead of the field's own heights, blended over the shoulder), which keeps real altimeter settings and approach plates usable; (b) lower the relief amplitude for the Baltics so airports land within tens of metres of their elevation; (c) accept. (a) is the truthful one and a contract-visible output change (golden hashes, api minor); decide before step 5.

## 7b. First play session: findings and fixes (2026-10-29)

The author flew the A320 out of EYVI in the seed world and reported artifacts, the map dropping at one moment, crash errors in the report folder, and everything blinking when the chase camera is moved. Root causes, from `user://reports`, `user://recorder`, the session logs and scripted reproductions:

| Symptom | Cause | Evidence | Fix |
| --- | --- | --- | --- |
| A multi-second freeze shortly after the flight started ("the map dropped") | The material prewarm never ran in SimPlane: the view starts inside the world node's `_Ready`, before the cameras are in the tree, and `WorldView` started the prewarm only with a camera at `Start()`. The first tile draw and the first dither fade then compiled every world shader and pipeline on Metal Forward+ inside one frame | report `20261029-083404`: `frame_max_ms` 4730, `cpu_process_max_ms` 4719, 50 frames in 5.9 s between the first two "Seed world" log lines; 7 s between frame 0 and 1 in the scripted runs | Addon: `StartPrewarmIfReady` at `Start()` and on every frame until a camera exists. Measured afterwards: worst frame 146 ms inside the 24 prewarm frames, nothing above 17 ms later |
| **Everything blinks once the flight is more than 5 km from the airport** (the author's recording at 10 300 ft: the ground absent in most frames, a 5 km wedge of it flashing in for a frame) | The library re-centred its floating origin at `origin.recenter_distance_m` (5 km) and shifted every tile; SimPlane's aircraft and cameras do not follow the shift yet (step 3), the chase camera is rebuilt from the aircraft each frame, so the camera was beyond 5 km again on the next frame and the origin moved **every frame**: the world jumped 5 km back and forth at 120 Hz | session log 08:52 UTC: `SimWorld origin shifted` on frames 5215, 5216, 5217, ...; scripted 8 km air start with the default: 184 shifts in 5 s, 19 tiles left; with the override: 0 shifts, 492 tiles | SimPlane: profile override `origin.recenter_distance_m` 50 000 (the schema maximum) until step 3 makes the frame shift-aware; a flight to Kaunas (95 km) still needs step 3. Addon: one `PushError` when the origin shifts on three consecutive frames, naming the cause, so no host hunts this again |
| Everything blinks when the chase camera is moved (near the airport) | The chase camera swings on a 60 m arm; `CameraSwitchSource` reported that swing as the velocity, and the library looks 20 s ahead along it (clamped per band), so every swing moved the look-ahead points of all bands: tiles streamed in and out and faded everywhere | scripted right-drag (`--world-orbit`) with the camera's velocity: 136 m/s, +812 / -475 tiles in 5 s, parked; with the aircraft's velocity: +0 / -0 | Addon: a velocity provider on `CameraSwitchSource`; SimPlane passes the aircraft node's velocity (`--world-camvel` restores the old behaviour for comparison) |
| The streamed set shrank and grew while parked | The library's adaptive LOD (`lod.adaptive`, frame budget 33 ms, scale up to 3x, 2 s smoothing) reacting to process-frame spikes; at that moment the whole headless check suite was running on the same machine | stress run with 12 CPU hogs: 517 -> 429 -> 554 tiles with the camera still; the user's session had `one_percent_low_fps` 13 | None needed: it is the design under load. Do not run `tools/run_checks.sh` while playing. Whether SimPlane wants a gentler `adaptive.max_scale` is a tuning question for the first measurement |
| "Crash" record in `user://recorder` | `rec-20261029-083051-crash.csv` is the forced `InvalidOperationException` of `BugReportChecks` (the first scene of the suite started at 08:30:00 UTC, 29 s long) writing its recorder dump; headless checks share `user://` with the game | `qa/runs/20261029T083000Z/logs/BugReportChecks.log` line 34 | None (test artefact) |
| Report `watchdog-stepsperframe` | A frame over 83 ms (6 physics steps) at sim 8.95 s of a scripted screenshot run: the first dither fade compiling shaders, same cause as the freeze | `20261029-082538-watchdog-stepsperframe` | Covered by the prewarm fix |
| Report `watchdog-oscillation` | A sustained pitch oscillation (period 3.5 s, 3.6 deg/s, not growing) while hand-flying the initial climb at 182 kt, 19 deg pitch, flaps 25, full thrust. SimPlane's watchdog files a Major report for it automatically in a windowed debug build | report `20261029-083404`; not related to the world | None here; a flight-model / watchdog-threshold question for SimPlane |
| "Artifacts" | Not identified from the one screenshot (it shows the faceted terrain and the countryside patchwork, both by design at this relief). If they were the blinking, the velocity fix covers them | | Ask for a screenshot if they persist |

Diagnostics kept in `World/SeedWorld.cs`: the 5 s "Seed world" log line now carries per-layer tile counts, deliveries and unloads since the last line, the origin shift count, `tau_eff` / quality / view / camera / velocity source, the worst frame time and the aircraft AGL; `--world-orbit` injects a chase-camera drag from 8 to 20 s; `--world-camvel` feeds the camera's own velocity; `--world-teleport=<km>` air-starts that far along the runway heading at 3 000 ft and 250 kt at 10 s; `--world-recenter=<m>` sets the re-centre distance (5000 reproduces the per-frame shift).

**Consequence for the order of work:** step 3 (the frame, S1) is no longer "after the first flight"; it is the next thing, because every flight beyond 50 km of EYVI hits the same wall. Its design, verified against both code bases, is SimPlane's `docs/world-frame-spec.md` (2026-10-29): one geodetic truth, model space = the library's ENU frame at the configured origin (continuous, never shifted), render space = model space minus a render offset whose horizontal part is the library's `TotalShift` and whose vertical part is SimPlane's existing 512 m rule; the ENU formulas ported in double from `geo.rs`; the JSBSim bridge switched from geocentric to geodetic latitude (a 21 km error at 54.6 N otherwise). No library change.

## 8. Order of work

1. L1, L5, L7 (library, one day): the addon loads in a fresh Forward+ project.
2. S5, S4 with a hard-coded EYVI spawn, S2 (SimPlane): the A320 sits on a Vilnius runway in the seed world, daylight only.
3. S1 (the frame): a flight to Kaunas and back with origin shifts.
4. S3, S9: the airport choice in the menu and the check scene.
5. S6 to S8: night, settings, nav data.

After step 2 the first measurement (acceptance 7) is taken, before any further library work.
