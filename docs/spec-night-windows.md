# Spec: Synthetic Night Windows

2026-10-27 · Aleksei

## Purpose and scope

Give R0/R1 buildings lit windows at night, driven by the local hour and the building's light profile, at almost no runtime cost. Buildings that have no windows in reality (castles, towers, ruins, chimneys, silos, garages, ...) must stay without them: landmarks get a floodlit facade, the rest stay dark.

This is the missing M6 piece "window shader with light profiles" from [spike-streamed-osm-city.md](spike-streamed-osm-city.md) ("Building generation and light profiles", "Rendering"). It refines that design in two places: which buildings get windows, and how windows are laid out on a wall.

Out of scope: interior mapping, per-window geometry, the window/sprite cross-fade between R1 and R2 (M6, separate), `set_clock` / `daylight()` in Rust (the host clock `TimeOfDay` is used as is).

**Owners:** tile schema and everything under `rust/` → `rust-world-streamer`; shader, `TilePool`, `TimeOfDay`, globals → `godot-csharp-integrator`.

## Status

Design only. Nothing built.

## Today

- `building_day.gdshader` is the daytime placeholder: palette by profile, faint floor lines from `UV.y / floor_h`. No emission, no `hour`.
- Vertex COLOR already has what windows need: `r` = profile id / 8 (0-4), `g` = seed per OSM id, `b` = floor height / 10 m, `a` = roof height / 400 m ([contract.md](contract.md) section 7).
- Wall UV is in meters, but `u` is cumulative along the whole ring (`mesher.rs` `emit_polygon`). The fragment shader cannot know where a wall starts or how long it is, so a window grid in meters is cut at every corner.
- The tiles carry only `kind` (raw `building=*`), `height`, `levels`, `min_height`, `colour` (`data/schema/simplane.yml`). A castle tagged `building=yes` + `historic=castle` arrives as `kind = yes`, resolves through landuse to residential and would get windows.
- `TilePool.SetNightFactor` feeds `night_factor` to sprites and ground, not to the building materials. `project.godot` has `[shader_globals]` (earth curve only).
- Light profiles in `default.json` have `color` and `kinds`; the lit-share table exists only in the design doc.

## What others do

**Streets GL** (OSM renderer, source read 2026-10-27):

- `isBuildingHasWindows(tags)`: `window`/`windows` = `no`/`yes` wins; then no windows for `bridge:support`, `man_made` = `storage_tank` / `chimney` / `stele`; then no windows if `building` is one of `garage, garages, greenhouse, storage_tank, bunker, silo, stadium, ship, castle, service, digester, water_tower, shed, ger, barn, slurry_tank, container, carport`. Separately, no windows if the wall height without roof is < 2 m.
- `WallsBuilder.getWalls`: per wall, `count = round(length / windowWidth)`, `actual = length / count`, and UV.x is rescaled to window units: each window spans exactly 1.0 and every wall ends on an integer. A wall with `count = 0` is flagged windowless. Consecutive smooth edges (round towers) count as one wall. The shader needs only `floor(uv)` and `fract(uv)`. No extra vertex data.
- Default level height 4 m; levels from `height` when missing.

**OSM2World** (`BuildingDefaults.java`): `hasWindows = false` for `greenhouse, garage, garages, carport, roof, church, hangar, industrial, chimney` and any `parking=multi-storey`. The tag `window=no` disables windows; otherwise building-type heuristics.

**3DWorld** (blog, 2018): window grid from world-space UV, `fract` for the position in a window and the integer part as the window id; per-building random lit density 10-50% and one light color per building; per-window hash compared against the density; nearby lights faded and distant ones hidden by fog against aliasing.

**Pixel City** (Shamus Young, 2009): one generated window texture (64 × 64 windows); every building uses a random section of it. Cheap, but the lit share cannot follow the hour.

**Interior mapping** (Godot "Fake Interior" shader, DagorEngine, Big Hero 6): parallax rooms behind glass. Too costly and invisible from an aircraft; not used.

