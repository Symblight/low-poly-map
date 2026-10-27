# Spec: SimWorld as a Reusable Library

2026-10-26 · Aleksei

## Purpose

This spec turns the streamed-city spike into a reusable library, the way JSBSim is one for flight dynamics. The library streams OSM tiles and renders buildings, ground, night windows and light sprites. This project (`low-poly-map/`) becomes its test bench and demo. The SimPlane game uses the same library unchanged.

The spec has three parts:

- An audit of the code as of 2026-10-26.
- The target structure.
- The changes, in phases, with acceptance criteria.

It does not change the rendering or streaming design in [spike-streamed-osm-city.md](spike-streamed-osm-city.md) or [spec-night-rendering.md](spec-night-rendering.md). It only changes how that design is packaged, configured, tested and versioned.

## What "like JSBSim" means here

JSBSim is reusable for four reasons. Each one maps to a requirement for us.

| JSBSim property | What it means | Our requirement |
|---|---|---|
| Engine-free core | The C++ core has no graphics or host dependency; FlightGear, Unreal and Python all embed the same core | A pure-Rust core crate with no `godot` dependency; Godot is one binding |
| Data-driven models | Aircraft are XML files, not code; changing a model needs no rebuild | Bands, light profiles, palettes, road widths and budgets live in a config file with compiled-in defaults |
| Standalone runner and scripts | `JSBSim.exe script.xml` runs a scripted flight headless and logs outputs; this is how JSBSim is tested | A headless CLI that flies a scripted path through the core, checks the guarantees and writes stats and meshes |
| Stable, versioned interface | Hosts talk through properties and a small API that changes slowly | A versioned `WorldStreamer` contract with one source document, additive changes and an `api_version` |

There is one difference from JSBSim: rendering is a main feature of this library, not something the host provides. The library therefore ships two parts that version together:

- **The Rust world core**, which streams tiles and generates meshes.
- **A Godot rendering addon**, which holds the shaders, the tile pool, the visibility rule and the night look.

## Audit (state on 2026-10-26)

### What is already library-shaped

- **The Godot coupling is thin.** Only `rust/src/lib.rs` and `rust/src/streamer.rs` import `godot`. `geo`, `rings`, `scheduler`, `fetch`, `mvt`, `mesher`, `ground`, `lights`, `synthetic` and `worker` are plain Rust. Splitting out a core crate is mostly moving files.
- **Workers produce plain structs.** Conversion to Packed arrays happens only in `poll_ready`, so the core already has no engine types.
- **C# goes through one facade.** `WorldStreamerFacade.cs` is the only place that touches method-name strings. It also probes for optional methods (`HasPollFades`, `HasGroundHeight`, `HasDebugTiles`), which already gives forward compatibility.
- **There is a Godot simulator in Rust tests.** `scheduler::tests::GodotSim` mirrors the C# visibility rule, so the guarantees can be tested without Godot.
- **The synthetic city** (empty `tile_url`) gives an offline, deterministic data source for tests.
- **The tile build is scripted and reproducible:** `data/build_tiles.sh` uses planetiler 0.10.2 in Docker.

### Problems found

