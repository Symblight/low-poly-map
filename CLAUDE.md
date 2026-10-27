# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project status

"SimWorld" (working name; the repo folder is still `lowpolymap`, the Rust crates are `simworld-core` / `simworld-godot` / `simworld-cli`) is a streamed low-poly OSM city for the SimPlane flight game: a Rust GDExtension (`WorldStreamer`) streams PMTiles around a camera and generates everything; a Godot 4.7 (.NET/C#) addon (`addons/simworld/`) displays it. It started as a spike and is being turned into a reusable library with a test bench. The packaging plan is in `docs/spec-library-packaging.md` (phases P0-P7); this repo has finished P0 (baseline), P1 (core split: Cargo workspace, `World`, `Log`, `Diagnostics`), P2 (contract: `docs/contract.md`, `api_version`), P3 (config: world profile JSON + schema, `configure_world`, `world_profile()`, golden determinism hashes), P4 (host API: geodetic camera input and conversions, `origin_shifted` / `status_changed` signals, `status()`, `attributions()`, `shutdown()`, cache folder lock, several instances) and P5 (addon: `addons/simworld/` with `WorldView`, settings resource, global shader uniforms, `NightLook`, editor plugin, multi-platform `.gdextension`; the test bench consumes it; **not verified: the minimal iOS export**, see "Addon"). Contract version: `api_version` 0.7 (`docs/contract.md`); crate version 0.5.0 (`CHANGELOG.md`).

Milestones (`docs/spike-streamed-osm-city.md`, "Milestones, acceptance and risks"):

- **M1 Toolchain, M2 Tiles, M3 Buildings, M4 LOD, M5 Ground: done.** Rust modules in `rust/simworld-core/src/`: `fetch` (PMTiles over HTTP range + disk LRU cache: one SQLite file per source, `tiles-{hash}.sqlite`, on its own cache thread; `docs/spec-tile-cache.md`), `mvt`, `geo`, `rings` (quadtree cover R0-R3), `scheduler` (cover, hysteresis, sibling swap, fades), `worker` (tokio + rayon), `mesher`, `ground`, `lights`, `synthetic` (offline city), `world` (`World`: frame, floating origin and re-center, re-delivery pacing, warning rate limit, profiling; mirrors the contract in plain Rust), `diag` (`Log` trait, `Diagnostics`). The only `godot` code is `rust/simworld-godot/src/` (`streamer.rs` type-conversion shell over `World`, `host.rs` mapping `Log` to `godot_print!`/`godot_warn!`/`godot_error!` and `SIMPLANE_PROFILE=1` to `Diagnostics`, `lib.rs` entry).
- **M6 Night: partway.** Built: dithered LOD fades (`poll_fades`, `shaders/lod_dither.gdshaderinc`), glow/fog/tone map/eye adaptation (`NightRenderController`), light sprites with a `night_factor` (roads do not glow), and an interim C# daylight clock (`TimeOfDay`, `DaylightMenu`; NOAA sun math in C#). **Not built:** the window shader with light profiles (`building_windows.gdshader`; `building_day.gdshader` is still the daytime placeholder and there is no `hour` uniform), sprite/window cross-fade, and `set_clock` / `daylight()` in Rust.
- **M7 Imagery, M8 Measure: not started.** `ground_height_at` returns 0 (no DEM).

The Rust side has a `cargo test` baseline of 108 passed, 0 failed, 3 ignored (105 in `simworld-core` + 3 in `simworld-godot`; tests pin `api_version`, the `#[func]` list and the `#[signal]` list to `docs/contract.md`, `lifecycle_tests` cover the 0.7 host API and lifecycle, `profile::tests` pin `addons/simworld/profiles/default.json` and the schema to the code, `golden_tests` pin per-tile output hashes and the cover). `scheduler::tests::GodotSim` mirrors the C# visibility rule.

## Commands

- Rust (from `rust/`, the workspace root, never from `low-poly-map/`): `cargo check`, `cargo test`, `cargo build` (debug `target/debug/libsimworld_godot.dylib` that the editor loads), `cargo build --release`. `cargo tree -p simworld-core | grep -i godot` must print nothing.
- **Addon sync (after every edit under `addons/simworld/`, before `dotnet build` or running the bench):** `tools/sync_addon.sh` copies `addons/simworld/` to `low-poly-map/addons/simworld/` (generated, git-ignored; the addon source is at the repo root, never edit the copy). It first pulls Godot-generated `.uid` files that exist only in the copy back to the source (commit them), and leaves out `simworld.gdextension` and `bin/` (the bench loads the library from `rust/target` through `low-poly-map/simplane_world.gdextension`, the editor-only dev override).
- C# (from `low-poly-map/`): `dotnet build`.
- Addon libraries: `tools/build_addon.sh [--release] [--ios]` builds `simworld-godot` for this OS and copies it to `addons/simworld/bin/<os>/` (git-ignored; macOS universal via `lipo` when both rust targets are installed, else the host architecture only). Needed only for a project that uses the addon's own `.gdextension` (a game, the fresh-project check), not for the bench.
- Godot binary is not on PATH: `G=~/Downloads/Godot_mono.app/Contents/MacOS/Godot`. Headless smoke test, from `low-poly-map/`: `$G --headless --path . --quit-after 120`. It prints the godot-rust init line, the Godot version, the `[WorldView]` lines (tile source, `poll_fades available`, `WorldStreamer configured ... [simworld-godot <version> (build ...)]`, disk cache stats, first tile, a `[WorldView] debug_stats {...}` line) and some `slow upload` lines. A green run has no errors and a `first tile` line. Add `-- --tile-url=` for the offline synthetic city.
- Run the test bench: `$G --path .` (add user arguments after `--`, see `low-poly-map/CLAUDE.md`).
- Hot reload: keep the editor open, `cargo build`, refocus the editor window (`reloadable = true` in `simplane_world.gdextension`). `WorldStreamer` is not a tool class, so it only runs in-game, not in the editor.
- Tile build: `data/build_tiles.sh` (Docker, planetiler 0.10.2) writes `data/out/vilnius.pmtiles`; `data/serve.sh` serves it with Range support at `http://127.0.0.1:8787/`. See `data/README.md`.

## Docs

- `docs/spec-library-packaging.md`: how the spike becomes a reusable library (audit, target structure, phases P0-P7, versioning, daylight menu acceptance). Read it before restructuring anything.
- `docs/spike-streamed-osm-city.md`: the full design (LOD rings, data pipeline, light profiles, imagery, daylight, library API, rendering, iOS constraints, budgets, milestones). Source: https://claude.ai/artifact/1Li6b4rWTHqGtKGDYQZE6E. Read it before making architectural decisions.
- `docs/spec-night-rendering.md`: night details (LOD fades, bloom, tone map, exposure, depth precision). Source: https://claude.ai/artifact/HpzXEiRh1VS71asBF2ER4d.
- `docs/contract.md`: **the** `WorldStreamer` contract (methods, call order, ready-tile dictionary, visibility rule and guarantees, fades, encodings, lights, ground, coordinates, planned API with target `api_version`s). The single source; read it before changing either side.
- `docs/spec-tile-cache.md`: replacing the file-per-tile disk cache with one SQLite file per source (design, eviction, tests, phases T1-T3). Built; its "Status and measured results" section has the benchmark (`cargo bench -p simworld-core --bench cache_bench`), the acceptance results and the deviations from the design.
- `CHANGELOG.md`: API history. Update it with every contract change.

## Layout (today)

```
lowpolymap/                  # repo root (git); the future library repo is called `simworld`
  CLAUDE.md                  # this file
  CHANGELOG.md
  docs/                      # contract.md (the WorldStreamer contract), specs and the design doc
  .claude/agents/            # rust-world-streamer.md, godot-csharp-integrator.md (point to docs/contract.md)
  addons/simworld/           # the Godot rendering addon (P5): plugin.cfg, simworld.gdextension, runtime/ (C#, namespace SimWorld), shaders/, textures/,
                             #   profiles/ (default.json + world-profile.schema.json, P3), bin/ (built libraries, ignored)
  tools/                     # sync_addon.sh (addon -> bench copy), build_addon.sh (libraries -> addons/simworld/bin)
  low-poly-map/              # Godot project root and test bench (project.godot, scenes/, scripts/ = host code, shaders/fallback_ground; addons/ = generated copy)
  rust/                      # Cargo workspace, OUTSIDE the Godot project:
    simworld-core/           #   pure Rust world (no godot), rlib; World, Log, Diagnostics, all modules
    simworld-godot/          #   gdext binding, WorldStreamer node only (cdylib + staticlib -> libsimworld_godot.*); build.rs (build marker)
    simworld-cli/            #   headless runner: validate-profile, default-profile (fly/bake/tile/bench in P6)
  data/                      # tile build: planetiler schema, build_tiles.sh, serve.sh, out/vilnius.pmtiles
```

- The Godot project is `low-poly-map/` itself; don't create a nested `godot/` folder.
- Never put Rust crates inside `low-poly-map/`. They go in `rust/`.
- `low-poly-map/simplane_world.gdextension` (file name kept) points at `res://../rust/target/{debug,release}/libsimworld_godot.dylib` (macOS only; editor use only). Exported builds and the iOS `.xcframework` need the library copied into the project.

## Addon (`addons/simworld/`, P5)

- `runtime/` (namespace `SimWorld`, compiled into the host's assembly): `WorldView` (the entry node: owns the streamer, runs the per-frame calls, uploads, fades, shifts the origin; no default origin, no command line; `AutoStart` or `Start()`, `SetTime(hour, night)`, C# event `OriginShifted`), `IWorldCameraSource` (+ `Camera3DSource` adapter; a `Camera3D` needs no code), `WorldRenderSettings` (resource: upload budget, prewarm, debug, building/ground/light-sprite material tunables), `NightLook` (resource: glow threshold/intensity and fog on an `Environment` the host passes in; exposure, tone map, sun, sky stay in the host), `ShaderGlobals` / `AddonPaths`, `SimWorldPlugin` (editor plugin), `WorldStreamerFacade`, `TilePool`, `TileCover`, `TileKey`.
- Shader globals `simworld_night_factor`, `simworld_hour`, `simworld_curve_center`, `simworld_curve_inv_2r`: the editor plugin writes them into `[shader_globals]` when enabled; `WorldView` adds any that are missing at runtime. `GlobalShaderParameterGetList` is editor-only: never use it at runtime.
- Asset paths come from the script's resource path (`AddonPaths.Root`); shader `#include`s are relative (the dither variant rewrites them to absolute because an in-memory shader has no path). `build_addon.sh` fills `bin/`; the release zip (P7) is this folder with `bin/`.
- Not done: the minimal iOS export of a project that uses the addon (needs the iOS rust target, Xcode and Godot's iOS .NET export; the plan has a fallback if C# on iOS is blocked), `ios` `.gdextension` entries are written but unbuilt; Windows and Linux libraries are built only on those hosts (CI, P7); the profile colours / lit shares as shader globals wait for the window shader (M6).

## Planned library structure (docs/spec-library-packaging.md, "Target structure")

- Done: P1 workspace (`simworld-core`, `simworld-godot`, `simworld-cli`), P3 world profile (`simworld-core/src/profile.rs`; `addons/simworld/profiles/`; `simworld-cli validate-profile` / `default-profile`), P5 addon. Still to come in the core: `daylight` (M6); the other CLI commands (P6).
- `low-poly-map/` stays as the test bench and demo host: `FreeFlyCamera`, `BenchArgs` (all launch arguments), `DebugOverlay`, `TimeOfDay`, `DaylightMenu`, `SettingsMenu`, `NightRenderController`, test drivers.
- Releases: git tag `vX.Y.Z` plus `simworld-addon-vX.Y.Z.zip` with prebuilt libraries; the SimPlane game vendors the zip and needs no Rust toolchain.

## Architecture

- **Rust owns the world, Godot only displays it.** The `WorldStreamer` Node does the quadtree cover by 3D slant distance (R0 z16 < 0.6 km, R1 z15 < 2.5 km, R2 z14 < 7 km, R3 z13 < 25 km), PMTiles fetching over HTTPS range requests, disk LRU cache, MVT decoding, meshing, ground meshes and light points on worker threads (tokio for I/O, rayon for meshing, crossbeam channel back). C# drives the camera, uploads finished tiles (at most 2 per frame, at most 2 ms) and renders.
- **The `WorldStreamer` API is a contract** shared by both sides: `api_version`, `configure`, `configure_world`, `world_profile`, `update_camera`, `update_camera_geodetic`, `geodetic_to_local`, `local_to_geodetic`, `poll_ready`, `poll_unload`, `poll_fades`, `take_origin_shift`, `attributions`, `status`, `shutdown`, `debug_stats`, `debug_tiles`, `ground_height_at`, signals `origin_shifted` and `status_changed`, plus the ready-tile dictionary shape (incl. the quadtree sibling-swap visibility rule and dithered fades) and the vertex COLOR encodings. **The contract is `docs/contract.md`** (single source; the agent files only point to it). Any change updates that file, `api_version` (`API_MAJOR`/`API_MINOR` in `rust/simworld-core/src/lib.rs`) and `CHANGELOG.md` in the same commit. `api_version` = contract `major.minor`, independent of the crate version: a minor bump is additive only, a major bump is breaking. `cargo test` fails if the constants and the `api_version:` line of `docs/contract.md` disagree or a `#[func]` or `#[signal]` is missing from it.
- **Floating origin:** f64 ENU inside Rust, f32 relative to origin in Godot; re-center beyond 5 km and shift camera + all tile roots in the same frame.
- **Coordinates:** Y-up, x = east, z = -north, meters.
- **Determinism:** same tile gives the same mesh and the same lit windows (seeded by OSM id). Pinned by `simworld-core` `golden_tests` (quantized per-tile hashes of the synthetic city, all bands; regenerate only for an intended change with `SIMWORLD_UPDATE_GOLDEN=1 cargo test -p simworld-core golden -- --nocapture`).
- **Tuning is data:** band edges, DP tolerances, budgets, light profiles, road/area tables etc. live in the world profile (`docs/contract.md` 3.2), not in module constants. A changed default changes host-visible behaviour: update `default.json`, the schema (the tests enforce both) and `CHANGELOG.md`. Structural values (4 bands, zooms 13-16, layer names) stay constants.

## Constraints

- Renderer is **Mobile** (`low-poly-map/project.godot`); don't use Forward+-only features. Physics is Jolt. Windows uses D3D12.
- Desktop first, but stay iOS-ready: Rust uses rustls (no OpenSSL), no desktop-only crates, crate-type `cdylib` + `staticlib` for a later `.xcframework`.
- Never call Godot APIs from Rust worker threads; convert to Packed arrays only on the main thread.
- Pool `MeshInstance3D`/`MultiMeshInstance3D` nodes instead of freeing/allocating per tile.
- Tile data comes from a self-hosted custom `.pmtiles` (planetiler profile with a `buildings` layer keeping building `kind`), never public OSM tile servers. Attribution "© OpenStreetMap contributors" is required.
- No git commits, tags or pushes unless the user asks. `.godot/`, `rust/target/`, `data/out/`, `data/sources/` are generated and ignored (`.gitignore`).

## Subagents

Project subagents (`.claude/agents/`): `rust-world-streamer` (anything under `rust/`) and `godot-csharp-integrator` (the Godot project: scenes, C# scripts, shaders). Use them when the user asks to delegate work on those areas.

## Conventions

- `low-poly-map/.editorconfig`: UTF-8; `low-poly-map/.gitattributes`: LF line endings for text files (they still sit in the Godot project, not at the repo root).
- `low-poly-map/.godot/` is generated editor cache; don't edit or commit it.
