# Spec: Terrain (landscape relief)

Status: **built as the procedural design below** (phase 1 api 1.4, terrain mesh layer + Godot side api 1.5, `terrain_center` / `terrain_active` api 1.6; the DEM sections are the superseded proposal and are not built). Original target: the next free additive minor after the M6/M7 entries of `docs/contract.md` "Version plan" (1.3 if it lands after 1.0 night windows, 1.1 clock and 1.2 imagery; it takes whatever minor is free when it lands) and `profile_version` 5 (4 is the night-windows key set). Milestone name: **M9 Terrain** (M7 Imagery and M8 Measure keep their numbers; M9 can run before them).

**Design change: procedural, no data (supersedes parts of this spec; phase 1 built, api 1.4, profile_version 5).** Terrain is not read from a DEM archive: heights come from a seeded, pure function (`simworld-core/src/terrain/`: 3D integer-hash value noise on the Earth sphere, fractal hills, ridged mountain chains, plateaus, valleys, a sea level, a flat zone around the configure origin), the way the synthetic city is generated. Superseded and dropped: sections 3.1-3.3 (sources, encodings, build tooling, runway flattening from a DEM, Copernicus), 4.1 archive / `terrain_url` / `terrain_encoding` / `terrain_attribution` / terrain attribution / terrain disk cache, 4.2 residency / LRU / coarse field (`heightfield_cache_mb`, `coarse_zoom`, `cache_cap_mb`), 4.3 DEM fetch / fallback / timeouts (`dem_zoom`, `dem_fallback_timeout_s`, `synthetic_amplitude_m`; the "first delivery may be a fallback" caveat no longer applies: terrain output is final on first delivery). Kept and built in phase 1: the `layers` opt-in (4.1), explicit height plumbing instead of `geo::ground_height` (4.4: a `Terrain` value in every job and in the cover), deterministic slant boxes from a per-key sample grid (4.4), the vector world on the terrain (section 6: lowest-corner bases + foundations, drape with the lift rule, subdivision, water snap; bridges/tunnels skipped: the archive has no `brunnel` attribute), the profile section with the terrain band table (7.1, keys renamed/added as in `docs/contract.md` 3.2), the `PatchSampler` grid helper for the TIN phase. **Phase 2, Rust half built (api 1.5):** terrain layer in the scheduler (`rings::Layer`, one `Scheduler` per layer), Delatin TIN + skirts + curvature seeds (`terrain/tin.rs`; patches sampled directly from the function: z14 64, z13 128, z12 and coarser 256 cells; the 1-sample border of a patch is not used by the TIN, which only covers the tile), horizon cull, cross-layer delivery order, `"t/z/x/y"` keys, `set_terrain_detail`, `debug_stats` `terrain_*` (no `heightfield_mb` / `terrain_cache_mb`: nothing is cached). Measured (release, one thread, default profile, amplitude 160): CPU per tile mean/max T0 0.8/1.3 ms, T1 3.4/4.7, T2 13.1/17.3, T3 14.6/20.3, T4 14.4/17.4 (≈ 80 % sampling the function); resident terrain tiles settled at 300 m AGL 351 (28/29/99/113/82, 77k triangles), at 3 000 m 341 (78k triangles): the horizon cull keeps most of T4 because the relief term `√(2R·h_max)` adds ≈ 40 km at 150 m hills. Not built: Godot shader and pool (Godot half of T2).


**Design change: drape frozen (2026-10-28, Tiles v2 + Terrain v2 step P0; no api / profile version change).** The drape margin and the building foundation depth per vector band are a fixed table in `simworld-core/src/terrain/mod.rs` (`DRAPE_REFERENCE`): margin R0 0.125, R1 0.375, R2 1, R3 2.5 m; foundation R0 3, R1 7, R2 17, R3 41 m. Until now `Terrain::new` derived them at configure time from `terrain.bands[].max_error_m` / `edge_m`, `terrain.detail_scale_max` and the vector band edges × hysteresis (the 1.7 rules `band_error_rule` / `drape_margin_rule` / `foundation_rule`, kept as test-only `terrain::reference` to prove the table equals them at the default profile). Why: Tiles v2 derives the vector band edges from fov / τ (screen-space error) and Terrain v2 drops the terrain bands' `max_error_m`, so those inputs lose their meaning; `ground_height_at`, building bases and the drape stay on the exact height function (user decision), and the TIN is sunk under that function at every detail, so the margin no longer has to cover a terrain-mesh error. Default output is bit-identical (all tile, cover, terrain-mesh and seed goldens produce the same hashes as before). What changes: overriding `terrain.bands`, `terrain.detail_scale_max` or `bands.*` no longer moves the drawn ground, lights or foundations (the terrain mesh, skirts and cover still read them). Known limit: with terrain bands much coarser than the defaults (`max_error_m × detail_scale_max` above ~1 / 3 / 8 m in reach of R0 / R1 / R2) the foundation can be shallower than the sunk TIN under a building; Terrain v2 resizes the table when it defines its error bound. Pinned by `height_golden_tests.rs` (exact f64 bits of the height function and `ground_height_at`: ~9 000 points of the plain procedural terrain incl. flat-zone edges and a `terrain_center` offset, ~8 000 points of the two-cities seed world incl. sea, lakes, rivers, runway strips, fences and shoulders; regenerate with `SIMWORLD_UPDATE_GOLDEN=1` like the other goldens). Section 6.2's lift rule below is the history of this table.

**Terrain v2 step G2 (2026-10-28, api 1.10 / profile_version 7): cover by screen-space error.** The terrain layer refines by the G1 error proxy (`terrain::height::ErrorProxy`, without edit floors while the render path is TIN) instead of `bands[].edge_m`: refine distance = `max(e · H / (2 tan(fov/2) · terrain.tau_px · detail), foundation guard)`, then a 2:1 balance; `set_terrain_detail` scales tau (no re-mesh); the TIN keeps the per-ring `max_error_m`; the foundation guard (`terrain::foundation_guard_m`) closes the known limit above for the cover (a ring whose error exceeds a vector band's foundation allowance refines inside that band's reach). Details and measurements: `docs/contract.md` section 5 "Terrain cover by screen-space error", `CHANGELOG.md`.

This spec adds real relief (hills, valleys, mountains, coasts) to the streamed world the way Google Earth shows it, while keeping the low-poly look and the tile grid, quadtree cover, sibling swap, dithered fades and floating origin exactly as they are (`docs/contract.md` sections 2, 5, 6; `docs/spike-streamed-osm-city.md` "Tile grid and 3D LOD"). It fills the "terrain hook" the design doc left open (`ground_height(lat, lon)` returns 0 today). It also says which data, tools and algorithms to use and why they are the cheapest choice for memory, GPU and CPU.

Related: `docs/contract.md` (the contract this spec changes), `docs/spec-tile-cache.md` (the disk cache the DEM reuses), `docs/spec-night-rendering.md` (depth precision, fog), `docs/spike-streamed-osm-city.md` "Satellite imagery" (M7 drapes imagery on this mesh later).

## 1. Goals and non-goals

Goals:

1. **Relief like Google Earth, styled low-poly.** Mountains, valleys and coasts are recognisable from 300 m to cruise altitude; the surface is faceted (flat-shaded triangles whose size follows the terrain), not a smooth photogrammetry blob.
2. **Same grid, same streaming.** Terrain tiles are Web Mercator `z/x/y` tiles chosen by slant distance with hysteresis, swapped parent ↔ 4 children with the dithered fade, re-centred with the floating origin, fetched over HTTP range requests from a self-hosted PMTiles archive and cached in the same SQLite disk cache. No clipmaps, no single continuous mesh, no host-side LOD.
3. **The vector world stands on the terrain.** Building bases, ground areas, road strips and light points follow the relief; `ground_height_at` returns the real height; the slant-distance cover uses it.
4. **Adjustable by API.** How low-poly (max vertical error per band, triangle cap), how far (band edges), shading (flat or smooth), vertical exaggeration, palette: all in the world profile; the detail level also at runtime without a teleport.
5. **Cheapest sane implementation**: positions-only meshes, flat shading from screen derivatives (no normal arrays), one small heightfield cache, a greedy TIN on the worker pool, no textures and no GPU displacement.
6. **Opt-in for hosts.** A host that does not ask for the terrain layer sees today's flat world, whatever the profile says (section 4.1); an older addon never receives keys it cannot parse.

Non-goals (later or never): photogrammetry / 3D buildings from imagery; terrain collision shapes for Jolt (the host can build them from the delivered arrays, section 6.5); erosion/detail noise; terrain beyond ~130 km (a sky/horizon imposter is a host concern); ocean rendering (water stays the flat class-coloured polygons of M5).

## 2. Decision in one paragraph

