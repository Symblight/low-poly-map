# Changelog

All notable changes to the SimWorld library (Rust `WorldStreamer` GDExtension plus the Godot-side runtime) and its `WorldStreamer` contract. Versions follow semver. `api_version` (from P2) is the contract major.minor, independent of the crate version, and is listed per release; the contract itself is `docs/contract.md`.

Format: one section per release, newest first. Every contract change (a method, a dictionary key, an encoding, a guarantee) must be listed here, in `docs/contract.md` and in `api_version` (`API_MAJOR`/`API_MINOR`), in the same commit.

## Unreleased

P2 "Contract" (`docs/spec-library-packaging.md`). **api_version 0.5** (introduced; additive only, nothing existing changed).

- `docs/contract.md` is the single statement of the `WorldStreamer` contract: methods and call order, ready-tile dictionary, visibility rule and guarantees, fades and dither rule, vertex/ground COLOR and UV encodings, lights, ground, coordinates, data source, determinism, and the planned API with target versions (0.6 configuration, 0.7 host API and lifecycle, 0.8 daylight, 0.9 imagery). It records behaviour the old copies left out (no-fade exceptions, `update_camera` ignoring `local_pos.y`, horizontal re-center distance, tile-local roof UVs, profile id 5 never emitted). The copies in `.claude/agents/*.md` are replaced by a pointer to it.
- New `#[func] api_version() -> Vector2i(major, minor)` on `WorldStreamer` (currently `(0, 5)`); callable before `configure`.
- `debug_stats()` gains `api_major` and `api_minor` (int), same values.
- `simworld_core::API_MAJOR` / `API_MINOR` constants. Two new tests pin them to the `api_version:` line of `docs/contract.md` and check that every `#[func]` is documented there (`cargo test`: 68 passed).

P1 "Core split" (`docs/spec-library-packaging.md`). No contract change: the 9 `#[func]` methods, their signatures and every dictionary key are unchanged.

- `rust/` is a Cargo workspace: `simworld-core` (rlib, no `godot`), `simworld-godot` (cdylib + staticlib, the `WorldStreamer` node), `simworld-cli` (placeholder). The library file is now `libsimworld_godot.*` (was `libsimplane_world.*`); `low-poly-map/simplane_world.gdextension` keeps its name and `entry_symbol = "gdext_rust_init"`, only the library paths changed.
- `debug_stats()["version"]` (display string) now reads `simworld-godot 0.5.0 (build ...)` instead of `simplane_world 0.5.0 (build ...)`; the `configure` log line shows the same label.
- New core types: `World` (floating origin and re-center, re-delivery pacing, warning rate limit, profiling; the logic that was in `streamer.rs`), `Log` trait, `Diagnostics`. The core reads no environment variables; the Godot binding maps `SIMPLANE_PROFILE=1` onto `Diagnostics` when the node is created (it used to be read on each `configure`).

## 0.5.0-spike (2026-10-26)

First baseline: the spike as built, at milestone M5 and partway through M6. Crate `simplane_world` 0.5.0 (gdext 0.5.5, `api-4-7`, Godot 4.7). There is no `api_version` yet; `debug_stats()["version"]` is a display string only.

### API: `WorldStreamer` (Node, registered by the GDExtension; entry symbol `gdext_rust_init`)

Methods (all `#[func]` in `rust/src/streamer.rs`). Per-frame call order: `update_camera`, `take_origin_shift`, `poll_ready`, `poll_fades`, `poll_unload`. Coordinates: Y-up, x = east, z = -north, meters, relative to the current floating origin.

