# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository. It is loaded into every session and every subagent: keep it short. Detail lives in `docs/status.md` (milestones, test baseline), `docs/addon-notes.md` (addon internals), `docs/README.md` (docs index) and `docs/bench/` (test bench).

## Project status

"SimWorld" (repo folder `lowpolymap`; crates `simworld-core` / `simworld-godot` / `simworld-cli`): a streamed low-poly OSM city for the SimPlane flight game. A Rust GDExtension (`WorldStreamer`) streams PMTiles around a camera and generates everything; a Godot 4.7 .NET addon (`addons/simworld/`) displays it; `low-poly-map/` is the test bench.

- Contract `api_version` 1.12 (`docs/contract.md`, history in `CHANGELOG.md`), crate 0.5.0, profile_version 7. Packaging phases P0-P5 done (`docs/spec-library-packaging.md`); P6 / P7 ahead; minimal iOS export not verified.
- Milestones: M1-M5 (tiles, buildings, LOD, ground) done; M6 night partway (windows W1-W4, street lamps, beacons; no `set_clock` / `daylight()` in Rust); M7 imagery, M8 measure not started; M9 terrain built procedurally (no DEM); M10 world seed: S1 built both sides, S2-S4 (generated cities) built, Godot side checked by `--seed-selftest` S2 / S3-S4 groups.
- Direction (`docs/decision-library-direction.md`, 2026-10-29): scenery engine, static files only, curated seed cities are the product, flat country until a DEM exists, feature freeze until SimPlane flies over the seed world. Read it before proposing a new subsystem, server or data source.
- Full milestone state and the `cargo test` baseline: `docs/status.md`.

## Commands

- Rust (from `rust/`, the workspace root, never from `low-poly-map/`): `cargo check`, `cargo test`, `cargo build` (debug `target/debug/libsimworld_godot.dylib` that the editor loads), `cargo build --release`. `cargo tree -p simworld-core | grep -i godot` must print nothing. Goldens change only for an intended output change: `SIMWORLD_UPDATE_GOLDEN=1 cargo test -p simworld-core golden -- --nocapture`.
- Addon sync: `tools/sync_addon.sh` copies `addons/simworld/` to the generated, git-ignored `low-poly-map/addons/simworld/` (the `.claude` hook runs it after edits; run it by hand before `dotnet build` if in doubt). Never edit the copy. It pulls Godot-generated `.uid` files back into the source (commit them).
- C# (from `low-poly-map/`): `dotnet build`.
- Godot binary: `G=~/Downloads/Godot_mono.app/Contents/MacOS/Godot`. Headless smoke test from `low-poly-map/`: `$G --headless --path . --quit-after 120` (add `-- --tile-url=` for the offline synthetic city); green = no errors and a `[WorldView] ... first tile` line. Self-tests: `/bench-check`. Measurements: `/bench-measure`.
- Addon libraries for a game / fresh project: `tools/build_addon.sh [--release] [--ios]` (not needed for the bench).
- Tile build: `data/build_tiles.sh` (Docker, planetiler) -> `data/out/vilnius.pmtiles`; serve with `data/serve.sh` at `http://127.0.0.1:8787/`. Two-level bake: `cargo run --release -p simworld-cli -- bake ../data/out/vilnius.pmtiles ../data/out/vilnius-v2.pmtiles`. See `data/README.md`.

## Reading big docs

`docs/contract.md` (136 KB), `docs/spec-world-seed.md` (121 KB), `docs/spec-terrain.md` (73 KB), `docs/spec-seed-city-pipeline.md` (59 KB) are too big to read whole. List the sections with `grep -n "^## \|^### " docs/<file>` and read only those the task needs (Read with offset / limit). The contract is the single source of truth; the specs are design history.

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


## Conventions

- `low-poly-map/.editorconfig`: UTF-8; `low-poly-map/.gitattributes`: LF line endings for text files (they still sit in the Godot project, not at the repo root).
- `low-poly-map/.godot/` is generated editor cache; don't edit or commit it.

## Subagents and token budget

Each spawn starts cold (this file plus its own prompt plus whatever it reads), so spawn only when it pays.

- Default: make the change in the main session. Spawn only when the user asks, or when a change spans Rust and Godot.
- Contract or profile change: `library-architect` (brief) -> implementer(s) -> `/contract-bump` -> `code-critic`.
- Seed / terrain / LOD change inside `rust/`: `rust-world-streamer` -> `/bench-check rust` plus the named self-tests (`seed`, `terrain`, `lod`, `cover`). Seed generation: budgets, rules and the `seed perf` report line in `docs/perf-seed.md`.
- Host side (addon, bench, shaders): `godot-csharp-integrator` -> `/bench-check <tests>`.
- Cost questions: `/bench-measure` (numbers), then `code-critic` for the cost review of a diff. No separate perf agent.
- A brief to an agent names the files, the contract / spec sections (numbers) and the acceptance tests, so the agent does not re-derive them; agents read only those sections.
- `selftest-runner` (haiku) runs builds and tests behind `/bench-check` and `/bench-measure`; never paste long logs into the main context.