| # | Finding | Where | Impact on reuse |
|---|---|---|---|
| A1 | **No version control.** Nothing under `lowpolymap/` is a git repository. | whole tree | There is no history, no tags and no way to pin a library version in the game. This blocks everything else. |
| A2 | *Resolved.* An earlier audit found 6 failing `scheduler::tests`. Re-checked on 2026-10-26: `cargo test` gives **66 passed, 0 failed, 3 ignored**. | `rust/src/scheduler.rs` | None now. The baseline is green. |
| A3 | *Resolved.* `poll_fades` is exported (`#[func] poll_fades` in `streamer.rs`), and the bridge turns dithered fades on through `HasPollFades`. The underlying risk remains: when a method is missing, the facade's probing falls back silently, and only `api_version` (A5) fixes that. | `rust/src/streamer.rs` | None now; see A5. |
| A4 | **The contract is written twice.** The full API, dictionary shapes, encodings and visibility rules are copied into `.claude/agents/rust-world-streamer.md` and `godot-csharp-integrator.md`, and the design doc holds a third partial copy. | `.claude/agents/`, `docs/` | The copies will drift. A host game has no single document to read. |
| A5 | **There is no `api_version`.** Only `debug_stats()["version"]` exists, and it is a display string with a build marker. | `streamer.rs`, `lib.rs` | A host can't check compatibility, so feature probing by method name is the only option. |
| A6 | **Tuning values are compiled-in constants.** Examples: `R0/R1/R2_PARAMS`, `MAX_FETCH_PER_RING`, `CLUSTER_ON/OFF_AGL_M`, `FADE_CAP`, `RECENTER_DISTANCE_M`, the 300 MB cache cap, and the road widths and light colors in `ground.rs` and `lights.rs`. | `mesher.rs`, `scheduler.rs`, `fetch.rs`, `lights.rs`, `ground.rs` | The game can't retune a city or an altitude profile without forking the crate. |
| A7 | **`configure` takes only positional arguments** `(lat, lon, tile_url, cache_dir)`. The planned `configure_imagery`, `set_clock`, `daylight` and `attributions` are not built yet. | `streamer.rs` | Every new option would break the signature or add another method. |
| A8 | **The camera input is Godot-local only.** `update_camera(local_pos, velocity, alt_agl)` takes local coordinates, and there is no public geodetic↔local conversion. | `streamer.rs` | A JSBSim-driven aircraft has a geodetic position (lat, lon, alt MSL, NED velocity). Every host would have to rebuild `Frame` and `LocalSpace` itself, and the floating-origin shift has to reach the game's own nodes too. |
| A9 | **There is no origin-shift notification.** `take_origin_shift()` is pull-only and consumed once, so only the bridge sees it. | `streamer.rs`, `StreamerBridge.cs` | Aircraft, particles and other game objects also need to be shifted in the same frame. |
| A10 | **The spike host and the library code are mixed in `scripts/`.** The library-grade parts are `TilePool`, `TileCover`, `TileKey`, `WorldStreamerFacade` and most of `StreamerBridge`. They sit next to spike-only parts: `FreeFlyCamera`, `DebugOverlay`, `CoverSelfTest`, command-line arguments such as `--tile-url` and `--light-alpha`, and a default origin in Vilnius. | `low-poly-map/scripts/` | The game can't take "the renderer" without the spike. |
| A11 | **Shader globals are per-material parameters.** `night_factor` is set on each material by `TilePool.SetNightFactor`, and `hour` doesn't exist yet. Asset paths are hard-coded `res://shaders/...` and `res://textures/...`. | `TilePool.cs`, shaders | The library's look can't be driven by the host's time-of-day system, and the paths break when the files move into an addon. |
| A12 | **`NightRenderController` mixes library look and host policy.** Glow threshold and fog are library look. Exposure, view mode and sun placement are host and camera policy. All of it lives on the scene's `WorldEnvironment`. | `NightRenderController.cs` | The game has its own environment and cameras and would have to copy or fight this node. |
| A13 | **The `.gdextension` only lists macOS.** It loads from `res://../rust/target/...`, which is outside the project, so it works in the editor only. The project targets D3D12 on Windows, but there is no Windows, Linux or iOS entry. | `simplane_world.gdextension` | The library can't be exported or dropped into another project. |
| A14 | **`CLAUDE.md` is stale.** It says "Milestone 1 done, rings, fetching, meshing not built". The code is at M5 and partway through M6. | `CLAUDE.md` | Agents and readers start from wrong assumptions. |
| A15 | **Diagnostics are Godot-only.** Logging uses `godot_print!` and `godot_warn!`, and profiling is switched on with the `SIMPLANE_PROFILE` environment variable. | `streamer.rs`, `worker.rs` | A headless runner or another host can't capture logs, and an environment variable is a poor switch for an embedded library. |
| A16 | **Determinism is claimed but not tested.** The doc requires "same tile → same mesh and lit windows", but no test pins mesh hashes. | tests | Regressions in meshing would go unnoticed. That matters more once two hosts depend on the output. |
| A17 | **The floating origin lives in the binding.** `RECENTER_DISTANCE_M`, the pending shift, `REDELIVERIES_PER_FRAME`, the warning rate limit (`WARNINGS_PER_SECOND`) and the profiling switch are all in `streamer.rs`, not in a plain-Rust module. | `rust/src/streamer.rs` | "Move everything except `streamer.rs`" would leave the re-center logic in the Godot layer, where the CLI and other hosts can't reach it. |
| A18 | **Some constants are structural, not tuning.** `RING_COUNT`, `ROOT_ZOOM` and `LEAF_ZOOM` (`rings.rs`) size arrays and must match the tileset's zoom range and layers (`data/schema`). | `rings.rs`, `mvt.rs` | Putting them in a profile next to tuning values invites profiles that don't match the `.pmtiles`. |
| A19 | **Docs and agent files sit inside the test bench.** `docs/`, `.claude/agents/` and `CLAUDE.md` are under `low-poly-map/`, not at the future repo root. | `low-poly-map/` | Once the library repo exists, the contract and the agent instructions would be hidden inside the demo host. |
| A20 | **No daylight control in the test bench.** Until 2026-10-26 the only control was `--night=<0..1>`. The sun stayed fixed, and there was no time of day. *Interim fix:* `scripts/TimeOfDay.cs` and `scripts/DaylightMenu.cs` (see "Test bench: daylight menu"). | `low-poly-map/scripts/` | Day/night look, LOD fades and glow couldn't be checked at real sun angles or through twilight. |

`dotnet build` succeeds with 0 errors.

## Target structure

