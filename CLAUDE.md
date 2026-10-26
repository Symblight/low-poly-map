# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project status

"SimWorld" (working name; the repo folder is still `lowpolymap`, the Rust crates are `simworld-core` / `simworld-godot` / `simworld-cli`) is a streamed low-poly OSM city for the SimPlane flight game: a Rust GDExtension (`WorldStreamer`) streams PMTiles around a camera and generates everything; a Godot 4.7 (.NET/C#) project displays it. It started as a spike and is being turned into a reusable library with a test bench. The packaging plan is in `docs/spec-library-packaging.md` (phases P0-P7); this repo has finished P0 (baseline), P1 (core split: Cargo workspace, `World`, `Log`, `Diagnostics`), P2 (contract: `docs/contract.md`, `api_version`), P3 (config: world profile JSON + schema, `configure_world`, `world_profile()`, golden determinism hashes) and the Rust half of P4 (host API: geodetic camera input and conversions, `origin_shifted` / `status_changed` signals, `status()`, `attributions()`, `shutdown()`, cache folder lock, several instances). Contract version: `api_version` 0.7 (`docs/contract.md`); crate version 0.5.0 (`CHANGELOG.md`).

Milestones (`docs/spike-streamed-osm-city.md`, "Milestones, acceptance and risks"):

- **M1 Toolchain, M2 Tiles, M3 Buildings, M4 LOD, M5 Ground: done.** Rust modules in `rust/simworld-core/src/`: `fetch` (PMTiles over HTTP range + disk LRU cache), `mvt`, `geo`, `rings` (quadtree cover R0-R3), `scheduler` (cover, hysteresis, sibling swap, fades), `worker` (tokio + rayon), `mesher`, `ground`, `lights`, `synthetic` (offline city), `world` (`World`: frame, floating origin and re-center, re-delivery pacing, warning rate limit, profiling; mirrors the contract in plain Rust), `diag` (`Log` trait, `Diagnostics`). The only `godot` code is `rust/simworld-godot/src/` (`streamer.rs` type-conversion shell over `World`, `host.rs` mapping `Log` to `godot_print!`/`godot_warn!`/`godot_error!` and `SIMPLANE_PROFILE=1` to `Diagnostics`, `lib.rs` entry).
- **M6 Night: partway.** Built: dithered LOD fades (`poll_fades`, `shaders/lod_dither.gdshaderinc`), glow/fog/tone map/eye adaptation (`NightRenderController`), light sprites and lit roads with a `night_factor`, and an interim C# daylight clock (`TimeOfDay`, `DaylightMenu`; NOAA sun math in C#). **Not built:** the window shader with light profiles (`building_windows.gdshader`; `building_day.gdshader` is still the daytime placeholder and there is no `hour` uniform), sprite/window cross-fade, and `set_clock` / `daylight()` in Rust.
- **M7 Imagery, M8 Measure: not started.** `ground_height_at` returns 0 (no DEM).

The Rust side has a `cargo test` baseline of 93 passed, 0 failed, 3 ignored (P4: 90 in `simworld-core` + 3 in `simworld-godot`; tests pin `api_version`, the `#[func]` list and the `#[signal]` list to `docs/contract.md`, `lifecycle_tests` cover the 0.7 host API and lifecycle, `profile::tests` pin `addons/simworld/profiles/default.json` and the schema to the code, `golden_tests` pin per-tile output hashes and the cover). `scheduler::tests::GodotSim` mirrors the C# visibility rule.

## Commands

- Rust (from `rust/`, the workspace root, never from `low-poly-map/`): `cargo check`, `cargo test`, `cargo build` (debug `target/debug/libsimworld_godot.dylib` that the editor loads), `cargo build --release`. `cargo tree -p simworld-core | grep -i godot` must print nothing.
- C# (from `low-poly-map/`): `dotnet build`.
- Godot binary is not on PATH: `G=~/Downloads/Godot_mono.app/Contents/MacOS/Godot`. Headless smoke test, from `low-poly-map/`: `$G --headless --path . --quit-after 120`. It prints the godot-rust init line, the Godot version, the `[StreamerBridge]` lines (tile source, `poll_fades available`, `WorldStreamer configured ... [simworld-godot <version> (build ...)]`, disk cache stats, first tile, a `[StreamerBridge] debug_stats {...}` line) and some `slow upload` lines. A green run has no errors and a `first tile` line. Add `-- --tile-url=` for the offline synthetic city.
- Run the test bench: `$G --path .` (add user arguments after `--`, see `low-poly-map/CLAUDE.md`).
- Hot reload: keep the editor open, `cargo build`, refocus the editor window (`reloadable = true` in `simplane_world.gdextension`). `WorldStreamer` is not a tool class, so it only runs in-game, not in the editor.
- Tile build: `data/build_tiles.sh` (Docker, planetiler 0.10.2) writes `data/out/vilnius.pmtiles`; `data/serve.sh` serves it with Range support at `http://127.0.0.1:8787/`. See `data/README.md`.

## Docs

- `docs/spec-library-packaging.md`: how the spike becomes a reusable library (audit, target structure, phases P0-P7, versioning, daylight menu acceptance). Read it before restructuring anything.
- `docs/spike-streamed-osm-city.md`: the full design (LOD rings, data pipeline, light profiles, imagery, daylight, library API, rendering, iOS constraints, budgets, milestones). Source: https://claude.ai/artifact/1Li6b4rWTHqGtKGDYQZE6E. Read it before making architectural decisions.
- `docs/spec-night-rendering.md`: night details (LOD fades, bloom, tone map, exposure, depth precision). Source: https://claude.ai/artifact/HpzXEiRh1VS71asBF2ER4d.
- `docs/contract.md`: **the** `WorldStreamer` contract (methods, call order, ready-tile dictionary, visibility rule and guarantees, fades, encodings, lights, ground, coordinates, planned API with target `api_version`s). The single source; read it before changing either side.
- `CHANGELOG.md`: API history. Update it with every contract change.

## Layout (today)

```
lowpolymap/                  # repo root (git); the future library repo is called `simworld`
  CLAUDE.md                  # this file
  CHANGELOG.md
  docs/                      # contract.md (the WorldStreamer contract), specs and the design doc
  .claude/agents/            # rust-world-streamer.md, godot-csharp-integrator.md (point to docs/contract.md)
  addons/simworld/profiles/  # world profile: default.json (all keys = compiled-in defaults) + world-profile.schema.json (P3)
  low-poly-map/              # Godot project root and test bench (project.godot, scenes/, scripts/, shaders/, textures/)
  rust/                      # Cargo workspace, OUTSIDE the Godot project:
    simworld-core/           #   pure Rust world (no godot), rlib; World, Log, Diagnostics, all modules
    simworld-godot/          #   gdext binding, WorldStreamer node only (cdylib + staticlib -> libsimworld_godot.*); build.rs (build marker)
    simworld-cli/            #   headless runner: validate-profile, default-profile (fly/bake/tile/bench in P6)
  data/                      # tile build: planetiler schema, build_tiles.sh, serve.sh, out/vilnius.pmtiles
```

- The Godot project is `low-poly-map/` itself; don't create a nested `godot/` folder.
- Never put Rust crates inside `low-poly-map/`. They go in `rust/`.
- `low-poly-map/simplane_world.gdextension` (file name kept) points at `res://../rust/target/{debug,release}/libsimworld_godot.dylib` (macOS only; editor use only). Exported builds and the iOS `.xcframework` need the library copied into the project.

## Planned library structure (docs/spec-library-packaging.md, "Target structure")

Not built yet; do not create these folders without the matching phase.

- Done in P1: `rust/` is a Cargo workspace with `simworld-core`, `simworld-godot`, `simworld-cli` (see Layout). Done in P3: the world profile (`simworld-core/src/profile.rs`; `addons/simworld/profiles/default.json` + `world-profile.schema.json`) and `simworld-cli validate-profile` / `default-profile`. Still to come in the core: `daylight` (M6); the other CLI commands (P6).
- `addons/simworld/` at the repo root: the Godot rendering addon (`plugin.cfg`, multi-platform `.gdextension`, C# runtime `WorldView`/`TilePool`/`TileCover`/`TileKey`/facade, shaders, textures). Only `addons/simworld/profiles/` exists so far (P3); the rest is phase P5.
- `low-poly-map/` stays as the test bench and demo host: `FreeFlyCamera`, `DebugOverlay`, `TimeOfDay`, `DaylightMenu`, test drivers.
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