**Microsoft Flight Simulator 2020:** places artist-made models picked by building type from OSM footprints; night lights are procedural, built locally from streamed data. No documented window shader.

### OSM tag statistics (taginfo, worldwide, 2026-10-27)

| Tag | Count | Consequence |
|---|---|---|
| `windows=no` + `window=no` | ~7 k | too rare to rely on; honour it when present |
| `building=castle` | 3.9 k | a kind list alone misses most castles |
| `historic=castle` | 54.7 k; 33.7 k with `building`, **26.1 k as `building=yes`** | export `historic` |
| `man_made=tower` with `building` | 79.4 k, **68.1 k as `building=yes`** (`tower:type`: communication, bell_tower, observation, defensive, minaret, ...) | export `man_made` |
| `historic=ruins` with `building` | 25.2 k, 15.2 k as `building=yes` | same |
| `building=church` | 441 k | already civic; moves to `landmark` |

## Design

### 1. Which buildings have windows

Resolution order in Rust (`mesher::resolve`), first match wins:

1. **Override by OSM id** from the world profile (`buildings.overrides`, e.g. `"w123456": "landmark"`). For hand-picked famous places where tagging or shape fails (Gediminas Tower in Vilnius).
2. **Explicit tag:** `windows = no` → the building keeps its profile but has no windows; `windows = yes` → windows even for a no-window profile.
3. **Kind table:** `kind` → light profile (as today, with the new profiles below and the extended lists).
4. **Shape heuristic** for generic kinds only (`yes`, unknown): tall and thin (`height ≥ 3 · sqrt(area)` and `area < 150 m²`) → `landmark` (a tower).
5. **Landuse fallback** as today.

Independent of the profile: a wall part shorter than 2 m (`height - min_height`) gets no windows.

**Tile schema (decided):** `kind` stays the raw `building=*` value. The `buildings` layer gets three optional attributes, each written only when the tag is present:

| Attribute | From | Example |
|---|---|---|
| `historic` | `historic=*` | `castle`, `ruins`, `city_gate` |
| `man_made` | `man_made=*` | `tower`, `chimney`, `water_tower` |
| `windows` | `windows=*` or `window=*` = `yes` / `no` | bool |

In `mvt.rs`, step 3 of the resolution order becomes: look up `kind`. If `kind` is generic (`yes` or not in any profile), look up `historic`, then `man_made`, in the same kind tables. A profile's `kinds` list therefore holds `building`, `historic` and `man_made` values together (`castle`, `tower`, `ruins`, ...).

Why raw attributes instead of rewriting `kind` in the tile build: the tables stay in the world profile, so tuning which tag means what needs no tile rebuild (Docker, planetiler). `kind` also keeps its documented meaning. The cost is a few lines in `mvt.rs` and about the same tile size, because the tags are rare.

**New light profiles** (ids fit the `r = id / 8` channel; 5 stays reserved for "unknown"):

| Id | Profile | Windows | Night look | Kinds (start list) |
|---|---|---|---|---|
| 0 | residential | yes | warm 2700 K | as today |
| 1 | office | yes | cool 4500 K | as today |
| 2 | retail | yes | bright white | as today |
| 3 | industrial | no (wall lamps later) | sodium | as today + `hangar` |
| 4 | civic | yes | warm | school, university, hospital, government, townhall, museum, library, courthouse, ... (minus the landmarks below) |
| 6 | **landmark** | no | floodlit facade: vertical gradient, warm ~3000 K | castle, fort, tower, bell_tower, church, cathedral, chapel, mosque, synagogue, temple, monastery, shrine, gatehouse, city_gate, lighthouse, monument, memorial |
| 7 | **dark** | no | nothing | ruins, roof, carport, garage, garages, shed, barn, bunker, silo, storage_tank, slurry_tank, digester, chimney, water_tower, container, greenhouse, ship, service, transformer_tower, bridge |

The lists merge Streets GL and OSM2World. They are data in the world profile and tuned by eye.

**Lights stay consistent at R2/R3:** `landmark` gives a light point in the floodlight color, `dark` gives none. Otherwise a castle that is dark up close would glow as a residential dot from afar.

