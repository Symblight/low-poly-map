# WorldStreamer contract

api_version: 0.6

This is the only full statement of the contract between the SimWorld library (the Rust `WorldStreamer` GDExtension node, crates `simworld-core` + `simworld-godot`) and any host that displays it (today the C# test bench in `low-poly-map/`, later the SimPlane game). Everything a host may rely on is here; everything else is an implementation detail.

The line `api_version: <major>.<minor>` above is checked by `cargo test` against `simworld_core::API_MAJOR` / `API_MINOR`, and every `#[func]` of `WorldStreamer` must appear in this file. Each section and table row carries the `api_version` that introduced it (**[0.5]**, …).

## 1. Versioning and change rules [0.5]

- `api_version` is the contract version `major.minor`. It is independent of the crate version (`debug_stats()["version"]`) and of the crate patch level.
- **Minor bump = additive only:** a new method, signal, dictionary key or optional argument. Existing methods, keys, encodings and guarantees keep their meaning. A host written for `0.x` works on any later `0.y` with the same major.
- **Major bump = breaking:** a removed or renamed method/key, a changed type, encoding or guarantee. A host must refuse to drive a library with a different major.
- **Host check:** read `api_version()` once after creating the node (or `debug_stats()["api_major"/"api_minor"]`). A library without either predates `api_version` (crate 0.5.0-spike); hosts then fall back to probing method names. A lower minor than the host expects means the features introduced after it are missing; the host should say which ones.
- **Changing the contract:** any change updates this file, `API_MAJOR`/`API_MINOR` in `rust/simworld-core/src/lib.rs` and `CHANGELOG.md` in the same commit. New items get the new version tag. The agent files (`.claude/agents/*.md`) only point here.

### Version plan

Planned items (section 12) carry target versions. They are assigned in this order; if work lands in a different order (M6/M7 run in parallel after P2), the item that lands takes the next free minor and this table is updated.

| api_version | Content | Phase |
|---|---|---|
| 0.5 | Everything built today (sections 3–11), plus `api_version()` and `debug_stats` `api_major`/`api_minor` | baseline + P2 |
| 0.6 | `configure_world(options)`, `world_profile()`, the world profile (section 3.2); `configure(...)` becomes a wrapper (**built**) | P3 Config |
| 0.7 | `update_camera_geodetic`, `geodetic_to_local`, `local_to_geodetic`, signal `origin_shifted`, `attributions()`, `shutdown()`, `status()` + signal `status_changed`, lifecycle rules (several instances, re-configure teleport, cache lock) | P4 Host API |
| 0.8 | `set_clock`, `daylight()` | M6 Night |
| 0.9 | `configure_imagery`, ready-tile key `imagery`, ground `TEX_UV`, R3 imagery ground quad | M7 Imagery |

## 2. Coordinates and units [0.5]

- **Local frame = Godot axes:** Y-up, **x = east, y = up, z = −north** (−Z faces north). Meters. Angles in degrees, WGS84 lat/lon.
- Rust keeps all world math in f64 (fixed ENU frame at the configured origin). Everything handed to the host is f32/f64 **relative to the current floating origin**.
- **Floating origin:** when the camera's horizontal local distance `hypot(x, z)` from the origin exceeds **5000 m**, `update_camera` moves the origin onto the camera (translation only, never rotation; the shift's `y` is always 0). The shift is reported once by `take_origin_shift()`; the host subtracts it from the camera and every tile root in the same frame. All positions handed out after the `update_camera` call that re-centered (`poll_ready` origins, `debug_tiles` rects, `ground_height_at` input) are in the post-shift frame.
- `debug_stats()["origin_lat"/"origin_lon"]` is the WGS84 position of the *current* origin (it moves with re-centering).
- **Tile keys** are Strings `"z/x/y"` (slippy-map / XYZ scheme, y down from the north, Web Mercator). Zooms 13..16 only. `ring = 16 − z` (R0 = z16 … R3 = z13).
- Mesh positions, light positions and ground positions are relative to the tile's `origin` (tile center, y = 0).
- **Triangle winding:** clockwise seen from the front (Godot's front face), for buildings and ground.

## 3. Methods [0.5]

Class `WorldStreamer` extends `Node`, registered by the GDExtension (entry symbol `gdext_rust_init`). It is not a tool class (runs in-game only). Create it with `ClassDB.Instantiate("WorldStreamer")` and call it through `GodotObject.Call(...)`, from the **main thread only**. Every method is non-blocking; `update_camera` and `poll_*` target < 0.5 ms. Before `configure` / `configure_world` every method is a harmless no-op returning an empty value (`api_version` works always; `world_profile` returns the defaults). Errors never panic across FFI: they are logged (`godot_warn!` / `godot_error!`, rate-limited) and retried with backoff.

| Method | Since | Purpose |
|---|---|---|
| `api_version() -> Vector2i` | 0.5 | Contract version `(major, minor)`, see section 1. Constant; callable before `configure`. |
| `configure(origin_lat: float, origin_lon: float, tile_url: String, cache_dir: String)` | 0.5 | Set the world origin (WGS84 degrees) and data source, (re)start the workers. `tile_url` (trimmed): an `http://` or `https://` URL of a `.pmtiles` v3 archive (section 10); **empty = the offline deterministic synthetic city**. `cache_dir`: absolute folder for the disk tile cache (empty = no disk cache). Calling it again drops the old workers and cover; every tile delivered before is returned by the next `poll_unload` (the cache is kept). **[0.6]** A wrapper over `configure_world` with these four keys and the **default** world profile (a profile set by an earlier `configure_world` is not kept); errors (e.g. a non-http URL, an origin outside ±85.05° / ±180°) are logged and leave the previous configuration running. |
| `configure_world(options: Dictionary) -> Dictionary` | 0.6 | Like `configure`, plus the world profile and diagnostics. Options and result: section 3.1. Validates everything first: on an error nothing changes and the result says why. |
| `world_profile() -> Dictionary` | 0.6 | The resolved world profile (section 3.2) as a Dictionary with exactly the keys of `addons/simworld/profiles/default.json` (objects → Dictionary, arrays → Array, integers → int, other numbers → float, names → String). Defaults until a `configure_world` with a profile; a later `configure` resets it to the defaults. For shader globals and debug UI. |
| `update_camera(local_pos: Vector3, velocity: Vector3, alt_agl_m: float)` | 0.5 | Call every frame first. `local_pos`: camera in the local frame — only **x and z** are used; `y` is ignored, the height comes from `alt_agl_m`. `velocity`: m/s in local axes (vertical speed included, it feeds the look-ahead). `alt_agl_m`: camera height above ground (m); negative or non-finite values count as 0. Applies worker results, re-centers if needed (section 2), recomputes the cover, advances fades and queues jobs. |
| `take_origin_shift() -> Vector3` | 0.5 | Non-zero exactly once after a re-center (`(dx, 0, dz)`), else `Vector3.ZERO`. Subtract it from the camera and every tile root in the same frame. |
| `poll_ready(max: int) -> Array[Dictionary]` | 0.5 | Up to `max` finished tiles (`max < 0` counts as 0), finer bands and nearer tiles first. Shape: section 4. At most 1 light-mode re-delivery per frame (section 8). |
| `poll_fades() -> Dictionary` | 0.5 | Key (String `z/x/y`) → float `fade` for every tile in a LOD transition. Section 6. |
| `poll_unload() -> PackedStringArray` | 0.5 | Tile keys the host must remove (buildings, ground and lights of that key). |
| `ground_height_at(local_x: float, local_z: float) -> float` | 0.5 | Terrain height (m) under a point in current local x/z (through the current floating origin). **Returns 0 until a DEM exists.** A few µs; fine once per frame (e.g. height above ground). Non-finite input → 0. |
| `debug_stats() -> Dictionary` | 0.5 | Overlay counters, see below. |
| `debug_tiles() -> Array[Dictionary]` | 0.5 | One entry per tracked tile, see below. For overlays (e.g. 4× per second), not per frame. |

**Per-frame call order [0.5]:** `update_camera` → `take_origin_shift` → `poll_ready` → `poll_fades` → `poll_unload`. Re-centering and fade progress happen inside `update_camera`. A fade started by a `poll_ready` delivery is already in the same frame's `poll_fades`; a fade that completes is reported by `poll_fades` and its outgoing tiles are in the same frame's `poll_unload`.

**`debug_stats()` keys:**

| Key | Type | Since | Meaning |
|---|---|---|---|
| `queued`, `fetching`, `meshing`, `loaded` | PackedInt32Array (length 4, indexed by ring) | 0.5 | Tiles per state and ring (`loaded` = delivered and held by the host) |
| `cache_mb` | float | 0.5 | Disk cache size (MiB) |
| `origin_lat`, `origin_lon` | float | 0.5 | WGS84 position of the current floating origin |
| `version` | String | 0.5 | Display only: crate name, version and build marker, e.g. `simworld-godot 0.5.0 (build debug @...)`. Never parse it; use `api_version`. |
| `api_major`, `api_minor` | int | 0.5 | Same as `api_version()` |

Hosts must ignore unknown keys (new keys are additive).

**`debug_tiles()` entry keys:** `key` (String `z/x/y`), `ring` (int 0–3), `state` (String: `queued`, `fetching`, `meshing`, `ready`, `failed`, `unloading`; `ready` also covers tiles delivered and held by the host), `min`, `max` (Vector2: tile rect corners in current local x/z meters).

### 3.1 `configure_world` options and result [0.6]

`options` keys (unknown keys are logged as a warning and ignored, so a host written for a later minor can pass newer keys):

| Key | Type | Required | Meaning |
|---|---|---|---|
| `origin_lat`, `origin_lon` | float (int accepted) | yes | World origin, WGS84 degrees: latitude within ±85.0511 (Web Mercator), longitude within ±180 |
| `tile_url` | String | no (default `""`) | As in `configure`: `http(s)://` `.pmtiles` URL, empty = synthetic city. Any other scheme is an error. |
| `cache_dir` | String | no (default `""`) | As in `configure` |
| `profile_path` | String | no | World profile JSON file (`res://`, `user://` or an OS path; read through Godot's file system). Partial files are fine. Omitted = the defaults. |
| `profile_overrides` | Dictionary | no | Partial world profile merged onto the file (or the defaults): same keys and nesting as the JSON, objects merge deeply, arrays and scalars replace. Integral floats are accepted for integer keys (Godot's JSON parser yields floats); a `Color` is accepted for an rgb key (alpha ignored). |
| `diagnostics` | Dictionary | no | `profile` (bool: log a phase breakdown for main-thread calls slower than 1 ms), `warnings_per_second` (int 1..1000, default 5). Omitted keys keep their current values (initially the host defaults, e.g. `SIMPLANE_PROFILE=1`). |

Result Dictionary (always all keys):

| Key | Type | Meaning |
|---|---|---|
| `ok` | bool | The configuration was applied |
| `error` | String | Why not (empty when `ok`). Profile errors name the key, the value and the allowed range, e.g. ``profile: bands.hysteresis = 3 is out of range [1.01, 2]`` or ``profile_overrides: unknown key `bands.r0_edge` ``. |
| `api_version` | Vector2i | Same as `api_version()` |
| `attributions` | PackedStringArray | Attribution strings the host must show while map data is visible: `["© OpenStreetMap contributors"]` for a `tile_url` source, empty for the synthetic city. On failure, those of the configuration still running. |

On success it behaves like `configure` (teleport): the old workers and cover are dropped, every tile delivered before is returned by the next `poll_unload`, the disk cache is kept. On failure nothing changes (the previous configuration keeps running, nothing is unloaded).

**Tileset contract check [0.6]:** the archive is opened asynchronously (a warm cache works offline, so `configure_world` never waits for the network). When it opens, its header zoom range must cover 13..16 and its metadata `vector_layers` must contain `buildings`, `landuse` and `roads`. On a mismatch an error naming the problem is logged once (`godot_error!`) and every fetch from that archive fails (logged, rate-limited, retried with backoff); `configure_world` itself has already returned `ok`. The synthetic city is exempt.

### 3.2 World profile [0.6]

Every tuning value of the world is data: band edges, hysteresis and look-ahead; building and ground Douglas-Peucker tolerances and minimum areas; fetch and mesh concurrency, re-delivery pacing and retry backoff; light carpet altitudes and cell size; the fade safety cap; the re-center distance; cache cap and HTTP timeouts; light profiles (building kinds → profile, sprite colors, landuse fallback); road classes (highway values), widths and link rule; area groups (landuse kinds → class and stacking level) and the per-band lift step.

- **Files:** `addons/simworld/profiles/default.json` lists every key with its default (= the compiled-in defaults, the values in sections 2, 5, 8, 9 and 10; pinned by `cargo test`). `addons/simworld/profiles/world-profile.schema.json` (JSON Schema 2020-12) documents every key with units and allowed range. The core checks values itself (`simworld-cli validate-profile <file>` runs that check); the schema is for editors and humans.
- **Partial profiles:** every key is optional; omitted keys keep their defaults. Objects merge deeply (`profile_path` file first, then `profile_overrides`), arrays and scalars replace. `ground.areas` is a map with free group names (a new name adds a group).
- **`profile_version`** (int, today **1**): a file (or overrides) with the library's version or older is strict, an unknown key is an error (a typo). A newer version loads: unknown keys are logged as warnings and ignored, known keys apply. The resolved profile (`world_profile()`) always carries the library's version.
- **Validation:** out-of-range values, wrong types, `r0_edge_m < r1_edge_m < r2_edge_m < r3_edge_m` violations, `cluster_off_agl_m > cluster_on_agl_m`, `retry_max_s < retry_base_s`, a building kind in two light profiles, a landuse kind in two area groups or a highway value in two road classes are errors.
- **Not in the profile (structural, part of the tileset contract):** 4 bands over zooms 13..16 (`ring = 16 − z`), the layer names, the COLOR encodings (section 7, 9), the fade quantum, the light intensity formula (section 8).
- A profile change never serves stale geometry: only raw tile bytes are cached (section 10).

## 4. Ready-tile dictionary [0.5]

Every entry of `poll_ready` has all of these keys, on first delivery and on every re-delivery:

| Key | Type | Meaning |
|---|---|---|
| `key` | String | `"z/x/y"` |
| `ring` | int | 0–3 (= 16 − z) |
| `origin` | Vector3 | Tile root (tile center) in the current local frame; `y` = 0 |
| `mesh` | Array | Building mesh in `Mesh.ARRAY_MAX` layout (section 7), or an **empty Array** (size 0) for R3 and tiles without buildings |
| `ground` | Array | Ground mesh in `Mesh.ARRAY_MAX` layout (section 9), or an **empty Array** for R3 and tiles without ground features. Always sent: an empty Array means "this tile has no ground" (drop any previous one), never "keep the current one" |
| `lights` | PackedVector3Array | Light sprite positions relative to `origin` (R2/R3 only; empty for R0/R1) |
| `light_colors` | PackedColorArray | One per light (section 8) |

Mesh Arrays (non-empty) have size `Mesh.ARRAY_MAX`; filled slots are `VERTEX` (PackedVector3Array, meters relative to `origin`), `NORMAL` (PackedVector3Array), `COLOR` (PackedColorArray), `TEX_UV` (PackedVector2Array; buildings only, nil for ground) and `INDEX` (PackedInt32Array, triangles). All other slots are nil. The Array can go straight into `ArrayMesh.AddSurfaceFromArrays(PRIMITIVE_TRIANGLES, …)`.

**Re-delivery:** a key that is already uploaded can be delivered again (light-mode switch, section 8; coarsening, section 5). The host replaces that key's mesh, ground and lights in place: a delivery of a held key always replaces, it never adds a second copy.

## 5. LOD cover, guarantees and visibility rule [0.5]

**Cover (default values; tunable through the world profile `bands`, `buildings`, `ground` since 0.6).** Rust selects tiles as a quadtree cover by 3D slant distance `d = √(horizontal² + (cam_alt − ground)²)` to the nearest point of the tile box (footprint × the tile's max building height):

| Band | ring | Zoom | Slant distance | Buildings | Ground | Lights |
|---|---|---|---|---|---|---|
| R0 | 0 | 16 | < 0.6 km | full extrusion, DP 0.5 m | all roads and areas | none (windows) |
| R1 | 1 | 15 | 0.6–2.5 km | extrusion, DP 2 m, drop < 20 m² | no minor paths, drop areas < 200 m² | none (windows) |
| R2 | 2 | 14 | 2.5–7 km | merged coarse blocks, DP 4 m, drop < 50 m² | no class-6 roads, drop areas < 2500 m² | yes |
| R3 | 3 | 13 | 7–25 km | none | none | yes |
| — | — | — | > 25 km | nothing requested | | |

Each location is shown by the band its slant distance falls in; candidates lie within horizontal radius `√(25 km² − h²)` of the look-ahead point (`h` = camera height). Look-ahead point per band = camera + velocity × 20 s (vertical speed included), offset clamped to half the band's radius. Work is prioritized finer band first, then by slant distance to the look-ahead point, with at most 4 fetches and 2 mesh jobs in flight per band. Hysteresis: a tile changes band only once its slant distance is 15 % past the band edge (z13 roots are added below 25 km and dropped beyond 28.75 km). Bands are chosen by slant distance only; there are no altitude rules, and the host has none either. Coarsening is bottom-up, so the two sides of any swap differ by exactly one zoom level.

**Guarantees from Rust [0.5]:**

1. Refining always requests all 4 children of a tile as one sibling group.
2. Every requested tile is delivered through `poll_ready`, even when empty (empty `mesh`/`ground` Arrays, empty `lights`), so sibling groups always complete.
3. **Refine:** a parent is unloaded only after all 4 of its children have been delivered **and** the swap fade (section 6) has completed. The fade starts in the `poll_ready` call that delivers the last child; the parent is unloaded in the frame its final 0.0 is reported.
4. **Coarsen:** the parent is delivered again, fades in while its children fade out, and the children are unloaded (in one `poll_unload` batch) in the frame the fade completes. While this happens, the children's subtree is frozen (no new requests or deliveries in it).
5. Delivery order: a tile is handed out only when its parent is covered (present, with its own sibling group present up to the root) and its parent is not in a fade, so a new sibling group never overlaps a visible coarser tile and every tile takes part in at most one fade at a time.
6. **No-fade exceptions:** a z13 root leaving the cover takes its whole subtree in one `poll_unload` batch with no fade; a coarsening parent that is still uploaded while its child group is incomplete (children not visible) needs no fade, and the subtree is unloaded at once.

**Visibility rule (evaluated by the host, locally from keys) [0.5]:**

- A tile is *present* if it is uploaded, or all 4 of its children are present (recursive).
- A tile without a `poll_fades` entry is visible iff it is uploaded, **and** (z == 13 or all 4 of its siblings are present), **and** not all 4 of its children are present.
- A tile that has a `poll_fades` entry is visible whenever it is uploaded, regardless of the sibling/children clauses (both sides of a swap show at once).
- New tiles stay hidden until `poll_fades` has run once after their upload, so the shader `fade` is set before the first visible frame.
- The host never hides tiles any other way (no distance or altitude culling of its own); the coarser tile stays until Rust unloads it.
- The ground mesh and the light sprites of a key follow the same visibility as its building mesh.

History: this replaces the M3 `quadrants` ready-tile key, which no longer exists. The first M4 draft of the rule said "uploaded" where it now says "present" and left holes once one sibling was refined.

## 6. Dithered LOD fades [0.5]

A sibling swap (parent ↔ its 4 children, either direction) is not instant: both sides exist together while `fade` ramps. Rust adds no geometry.

- `fade` = the tile's visible fraction (0..1). The incoming side goes 0→1, the outgoing side 1→0, and the two always **sum to exactly 1** (values are quantized to 1/256, so `f` and `1 − f` are exact in f32).
- `fade` is driven by slant distance, not time: it ramps across the band's hysteresis gap (15 % of the finer band's edge), so hovering freezes a transition. It only moves forward; if the split decision flips mid-fade, the fade reverses from its current value (no pop). Rust force-completes any transition 3 s after it started (safety cap). At 250 m/s the overlap is ~0.5 s.
- `poll_fades()` returns key → float for **every** tile currently in a transition, changed or not, so the host may replace its whole map each call. A tile not in the dictionary has fade 1.
- Incoming tiles get one last entry with 1.0 on the call where they finish; that call is also where the host drops the entry. Outgoing tiles get a last entry with 0.0 and are sent through `poll_unload` in the same frame.
- A tile delivered by `poll_ready` that starts a transition is already in the next `poll_fades` result with its starting value (0.0 for incoming).

**Dither rule (host shader; buildings and ground, never light sprites):** per mesh instance, uniform `fade` (float) and `fade_parity` = `z & 1` of the tile key (the two sides of a swap differ by one zoom, so their parity differs). Screen-pixel pattern `p` in [0,1) from a 4×4 Bayer matrix of `FRAGCOORD.xy`. Parity 0 keeps a pixel when `p < fade`; parity 1 keeps it when `p >= 1.0 - fade`. With fade_in + fade_out = 1 the two sides cover complementary pixels. At `fade == 1` the shader skips the test (no discard), so opaque tiles keep early depth rejection.

## 7. Building mesh [0.5]

One merged mesh per R0–R2 tile (one draw call). Buildings that cross tile edges are clipped exactly to the tile rectangle; a tile's 4 children cover exactly the same geometry as the tile itself (no doubles, no holes across a swap). Walls are flat-shaded quads with outward normals; roofs are flat earcut polygons with holes, normal +Y. A raised part (`min_height` > 0.25 m) also gets a bottom cap with normal −Y. Building bases stand on the terrain (`ground_height`, lift 0).

**Vertex COLOR:**

| Channel | Meaning |
|---|---|
| `r` | light profile id / 8: 0 residential, 1 office, 2 retail, 3 industrial, 4 civic. Id 5 ("unknown") is reserved: unknown kinds are resolved through the surrounding landuse before encoding, so the mesh only carries 0–4. |
| `g` | seed 0..1, a stable hash of the OSM id and type (for lit windows) |
| `b` | floor-to-floor height / 10 m (clamped 0..1; typically 2.5–5 m) |
| `a` | building roof height / 400 m (clamped 0..1) |

**TEX_UV (meters):**

- Walls: `u` = meters along the outline ring, starting at 0 for each ring (outer ring and each hole); `v` = meters up from the bottom of the extrusion (the part's `min_height`, not the ground).
- Roofs and bottom caps: `(x, z)` of the vertex in tile-local meters (relative to `origin`), not world meters.

## 8. Lights [0.5]

- `lights` / `light_colors` are filled for R2 and R3 tiles only (empty for R0/R1, which get windows).
- One light per building at its footprint centroid and roof height (a building split across tiles counts once).
- `light_colors`: rgb = average color of the lights in the point, a = intensity weight `clamp(sqrt(count) / 8, 0.125, 1)`; a single light has count 1 (a = 0.125). Colors by profile (informative): residential warm 2700 K, office cool 4500 K, retail bright white, industrial sodium, civic warm floodlight ~3000 K.
- **Light carpet:** above ~3000 m AGL (on at 3300 m, off below 2700 m) each tile's lights are clustered into 100 m cells (mean position, mean roof height, average color, `count` = lights in the cell). On every mode switch Rust re-delivers already-uploaded R2/R3 tiles under the same key with the other light set (and the same mesh and ground); the host re-uploads in place. Re-deliveries are limited to 1 per frame.

## 9. Ground [0.5]

One flat ground mesh per R0–R2 tile: landuse/park/water areas plus buffered road strips, relative to `origin`, clipped to the tile, same sibling-swap visibility as the building mesh. NORMAL = +Y; triangles clockwise seen from above; no TEX_UV (until 0.9).

**Ground COLOR (exact multiples):**

| Channel | Meaning |
|---|---|
| `r` | class / 16: 0 grass/park, 1 forest, 2 water, 3 residential, 4 commercial, 5 industrial, 6 other landuse, 8 road |
| `g` | lit: 1 for lit roads (`lit` = yes, 24/7, automatic, sunset-sunrise, dusk-dawn, limited, interval, night), else 0; always 0 for areas |
| `b` | road class / 8 (0 for areas) |
| `a` | 1 |

**Road classes** (`b` × 8) and full strip widths: 1 motorway (14 m), 2 trunk + primary (10 m), 3 secondary (8 m), 4 tertiary (6 m), 5 residential + unclassified + living_street (6 m), 6 everything else (service, pedestrian, footway, path, cycleway, track, steps, …; 3 m). `_link` roads keep their class at 60 % width (min 3 m). R0 has all roads; R1 drops minor paths (footway, path, cycleway, steps, bridleway, track, corridor, elevator, via_ferrata, escalator); R2 drops class 6.

**Area classes** from the `landuse` layer `kind`: grass/park = park, garden, playground, pitch, golf_course, recreation_ground, nature_reserve, grass, meadow, grassland, scrub, heath, wetland, village_green, cemetery, allotments, orchard, vineyard, farmland, plant_nursery, flowerbed; forest = forest, wood; water = water, riverbank, reservoir, basin, salt_pond; residential; commercial = commercial, retail; industrial = industrial, railway, port, depot, garages, quarry, landfill; other = any other kind. R1/R2 drop areas under 200 / 2500 m².

**Heights:** ground y = terrain height + a small lift per stacking level against z-fighting. Levels bottom to top: nature_reserve, other landuse, residential, industrial, commercial, grass/park, forest, water, then roads from class 6 up to motorway. Lift = level × 0.02 m (R0), 0.06 m (R1), 0.15 m (R2); at most ~2 m at R2. All Rust heights (building bases, light points, ground vertices, the slant-distance query, `ground_height_at`) go through one internal `ground_height(lat, lon)`, which returns 0 until a DEM spike; a DEM changes the values, not the API.

## 10. Data source, cache, attribution [0.5]

- Tile source: a self-hosted PMTiles v3 archive (planetiler profile in `data/`) with layers `buildings` (keeping building `kind`), `landuse` and `roads`, zooms 13–16, fetched with HTTP range requests (rustls; no OpenSSL). Never public OSM tile servers.
- Disk cache under `cache_dir` (per source URL), raw tile bytes keyed by `z/x/y`, LRU capped at 300 MiB (profile `cache.cap_mb` since 0.6), survives restarts and works offline when warm. It is wiped when the archive's header fingerprint or the cache schema version changes. Meshes are never cached, so a meshing change never serves stale geometry.
- Attribution "© OpenStreetMap contributors" must be shown by the host whenever map data is visible (returned by `configure_world` since 0.6; `attributions()` from 0.7).
- The archive must match the tileset contract (zooms 13..16, layers `buildings`, `landuse`, `roads`); checked when it opens (section 3.1). **[0.6]**

## 11. Determinism and threading [0.5]

- Same tile → same mesh, same ground, same lights and same lit windows: everything is seeded by OSM id, with no time-based or process-random state. Output order is deterministic. Pinned since 0.6 by golden hashes in `cargo test` (`simworld-core` `golden_tests`: per-tile hashes of the synthetic city for all 4 bands, positions quantized to 1 mm, colors to 8 bits, per-building output sorted by OSM id; plus the cover for fixed cameras). P6 adds the real-data fixture and both CPU architectures.
- The library never calls Godot APIs off the main thread; workers produce plain Rust structs and conversion to Packed arrays happens inside `poll_ready` on the calling thread.

**Lifecycle today [0.5]:** one node = one world. Re-calling `configure` or (0.6) a successful `configure_world` is a fresh start (see `configure`); a failed `configure_world` changes nothing. Freeing the node stops the workers in the background (no wait, no cache flush). Several nodes in one process are not specified yet and must not share a `cache_dir` (rules arrive in 0.7: a lock on `cache_dir/.simworld.lock` taken synchronously in `configure_world`, see the planned lifecycle rules below).

## 12. Planned API (not built)

Shapes below are targets from `docs/spec-library-packaging.md` (section 4) and `docs/spike-streamed-osm-city.md`. Until the stated `api_version` is reported, hosts must not call them. Details may still change before they land; once landed they follow section 1.

### 0.7 — Host API and lifecycle (P4)

| Addition | Shape |
|---|---|
| `update_camera_geodetic(lat: float, lon: float, alt_msl_m: float, vel_ned: Vector3)` | Same path as `update_camera`, from geodetic input. (a) `lat`/`lon` WGS84 geodetic. (b) Altitude above MSL (ellipsoid height must be converted by the host, unless an `alt_wgs84_m` variant is added). (c) AGL = altitude − `ground_height_at`. (d) NED maps to local as x = E, y = −D, z = −N. |
| `geodetic_to_local(lat: float, lon: float, alt_msl: float) -> Vector3` | Position in the current floating-origin frame |
| `local_to_geodetic(local: Vector3) -> Vector3` | `(lat, lon, alt)` |
| signal `origin_shifted(shift: Vector3)` | Emitted on the main thread during `update_camera` when a re-center happens; `take_origin_shift()` stays for the same-frame ordering of the tile host |
| `attributions() -> PackedStringArray` | Attribution strings currently required on screen (OSM, plus each active image source from 0.9) |
| `shutdown()` | Stops the workers, then waits (bounded, 1 s) for the disk cache to flush and close and releases the `cache_dir` lock (for level unloads without freeing the node). Dropping the node remains a background stop with no wait. |
| `status() -> Dictionary` + signal `status_changed(status: Dictionary)` | `{ state: "ok" \| "degraded" \| "offline" \| "error", message: String, failed_fetches: int, last_error: String }`. A disk cache disabled for the session after repeated write failures sets `degraded`. |

**Lifecycle rules (0.7):**

- Several instances in one process are allowed (e.g. a main view plus a map viewport), each with its own world; they share nothing except optionally the tokio runtime. Two instances must not use the same `cache_dir`: `configure_world` takes an exclusive lock on `cache_dir/.simworld.lock` synchronously, before the asynchronous archive and cache open, and returns an error ("cache_dir in use") if the folder is held by another instance or process. The lock is per folder (not per tile source); a world with no disk cache (empty `cache_dir`, or the synthetic city) takes none.
- **Teleport:** calling `configure_world` again with a new origin drops all tiles (every loaded key appears in `poll_unload`), resets the origin and keeps the cache. The host treats it as a fresh start. The old world's cache is flushed and closed (bounded wait), or its open handle reused when `tile_url` and `cache_dir` are unchanged, before the new world opens it, and the folder lock stays held across a teleport with the same `cache_dir`. (Already holds for `configure` since 0.5 and `configure_world` since 0.6; 0.7 adds the cache lock and several instances.)
- The cache key includes the tileset fingerprint and the cache schema version; raw bytes are cached, meshes are not, so a profile change never serves stale geometry. If meshes are ever cached, the profile hash joins the key.

### 0.8 — Daylight (M6)

| Addition | Shape |
|---|---|
| `set_clock(unix_utc: float, utc_offset_hours: float)` | Sets simulated time; the host advances it (real time or a time-of-day slider) |
| `daylight() -> Dictionary` | For the current camera position: `sun_dir` (Vector3 toward the sun, local axes), `sun_elevation_deg` (float), `night_factor` (float, 0 day … 1 night, smooth between sun elevation +6° and −6°), `local_hour` (float 0–24, used by the window `lit_share`). Low-precision solar algorithm in Rust, no network. |

### 0.9 — Imagery (M7)

| Addition | Shape |
|---|---|
| `configure_imagery(sources: Array[Dictionary])` | Each source: `url` (raster `.pmtiles` or XYZ template `https://host/{z}/{x}/{y}.jpg`), `min_zoom`, `max_zoom`, `attribution`; plus `cache_mb` (default 500), `timeout_s` (default 3). Empty Array = imagery off. Self-hosted only. |
| Ready-tile key `imagery` | The tile's ground texture as a Godot `Image` with mipmaps (RGBA8), or absent/empty when there is none yet; the host copies it into the tile's pooled `ImageTexture`. A tile is delivered when meshed and its image is ready, or after the imagery timeout without it, then re-delivered under the same key when the image arrives (rate-limited like light re-deliveries). Changing the source re-delivers every held ground tile. |
| Ground `TEX_UV` | With imagery on, every ground vertex gets `TEX_UV` = its position in the tile, 0..1 in Web Mercator x/y |
| R3 ground | With imagery on, R3 tiles get a ground mesh: one textured quad (2 triangles) |