```
simworld/                           # the LIBRARY repo (today's lowpolymap/, restructured); tagged per release
                                    # the SimPlane game is a separate repo that consumes release zips
  rust/                             # Cargo workspace
    simworld-core/                  # pure Rust, no godot: geo, rings, scheduler, fetch, mvt,
                                    #   mesher, ground, lights, daylight, synthetic, worker, config
    simworld-godot/                 # gdext binding: WorldStreamer node only (thin), cdylib + staticlib
    simworld-cli/                   # headless runner: fly scripts, bake cache, dump tiles, bench
  addons/simworld/                  # the Godot rendering addon (copied or symlinked into hosts)
    plugin.cfg
    simworld.gdextension            # paths into addons/simworld/bin/
    bin/                            # built libraries per platform (macOS, Windows, Linux, iOS xcframework)
    runtime/                        # C#: WorldStreamerFacade, WorldView (ex StreamerBridge core),
                                    #   TilePool, TileCover, TileKey, WorldRenderSettings, NightLook
    shaders/                        # building, ground, light_sprite, lod_dither
    textures/                       # light_dot_32.png
    profiles/                       # default world profile (JSON) + its JSON Schema, render settings (.tres)
  low-poly-map/                     # test bench + demo host (this project)
    scenes/CitySpike.tscn           # free-fly demo
    scenes/tests/                   # contract replay, scripted flight bench, golden screenshots
    scripts/                        # FreeFlyCamera, DebugOverlay, TimeOfDay, DaylightMenu, test drivers (host-only)
  docs/                             # moved up from low-poly-map/docs/
    contract.md                     # THE contract: single source (moved out of the agent files)
  data/                             # tile build (unchanged)
  .claude/agents/                   # moved up from low-poly-map/.claude/agents/
  CLAUDE.md                         # repo-level; low-poly-map/CLAUDE.md shrinks to test-bench notes
  CHANGELOG.md
  LICENSE, THIRD_PARTY_NOTICES.md   # library license + collected Rust/C# dependency licenses
```

The library gets its own repository, `simworld`, made from today's `lowpolymap/` folder. The library, its test bench (`low-poly-map/`) and the tile build (`data/`) version together, because the engine-level tests need a Godot project in the same repo. The SimPlane game is a separate repository and only consumes tagged releases (see Versioning and release). `addons/simworld/` lives at the repo root rather than inside `low-poly-map/`, so it can be zipped and vendored into the game. The test bench links it with a symlink, or with a copy step in the build script.

**Names.** The crates are `simworld-*`, and the Godot class stays `WorldStreamer` so existing code keeps working. Before the first tagged release, decide whether to rename the class to `SimWorldStreamer` to avoid collisions in the game (see Open decisions).

### Layer responsibilities

| Layer | Owns | Must not |
|---|---|---|
| `simworld-core` | Tile math, LOD cover, fetching and cache, decoding, meshing, lights, daylight, floating origin math, config parsing, all guarantees | Depend on `godot`, read environment variables, or log anywhere except through a `Log` trait |
| `simworld-godot` | Converting to Packed arrays, `#[func]` methods, signals, mapping the `Log` trait to `godot_print!` | Contain logic beyond type conversion and calls into the core |
| `addons/simworld` | Uploading to pooled nodes, the visibility rule, materials and shaders, shader globals, the night look (bloom threshold, fog curve) | Own cameras, input, UI, exposure policy, the sun node, or a default origin |
| Host (spike or game) | Camera or aircraft, input, UI, time of day (when to call `set_clock`), the environment node, exposure, attribution UI placement | Re-implement visibility, shader globals or coordinate math |

## Changes

### 1. Repository and hygiene (fixes A1, A14, A19)

- Run `git init` at `lowpolymap/`. Use one `.gitignore` covering `rust/target/`, `low-poly-map/.godot/`, `data/out/`, `data/sources/` and `addons/simworld/bin/` (built artifacts are attached to releases, not committed).
- Keep `cargo test` green. It was green on 2026-10-26 (66 passed), so every later step starts from a passing baseline. A2 and A3 are already resolved.
- Move `docs/` and `.claude/agents/` from `low-poly-map/` to the repo root, and add a repo-level `CLAUDE.md` for the new layout and the real milestone state. `low-poly-map/CLAUDE.md` keeps only test-bench notes.
- Add `CHANGELOG.md`. Its first entry is `0.5.0-spike`, matching the first tag: the current API as built.
- Add CI for every push, not only releases: `cargo test`, `cargo clippy`, `dotnet build` and the headless smoke test on macOS.

### 2. Split the Rust workspace (fixes A15, A17, prepares A6)

- Turn `rust/` into a workspace with `simworld-core`, `simworld-godot` and `simworld-cli`. Move every module except `streamer.rs` and the `#[gdextension]` entry into the core unchanged.
- Then move the engine-free logic out of `streamer.rs` (A17) into the core `World`: re-center decision and pending shift, re-delivery pacing, and warning rate limiting.
- Renaming the crate changes the library file (`libsimplane_world.*` becomes `libsimworld_godot.*`). Update the `.gdextension` entries in the same commit. Keep `entry_symbol = "gdext_rust_init"`.
- Add a `Log` trait (`info`, `warn`, `error`) and a `Diagnostics` struct (profiling on/off, warning rate limit) to the core. Pass both in at construction. Remove `std::env::var("SIMPLANE_PROFILE")` from library code; the binding and the CLI map their own switches onto `Diagnostics`.
- Wrap the scheduler and worker in a core `World` type that holds the frame, the local space, the scheduler and the worker. Its methods mirror the contract in plain Rust: `update_camera`, `pop_ready`, `drain_fades`, `drain_unload`, `take_origin_shift` and `stats`. `streamer.rs` becomes a type-conversion shell over `World`.
- Keep `crate-type = ["cdylib", "staticlib"]` on `simworld-godot` only. The core stays an `rlib`, and the iOS rules (rustls, no desktop-only crates) apply to the core too. The disk cache uses `rusqlite` with the `bundled` feature, which compiles the SQLite amalgamation with `cc`, so every target needs a C toolchain (for iOS: Xcode and the iPhoneOS SDK). That iOS build is not yet verified (see spec-tile-cache.md, acceptance 5).

