# Spec: Road traffic (minimal: boxes moving on near roads)

Oct 28, 2026 · @Aleksei

## Purpose and scope

Animate the roads close to the camera: very low-poly cars (a box) sliding along the near road strips, both directions, so a city seen from a landing or a low pass is not frozen. At night each car carries two lamp dots, white head lamps towards the camera and red tail lamps away from it, plus a yellow window band on the body, and the dots stay visible as moving points of light as far as the near bands reach (R0 and R1, about 2.5 km) because a lit lamp is a few pixels at any distance while the body is not. Nothing is drawn beyond that: this is a close-range effect, the cheapest that still reads as traffic.

Goals, in order:

1. **Performance first.** Zero CPU per frame (one global uniform write), one draw call per near tile, no textures, no per-car objects, no per-frame buffer writes. Everything a car does is a function of `(slot, time)` in a vertex shader.
2. **Two knobs.** `traffic.density` (cars per km of lane) and `traffic.enabled` in the world profile; `WorldView.TrafficEnabled` and a live `TrafficDensityScale` (0..1) on the host; `--no-traffic` in the bench.
3. **Contract rules as everything else:** tile data relative to the tile origin, the sibling-swap visibility rule and dithered fades, the floating origin, the Mobile renderer, no `instance uniform`.

**Out of scope (deliberately):** cars beyond R1 (the far "light rivers" seen from cruise altitude on R2 / R3 roads), intersections, lane changes, traffic lights, several lanes per direction, trucks, parked cars, sounds, a car mesh asset. Cars pass through each other at junctions and that is accepted.

**Status:** proposed, 2026-10-28, no code. **Owners:** `rust/` (runs, profile keys, tests) → `rust-world-streamer`; `TrafficPool`, shader, bench → `godot-csharp-integrator`; measurement → `/bench-measure` (+ `code-critic` for the cost review).

## Today

State at `api_version` 1.11.

| Have | Where | Reused |
| --- | --- | --- |
| Road centrelines per tile per band with class, `_link`, `lit`, OSM id | `mvt.rs` `RoadLine` | The path source |
| Strip centreline points with drape heights and the stacking lift (`flush_run` / `densify_run`) | `ground.rs` | The car path takes the same points, so a car sits on the drawn strip with the terrain on or off |
| Per-tile MultiMesh sets filled through one shared staging array, a container hidden by day, `FadeMaterials` dither clones | `TilePool.Sprites.cs`, `FadeMaterials.cs` | The upload path and the fades, one more set per tile |
| `simworld_night_factor`, earth curvature include | `ShaderGlobals.cs`, `earth_curve.gdshaderinc` | Night emission, curvature drop |

Missing: road direction (`oneway`) in the archive, any animation.

## What others do (short)

