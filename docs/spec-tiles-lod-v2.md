Source: https://claude.ai/artifact/14xnimbkBcTDsbUbo7ParR (exported 2026-10-28; the Claude Doc is the living version, its "Review decisions" section wins over the text).

# Spec: Tiles and LOD v2

Oct 28, 2026 · @Aleksei

## Purpose and scope

Rework how SimWorld loads data and picks detail so that every building is downloaded once, far fewer requests go out, and the level of detail follows what is actually visible on screen. Client-side extrusion of OSM footprints stays: it is the cheapest way to get buildings in bytes, and MSFS and Streets GL build them the same way.

Three changes:

1. **A data tile is not a render chunk.** Two data levels (`detail` z14, `overview` z11) are fetched and decoded once; render chunks z13–z16 are cut from them in memory.
2. **LOD by screen-space error (SSE)** instead of fixed distance bands. One threshold in pixels drives the whole cover and adapts to field of view, viewport size and camera zoom.
3. **Generalisation moves to the bake**: block merging, minimum areas, height quantisation and simplification per data level.

**Unchanged for the host:** chunk keys `z/x/y`, the visibility rule, sibling swap, `poll_fades`, dithered fades and the ready-tile dictionary. The work is inside `simworld-core` and in the tile build.

**Out of scope:** the terrain layer (`t/z/x/y`, bands T0–T4) keeps its own cover; moving it to SSE is a separate task. The seed world keeps its generator.

**Status:** proposed, 2026-10-28. **Owners:** tile profile, `simworld-core` and everything under `rust/` → `rust-world-streamer`; `set_view` call, settings and self-tests in the addon and bench → `godot-csharp-integrator`.

## Today

State at `api_version` 1.7. The costs come from one tile being both the unit of download and the unit of LOD.

| Problem | Why it costs |
| --- | --- |
| Each band fetches its own zoom: R0 z16, R1 z15, R2 z14, R3 z13 | One building arrives in z14, z15 and z16, up to three times, plus z13 for lights |
| R0 on z16 | Over the area of one z14 tile (≈ 1.4 km in Vilnius) up to 16 + 4 + 1 = 21 requests instead of one; on mobile, latency costs more than bytes |
| R2 blocks are merged on the client from z14 footprints | The phone downloads every footprint just to glue them into blocks |
| Bands at fixed distances (0.6 / 2.5 / 7 / 25 km) | Ignore field of view, resolution and camera zoom: a narrow cockpit FOV and an iPhone get the same tiles as a wide FOV on 4K |
| No single load knob | Lightening the scene means moving four edges and a hysteresis |
| Empty children are requested as a group | If empty tiles go to the network, those are wasted requests; the PMTiles directory already says a tile does not exist |

Kept as is: the Web Mercator quadtree, slant distance to the tile box, look-ahead, sibling swap without holes, dithered fades capped at 4, horizon culling for terrain.

## What others do

Every technique below is established in a standard or a shipping product; the spec adopts them rather than inventing new ones.