### 3. Data-driven configuration (fixes A6, A7)

- Define a world profile in JSON (`addons/simworld/profiles/default.json`), parsed with `serde_json` in the core. JSON is readable by Godot without extra code, and it maps one-to-one onto the `profile_overrides` Dictionary. JSON has no comments, so a JSON Schema (`profiles/world-profile.schema.json`) documents every key with units and ranges and gives editor autocompletion. The CLI's `validate-profile` checks files against it. The profile holds every value audited in A6:
  - band distances, zooms and hysteresis
  - Douglas–Peucker tolerances and minimum areas
  - fetch and mesh concurrency
  - cluster altitudes and cell size
  - fade cap
  - re-center distance
  - cache caps
  - light profiles: kinds → profile, colors, lit shares
  - road classes and widths, area classes and lifts
- The compiled-in defaults equal today's constants exactly, so behavior doesn't change. The core parses and validates the profile, and rejects out-of-range values with a clear error.
- **The profile carries `profile_version`** (an integer, bumped when keys are added).
  - When the version is the library's own or older, an unknown key is an error, because it is a typo.
  - When the version is newer than the library knows, unknown keys are a warning and are ignored. A game profile written for v0.7 still loads on v0.6, which keeps "additive changes" true for profiles as well as the API.
- **Structural values stay out of the profile** (A18): ring count, root zoom and leaf zoom. They are part of the tileset contract. At `configure_world` the core checks the opened `.pmtiles` header (min/max zoom) and the required layers (`buildings`, `landuse`, `roads`) and fails with a clear error on a mismatch.
- Add `configure_world(options: Dictionary) -> Dictionary` (additive). Keys:
  - `origin_lat`, `origin_lon`
  - `tile_url`, `cache_dir`
  - `profile_path` (optional)
  - `profile_overrides` (optional, nested dictionary)
  - `diagnostics`

  The result is `{ ok, error, api_version, attributions }`. The old `configure(...)` stays as a wrapper that calls it.
- Light-profile values that the shaders also need (colors, lit shares by hour) reach the GPU from the same profile. The addon reads them with `world_profile()` and writes them into shader globals, so Rust and the shaders can't disagree.

### 4. Host-agnostic API (fixes A5, A8, A9)

All additions follow the "additive where possible" rule from the design doc.

| Addition | Purpose |
|---|---|
| `api_version() -> Vector2i(major, minor)` (also in `debug_stats` as `api_major`, `api_minor`) | Contract version. A minor bump is additive; a major bump means breaking. The facade checks it once and logs what is missing, replacing silent method probing as the primary check. Two fields avoid an encoding such as `major*100+minor`. |
| `update_camera_geodetic(lat, lon, alt_msl_m, vel_ned: Vector3)` | Direct input from JSBSim (or any flight model) with no host-side coordinate math. Internally it converts to local coordinates and calls the same path as `update_camera`. The input must be pinned down in the contract: (a) `lat`/`lon` are WGS84 geodetic. (b) Altitude is above MSL; JSBSim's geodetic altitude above the ellipsoid must be converted by the host, or the library adds an `alt_wgs84_m` variant. (c) AGL = altitude − `ground_height_at` (0 until a DEM exists). (d) NED maps to Godot as x = E, y = −D, z = −N. |
| `geodetic_to_local(lat, lon, alt_msl) -> Vector3` / `local_to_geodetic(Vector3) -> Vector3(lat, lon, alt)` | Lets the host place airports, aircraft and markers in the current floating-origin frame |
| signal `origin_shifted(shift: Vector3)` | Emitted on the main thread during `update_camera` when a re-center happens. Any game node can subscribe; `take_origin_shift()` stays for the bridge's same-frame ordering. |
| `world_profile() -> Dictionary` | The resolved profile, for shader globals and debug UI |
| `attributions() -> PackedStringArray` | Already planned. It moves forward, because a reusable library must state its own licensing needs (© OpenStreetMap contributors). |
| `shutdown()` | Stops the workers, then waits (bounded, 1 s) for the disk cache to flush pending writes and close, and releases the cache-folder lock. This is needed when a game unloads a level without freeing the node. Today `Worker::drop` calls `shutdown_background()`, which doesn't wait or flush. Dropping the node stays a background stop with no wait. |
| signal `status_changed(status: Dictionary)` + `status() -> Dictionary` | `{ state: "ok" \| "degraded" \| "offline" \| "error", message, failed_fetches, last_error }`. A game needs to show "map data unavailable" or switch to a fallback. Today this information exists only in `debug_stats` and warnings. Disk-cache trouble also feeds it: a corrupt cache that was recreated is only logged, but a cache disabled for the session after repeated write failures sets `degraded` (see [spec-tile-cache.md](spec-tile-cache.md)). |