- **GTA V** draws distant cars as two moving light points and the full model only up close ([graphics study](https://www.adriancourreges.com/blog/2015/11/02/gta-v-graphics-study-part-2/)). This spec keeps only the "up close" half and makes the model a box.
- **X-Plane** spawns cars per metre of road per road type on the CPU under a budget, slow to build up and CPU-heavy ([developer blog](https://developer.x-plane.com/?p=4261)); the lane position is a ratio of the road width ([roads state of the union](https://developer.x-plane.com/2007/10/roads-state-of-the-union/)). Adopted: density per road class, lane offset as a share of the strip width. Rejected: CPU cars.
- **GPU path animation:** a path record per instance and `time` evaluated in the vertex shader ([FlightGear wiki](https://wiki.flightgear.org/Special:MobileDiff/133754); [Spline Architect, 1 M spline instances](https://80.lv/articles/spline-architect-s-new-version-adds-gpu-instancing-for-1-000-000-objects-along-splines/)). Adopted, in the simplest form: the path record is one straight segment packed into the instance transform, no texture.
- **Godot MultiMesh:** one draw call per set, instances are never culled individually, a moving set needs a custom AABB, `set_buffer` resizes and is not a per-frame call ([docs](https://docs.godotengine.org/en/stable/classes/class_multimesh.html)). Adopted: the tile rectangle as the AABB, the buffer written once at upload.
- **Density numbers:** free flow is 10 to 28 vehicles per km per lane ([highD study](https://arxiv.org/pdf/1903.04249)); the default of 10 is the low end, and the slider thins it further.

## Design

### 1. Runs and slots (Rust, `simworld-core/src/traffic.rs`)

Input: the band's `RoadLine`s after the band's own filter (R0 has every road, R1 drops minor paths), restricted to classes 1..`traffic.max_class` (default 5, residential; service roads and paths never carry cars). Bands `traffic.min_ring`..`traffic.max_ring` (default 0..1: R0 bodies and lamps, R1 lamps only; the host picks the mesh by ring, section 2).

- **Directions:** two per road, except one (along the line order) for `motorway` / `motorway_link` and for a road whose `oneway` attribute is `yes` (`-1` reverses the line). The attribute is optional (section 4); without it only the motorway rule applies.
- **One lane per direction.** Offset from the centreline: two-way roads a quarter of the strip width to the right of travel (`traffic.drive_left` flips it), one-way roads on the centreline. The offset polyline is the strip's centreline run shifted along the vertex bisectors, the same join rule as `strip`, so the lane stays inside the drawn strip. Heights = the strip's own centreline heights (the `densify_run` output plus `base_lift + road_level(class) × step`) plus `traffic.lift_m` (0.25 m).
- **Runs:** the lane polyline is cut into straight runs: consecutive segments merge while the heading turns less than 12° in total and the run is shorter than 120 m. A car drives straight along a run (the chord of a gentle curve lies within the strip; on a sharp corner the run ends and the next one starts, the car turns there).
- **Slots:** per lane `gap_m = 1000 / traffic.density` (default 10 → 100 m), `slots = ceil(lane length in tile / gap_m)`, at most `traffic.max_slots_per_lane` (32). Slot `k` of a lane is at arc length `s(t) = (k × gap_m + phase + v × t) mod L`, where `L` = the lane's length inside the tile, `phase` = a hash of the road's OSM id and the direction scaled to `gap_m`, `v` = `traffic.speed_mps_by_class[class]` (default motorway 25, trunk / primary 18, secondary 14, tertiary 11, residential 8, service 5 m/s). The arc length is measured from the lane's first point **inside this tile**: cars cycle within the tile, a car reaching the tile edge reappears at the lane's entry edge. Seam-exact continuation across tiles is not attempted (section "Decisions").
- **Instances:** one per `(run, slot)`: a slot is drawn by the run that contains its `s` and is collapsed by every other run of its lane. Instance count per lane = runs × slots. The tile cap `traffic.max_instances_per_tile` (2048) drops whole lanes, least important class first, deterministically (counted in `debug_stats`).

**Output per tile:** ready-tile key `traffic_cars`, a PackedFloat32Array of **12 floats per instance**, relative to the tile origin:

| Floats | Content |
| --- | --- |
| 0–2 | run start `p0` (x, y, z) |
| 3–5 | run vector `p1 − p0` (x, y, z) |
| 6 | `s_run`: arc length of `p0` along the lane (m) |
| 7 | `L`: lane length inside the tile (m) |
| 8 | `v`: speed (m/s) |
| 9 | `offset = k × gap_m + phase` (m) |
| 10 | `seed`: per-slot hash in 0..1 (colour, scale) |
| 11 | flags: 0 none, 1 lit road (unused in phase 1) |

The order is the MultiMesh transform layout (three rows of `basis | origin`): the host copies the array into the set's buffer in one block (`FloatsPerInstance` 12, no colours, no custom data) and the shader reads the three basis columns back as data. A tile root is a pure translation, so `MODEL_MATRIX` carries `p0` into the frame and leaves the data columns untouched.

### 2. Rendering (Godot, `TrafficPool`, `traffic_car.gdshader`)

- **Set per tile:** `TileNodes.Traffic`, a `MultiMeshInstance3D` with the car mesh of section 3 (R0: body surface + lamp surface, two materials; R1 and above: the lamp-only mesh, 8 vertices per instance, so far cars cost almost nothing), `TransformFormat` 3D, no colours, no custom data, `CustomAabb` = the tile rectangle padded 10 m horizontally and 0..6 m vertically plus the curvature margin (`CullMargins` as for the other sets), `CastShadow` off. Acquire / release / clear / trim like the sprite sets (`RetireQueue`, `PoolTrimmer`, `ReleaseBudgetUs`), filled within the 2 ms upload budget (a cap tile is 2048 × 12 floats = 96 KB).
- **Vertex shader:** `t` = global uniform `simworld_traffic_time` (section 7). `s = mod(offset + v × t, L)`; if `s < s_run` or `s > s_run + |run|` the instance collapses (all vertices to one point: no fill cost, 24 vertices of vertex cost); else `pos = p0 + run_dir × (s − s_run)`, `heading = run_dir`, `VERTEX = pos + R(heading) × VERTEX × scale(seed)` with `scale` in `traffic.scale` (0.9..1.2) and a van-ish 1.35 on 10 % of seeds. `earth_curve_drop` applied. Also collapsed when `hash(seed) ≥ simworld_traffic_density` (the live density scale) and beyond `traffic.max_distance_m` (fade out over the last 20 % with dither, so the edge of the traffic zone does not pop).
- **Body fragment:** unshaded flat colour: body colour = a 6-entry palette by `seed` × a fixed hemisphere term from the vertex normal (top bright, sides mid, Mobile-safe, no lights). Night: the window band (vertex COLOR flag, section 3) glows `traffic.window_color` (yellow) × `simworld_night_factor`, under the glow threshold like the street pools so it does not bloom; the lamp faces of the box glow faintly in their colour so the dots sit on something. Depth written and tested (a wall hides a car). `fade_mul` through `FadeMaterials` as a dither like the buildings.
- **Lamp dots (the night lights):** the second surface is two billboard quads per car, one at the front and one at the rear (one dot per end, not one per lamp: at 2–3 px a pair would merge anyway). They are drawn with the light sprite energy model already in `light_sprite.gdshader` (constant pixel size `traffic.lights_px` 2..3 px via the same `px_per_depth` uniform, inverse-square brightness, fog by hand, additive, `depth_draw_never` but depth tested at the car with the sprites' `lift_m` pull). Colour by the facing term `f = dot(heading, normalize(camera − pos))`: the front quad is `traffic.head_color` × `smoothstep(−0.2, 0.4, f)` (head lamps are directional: white when the car comes towards the camera, gone from behind), the rear quad is `traffic.tail_color` × `smoothstep(−0.4, 0.2, −f)` (tail lamps are seen from behind and from the side). Brightness × `simworld_night_factor`; by day the quads collapse (no fill, 8 vertices of vertex cost). The dots are visible out to `traffic.lights_max_distance_m` (2 500 m, the R1 reach) while the body stops at `traffic.max_distance_m` (900 m), so from a low approach the whole near city shows moving white and red points and the boxes appear only in the last kilometre. The lamp dots also sit on the body up close (the car's own lamps), so the body-to-dot transition is a fade of the body alone, nothing swaps.
- **Day switch:** none for the body, cars are drawn by day (the animation is the feature). The lamps are zero by day; R1 sets (lamps only) are hidden with the `Sprites` container by day so they cost no draw call.

### 3. The car

A box, no asset file: 4.2 × 1.75 × 1.45 m, origin at ground level under the centre, +Z forward in mesh space. Surface 0, the body: 12 triangles, 24 vertices (flat faces), split once horizontally so the upper 40 % is the window band (20 triangles, 36 vertices in total); vertex COLOR as flags: `r` = 1 on the rear face (tail lamp glow), `g` = 1 on the front face (head lamp glow), `b` = 1 on the window band. Surface 1, the lamps: two unit quads with COLOR `g` = 1 (front, at z = +2.1, y = 0.7) and `r` = 1 (rear, at z = −2.1, y = 0.8), 4 triangles, 8 vertices; the vertex shader expands them into billboards. A second mesh, lamps only (surface 1 alone), is used for R1 sets. Built in C# at startup (`TrafficMesh.Build()`), pinned by a test on triangle counts and bounds. That is the whole model; a cabin step or wheels are "Later".

### 4. World profile keys (`docs/contract.md` 3.2, profile_version 8)

| Key | Range | Default | Meaning |
| --- | --- | --- | --- |
| `traffic.enabled` | bool | `true` | Emit `traffic_cars`. Off = empty array, nothing else changes (the mesh / ground / light goldens are the same either way) |
| `traffic.density` | 1..60 | `10` | Slots per km of lane (`gap_m = 1000 / density`). The "frequency of cars" knob (a layout change: needs a reconfigure) |
| `traffic.min_ring`, `traffic.max_ring` | 0..4 | `0`, `1` | Bands with traffic: R0 (bodies and lamps) and R1 (lamps only on the host). `max_ring` 0 = bodies-range only (cheapest); 2 and 3 are accepted but the cap will drop most lanes; `min_ring` 4 = none |
| `traffic.max_class` | 1..6 | `5` | Most minor road class with cars |
| `traffic.speed_mps_by_class` | 6 × 1..50 | `[25, 18, 14, 11, 8, 5]` | Flow speed per class |
| `traffic.drive_left` | bool | `false` | Left-hand traffic |
| `traffic.lift_m` | 0..2 | `0.25` | Car base above the strip |
| `traffic.max_slots_per_lane` | 1..256 | `32` | Slots per lane per tile |
| `traffic.max_instances_per_tile` | 64..8192 | `2048` | Instance cap, whole lanes dropped least important class first |
| `traffic.max_distance_m` | 100..5000 | `900` | Host: view depth beyond which car bodies are not drawn (fade over the last 20 %) |
| `traffic.lights_max_distance_m` | 100..25000 | `2500` | Host: view depth beyond which the lamp dots fade out (the R1 reach; the dots exist only on R0 / R1 tiles anyway) |
| `traffic.lights_px` | 1..6 | `2.5` | Host: on-screen size of a lamp dot in pixels (constant with distance, like the light sprites) |
| `traffic.scale` | `[min, max]` 0.5..2 | `[0.9, 1.2]` | Per-car scale |
| `traffic.head_color`, `tail_color`, `window_color`, `palette` | rgb, 6 × rgb | `[1, 0.95, 0.8]`, `[1, 0.06, 0.02]`, `[1, 0.78, 0.35]`, 6 muted body colours | Host colours: head lamps white-yellow, tail lamps deep red, windows yellow (the reference photos) |

Validation like the other blocks: ranges, exactly 6 speeds and 6 palette entries, `scale[0] ≤ scale[1]`, `min_ring ≤ max_ring`. Everything the host reads (`max_distance_m`, colours, scale) comes through `world_profile()` like the window shares, so the look stays data.

### 5. Tile build (optional step, `data/schema/simplane.yml`, `simworld-cli bake`)

One attribute on the `roads` layer at both levels: `oneway` = `yes` / `-1` / `no` when tagged, `yes` for `junction=roundabout`. Without it dual carriageways of trunk / primary roads get two-way traffic on each carriageway (head-on cars 10 m apart); with it they are right. Motorways are one-way by class rule in Rust either way, so an old archive stays valid. `bake --verify` lists the field; `data/README.md` gets the row. No `lanes` attribute (one lane per direction by design).

### 6. Contract impact (`docs/contract.md`, target api 1.12, additive)

- Section 4: key `traffic_cars` (PackedFloat32Array, 12 floats per instance, the table of section 1; empty on terrain and landmark entries, and on every band outside `traffic.min_ring..max_ring`; sent on every delivery and re-delivery).
- New section 8b "Traffic": the run and slot rules, the formula `s = (offset + v × t) mod L`, the collapse rule, the direction rule, heights, the cap order. The host owns `t` (any seconds; the contract states the formula so a test can compute a car's position).
- Section 3.2: the `traffic` block, profile_version 8. Section 10: `roads.oneway` optional.
- `debug_stats`: `traffic_instances` (held), `traffic_capped_tiles`; `debug_tiles` entries: `traffic_instances`.
- `CHANGELOG.md` 1.12; crate 0.6.0; `low-poly-map/CLAUDE.md` switches; root `CLAUDE.md` status line.

### 7. Host (`addons/simworld/`)

- `WorldView.TrafficEnabled` (default true; a library below 1.12 gets nothing and one `traffic unavailable` log line, like `set_view`), `TrafficActive`, `TrafficDensityScale` (0..1 live: the shader drops slots whose seed is above it, no re-delivery), `TrafficStats` (instances held, visible estimate = instances × scale / runs-per-slot, buffer bytes).
- `simworld_traffic_time`: written every frame by `WorldView` from its own stopwatch, seconds as a float wrapped at 3600 (one `GlobalShaderParameterSet` per frame; at the wrap every car jumps once an hour, accepted). Godot's `TIME` is not used: its rollover is a project setting the host may change, and the precision of a large float time would make a 25 m/s car jitter.
- `simworld_traffic_density`: written on change of `TrafficDensityScale`.
- `WorldRenderSettings.Traffic*` mirror the profile's host keys with the usual override order.
- Bench: `--no-traffic`, `--traffic=<scale>`, `--traffic-selftest` (section 9), F1 line `traffic: inst … tiles … capped …`, Settings panel: Traffic slider (live), Enabled (restart).

### 8. Budget

Per frame, fixed by construction: 0 CPU (two global writes at most), one draw call per R0 tile with roads (two surfaces, one call each) and one per R1 tile at night, 44 vertices per R0 instance and 8 per R1 instance (collapsed instances cost vertices, not fill), no per-instance culling (tile AABB).

Estimate at the Vilnius archive, `low-250` at 300 m AGL: the R0 cover holds about 3 km² of city with roughly 40 km of class 1–5 road → 80 km of lane → 800 slots at density 10, about 3.5 runs per slot on R0's 0.5 m DP centrelines → 2 800 instances → 125 k vertices per frame, of which about 800 bodies (16 k triangles) and 1 600 dots are drawn. R1 (lamps only, night): about 20 km² with roughly 250 km of class 1–5 road → 500 km of lane → 5 000 slots, about 2.5 runs per slot on the 2 m DP centrelines → 12 500 instances × 8 vertices = 100 k vertices, 10 000 dots of 2.5 px when all slots are shown. Together under 250 k vertices and far below the 950 k primitives of the current frame; the dots are the same fill cost as a sixth of today's light carpet. Memory: 15 000 × 48 B = 720 KB. `max_ring` 0 removes the R1 half.

Measurement (release library, fullscreen, idle machine; `--benchmark=low-250` and `night-low`, with and without `--no-traffic`): frame p95, `other_wait_ms`, draw calls, primitives, `gc` section, the upload marker. Acceptance: p95 within +5 %, draw calls = no-traffic + R0 tiles with roads, no new managed allocation per frame.

### 9. Tests

Rust: lane offset inside the strip (straight, 90° corner, `_link`), the motorway and `oneway` direction rules, heights = strip centreline height + `lift_m` with and without the terrain, run merging (12° / 120 m), the formula evaluated at three times puts slot `k` on exactly one run of its lane and on the lane, the cap order, determinism (same bytes twice; a golden hash of `traffic_cars` per band on the synthetic city, the existing goldens unchanged), profile pinning and validation errors, contract tests for the key and `api_version` 1.12.

Godot `--traffic-selftest` (headless): a delivered R0 tile with roads has a traffic set with `instances == debug_tiles.traffic_instances` and the two-surface mesh, an R1 tile the lamp-only mesh, an unloaded tile returns it, `TotalCreated == roots`; a C# `TrafficMath.Position(record, t)` agrees with Rust test vectors; `--no-traffic` → nothing; a 1.11 library → one log line; a fading tile's traffic materials carry `fade_mul`; the meshes have 20 + 4 and 4 triangles and the stated bounds; a pixel probe (`HolePixelProbe`) over a residential road at 22 h finds red and white pixels at 400 m and at 1 800 m (the dots), yellow at 150 m (the windows), and none of them at 13 h.

## Phases

| Phase | Owner | Content | Done when |
| --- | --- | --- | --- |
| **R1 Runs** | rust-world-streamer | `traffic.rs`, profile block + schema + `default.json` (profile_version 8), `traffic_cars`, `debug_stats` / `debug_tiles`, contract 1.12 + CHANGELOG, tests | `cargo test` green, goldens unchanged |
| **C1 Cars** | godot-csharp-integrator | `TrafficMesh` (body + lamp surfaces, lamp-only mesh), `TrafficPool`, `traffic_car.gdshader` + `traffic_lamp.gdshader`, the two globals, `TrafficEnabled` / `TrafficDensityScale`, bench switches, F1 line, settings, `--traffic-selftest` | boxes drive on the roads by day; at night white and red dots move on every R0 / R1 road and the windows glow yellow up close; self-test passes |
| **P1 Measure** | `/bench-measure` | section 8 plan and acceptance | numbers under "Status and measured results" |
| **T0 Oneway** (optional) | rust-world-streamer | `oneway` in the schema and `bake`, rebuild the archives | dual carriageways carry one direction each |

_Status and measured results: to be filled after each phase._

## Decisions (2026-10-28)

- **Near bands only, a box for a car, lamp dots for the night.** Asked for explicitly: the cheapest thing that animates the roads, and the car lights must be visible at night. A box's emissive face is sub-pixel beyond about 100 m, so the lights are billboard dots with the light sprites' constant-pixel energy model, and they reach R1 (lamps only, 8 vertices per instance) because at night a lit city with dark roads beyond 600 m would look wrong from an approach. The far "light river" LOD on R2 / R3 (hourly occupancy, lane textures, jam mode) from the first draft stays dropped; it is listed under "Later".
- **Pure function of time on the GPU.** No CPU cars, no simulation state: a tile is full the frame it appears and the per-frame cost is one uniform.
- **Straight runs packed in the instance transform, no texture.** The simplest GPU path record; a curve is a chain of runs, each with its own instance per slot. The vertex overhead of collapsed instances (about 3.5×) is cheaper than a lane texture, a binary search in the shader and a second upload path.
- **Cars cycle within the tile.** A car reaching the tile edge reappears where the lane enters the tile. A seam-exact hand-over needs the arc length from the way start and a two-tile agreement on phase; at 900 m of view depth with R0 tiles of about 350 m the pop at a seam is visible but rare, and it is the price of the simple record. Revisit if it bothers.
- **Density is baked, the slider thins.** `traffic.density` fixes the layout (reconfigure), `TrafficDensityScale` drops slots live by their seed.
- **No `instance uniform`, no per-frame buffer write, no `GlobalShaderParameterGetList`:** the addon's known pitfalls.

## Later

- The far LOD: the same lamp dots on R2 / R3 roads (motorways and primaries from altitude, the night rivers of the reference photos); hourly occupancy; a jam mode. The first draft of this file (git history) has that design.
- Seam-exact continuation (arc length from the way start, phase from the OSM id in both tiles).
- A second body (van / bus) and wheels; a cabin step on the box.
- Stop at junctions with a more important road; the `oneway` data makes roundabouts one-way already.

## Sources

- [GTA V graphics study, part 2](https://www.adriancourreges.com/blog/2015/11/02/gta-v-graphics-study-part-2/): distant cars as two moving lights, the full model up close.
- [X-Plane developer blog: car budget](https://developer.x-plane.com/?p=4261), [roads state of the union](https://developer.x-plane.com/2007/10/roads-state-of-the-union/): CPU spawning per metre of road, lane position as a width ratio.
- [MSFS forum: night road traffic](https://forums.flightsimulator.com/t/realistic-night-road-traffic-with-headlights-and-tail-lights-driving-on-the-road/296690): what users notice (moving head / tail lights up close).
- [FlightGear wiki, AI traffic GPU idea](https://wiki.flightgear.org/Special:MobileDiff/133754), [Spline Architect GPU instancing](https://80.lv/articles/spline-architect-s-new-version-adds-gpu-instancing-for-1-000-000-objects-along-splines/): path records evaluated in the vertex shader.
- [Godot docs: MultiMesh](https://docs.godotengine.org/en/stable/classes/class_multimesh.html): single draw call, no per-instance culling, custom AABB, buffer semantics.
- [highD traffic study](https://arxiv.org/pdf/1903.04249): free-flow densities per lane.
- [OSM tags for routing](https://wiki.openstreetmap.org/wiki/OSM_tags_for_routing): the one-way rule (motorways and roundabouts implied).
