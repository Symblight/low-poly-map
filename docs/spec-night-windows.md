# Spec: Synthetic Night Windows

2026-10-27 · Aleksei

## Purpose and scope

Give R0/R1 buildings lit windows at night, driven by the local hour and the building's light profile, at almost no runtime cost. Buildings that have no windows in reality (castles, towers, ruins, chimneys, silos, garages, ...) must stay without them: landmarks get a floodlit facade, the rest stay dark.

This is the missing M6 piece "window shader with light profiles" from [spike-streamed-osm-city.md](spike-streamed-osm-city.md) ("Building generation and light profiles", "Rendering"). It refines that design in two places: which buildings get windows, and how windows are laid out on a wall.

Out of scope: interior mapping, per-window geometry, the window/sprite cross-fade between R1 and R2 (M6, separate), `set_clock` / `daylight()` in Rust (the host clock `TimeOfDay` is used as is).

**Owners:** tile schema and everything under `rust/` → `rust-world-streamer`; shader, `TilePool`, `TimeOfDay`, globals → `godot-csharp-integrator`.

## Status

Design only. Nothing built. Revised 2026-10-27 after review (see "Decisions").

## Today

- `building_day.gdshader` is the daytime placeholder: palette by profile, faint floor lines from `UV.y / floor_h`. No emission, reads no global.
- Vertex COLOR already has what windows need: `r` = profile id / 8 (0-4), `g` = seed per OSM id, `b` = floor height / 10 m, `a` = roof height / 400 m ([contract.md](contract.md) section 7).
- Wall UV is in meters, but `u` is cumulative along the whole ring (`mesher.rs` `emit_polygon`). The fragment shader cannot know where a wall starts or how long it is, so a window grid in meters is cut at every corner. `v` is meters up from the extrusion bottom. Roofs and the bottom caps of raised parts (normal −Y) carry tile-local meters in both channels.
- One building material for all bands (`TilePool._buildingMaterial` + its `LOD_DITHER` twin `_buildingFadeMaterial`). R2 tiles have a building mesh **and** light sprites (contract section 8).
- The tiles carry only `kind` (raw `building=*`), `height`, `levels`, `min_height`, `colour` (`data/schema/simplane.yml`). A castle tagged `building=yes` + `historic=castle` arrives as `kind = yes`, resolves through landuse to residential and would get windows. Footprints arrive clipped by planetiler to tile + buffer, so a building's area differs per tile and per zoom.
- Shader globals `simworld_hour` and `simworld_night_factor` exist (`project.godot` `[shader_globals]`, `ShaderGlobals.cs`); `WorldView.SetTime(hour, nightFactor)` writes both. `light_sprite.gdshader` reads `simworld_night_factor`; nothing reads `simworld_hour`; the building shader reads neither. Godot global uniforms cannot be arrays.
- `NightLook` sets the glow threshold to `EmissionLevel × ThresholdBelowEmission` (0.82) at night: anything emitting at or above `EmissionLevel` blooms.
- Light profiles in `default.json` have `color` and `kinds`; `LightProfiles` is a fixed struct of five named profiles with `deny_unknown_fields`; a kind in two profiles is a validation error. The lit-share table exists only in the design doc.
- Two addon folders exist during the P5 move: `addons/simworld/` (repo root; pinned by `profile::tests`, has `bin/` and the `.gdextension`) and `low-poly-map/addons/simworld/` (what Godot loads; its `default.json` and schema already differ from the root copy). **Until P5 unifies them, the Godot project copy is the one a shader edit must land in**; the root copy is the one tests pin. P5 should finish before W1 starts.

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

1. **Override by OSM id** from the world profile (`buildings.overrides`, e.g. `"w123456": "landmark"`). For hand-picked famous places where tagging or shape fails (Gediminas Tower in Vilnius). Expect to need these: many castles are mapped as `historic=castle` on an enclosing area or site relation with plain `building=yes` ways inside, which the tiles cannot see.
2. **State tags** (`buildings.dark_states`, start list: `historic = ruins`, `ruins = yes`, `disused = yes`, any `abandoned:building`) → `dark`, whatever `kind` says. A `building=house` + `historic=ruins` must not get lit windows.
3. **Kind table:** `kind` → light profile (as today, with the new profiles below and the extended lists). If `kind` is generic (`yes`, or in no profile), look up `historic`, then `man_made`, in the same tables.
4. **Shape heuristic** for generic kinds only, and only when `height` or `levels` is **tagged** and the footprint is **not clipped** by the encoder (no vertex on the tile + buffer boundary): `height ≥ ratio · sqrt(area)` and `area < max_area_m2` → `landmark` (a tower). Fallback heights are excluded because a 5 × 5 m `yes` in residential landuse would get 15 m and pass; clipped footprints because their area differs per tile and zoom, which would give the same building two profiles.
5. **Landuse fallback** as today.