`set_clock`, `daylight`, `configure_imagery` and `poll_fades` keep their planned shapes. They are listed in `docs/contract.md` with the `api_version` that introduces them. `poll_fades` already exists and goes in as `api_version` 0.5.

**Lifecycle rules** (go in `contract.md`):

- **Several instances in one process are allowed.** Example: a main view plus a map viewport, each with its own `World`. They share nothing except, optionally, the tokio runtime. Two instances must not use the same `cache_dir`. `configure_world` takes an exclusive lock on `cache_dir/.simworld.lock` **synchronously, before** any asynchronous archive or cache open, and returns an error ("cache_dir in use") if another instance or process holds it. The lock is per folder, not per tile source, and a world with no disk cache (empty `cache_dir`, or the synthetic city) takes none. It uses `std::fs::File::try_lock` (stable since Rust 1.89, MSRV is 1.94; verify on all targets), so it needs no new dependency and works with whichever cache implementation sits behind it, including the SQLite cache in [spec-tile-cache.md](spec-tile-cache.md), which keeps this lock and uses its own `locking_mode=EXCLUSIVE` only as a second safeguard.
- **Teleport:** calling `configure_world` again with a new origin drops all tiles (every loaded key appears in `poll_unload`), resets the origin, and keeps the cache. The facade treats it as a fresh start. The instance must not lock itself out: the old world's cache is flushed and closed (bounded wait) before the new one opens, or the open handle is reused when `tile_url` and `cache_dir` are unchanged. The folder lock stays held across a teleport with the same `cache_dir`; it is released and retaken if `cache_dir` changes.
- **The cache key includes** the tileset fingerprint (already present) and `CACHE_SCHEMA_VERSION`. Raw tile bytes are cached; meshes are not. So a profile change never serves stale geometry. Keep it that way, or add the profile hash to the key if meshes are ever cached.

### 5. Single contract document (fixes A4)

- Create `docs/contract.md` as the only full statement of:
  - methods and call order
  - the ready-tile dictionary
  - the visibility rule and the guarantees
  - the fade rule
  - vertex COLOR and ground COLOR encodings
  - lights
  - coordinates
- Each item carries the `api_version` that introduced it.
- Both agent files replace their copied contract with a short pointer: "The contract is `docs/contract.md`; read it before changing either side; any change updates that file, `api_version` and `CHANGELOG.md` in the same commit." This replaces the CLAUDE.md rule that the contract must be edited identically in two places.
- The design doc's "Library API" table links to `contract.md` instead of repeating it.

### 6. Godot rendering addon (fixes A10, A11, A12, A13)

- **Move the library code into `addons/simworld/runtime/`** under its own namespace (`SimWorld`). This covers `WorldStreamerFacade`, `TilePool`, `TileCover`, `TileKey` and the core of `StreamerBridge`. The bridge's core is renamed `WorldView`: it creates the streamer, runs the per-frame calls, uploads, applies fades and shifts origins.
- **Keep spike-only parts in `low-poly-map/scripts/`:** `FreeFlyCamera`, `DebugOverlay`, `CoverSelfTest`, `TimeOfDay`, `DaylightMenu`, command-line parsing and the Vilnius default.
- **`WorldView` takes a camera-pose source, not a `FreeFlyCamera`.** Use a small interface (`IWorldCameraSource` with position, velocity and AGL), or a `Node3D` plus a velocity callback. The game can then feed it from the aircraft.
- **Move the tuning exports into resources.** Upload budget, tiles per frame, prewarm count and all material tunables become a `WorldRenderSettings` resource (`.tres`), not exports scattered across nodes.
- **Make shader globals real global uniforms.** These are `simworld_night_factor`, `simworld_hour`, and the profile colors and lit shares. The shaders read them as `global uniform`. The host's time-of-day system sets them through `WorldView.SetTime(...)`, or straight from `daylight()`. They are registered in two places:
  - **Editor:** the addon's `EditorPlugin` (`plugin.cfg` → `SimWorldPlugin.cs`) adds them to Project Settings (`shader_globals/simworld_*`) when the plugin is enabled. Without this, a shader that uses `global uniform` fails to compile in the editor.
  - **Runtime:** `WorldView` calls `RenderingServer.GlobalShaderParameterAdd` for any global that is missing. This covers hosts that never enabled the plugin.
- **Split `NightRenderController`:**
  - `NightLook` (addon) derives the glow threshold from emission, the altitude-driven glow intensity and the fog curve, and applies them to an `Environment` the host passes in.
  - Exposure, view mode, tone-map choice and the sun or moon node stay in the host. The spike keeps its current controller, built on `NightLook`.