| Method | Description |
|---|---|
| `configure(origin_lat: f64, origin_lon: f64, tile_url: String, cache_dir: String)` | Sets the origin and data source and (re)starts the workers. Empty `tile_url` = deterministic synthetic city. Re-calling it drops the old scheduler; everything already delivered is returned by the next `poll_unload`. |
| `update_camera(local_pos: Vector3, velocity: Vector3, alt_agl_m: f64)` | Every frame. Recomputes the quadtree cover (3D slant distance, look-ahead, hysteresis) and queues jobs. Triggers a floating-origin re-center when the camera is more than 5000 m from the origin. |
| `poll_ready(max: i32) -> Array<Dictionary>` | Finished tiles, at most `max` (the bridge asks for 2). Every requested tile is delivered, even when empty. Light re-deliveries (clustering above ~3000 m AGL) are limited to 1 per frame. |
| `poll_unload() -> PackedStringArray` | Tile keys (`z/x/y`) to remove. |
| `poll_fades() -> Dictionary` | Key (`z/x/y`) to `fade` (float 0..1) for every tile in a LOD transition. Incoming tiles go 0 to 1, outgoing 1 to 0, summing to 1. Driven by slant distance, force-completed after 3 s. Tiles not listed have fade 1. |
| `take_origin_shift() -> Vector3` | Non-zero once after a re-center; the host subtracts it from the camera and all tile roots in the same frame. |
| `debug_stats() -> Dictionary` | `queued`, `fetching`, `meshing`, `loaded` (each `PackedInt32Array` of length 4, indexed by ring), `cache_mb` (float), `origin_lat`, `origin_lon` (of the current local origin), `version` (String, e.g. `simplane_world 0.5.0 (build debug @...)`). |
| `debug_tiles() -> Array<Dictionary>` | One entry per tracked tile: `key`, `ring` (0..3), `state` (`queued`, `fetching`, `meshing`, `ready`, `failed`, `unloading`), `min`, `max` (`Vector2`, tile rect corners in local x/z meters). |
| `ground_height_at(local_x: f64, local_z: f64) -> f64` | Terrain height (m) under a local x/z point, through the current floating origin. Returns 0 (no DEM yet). |

Ready-tile dictionary (`poll_ready`): `key` (String `z/x/y`), `ring` (int 0..3, = 16 - z), `origin` (Vector3), `mesh` (Array in `Mesh.ARRAY_MAX` layout: VERTEX, NORMAL, COLOR, TEX_UV, INDEX; empty for R3 and empty tiles), `ground` (Array in `Mesh.ARRAY_MAX` layout: VERTEX, NORMAL, COLOR, INDEX; sent on every delivery, empty for R3 and tiles without ground), `lights` (PackedVector3Array relative to `origin`), `light_colors` (PackedColorArray; rgb = average colour, a = `clamp(sqrt(count) / 8, 0.125, 1)`). Lights are filled for R2 and R3 only.

Encodings:

- Building vertex COLOR: `r` = profile id / 8 (0 residential, 1 office, 2 retail, 3 industrial, 4 civic, 5 unknown), `g` = seed 0..1, `b` = floor height / 10 m, `a` = building height / 400 m. UV = meters (u along wall, v up).
- Ground COLOR: `r` = class / 16 (0 grass/park, 1 forest, 2 water, 3 residential, 4 commercial, 5 industrial, 6 other landuse, 8 road), `g` = lit road flag, `b` = road class / 8 (0 for areas), `a` = 1.

Guarantees: refining requests all 4 children as one sibling group; every requested tile is delivered; a parent is unloaded only after its 4 children were delivered and the swap fade finished; when coarsening, the parent is delivered again before its children are unloaded. Visibility rule (evaluated by the host): see `docs/contract.md` section 5 (until P2 it lived in `.claude/agents/godot-csharp-integrator.md`).

### Rings and data

- Quadtree cover: R0 z16 < 0.6 km, R1 z15 < 2.5 km, R2 z14 < 7 km, R3 z13 < 25 km. R0 full buildings and all roads, R1 boxes, R2 merged coarse blocks, R3 light points only.
- Sources: PMTiles v3 over HTTP range requests with a disk LRU cache (300 MB cap), or the synthetic city. Layers `buildings`, `landuse`, `roads` (see `data/README.md`).
- Environment variable `SIMPLANE_PROFILE=1` prints main-thread profiling lines (to be replaced by a `Diagnostics` setting in P1).

### Godot side (test bench `low-poly-map/`)

- `WorldStreamerFacade` (typed wrapper; probes optional methods `poll_fades`, `ground_height_at`, `debug_tiles`), `StreamerBridge`, `TilePool`, `TileCover`, `TileKey`, dithered LOD fades, ground and sprite shaders, `NightRenderController`, interim `TimeOfDay` and `DaylightMenu` (sun and night factor computed in C#).

### Not built

`api_version`, `configure_world` / world profile, `set_clock` / `daylight()`, `configure_imagery`, `attributions`, geodetic camera input, `origin_shifted` signal, window shader with light profiles, DEM terrain. See `docs/spec-library-packaging.md` (phases P1-P7) and `docs/spike-streamed-osm-city.md` (M6-M8).
