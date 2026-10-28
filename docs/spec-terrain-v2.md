Source: https://claude.ai/artifact/8WFMj1Rbh6HGBFztHWqkSH (exported 2026-10-28; the Claude Doc is the living version, its "Review decisions" section wins over the text).

# Spec: Terrain v2

Oct 28, 2026 · @Aleksei

## Purpose and scope

Rework terrain so that heights are computed once per tile, shared by every consumer, selected by screen-space error and drawn as one shared grid on the GPU, with a job system that has priorities, cancellation and a memory budget. The procedural height function stays: it is the fallback that lets SimWorld build its own cities when no server data is available, and it becomes one **layer** of a height source that can also use baked DEM.

**In scope**

- Height sources as layers: base (baked DEM or procedural), procedural detail, vector edits.
- `HeightTile`: one cached height grid per terrain tile, used by the mesh, collision, ground drape, building bases and `ground_height_at`.
- SSE cover for the terrain layer with a **measured** error per tile.
- CDLOD-style rendering: one shared grid mesh, heights from a texture array, vertex morphing between levels.
- A job system with priorities, cancellation and dependency tracking (MSFS "markers"), and one byte budget for memory.

**Out of scope:** imagery (M7), vegetation, water rendering. Vector tile chunks are covered by [Spec: Tiles and LOD v2](https://claude.ai/artifact/14xnimbkBcTDsbUbo7ParR).

**Status:** proposed, 2026-10-28. **Owners:** everything under `rust/` → `rust-world-streamer`; terrain shader, texture array, `TerrainPool`, `TerrainCollision` → `godot-csharp-integrator`.

## Today

State at `api_version` 1.7 (`architecture.md` section 9).

| Part | Today | Cost |
| --- | --- | --- |
| Height | Seeded value noise in f64, a pure function of profile, centre and point | Every consumer samples it again: TIN build, ground drape, building bases, lights, cover boxes, collision |
| Mesh | Delatin TIN per tile (greedy insertion to `max_error_m × detail`) with vertical skirts | CPU-heavy on rayon; a full mesh upload per tile; skirts add fill |
| LOD | Bands T0–T4, zooms 10–14, up to 130 km, horizon culling | Fixed distances; flat land refines as much as hills |
| Transitions | Dithered fades, as for vector tiles | `discard` disables HSR on Apple GPUs; counts against `fades.max_active` |
| Consistency | `lift(band)` raises vector ground by the worst TIN error that can be visible | Roads float by up to 20 m at R3 by design |
| Collision | `TerrainCollision` samples the function into a 33 × 33 patch | A second sampler, separate from what is drawn |
| Threads | tokio (I/O), SQLite thread, rayon (CPU), one crossbeam channel back | No priorities and no cancellation inside rayon; stale jobs run to the end |

Kept: the height function and its determinism (`libm`, integer hash), the `t/z/x/y` keys, horizon culling, the 130 km range, seed-world height rules (field → water → runways).

## What MSFS and others do

MSFS splits each terrain tile into small typed pieces of work with dependencies; CDLOD shows how to draw height grids with no cracks and no per-tile meshes.

| Technique | What it does | Source | Adopted |
| --- | --- | --- | --- |
| Tile = markers | Each piece of a tile (DEM, DEM mesh, TIN, aerial, water mask, vegetation mask…) is a marker with a state machine: REQUESTED, PROCESSING, AVAILABLE, FAILED; each step decides the next one: a CPU task, GPU work or an HTTPS request | [Asobo, GDC 2022](https://www.asobostudio.com/files/inline-images/Designing_Terrain_System_Fuentes_Lionel.pdf) | Yes: job graph per terrain tile |
| Dependencies and versions | Markers depend on other markers; a new data version is processed beside the old one, swapped in, and only dependants reload | Asobo | Yes |
| Request and draw split | Request refines by screen size and measured bandwidth; draw uses the best data available, including the parent's | Asobo | Yes |
| DEM or TIN per area | An area is either DEM + aerials or a photogrammetry TIN, never both | Asobo | DEM-style grid for all terrain |
| Edits as vector data | Runways, flattening and water are vector shapes with heights and radius, turned into a distance field and merged over the original DEM, which is never edited | Asobo | Yes, for seed rules and city flattening |
| Cloud vs runtime work | Heavy, global work precomputed in the cloud; cheap local work on the client | Asobo | Yes: DEM bake vs procedural detail |
| Collision from one level | One terrain LOD (15) is read back to the CPU for collisions | Asobo | Yes, from `HeightTile` |
| Large coordinates | Doubles on the CPU; on the GPU an "anchor space" at the camera and an inverted depth buffer | Asobo | Keep today's floating origin |
| CDLOD | A quadtree of regular grids drawn from the source heightmap; vertices morph between levels by 3D distance, so seams close without skirts ([CDLOD](https://github.com/fstrugar/CDLOD), [demo](https://github.com/tschie/terrain-cdlod)) | Strugar | Yes |
| Normals from the heightmap | Rebuild the tangent basis in the vertex shader from the heightmap instead of storing it ([blog](https://longforgottenblog.blogspot.com/2014/09)) | DICE, via CDLOD users | Yes |

## Overview

The height layers feed one job queue; each terrain tile gets one `HeightTile`, and everything that needs ground height reads it instead of sampling the function again. The GPU draws all terrain from one grid mesh.

> Diagram (embedded in the Claude Doc, not exported): terrain v2 · height layers, job queue, HeightTile cache, consumers

## Design

### 1. Height source as layers

The height at a point is still one pure, deterministic function, now built from three layers:

```
h(p) = edits( base(p) + detail(p) )
```

| Layer | Source | When |
| --- | --- | --- |
| `base` | **DEM** baked into the world archive: \~1 km grid everywhere, \~30 m around airports. **Procedural** value noise (today's function) when the area has no DEM or the world is a seed world | Always one of the two |
| `detail` | Procedural octaves only **above** the base resolution (finer than 1 km, or than 30 m near airports); amplitude scaled by slope and landuse, zero on water and inside city polygons | Optional, `terrain.detail.enabled` |
| `edits` | Vector rules as a distance-field blend: field → water (flat sea and lakes) → runways (slope ≤ 1 %) → **city flattening** for seed cities | Always applied last |

- The base DEM is interpolated bicubically between DEM samples, so the function stays continuous.
- A seed city gets a polygon in `edits` that damps relief to `terrain.city_flatten.max_slope`, so procedural streets and blocks sit on usable ground.
- Determinism is kept: DEM samples are quantised integers, noise uses the integer hash and `libm`, edits are quantised inputs.

### 2. `HeightTile`: one grid, every consumer

- Per terrain tile `t/z/x/y`: a regular grid of `N + 3` samples per side (`N` = 64 quads, one shared edge, one border sample on each side for normals and morphing), stored as f32 relative to the tile's minimum height, plus `min_h`, `max_h` and the measured `error_m` (section 3).
- Computed once by a worker, cached in the terrain LRU, then shared by:
  - the GPU height texture (section 4);
  - `ground_height_at` and AGL: bilinear on the finest **resident** `HeightTile`, so the reported ground matches the drawn surface;
  - ground drape and building bases of vector chunks: they sample the `HeightTile` of the matching level, which shrinks `lift(band)` to the remaining morph error;
  - `TerrainCollision`: the patch is copied from `HeightTile` instead of re-sampling the function;
  - cover boxes (`min_h`, `max_h`).
- A child shares every second sample with its parent; the worker copies those instead of evaluating them (about a quarter of the samples).

### 3. LOD by screen-space error with a measured error

- When a `HeightTile` is built, the worker measures `error_m`: the largest vertical difference between this tile's grid and the function sampled at twice the density. That is the error of drawing this tile instead of its children.
- Before a tile is built, its error is estimated as the parent's error / 2.
- Refine when `SSE = error_m · H / (2 d tan(fov_y / 2)) > τ_terrain`, same formula and hysteresis as Tiles v2; `d` is the slant distance to the tile box.
- Flat land (most of Lithuania) has small errors and stays coarse; hills refine where they are. Horizon culling and the 130 km limit stay.

### 4. Rendering: one shared grid (CDLOD)

- **One mesh** for all terrain: a regular grid of `N × N` quads, created once.
- **One texture array**: each resident tile owns a layer of a `Texture2DArray` (R16, heights quantised to `min_h..max_h`; 256 layers × 67² × 2 B ≈ 2.3 MB). Godot updates a single layer with `ImageTextureLayered.update_layer`.
- **One draw call**: all visible tiles are instances of one `MultiMeshInstance3D`. Per instance: transform (origin, size) and custom data (layer index, `min_h`, `max_h`, morph range).
- **Vertex shader:** sample the height, morph each vertex toward the parent grid position across the last 20 % of its refine range (from the SSE threshold), rebuild the normal from neighbouring samples, then the existing earth-curve drop.
- Morphing closes seams between levels, so **skirts and dithered fades are removed** for terrain; it no longer counts against `fades.max_active`.
- Upload per tile becomes one texture layer of \~9 KB instead of a mesh.

### 5. Jobs and threads (markers)

**Job kinds per terrain tile**

| Job | Runs on | Depends on | Output |
| --- | --- | --- | --- |
| `DemFetch` | tokio (I/O) | — | Raw DEM bytes (only when `base` = DEM) |
| `HeightBuild` | worker pool | `DemFetch` or none; parent `HeightTile` if resident | `HeightTile`, `error_m` |
| `CollisionPatch` | worker pool | `HeightTile` | Collision heights |
| `Drape` | worker pool | `HeightTile` + vector `DataTile` | Ground and building bases for vector chunks |
| `Upload` | main thread | `HeightTile` | Texture layer + instance data |

**Scheduler**

- Each job is a small state machine: `Requested → Waiting(deps) → Ready → Running → Done | Failed`, with a version = hash(terrain profile, edits version, DEM version). A new version is built beside the old one and swapped in; dependants reload (MSFS marker reload).
- **Priority queue** in front of the workers: rayon has no priorities, so a binary heap of ready jobs, keyed by (on-demand before prefetch, SSE priority), feeds a fixed pool of worker threads.
- **Cancellation:** every job carries the tile's generation number; a worker checks it before starting and every few grid rows, and a stale result is dropped without delivery.
- **Worker count:** desktop = cores − 2; iOS = performance cores − 1, prefetch jobs at a lower QoS (verify on device).
- The main thread still uploads within its 2 ms budget; terrain uploads are now small enough to fit many per frame.

### 6. Memory

- **One byte budget** for everything SimWorld holds: decoded data tiles (Tiles v2), `HeightTile`s, collision patches and GPU layers. Eviction by priority (distance and SSE), not by count. Defaults: 512 MB desktop, 256 MB iOS.
- **Pools, no steady-state allocation:** fixed-size height buffers from a free list; texture-array layers and MultiMesh instance slots as fixed pools.
- **Layout:** structure-of-arrays, f32 relative to the tile, f64 only for tile origins.
- **Sizes:** a `HeightTile` is 67² × 4 B ≈ 18 KB; 400 resident tiles ≈ 7 MB of CPU memory.

### 7. SIMD and determinism

- `HeightBuild` evaluates rows four points at a time with the stable `wide` crate (f64x4).
- Value noise needs only add, multiply, floor and the integer hash, so the SIMD path gives bit-identical results to the scalar one; no fused multiply-add anywhere in the height path.
- Golden height hashes must stay identical; a test compares the SIMD and scalar paths on random points.

## Profile keys and contract

```json
"terrain": {
  "render": "grid",
  "grid_quads": 64,
  "tau_px": 3.0,
  "morph_ratio": 0.2,
  "max_distance_m": 130000,
  "base": { "source": "auto", "dem_level": "dem" },
  "detail": { "enabled": true, "octaves": 4, "amplitude_m": 6.0, "zero_on": ["water", "city"] },
  "city_flatten": { "max_slope": 0.03, "blend_m": 300 }
},
"jobs": { "workers": "auto", "prefetch_qos": "utility" },
"memory": { "budget_mb": { "desktop": 512, "ios": 256 } }
```

- `render`: `"grid"` (this spec) or `"tin"` (today's path, kept until G6 for comparison).
- `base.source`: `"auto"` uses the archive's DEM where present and procedural elsewhere; `"procedural"` forces today's function (seed worlds, offline own cities); `"dem"` fails the area with no DEM.
- Validation: `grid_quads` a power of two in 16..128; `morph_ratio` in 0.05..0.5; `tau_px` in 0.5..64.
- `profile_version` + 1.

**Contract impact**

| Item | Change | Kind |
| --- | --- | --- |
| Terrain ready record | With `render: "grid"`: new keys `height` (PackedByteArray, R16, `(N+3)²`), `min_h`, `max_h`, `error_m`, `grid_quads`; `mesh` is empty. With `"tin"`: unchanged | Additive (minor) |
| `terrain_grid_mesh()` | New: returns the shared grid arrays once | Additive |
| `ground_height_at` | Reads the finest resident `HeightTile`; falls back to the function where none is resident. Results can differ from today by up to the drawn error | Behaviour change, documented |
| `poll_fades` | No terrain entries with `render: "grid"` | Behaviour change |
| `debug_stats()` | `jobs_queued`, `jobs_cancelled`, `height_tiles`, `memory_mb` by pool | Additive |
| Archive | Optional DEM level (`simworld_levels.dem`), quantised int16 grids | Additive |

- The terrain chunk keys `t/z/x/y` stay; the zoom range is no longer fixed to 10–14 but follows the SSE cover.
- Removing `"tin"` (phase G6) is a major change and lands with the next major version.
- Every change updates `docs/contract.md`, `API_MINOR`, `CHANGELOG.md`, `default.json`, the schema and both agent files in the same commit.

## Review decisions

The code review on 2026-10-28 (Rust and Godot sides) changed the spec as follows; where this section and the text above disagree, this section wins.

| # | Decision | Why |
| --- | --- | --- |
| T1 | `ground_height_at`, the vector drape and building bases stay on the exact height function; the grid is sunk in the shader by a continuous `sink(d)` that is equal on both sides of a seam | Reading the finest resident HeightTile makes heights depend on load history and breaks contract sections 2, 9a, 11, the airport 1 cm rule, AGL and the seed self-tests; the TIN is already sunk since 1.7, so the "roads float 20 m" row is out of date |
| T2 | Collision samples the exact function, filled through a new additive `height_patch(cx, cz, n, spacing)` (one call instead of 1089) | Same surface as `ground_height_at`; no second sampler |
| T3 | Terrain SSE uses a pure, memoized per-key error proxy, with an error floor (or minimum zoom) on tiles touched by an edit | Measured error with parent / 2 makes the cover depend on history and misses small lakes and strips; a pure proxy keeps a golden cover |
| T4 | `detail.enabled` defaults to false and stays off in seed worlds | Detail inside the field moves runway thresholds and lake levels and every terrain / seed golden |
| T5 | One byte budget (`memory.budget_mb`) for the core's pools replaces `data.decoded_lru_tiles` of Tiles v2; GPU layers are capped by `terrain.max_tiles`, not by the budget | The core cannot see GPU memory; the 256-layer array needs a tile cap with τ backing off |
| T6 | `set_terrain_detail` becomes a τ scale for the terrain cover (behaviour change, minor) | Under SSE there is no per-tile detail to re-mesh in place |
| T7 | Separate versions: Tiles v2 at api 1.10 (profile 7), the grid record at 1.11 (profile 8); TIN removal is 2.0 | Smaller releases, each testable |
| T8 | The airport 1 cm guarantee is kept: forced refinement over strips and a lift-weight channel so the sink is 0 inside fences | Contract section 9 "Airports" |
| T9 | One MultiMesh per terrain ring (5 draw calls, engine culling) instead of one | Godot culls a MultiMesh only as a whole; per-instance host culling costs CPU; acceptance becomes 5 draw calls |
| T10 | The host owns the neighbour stitch (per-edge coarser-neighbour flags in the instance colour), plus a short time-based morph-in on late swaps and a micro-skirt ring until the crack test passes | Distance morph alone cracks beside a coarser neighbour and pops on late arrivals |
| T11 | Keep the faceted flat-shaded look (normals from derivatives) | Today's low-poly style; heightmap normals only for the "smooth" option |

Further corrections from the review:

- Add a height golden (plain and seed world, about 10⁴ points) and freeze `band_error_rule`, `drape_margin_rule` and `foundation_rule` to a reference table **before** any refactor; no height golden exists today.
- HeightTile keeps the f64 grid for parent copies (f32 relative to `min_h` is not the function's value); `error_m` at 2× density would cost the children's samples, so it is dropped with T3.
- R16 uses one world-wide quantum from `Terrain::bounds()` (about 1.5 cm at amplitude 450), not a per-tile min..max, so shared samples quantise the same on both sides; R32F only if a world's range needs it.
- The grid mode is requested by the host (like `layers`), not only by the profile; reuse `h_min` / `h_max`; new ready-record key `class_map` (seed class and tint, second texture array); new profile key `terrain.max_tiles`.
- `far_zoom..near_zoom` stay as SSE bounds, so `t/` keys and ring parsing are unchanged.
- One job scheduler (priority heap, fixed pool, generation cancel, dependencies) replaces rayon for both the vector and the terrain layer, and lands before Tiles v2 T2. iOS QoS needs its own thread group.
- SIMD (G5) only if profiling shows the noise dominating: the libm `sin_cos` / `atan(sinh)` per point are separable per row and column and give most of the gain without SIMD; `wide::mul_add` is forbidden.
- G1 needs a 1-day host spike first: R16 `update_layer` cost on Metal, `texelFetch` on `sampler2DArray` in the Mobile vertex stage, f32 instance custom data.

**H0 spike (2026-10-28, M2 Pro, Godot 4.7.2 Mobile / Metal): GO.** R16 UNORM texture array (256 × 67², 2.2 MiB) reads back bit-exact through `texelFetch` on `sampler2DArray` in the vertex stage; RH is a no-go (15.6 q-unit error). `update_layer` costs about 0.015 ms CPU per layer, but a frame with any update ran about 1.5 ms slower on a GPU-bound scene (fixed cost, not per layer; batch uploads into few frames). `INSTANCE_CUSTOM` and `COLOR` arrive as full f32, unclamped. 300 tiles of 64² quads (2.46 M triangles) cost about 1.06 ms GPU, 20 % less than one static mesh; 1 vs 5 MultiMeshes makes no difference, so T9 (one per ring) stands. Metal has no GPU timer in this build: measure by frame-time slope or an Xcode capture. iPhone not measured.

## Tests, acceptance and phases

**Tests**

- Height: the layered function with `base = procedural` and detail and city flattening off returns today's golden heights bit for bit; SIMD and scalar paths agree on 10⁶ random points; a DEM base is continuous across DEM cells and tile edges.
- `HeightTile`: shared border samples equal the neighbour's; samples copied from the parent equal the function; `error_m` equals a brute-force check at 4× density within 1 %.
- Cover: a flat synthetic area refines less than a hilly one at the same distance; hysteresis holds the cover steady when circling; SSE falls when `fov_y` widens.
- Jobs: a tile leaving the wanted set cancels its queued and running jobs (counter rises, nothing is delivered); on-demand jobs overtake prefetch; dependants of a new edits version reload and nothing else does.
- Memory: steady-state flight allocates no new height buffers (pool counters); the byte budget is never exceeded; eviction removes the lowest-priority items first.
- Godot: `--terrain-selftest` with `render: "grid"`; morph closes seams (no gaps in a top-down depth readback at level borders); `--terrain-collision-selftest` reads `HeightTile` data.

**Acceptance**

- [ ] No cracks and no visible popping at any altitude from 0 to 12 km, at 250 m/s.
- [ ] Terrain draw calls: 1 (plus overlays).
- [ ] CPU time per terrain tile at least 5× lower than the TIN path on the dev Mac (measure with `SIMPLANE_PROFILE=1`).
- [ ] Main-thread terrain upload ≤ 0.2 ms per tile.
- [ ] `ground_height_at` matches the drawn surface within 0.1 m at the finest resident level.
- [ ] Memory stays within `memory.budget_mb`; no allocations in steady flight.
- [ ] Golden heights unchanged with the procedural base.

**Phases**

| Phase | Work | Visible change | Owner |
| --- | --- | --- | --- |
| G1 | `HeightTile`, pools, byte budget, priority job queue with cancellation; consumers read `HeightTile` | None | `rust-world-streamer` |
| G2 | SSE cover for terrain with measured `error_m` | Fewer tiles on flat land | `rust-world-streamer` |
| G3 | Grid render path: shared mesh, texture array, MultiMesh, morph shader, normals; behind `render: "grid"` | New render path | both |
| G4 | Height source layers: DEM base in the archive, detail octaves, city flattening | Real relief where DEM exists | `rust-world-streamer` |
| G5 | SIMD height evaluation | Faster builds | `rust-world-streamer` |
| G6 | Remove TIN, skirts and terrain fades (next major) | — | both |

**Open questions**

- Cost of `ImageTextureLayered.update_layer` and of vertex texture fetch on the Mobile renderer with Metal: measure on the dev Mac in G3, on iPhone 17 later.
- R16 precision: a 300 m tile range gives \~5 mm steps; tiles with larger ranges (mountains in other regions) may need R32F.
- Whether the procedural `detail` should also run on top of a 30 m DEM near airports, or stay off there.
- Merging the terrain job queue with the vector-tile workers into one scheduler, or keeping two pools.

## Sources

- [Asobo, GDC 2022: Designing the Terrain System of Flight Simulator](https://www.asobostudio.com/files/inline-images/Designing_Terrain_System_Fuentes_Lionel.pdf)
- [Filip Strugar: CDLOD paper and source](https://github.com/fstrugar/CDLOD)
- [terrain-cdlod: a minimal CDLOD implementation](https://github.com/tschie/terrain-cdlod)
- [Normals from the heightmap in the vertex shader (CDLOD in practice)](https://longforgottenblog.blogspot.com/2014/09)
- [Spec: Tiles and LOD v2](https://claude.ai/artifact/14xnimbkBcTDsbUbo7ParR)