The `windows` tag is applied after resolution: `windows = no` → the building keeps its profile (colour, light point) but all walls are windowless; `windows = yes` → windows even on a `landmark` / `dark` building, drawn with the **landuse fallback profile's** window width, colour and `lit_share` (those profiles have none of their own).

Independent of the profile: a wall part shorter than `min_window_wall_m` (2 m; `height - min_height`, the extrusion height, which is the wall height since roofs are flat) gets no windows. **R2 walls never get windows** (R2 has light sprites; a grid under the sprites and on coarse merged blocks is wrong), so `mesh_tile` writes `u = -1` on every R2 wall.

Residual inconsistency, accepted: a slender `yes` building that straddles a z16 tile edge but not the z13 edge skips the heuristic at R0/R1 and applies it at R3 (its far light is the landmark colour, its near walls residential). Towers are small, so this is rare, and it only changes a light's colour.

**Tile schema (decided):** `kind` stays the raw `building=*` value. The `buildings` layer gets attributes, each written only when the tag is present:

| Attribute | From | Example |
|---|---|---|
| `historic` | `historic=*` | `castle`, `ruins`, `city_gate` |
| `man_made` | `man_made=*` | `tower`, `chimney`, `water_tower` |
| `windows` | `windows=*`, else `window=*`; only the values `yes` / `no` (expression attribute, not `tag_value`; `windows=many` etc. are dropped) | bool |
| `ruins` | `ruins=yes` | bool |
| `disused` | `disused=yes` | bool |
| `abandoned` | `abandoned:building=*` present | bool |

A profile's `kinds` list holds `building`, `historic` and `man_made` values together (`castle`, `tower`, `ruins`, ...). The state booleans are matched by `buildings.dark_states`, not by the kind lists.

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

The lists merge Streets GL and OSM2World. They are data in the world profile and tuned by eye. A kind lives in exactly one profile (validation), so the new lists are **moves, not copies**: `church, cathedral, chapel, mosque, synagogue, temple, monastery, shrine` leave `civic`; `garage, garages, shed, barn, service, transformer_tower, greenhouse, storage_tank` leave `industrial`. Those buildings change their R2/R3 light colour (or lose the light) today; that goes in `CHANGELOG.md`.

**Lights stay consistent at R2/R3:** `landmark` gives a light point in the floodlight color, `dark` gives none. Otherwise a castle that is dark up close would glow as a residential dot from afar.

### 2. Window layout on a wall (Streets GL method)

In `emit_polygon`, per polygon:

1. **Corners.** A vertex is a corner when the outline turns by more than 35° there, or by more than 20° with both adjacent edges at least `2 × width_m` long. A 16-gon tower (22.5° per vertex, short edges) is therefore one run; a rectangle is four; a DP-simplified curve stays one run. Rotate each ring so iteration starts at its first corner; a ring without corners (a circle) is one run starting at vertex 0. Edges skipped as clip seams also end a run.
2. **Runs.** Per run: `count = round(run_length / width_m(profile))`, `cell = run_length / count`. Cells range from `0.67 × width_m` to `2 × width_m` (a 1.49 w wall gets one wide window, a 1.51 w wall two narrow ones); accepted, that is what Streets GL does.
3. **`u` = window units:** `column_base + meters_into_run / cell`. Every run starts and ends on an integer, so windows never wrap a corner. `column_base` carries on **across rings** of the same polygon (outer ring, then holes), so a courtyard wall never hashes like the facade.
4. **Windowless walls:** `count = 0`, a windowless profile, `windows = no`, a wall part < `min_window_wall_m`, or **any R2 wall** → `u = -1` on all four vertices and **`v` in meters** up from the extrusion bottom (as today). The shader draws no windows and no floor lines where `u < 0`, and the landmark gradient needs meters.
5. **Floors are stretched to the wall height** on windowed walls: `floors = max(1, round(wall_h / floor_h))` and `v = meters_up / (wall_h / floors)`. Every floor is whole: no partial top row, and no wall height is needed in the vertex. `v` counts floors from the bottom of the extrusion (the part's `min_height`). A raised part starts its own count at 0, which only changes its hash.

`width_m` per profile (start values): residential 3.0, office 2.0, retail 4.0, civic 3.5.

**Seams:** a building clipped at a tile edge counts windows per piece, and R0 and R1 simplify the outline differently (0.5 m vs 2 m). The grid may shift slightly at tile seams, and during a dithered band swap (up to `fades.cap_s` = 0.75 s) the R0 and R1 grids overlay each other. Accepted for v1. Later fix: count windows on the unclipped, unsimplified edge and offset the piece by its distance from the edge start.

**Roofs and bottom caps:** UV unchanged (tile-local meters); the shader never draws windows on faces with `abs(normal.y) > 0.7`. The bottom cap of a raised part has normal −Y and a `u` that is usually ≥ 0, so testing only `normal.y > 0.7` would light the underside of overhangs.

### 3. Shader (`building_windows.gdshader`, replaces `building_day.gdshader`)

One material for all building tiles, plus the existing `LOD_DITHER` variant. Per fragment on walls with `u >= 0`:

```glsl
ivec2 cell = ivec2(floor(UV));                                 // window id: column, floor (both in window units)
vec2  f    = fract(UV);                                        // position inside the cell
float in_window = float(all(greaterThan(f, vec2(0.2, 0.25))) && all(lessThan(f, vec2(0.8, 0.8))));
uint  s    = uint(seed * 16777216.0);                          // COLOR.g is 24-bit exact
float share = clamp(window_share[profile] * building_factor(s), 0.0, 1.0);   // building_factor: 0.6..1.4
float h1 = hash01(s, cell), h2 = hash01(s ^ 0x9E3779B9u, cell);              // integer hash (PCG-style), see below
float lit  = step(h1, share);                                  // monotone in share: windows go off in a fixed order
float glow = lit * (0.85 + 0.3 * h2) * in_window;              // per-window brightness jitter
float avg  = share * 0.33;                                     // window area 0.6 × 0.55 = 0.33 of the cell
float w    = smoothstep(0.25, 0.75, max(fwidth(UV.x), fwidth(UV.y)));        // 0 near .. 1 when a cell is ~1 px
EMISSION = profile_color[profile] * mix(glow, avg, w) * window_emission * emission_level * simworld_night_factor;
```

- **Hash:** integer only. `hash01(seed, cell)` is a PCG/xxhash-style `uint` mix of `(seed, cell.x, cell.y)` scaled to 0..1. A `sin`-based float hash is not reproducible across GPUs and precisions, and acceptance 3 and the determinism rule need the same window lit everywhere.
- **Hour → share on the CPU:** `TilePool.SetWindowShares(float[8])` interpolates each profile's 24-value `lit_share` table at the current hour and sets `uniform float window_share[8]` on **both** `_buildingMaterial` and `_buildingFadeMaterial`. `WorldView.SetTime` calls it when the hour has moved by more than 0.01 h, not every frame. Global uniforms cannot be arrays, so this is a material uniform; the one place that writes both materials is the only guard against them drifting apart. Index = profile id; ids 3, 5 and 7 are always 0. The shader does no time math and does not read `simworld_hour`.
- **Bloom (decided):** windows do **not** bloom. `window_emission` = 0.65, so a window emits 0.55–0.75 × `EmissionLevel`, below the glow threshold of 0.82 × `EmissionLevel`; sprites and floodlights stay above it. Without this every window up close would bloom (0.85–1.15 × `EmissionLevel`) and the bloom would vanish exactly at the distance blend, where the average drops to ~0.3. `window_emission` is a material uniform for tuning; `floodlight.strength` is set so the landmark base does bloom.
- **Retail:** the same windows as the other profiles. A special ground-floor share is dropped: v1 has no ground-floor rule.
- **Landmark (6):** no windows; emission = floodlight color × `floodlight.strength` × `simworld_night_factor` × `1 - smoothstep(0, floodlight.fade_height_m, UV.y)` (`v` is meters on `u = -1` walls; bright at the base, fading up).
- **Day:** window cells slightly darker and smoother (lower roughness), emission 0. On windowed walls the floor lines are `fract(UV.y)` near 0; on `u = -1` walls they are `UV.y / floor_h` in meters, as today, except that `landmark` and `dark` walls draw none (castles have no floors). Roofs and caps as today.
- **Distance:** the `w` blend above replaces the grid by its average where a cell is near 1 px, so there is no shimmer and no pop at a fixed distance. A hard switch (`fwidth > 0.5`) leaves a visible line and ignores grazing angles in `v`.
- **Inputs:** shader globals `simworld_night_factor` (exists; set through `WorldView.SetTime` by the host clock, e.g. `TimeOfDay`); `window_share[8]`, `profile_color[8]`, `window_width` (unused in the shader, kept for debugging) and `floodlight_*` as material uniforms filled from `world_profile()`; `emission_level` from `NightLook.EmissionLevel`.

**Cost:** no texture fetch, two integer hashes and ~30 ALU ops per wall fragment; no extra vertex data.

### 4. World profile keys (`docs/contract.md` 3.2)

```json
"lights": {
  "profiles": {
    "residential": {
      "color": [1.0, 0.655, 0.341],
      "kinds": ["..."],
      "windows": { "width_m": 3.0,
                   "lit_share": [0.08,0.06,0.05,0.05,0.05,0.08,0.20,0.30,0.20,0.10,0.08,0.08,
                                 0.08,0.08,0.08,0.10,0.15,0.30,0.45,0.45,0.45,0.40,0.25,0.12] }
    },
    "landmark": { "color": [1.0, 0.75, 0.45], "kinds": ["castle", "tower", "..."],
                  "floodlight": { "strength": 1.2, "fade_height_m": 25.0 } },
    "dark":     { "color": [0.0, 0.0, 0.0], "kinds": ["roof", "carport", "..."] }
  }
},
"buildings": {
  "overrides": { "w123456": "landmark" },
  "dark_states": ["historic=ruins", "ruins", "disused", "abandoned"],
  "slender_tower": { "ratio": 3.0, "max_area_m2": 150.0 },
  "min_window_wall_m": 2.0
}
```

- `windows` is present on residential, office, retail and civic, absent on industrial, landmark and dark: **no windows = no `windows` key** (no `enabled` flag). `LightProfiles` becomes a struct of seven named profiles (`landmark`, `dark` added; `deny_unknown_fields` stays).
- `color` stays required on every profile. `dark` has no light point by rule (section 8), not by a `light: false` key; its colour is unused.
- `floodlight` is required on `landmark` only.
- `lit_share` start values, 24 hourly entries: residential as above; office 0.05 at 0–6 h, 0.70 at 8–19 h, 0.15 at 20–22 h; retail 0.10 at 0–7 h, 0.90 at 9–20 h, 0.10 from 22 h; civic 0.03 at 0–6 h, 0.60 at 8–17 h, 0.10 at 18–22 h. Ramps of one hour between levels.
- `width_m` is read by Rust (layout); `lit_share`, `color` and `floodlight` by the host (shader).
- Validation: `lit_share` has 24 values in 0..1; a kind or override is in one profile only; an override id matches `^[nwr][0-9]+$`; `dark_states` entries are `key` or `key=value` over the exported state attributes. Warning (not error) when a `lit_share` rises anywhere between 18 h and 3 h, because acceptance 3 depends on it falling monotonically.

## Contract and docs impact

- **Light profile ids 6 and 7** in COLOR `r`; the "mesh only carries 0-4" guarantee changes to 0-4, 6, 7.
- **Wall UV** changes from meters to window units on windowed walls: `u` in windows, `v` in floors. On windowless walls `u = -1` and `v` in meters. **R2 walls are always windowless.** Roof and bottom-cap UV stays in meters.
- **Profile keys** in 3.2 (`windows`, `floodlight`, `overrides`, `dark_states`, `slender_tower`, `min_window_wall_m`; profiles `landmark`, `dark`) and `profile_version` + 1. Kind moves out of `civic` and `industrial` (section 1) change R2/R3 light colours.
- **Lights (section 8):** no light point for `dark`, floodlight color for `landmark`.
- **Tiles:** new optional attributes `historic`, `man_made`, `windows`, `ruins`, `disused`, `abandoned` in the `buildings` layer (`data/README.md`); `kind` unchanged.
- **Version plan (contract section 1):** the table moves `set_clock` / `daylight()` from 0.8 to **1.1** and imagery from 0.9 to **1.2**. 1.0 means "first breaking change", not "stable library"; the library's own stability milestone is P7.

**Decided: one breaking change, `api_version` 1.0.** Under section 1 of the contract, a changed encoding and guarantee is a major bump. We keep that rule rather than make an exception.

- Vertex memory stays the same. The rejected alternative added a `TEX_UV2` channel, which cost about 15% more building vertex memory just to stay additive.
- The test bench is the only host, and nothing reads wall `UV.x` today. The bench is updated in the same change, so the major check passes.
- Every breaking item lands in this one bump (W2 + W3 together, including the R2 and cap rules), not spread over several versions.

Every change updates `docs/contract.md`, `API_MAJOR`/`API_MINOR`, `CHANGELOG.md`, `default.json` and the schema in the same commit.

## Tests

- `mesher`: a wall run of length L gets `round(L / width)` windows and its `u` ends on an integer; a 40° turn splits a run, a 22.5° turn on short edges (16-gon) does not, a 25° turn on two 8 m edges does; a ring whose vertex 0 lies mid-run is rotated (the run is not split there); a hole's first column continues after the outer ring's last; a wall < 2 m, a `dark`/`landmark` building, `windows = no` and **every R2 wall** give `u = -1` with `v` in meters; roof and bottom-cap UV unchanged.
- `mesher`: a wall of 10.4 m with `floor_h` 3 m gets 3 floors and its top `v` is exactly 3.0.
- `resolve`: `kind = castle` → landmark; `kind = yes` + `historic = castle` → landmark; `kind = yes` + `man_made = chimney` → dark; `kind = house` + `historic = ruins` → dark; `kind = yes` + tagged height + slender shape → landmark; the same shape with a **fallback** height → not landmark; the same shape clipped by the encoder → not landmark; override wins over kind and state; `windows = yes` on a castle gives windows in the landuse profile's width.
- `lights`: `dark` buildings give no light point, `landmark` the floodlight color; the synthetic castle and tower resolve to the same profile at z13 and z16.
- `profile::tests`: new keys in `default.json` and the schema; seven profiles load; validation errors for a bad `lit_share` length, a duplicate kind, a bad override id and a `windows` key on `dark`; the 18–3 h rise warning fires on a crafted table.
- `golden_tests`: regenerated once (`SIMWORLD_UPDATE_GOLDEN=1`), the synthetic city gets one castle, one tower and one ruin.
- Contract test: `api_version` line matches the constants.
- Godot: headless smoke test green; `dotnet build` clean.

## Acceptance

1. At 22:00 over the Vilnius old town, no windows on castles, towers, churches or ruins (check the Gediminas Tower, the cathedral bell tower, the city gate); landmarks show a floodlit facade.
2. Windows are never cut at building corners; every wall ends on a whole window.
3. Returning to a place shows the same lit windows; with the default `lit_share` tables, scrubbing the hour from 18:00 to 03:00 turns windows off one by one, never on again in between.
4. No visible shimmer of windows at 1-3 km slant distance, and no visible line or pop where the grid blends into its average.
5. Night frame time over the old town rises by ≤ 0.3 ms on the dev Mac compared with `building_day` (`SIMPLANE_PROFILE=1`).
6. No growth in building vertex memory (`debug_stats`).
7. No partial top-floor windows: every floor row is whole.
8. No window grid on R2 blocks (fly at 3 km with R0/R1 forced off in the settings panel) and none on the underside of a raised part.
9. Windows do not bloom; the base of a floodlit landmark does.

## Phases

| Phase | Work | Contract | Owner |
|---|---|---|---|
| W1 | Shader only: grid from the existing meter UV (fixed 3 m columns), integer hash, `global uniform float simworld_night_factor`, hard-coded shares, `window_emission` 0.65, distance blend. Shows the look; corners are cut, castles still lit, R2 shows a grid. Edit the copy Godot loads (see "Today") | none | godot-csharp-integrator |
| W2 | Tile schema (`historic`, `man_made`, `windows`, state attributes), profiles `landmark` / `dark` with kind moves, overrides, `dark_states`, slender heuristic with its two guards, lights consistency; rebuild tiles | profile + ids 6/7 | rust-world-streamer |
| W3 | Corners and runs, window-unit `u` and floor-unit `v`, `u = -1` + meters on windowless, R2 and cap rules in `mesher.rs`; shader switches to `floor` / `fract(UV)`, `abs(normal.y)` | wall UV | both |
| W4 | `lit_share` tables in the profile, `TilePool.SetWindowShares` on both materials, `floodlight` keys and gradient, tuning by eye | profile keys | both |

W2 and W3 ship together as the `api_version` 1.0 change. P5 (one addon folder) finishes before W1.

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

## Decisions (2026-10-27, after review)

- **R2 walls are windowless** (`u = -1` from `mesh_tile`): R2 has sprites, and coarse blocks must not carry a grid. Part of the 1.0 bump.
- **Windowless walls keep `v` in meters.** Floor units only on windowed walls; the landmark gradient and the daytime floor lines need meters. Bottom caps excluded via `abs(normal.y)`.
- **Slender heuristic** only with a tagged `height`/`levels` and an unclipped footprint; the rare z13/z16 mismatch is accepted.
- **State tags** (`ruins`, `disused`, `abandoned`, `historic=ruins`) override `kind` through `buildings.dark_states`; `windows = yes` on a windowless profile borrows the landuse profile's window data.
- **Corner rule:** > 35°, or > 20° with both edges ≥ 2 × `width_m`; rings rotated to a corner; column ids continue across holes.
- **Shader:** integer hash; `smoothstep` distance blend instead of a hard switch; **windows do not bloom** (`window_emission` 0.65 under the 0.82 threshold), floodlights do.
- **Share delivery:** material uniform `window_share[8]` set by `TilePool.SetWindowShares` on both building materials when the hour moves > 0.01 h (globals cannot be arrays).
- **Profile schema:** seven named profiles, `color` required everywhere, no `enabled` / `light` flags, `floodlight` on `landmark`; kinds move (not copy) out of `civic` and `industrial`.
- **Version plan:** `set_clock` → 1.1, imagery → 1.2; 1.0 is not a stability promise.

## Sources

- Streets GL: [isBuildingHasWindows.ts](https://github.com/StrandedKitty/streets-gl/blob/main/src/lib/tile-processing/vector/qualifiers/factories/osm/helpers/isBuildingHasWindows.ts), [WallsBuilder.ts](https://github.com/StrandedKitty/streets-gl/blob/main/src/lib/tile-processing/tile3d/builders/WallsBuilder.ts), [getBuildingParamsFromOSMTags.ts](https://github.com/StrandedKitty/streets-gl/blob/main/src/lib/tile-processing/vector/qualifiers/factories/osm/helpers/getBuildingParamsFromOSMTags.ts)
- OSM2World: [BuildingDefaults.java](https://github.com/tordanik/OSM2World/blob/master/core/src/main/java/org/osm2world/world/modules/building/BuildingDefaults.java), [forum: drawBuildingWindows](https://community.openstreetmap.org/t/osm2world-parameter-drawbuildingwindows-has-no-effect/95556), [Streets GL thread](https://community.openstreetmap.org/t/streets-gl-a-new-3d-renderer-for-osm/98594/14)
- OSM: [Key:windows](https://wiki.openstreetmap.org/wiki/Key:windows), [taginfo](https://taginfo.openstreetmap.org/)
- [3DWorld: building window generation](https://3dworldgen.blogspot.com/2018/04/building-window-generation.html)
- [Shamus Young, Procedural City, Part 2](https://shamusyoung.com/twentysidedtale/?p=2954)
- Interior mapping: [Godot Fake Interior shader](https://godotshaders.com/shader/fake-window-interior-shader/), [DagorEngine](https://gaijinentertainment.github.io/DagorEngine/assets/shaders/rendinst_interior_mapping.html)
- [MSFS forum: night lighting of buildings](https://forums.flightsimulator.com/t/add-more-night-lighting-to-buildings/323399)
