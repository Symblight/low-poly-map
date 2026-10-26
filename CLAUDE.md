# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project status

"SimWorld" (working name; the repo folder and the Rust crate are still `lowpolymap` / `simplane_world`) is a streamed low-poly OSM city for the SimPlane flight game: a Rust GDExtension (`WorldStreamer`) streams PMTiles around a camera and generates everything; a Godot 4.7 (.NET/C#) project displays it. It started as a spike and is being turned into a reusable library with a test bench. The packaging plan is in `docs/spec-library-packaging.md` (phases P0-P7); this repo is at P0 (baseline). Current API version in `CHANGELOG.md`: `0.5.0-spike`.

Milestones (`docs/spike-streamed-osm-city.md`, "Milestones, acceptance and risks"):

- **M1 Toolchain, M2 Tiles, M3 Buildings, M4 LOD, M5 Ground: done.** Rust modules in `rust/src/`: `fetch` (PMTiles over HTTP range + disk LRU cache), `mvt`, `geo`, `rings` (quadtree cover R0-R3), `scheduler` (cover, hysteresis, sibling swap, fades), `worker` (tokio + rayon), `mesher`, `ground`, `lights`, `synthetic` (offline city), `streamer` (the only `godot` code besides `lib.rs`).
- **M6 Night: partway.** Built: dithered LOD fades (`poll_fades`, `shaders/lod_dither.gdshaderinc`), glow/fog/tone map/eye adaptation (`NightRenderController`), light sprites and lit roads with a `night_factor`, and an interim C# daylight clock (`TimeOfDay`, `DaylightMenu`; NOAA sun math in C#). **Not built:** the window shader with light profiles (`building_windows.gdshader`; `building_day.gdshader` is still the daytime placeholder and there is no `hour` uniform), sprite/window cross-fade, and `set_clock` / `daylight()` in Rust.
- **M7 Imagery, M8 Measure: not started.** `ground_height_at` returns 0 (no DEM).

The Rust side has a `cargo test` baseline of 66 passed, 0 failed, 3 ignored (2026-10-26). `scheduler::tests::GodotSim` mirrors the C# visibility rule.

## Commands

- Rust (from `rust/`, never from `low-poly-map/`): `cargo check`, `cargo test`, `cargo build` (debug dylib that the editor loads), `cargo build --release`.
- C# (from `low-poly-map/`): `dotnet build`.
- Godot binary is not on PATH: `G=~/Downloads/Godot_mono.app/Contents/MacOS/Godot`. Headless smoke test, from `low-poly-map/`: `$G --headless --path . --quit-after 120`. It prints the godot-rust init line, the Godot version, the `[StreamerBridge]` lines (tile source, `poll_fades available`, `WorldStreamer configured ... [simplane_world <version> (build ...)]`, disk cache stats, first tile) and some `slow upload` lines. It does not print a `debug_stats` block any more (the M1 stub printed one); a green run has no errors and a `first tile` line.
- Run the test bench: `$G --path .` (add user arguments after `--`, see `low-poly-map/CLAUDE.md`).
- Hot reload: keep the editor open, `cargo build`, refocus the editor window (`reloadable = true` in `simplane_world.gdextension`). `WorldStreamer` is not a tool class, so it only runs in-game, not in the editor.
- Tile build: `data/build_tiles.sh` (Docker, planetiler 0.10.2) writes `data/out/vilnius.pmtiles`; `data/serve.sh` serves it with Range support at `http://127.0.0.1:8787/`. See `data/README.md`.

## Docs

- `docs/spec-library-packaging.md`: how the spike becomes a reusable library (audit, target structure, phases P0-P7, versioning, daylight menu acceptance). Read it before restructuring anything.
- `docs/spike-streamed-osm-city.md`: the full design (LOD rings, data pipeline, light profiles, imagery, daylight, library API, rendering, iOS constraints, budgets, milestones). Source: https://claude.ai/artifact/1Li6b4rWTHqGtKGDYQZE6E. Read it before making architectural decisions.
- `docs/spec-night-rendering.md`: night details (LOD fades, bloom, tone map, exposure, depth precision). Source: https://claude.ai/artifact/HpzXEiRh1VS71asBF2ER4d.
- `CHANGELOG.md`: API history. Update it with every contract change.

## Layout (today)

```
lowpolymap/                  # repo root (git); the future library repo is called `simworld`
  CLAUDE.md                  # this file
  CHANGELOG.md
  docs/                      # specs and the design doc
  .claude/agents/            # rust-world-streamer.md, godot-csharp-integrator.md (hold the contract today)
  low-poly-map/              # Godot project root and test bench (project.godot, scenes/, scripts/, shaders/, textures/)
  rust/                      # the `simplane_world` crate (cdylib + staticlib), OUTSIDE the Godot project
  data/                      # tile build: planetiler schema, build_tiles.sh, serve.sh, out/vilnius.pmtiles
```

- The Godot project is `low-poly-map/` itself; don't create a nested `godot/` folder.
- Never put Rust crates inside `low-poly-map/`. They go in `rust/`.
- `low-poly-map/simplane_world.gdextension` points at `res://../rust/target/{debug,release}/...` (macOS only; editor use only). Exported builds and the iOS `.xcframework` need the library copied into the project.

## Planned library structure (docs/spec-library-packaging.md, "Target structure")

Not built yet; do not create these folders without the matching phase.

- `rust/` becomes a Cargo workspace: `simworld-core` (pure Rust, no `godot`: geo, rings, scheduler, fetch, mvt, mesher, ground, lights, daylight, synthetic, worker, config), `simworld-godot` (thin gdext binding, cdylib + staticlib), `simworld-cli` (headless runner). Phase P1.
- `addons/simworld/` at the repo root: the Godot rendering addon (`plugin.cfg`, multi-platform `.gdextension`, C# runtime `WorldView`/`TilePool`/`TileCover`/`TileKey`/facade, shaders, textures, default JSON world profile). Phase P5.
- `low-poly-map/` stays as the test bench and demo host: `FreeFlyCamera`, `DebugOverlay`, `TimeOfDay`, `DaylightMenu`, test drivers.
- `docs/contract.md` becomes the single source of the `WorldStreamer` contract (phase P2); until then it is duplicated in the two agent files.
- Releases: git tag `vX.Y.Z` plus `simworld-addon-vX.Y.Z.zip` with prebuilt libraries; the SimPlane game vendors the zip and needs no Rust toolchain.

## Architecture

- **Rust owns the world, Godot only displays it.** The `WorldStreamer` Node does the quadtree cover by 3D slant distance (R0 z16 < 0.6 km, R1 z15 < 2.5 km, R2 z14 < 7 km, R3 z13 < 25 km), PMTiles fetching over HTTPS range requests, disk LRU cache, MVT decoding, meshing, ground meshes and light points on worker threads (tokio for I/O, rayon for meshing, crossbeam channel back). C# drives the camera, uploads finished tiles (at most 2 per frame, at most 2 ms) and renders.
- **The `WorldStreamer` API is a contract** shared by both sides: `configure`, `update_camera`, `poll_ready`, `poll_unload`, `poll_fades`, `take_origin_shift`, `debug_stats`, `debug_tiles`, `ground_height_at`, plus the ready-tile dictionary shape (incl. the quadtree sibling-swap visibility rule and dithered fades) and the vertex COLOR encodings. It is specified identically in `.claude/agents/rust-world-streamer.md` and `.claude/agents/godot-csharp-integrator.md`; any change must be made in both (and in `CHANGELOG.md`) until P2 moves it to `docs/contract.md`.
- **Floating origin:** f64 ENU inside Rust, f32 relative to origin in Godot; re-center beyond 5 km and shift camera + all tile roots in the same frame.
- **Coordinates:** Y-up, x = east, z = -north, meters.
- **Determinism:** same tile gives the same mesh and the same lit windows (seeded by OSM id). Not yet pinned by tests (spec audit A16).

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