| Technique | What it does | Where | Adopted |
| --- | --- | --- | --- |
| Geometric error + SSE | A tile carries the error in metres if it is drawn without its children; the client converts it to pixels (SSE) to decide whether to refine ([3D Tiles](https://github.com/AnalyticalGraphicsInc/3d-tiles/blob/master/specification)) | 3D Tiles, Cesium (default threshold 16 px, [Cesium](https://pointcloud.ucsd.edu/CCAS/Shiqmim/libs/Cesium/Build/Documentation/Cesium3DTileset.html)) | Yes |
| LOD from screen size and network | Requested LOD depends on estimated screen size and measured bandwidth ([Asobo, GDC 2022](https://www.asobostudio.com/files/inline-images/Designing_Terrain_System_Fuentes_Lionel.pdf)) | MSFS | Yes |
| REPLACE and ADD refinement | Children either replace the parent or add to it ([3D Tiles](https://github.com/gijs/3d-tiles/blob/master/README.md)) | 3D Tiles | Both: REPLACE for ground and blocks, ADD for landmarks |
| Knowing empty tiles | Availability bitstreams stop the client from requesting tiles without content ([Geodan](https://research.geodan.nl/3d-tiles-1-1-implicit-tiling/)) | 3D Tiles 1.1 | Yes, from the PMTiles directory |
| Block merging at low zoom | Close buildings merge into one block polygon at z13; expensive, so done at tile build ([planetiler](https://www.github.com/onthegomap/planetiler/blob/main/config-example.properties)) | OpenMapTiles, planetiler | Yes, in the bake |
| Minimum feature size | Features smaller than a size in tile pixels are not emitted ([planetiler](https://www.github.com/onthegomap/planetiler/blob/main/planet-logs/v0.7.0-planet-c3d-standard-180-no-z13-building-merge.txt)) | planetiler | Yes |
| Height quantisation | Heights rounded at mid zooms so more buildings merge and tiles shrink ([Protomaps](https://classic.yarnpkg.com/en/package/protomaps-themes-base)) | Protomaps | Yes |
| Foveation, cull requests while moving | Centre of the screen first; skip requests that will be stale on arrival ([Cesium](https://cesium.com/downloads/cesiumjs/releases/1.84/Build/Documentation/Cesium3DTileset.html)) | Cesium | Yes, rear hemisphere and external camera |
| Skip levels | On a fast zoom-in, load the needed level directly ([Cesium](https://help.agi.com/STKWebVisualizationLibrary/Cesium3DTileset.html)) | Cesium | After teleport |
| Draw the best available | While children are missing, the parent is drawn ([Asobo](https://www.asobostudio.com/files/inline-images/Designing_Terrain_System_Fuentes_Lionel.pdf)) | MSFS | Already in place |

## Overview

Download and LOD are separate: two data levels are fetched and decoded once, and four levels of render chunks are built from them in memory. R3 and R2 come from `overview`, R1 and R0 from `detail`.

> Diagram (embedded in the Claude Doc, not exported): data levels and render chunks · 2 data levels, 4 LOD levels

## Design

### 1. Two data levels

| Level | Zoom | Tile size in Vilnius | Content | Feeds |
| --- | --- | --- | --- | --- |
| `detail` | z14 | ≈ 1.4 km | Every footprint in the bake area, roads, landuse, light cells | R0, R1 |
| `overview` | z11 | ≈ 11 km | Merged blocks, major roads (≥ secondary), water, coarse landuse, aggregated light cells, landmarks | R2, R3 |

- One `overview` tile covers 64 `detail` tiles.
- Every building travels **once**: as a footprint in `detail`, or as a landmark in `overview`. Blocks in `overview` are derived data, not copies.
- The archive stays one `.pmtiles`; the two levels are its only zooms. Metadata key `simworld_levels: {"detail": 14, "overview": 11}`.

### 2. Render chunks are cut in memory

- Chunk keys stay `z/x/y`, z13–z16. The host cannot tell them from today's tiles.
- R0 (z16) and R1 (z15) chunks are clipped from the decoded `detail` z14 tile, exactly to the chunk rectangle, as today. R2 (z14) and R3 (z13) chunks are built from `overview`.
- New in-memory LRU of **decoded** data tiles (`DataTile`: buildings, roads, landuse, cells as Rust structs, not MVT bytes). Decode once, mesh many chunks. The disk cache keeps raw bytes as today.
- A chunk job waits on its data tile; the scheduler requests the data tile once per group of chunks that need it.

### 3. LOD by screen-space error

Each chunk level has a geometric error `e`: the metres lost if this chunk is not refined to the next level. A chunk refines when

```latex
\mathrm{SSE} = \frac{e \cdot H}{2\, d \tan(\mathrm{fov}_y / 2)} > \tau
```

with `d` today's slant distance to the chunk box, `H` the viewport height in physical pixels, `fov_y` the vertical field of view and `τ` the threshold in pixels.

| Chunk | Lost without refinement | `e` (start) | Refine distance at 1080 px, 60°, τ = 4 |
| --- | --- | --- | --- |
| R1 → R0 | 2 m simplification, roof detail | 2.5 m | ≈ 0.6 km |
| R2 → R1 | Block merging, 5 m height steps | 10 m | ≈ 2.3 km |
| R3 → R2 | Whole buildings (lights only) | 30 m | ≈ 7 km |

- With these start values a 1080p, 60° view reproduces today's 0.6 / 2.5 / 7 km bands; a narrow cockpit FOV, an iPhone viewport or a camera zoom shift them on their own.
- Hysteresis: refine at SSE > τ, coarsen at SSE < τ / 1.15 (today's 15 %).
- `bands.max_distance_m` (25 km) stays as the outer limit.
- **One load knob, `τ`.** It is scaled by the quality setting, by the frame budget (dynamic, as in Cesium) and by measured bandwidth (as in MSFS): `τ_eff = τ × quality × frame_scale × net_scale`, each factor clamped and smoothed over \~2 s.
- The host sends the view with a new call `set_view(fov_y_deg, viewport_h_px)`; until it is called, defaults of 60° and 1080 px apply.

### 4. Refinement: REPLACE and ADD

- **REPLACE** (unchanged): ground, blocks, buildings. Sibling swap and dithered fades as today.
- **ADD** for landmarks: buildings ≥ 60 m and entries in `buildings.overrides` live in `overview` with full geometry and are excluded from `detail`. They show from 25 km, never pop on a level change and take no part in fades.

### 5. Requests

- **Empty tiles without network:** check the PMTiles directory before a request; no entry → deliver an empty chunk at once (contract guarantee 2 still holds).
- **Priority:** SSE × direction weight. The rear hemisphere uses `τ × lod.rear_tau_scale` (2.0); with the external camera, foveation as in Cesium.
- **Cull while moving:** a data tile that will leave the wanted set before it can arrive (speed × latency estimate from `latency`) is not requested.
- **After teleport:** `overview` first, then the `detail` tile under the camera: at most two fetch rounds with two data levels.

### 6. Bake (planetiler profile)

| Step | `detail` z14 | `overview` z11 |
| --- | --- | --- |
| Footprint simplification | 0.5 m | 5 m after merging |
| Minimum area | 20 m² per building | 2000 m² per block |
| Merge close buildings | no | yes, 3–5 m buffer |
| Heights | as tagged | rounded to 5 m |
| `building:part` | yes | no |
| Landmarks (ADD) | excluded | full geometry |
| Roads | all classes used today | ≥ secondary |

Extrusion, roofs, windows, lights and fades stay on the client: they depend on the camera, the hour and the profile.

## Profile keys and contract

```json
"lod": {
  "tau_px": 4.0,
  "hysteresis": 0.15,
  "error_m": { "r1": 2.566, "r2": 10.69, "r3": 29.94 },
  "rear_tau_scale": 2.0,
  "adaptive": {
    "enabled": true,
    "frame_budget_ms": 33.3,
    "min_scale": 0.5,
    "max_scale": 3.0,
    "smoothing_s": 2.0
  }
},
"bands": { "max_distance_m": 25000 },
"data": { "decoded_lru_tiles": 24 }
```

- `bands.r0_edge_m`, `bands.r1_edge_m`, `bands.r2_edge_m` are removed; `bands.max_distance_m` stays.
- `error_m.rN` is the error of the coarser chunk on the step to the next finer one (`r1` = the R1 chunk's error against R0).
- Validation: `tau_px` in 0.5..64; errors strictly increasing `r1 < r2 < r3`; `min_scale ≤ 1 ≤ max_scale`; `decoded_lru_tiles` ≥ 4.
- `profile_version` + 1.

**Contract impact**

| Item | Change | Kind |
| --- | --- | --- |
| `set_view(fov_y_deg: float, viewport_h_px: int)` | New method; defaults 60° and 1080 px until called | Additive (minor) |
| Chunk keys, visibility rule, guarantees 1–5, `poll_fades`, ready-tile dictionary | Unchanged | — |
| `debug_stats()` | New fields `tau_eff`, `data_tiles_loaded`, `data_tiles_decoded`, `requests_skipped_empty`, `requests_skipped_moving` | Additive |
| Tileset contract | Zooms 11 and 14 only, `simworld_levels` metadata; an archive without it is refused (`status() = error`) | Breaking for archives, not for the API |
| Profile 3.2 | `lod.*`, `data.decoded_lru_tiles`; band edges removed | `profile_version` + 1 |

- The version is the next free minor in the contract's version table (after the ones reserved for Daylight and imagery).
- **Light Zones** must follow: cells move to `detail` (16 × 16 z18 cells per z14 tile) and to an aggregated form in `overview`. Update `spec-light-zones.md` before its phase L1.
- Every change updates `docs/contract.md`, `API_MINOR`, `CHANGELOG.md`, `default.json`, the schema, `data/schema/simplane.yml` and both agent files in the same commit.

## Review decisions

The code review on 2026-10-28 changed the spec as follows; where this section and the text above disagree, this section wins.

| # | Decision | Why |
| --- | --- | --- |
| D1 | Default errors 2.566 / 10.69 / 29.94 m (edge × 2·tan 30° × 4 / 1080) | The round 2.5 / 10 / 30 give 585 / 2338 / 7015 m, not today's bands; the exact values keep `GOLDEN_COVER` as the equivalence test |
| D2 | Keep a compatibility path: an archive without `simworld_levels` is read as detail 14 / overview 13 until R2/R3 come from overview | Data tiles and SSE can land and be tested on today's Vilnius archive |
| D3 | Bake with a Rust `simworld-cli bake`, not the planetiler YAML | YAML cannot do a buffered polygon merge; one language, shared tests (P6 already plans `bake`) |
| D4 | Landmarks are a third layer `l/z/x/y` (root zoom = leaf zoom, always visible, never in `poll_fades`) | A landmark inside a chunk would vanish or fade with it; contract change, the ready-tile dictionary is not unchanged |
| D5 | The synthetic city emits both data levels | Tests the real path; `GOLDEN` moves once |
| D6 | `bands.r3_edge_m` is renamed `bands.max_distance_m`; a profile\_version 7 profile with edge keys is refused, older profiles migrate (r3 → max distance, r0–r2 dropped with a warning). `bands.hysteresis` (×1.15) and the look-ahead keys stay; `lod.hysteresis` is dropped | There is no `max_distance_m` today; terrain and look-ahead use `bands.hysteresis` |
| D7 | `set_view(fov_y_deg, viewport_h_px, forward)` carries the view direction | Rear-hemisphere τ and foveation need it; velocity is wrong when looking around |
| D8 | F3 panel: the four edge sliders become a live τ / quality slider (new `set_lod_quality(x)`), a max-distance row and a read-only refine-distance readout | One load knob; `configure_world` is a teleport, so a live knob needs its own call |

Further corrections from the review:

- Overview carries one light point per building (id, kind, height, state tags) instead of baked light cells, so profile colours, the dark profile and `buildings.overrides` stay runtime data.
- The terrain drape (`terrain::band_error_rule`) uses fixed reference edges (1080 px, 60°, profile τ), never the live SSE edges, so meshes stay deterministic.
- Adaptive τ gets the frame time from the host (`report_frame_ms(ms)`, max of CPU and GPU time); self-tests force `lod.adaptive.enabled = false` and `--view=60,1080`.
- Host: H is physical output pixels (`WorldRenderSettings.LodPixelScale` for the iPhone question); `--area=a` becomes τ / a plus max distance × a; the window/sprite blend and the street-pool fade follow the R2→R1 refine distance.
- Contract version: 1.10 (1.8 and 1.9 are reserved for Daylight and imagery). `docs/spec-light-zones.md` does not exist yet.

From the Terrain v2 review: `data.decoded_lru_tiles` is replaced by the shared byte budget `memory.budget_mb`, and data-tile and chunk jobs run on the one job scheduler (priorities, cancellation, dependencies) that lands before T2.

**T1a measurement (2026-10-28, vilnius.pmtiles, 20 km flight W→E at 300 m, default bands):** today 1149 requests / 18.98 MB; v2 with overview z11 107 requests / 5.5 MB (10.7× fewer requests, 3.4× fewer bytes); with z12 177 / 5.6 MB.

- **Overview is z11** (33 requests vs 103 at z12 for the same bytes; worst tile 442 KB, under 500 KB).
- **Detail stays z14** (largest tile 109 KB stored); extent 4096 gives 0.345 m per unit, coarser than today's R0 at z16 (0.086 m): consider extent 8192 for detail.
- **Landmarks get their own extent** (16384, 0.69 m at z11); only 9 buildings in the archive have a roof of 60 m or more.
- **Decoded tiles held at once: about 45** (25–28 overview + 16–18 detail); the byte budget must hold them.
- **Decided: the overview block minimum drops to about 150–200 m² (T1b measures the bytes); the 2000 m² minimum dropped 87 % of buildings** (detached houses do not merge), so R2 would show suburbs as lights only. Light points are about 60 % of overview bytes (about 7 B per building, mostly the OSM id).

**T1b bake (2026-10-28, `simworld-cli bake`, from vilnius.pmtiles):** 19.0 MB against 63.9 MB today. Detail z14 (extent 8192): 1518 tiles, 14.7 MB, max 115 KB. Overview z11: 36 tiles, 4.3 MB, p95 459 KB, **max 757 KB** (above the 500 KB guide; watch first-fetch latency, z12 stays the fallback). Block minimum 150 m²: 45 % of buildings by count, 78 % by footprint area end up in blocks; the rest show as light points at R2/R3. Light points are 43 % of overview bytes (the OSM id moved to the feature id). 18 landmarks. Overview tiles mix extents (landmarks 16384, other layers 4096): the runtime decoder must read the extent per layer.

## Tests, acceptance and phases

**Tests**

- Bake: every building id appears in exactly one level; landmarks only in `overview`; `overview` blocks cover the merged footprints within the buffer; minimum areas and height steps applied; metadata carries `simworld_levels`.
- `simworld-core`: an R0 chunk cut from `detail` z14 matches today's z16 tile mesh within the simplification tolerance (synthetic city); four R0 siblings cover exactly their parent R1 chunk; a data tile is decoded once for all chunks that use it (counter in `debug_stats`).
- Cover: at 1080 px, 60°, τ = 4 the SSE cover equals today's band cover for the fixed golden cameras; halving `fov_y` moves every refine distance out by about the factor `tan(30°) / tan(15°)`; hysteresis holds the cover steady when circling at a band edge.
- Requests: no network request for a tile absent from the PMTiles directory; a tile that would arrive after leaving the wanted set is not requested; the rear hemisphere refines at `τ × rear_tau_scale`.
- Adaptive τ: a simulated frame time over budget raises `tau_eff` within `max_scale`, smoothed; back under budget it returns.
- Profile: new keys load; removed band edges are refused in a current-version profile; bad ranges fail validation.
- Godot: `--cover-selftest` green with `set_view`; headless smoke test green.

**Acceptance**

- [ ] At 1080 px, 60°, τ = 4, screenshots at the golden points match today's.
- [ ] On a fixed flight over Vilnius, requests fall by at least 10× and building bytes by at least 2× against `api_version` 1.7.
- [ ] No holes or pop-in at 250 m/s and 300 m AGL.
- [ ] Main-thread upload time and frame time are no worse than today.
- [ ] On iPhone 17 (later), the same flight stays within the iOS budgets of the spike spec.

**Phases**

| Phase | Work | Contract | Owner |
| --- | --- | --- | --- |
| T1 | planetiler profile with two levels, merging, quantisation, landmarks; measure sizes against today's archive | tileset | `rust-world-streamer` |
| T2 | Split data tiles from chunks, decoded LRU, chunks z13–z16 from data levels; golden tests | — | `rust-world-streamer` |
| T3 | SSE cover, `set_view`, `lod.*` keys; equivalence check at 1080p / 60° | minor + profile | both |
| T4 | Empty tiles from the directory, priority, cull while moving, rear relaxation | `debug_stats` | `rust-world-streamer` |
| T5 | Adaptive τ from frame budget and bandwidth | — | both |

**Open questions**

- `overview` at z11 or z12: decide from T1 tile sizes.
- If a dense-core `detail` z14 tile exceeds \~500 KB, move `detail` to z15.
- τ on iPhone in physical or logical pixels: check on the device.
- Terrain cover (`t/z/x/y`, T0–T4) on SSE: separate spec.

## Sources

- [3D Tiles specification: geometricError and refine](https://github.com/AnalyticalGraphicsInc/3d-tiles/blob/master/specification)
- [3D Tiles README: ADD and REPLACE](https://github.com/gijs/3d-tiles/blob/master/README.md)
- [Geodan: implicit tiling and availability bitstreams](https://research.geodan.nl/3d-tiles-1-1-implicit-tiling/)
- [Cesium3DTileset: maximumScreenSpaceError, cullRequestsWhileMoving](https://pointcloud.ucsd.edu/CCAS/Shiqmim/libs/Cesium/Build/Documentation/Cesium3DTileset.html)
- [Cesium3DTileset: foveation and dynamic SSE](https://cesium.com/downloads/cesiumjs/releases/1.84/Build/Documentation/Cesium3DTileset.html)
- [Cesium3DTileset: skip level of detail](https://help.agi.com/STKWebVisualizationLibrary/Cesium3DTileset.html)
- [Asobo, GDC 2022: Designing the Terrain System of Flight Simulator](https://www.asobostudio.com/files/inline-images/Designing_Terrain_System_Fuentes_Lionel.pdf)
- [planetiler: building merge at z13](https://www.github.com/onthegomap/planetiler/blob/main/config-example.properties)
- [planetiler: minimum feature size](https://www.github.com/onthegomap/planetiler/blob/main/planet-logs/v0.7.0-planet-c3d-standard-180-no-z13-building-merge.txt)
- [Protomaps: building height quantisation](https://classic.yarnpkg.com/en/package/protomaps-themes-base)