### 2. Window layout on a wall (Streets GL method)

In `emit_polygon`, per ring:

1. Group consecutive edges into **wall runs**: a new run starts where the outline turns by more than 20° (round towers and DP-simplified curves become one run). Edges skipped as clip seams end a run.
2. Per run: `count = round(run_length / window_width_m(profile))`, `cell = run_length / count`.
3. Wall `u` = window units: `run_start_index + meters_into_run / cell`. Every run starts and ends on an integer, so windows never wrap a corner.
4. `count = 0`, a windowless profile, `windows = no` or a wall part < 2 m → `u = -1` on all four vertices (the shader draws no windows where `u < 0`).
5. **Floors are stretched to the wall height:** `floors = max(1, round(wall_h / floor_h))` and `v = meters_up / (wall_h / floors)`. Every floor is whole: no partial top row, and no wall height is needed in the vertex. `v` counts floors from the bottom of the extrusion (the part's `min_height`). A raised part starts its own count at 0, which only changes its hash.

`window_width_m` per profile (start values): residential 3.0, office 2.0, retail 4.0, civic 3.5.

**Seams:** a building clipped at a tile edge counts windows per piece, and R0 and R1 simplify the outline differently (0.5 m vs 2 m). The grid may shift slightly at tile seams and across a band swap; the dithered fade hides most of the swap. Accepted for v1. Later fix: count windows on the unclipped, unsimplified edge and offset the piece by its distance from the edge start.

**Roofs:** UV unchanged (tile-local meters); the shader never draws windows on faces with `normal.y > 0.7`.

### 3. Shader (`building_windows.gdshader`, replaces `building_day.gdshader`)

One material for all building tiles, plus the existing `LOD_DITHER` variant. Per fragment on walls with `u >= 0`:

```glsl
vec2 cell = floor(UV);                                         // window id: column, floor (both in window units)
vec2 f    = fract(UV);                                         // position inside the cell
bool in_window = all(greaterThan(f, vec2(0.2, 0.25))) && all(lessThan(f, vec2(0.8, 0.8)));
float share = window_share[profile] * building_factor(seed);   // building_factor: 0.6..1.4 from the seed
bool lit = hash(seed, cell) < share;                           // monotone in share: windows go off in a fixed order
float glow = lit ? (0.85 + 0.3 * hash2(seed, cell)) : 0.0;     // per-window brightness jitter
EMISSION = profile_color[profile] * glow * emission_level * night_factor * float(in_window);
```

- **Hour → share on the CPU:** the host interpolates each profile's 24-value `lit_share` table at the current hour once per frame and sets `uniform float window_share[8]`. The shader does no time math.
- **Retail:** the same windows as the other profiles. A special ground-floor share is dropped: v1 has no ground-floor rule.
- **Landmark (6):** no windows; emission = floodlight color × `night_factor` × a vertical gradient (bright at the base, fading up).
- **Day:** window cells slightly darker and smoother (lower roughness), emission 0. The floor lines become `fract(UV.y)` near 0. On walls with `u = -1` and on roofs, `v` is still in floor units (walls) or meters (roofs), as today.
- **Distance:** where a window is under ~1 px (`fwidth(UV.x) > 0.5`), replace the grid by its average `share × window_area_fraction × color`, so there is no shimmer and no extra cost.
- **Inputs:** shader globals `hour` and `night_factor` in `project.godot` `[shader_globals]` (set by `TimeOfDay` / `StreamerBridge`); `window_share[8]` and `profile_color[8]` as material uniforms from the world profile (`world_profile()`); `emission_level` from `NightRenderController.EmissionLevel`, so the glow threshold stays "just below" the windows.

**Cost:** no texture fetch, two hashes and ~25 ALU ops per wall fragment; no extra vertex data with the recommended contract option below.

### 4. World profile keys (`docs/contract.md` 3.2)

```json
"lights": {
  "profiles": {
    "residential": {
      "color": [1.0, 0.655, 0.341],
      "kinds": ["..."],
      "windows": { "enabled": true, "width_m": 3.0,
                   "lit_share": [0.08,0.06,0.05,0.05,0.05,0.08,0.20,0.30,0.20,0.10,0.08,0.08,
                                 0.08,0.08,0.08,0.10,0.15,0.30,0.45,0.45,0.45,0.40,0.25,0.12] }
    },
    "landmark": { "color": [1.0, 0.75, 0.45], "kinds": ["castle", "tower", "..."], "windows": { "enabled": false } },
    "dark":     { "kinds": ["ruins", "roof", "..."], "windows": { "enabled": false }, "light": false }
  }
},
"buildings": {
  "overrides": { "w123456": "landmark" },
  "slender_tower": { "ratio": 3.0, "max_area_m2": 150.0 },
  "min_window_wall_m": 2.0
}
```

`lit_share` start values come from the design-doc table (residential 45% 18-22 h, 8% 0-5 h; office 70% until 20 h then 15%, 5% at night; retail 90% until 21 h, 10% at night). Profile kind lists may hold `building`, `historic` and `man_made` values. `width_m` is read by Rust (layout); `lit_share` and `color` by the host (shader). Validation: 24 values in 0..1, a kind or override in one profile only.

## Contract and docs impact

- **Light profile ids 6 and 7** in COLOR `r`; the "mesh only carries 0-4" guarantee changes.
- **Wall UV** changes from meters to window units: `u` in windows (`-1` for windowless walls), `v` in floors. Roof UV stays in meters.
- **Profile keys** in 3.2 (`windows`, `light`, `overrides`, `slender_tower`, `min_window_wall_m`) and `profile_version` + 1.
- **Lights (section 8):** no light point for `dark`, floodlight color for `landmark`.
- **Tiles:** new optional attributes `historic`, `man_made` and `windows` in the `buildings` layer (`data/README.md`); `kind` unchanged.

**Decided: one breaking change, `api_version` 1.0.** Under section 1 of the contract, a changed encoding and guarantee is a major bump. We keep that rule rather than make an exception.

- Vertex memory stays the same. The rejected alternative added a `TEX_UV2` channel, which cost about 15% more building vertex memory just to stay additive.
- The test bench is the only host, and nothing reads wall `UV.x` today. The bench is updated in the same change, so the major check passes.
- Every breaking item lands in this one bump (W2 + W3 together), not spread over several versions.

Every change updates `docs/contract.md`, `API_MAJOR`/`API_MINOR`, `CHANGELOG.md`, `default.json` and the schema in the same commit.

## Tests

- `mesher`: a wall run of length L gets `round(L / width)` windows and its `u` ends on an integer; a 20° turn splits a run, a 10° turn does not; a wall < 2 m, a `dark`/`landmark` building and `windows = no` give `u = -1`; roof UV unchanged.
- `mesher`: a wall of 10.4 m with `floor_h` 3 m gets 3 floors and its top `v` is exactly 3.0.
- `resolve`: `kind = castle` → landmark; `kind = yes` + `historic = castle` → landmark; `kind = yes` + `man_made = chimney` → dark; `kind = yes` + slender shape → landmark; override wins over kind; `windows = yes` beats a windowless profile.
- `lights`: `dark` buildings give no light point, `landmark` the floodlight color.
- `profile::tests`: new keys in `default.json` and the schema; validation errors for a bad `lit_share` length and for duplicate kinds.
- `golden_tests`: regenerated once (`SIMWORLD_UPDATE_GOLDEN=1`), the synthetic city gets one castle, one tower and one ruin.
- Contract test: `api_version` line matches the constants.
- Godot: headless smoke test green; `dotnet build` clean.

## Acceptance

1. At 22:00 over the Vilnius old town, no windows on castles, towers, churches or ruins (check the Gediminas Tower, the cathedral bell tower, the city gate); landmarks show a floodlit facade.
2. Windows are never cut at building corners; every wall ends on a whole window.
3. Returning to a place shows the same lit windows; scrubbing the hour from 18:00 to 03:00 turns windows off one by one, never on again in between.
4. No visible shimmer of windows at 1-3 km slant distance.
5. Night frame time over the old town rises by ≤ 0.3 ms on the dev Mac compared with `building_day` (`SIMPLANE_PROFILE=1`).
6. No growth in building vertex memory (`debug_stats`).
7. No partial top-floor windows: every floor row is whole.

## Phases

| Phase | Work | Contract | Owner |
|---|---|---|---|
| W1 | Shader only: grid from the existing meter UV (fixed 3 m columns), hash, `hour` / `night_factor` globals, hard-coded shares. Shows the look; corners are cut, castles still lit | none | godot-csharp-integrator |
| W2 | Tile schema (`historic`, `man_made`, `windows` attributes), profiles `landmark` / `dark`, extended kind lists, overrides, slender heuristic, lights consistency; rebuild tiles | profile + ids 6/7 | rust-world-streamer |
| W3 | Wall runs, window-unit `u` and floor-unit `v` in `mesher.rs`; shader switches to `floor` / `fract(UV)`, `u = -1` | wall UV | both |
| W4 | `lit_share` tables in the profile, CPU interpolation, distance average, landmark gradient, tuning by eye | profile keys | both |

W2 and W3 ship together as the `api_version` 1.0 change.

## Later

- Seam-exact window grid across tiles and bands (count on the unclipped edge).
- Industrial wall lamps; R1 → R2 window/sprite cross-fade (M6).
- `building:part` export (bell towers on churches are usually parts).
- A ground-floor rule (bright shop fronts for retail). It needs a "part starts at the ground" flag in the vertex.

## Decisions (2026-10-27)

- **Contract:** one breaking change, `api_version` 1.0. Wall UV moves to window and floor units, and profiles 6/7 are added. No extra vertex data.
- **Tags:** `kind` stays raw. `historic`, `man_made` and `windows` are separate attributes, resolved in Rust through the profile tables, so tuning needs no tile rebuild.
- **Floors:** the mesher stretches floors to the wall height (`round(wall_h / floor_h)` whole floors), so there is no partial top row.
- **Ground floor:** no special rule in v1 (moved to "Later").

## Sources

- Streets GL: [isBuildingHasWindows.ts](https://github.com/StrandedKitty/streets-gl/blob/main/src/lib/tile-processing/vector/qualifiers/factories/osm/helpers/isBuildingHasWindows.ts), [WallsBuilder.ts](https://github.com/StrandedKitty/streets-gl/blob/main/src/lib/tile-processing/tile3d/builders/WallsBuilder.ts), [getBuildingParamsFromOSMTags.ts](https://github.com/StrandedKitty/streets-gl/blob/main/src/lib/tile-processing/vector/qualifiers/factories/osm/helpers/getBuildingParamsFromOSMTags.ts)
- OSM2World: [BuildingDefaults.java](https://github.com/tordanik/OSM2World/blob/master/core/src/main/java/org/osm2world/world/modules/building/BuildingDefaults.java), [forum: drawBuildingWindows](https://community.openstreetmap.org/t/osm2world-parameter-drawbuildingwindows-has-no-effect/95556), [Streets GL thread](https://community.openstreetmap.org/t/streets-gl-a-new-3d-renderer-for-osm/98594/14)
- OSM: [Key:windows](https://wiki.openstreetmap.org/wiki/Key:windows), [taginfo](https://taginfo.openstreetmap.org/)
- [3DWorld: building window generation](https://3dworldgen.blogspot.com/2018/04/building-window-generation.html)
- [Shamus Young, Procedural City, Part 2](https://shamusyoung.com/twentysidedtale/?p=2954)
- Interior mapping: [Godot Fake Interior shader](https://godotshaders.com/shader/fake-window-interior-shader/), [DagorEngine](https://gaijinentertainment.github.io/DagorEngine/assets/shaders/rendinst_interior_mapping.html)
- [MSFS forum: night lighting of buildings](https://forums.flightsimulator.com/t/add-more-night-lighting-to-buildings/323399)