- **Load assets relative to the addon,** using paths derived from the script's resource path or exported resource references instead of `res://shaders/...`.
- **The `.gdextension` points into `addons/simworld/bin/`.** It gets entries for macOS (universal), Windows x86_64, Linux x86_64 and iOS (xcframework). A `rust/xtask` or script (`build_addon.sh`) builds the platforms and copies the libraries. The macOS universal dylib is built for `aarch64-apple-darwin` and `x86_64-apple-darwin` and merged with `lipo`. The editor-only `res://../rust/target/debug` path stays available as a dev override in the test bench.
- **Move scripts together with their `.uid` files.** `CitySpike.tscn` references scripts by path and uid. Moving `.cs` files without their `.uid` files breaks the scene. The namespace changes from `LowPolyMap` to `SimWorld` for the moved files only.
- **The C# runtime ships as source.** It is compiled into the host's assembly, so the host must be a Godot .NET project (the game is). Document the minimum Godot .NET version and the .NET SDK version (today 4.7 and net9.0).
- **iOS risk:** the addon's runtime is C#, and Godot .NET on iOS is still experimental (NativeAOT). Being iOS-ready therefore depends on Godot's C# iOS export as well as on the Rust static library. Before P7, run a minimal Godot .NET iOS export of the addon. If that is blocked, the fallback is to port the runtime to GDScript, or to move the visibility and pooling logic into the gdext binding.

### 7. Headless runner and test strategy (fixes A2, A16)

The test bench is the project's main role after the split. Testing happens at four levels, from fast to slow.

| Level | Where | What it checks | Runs |
|---|---|---|---|
| Unit | `cargo test -p simworld-core` | Tile math, cover, meshing, ground, lights, config validation, daylight | Every change |
| Scenario | `simworld-cli fly scripts/*.json`, also run from `cargo test` | Scripted flights through the synthetic city and a small committed fixture `.pmtiles`: climb to 10 km, 250 m/s at 300 m AGL, hover and circle, re-center crossings. The checks are the sibling-swap and fade guarantees (through `GodotSim`), no holes, budgets on queue depth, and a determinism hash per tile (positions, indices, colors → u64) compared with committed golden values. | Every change |
| Contract replay | `low-poly-map/scenes/tests/ContractReplay.tscn`, headless | The CLI records a JSON trace of `poll_ready`, `poll_fades` and `poll_unload` events. The C# `TileCover` replays it, and the visible set must equal `GodotSim`'s visible set each frame. This tests the Rust simulator and the real C# rule against the same input. It replaces the hand-written `CoverSelfTest` as the gate. | Every contract change |
| Engine | `low-poly-map` headless and windowed | (a) smoke test: the library loads, `api_version` matches, a frame runs. (b) Flight bench: a scripted path at fixed `set_clock` writes frame time, upload time, draw calls, triangles and RAM to JSON and checks the spike budgets. (c) Golden screenshots: day and night at fixed poses on the synthetic city, compared with a tolerance. | Before each release tag |

`simworld-cli` commands:

- `fly <script.json>` runs a scripted flight and reports.
- `bake --bbox ... --zooms ...` pre-fills a cache directory (this is the planned "pre-download the test area"). It writes the same on-disk format as the runtime cache, the one-file SQLite format in [spec-tile-cache.md](spec-tile-cache.md) (built, T1-T3), so a baked cache can be shipped or copied. It takes the same folder lock, so it cannot run against a cache folder a game is using.
- `tile <z/x/y> --out tile.obj` dumps one tile's meshes for inspection.
- `validate-profile <file>` checks a world profile.

A fixture `.pmtiles` of a few km² around the Vilnius old town is committed to `rust/simworld-core/tests/fixtures/`. It is cut from `data/out/vilnius.pmtiles` (61 MB) with `pmtiles extract --bbox=...`, and the target size is under 2 MB. Tests then never need the network or `serve.sh`. The fixture is an ODbL derived database, so it ships with `fixtures/README.md` stating source, date, license and attribution (see Licensing).

**Determinism hashes have to work across platforms.** f32/f64 results can differ between arm64 and x86_64: fused multiply-add, and libm differences in `sin`/`atan2`. Rayon must not affect output order either. The hash therefore:

- quantizes positions to 1 mm and colors to 8 bits before hashing
- sorts per-building output by OSM id

If hashes still differ, golden files are kept per architecture. The CI matrix runs the scenario level on both.

**Golden screenshots need a real GPU.** `--headless` uses the dummy renderer and renders nothing. Run level (c) of the engine tests on a GPU runner (a self-hosted Mac), or under a software Vulkan driver (lavapipe/SwiftShader) with looser tolerances. Until then, it runs locally before tagging.

### 8. Versioning and release