A second self-hosted PMTiles archive holds **terrain-RGB raster tiles** (Terrarium encoding, lossless WebP, 256 px, zooms 0–12) built offline from **Copernicus GLO-30** with `massif`. Rust fetches and caches them like vector tiles, decodes them into a small f32 **heightfield cache**, and serves one height source to everything: per-job patches to the workers (mesher, ground, lights), a `Terrain` handle to the main thread (cover, `ground_height_at`). A **terrain layer** runs the existing quadtree cover a second time with its own, coarser band table (default zooms 14 → 10 out to 130 km), and for each terrain tile a worker builds a **TIN with the Delatin greedy-insertion algorithm** at a per-band maximum vertical error plus a skirt, delivered through `poll_ready` as a new `terrain` mesh (VERTEX + INDEX only). The host draws it with one shader that **flat-shades from `dFdx`/`dFdy`** (as `ground.gdshader` already does), colours by altitude and slope, bends with the earth-curvature include and fades with the dither include. The vector world is draped onto the **heightfield** (not onto the TIN) with a lift equal to the largest terrain error that can be visible at that band's distance, so a terrain LOD swap never cuts through a road or a building. Terrain tiles below the horizon are not requested, flat TIN triangles are capped in edge length so the visual earth curvature still bends them, and runways are flattened in the DEM at build time. The host turns the layer on with a `configure_world` option; without it the profile's `terrain.enabled` is ignored.

Why this and not the alternatives: section 3.4 and 3.5.

## 3. Research

### 3.1 Elevation sources