- Library versions use semver. `api_version` is the contract major·minor and is independent of the crate patch level.
- **Decision: the game gets prebuilt libraries; only the library repo builds from source.** The game then needs no Rust toolchain, cross targets or Xcode-for-iOS setup. Each zip holds native libraries, the C# runtime, shaders and `contract.md` from one tag, so they can't drift apart.
- A release is a git tag `vX.Y.Z` in the `simworld` repo plus a release asset `simworld-addon-vX.Y.Z.zip`: `addons/simworld/` with the built libraries in `bin/`.
- CI builds the release on one runner per platform: macOS for the universal dylib and the iOS xcframework, Windows for the x86_64 dll, Linux for the x86_64 so. Libraries are built in release mode, and a debug build of the macOS library goes in too so the editor can hot-reload.
- **The game vendors the unzipped addon** into `addons/simworld/` and commits it (the libraries are a few MB; use git LFS if they grow). `addons/simworld/VERSION` records the tag. A `tools/update_simworld.sh vX.Y.Z` script in the game downloads the zip and replaces the folder.
- **To work on the library and the game at the same time,** use a local, uncommitted dev override in the game: a `simworld.dev.gdextension`, or a symlink of `addons/simworld/` to the `simworld` checkout, that loads `../simworld/rust/target/debug`. Remove it before committing. CI in the game always uses the vendored release. Prefer the `.gdextension` override or a copy step over symlinks: Godot's file watching and import handle symlinked folders badly, and on Windows they need developer mode. The same applies to the test bench linking `addons/simworld/` (P5).
- **Signing:** the macOS dylib is code-signed (and the game's export notarized), or Gatekeeper blocks it in exported builds. The Windows dll is signed when the game ships.
- **Compatibility matrix:** each release lists the Godot version (`compatibility_minimum`), Godot .NET / .NET SDK, gdext, the Rust toolchain (MSRV 1.94) and the tileset schema version it was tested with.
- Release checklist:
  - all four test levels pass
  - `CHANGELOG.md` and `docs/contract.md` are updated
  - the iOS static library builds (`cargo build --target aarch64-apple-ios -p simworld-godot`), even before the iOS port, so that iOS readiness can't regress unnoticed; this includes compiling the bundled SQLite of the disk cache
  - `THIRD_PARTY_NOTICES.md` is regenerated

### 9. Licensing

- **The library's own license** (`LICENSE`) must be chosen before the first release zip leaves the repo, even if that zip only goes to our own game.
- **Dependency licenses:** Rust crates are collected with `cargo about` (or `cargo deny check licenses` in CI) into `THIRD_PARTY_NOTICES.md`. It ships in the release zip.
- **OSM data is ODbL.** The committed fixture `.pmtiles` and any distributed `.pmtiles` are derived databases. Distributing them carries ODbL obligations: attribution and share-alike for the database, though not for the game. "© OpenStreetMap contributors" in the UI covers the rendered output only. `attributions()` covers the UI side; the release notes and `data/README.md` cover the database side.
- Imagery (M7) brings its own provider license and attribution, which go through `attributions()` too.

### 10. Test bench: daylight menu (fixes A20)

The test bench must let a tester set any time of day and see the result, since day/night is half the look. This is host code, not library code: the game has its own clock and weather.

- **`TimeOfDay`** (`low-poly-map/scripts/TimeOfDay.cs`, exists as an interim implementation). It owns date, local time, clock speed and UTC offset (derived from the origin longitude unless set). It computes the sun's elevation and azimuth at the streamer origin, points the `Sun` light along it, and writes the night factor: 0 at elevation +6°, 1 at −6°, smooth in between. At night the light comes from a fixed moon opposite the sun's azimuth, so nothing is lit from below the horizon. Today the sun math is the NOAA approximation in C#. Once `set_clock`/`daylight()` exist in Rust (M6), `TimeOfDay` calls `set_clock` and reads `daylight()` instead, with the same public surface. The library and the game then share one answer for where the sun is.
- **`DaylightMenu`** (`low-poly-map/scripts/DaylightMenu.cs`, in the HUD, toggled with F2) has:
  - a time slider (0–24 h)
  - a date slider (day of year)
  - clock speed: paused, 1×, 1 min/s, 10 min/s, 1 h/s
  - presets computed for the chosen date: Sunrise, Noon (highest sun), Dusk (sun at −3°) and Night (lowest sun)
  - a night-factor override (checkbox + slider) for tuning the look in isolation
  - a readout of sun elevation, azimuth, the sun-derived night factor, the origin and the UTC offset

  Controls never take keyboard focus, so flying keeps working while the menu is open.
- **Launch arguments** for scripted shots and benches:
  - `--time=HH:MM`
  - `--date=MM-DD`
  - `--time-speed=<x>`
  - `--night=<0..1>`, which turns the override on
  
  The engine-level flight bench and the golden screenshots use `--time`/`--date` rather than `--night`, so they cover the real sun direction too.
- **After P5**, `TimeOfDay` drives the addon's shader globals (`simworld_night_factor`, `simworld_hour`) through `WorldView.SetTime(...)` instead of `StreamerBridge.NightFactor`, and the menu stays unchanged.
- **Acceptance:**
  - Dragging the time slider through dusk shows sun direction, sky, fog, window lights and glow changing continuously, with no frame where the night factor jumps.
  - On 21 Jun at 13:00 local time in Vilnius, the readout shows a sun elevation of 57–59°.
  - `--time=22:00 --date=10-26` gives night factor 1.

## Phases

The phases run in order. Each phase leaves the spike running as it does today.

| Phase | Content | Acceptance |
|---|---|---|
| P0 Baseline | Git repo, per-push CI, `docs/` + `.claude/agents/` moved to the root, `CLAUDE.md` refreshed, daylight menu (section 10) | `cargo test` passes (66 tests on 2026-10-26); the headless smoke test prints the `debug_stats` block; the daylight menu meets its acceptance; first commit tagged `v0.5.0-spike` |
| P1 Core split | Workspace, `simworld-core` with `World` (including the re-center logic from `streamer.rs`), `Log` and `Diagnostics`; thin `simworld-godot` | `simworld-core` builds with no `godot` in its dependency tree (`cargo tree -p simworld-core`); `streamer.rs` holds no constants or state beyond type conversion; the spike behaves identically |
| P2 Contract | `docs/contract.md`, `api_version`, agent files point to it, `CHANGELOG.md` | One contract document; the facade logs a version mismatch instead of probing silently |
| P3 Config | World profile JSON + schema, `configure_world`, `world_profile()` | Defaults reproduce today's golden tile hashes bit for bit; an override (e.g. R0 radius) changes the cover in a scenario test |
| P4 Host API | Geodetic camera input, conversions, `origin_shifted` signal, `status_changed`, `attributions`, `shutdown`, lifecycle rules (multiple instances, re-configure, cache lock) | A test node driven only by geodetic input flies the scenario path with no coordinate code in C#; a second `configure_world` teleports cleanly; a missing tile server is reported through `status_changed` |
| P5 Addon | `addons/simworld/` with `WorldView`, settings resources, global shader uniforms (`EditorPlugin` + runtime registration), `NightLook`, multi-platform `.gdextension`; the spike consumes it; `TimeOfDay` switches to `WorldView.SetTime` | A fresh empty Godot 4.7 .NET project with the addon copied in, a `Camera3D` and a `WorldView` shows the synthetic city day and night, with no files from `low-poly-map/scripts/` and no shader errors in the editor; a minimal iOS export of that project builds |
| P6 Test bench | `simworld-cli`, fixture `.pmtiles`, scenario and quantized golden hashes, contract replay, flight bench, golden screenshots (at fixed `--time`/`--date`) | All four test levels run from one command each; scenario hashes match on arm64 and x86_64; the budgets are reported as JSON |
| P7 Release | Push the `simworld` repo (created in P0) to its remote, release CI for all platforms (universal macOS via `lipo`, signing), iOS static build check, `LICENSE` + `THIRD_PARTY_NOTICES.md`, release zip, tag, the game's `update_simworld.sh` | The SimPlane game project imports the zip with no Rust toolchain installed and runs the demo scene. `../Simplane/` has no Godot project yet; creating one is a prerequisite |

**P5 status (2026-10-27).** Built: `addons/simworld/` (`plugin.cfg`, `SimWorldPlugin`, multi-platform `simworld.gdextension`, `runtime/`, `shaders/`, `textures/`), `WorldView`, `IWorldCameraSource`, `WorldRenderSettings`, `NightLook`, global shader uniforms (editor plugin and runtime registration), `TimeOfDay` through `WorldView.SetTime`, `tools/sync_addon.sh` (copy step, not a symlink, as section 8 advises) and `tools/build_addon.sh`. The fresh-project check passes (empty Godot 4.7 .NET project, addon copied in, `Camera3D` + `WorldView` + a 20-line clock script: synthetic city at day and night, plugin registers the globals, no shader errors). Not done: the minimal iOS export (no iOS rust target, Xcode export or Godot iOS .NET export tried), Windows / Linux libraries (need those hosts, P7 CI), a universal macOS dylib (only `aarch64-apple-darwin` is installed here), and the profile colours / lit shares as shader globals (no shader reads them until the window shader, M6). Deviation: the addon has no default origin, so `OriginLat` / `OriginLon` are required (section "Layer responsibilities"). Details: `CHANGELOG.md`, root `CLAUDE.md` "Addon".

The night-rendering work (M6) and imagery work (M7) continue in parallel after P2. New API from them goes straight into `contract.md` with an `api_version` bump.

## Decided (2026-10-26)

- **Repo:** a separate library repo, `simworld` (today's `lowpolymap/` restructured, with the test bench inside). The SimPlane game is a consumer repo.
- **Profile format:** JSON with a JSON Schema.
- **Delivery to the game:** prebuilt release zips from CI; source builds only inside the library repo, plus a local dev override for working on both at once.

## Open decisions

- **Class name:** keep `WorldStreamer`, or rename it to `SimWorldStreamer` before the first tag? Renaming later is a major version.
- **Should `NightLook` own the `Environment`** (simpler for hosts), or only suggest values the host applies (more control for the game's own weather and sky)?
- **Library license:** MIT/Apache-2.0 (like gdext), or proprietary while only SimPlane uses it? This is needed before P7.
- **Geodetic altitude datum:** should `update_camera_geodetic` take MSL only, or also accept ellipsoid height (JSBSim's native output) through a geoid model in the core?
- **A C ABI later?** A `simworld-ffi` crate would let non-Godot hosts embed the core, as JSBSim does with Python and Unreal. Not needed now, but the core/binding split keeps it possible.