| Source | Resolution | Coverage | Datum | Access | Licence / attribution | Verdict |
|---|---|---|---|---|---|---|
| **Copernicus DEM GLO-30** | 30 m (1 arc-second) | Global except a few countries withheld from the public 30 m set (GLO-90 covers them) | EGM2008 orthometric (≈ MSL) | AWS Open Data bucket `copernicus-dem-30m` (eu-central-1, no account), GeoTIFF 1°×1° COGs | Free; attribution required, the wording comes from the current ESA mission-specific annex linked from the registry (the bucket's own readme is outdated per the provider) | **Primary source.** Newest global model (TanDEM-X 2010–2015), water bodies edited flat, Lithuania fully covered |
| AWS Terrain Tiles (Mapzen "Terrarium") | mixed: EU-DEM 25 m in Europe, 3DEP 10 m US, SRTM/GMTED/ETOPO1 elsewhere | Global, ready-made Web Mercator PNG tiles z0–15 (zooms above the native data are oversampled) | mixed (EGM96 mostly) | Bucket `elevation-tiles-prod` (+ `-eu` replica), e.g. `terrarium/{z}/{x}/{y}.png` | Free; per-source credits (Europe: "produced using Copernicus data and information funded by the European Union - EU-DEM layers"), plus "Mapzen" | **Fallback / quick start:** no build step (copy the needed tiles into a PMTiles archive), but EU-DEM is an older SRTM/ASTER blend and the credit list is long |
| SRTM 1" / NASADEM, ALOS AW3D30 | 30 m | 60°N–56°S (SRTM), global (AW3D30) | EGM96 | Free downloads | Attribution | Not needed while Copernicus exists; AW3D30 is an option for the withheld GLO-30 countries |
| National LiDAR DEMs (e.g. Lithuania's 1 m model on geoportal.lt) | 1–5 m | One country | national | Varies | Varies, licence to check | **Later (T4)** for R0 near airports; 30 m is enough for a flight game above 300 m |
| Cesium World Terrain, MapTiler Terrain, Mapbox Terrain-RGB | 1–30 m | Global | — | Hosted APIs, tokens | Commercial; caching and re-hosting restricted | Rejected: the project rule is self-hosted data (`docs/contract.md` section 10) |

### 3.2 Tile encodings

| Encoding | What it is | Precision | Pros | Cons |
|---|---|---|---|---|
| **Terrarium** (RGB PNG/WebP) | `h = R·256 + G + B/256 − 32768` | 1/256 m, range −11 000..8 900 m | Fixed formula, no metadata; produced by `massif`, `rio-rgbify`, AWS tiles; lossless WebP ≈ 2× smaller than PNG | Must be decoded losslessly (JPEG would wreck the bits) |
| Mapbox Terrain-RGB | `h = −10000 + (R·65536 + G·256 + B)·0.1` (base/interval configurable) | 0.1 m | Same tooling | Encoding parameters live outside the tile; nothing it does better for us |
| Raw 16-bit (PNG16 / own) | u16 height per pixel | 0.1–1 m | Half the bytes before compression | No off-the-shelf tiler; no advantage after WebP |
| Cesium quantized-mesh (`.terrain`) | Pre-built TIN per tile, 16-bit quantized vertices, edge lists, skirts | mesh | Zero meshing CPU on the device | Triangle budget fixed at build time (not adjustable by API), needs the Cesium tiler toolchain, and we still need a heightfield for `ground_height` and draping |
| Normal-map tiles (`normal` on AWS) | Normals + quantized height | coarse | — | Not useful for flat shading from geometry |

Pick: **Terrarium in lossless WebP**, 256 px tiles. The Rust side also accepts Mapbox RGB (one `configure_world` option) because MapTiler / Mapbox-style archives exist in the wild.

### 3.3 Build tooling (offline, `data/`)

| Tool | Notes |
|---|---|
| **`massif`** (Rust, MIT) | GDAL-readable raster (GeoTIFF, VRT, HGT) → terrain-RGB tiles directly into **PMTiles** or MBTiles; Terrarium or Mapbox encoding; WebP (default) or PNG; `--min-z`/`--max-z` (defaults 5–12); multi-threaded; needs GDAL 3.5–3.12 natively or runs in Docker. Replaces `rio-rgbify` (which the author reports did not finish in 48 h on a Europe VRT that `massif` did in 4 h). **Recommended.** |
| `rio-rgbify` + `pmtiles convert` | Python/rasterio; writes MBTiles, then the `pmtiles` CLI converts to PMTiles. Works, slower. |
| `rio-pmtiles` | Raster → PMTiles v3 (WebP, `LOSSLESS=TRUE`), but needs a 3-band input, so the RGB encoding has to happen first in GDAL. |
| GDAL (`ghcr.io/osgeo/gdal` image) | `gdalbuildvrt` over the Copernicus 1° COGs, `gdalwarp -t_srs EPSG:3857` only if the tiler needs it (`massif` reprojects itself and is faster from EPSG:4326). |
| PMTiles v3 | Raster tile types `png`, `jpeg`, `webp`, `avif` are first-class (`pmtiles` crate `TileType`); the existing fetcher only needs to stop assuming MVT. |

Pipeline for the bench (`data/build_terrain.sh`, T0): download the GLO-30 1° tiles covering the vector bounds plus a 150 km margin from the AWS bucket (`aws s3 cp --no-sign-request` or plain HTTPS) → `gdalbuildvrt` → **runway flattening** (below) → `massif --encoding terrarium --format webp --min-z 0 --max-z 12 -j 8 cop30.vrt out/vilnius-terrain.pmtiles`.

**Runway flattening (offline, T0).** A 30 m DEM under a runway has bumps of a few metres, which a flight game feels through `ground_height_at` (take-off, landing, crash test). The build extracts `aeroway=runway` areas (or runway lines buffered by their `width`, default 45 m) from the same OSM extract with `osmium tags-filter`, and for each runway writes a ruled surface into the DEM: the height along the runway axis is linear between the two threshold heights (median DEM over a 60 m disc at each end), constant across the width, blended over a 100 m shoulder back to the DEM. `gdal_rasterize`/a small Python step over the VRT writes a patched GeoTIFF before tiling. Because it is in the archive, the TIN, the drape and `ground_height_at` agree without any runtime vector input (terrain tiles beyond 25 km have no vector tiles to read runways from). Taxiways and aprons stay as the DEM gives them. Expected size: a 300×300 km area at z12 is ~2 500 tiles, 20–150 KB each → under 150 MB with the pyramid. Serve it from `data/serve.sh` next to `vilnius.pmtiles`. Zoom 12 is the native limit of 30 m data at Vilnius latitude (22 m/px; 38 m/px at the equator): finer zooms would only interpolate.

### 3.4 Mesh algorithms (heightfield → low-poly triangles)

| Approach | CPU per tile | GPU memory | Triangles for a given visual error | Low-poly look | Fit with the tile grid |
|---|---|---|---|---|---|
| **Delatin / hmm (greedy TIN insertion, Garland–Heckbert 1995)** | Moderate: iterative, O(points inserted × log); a 258² grid at 1–4 m error takes single-digit ms to tens of ms (to measure, T2) | Lowest: irregular vertices only where the relief needs them | **Best** (flat land → a handful of triangles, ridges → dense) | **Yes**: triangles follow ridges and valleys, the classic "low-poly mountain" look | Per tile, any grid size; needs skirts |
| Martini (RTIN, Mapbox) | Fast (precomputed error pyramid) | Low | 2–3× more triangles than Delatin for the same error; right-angle, grid-aligned triangles | Looks like a subdivided grid, less "art" | Needs a 2ⁿ+1 grid (257², fine); needs skirts |
| Regular grid + vertex-texture displacement | Near zero (one shared grid mesh, per-tile R16 heightmap texture) | Texture per tile (65²–257² × 2 B) + shared index buffer | Worst: uniform density regardless of relief; LOD by grid step | No faceting unless the grid is coarse everywhere | Per tile; needs per-instance texture (instance uniforms cannot be textures in Godot, so one material per tile) |
| Clipmap / single terrain (Terrain3D style) | Low | Large resident heightmap | — | — | **Breaks the grid**: one continuous mesh, host-side LOD, no sibling swap or fades; rejected |
| Pre-built quantized-mesh tiles | Zero | Low | Fixed at build time | Yes | Per tile, but not adjustable by API (section 3.2) |

Pick: **Delatin**, vendored into `simworld-core/src/terrain/tin.rs` (ISC licence, ~300 lines; the `delatin` 0.2.1 crate exists but its README and source disagree on the signature and it has no triangle cap). Vendoring gives us a triangle cap, deterministic tie-breaking (golden hashes), `f32` input and no dependency on a thinly documented crate. Martini stays the documented fallback if Delatin's CPU time turns out too high at small errors on mountainous z10 tiles (section 10).

### 3.5 Renderer facts that shape the design (Godot 4.7, Mobile renderer)

- Screen-space derivatives (`dFdx`, `dFdy`) are GLSL ES 3.0 built-ins and already used by `ground.gdshader`: flat shading costs **no normal array**. `flat` varyings exist too but need unshared vertices to carry per-face data, so derivatives win.
- Per-instance uniforms work in Forward+ and Mobile (RenderingDevice renderers; practical limit 16 per shader, no textures or arrays). The fade uniforms used them until the instance-uniform bug (a node without a slot reads 0; the buffer holds 4096 nodes per project, CHANGELOG, Unreleased): fades now use one dither material clone per fading tile (`FadeMaterials`), so the addon has no instance uniform and the terrain needs none.
- `ArrayMesh` compressed attributes (`ARRAY_FLAG_COMPRESS_ATTRIBUTES`) pack positions into 16-bit unorm over the surface AABB: 0.3 m steps on a 22 km z10 tile, visible as seams. Terrain surfaces stay **uncompressed** (today's default for `AddSurfaceFromArrays`).
- Godot stores a surface's index buffer with 16-bit indices when it has ≤ 65 536 vertices; terrain tiles stay far below that (triangle cap, section 5.3).
- Godot 4.3+ uses a reversed-Z depth buffer in the RenderingDevice renderers, so pushing the camera far plane from 32 km to ~150 km for the far terrain bands is acceptable with near = 0.5 m (`docs/spec-night-rendering.md` "depth precision"). Check in T0 on the Mobile renderer with a Z-fighting shot at 10 km.
- Draw calls are per `MeshInstance3D`: every terrain tile is one draw **if it survives frustum culling**. The cover is omnidirectional, so *resident* tiles and *drawn* tiles differ: the band table in section 5.2 keeps the layer at ≈ 250–280 resident tiles out to 130 km before the horizon cull (section 5.1), of which roughly a third to a quarter is inside a 60° vertical / ~90° horizontal frustum (estimate ≈ 70–120 draws, to measure in T2). The design doc caps the **whole frame at ≤ 300 draw calls** on iOS (`docs/spike-streamed-osm-city.md` budgets; ground already counts against it), and the vector world uses most of that today, so terrain does not fit beside it unchanged; see section 9 and the open decision in section 13.

## 4. Data and the heightfield service (core)

### 4.1 Terrain archive and tileset contract

- `configure_world` gains `terrain_url` (String, `http(s)://…pmtiles`, empty = no terrain) and `terrain_encoding` (`"terrarium"` default, or `"mapbox"`; Mapbox base/interval through two more optional keys only if ever needed). Wrong scheme → error, nothing changes (same rules as `tile_url`).
- **Host opt-in:** `configure_world` option `layers` (Array of String, default `["world"]`). Terrain is active only when `layers` contains `"terrain"` **and** the profile has `terrain.enabled = true`. Without the option, `terrain.enabled` is ignored with one warning (`terrain.enabled ignored: host did not request layer "terrain"`): no `"t/…"` keys, no draping, `ground_height_at` stays 0, exactly today's flat world. Reason: a profile file is data a designer can change without touching the host, and today's `WorldView` reports every key it cannot parse as an error (`TileKey.TryParse` fails on `"t/…"`, `WorldView.UploadTile` calls `GD.PushError`) and would also show a city standing on invisible relief. Draping without the terrain layer drawn is never wanted, so the two are one switch. An unknown layer name is an error, nothing changes. The addon version that sends `layers: ["terrain"]` is the minimum host version for terrain.
- The archive is opened asynchronously like the vector archive. **Terrain tileset contract:** PMTiles v3, `tile_type` `png` or `webp`, square tiles of 256 or 512 px (read from the first decoded tile; the zoom math adapts), `min_zoom` ≤ `terrain.far_zoom` (default 10) and `max_zoom` ≥ `terrain.dem_zoom` (default 12). A mismatch is a sticky `status()` `error` for the terrain source (the vector world keeps running without relief, heights 0), logged once.
- Attribution: the archive metadata `attribution` string, else the `terrain_attribution` option; appended to `attributions()` and the `configure_world` result. The bench passes the Copernicus credit line from the ESA annex.
- Disk cache: the same `DiskCache`, one more file `tiles-{hash(terrain_url)}.sqlite`, same LRU, own cap `terrain.cache_cap_mb` (default 200). Raw tile bytes only, never heightfields or meshes (profile changes never serve stale geometry; `docs/spec-tile-cache.md`). The cache lock already covers the folder.
- Synthetic terrain: with terrain active and an empty `terrain_url`, heights come from a deterministic analytic field (sum of three sine ridges plus a plateau, amplitude `terrain.synthetic_amplitude_m`, default 0 = flat). The field is **sampled into in-memory DEM tiles** (f32 grids at the same zooms and pixel centres a real archive would have) that enter the same heightfield cache, patch assembly and TIN path as fetched tiles; only fetch and WebP/PNG decode are skipped (they have their own unit tests). The synthetic city (`tile_url` empty) gets relief this way for offline runs, tests and golden hashes, and the goldens exercise the real patch code.

### 4.2 Heightfield cache

- A **DEM tile** = one decoded raster tile (`dem_zoom` or coarser) as `f32` meters, row-major, 256² × 4 B = 256 KiB (512 px tiles: 1 MiB). Decoders: `png` and `image-webp` (both pure Rust, iOS-safe, no C), then the Terrarium/Mapbox formula per pixel. Decoding happens on the rayon pool, ~1–3 ms per tile.
- **How many DEM tiles the cover touches.** Every patch needs its tile's DEM tile plus the border neighbours (section 4.3), so the union over the default cover is the tiles of each band plus a ring of neighbours: ≈ 100 z12 (T2 + vector bands + T0/T1 crops), ≈ 125 z11 (T3), ≈ 140 z10 (T4), **≈ 350–400 DEM tiles ≈ 90–100 MB as f32**. Keeping them all decoded is not an option on iOS. (It matches the network estimate in section 9: they are all fetched once, but they do not all stay decoded.)
- **Residency policy.** The decoded LRU (cap `terrain.heightfield_cache_mb`, default 24) holds three kinds of tile:
  1. the **coarse field**: the `terrain.coarse_zoom` (default 8; ≈ 90 km tiles at 54.7°) DEM tiles covering the terrain reach plus one tile, pinned (≈ 9–16 tiles, ≤ 4 MB). It answers every height query outside the near set and gives the cover deterministic tile boxes (section 4.4);
  2. the **near set**: the `dem_zoom` tiles within the vector reach (`bands.r2_edge_m × hysteresis` + one tile) of the camera, pinned while inside it (≈ 16–25 tiles ≈ 4–6 MB). Main-thread queries (`ground_height_at`, `update_camera_geodetic` AGL, `slant_m` near the camera) read these;
  3. **transient** tiles: decoded for patch assembly of far terrain tiles and dropped by the LRU once no in-flight job needs them. The compressed bytes stay in the SQLite disk cache (20–80 KB each), so a re-mesh (`set_terrain_detail`, DEM fallback → real data) decodes them again from disk (1–3 ms, no network).
  Pinned plus transient stays under 24 MB at the defaults with `terrain.max_fetch` 4 per band (≤ 9 tiles per in-flight patch). `f32` is kept for simplicity; a u16 fixed-point variant (0.1 m steps, −500..6 000 m) halves it and is the first knob if iOS memory asks for it (open decision).
- **Sharing.** Each decoded tile is an immutable `Arc<[f32]>`; the LRU index sits behind an `RwLock` held only to look up or insert an `Arc` (never during decode, patch assembly or a bilinear read). The main thread's per-frame queries (`slant_m` runs per tile per band per frame) therefore never wait for a worker building a 258² patch. Queries are a bilinear read of one tile (microseconds).

### 4.3 Patches (deterministic worker input)

- The **patch** of a tile `(Z, x, y)` of either layer is the rectangle of DEM samples covering the tile plus **one sample of border** on every side, assembled from the DEM tiles of zoom `min(Z, dem_zoom)` that intersect it. For `Z ≤ dem_zoom` the tile is exactly one DEM tile and its border always lies in the 8 neighbours, so a patch reads **9 DEM tiles** (the full 3×3 block; it uses only one edge row or one corner sample of each neighbour, but each neighbour is still a whole tile fetch and decode). For `Z > dem_zoom` the patch is a crop of the ancestor DEM tile (z13 → 128² + border, z14 → 64², z15 → 32², z16 → 16²); only crops on the ancestor's edge reach a neighbour (1, 2 or 4 tiles).
- A job (vector tile or terrain tile) waits for its patch's DEM tiles before meshing. A job's **final** output depends only on the tile key, the profile and the archive, never on what happens to be cached (determinism, section 8). The **first** delivery may be a fallback (next bullet) and therefore depends on timing; golden tests compare the settled output after every re-delivery, and the contract says so.
- **Vector-tile latency.** Today a vector job needs only its MVT. With terrain it starts the MVT fetch and its DEM fetches **at the same time** with the same priority (one z12 DEM tile serves 16 z14 vector tiles, so most are disk-cache or LRU hits), and meshes when both are in. The added time to the first tile is at most one range request; T1 measures the bench's `first tile` time with and without terrain and records it in section 9.
- A DEM tile that fails to load, or is not in after `terrain.dem_fallback_timeout_s`, falls back to its nearest loaded ancestor (down to the pinned coarse field; coarser samples, logged, `status()` degraded) so the world never waits forever on terrain; every tile meshed on a fallback is re-delivered when the real DEM tile arrives (rate-limited like light-mode re-deliveries).

### 4.4 `ground_height` and the cover

- **This is a plumbing change, not a body swap.** `geo::ground_height(lat, lon)` (`geo.rs`) is a pure free function today, called per vertex on worker threads by the mesher, ground and lights and per tile per frame by `rings::ViewPoint::slant_m` and the scheduler's view points. Terrain heights need state, so the free function goes away and every caller gets its heights explicitly:
  - worker jobs (mesher, ground, lights) receive their **patch** (section 4.3) in the job and sample it through a `PatchSampler` (bilinear, tile-local); the `TileProjector` gets a `height(x, z)` that reads it. No worker touches the shared cache after the patch is built;
  - main-thread callers (`World::ground_height_at`, `update_camera_geodetic` AGL, `Scheduler::views`, `ViewPoint::slant_m`) hold a `Terrain` handle owned by `World`;
  - with terrain off, both paths return 0 and the code is otherwise identical (goldens unchanged).
  T1 lists this as its main work item.
- `Terrain::height(lat, lon)` (main thread): bilinear in the near-set `dem_zoom` tile when the point is inside the near set and that tile is loaded, else in the pinned coarse field, times `terrain.exaggeration` plus `terrain.height_offset_m`; 0 only before the coarse field is in. Never "whatever finest tile happens to be cached", so the answer at a point depends on the camera position and load state of two well-defined sets, not on LRU history. `ground_height_at` is documented as "may refine from the coarse field to the 30 m data while DEM tiles arrive".
- **Rendered surface vs `ground_height_at`.** `ground_height_at` is the heightfield; the visible surface is the TIN, which differs from it by up to `max_error_m × detail_scale` of the terrain band at that point (T0 default 0.5 m, up to 1 m at `detail_scale_max` 2). A host doing touchdown or crash tests gets the heightfield, so wheels can sit up to that much above or below the drawn facets. The T0 default is chosen tight for this reason; runways are flat in the data (section 3.3). An exact `surface_height_at` that reads the delivered TIN is an open decision (section 13).
- **Slant-distance boxes, deterministically.** A tile's box is `[h_min, h_max + max building height]`: from its patch (vector tiles) or TIN (terrain tiles) once delivered, as `max_height` is today; **before that from the coarse field** (min/max of the coarse samples under the tile rect, plus `default_tile_height_m` for vector tiles), never from the parent's or another cached tile. The cover therefore depends only on the camera, delivery state (as today) and the pinned coarse field. Until the coarse field is loaded the terrain layer requests nothing but the coarse tiles and the vector cover uses height 0 (today's behaviour). This is what keeps a 2 000 m peak 20 km away in the right band, and what makes the cover goldens with a synthetic peak reproducible.
- `rings::MAX_BOX_HEIGHT_M` (400 m, used to prune root candidates by camera altitude) becomes `coarse max height in reach + MAX_BOX_HEIGHT_M` when terrain is active; with terrain off it is unchanged.
- Datum: Copernicus heights are orthometric (EGM2008), Terrarium heights are mostly EGM96; both are "meters above mean sea level" within a metre or two, which matches the contract (local `y` = MSL). No geoid model is added; the `update_camera_geodetic` ellipsoid question in `docs/spec-library-packaging.md` stays open and independent.

## 5. The terrain layer

### 5.1 One scheduler, two layers

The `Scheduler` (cover, hysteresis, sibling groups, fades, delivery order) is generalised over a **layer** = `{ root zoom, leaf zoom, band edges, content kind }`. The vector layer is today's `{13, 16, bands.*, buildings+ground+lights}`. The terrain layer is `{ terrain.far_zoom, terrain.near_zoom, terrain.bands[*].edge_m, terrain }`. Each layer has its own quadtree, its own look-ahead points per band (same formula) and its own in-flight limits (`terrain.max_fetch`, `terrain.max_mesh`, default 4 and 2). All guarantees of `docs/contract.md` section 5 hold **per layer**; the two layers never share a key.

Terrain keys are strings `"t/z/x/y"` (the `t/` prefix is the layer). They flow through the same `poll_ready`, `poll_fades` and `poll_unload` (only for a host that opted in, section 4.1); a host keeps one tile map per layer and evaluates the visibility rule per layer with the layer's root zoom in place of `z == 13`. `ring` is per layer: vector rings stay `16 − z` (0..3), terrain rings are the index in `terrain.bands` (0..len−1, i.e. `near_zoom − z`), so per-ring host statistics are kept per layer (the addon's `TileCover` stops clamping `ring` to 3).

**Horizon cull (terrain layer only).** The slant-distance cover is omnidirectional and knows nothing of the Earth's curvature; from 300 m AGL the sea-level horizon is ≈ 62 km away, so most of T4 (60–130 km) would be fetched, meshed and kept without ever being seen. A terrain tile is out of the cover when the horizontal distance to its nearest point exceeds
`√(2R·max(h_cam − h_ref, 0)) + √(2R·max(h_max − h_ref, 0)) + terrain.horizon_margin_m`,
with `R` = 6 371 km, `h_cam` the camera altitude MSL, `h_max` the tile's box top (coarse field until delivered), and `h_ref` = the lower of the ground under the camera and the tile's `h_min`. The test uses the band hysteresis like the edges (a tile drops out only `hysteresis` past the line, comes back at the line). To keep the sibling-swap rule intact, it applies to **roots** and to **whole sibling groups**: a split parent keeps all 4 children unless all 4 are below the horizon (then it does not split). It assumes a real-size Earth; a host that renders with a different visual curvature (bench `--curvature`) sets `terrain.horizon_cull = false` or widens the margin. The vector layer is not culled (its 25 km reach is inside the horizon from any flight altitude above ≈ 50 m AGL).

**Delivery order across layers.** Both layers share the host's upload budget (at most 2 tiles / 2 ms per frame, contract section 4). Ready tiles of both layers are merged into one order: fresh before re-deliveries (today's rule), then by slant distance, with a terrain tile counted at half its distance so the ground under the near city tends to arrive before the buildings on it. The first fill at the defaults is ≈ 340 vector + ≈ 200 terrain tiles (after the horizon cull), ≈ 4–5 s at 60 fps; T2 measures it during the geodetic flight test at 250 m/s and records whether holes appear.

### 5.2 Default band table

Tile widths at Vilnius latitude (54.7°): z10 22.6 km, z11 11.3 km, z12 5.65 km, z13 2.83 km, z14 1.41 km. Edges align with the vector band edges so LOD changes of both layers coincide.

| Terrain band | ring | Zoom | Slant distance | `max_error_m` | `max_edge_m` | `max_triangles` | Tiles in the annulus (≈) | Patch samples |
|---|---|---|---|---|---|---|---|---|
| T0 | 0 | 14 | < 2.5 km | 0.5 | 0 (off) | 4 096 | 10–14 | 64² (crop of z12) |
| T1 | 1 | 13 | 2.5–7 km | 1.5 | 0 (off) | 4 096 | 17–22 | 128² (crop of z12) |
| T2 | 2 | 12 | 7–25 km | 4 | 2 000 | 8 192 | 57–65 | 256² |
| T3 | 3 | 11 | 25–60 km | 10 | 3 000 | 8 192 | 73–80 | 256² (z11 DEM tile) |
| T4 | 4 | 10 | 60–130 km | 25 | 5 000 | 8 192 | 82–90 before the horizon cull | 256² (z10 DEM tile) |

Angular size of the error at the band's near edge is ≈ 0.02–0.03° (under one pixel at 1080p / 60° FOV) for T1–T4 and ~2 px for T0 at 300 m AGL, which is where the faceting should start to show. `terrain.bands` is an array; a profile may use fewer bands (e.g. stop at T2 for a 25 km world) or move the edges. Structural: contiguous zooms, finest ≤ 16, coarsest ≥ 8, ring = index in the array.

Resident tiles: ≈ 240–280 for the whole layer at the defaults before the horizon cull; from 300 m AGL the cull removes most of T4 and part of T3 (≈ 170–200 left), from 3 000 m almost nothing (horizon ≈ 195 km). Drawn tiles are the frustum-visible subset (section 3.5). Beyond 130 km nothing is requested; fog and the host's sky take over (section 6.4).

### 5.3 TIN generation (worker)

For a terrain tile with patch `P` (n×m samples, 1-sample border) at band `b`:

1. Heights `h = P × exaggeration + offset` (the same transform as `ground_height`).
2. **Delatin** greedy insertion on the interior grid until the maximum vertical error < `bands[b].max_error_m × detail_scale` or the triangle count reaches `bands[b].max_triangles`. The four corners are always present. **Curvature seeds:** when `bands[b].max_edge_m > 0`, the triangulation starts from a regular grid of `n = ⌈tile width / max_edge_m⌉ + 1` samples per side (corners included) instead of the corners alone. Reason: `earth_curve.gdshaderinc` drops the surface per vertex by d²/2R, and a triangle is a straight chord between its vertices, so a 22 km z10 tile that Delatin collapses to 2 triangles sags L²/8R ≈ 10 m below its curved neighbours' edge vertices (more with the bench's `--curvature=50`) and opens cracks the skirt was not sized for. At the defaults the worst chord sag is 5 000²/8R ≈ 0.5 m; the seeds cost ≤ 2·n² triangles (≤ 50 on z10) and only matter on flat land, where Delatin adds nothing else. A host that renders a stronger curvature lowers `max_edge_m` by √(curvature). Vertex positions: sample column/row → tile-local meters relative to the tile centre (Mercator metres scaled at the tile's latitude, the same projection as `TileProjector`), `y` = height. Border samples are used only for error evaluation near the edges, not as vertices, so every vertex lies inside the tile.
3. **Skirt:** every edge vertex gets a copy pushed down by `terrain.skirt_m` (default 0 = automatic: `2 × max_error_m` of the band plus 1 m, capped at 60 m), and the edge is closed with quads. Skirts hide the ≤ 2·error cracks between neighbours of the same band and the larger ones across band boundaries; they face outwards and are invisible from above. This is the Google Earth / Cesium approach and costs ~4·√(vertices) extra vertices.
4. Output `MeshData` with VERTEX and INDEX only; NORMAL only when `terrain.shading = "smooth"` (area-weighted vertex normals, +12 B/vertex); no COLOR, no UV. Winding clockwise seen from above (contract section 2).
5. Record `h_min`, `h_max` and the triangle count for the box and `debug_tiles`.

Flat land (most of a city) collapses to a few dozen triangles per tile; the triangle cap bounds the mountains. Estimated cost per 258² mountainous tile at 4 m error: a few thousand insertions, each a local Delaunay update plus error re-evaluation over the affected triangles, i.e. low tens of milliseconds on the rayon pool; to measure in T2 against the budget in section 9.

### 5.4 Ready-tile shape (additive)

Every `poll_ready` entry gains `layer` (`"world"` or `"terrain"`). Terrain entries carry: `key` (`"t/z/x/y"`), `layer`, `ring` (index in `terrain.bands`), `origin` (tile centre, `y` = 0), `terrain` (Array in `Mesh.ARRAY_MAX` layout: VERTEX, INDEX, optional NORMAL; never empty: a terrain tile always has at least its 2 triangles and skirts), `h_min`, `h_max` (float). The vector-layer keys `mesh`, `ground`, `lights`, `light_colors` are present but empty on terrain entries. Terrain entries reach only a host that requested the layer (section 4.1); a host that did not never sees a `"t/…"` key, in `poll_ready`, `poll_fades` or `poll_unload`. Re-delivery under the same key replaces the terrain mesh in place (detail-scale changes, DEM fallback → real data).

## 6. The vector world on the terrain

### 6.1 Buildings

- Base height = **minimum** `ground_height` over the footprint's outline vertices (sampled at the mesher's DP-simplified ring, so a building across a slope stands on its lowest corner). Roof height is unchanged (relative to the base), so COLOR `a` and `lights` keep their meaning.
- **Foundation:** walls are extended down from the base by `lift(band) + 1 m` (section 6.2), closed by nothing (the bottom is never seen). On the uphill side the terrain buries part of the wall; on the downhill side the foundation closes the gap left by the terrain error. Wall `TEX_UV.v` keeps 0 at the true base, so the foundation has negative `v` and the window shader (M6) draws no windows there.
- Raised parts (`min_height`) unchanged. Clipping at tile edges unchanged; a building clipped across two tiles uses the same base on both sides because both sides sample the same heightfield at the same (full) footprint vertices before clipping.

### 6.2 Ground areas and road strips (the drape rule)

Ground vertices move from `y = lift_level` to `y = ground_height(vertex) + lift(band) + level × lift_step_m`. Long edges are subdivided to `terrain.drape_step_m` (default 25 m, about one DEM sample) before draping so a strip follows the slope instead of bridging it; the subdivision is skipped where the patch is flat within `0.1 m` over the edge (cities stay as cheap as today).

**Lift rule.** `lift(band N)` = the largest `max_error_m × detail_scale_max` of every terrain band whose distance range intersects `[0, bands.rN_edge_m × hysteresis]`, i.e. the worst-case gap between any terrain TIN that can be on screen at that distance and the heightfield the ground was draped on. With the defaults and `detail_scale_max` 2: R0 1.0 m, R1 3.0 m, R2 8.0 m, all below 0.2° at the band's nearest distance. The library computes it from the profile; it is not a user knob. Rationale: draping onto the heightfield rather than onto a specific TIN keeps vector tiles independent of terrain LOD swaps (the two layers refine at different moments and zooms), at the price of a sub-pixel float. The existing `depth_bias_per_m` in `ground.gdshader` stays.

**Decision: building bases are not lifted (checked 2026-10-27 in the bench, synthetic city and the real Vilnius archive).** The draped ground sits `lift(band)` above the base, so a building loses that much of its visible wall: about a third of a floor at R0 (1 m), up to one floor of a 3-4 floor building at R1 (3 m); at R2 (8 m, merged boxes of 15-30 m) it is not visible at 3-6 km, and a housing estate on a slope looks the same as on flat ground. Lifting the bases by the lift would restore the true height but make every roof jump by 3 m (R0 to R1) and 5 m (R1 to R2) at each band swap, which shows more than the hidden floor. Revisit only with a case where the hidden wall is conspicuous (a steep slope with tall R1 blocks).

**Water areas** (class 7). **Procedural terrain (built): water drapes like every other area** (plain height + lift, subdivided by `drape_step_m` with the band's tolerance; `terrain.water_snap_m` default 0). The height function knows nothing of OSM water, so there is no DEM water mask to disagree with, and the snap only pushed shore vertices into valleys the terrain mesh does not have (3-8 m under it on slopes, against a lift of 1-3 m) while hills inside a lake poked through the flat water. Consequence: water on a slope is a draped, tilted surface (a lake on a hillside is not level) until water is burned into the height function (open decision below, and seed spec section 5.2a). *DEM design (not built):* Copernicus edits lakes flat and rivers monotonic, but the OSM outline and the DEM water mask do not match: shore vertices sample the bank, so a per-vertex drape tilts the lake surface up at its edges by metres. Water vertices then take the **minimum** heightfield sample within `terrain.water_snap_m` (a 5×5 window at 22 m/px) around the vertex instead of the bilinear height, and water is not subdivided; the rule is per vertex, so the same vertex on both sides of a tile seam gets the same height. An island inside a lake drapes normally.

**Bridges and tunnels.** `data/schema/simplane.yml` has no bridge/tunnel attribute today, so a road would dive into the Neris valley and climb over a hill. T0 adds an optional road attribute `brunnel` (`bridge` / `tunnel`, from the OSM tags of the same name; absent = on the ground; archive rebuilt like the W2 attributes) and T3 drapes by it: a **bridge** run takes the heights of its two end vertices (draped normally) and interpolates linearly along its length, never below `ground + lift`; it is not subdivided. A **tunnel** run is not emitted (the road disappears at the portal; its centre line is not a surface). Runs split at attribute changes, which planetiler already does because bridges are separate OSM ways. Without the attribute (old archive), everything drapes as ground.

**Colour transition at the vector reach.** Vector ground exists in R0–R2 only (to 7 km); beyond it, and between landuse polygons inside it, the terrain palette shows. The terrain shader's low colour (`TerrainLowColor`) defaults to the ground shader's default land colour (one shared `WorldRenderSettings` value), and the R2 → R3 fade of the vector ground dithers onto that same colour, so the reach edge shows as a gradual loss of landuse detail rather than a colour step. Imagery (M7) replaces the palette later.

### 6.3 Lights and the cover

- Light points: centroid height = `ground_height(centroid) + roof height`; clustered cells use the mean of their members (unchanged formula on new inputs).
- Vector tile boxes use `h_min`/`h_max` of their patch (section 4.4). `default_tile_height_m` keeps its meaning for the building part.

### 6.4 Rendering (addon, host)

- `addons/simworld/shaders/terrain.gdshader`: `vertex()` applies `earth_curve_drop` (same include as every world shader); `fragment()` rebuilds the normal from `dFdx`/`dFdy` of the view-space position (flat) or uses NORMAL (smooth), applies the dither include, and shades with a palette by **altitude above sea level** (model-space `y` before the curvature drop, read in `vertex()`) and **slope** (`1 − n.y`): low green → dry grass → rock above a slope threshold → snow above a snow line, all `WorldRenderSettings` uniforms (`TerrainLowColor`, `TerrainHighColor`, `TerrainRockColor`, `TerrainSnowColor`, `TerrainRockSlope`, `TerrainSnowLineM`, `TerrainRoughness`), plus the `simworld_night_factor` dimming imagery will use. A `uv` for M7 imagery is computed in the shader from the tile-local `x/z` and a `tile_size_m` uniform (set per material instance by the host from the tile's zoom), so no UV array is ever uploaded.
- `TilePool` gets a second pool (`TerrainNodes`: one `MeshInstance3D` + reused `ArrayMesh`, shared terrain material and its dither variant, `ExtraCullMargin` = curve margin), `WorldView` a second `TileCover` keyed by terrain keys; the per-frame loop is the same code path with the layer picked from the entry. Shadows: terrain receives, does not cast (one draw per caster otherwise); the host's `DirectionalLight3D.directional_shadow_max_distance` stays at its few-km value and is never tied to the raised far plane (shadow-map resolution would collapse); terrain beyond it receives none. Revisit with the daylight work. `TileCover` and the debug overlay keep ring statistics per layer (terrain has 5 rings).
- Camera far plane: the host raises it to `terrain.bands.last().edge_m × 1.15 + margin` when the terrain layer is on (the bench: `--far=`), and sets depth + height fog so T3/T4 facets read as haze, not polygons (`NightLook` gains the day-time fog density).
- The bench's fallback plane (`fallback_ground.gdshader`) drops to `y = −30 m` when terrain is on so it only shows through missing tiles, and is hidden entirely once T4 proves the cover has no holes.

### 6.5 Physics

Not part of the library. A host that wants ground collision builds a `ConcavePolygonShape3D` from the delivered `terrain` arrays of T0/T1 tiles (Jolt handles static concave meshes) in its own `poll_ready` handler, or uses `ground_height_at` for a height test, which is what SimPlane needs (crash and AGL). The spec records this so nobody adds collision shapes to the pool by default.

## 7. Adjustable by API

### 7.1 World profile section `terrain` (profile_version 5)

| Key | Type / range | Default | Meaning |
|---|---|---|---|
| `terrain.enabled` | bool | **false** | Off keeps today's flat world bit for bit (golden hashes unchanged). On with no `terrain_url` = synthetic relief. Takes effect only when the host passes `layers: ["world", "terrain"]` (section 4.1) |
| `terrain.dem_zoom` | int 8..15 | 12 | Zoom at which the archive holds native data; finer tiles crop it |
| `terrain.coarse_zoom` | int 5..10, ≤ `far_zoom` | 8 | Zoom of the pinned coarse field (cover boxes, far queries, last fallback) |
| `terrain.near_zoom`, `terrain.far_zoom` | int 8..16, near ≥ far | 14, 10 | Zoom range of the terrain layer; `bands` has `near − far + 1` entries |
| `terrain.bands[i].edge_m` | float, increasing | 2 500 / 7 000 / 25 000 / 60 000 / 130 000 | Outer slant-distance edge of band i (ring i, zoom `near − i`) |
| `terrain.bands[i].max_error_m` | float 0.05..200 | 0.5 / 1.5 / 4 / 10 / 25 | Delatin stop criterion = the low-poly knob |
| `terrain.bands[i].max_edge_m` | float 0 or 500..50 000 | 0 / 0 / 2 000 / 3 000 / 5 000 | Seed grid spacing so the visual earth curvature bends flat tiles (section 5.3); 0 = corners only |
| `terrain.bands[i].max_triangles` | int 2..65 000 | 4 096 / 4 096 / 8 192 / 8 192 / 8 192 | Hard cap per tile (the curvature seeds count towards it) |
| `terrain.horizon_cull` | bool | true | Drop terrain tiles below the horizon of a real-size Earth (section 5.1) |
| `terrain.horizon_margin_m` | float 0..50 000 | 2 000 | Extra distance past the computed horizon before a tile is culled |
| `terrain.water_snap_m` | float 0..200 | 0 | Water vertices take the minimum height within this radius and water is not subdivided (section 6.2, DEM only); 0 = water drapes like every other area |
| `terrain.detail_scale_max` | float 1..8 | 2 | Upper bound of the runtime `set_terrain_detail`; sizes the drape lift |
| `terrain.skirt_m` | float 0..200 | 0 (= automatic) | Skirt depth; 0 = `2 × max_error_m + 1` per band |
| `terrain.shading` | `"flat"` / `"smooth"` | `"flat"` | Smooth adds a NORMAL array |
| `terrain.exaggeration` | float 0.1..5 | 1.0 | Vertical scale of every height (terrain, bases, lights, `ground_height_at`) |
| `terrain.height_offset_m` | float −500..500 | 0 | Added to every height (datum nudge) |
| `terrain.drape_step_m` | float 5..200 | 25 | Max edge length of draped ground geometry on non-flat patches |
| `terrain.heightfield_cache_mb` | float 4..256 | 24 | Decoded DEM tile LRU (pinned coarse field + near set + transient, section 4.2) |
| `terrain.cache_cap_mb` | float 10..5 000 | 200 | Disk cache cap of the terrain archive |
| `terrain.max_fetch`, `terrain.max_mesh` | int 1..16 | 4, 2 | In-flight limits of the terrain layer (per band, like `streaming.*`) |
| `terrain.synthetic_amplitude_m` | float 0..3 000 | 0 | Synthetic relief amplitude when `terrain_url` is empty |
| `terrain.dem_fallback_timeout_s` | float 0..60 | 3 | How long a job waits for its DEM tiles before meshing on the coarser ancestor |

The bands' hysteresis, look-ahead and clamp reuse `bands.hysteresis`, `bands.look_ahead_s`, `bands.look_ahead_clamp`. Validation follows section 3.2 of the contract (ranges, increasing edges, array length = zoom range). `default.json` and the schema gain the section and move to `profile_version` 5; a version 1–4 file loads strictly with the section at its defaults (terrain off); `simworld-cli validate-profile` covers it.

### 7.2 Runtime

| Addition | Shape |
|---|---|
| `set_terrain_detail(scale: float)` | **Superseded in api 1.10 (Terrain v2 G2): the scale multiplies the terrain cover's `terrain.tau_px`, nothing is re-meshed; see `docs/contract.md` section 3.** Originally: multiplies every band's `max_error_m` by `scale` (clamped to `[0.25, terrain.detail_scale_max]`; 1 = profile). Held terrain tiles are re-meshed and re-delivered under the same keys, nearest first, at the re-delivery rate; no teleport, the vector world is untouched (the lift rule already covers the whole range). A quality slider for the game's settings. |
| `configure_world` options `layers`, `terrain_url`, `terrain_encoding`, `terrain_attribution` | Section 4.1 |

Everything else (exaggeration, palette, shading, bands) is a `configure_world` (teleport) because it changes the vector meshes or the cover.

## 8. Contract changes (target: next free minor after the planned 1.x entries, additive)

| Item | Change |
|---|---|
| Version plan | New row for terrain at the next free minor (1.3 if night windows 1.0, clock 1.1 and imagery 1.2 land first); `profile_version` 5 |
| `configure_world` options | `layers` (opt-in, default `["world"]`), `terrain_url`, `terrain_encoding`, `terrain_attribution` (section 4.1) |
| Ready-tile dictionary | New keys `layer`, `terrain`, `h_min`, `h_max`; vector entries carry `layer: "world"` and an empty `terrain` Array |
| Tile keys | `"t/z/x/y"` for the terrain layer, emitted only when the host requested it; the visibility rule, fades and unloads apply per layer with the layer's root zoom |
| Section 2 wording | "Zooms 13..16 only" and "`ring = 16 − z`" become per layer: the vector layer keeps both, the terrain layer has zooms `far_zoom..near_zoom` and `ring` = index in `terrain.bands`; "the terrain is at `y = ground_height_at`" gets the TIN-vs-heightfield bound of section 4.4 |
| Determinism wording | A tile's settled output (after all re-deliveries) is deterministic; a first delivery made on a DEM fallback is not |
| `ground_height_at` | Returns the heightfield (near set, else the coarse field) when terrain is active, may refine as DEM tiles arrive; the drawn TIN differs by up to the band's `max_error_m × detail_scale`; still 0 when inactive or before the coarse field is loaded |
| `set_terrain_detail(scale)` | New method (section 7.2) |
| `attributions()` / `configure_world` result | Include the terrain credit |
| `status()` | `error` also when the terrain archive breaks its tileset contract (message names the source); `degraded` while DEM fallbacks are in use; `failed_fetches` and `last_error` cover both archives |
| `debug_stats()` | `terrain_queued`, `terrain_fetching`, `terrain_meshing`, `terrain_loaded` (PackedInt32Array per terrain ring), `terrain_tris` (int), `heightfield_mb` (float), `terrain_cache_mb` (float) |
| `debug_tiles()` | Entries gain `layer`; terrain entries use terrain rings |
| Section 7 / 9 wording | "Building bases stand on the terrain" and "ground y = terrain height + lift" become true; the lift rule (section 6.2) and the foundation are documented |
| Section 7 / 9 additions | Water snap, bridge interpolation and tunnel omission (section 6.2); optional road attribute `brunnel` in the tileset contract (section 10) |
| Profile | `profile_version` 5 with section `terrain` |

`cargo test` pins: `api_version`, the `#[func]` list (`set_terrain_detail`), `default.json` and the schema, and new golden hashes for the synthetic city with terrain active and `synthetic_amplitude_m` 120 (vector bands and terrain bands, settled output). Existing goldens stay valid because the default is off. `CHANGELOG.md` and `docs/contract.md` (sections 3, 4, 5, 7, 9, 12 and the version plan) change in the same commit as the code (T1 opens with the contract rows).

## 9. Budgets

Estimates at the default profile, Vilnius region, camera at 300–3 000 m; to be measured in T2/T3 and written back here as `docs/spec-tile-cache.md` does.

| Resource | Budget | Estimate |
|---|---|---|
| Heightfield cache (RAM) | ≤ 24 MB (profile) | Pinned coarse field ≤ 4 MB + near set 4–6 MB; transient tiles get the remaining ≥ 14 MB (≈ 56 tiles; in-flight far patches share most of their neighbours, and a job whose tiles cannot fit waits instead of evicting a tile another in-flight job holds). (Keeping every DEM tile the cover touches would be 350–400 tiles ≈ 95 MB, section 4.2) |
| Terrain meshes (GPU) | ≤ 16 MB | 200–260 resident tiles × (≈ 1 500 tris avg = 750 verts × 12 B + 1 500 × 6 B ≈ 18 KB) ≈ 4–5 MB; mountainous areas at the caps ≈ 260 × 8 192 tris ≈ 25 MB worst case |
| Draw calls | **Combined** vector + terrain within the design doc's frame cap (≤ 300 on iOS today) | Terrain: ≈ 70–120 frustum-visible tiles after the horizon cull (to measure); the vector world already uses most of the 300. Terrain does not fit under the current cap unchanged: either the cap is raised in the design doc's budget table (proposal: ≤ 420 with terrain on, measured on device in T4), or T3/T4 tiles are merged per parent, or the vector side gives up draws (e.g. ground merged into the building surface). Open decision, section 13 |
| Worker CPU per terrain tile | ≤ 20 ms (z ≤ 12), ≤ 3 ms (z14 crop) on the rayon pool | Delatin on 258² at 4 m error: low tens of ms in the Alps, < 2 ms on flat land; WebP decode 1–3 ms |
| Main-thread upload | within the existing 2 ms / 2 uploads per frame | A terrain tile is one `AddSurfaceFromArrays` of ≤ 100 KB |
| `ground_height_at` | ≤ 5 µs | One tile lookup + bilinear |
| Network, first flight | ≤ 30 MB for a 130 km radius | ≈ 400 DEM tiles × 20–80 KB |
| Disk | ≤ 200 MB (`terrain.cache_cap_mb`) | The whole Vilnius archive is ~150 MB |
| Extra vector-mesh cost | ≤ 10 % vertices in hilly tiles | Drape subdivision at 25 m on non-flat patches only |
| Time to first vector tile | ≤ + 1 range request vs terrain off | MVT and DEM fetched in parallel (section 4.3); measured in T1 |
| First fill, both layers | ≤ 6 s at 60 fps | ≈ 540 tiles at 2 per frame ≈ 4.5 s (section 5.1) |

## 10. Phases

| Phase | Work | Acceptance |
|---|---|---|
| **T0 Data** | `data/build_terrain.sh` (GLO-30 download + runway flattening + `massif` in Docker), `out/vilnius-terrain.pmtiles` served by `serve.sh`; attribution line recorded in `data/README.md`; road attribute `brunnel` in `data/schema/simplane.yml` (vector archive rebuilt); a second fixture area with real mountains and an airport (e.g. an Alpine valley: Austria extract through `build_tiles.sh` with `BOUNDS=`) for visual tests; verify reversed-Z on Mobile with far = 150 km | Archive opens with the `pmtiles` CLI, `show` reports webp and zooms 0–12; a z12 tile decodes to plausible heights (Gediminas hill ≈ 140 m); Vilnius airport runway heights along the axis are linear within 0.1 m; bridges over the Neris carry `brunnel=bridge`; a 10 km Z-fighting shot is clean |
| **T1 Heightfield** | `terrain` module: archive open + tileset check, DEM fetch through `Fetcher`/`DiskCache`, decoders, heightfield LRU with coarse field / near set / transient residency and `Arc` snapshots, patches, `Terrain::height`, synthetic field as in-memory DEM tiles; **replacing the free `geo::ground_height`** by patches in jobs and the `Terrain` handle on the main thread (section 4.4); `layers` option; profile section + schema (`profile_version` 5) + `validate-profile`; contract rows (version plan row, section 2 wording) and `CHANGELOG` | `cargo test`: decode round trip for both encodings, patch assembly on a DEM tile boundary (3×3 tiles for Z ≤ dem_zoom, 2 and 4 for crops), determinism of patches, goldens with synthetic relief; a profile with `terrain.enabled` but no `layers` option stays flat and warns once; bench overlay shows a live `ground_height_at` under the camera; vector world still flat-rendered but buildings now sit at the right heights; `first tile` time with terrain within section 9 |
| **T2 Terrain layer** | Scheduler generalised over layers; terrain cover, keys, horizon cull, cross-layer delivery order, Delatin + curvature seeds + skirts + caps, ready-tile `terrain`, `set_terrain_detail`; addon: `layers` option, terrain pool, `terrain.gdshader`, `WorldRenderSettings` palette (shared low colour), per-layer `TileCover` and ring stats, far plane and fog in the bench, `--terrain-url=`, `--terrain-detail=` | Headless smoke test prints a `first terrain tile` line; visible relief in both fixture areas; no holes or pops during the geodetic flight test at 250 m/s; no cracks at T2–T4 seams with `--curvature=1` and with `--curvature=50` plus `max_edge_m` lowered; resident and drawn terrain tiles logged at 300 m and 3 000 m AGL, and the draw-call decision (section 13) taken from them; worker CPU per tile within section 9 (logged by `Diagnostics`) |
| **T3 Drape** | Building bases + foundations, ground drape with subdivision and the lift rule, water snap, bridge interpolation and tunnel omission, light heights, deterministic slant boxes (coarse field, then `h_min`/`h_max`), `MAX_BOX_HEIGHT_M` with terrain, DEM fallback + re-delivery, status/debug keys | Shots on slopes show no floating roads or buried entrances beyond the lift; lakes are flat and their shores do not tilt up; Neris bridges span the valley; `golden_tests` for the synthetic relief (settled output) pass twice; `CoverSelfTest` and `--settings-selftest` pass with terrain on; measured budgets written into section 9 |
| **T4 Polish** | Smooth shading option, palette as shader globals (with the M6 window work), optional neighbour-edge stitching if skirts show from low altitude, `surface_height_at` if the TIN-vs-heightfield gap matters for landing, u16 heightfield if iOS memory needs it, national LiDAR for R0 if licensed | iPhone run inside the agreed combined draw-call cap and the M8 budgets with terrain on; a 15-minute recorded flight has no terrain-related frame spike > 4 ms |

## 11. Tests

- Unit: Terrarium and Mapbox decode (known pixels), patch assembly (3×3 DEM tiles for a z ≤ `dem_zoom` tile, 2 and 4 for crops on an ancestor's edge), Delatin on analytic fields (plane → 2 triangles; plane with `max_edge_m` → the seed grid and nothing else; sphere cap → error bound respected; cap respected), skirt topology (closed, outward), lift rule from a profile, horizon test (tile just inside / just outside, hysteresis, whole sibling groups only), water snap (shore vertex takes the lake level; same vertex in two tiles → same height), bridge run interpolation and tunnel omission, heightfield residency (pinned tiles never evicted, transient tiles dropped after their jobs, cap respected), band validation errors.
- Golden: synthetic relief (terrain active, amplitude 120 m) for every vector band and every terrain band, settled output; cover with fixed cameras including a 2 000 m synthetic peak in the T3 ring, computed after the coarse field is in and independent of which transient tiles happen to be cached (run once with a cold and once with a warm LRU, same cover).
- Integration (`lifecycle_tests`): `terrain.enabled` without the `layers` option → flat world, no `"t/…"` key, one warning; terrain archive that fails its contract → status `error`, vector world keeps running; DEM fetch failure → fallback mesh then re-delivery; `set_terrain_detail` re-delivers every held terrain key exactly once; teleport unloads both layers.
- Bench: `GeodeticFlightTest` and `CoverSelfTest` extended to the terrain layer (per-layer visibility rule); a `--terrain-selftest` that flies the Alpine fixture and asserts no frame holds a terrain tile whose parent and all children are visible at once.

## 12. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Delatin too slow at small errors on mountain z10/z11 tiles | Worker backlog, late far terrain | Triangle caps; coarser `max_error_m` for T3/T4; Martini fallback (section 3.4); measure in T2 before tuning |
| Cracks between terrain tiles visible from low altitude | Seams, light leaks | Skirts sized from the error; the one-sample border in patches; T4 edge stitching (shared edge vertices forced into both TINs) if still visible |
| Terrain draws push the frame past the design doc's 300-draw iOS cap | Frame time | Horizon cull; fewer bands (`terrain.bands` with T4 dropped); coarser near zoom (13); merging T3/T4 tiles into a per-parent mesh if the sibling-swap rule can be kept; raising the cap only with a device measurement (section 13) |
| Heightfield residency thrashes (far patches re-decoded from disk repeatedly) | Worker CPU, late far tiles | Transient tiles are kept while any in-flight job needs them; decode is 1–3 ms from the SQLite cache; raise `heightfield_cache_mb` on desktop |
| A vector tile arrives before the terrain under it | For one fetch, the city stands on relief over the −30 m fallback plane | Cross-layer delivery order favours terrain at equal distance (section 5.1); the gap is the same kind as today's late tiles |
| Horizon cull assumes a real-size Earth | Missing far terrain when the host renders a stronger or no curvature | `terrain.horizon_cull` / `horizon_margin_m`; the bench turns it off for `--curvature` ≠ 1 |
| Runway or bridge data missing or wrong in OSM | A bumpy runway, a road diving into a valley | Both are build-time data and fixable in the archive; without `brunnel` everything drapes as ground |
| Draped roads float by the lift on steep R2 slopes | Visible at 2.5 km on hillsides | Lift bound is sub-0.2°; lower `detail_scale_max` to 1 (lifts halve) if the runtime knob is not needed |
| Copernicus licence wording changes or the public 30 m set withholds a target country | Attribution or coverage gap | Keep the credit line in `terrain_attribution` (data, not code); GLO-90 or AW3D30 for withheld tiles |
| Heights change `ground_height_at` under the camera while tiles load | AGL jumps in the host (coarse field → 30 m data) | Documented; the near set is fetched first; the host reads AGL from its flight model when it has one, the library only needs it for the cover |
| `ground_height_at` differs from the drawn TIN | Wheels float or sink by up to the band error at touchdown | T0 error 0.5 m by default; runways flat in the data; `surface_height_at` (section 13) if it still shows |
| `h_max` unknown before the patch loads mis-bands far mountain tiles briefly | A late far tile | Coarse-field box first (deterministic); the hysteresis absorbs the rest |

## 13. Open decisions

- **Draw-call cap with terrain on:** the design doc caps the frame at 300 draws on iOS and the vector world uses most of it. Options: raise the cap (proposal ≤ 420, needs a device measurement), merge T3/T4 tiles per parent, or save draws on the vector side. Decide from the T2 measurements of drawn tiles at 300 m and 3 000 m AGL; the decision updates the design doc's budget table.
- **`surface_height_at`:** an exact height of the delivered TIN (needs the host-side mesh or a copy of the TIN kept in Rust per near tile) for landing gear, or keep only the heightfield with the documented bound. Keep only the heightfield unless T3 shots show the gap at touchdown.
- **Heightfield storage:** `f32` (simple, 256 KiB per tile) or u16 fixed point (half). Start with `f32`, decide after the iOS measurement.
- **Vendored Delatin vs the `delatin` crate:** vendored (section 3.4) unless the crate gains a triangle cap and stable docs.
- **Key syntax** `"t/z/x/y"` vs a separate `layer` argument on `poll_*`: the prefixed key is additive and keeps the three poll calls unchanged (old hosts never see it thanks to the `layers` opt-in); the alternative is cleaner but a bigger host change.
- **DEM tile size:** 256 px (default) or 512 px (4× fewer requests, 4× bigger cache entries). 256 unless network latency dominates on device.
- **Runtime `set_terrain_detail`:** keep (needs the `detail_scale_max` lift bound) or drop (lifts halve, quality changes need a teleport).
- **Terrain beyond 130 km:** nothing, a host sky dome with a painted horizon, or a one-mesh z8 ring without fades. Decide after T2 screenshots at 3 000 m.
- **Water:** rely on Copernicus water editing (lakes flat, rivers monotonic) plus the water snap of section 6.2, or burn vector water polygons into the DEM at build time like the runways (section 3.3). Start with the snap; burn offline if T3 shots show terrain facets poking through lakes.

## 14. World seed (`docs/spec-world-seed.md`)

The world seed (M10, proposal) replaces the vector archive with generated tiles over a whole region. Revised 2026-10-27 for the procedural terrain (no DEM, no build step). With a seed world:

1. **The terrain layer is required.** `seed_world` without the host layer `terrain` and `terrain.enabled` is a configuration error; the terrain tiles (out to 130 km) are the surface beyond the vector cover. The seed spec has no far layer of its own.
2. **Runways in the height function.** No DEM flattening: the seed world hands the terrain a list of ruled strips from its `airports.json` (with `airports.overrides`), one per open runway, height linear between the field's own heights at the two landing thresholds, constant across, blended over `seed.airport.shoulder_m` like the flat zone; the rest of the fence takes the nearest strip's height. `elevation_ft` is informational. One runway source for the drawn runway, `ground_height_at`, the terrain mesh and the game. Without a seed world nothing changes.
3. **Water in the height function.** The seed world's z11 water index plus a distance raster (seed spec section 5.2a): flat at `sea_level_m` inside the sea, flat at each lake's level inside lakes, land blended down to the water over `seed.water.blend_m`. Without it the noise raises hills in the sea.
4. **No configure-origin flat zone.** It depends on where the session started; with a seed world `flat_radius_m` / `flat_blend_m` must be 0. The strips and the water rule are data, so heights stay independent of the session.
5. **Terrain COLOR.** Seed-world terrain tiles carry COLOR (`r` = ground class / 8, `g` = tint / 8) from the same water test and countryside noise as the vector ground (contract 9a change, api 1.9); `terrain.gdshader` colours by class. Non-seed terrain tiles keep no COLOR. The S1 screenshots showed bleeding at the coast. Built instead of breaklines: per-triangle class resolution at the coast, with duplicated vertices and no extra triangles (seed spec 5.5, 11e).
6. **Draping.** Seed tiles carry R3 ground under the existing `ground.r3` rules; the lift rule already covers R3. **Inside airport fences there is no drape lift** (S1 fix): the lift is weighted by the strip rule's blend `k` (0 in the fence, smoothstep over the shoulder), and the TIN's per-sample error bound is scaled by the same `k` (at least 1 cm). So the drawn runway is `ground_height_at` + its stacking offset, and the terrain never pokes through (contract section 9 "Airports"). Airport lights stand on the drawn surface.
7. **Colour transition at the vector reach** (section 6.2): unchanged, at R3 (25 km); the terrain class colours match the R3 ground there.
8. **Heights in generation.** Seed decisions (slope → park, contour streets) read the pure height function directly, quantized to 1 cm; there is no load state or fallback to avoid.

## 15. Sources

- Copernicus DEM on AWS Open Data: <https://registry.opendata.aws/copernicus-dem/> (bucket `copernicus-dem-30m`, licence annex linked from the registry YAML: <https://github.com/awslabs/open-data-registry/blob/main/datasets/copernicus-dem.yaml>); licence discussion: <https://forum.sentinel-hub.com/t/copernicus-dem-license-update-on-aws/3815>
- AWS Terrain Tiles (Mapzen / tilezen): <https://registry.opendata.aws/terrain-tiles/>, formats: <https://github.com/tilezen/joerd/blob/master/docs/formats.md>, attribution: <https://github.com/tilezen/joerd/blob/master/docs/attribution.md>
- `massif` terrain-RGB tiler (Rust, PMTiles output): <https://github.com/mapriot/massif>; `rio-pmtiles`: <https://pypi.org/project/rio-pmtiles/>; GDAL discussion on terrain-RGB and WebP: <https://lists.osgeo.org/pipermail/gdal-dev/2026-January/061292.html>
- Delatin (Rust port of Agafonkin's delatin / Fogleman's hmm, Garland–Heckbert 1995): <https://docs.rs/delatin/latest/delatin/>, <https://github.com/pokhanto/delatin-rs>; Martini RTIN port: <https://docs.rs/crate/martini/latest>
- Cesium quantized-mesh (the pre-built alternative): <https://github.com/CesiumGS/quantized-mesh>, MapTiler Terrain 3D: <https://docs.maptiler.com/schema-raster/terrain-3d/>, Rust codec: <https://docs.rs/crate/quantized-mesh/0.2.1>
- Godot: shading language (instance uniforms, `flat`, GLSL ES 3.0 built-ins): <https://docs.godotengine.org/en/stable/tutorials/shaders/shader_reference/shading_language.html>; renderers overview: <https://docs.godotengine.org/en/4.5/tutorials/rendering/renderers.html>; `Mesh.ARRAY_FLAG_COMPRESS_ATTRIBUTES`: <https://docs.godotengine.org/en/stable/classes/class_mesh.html>
- PMTiles raster tile types in the `pmtiles` crate: <https://docs.rs/pmtiles/latest/pmtiles/enum.TileType.html>
