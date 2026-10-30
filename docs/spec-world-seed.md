# Spec: World seed (synthetic cities, airports, coast)

Status: proposal; **S0 built** (Rust core + CLI, no contract / api / profile change, results in section 11b); **S1 built on the Rust side as api 1.7 / profile_version 6** (the next free minor when it landed; daylight and imagery moved to 1.8 / 1.9; results and corrections in section 11c; the Godot half follows); **S2, S3 and the core of S4 built as api 1.12** (Rust, section 11f); S2b, S4a, S4b, S5, S6 not built. Target was **api 1.9** and **profile_version 6**. Milestone name: **M10 World seed** (independent of M7 imagery). **It requires the terrain layer (M9, built since api 1.4 / 1.5)**: a seed world is configured with `layers: ["world", "terrain"]` and `terrain.enabled`; the terrain tiles (out to 130 km) carry the surface beyond the vector cover, so this spec has no far layer of its own (section 5.5).

Revision 2026-10-29, revised 2026-10-30 (city pipeline): city generation is redesigned as the four-stage pipeline of Shariatpour et al. 2026 (context, road growth by organic / grid / radial morphology, zoning on blocks and lots, buildings at LoD0-3), with no hex cells at all, in **`docs/spec-seed-city-pipeline.md`**, which supersedes sections 5.3 and 6.1-6.4 below and amends 4.2, 8a.1 and 9.2. Those sections are kept below as the description of **generator 3** (built, S2-S4, section 11f) until generator 4 replaces it. Proposal only, no code changed.

Revision 2026-10-27 (review): terrain is procedural (no DEM, no build step), so runway flattening and the sea are rules of the height function (sections 5.1, 5.2a); R3 ground already exists (api 1.3); the far layer is cut; multi-source caches and status (section 8c); connectivity, radial meeting and district size rules (section 5.3); hash arity (section 7).

This spec lets a game (first host: SimPlane, region Lithuania, Latvia, Estonia) fly over a whole region with no vector tile server. The game ships a small set of hand-written files: a world manifest, one JSON file per city it wants, a pre-computed low-poly coast and water file, and an airport file cut from OurAirports. From these the library generates every tile on the fly: water and countryside everywhere, airports with their runways at the real positions, and a believable city (districts, curved streets, blocks, buildings) wherever a city file says so. The same files give the same world on every run, every machine and every platform. Any city can be switched to real OSM data (today's Vilnius archive) from a game menu ("download city"), and it then replaces the synthetic city tile by tile.

Related: `docs/contract.md` (ready-tile dictionary, ground classes, determinism, terrain layer 9a), `docs/spike-streamed-osm-city.md` ("Tile grid and 3D LOD", "Ground and terrain hook"; its band table still says "> 25 km: nothing, fog and horizon glow": with the terrain layer and this spec the terrain reaches 130 km and carries land and sea), `docs/spec-terrain.md` (relief, section 14 "World seed"), `docs/spec-night-windows.md` (light profiles the generated buildings resolve through), `rust/simworld-core/src/synthetic.rs` (today's offline city; this spec replaces it as the generator behind an empty `tile_url` when a manifest is given).

Naming: "seed" everywhere in code and docs. Not "SID": in aviation SID is a Standard Instrument Departure, which a flight game will show in the same UI.

## 1. Goals and non-goals

Goals:

1. **The airport is in the right place.** Runways come from OurAirports at their surveyed ends; nothing generated (building, street, district) overlaps a runway, its strip or its approach. This is the one hard rule; everything else is believable, not real.
2. **Same files → same world.** Bit-identical output for the same manifest, city files, data files and generator version, on every platform and thread count. A change in one city never changes another city or the countryside.
3. **The author decides, the generator fills in.** The game author lists the cities and describes each one (layout type, which districts, heights, density, linked airport); the generator places the districts automatically and builds the streets, blocks and buildings.
4. **No straight-line look.** Districts with their own orientation and style, curved arterials, warped local streets, irregular old-town blocks.
5. **Real data per city on demand.** A city can be switched between synthetic and a real OSM archive at runtime without a teleport; the airport stays identical in both modes.
6. **Same pipeline downstream.** The generator produces the same per-tile structures as the MVT decoder (`Building`, `GroundFeatures`), so meshing, LOD, the sibling swap, fades, night windows, light points and street lamps work unchanged.
7. **Land and sea to the horizon.** At cruise altitude (horizon ≈ 200 km at 3 000 m) the pilot sees coast, sea and lakes for the whole region, not a sea that ends with the vector ground: the terrain layer carries it to 130 km, flat at sea level over the sea and coloured by the seed land/water class (sections 5.2a, 5.5).

Non-goals (later or never): generated road networks between cities (inter-city roads are author-drawn, section 5.4); villages and buildings outside city files; railways; real street layouts without real data; a seed world without the terrain layer; worldwide coverage (the tools work for any bbox, but only a region is pre-computed and shipped); procedural landmarks beyond simple kinds (churches, towers); recognisable landmarks are author-made models placed by the game (section 6.7).

## 2. Decision in one paragraph

A **world manifest** (`world.json`) names a water file, an airport file and a list of city files, all JSON, all shipped inside the game (`res://`). `simworld-cli` produces the water file from OSM (coast, lakes, large rivers, simplified to ~50 m) and the airport file from OurAirports CSVs, filtered to the region. The library loads the manifest in `configure_world` and **routes every z13 tile** (the coarsest band; finer tiles inherit the decision): inside a city switched to real data → that city's PMTiles source over a countryside underlay; else → the **seed generator**. Seed tiles get ground in all four bands (R3 under the existing `ground.r3` rules), and beyond the vector cover the **terrain layer** shows the region's land and sea to the horizon: the seed world hands the height function its runway strips and its water (sea flat at `sea_level_m`, lakes flat), so relief never rises out of the Baltic. The seed generator composes, in fixed priority, water → airports (runway, strip, approach clearance, apron, terminal; geometry never random) → cities → countryside. A city is generated from its file in three cached levels: a **city plan** (hex cell layout around the centre, cells classified as water / airport / land, districts placed automatically by the file's requests, arterial roads on warped cell edges plus curved radials), a **district plan** per cell (streets in the district's style, blocks, lots, buildings) and the **tile assembly** (each building emitted into every tile it overlaps, `owned` by centroid, clipped by the mesher, projected to tile pixel space through lat/lon exactly like decoded MVT). Every random choice is a **counter-based hash** of a seed, a purpose tag and a stable id (city id, airport ident, cell coordinates, edge id), never a stateful RNG, so output does not depend on order, threads or what else is in the world.

## 3. Data

### 3.1 Sources

| Data | Source | Licence | Used for | Shipped as |
|---|---|---|---|---|
| Airports, runways | OurAirports `airports.csv`, `runways.csv` (ourairports.com/data) | Public domain | Airport positions, type, elevation; runway end coordinates, width, length, surface, closed flag | `airports.json` (region only, cut by `simworld-cli`) |
| Coast, lakes, large rivers | OSM: land/water polygons (osmdata.openstreetmap.de), plus `natural=water` / `waterway=riverbank` areas from the Geofabrik extracts the tile build already downloads | ODbL ("© OpenStreetMap contributors") | Water areas and the land mask | `water.json` (region only, simplified) |
| City centres, sizes | The game author (city file) | — | Everything about a city | `cities/*.json` |

No GeoNames: the author writes each city's centre and radius, so no population-to-size model is needed. Natural Earth stays a fallback for the water file (public domain, coarser) if the OSM build is too heavy.

### 3.2 `simworld-cli` tools (offline, part of the game's content build)

| Command | Input | Output |
|---|---|---|
| `seed airports --bbox <w,s,e,n> \| --countries LT,LV,EE <airports.csv> <runways.csv> -o airports.json` | OurAirports CSVs | Sorted by `ident`; fields in section 4.3; the CSV download date in the header; a report of airports with incomplete runway ends |
| `seed water --bbox <...> --tolerance-m 50 --min-area-m2 20000 <inputs> -o water.json` | OSM land/water polygons, extract | Water polygons (with holes) simplified by Douglas-Peucker at the tolerance, slivers under the min area dropped, coordinates quantized to 1e-7° |
| `seed validate <world.json>` | Manifest and every file it names | The same checks the library runs at load (section 4.6), plus a summary (cities, cells per district type, airports per city, warnings) |
| `seed preview <world.json> --city kaunas -o kaunas.svg` | Manifest | 2D plan of the city plan (cells, districts, arterials, airport zones): the author's iteration loop without starting Godot |
| `seed roads --osm <extract.pbf> --ref A1,A2,E67 [--class motorway,trunk] --tolerance-m 300 -o roads.json` | OSM extract | Draft inter-city roads: the named OSM roads merged into one line per `ref`, simplified to a few author points per km, for the author to trim and edit (section 5.4) |
| `seed roads --to-geojson roads.json` / `--from-geojson roads.geojson` | Roads file | Round-trip with QGIS, where roads are drawn and edited over OSM or imagery |
| `seed export <world.json> --city kaunas [--pristine] -o kaunas` | Manifest (+ the city's edits) | `kaunas-buildings.csv` (one row per building) and `kaunas.geojson` (footprints, for QGIS), section 8a |
| `seed import-edits <world.json> --city kaunas kaunas-buildings.csv` | The edited CSV | Diff against the pristine city, merged into the city's `edits` file (section 8a) |

The library never reads CSV or OSM; it reads only the JSON these tools write.

## 4. Files

All files are JSON with a JSON Schema 2020-12 in the addon (`addons/simworld/seed/*.schema.json`), like the world profile. The library reads them through Godot's file system (`res://`, `user://` or an OS path), the core gets their text, so `simworld-cli` runs the same parser. Coordinates are WGS84 degrees, quantized to 1e-7° on load (section 7).

### 4.1 Manifest `world.json`

```json
{
  "format": 1,
  "world_version": 1,
  "world_seed": "simplane-baltics",
  "water": "res://world/water-baltics.json",
  "airports": {
    "file": "res://world/airports-baltics.json",
    "types": ["large_airport", "medium_airport", "small_airport"],
    "include": [],
    "exclude": [],
    "overrides": {
      "EYPA": {
        "runways": {
          "01/19": { "le_lat": 55.9628424, "le_lon": 21.0896732, "he_lat": 55.9833478, "he_lon": 21.0979302,
                     "length_ft": 7679 }
        }
      }
    }
  },
  "cities": [
    "res://world/cities/kaunas.json",
    "res://world/cities/klaipeda.json",
    "res://world/cities/vilnius.json"
  ],
  "roads": "res://world/roads-baltics.json"
}
```

- `world_seed` (string) is hashed once (section 7) into the 64-bit root of every seed. Changing it re-rolls every city, never airports or water.
- `world_version` (int) is the author's version of the whole world; logged, returned by `seed_world_info()`, not part of any hash (the files' content is).
- `roads` (optional): the inter-city roads file (section 4.5).
- Airports: every airport in the file whose `type` is in `types`, plus `include`, minus `exclude` (idents), that has at least one open runway. A city does not have to list an airport for it to exist.
- `airports.overrides` (optional): per ident, airport fields (`lat`, `lon`, `elevation_ft`, `type`) and per runway (key `"<le>/<he>"` as in the airport file) any runway field (`le_lat`, `le_lon`, `he_lat`, `he_lon`, `width_ft`, `length_ft`, `surface`, `lighted`, `closed`, displaced thresholds), plus `"add"` (at airport level, an array of runways OurAirports lacks, same shape as a runway) and `"remove": true` (per runway key). Applied on load before everything else, so the generator, the terrain strips (section 5.1 step 9) and `airports()` see one corrected runway set. For airports whose OurAirports ends are missing or off (measured in S0). An override naming an unknown ident or runway is an error.

### 4.2 City file `cities/<id>.json`

```json
{
  "id": "kaunas",
  "name": "Kaunas",
  "version": 1,
  "variant": 0,
  "center": { "lat": 54.8985, "lon": 23.9036 },
  "radius_km": 7.0,
  "layout": { "type": "old_center_hex", "cell_size_m": 800, "rotation_deg": null },
  "max_height_m": 90,
  "density": 0.6,
  "airports": ["EYKA"],
  "districts": [
    { "type": "old_town", "cells": 2, "max_height_m": 25, "landmarks": 2 },
    { "type": "business", "share": 0.08, "rings": [1, 2], "max_height_m": 120, "density": 0.75 },
    { "type": "port", "cells": 1, "near": "water" },
    { "type": "industrial", "share": 0.12, "near": ["water", "airport"] },
    { "type": "residential", "style": "microdistrict", "share": 0.35, "rings": [1, 4], "max_height_m": 45 },
    { "type": "park", "share": 0.08, "near": "water" }
  ],
  "fill": { "type": "residential", "style": "houses", "max_height_m": 10, "density": 0.35 },
  "real": { "url": "https://example.org/tiles/kaunas.pmtiles" },
  "edits": "res://world/cities/kaunas.edits.json",
  "landmarks": [
    { "id": "cathedral", "name": "Kaunas Cathedral", "model": "res://world/landmarks/kaunas_cathedral.glb",
      "lat": 54.8977, "lon": 23.8862, "heading_deg": 12, "footprint_m": [86, 32], "height_m": 46,
      "clear_m": 15, "ground": "park", "kind": "cathedral", "in_real": "skip" }
  ]
}
```

| Key | Meaning |
|---|---|
| `id` | Stable id `[a-z0-9_-]+`, unique in the manifest. Part of the city seed: renaming it re-rolls the city. |
| `version` | The author's version of this city; logged. Bump it when an edit changes the city on purpose. |
| `variant` | Int, default 0. Part of the city seed: a different number is a different city with the same description ("try another layout"). |
| `center`, `radius_km` | Centre (old town / ring 0) and footprint radius, 0.5..30 km. |
| `layout.type` | `old_center_hex` (v1), `grid` (v1), `radial`, `linear` (later, section 11). |
| `layout.cell_size_m` | Hex cell size (centre to centre), 300..2000 m. The district grain. |
| `layout.rotation_deg` | Hex grid rotation; `null` = from the city seed. |
| `max_height_m`, `density` | City-wide defaults for districts that do not set them. `density` 0..1 = built footprint share of a block. |
| `airports` | Idents linked to the city: the city gets an access road to each, and `near: "airport"` measures to them. Each must exist in the airport file (else error). |
| `districts` | Placement requests, section 5.3. Order is priority. |
| `fill` | District for every land cell no request took. |
| `real` | Optional real-data source (section 8): `url` (`http(s)://…pmtiles`) or `path` (local file). Absent = this city is always synthetic. |
| `edits` | Optional author edits file (section 8a). Absent = the pristine city of the seed. |
| `landmarks` | Optional author-made models (section 6.7). |

District keys: `type` (`old_town`, `business`, `residential`, `industrial`, `port`, `park`), `style` (per type, section 6.2; default per type), exactly one of `cells` (count) or `share` (of the city's land cells, rounded, at least 1), optional `rings` ([min, max] inclusive, ring 0 = centre cell), optional `near` (`water`, `coast`, `river`, `airport`, or an array = any of them), `max_height_m`, `density`, `landmarks` (count of landmark buildings, old town default 1), `lit` (street lighting share 0..1, default per type).

### 4.3 Airport file `airports.json` (written by the CLI)

```json
{
  "format": 1,
  "source": "OurAirports",
  "source_date": "2026-10-27",
  "airports": [
    {
      "ident": "EYKA", "type": "medium_airport", "name": "Kaunas International Airport",
      "lat": 54.9639, "lon": 24.0848, "elevation_ft": 256, "country": "LT",
      "runways": [
        { "le": "08", "he": "26", "length_ft": 10663, "width_ft": 148, "surface": "ASP", "closed": false, "lighted": true,
          "le_lat": 0.0, "le_lon": 0.0, "he_lat": 0.0, "he_lon": 0.0,
          "le_heading_degT": 0.0, "le_displaced_threshold_ft": 0, "he_displaced_threshold_ft": 0 }
      ]
    }
  ]
}
```

(Values illustrative; the CLI copies them from the CSV.) The game reads the same file (or `airports()`, section 9) for spawn points, navigation and ILS, so the drawn runway and the game's runway are one source.

Runways use the displaced thresholds: the landing threshold of an end = the end moved inward by its `*_displaced_threshold_ft`. The strip still runs from the physical ends (section 5.1 step 2); the approach surface and the terrain strip heights start at the thresholds (steps 3 and 9).

### 4.4 Water file `water.json` (written by the CLI)

`{ "format": 1, "source": "OpenStreetMap", "bbox": [...], "outside": "sea", "tolerance_m": 50, "polygons": [ { "kind": "sea|lake|river", "rings": [[[lon, lat], ...], ...] } ] }`. Outer rings counter-clockwise, holes clockwise (GeoJSON order). Inside `bbox`, "outside every polygon" = land; **outside `bbox`** the surface is `outside` (`"sea"` or `"land"`, required), so open sea beyond the region stays sea. Size target: a few MB for the Baltics at 50 m.

**Load-time index.** The sea is one huge polygon, so tiles must never clip it whole. At load, every polygon is pre-clipped into z11 cells; each cell stores a flag `water` / `land` / `mixed` and, for `mixed` cells only, its clipped rings. A tile reads only the cells it overlaps (a fully water or land cell costs nothing). The same index feeds the terrain coupling (section 5.2a) and the terrain colour class (section 5.5). Each lake polygon also gets its **water level** at load: the minimum of the terrain field (before any water rule) over its outline vertices, quantized to 1 cm (deterministic, computed once).

### 4.5 Roads file `roads.json`

```json
{
  "format": 1,
  "roads": [
    { "id": "a1-vilnius-kaunas", "name": "A1", "class": "motorway", "lit": 0.0,
      "from": "vilnius", "to": "kaunas",
      "points": [[25.1802, 54.6801], [24.9050, 54.7350], [24.5100, 54.8200], [24.0300, 54.8850]] },
    { "id": "eyka-access", "class": "primary", "from": "kaunas", "to": "airport:EYKA",
      "points": [[23.9790, 54.9150], [24.0600, 54.9550]] }
  ]
}
```

| Key | Meaning |
|---|---|
| `id` | Stable id `[a-z0-9_-]+`, unique in the file; seeds the road's cosmetic noise (`H("road", id)`, independent of `world_seed`) |
| `class` | `motorway`, `trunk`, `primary`, `secondary`, `tertiary`: the profile's road classes (width, lamps, LOD drop) |
| `lit` | Share 0..1 of the road length with street lamps (in runs of ≥ 1 km chosen by hash), default 0 outside cities |
| `from`, `to` | Optional: a city id or `airport:<ident>`, the ends the road connects to (section 5.4); `null` = the road ends where its points end |
| `points` | Author points, lon/lat, ≥ 2, typically every 1–5 km; the road passes **through** every point |

### 4.6 Validation (load = `seed validate`)

Errors (the configuration fails, nothing changes, section 9): schema violations; unknown `format`; a duplicate city `id`; two city footprints (circles) overlapping (tested with radius × 1.15, the edge noise of section 5.3; the ring count is ⌊1.15·r / (cell·√3/2)⌋ + 1); a city listing an airport ident not in the file; a district request with both or neither of `cells` / `share`; `rings` outside the city's ring count; requests whose `cells` sum exceeds the land cells (S0 checks against the maximum possible cell count; the exact land count needs S2); a `real` with both `url` and `path`, a bad scheme, or a `path` under `res://` (section 8); a duplicate road id, a road with fewer than 2 points, a `from` / `to` naming an unknown city or airport, a road crossing a runway strip, a road crossing water longer than `seed.roads.max_bridge_m` (the author must route it around); a duplicate landmark id in a city, a landmark footprint intersecting a runway strip, a landmark taller than the approach surface over it, a `model` that is not a `res://` `.glb`, `.gltf`, `.tscn` or `.scn`. Warnings (logged, `seed validate` lists them): an airport with a runway end missing (approximate placement, section 5.1), requests whose `cells` plus `share` × land cells exceed the land cells, a request that could take fewer cells than asked (not enough candidates), a city centre in water, a city footprint more than 50 % water, a landmark outside its city's footprint, a road whose `from` / `to` end is more than 2 km from that city's footprint or that airport's fence.

## 5. Generation, world level

### 5.1 Airports (the hard rule)

Built once per airport at load, in a local tangent plane at the airport reference point; geometry is data, only the cosmetic parts (terminal shape, hangar count, side choice tie-break) use `H("airport", ident)`, independent of `world_seed`, so re-rolling the world never changes an airport.

1. **Runways:** each open runway is a rectangle from the `le` end to the `he` end, width `width_ft` (default by type when 0: 45 m large, 30 m medium, 18 m small). A runway with an end missing: centred on the airport point, along `le_heading_degT`, else the designator × 10° (magnetic, a few degrees off; logged as approximate), length `length_ft`. Surface: `ASP`/`CON`/`PEM`/`BIT`… → paved, `GRS`/`TURF`/`DIRT`/`GRE`… → grass (ground class, section 6.6).
2. **Strip:** the runway extended by `strip_end_m` (default 60 m) past each end, half-width `strip_half_m` by type (150 m large/medium, 40 m small). No buildings, no streets, no districts.
3. **Approach clearance:** measured from the **landing threshold** (displaced when the file says so, section 4.3), as ICAO does: the inner edge lies `strip_end_m` (60 m) before the threshold, across the strip width; from there outward a trapezoid widening at `approach_divergence` per side over `approach_len_m`, height limit rising at `approach_slope` (ICAO Annex 14 approach surface, first section, simplified). Values by runway class: paved runways ≥ 1 200 m (code 3/4) 15 % / 3 000 m / 2 %; shorter or grass runways (code 1/2) 10 % / 1 600 m / 5 %. Buildings inside are capped so that the roof stays at or below the surface everywhere over the footprint: the base is the mesher's base (the highest terrain under the footprint, contract 7 "[1.7] Per-vertex walls") and the limit is the lowest surface height over the footprint clipped to the trapezoid (a cap, not removal; below the minimum building height they are dropped; fix 2026-10-30, the earlier rule used the lowest ground and the vertices inside only, and left roofs up to 3.3 m over the surface at EYVI). Streets and districts are allowed; district placement avoids putting tall districts there (section 5.3).
4. **Fence:** the oriented rectangle around all strips plus `fence_margin_m` (default 100 m). Inside: no city cells (section 5.3), only airport content.
5. **Layout inside the fence** (by type): a parallel taxiway on the apron side at `taxi_offset_m` (180 m large, 120 m medium, none small) with connectors at both ends and every ~1 km; an apron rectangle; a terminal (kind `transportation`, size and floors by type), hangars (kind `hangar`, count from the seed), a control tower (tagged height by type, kind `office`), a parking area. Every kind the generator writes must resolve through the default profile to the intended light profile and ground class; a test pins this list (section 12). Apron side = the side facing the nearest linked city centre, else the seed.
6. **Access road:** one mechanism only, the city's radial (section 5.3 step 5): a linked city bends one radial toward the airport and extends it through the countryside to the fence gate, then to the terminal. An airport no city links gets a 300 m stub from the terminal.
7. **Lights:** for runways with OurAirports `lighted = 1`: edge lights every 60 m (white), threshold lights (green), end lights (red), and on paved runways ≥ 1 200 m a simple approach light row (900 m, white, every 30 m). Taxiway edge lights (blue) every 60 m on medium/large airports. They are light points like street lamps, in a new ready-tile key (`airport_lights` + `airport_light_colors`, section 9.1), always on at night (no lit share). `airports()` returns the same points plus the touchdown zone and threshold positions so a game can drive PAPI / ILS itself.
8. **Markings:** not in the ground mesh (ground has no UVs; `TEX_UV` on ground is reserved for imagery, api 1.3). The host draws runway markings as decals from the runway ends in `airports()`.
9. **Elevation: runway strips in the height function.** The terrain is a pure seeded function (api 1.4) with no data and no build step, so runways are levelled inside the function, the way the configure-origin flat zone is. At load the seed world hands the terrain a sorted list of **ruled strips**, one per open runway: the rectangle from end to end, `strip_half_m` wide, whose height along the centreline is linear between the field's own heights at the two landing thresholds (sampled before any strip, i.e. field + water rule, quantized to 1 cm; **S1:** if they differ by more than `seed.airport.max_slope` (1 %) of the threshold distance, both move toward their mean so the slope is exactly the cap: at the default 450 m relief a 3 km runway otherwise got 6 %), constant across, blended into the relief over `seed.airport.shoulder_m` (default 150 m) with the same smoothstep as the flat zone; apron, taxiways and the rest of the fence take the height of the nearest point of the nearest strip (no slope across the field). Overlapping strips (crossing runways) take the strip whose ident/designator sorts first, so the result is order-independent. **S1 fix (generator 2):** runways whose strips overlap or come within 1 m (crossing or close parallel runways, and approximate runways all centred on the reference point) form a group that shares the profile of its longest runway (ties: the first designator), measured along that runway's axis. Before, the first key won the overlap, so the height stepped across the other runway, by up to 12 m at EYPR (three approximate runways). 16 of the test world's 37 runways are in such groups. **Drawn ground (S1 fix):** inside the fence the vector ground has no drape lift. The drape lift of a vertex is `lift(band) × k`, with `k` the strip rule's blend weight (0 in the fence, smoothstep over `shoulder_m` outside), so the lift returns continuously over the shoulder. The fence is cut out of the countryside and water areas, and the airport levels are 1 / 2 / 3 (fence / pavement / runway). The drawn runway then lies `3 × lift_step_m` above `ground_height_at` (R0 0.06 m; contract section 9 "Airports"). The terrain mesh's error bound is scaled by the same `k` (at least 1 cm), so the TIN is exact on the levelled fence and never pokes through the lowered shoulder. A coarse z11 index of strips keeps the per-sample cost to a bounds check outside airports. `elevation_ft` stays **informational** (logged; `airports()` returns it): the drawn runway, `ground_height_at` and the terrain mesh all use the function. Landing physics and the AGL readout use `ground_height_at`, never `elevation_ft`. With the terrain off (a host that does not request the layer) every height is 0 as today.

Airports are generated identically whether the surrounding city is synthetic or real (section 8).

### 5.2 Water and countryside

- Water polygons → ground areas of class water. They cut everything below them in priority (cities, countryside); airports cut water (a runway on reclaimed land wins).
- Countryside = every land point outside cities and airport fences: ground areas from a value-noise field on the z15 grid (hash of the cell, bilinear, two octaves) thresholded into forest / farmland / meadow, with a lower forest share within 500 m of water. No buildings, no roads in v1 except airport access roads. Patch edges are the z15 grid warped by noise (section 6.4), so fields are not squares. `farmland` and `meadow` both resolve to the grass class in the default profile, so without more the land reads as forest and one green: a per-area **tint** (ground COLOR `g` = tint / 8 for areas, today always 0; the shader shifts hue/brightness by it) gives fields their patchwork. This is an additive contract change (section 9.1); an old host ignores `g` on areas.
- **Bands:** seed tiles carry ground in **all four bands**, R3 included, under the existing `ground.r3` rules (api 1.3: DP 20 m for areas, 15 m for roads, areas under 40 000 m² dropped, secondary roads and more important only). For R3 the generator emits water, countryside, each city district as one landuse area, arterials, radials and inter-city roads; no contract change.

### 5.2a Terrain and water

The procedural relief knows nothing about water: its noise would raise hills in the Baltic Sea and in every lake, and the terrain layer shows them to 130 km. The vector water snap (contract section 9) only levels water areas inside the vector cover. So the seed world also hands the height function its water:

- **Water test.** Which water a point is in (none, sea, lake id) comes from the z11 index exactly: the cell flag, and for `mixed` cells a point-in-polygon test on the cell's clipped rings (accelerated by latitude bands of edge references per cell: 16 in S0, 64 since S1; a point on the cell border is tested just inside the cell). The same polygons make the vector water areas, so terrain and ground agree on the coast to the water file's tolerance.
- **Distance raster.** At load, one raster per `mixed` z11 cell and its land neighbours: 64 × 64 samples (≈ 175 m at 55° N), each the **signed** distance from the sample to the nearest coast or lake edge (negative in water; an unsigned distance interpolates to a step at the coast line), clamped to `seed.water.blend_m` (default 400 m), stored as i16 decimetres. Cells along the bbox edge that are partly water but have no coast within `blend_m` store no raster. Pure land and pure water cells store nothing (land far from water = `blend_m`). Deterministic: computed from the quantized polygons with f64 `+ − × ÷ sqrt` only, cell by cell, no order dependence.
- **Rule inside the height function**, after the field and **before** the runway strips (**S1 correction:** the order is field -> water -> strips, so a runway near the coast or on reclaimed land stays exactly level, "airports cut water", and its threshold heights already include the water rule; evaluation is water first, so a point in the sea or a lake never samples the field): inside the sea, height = `terrain.sea_level_m`; inside a lake, height = that lake's water level (section 4.4); on land, height = w + (field − w) × smoothstep(0, `blend_m`, distance), w = the level of the nearest water, so the coast slopes down to the water instead of standing as a cliff. The lerp is continuous and never below w; do **not** add a separate "raise to w" clamp (S0 showed it makes a step at the blend edge). A raster corner with no water nearby takes the mean level of the other corners (using the field instead added a slope of field / spacing). The distance is sampled bilinearly. Outside the water bbox the manifest's `outside` decides (sea = flat at sea level); where the bbox cuts through land and `outside` is `"sea"` the height jumps at the bbox edge, so the water file's bbox must end in sea or the author accepts the step.
- **Consequences.** `ground_height_at` over water is the water level; the vector water snap still applies but finds a flat surface; the terrain mesh is flat over the sea and its TIN spends no triangles there.
- **Nearest water level** for the land rule: each raster sample also needs to know which water is nearest; a cell touches few waters, so each raster sample stores a u8 index into the cell's list of water levels (12 KB per 64 × 64 raster with the i16 distance). **Rivers** need a level that follows the river: a single level (minimum over the outline) turns a long river into a flat trench at its lowest point (the Nemunas would lie at sea level across Lithuania). **Decision (S1): no terrain rule for rivers.** Rivers are vector water only: a river point is land for the rule, river edges are not coast in the distance raster, and the river ground area drapes on the relief like any area (`terrain.water_snap_m` can snap it, default 0). Rivers stay water for the water test (ground class and terrain colour). Sloped river levels (a level per river section) are left for later if screenshots show rivers climbing hills; the procedural relief's valleys already end in flat river plains at sea level.
- **Budget:** ≈ 12 KB per raster cell (S1 measured on the two-cities world, section 11c: 572 rasters = 6.76 MiB, index 2.43 MiB; height cost 1.44x, land 1.39x); the Baltic coast and lakes are ≈ 1 500–2 500 cells ≈ 18–30 MB. Measured in S0 (section 11b): Lithuanian inland water 6.72 MiB at 64² / 1.72 MiB at 32²; Baltics estimate ≈ 18 MiB at 64², ≈ 4.7 MiB at 32². **Known limit:** where the coast wiggles at the raster spacing, land next to the water can start up to 2.3 m above it at 64² (smooth coasts show no step); the fix is an exact distance near the shore or a finer raster. Per-sample cost: one flag lookup outside water cells, a polygon test plus one bilinear lookup inside them.

### 5.3 Cities: the city plan (one per city, cached)

> **Superseded (generator 4):** `docs/spec-seed-city-pipeline.md` sections 2-3. There are no hex cells in generator 4: the footprint is a noisy outline, major roads grow by the city's morphology (organic / grid / radial), their faces become superblocks, and zoning assigns a land use to each superblock after the network (the district requests become zoning rules). Road entries and airport gates become growth seeds. The text below describes generator 3.

Built on a worker the first time a tile within `radius_km + cell_size_m` of the centre is requested; kept for the session (a 7 km city is ~300 cells, a few thousand arterial vertices; target < 20 ms).

1. **Cells.** A hex grid (pointy-top, size `cell_size_m`, rotation from the file or the seed) centred on `center`; ring `r` = hex distance from the centre cell; keep cells whose centre is within `radius_km`. The footprint edge is irregular: an outer-ring cell is kept when `distance < radius · (1 + 0.15 · noise(cell))`. **The city footprint** is the union of the kept cells' hexagons (after the edge warp of step 4), not the `radius_km` circle: land outside it is countryside, and roads are clipped and meet radials on its boundary (step 5, section 5.4).
2. **Classify.** Water (> 50 % of the cell's area in water), airport (centre inside a fence or > 30 % overlap), else land. Land cells partly in water or a fence are clipped later at district level. A land cell's `approach` share = its area inside any approach trapezoid.
3. **Place districts automatically.** For each request in file order: candidates = unassigned land cells within `rings` and, with `near`, within `2 · cell_size_m` of that feature (water edge, coast = sea edge, river = river polygon edge, airport = fence). Score = ring distance to the request's preferred ring (the lower bound) + 0.5 × distance to the nearest cell already in this request + `unit(H(city, "place", req_index, cell))` × 0.25 + `approach_penalty` × the cell's `approach` share for district types whose `max_height_m` exceeds the cap there (business first; default penalty 4 rings), so a business district does not end up as stubs under an approach. Take the best cell, repeat (region growing: the second term keeps a request contiguous) until `cells` / `share` is met or candidates run out (warning). `old_town` without `rings` means ring 0 outward. Remaining land cells get `fill`. Ties break on the cell's axial coordinates, never on iteration order.
4. **Arterials.** Every hex edge between two land cells of different districts, or on ring boundaries `r = 1, 3, 5…`, is a **secondary** road candidate; edges between cells of the same district are not roads (the district's own streets cover it) **unless the merged polygon would exceed `seed.city.max_district_m2`** (default 1.2 km², about two cells of 800 m): the same-district cells are merged greedily in cell order (axial `q`, then `r`), and an edge whose merge would cross the cap stays a road. Then a share of candidates is dropped (`arterial_drop`, default 25 %), so junctions become T and X shapes instead of a honeycomb of Y junctions, **without splitting the network**: first a spanning tree of the candidate graph (Kruskal, edge weight `unit(H(city, "edge", edge_id))`, ties by `edge_id`) is kept whole; only non-tree edges may be dropped, by the same hash, and never one that would leave a cell without an arterial or grow a district polygon past the cap (two cells joined across a dropped edge are laid out as one district polygon in step 6). Every remaining edge is a curve: its midpoint is displaced perpendicular by `±0.12 · cell_size_m · unit(H(city, "edge", edge_id))` and the polyline is smoothed by Chaikin with **fixed endpoints** (2 iterations). `edge_id` = the sorted pair of the cells' axial coordinates, so both cells compute the same curve; the hash uses the city seed, so different cities get different curves. Corners stay fixed, so the network is closed. **Water:** arterial segments inside water are dropped, except at designated bridges: per river or lake arm, the shortest crossing among the candidate edges, at most one per `bridge_spacing_km` (default 2 km) along the water.
5. **Radials.** `n = clamp(round(radius_km / 1.5), 3, 8)` **primary** roads from the centre cell's edge outward, start angles evenly spaced plus seed jitter, then each inter-city road entering the city (section 5.4 step 3) claims the radial nearest its bearing and turns it to that bearing (claims in road id order; extra roads beyond `n` add a radial); each advances in 100 m steps with a heading random walk (±4° per step from `H(city, "radial", i, step)`), steered away from water and fences, crossing a river only where the crossing is shortest within 1 km (a bridge, one per radial at most), ending at the footprint edge. **A radial claimed by a road is goal-directed:** its end is fixed in advance at the road's entry point (the road's clip point on the footprint, section 5.4 step 3); at each step the heading is blended toward the bearing to that point with weight `clamp(1 − remaining / (0.5 · radius), 0, 1)` (the random walk fades out as it approaches); the last 300 m are a straight run onto the entry point along the road's own bearing there, and the final vertex **is** the entry point (the same quantized coordinates the road ends on), so road and radial meet exactly. Unclaimed radials end where they cross the footprint boundary. For each linked airport, the radial whose end is closest to it bends toward it, leaves the footprint and continues through the countryside to the fence gate (the airport's only access road, section 5.1 step 6). Outside the footprint it is routed around water on a coarse 100 m grid (A* with a turn cost, deterministic tie-break by cell coordinates), then smoothed with fixed endpoints.
6. **Districts** = land cells, clipped by water, fences and the footprint, then cut by the radials (a radial crossing a cell splits it into two district polygons with the same type and style). Each district polygon gets `district_id = H(city, "district", cell, part)`.

### 5.4 Roads outside cities

Roads between cities are **drawn by the author**, not generated: a generated network between cities reads as random, while a few hand-placed lines along the real A1 / A2 / E67 read as the real country. `simworld-cli seed roads` drafts them from OSM; the author trims and edits them in QGIS (section 3.2).

1. **Curve.** At load, each road becomes a centripetal Catmull-Rom spline through its author points (it passes through every point and never overshoots), sampled every `seed.roads.sample_m` (default 25 m). Motorways and trunks get no noise (smooth like a real motorway); primary and lower roads get a low lateral wobble (amplitude by class, from `H("road", id)`) so they do not look drawn with a ruler.
2. **Water.** A sample run inside water becomes a bridge (ground strip over the water, class unchanged) when it is no longer than `max_bridge_m` (default 1 500 m); longer crossings are a validation error (section 4.6), so the author routes around lakes and lagoons.
3. **Cities.** A road is clipped at the footprints (union of kept cells, section 5.3 step 1) of the cities it touches; the clip point is computed in the city plan from the road's sampled line and quantized like any input (section 7), and both the road and the claimed radial end on it. At each clip point, the city's plan takes the road as an entry: the radial whose start angle is closest to the entry bearing is turned to that bearing (section 5.3 step 5), so the road continues into the city as a radial. A city's plan therefore reads the roads that enter it (and only those). Remaining radials end with a 300 m tapering stub.
4. **Airports.** A road with `to: "airport:<ident>"` ends at the fence gate and replaces the radial access road of section 5.1 step 6 for that airport.
5. **Real cities.** A road is clipped at the real coverage of an active real city (section 8: z13 tiles in the circle **and** inside the archive's header bounds); the real data carries the real road from there. Drafting roads from OSM keeps the two ends within metres of each other.
6. **Output.** Roads are `RoadLine` features in the ground of every seed tile they cross, with their class, `lit` and id (`H("road", id, segment)` in the OSM id space, like generated buildings), so widths, the per-band drop, street lamps and the night look work unchanged. Motorways, trunks and primaries are also in **R3 ground**, so the network reads from cruise altitude. Beyond 25 km roads are not drawn (the terrain layer has no roads).

Without a `roads` file, v1 behaviour holds: airport access radials only, other radials end with stubs.

### 5.5 Beyond the vector cover: the terrain layer

**Decision (2026-10-27): a seed world requires the terrain layer; there is no separate far layer.** The vector cover ends at 25 km (R3); the terrain layer (contract 9a) reaches 130 km and is already streamed, culled by the horizon and faded like the vector bands. A second whole-region mesh would duplicate it for a terrain-off host that a seed world does not support (`configure_world` with `seed_world` and without the layer `terrain` is an error).

- **Shape:** the terrain tiles follow the height function, which with section 5.2a is flat at sea level over the sea and flat over lakes, so the coast reads from altitude by shape and shading alone.
- **Colour:** terrain tiles have no COLOR today (contract 9a). For a seed world each terrain vertex gets **COLOR** (section 9.1): `r` = class / 8 with the ground classes (2 water incl. rivers, 0 grass (farmland, meadow), 1 forest, residential for city footprints from S2 on, class 7 = `seed.aeroway_class` inside fences; S1 built: water, fences, countryside), `g` = the countryside tint / 8, from the same water test and countryside noise as the vector ground at that point, so the transition at the vector reach (spec-terrain section 6.2) matches. A non-seed world's terrain tiles stay without COLOR.
- **Coast sharpness:** per-vertex colour on a TIN bleeds over long triangles. The S1 screenshots (11d) showed land-coloured spikes into the sea and hair lines across Kauno marios. **Built (S1 fix, generator 2), the cheapest of the options:** per-triangle class resolution. A surface triangle whose vertices are partly water takes the side of the water test at its centroid, and the vertices that change colour are duplicated with that class, so the coast is a hard edge along TIN edges. It adds no triangles. Vertices grow by 2-22 % on the coast and lake tiles without an airport (coast z14 492 -> 500, z11 284 -> 310; Kauno marios z11 613 -> 750). On those tiles the build time is within noise (section 11e). Breaklines (coast rings forced into the TIN) were not needed. They would cost about 2 triangles per lattice cell along every shore. Revisit only if the TIN-edge coastline reads as a staircase from low altitude beyond the vector reach.
- **Not drawn beyond 25 km:** roads, airports (beyond their flat strip and class 7 colour), buildings. City night lights beyond R3 are out of scope.

## 6. Generation, district level

> **Superseded (generator 4):** sections 6.1-6.4 are replaced by `docs/spec-seed-city-pipeline.md` sections 4-8: superblock plans (local streets by morphology, CityGML-style road objects, blocks by offset, lots by frontage / OBB subdivision with lot attributes, buildings from shape rules with pitched roofs and fenestration, LoD0-3 by band). R3 lights come from lots again (6.1a). Sections 6.5-6.7 still apply. The text below describes generator 3.

### 6.1 District plan (one per district, cached LRU)

Built on demand by the first tile overlapping it; holds the district's streets, blocks, lots and buildings in the city's tangent plane (f64 metres; never the world's floating origin). Target < 5 ms per district. A plan reads only its own polygon, its type/style/params, the arterials on its boundary and the water/fence/approach masks, so a plan never depends on a neighbouring plan.

- **Concurrency:** plans live in a map of `Arc<OnceLock<Plan>>` keyed by district id; the first worker to need a plan builds it, others needing the same plan wait on that `OnceLock` only from tokio blocking threads, never inside the rayon pool (a rayon job that finds a plan not ready re-queues the tile instead of blocking a pool thread). City plans work the same way.
- **Cache key and reconfigure:** caches are keyed by `(manifest content hash, seed.* profile hash, city id, district id)` and owned outside the per-configuration worker set, so a `configure_world` that changes neither (the bench settings panel re-applies on every change) keeps every plan.
- **Cap:** `seed.plan_cache` (default 2 048 plans) must exceed the plans the cover can touch at once: R3 reaches 25 km, so whole cities enter the cover (section 6.1a).

### 6.1a R3 and cost

R3 tiles carry light points from every building (contract section 8), which would force every district plan of a city the moment it enters the 25 km cover (~300 plans × 5 ms ≈ 1.5 s of worker time per city). Instead **R3 lights for seed tiles come from the lot grid without the buildings**: a district plan has two stages, (a) the street graph, blocks and lots (cheap: no footprints, shapes or heights; target < 1 ms) and (b) the buildings. R3 builds stage (a) only and emits one light point per lot that stage (b) will turn into a building. Each lot holds exactly one building (section 6.3 step 3; a U or L shape is still one building), and the light point of a seed building is defined from its lot, not its finished footprint: position = the lot's centroid, height = the lowest terrain height under the lot's corners + the building's height, in every band, both decided from `H(district, "building", lot)` and the lot alone (the keep/drop decision too), so stage (a) knows them. An R3 light is therefore the same point as the R2 light of that building and nothing jumps when an R3 tile splits into R2 children. The contract's "one light per owned building" holds unchanged. Stage (a) is cached with the plan; R2 and finer complete stage (b). The cover's box height uses the district's `max_height_m`. Budget per city: city plan < 20 ms; district plans only within R2 (7 km).

### 6.2 Street styles

All local streets end on the district boundary (an arterial or water), so districts never need to agree on local streets.

| Type / style | Streets | Blocks | Buildings (OSM `building` kinds, so light profiles resolve them) |
|---|---|---|---|
| `old_town` / `organic` | Voronoi of jittered points (spacing 50–90 m) inside the polygon, edges = streets (residential class, 5 m), 20 % of edges dropped by hash (merged blocks) | Small irregular | Perimeter blocks: buildings along the street edges with courtyards, 2–5 floors, kinds `apartments` / `retail` / `commercial`; `landmarks` × `church` or `tower` near the centre |
| `business` / `grid` | Grid aligned to the district's longest arterial edge, 110–150 m spacing, warped | Rectangular, slightly bent | Towers and slabs: height `max_height_m · u^2.5` (few tall), taller toward the district centre, kinds `office` / `commercial` (not `hotel`: the default profile resolves it to the residential light profile) |
| `residential` / `microdistrict` | Superblocks 300–450 m (grid as above), each with an internal service loop (class 6) | Large | Slabs 5/9/12/16 floors in rows or L/U groups around a yard, kinds `apartments`; a `school` or `kindergarten` per superblock |
| `residential` / `houses` | Grid 70–100 m with strong warp, 15 % of segments replaced by cul-de-sacs, contour-following once terrain exists | Small, curved | Detached houses on lots, 1–2 floors, pitched roof not modelled (flat), kinds `house` / `detached` |
| `industrial` / `grid` | Grid 200–300 m, light warp | Large | Big low boxes 6–15 m, kinds `industrial` / `warehouse` / `factory`, yards as industrial ground |
| `port` / `quay` | A quay road along the water edge, spurs inland every 120 m | Strips along the water | Warehouses, kind `warehouse`; open storage as industrial ground |
| `park` / `paths` | Curved footpaths (class 6) | One green area | None, or one `civic` pavilion by hash |

Every parameter in this table (spacings, warps, floors, kinds, height exponents, lit shares) is a profile default (section 9.2) a district can override with the same keys.

### 6.3 Blocks, lots and buildings

1. Blocks = faces of the district's street graph (streets buffered by half their width), clipped to the district polygon.
2. Lots: a block is split recursively across its oriented bounding box's long axis (split point jittered by hash) until a lot's frontage is within the style's range; lots without street frontage merge into a neighbour.
3. Buildings: footprint = lot inset by the style's setback; shape by hash among rectangle / L / U (style-dependent); height from the style and district (`max_height_m`, `density`), floors = height / 3 m, written as `levels` so night windows work; capped by any approach clearance (section 5.1). The building id is `H(district_id, "building", lot_index)` mapped into the OSM id space the mesher expects (way type, 2^40 offset so it never collides with a real OSM id).
4. Ownership, as for real data: a building is emitted into **every tile it overlaps** at each zoom; `owned` is true only in the tile holding its centroid (light point, slender-tower rule, footprint area), and the mesher clips it to the tile rectangle like a decoded MVT building. A building drawn only by its owner tile would stick out into the neighbour and pop when the sibling swap hides that tile. (Today's synthetic city avoids this only because its lots never cross a z16 edge; generated lots will.) Ownership and every keep/drop decision are taken in plan coordinates before projection (section 7). Streets and ground areas are clipped per tile like real ground.
5. Coarse bands: the same buildings at every zoom; the profile's per-band `min_area_m2` / `min_height_m` thin them (as for real data). A later option merges each block into one massing at R2 (section 11, S6).

### 6.4 Curves without seams

- Warp = value noise on an integer lattice (lattice point hash, quintic interpolation, only + and ×), applied to street vertices after the grid is laid out; amplitude per style. Districts warp in their own frame but never move their boundary vertices (arterial curves are fixed), so the street graph stays connected.
- Grids are laid out in district-local coordinates rotated to the district's main arterial edge, so neighbouring districts differ in orientation.

### 6.5 Lighting

Streets carry `lit` like real roads: arterials always, local streets by the style's lit share from `H(street_id)`; street lamps (`lights.street`, api 1.1) then work unchanged. Building light profiles come from the kinds (section 6.2) through the normal resolution; the generator never writes profile ids itself.

### 6.6 Ground classes

Districts write ground areas with landuse kinds of their type (`residential`, `commercial`, `industrial`, `park`, `port`); countryside writes `forest`, `farmland`, `meadow`; so the existing `ground.areas` groups and stacking apply. Airports need a new class: **class 7 `aeroway`** (unused today), with `b` = sub-kind / 8 (**0 airfield** = the grass inside the fence (S1), 1 runway paved, 2 runway grass, 3 taxiway, 4 apron and car park) instead of 0 for areas. Additive for the contract (section 9.1), but an old host does **not** draw it like class 6: `ground.gdshader` maps any unknown class to its "unknown class" colour (0.55, 0.60, 0.50), so on an old addon runways show as pale green. The addon version that ships the seed world adds the class 7 branch; a host that cannot update uses `seed.aeroway_class: 6` (profile) to write class 6 + the sub-kind in `b` instead.

### 6.7 Landmarks (author models)

A city is recognised by two or three buildings (a cathedral, a castle tower, a TV tower). The generator cannot make them, so the author makes them as models and the city file places them.

- **What the library does:** reads each landmark's position, heading, `footprint_m` ([width, depth] rectangle, centred, rotated by `heading_deg`; or `footprint` as a lon/lat ring) and `height_m`; removes every generated building intersecting the footprint grown by `clear_m`, leaves no lots there, and writes `ground` (`park` or `none`; a paved square needs a ground kind the profile tables do not have yet, added with S4a if wanted) under it; checks the runway and approach rules (section 4.6). It never loads the model: the core has no Godot types and no glTF reader.
- **Far stand-in:** in bands ≥ `stand_in_min_ring` (default R2, R3) the library emits a simple building in place of the model: the footprint extruded to `height_m`, with the landmark's `kind` (a building kind that the default profile resolves to the `landmark` light profile: `cathedral`, `church`, `tower`, `castle`, …; `landmark` itself is a profile id, not a kind; default `tower`), so the landmark keeps its floodlight and light point at night and its silhouette from far away. Bands below it get nothing from the library: the model is there.
- **What the game does (addon `WorldView`):** after configure, `landmarks()` lists the placements; `WorldView` instantiates each `model` (`PackedScene` / glTF) once, places it at `position` with `heading_deg` and `scale`, moves it with every origin shift, and shows it while the camera is within `bands.r{stand_in_min_ring − 1}_edge_m` of it (the same edge where the stand-in starts, with the addon's dither fade both ways). The earth-curvature drop (`d² / 2R`, ≈ 4 m at 7 km) is applied to the instance's y per frame, since the model's own materials do not include the curvature shader. Night: the model's materials may read the `simworld_night_factor` shader global.
- **Real cities:** `in_real` = `skip` (default) hides the landmark when the city's real source is active (OSM has the real building); `replace` keeps it and drops real buildings inside the footprint + `clear_m`, like airport fences.
- **Data, so deterministic:** landmarks are part of the city file and its hash; adding one changes only the districts it overlaps.

## 7. Determinism

- **Hash.** One function `H(parts…) → u64`: SplitMix64 folding of each part, implemented in `simworld-core` and pinned by a test vector. Each part is folded as a **type tag then its value**: tag 1 + `u64` for unsigned integers, tag 2 + two's complement for signed, tag 3 + byte length + FNV-1a 64 over the UTF-8 bytes for strings, tag 4 + the hash itself for a parent seed; after the last part the **part count** is folded. So `H("a", 1)`, `H("a1")`, `H(1, "a")` and `H("a", 1, 0)` all differ; the test vector includes these shapes. Never `std::hash`, `DefaultHasher` or `RandomState` (not stable across Rust versions). `unit(h)` = top 53 bits / 2^53.
- **Seed hierarchy.** `root = H("world", world_seed)`; `city = H(root, "city", id, variant)`; `airport = H("airport", ident)` (not under `root`: airports never re-roll); `district = H(city, "district", q, r, part)`; `edge = H(city, "edge", sorted cell pair)` (axial coordinates are city-local, so the city seed keeps cities apart); `building = H(district, "building", lot)`. Purpose tags keep independent choices uncorrelated. No seed depends on file order, the number of cities or any other city.
- **Locality.** A city plan reads only its file, the water near it and the airports near it; a district plan reads only its city plan entry. Editing Kaunas never changes Klaipėda, the countryside or any airport.
- **Numbers, two levels of guarantee.** (1) **Decisions are bit-identical everywhere:** inputs quantized on load (1e-7°, 1 cm); every decision (counts, types, keep/drop, which edge, which lot, ownership tile) is taken in plan coordinates on integers or on `unit(h)` compared with constants, before any projection. Plan geometry uses f64 `+ − × ÷ sqrt` (IEEE-exact on every target) and trigonometry only through the pure-Rust `libm` crate, never `f64::sin`. (2) **Output positions are equal after the golden quantization (1 mm)**, not bit-identical: plan → tile pixels goes through `geo` (`lat_to_tile_y`, `tile_y_to_lat`, ENU), which today uses std `tan`, `ln`, `sinh`, `atan`, `sin_cos` from the platform maths library (`geo.rs`, also used by real data). Moving `geo` to `libm` would make (2) bit-identical too, at the cost of one golden regeneration for real and synthetic data; decided in S0 (section 13).
- **No frame in generation.** Plan → tile pixels goes plan metres → lat/lon → tile pixels and never through the world `Frame` (floating origin). Today's `synthetic::generate(key, frame)` takes the frame (it places its landmarks at the origin); the seed generator's signature is `(key, &SeedWorld)` only, so the output does not depend on where the session started. The same holds for the terrain: the configure-origin flat zone (`terrain.flat_radius_m`, `flat_blend_m`) depends on where the session started, so **with a seed world it is off** (a non-zero value is a configuration error); the runway strips and the water rule (sections 5.1, 5.2a) are the only modifiers of the field, and they are data.
- **Heights in generation.** The height function is pure and has no load state (contract section 11), so plans may read it directly: `Terrain::height` with the seed world's strips and water, quantized to 1 cm before any decision, except the approach cap, which uses the unquantized mesher base (still deterministic: `geo` and `Terrain` go through `libm`).
- **Order.** Plans are built single-threaded per plan; collections are `Vec` sorted by id or `BTreeMap`; tile output is sorted (`GroundFeatures::sort`, buildings by id) before meshing, as today.
- **Versions.** `SEED_GEN_VERSION` (const in the core) changes with any intended output change and is returned by `seed_world_info()`; golden hashes (section 12) fail on any unintended one. The world a player sees = (generator version, manifest and file contents).

## 8. Real data per city

- **Routing.** Real coverage of a city = every z13 tile whose centre is within `radius_km` of its centre **and** that lies wholly inside the archive's header bounds (`min_lon`…`max_lat` of the PMTiles header, read when the source opens; a z13 tile only partly inside is not covered, so the underlay never has to fill a hole the archive was cut with). Computed when the source is activated (the header is needed), recomputed by `set_city_source`; `seed_world_info()` reports the covered tile count. A tile at any zoom is routed by its z13 ancestor: in an active real city → that city's source; else → seed generator. Because every finer tile nests in one z13 tile, all bands and siblings agree, and the sibling swap and fades need no new logic.
- **An active real city is skipped entirely by the generator**: no districts, streets or buildings of its synthetic footprint anywhere, also outside the square z13 coverage; that land becomes countryside. No half-synthetic edges, no duplicated districts.
- **Inside a real z13 tile the seed generator still draws** the airport layers (fences, runways, approach caps, airport lights) and a **countryside and water underlay** at the lowest stacking level, under the real ground: an archive cut along a polygon (or with sparse data) can leave covered tiles partly empty, and the underlay fills them. Inside an airport the generator wins over the archive, because OSM and OurAirports differ by metres: real **buildings** inside a fence are dropped; real **roads** (every class, service roads included) whose centreline enters a runway strip are cut at the strip edge; real **ground areas** of `aeroway` / airport landuse and every real area inside the fence are dropped (the generator draws the airport's own ground there). Real roads and areas outside the fence are untouched.
- **Sources.** Each city's `real` source is its own PMTiles archive (cut from the regional build with `data/build_tiles.sh BOUNDS=…`, or `pmtiles extract`), with its own SQLite cache file (one per source URL, as today). A `path` source is a local PMTiles file read with positional reads (new fetch backend; no HTTP, no disk cache), so it must be a real file: an OS path (or `user://`, resolved by the binding; not on iOS, section 8b). `res://` is refused (validation error): inside an exported `.pck` it is not a file. Shipping a real city inside the game means copying it to the cache folder on first run (host code).
- **Stale results.** Each city has a source generation counter, bumped by `set_city_source`; every tile job records the counters of the cities it touched, and `poll_ready` drops a finished tile whose counters are stale. A late synthetic tile never overwrites a real one (and the reverse).
- **"Download city"** (game menu): the game downloads the city's archive into the cache folder (section 8b) with its own `HTTPRequest` (progress UI is the game's), then calls `set_city_source("kaunas", "<OS.GetCacheDir()>/<project>/simworld/cities/kaunas.pmtiles")`, i.e. `cache://simworld/cities/kaunas.pmtiles` resolved like `WorldView.CacheDir` (`cache://simworld/tiles`). The downloads folder is a **sibling** of `cache_dir`, never inside it: the library may delete anything in its own folder (section 8b). Every tile in the city's coverage is re-delivered under the same key (rate-limited, like light-mode re-deliveries); no teleport. `set_city_source(id, "")` switches back to synthetic. Streaming the `url` directly works too (online, LRU-cached), but a downloaded file is never evicted.
- Vilnius today = a city file with `real.url` pointing at `data/out/vilnius.pmtiles` and the source set at configure time.

## 8a. Author edits

The model is Minecraft's: the seed always regenerates the pristine city; changes live beside it, never in it. Unlike Minecraft (which saves whole touched chunks), only the **difference** is stored, and only the **game author** edits, at development time. Players never change the world; there are no per-player saves and no runtime edit API.

```
city the player sees = generate(world_seed, city file)  +  apply(city edits file)
```

- **Reset** = remove the city's `edits` key (or the file): the same seed gives the city exactly as it was before any edit. `seed export --pristine` shows it without touching the file.
- **Format: JSON, shipped in the game** (`res://`), read through Godot's file system like every seed file. Not SQLite: in an exported game `res://` is inside the `.pck`, which SQLite cannot open without copying it out first, and a binary file gives no reviewable diff in git. An author-side SQLite workspace in `simworld-cli` stays possible later; the shipped format is this JSON.

### 8a.1 Edits file `cities/<id>.edits.json`

```json
{
  "format": 1,
  "city": "kaunas",
  "generator_version": 3,
  "city_hash": "9f2c41d07ab3e815",
  "cells": {
    "3,-2": { "type": "park" },
    "0,1": { "style": "organic", "max_height_m": 18 }
  },
  "buildings": {
    "s1099511627776123": { "height_m": 140, "kind": "office", "anchor": [23.9071, 54.8962, 2150] },
    "s1099511627776456": { "delete": true, "anchor": [23.9102, 54.8977, 640] }
  },
  "add": [
    { "id": "a1", "kind": "church", "height_m": 45, "levels": 1,
      "footprint": [[23.8920, 54.8968], [23.8925, 54.8968], [23.8925, 54.8965], [23.8920, 54.8965]] }
  ]
}
```

| Part | Meaning | Applied |
|---|---|---|
| `cells` (**generator 4: replaced by `zones` by point or polygon**, `docs/spec-seed-city-pipeline.md` section 10) | Per hex cell (axial `q,r`): `type`, `style` and any district key (`max_height_m`, `density`, `lit`, style parameters). Overrides the automatic placement for that cell | City plan, after placement (section 5.3 step 3), before arterials |
| `buildings` | Per generated building id: `height_m`, `levels`, `kind`, `delete`. `anchor` = centroid lon, lat and footprint area m² at export time, for re-matching | District plan, after section 6.3 step 3 |
| `add` | New buildings with an explicit footprint (lon/lat ring), `kind`, `height_m` / `levels`. Generated buildings intersecting an added footprint are removed. Must not intersect a runway strip (error) | District plan of the district holding the footprint centroid |

- Ids: generated buildings keep their `s`-prefixed string form of the id in the OSM id space (section 6.3), so the profile's `buildings.overrides` (light profile by id) works on them too. Added buildings get ids `H(city, "add", id)` in the same space.
- Edits never bypass the airport rule: an edited height is still capped by the approach surface (logged).
- Edits are data, so determinism holds: same seed + same files + same edits = same world (section 7). Edited districts are re-hashed in the golden tests like any other.

### 8a.2 Author workflow

1. Tune the city file (districts, layout, `variant`) with `seed preview` until the layout is right. **Layout first, buildings second**: section 8a.3.
2. `seed export --city kaunas` → `kaunas-buildings.csv` + `kaunas.geojson`. CSV columns: `id`, `district_id`, `district_type`, `style`, `cell`, `lat`, `lon`, `area_m2`, `height_m`, `levels`, `kind`, `light_profile`, `cap_m` (approach cap, empty if none), `edited` (yes/no).
3. Edit in a spreadsheet (change `height_m`, `levels`, `kind`; set `delete` in an extra column or remove the row) or in QGIS (GeoJSON, over OSM or imagery for reference; also the place to draw `add` footprints).
4. `seed import-edits` diffs against the pristine city and merges the result into the edits file (a removed CSV row = `delete`; unchanged rows write nothing). Cell edits and `add` entries are written by hand or from the GeoJSON.
5. Check in the bench: `--world=` loads the files; the debug method `debug_building_at` (section 9.1) shows the id and values of the building under the cursor, so a building seen in flight can be found in the CSV.
6. Commit the city file and its edits file together.

### 8a.3 Id stability and re-matching

A building id is `H(district_id, "building", lot)` (section 6.3), so it survives everything outside its district and breaks when its district is re-laid out:

| Change | Effect on edits |
|---|---|
| Another city, profile, night, airports elsewhere | None |
| A district's style parameters, density, its cell's type, or its arterial edges | Building edits **in that district** may orphan; cell edits and `add` never do |
| `variant`, `layout`, `cell_size_m`, `radius_km`, `center` | Most building edits may orphan |
| `generator_version` | Possibly many (an intended generator change) |

On load (and in `seed validate`), each building edit whose id no longer exists is **re-matched**: the generated building of the same district type whose centroid is within 10 m of `anchor` and whose area is within ±25 % takes the edit (closest first, each building at most once). Edits that still match nothing are **orphans**: ignored, warned once, listed by `seed validate` and `seed_world_info()`. `seed import-edits --rebase` rewrites the file with the new ids and anchors and drops orphans after confirmation. The edits file records `generator_version` and `city_hash` (hash of the city file's canonical content) so a mismatch is reported up front instead of discovered building by building.

## 8b. Storage: what the library owns

The library never connects to the game's database or save files, and the seed world adds no database of its own. The current split stays:

| Data | Owner | Format | Where |
|---|---|---|---|
| Seed files: manifest, cities, edits, airports, water | Game (shipped) | JSON, read-only | `res://` (read through Godot's file system) |
| Tile caches, one per source (`tiles-{hash}.sqlite`, `docs/spec-tile-cache.md`) | Library | SQLite, disposable | `cache_dir` |
| Downloaded real-city archives | Game downloads, library only reads | PMTiles | `cache://simworld/cities/` (sibling of `cache_dir`, never inside it) |
| Saves, settings, progress, anything gameplay | Game | Game's choice (its own SQLite is fine) | `user://` |

Rules:

- **The library only opens SQLite files it created**, in `cache_dir`, under its folder lock (contract section 11a). It may delete and rebuild them at any time (corruption, fingerprint change, schema change), which is why game data must never live there and why the library must never be pointed at a game database.
- **Cache folder on iOS is not `user://`.** Godot maps `user://` to the app's `Documents` folder on iOS, which iCloud backs up and the Files app can show; Apple's data storage guidelines reject re-downloadable data there. `cache_dir` and downloaded city archives go to `OS.GetCacheDir()` (`Library/Caches` on iOS; the platform cache folder elsewhere) under `<project name>/simworld/`: `tiles/` (= `cache_dir`, the library's) and `cities/` (the game's downloads). **Fixed (2026-10-27):** `WorldView.CacheDir` defaults to `cache://simworld/tiles`; the `cache://` prefix resolves to `OS.GetCacheDir()/<project name>/` (the desktop cache folder is shared by all apps, so the project name keeps games apart). Empty still means no disk cache; an explicit value still wins (the bench scene keeps `user://tile_cache` on desktop). Verify the mapping on a device in the iOS export check (`docs/spec-library-packaging.md`).
- **Purge is normal.** iOS may empty `Library/Caches` while the app is not running. Tile caches refill from the network (or the world shows the synthetic city until they do). A downloaded city whose file is gone falls back to synthetic: `set_city_source` / `real_sources` with a missing or unreadable `path` routes the city to the generator, sets `status()` to `degraded` with `real source missing: <city>` (logged once), and `seed_world_info()` reports the city's mode as `synthetic (missing)`, so the game can offer "download again". Nothing is lost that cannot be downloaded again; anything that can't be (the author's edits, the game's saves) is never in the cache folder.

## 8c. Many tile sources in one world

Today a world has one source: one fetcher, one SQLite cache (`tiles-{hash}.sqlite`), one 300 MiB cap, one tileset check and one `status()`. A seed world has none to N (one per active real city). Rules:

- **Fetchers.** One fetcher per active source, created on activation, closed (cache file kept) when the city goes back to synthetic. URL sources share the HTTP client and its connection pool; `path` sources read the local file with positional reads and have no disk cache.
- **One shared cap.** `cache.cap_mb` (300 MiB) is the cap for **all** tile cache files of the world together, not per file: each URL source keeps its own SQLite file (as today, so one source's fingerprint change wipes only its file), and eviction is global LRU by the last-access time across the files (the cache thread owns all of them). `debug_stats().cache_mb` reports the total, plus `cache_mb_by_source` (source → MiB). Downloaded `path` archives are not cache and do not count.
- **Tileset contract per source.** Each archive is checked on open like today's single source (layers, zooms, `buildings` with `kind`); a failing source puts only its city back to synthetic, with status `degraded` (`real source unusable: <city>: <reason>`).
- **Status aggregation.** The seed generator never fails, so a seed world is never `offline`. Per source the existing rules give ok / degraded / offline; the world's `status()` is the **worst** of them with offline counted as degraded (`real source offline: <city>`; the city shows synthetic, or its cached tiles if any), plus the missing-file rule of section 8b. Several problems: the message names the first by city id order and adds `(+N more)`; `seed_world_info()` lists each city's source state.
- **Cache folder lock (contract 11a).** A seed world with a non-empty `cache_dir` takes its lock when any URL source can be active (any city file with `real.url`, or `real_sources` with a URL), at configure time, so a later `set_city_source` never fails on the lock. With only `path` sources or none it takes no lock and needs no `cache_dir`. `set_city_source` with a URL on a world configured without `cache_dir` streams uncached.

## 9. Contract and profile changes (additive minor)

### 9.1 `WorldStreamer`

- `configure_world` option `seed_world` (String, path to `world.json`). With it, `tile_url` must be empty, the host must request the layer `terrain` with `terrain.enabled`, and `terrain.flat_radius_m` / `flat_blend_m` must be 0 (else error, sections 5.5, 7). Optional `real_sources` (Dictionary city id → url/path) activates real sources at configure time. Errors name the file, the key and the value, like profile errors. The result's `attributions` gains "© OpenStreetMap contributors" (water, real cities) and "Airport data: OurAirports" (courtesy, public domain).
- `set_city_source(city_id: String, source: String) -> Dictionary` (`ok`, `error`): section 8.
- `seed_world_info() -> Dictionary`: `generator_version`, `phases` (Array of the built phase names, `"S1"` … `"S6"`, so the bench self-test of section 11a runs only the checks that apply), `world_version`, `cities` (id, name, centre, radius, source mode, district counts, edit counts applied / re-matched / orphaned), warnings from validation.
- `debug_building_at(local_pos: Vector3) -> Dictionary` (author tool, like `debug_tiles`): the building whose footprint contains the point's x/z among resident R0/R1 tiles: `id`, `district_id`, `district_type`, `cell`, `height_m`, `levels`, `kind`, `edited`; empty when none. Not for gameplay picking.
- `airports() -> Array[Dictionary]`: ident, name, type, reference point (geodetic and local), elevation, runways with both ends (geodetic and local, with displaced thresholds), width, surface, `approximate` flag. Local coordinates follow the floating origin (call again after `origin_shifted`).
- Ground COLOR class 7 `aeroway` (section 6.6); ground COLOR `g` on areas = tint / 8 (section 5.2; roads keep `g` = lit). `debug_tiles` entries gain `source` (`"seed"`, `"real:<city>"`).
- **Terrain with a seed world:** `ground_height_at` and every terrain output include the runway strips and the water rule (sections 5.1 step 9, 5.2a); terrain tiles carry **COLOR** (`r` = ground class / 8, `g` = tint / 8; section 5.5) where contract 9a says "No COLOR" today; a non-seed world's terrain tiles are unchanged. The addon's `terrain.gdshader` gains the class colours.
- Status and stats with several sources (section 8c): `status()` aggregation and messages, `debug_stats()` `cache_mb_by_source`, the cache lock rule for a seed world.
- Ready-tile keys `airport_lights` (PackedVector3Array) and `airport_light_colors` (PackedColorArray), like the street-lamp keys of 1.1 (section 5.1 step 7). `airports()` gains each runway's `lighted` flag, light positions, threshold and touchdown zone points.
- `landmarks() -> Array[Dictionary]` (section 6.7): `city`, `id`, `name`, `model`, `position` (local, y = `ground_height_at`), `lat`, `lon`, `heading_deg`, `scale`, `stand_in_min_ring`. Local positions follow the floating origin (call again after `origin_shifted`); landmarks of a city whose real source is active and `in_real` = `skip` are left out.
- The ready-tile dictionary is otherwise unchanged.
- Addon: `WorldView.SeedWorld` (path to `world.json`; non-empty = seed world, `TileUrl` must be empty, the terrain layer is turned on) and `WorldView.SetCitySource(id, source)`, a thin wrapper that also resolves `cache://`.

### 9.2 World profile section `seed` (profile_version 6)

`seed.airport` (`strip_half_m`, `strip_end_m`, approach `divergence` / `length_m` / `slope` per runway class, `fence_margin_m`, `taxi_offset_m`, default runway widths by type, light spacings and colours, `approach_penalty`), `seed.city` (`arterial_drop`, `bridge_spacing_km`, edge warp amplitude), `seed.styles.<type>.<style>` (every parameter of section 6.2), `seed.countryside` (forest share, noise scale, tint range), `seed.water` (`blend_m`, raster size), `seed.airport.shoulder_m`, `seed.city.max_district_m2`, `seed.plan_cache` (district plan cap), `seed.aeroway_class` (7 or 6, section 6.6), `seed.roads` (`sample_m`, `max_bridge_m`, wobble amplitude per class), `seed.landmarks` (`stand_in_min_ring`, default 2). The profile says how a style looks; the city file says what is where. City district entries may override style keys by name.

## 10. Interaction with other milestones

1. **Night (M6):** generated buildings have kinds and `levels`, so windows, light profiles, landmark floodlights and street lamps work unchanged; the business district's towers carry the skyline at night.
2. **Imagery (M7):** imagery is per tile and does not exist for a synthetic city; a synthetic city keeps class colours. No conflict.
3. **Terrain (M9, procedural, built):** a seed world requires the terrain layer (section 5.5). The seed world modifies the height function with data only: runway strips (section 5.1 step 9) and the water rule (section 5.2a); the configure-origin flat zone is off. The drape rule (contract section 9) applies to generated ground in all four bands and to airport lights like street lamps. Since the function is pure and available at plan time, **terrain-aware placement is part of S4**: district cells whose mean slope exceeds `seed.city.max_slope` become `park`; `houses` streets follow contours; arterials and radials avoid steep runs. These decisions read the quantized function directly (section 7). The matching changes to `docs/spec-terrain.md` are in its section "World seed".

## 10a. Budgets

| Item | Budget (Baltics, to measure in S1–S4) |
|---|---|
| Water index (z11 cells, mixed cells' rings) | ≤ 16 MB |
| Terrain water raster (section 5.2a) | ≤ 30 MB desktop; iOS ≤ 16 MB (32 × 32 raster) |
| Runway strip index | ≤ 256 KB; per terrain sample outside airports: one bounds check |
| Height function with seed modifiers | ≤ 1.5 × today's per-sample cost (terrain bench) |
| Airport set (≈ 100 airports after filtering) | ≤ 2 MB |
| City plan | < 20 ms build, ≤ 1 MB each; all cities of the manifest resident ≤ 16 MB |
| District plan | stage (a) streets + lots < 1 ms, full < 5 ms build, ≤ 200 KB (districts capped at `max_district_m2`, section 5.3 step 4); cache ≤ 2 048 plans ≈ 64 MB desktop; iOS cap 512 plans ≈ 32 MB (R2 needs ≈ 150–250 plans for one city) |
| Tile caches, all sources together | `cache.cap_mb` (300 MiB) shared (section 8c) |
| Total seed memory on iOS | ≤ 64 MB on top of today's budgets |

## 11. Phases

| Phase | Content | Visible result |
|---|---|---|
| S0 | Hash + test vector (arity shapes); file schemas incl. `airports.overrides`; `simworld-cli seed airports / water / validate`; check OurAirports runway ends against OSM for EYVI, EYKA, EYPA, EVRA, EETN (offset in metres, recorded here, overrides written where needed); **measure the water file size and the water raster memory, and prototype the terrain-water rule** (section 5.2a) on the Baltic coast, since both gate S1; decide `geo` → `libm` (section 7) | Files for the Baltics; validation report; a terrain screenshot with a flat Baltic. **Built, except the sea / full Baltic `water.json` and the screenshot** (section 11b) |
| S1 (**built, Rust and Godot half**, sections 11c, 11d) | Manifest load, water index, z13 routing, water + countryside ground in **all four bands** (R3 under `ground.r3`) with the field tint, the **terrain coupling** (runway strips, water rule, terrain COLOR by class + `terrain.gdshader` branch), airports (runway, strip, taxiway, apron, terminal, hangars, tower, class 7 + host shader branch, airport lights), `airports()`, `seed_world_info()`, contract bump | Flying over the Baltics at 300 m and at 3 000 m: sea and coast to the horizon, lakes, fields, every airport in place and lit at night |
| S2 | City plan: hex cells, footprint union, classification, automatic placement (approach penalty), arterials (spanning tree, drop share, district area cap, water rule, bridges), radials and access roads; `seed preview` SVG; districts drawn as ground areas only. Experiment: Voronoi cells from jittered points (one per hex, still local) against plain hexes, pick by preview | Cities as coloured district maps with curved arterials |
| S2b | Inter-city roads: roads file, `seed roads` draft + GeoJSON round-trip, splines, bridges, city entries claiming goal-directed radials, R3 roads | A1 Vilnius–Kaunas visible from altitude, entering both cities as radials |
| S3 | District street styles, blocks, lots (district plan stage (a)); R3 lights from the lot grid | Street networks; city light carpets from R3 |
| S4 | Buildings (stage (b)), heights, approach caps, lighting; terrain-aware placement (slope → park, contour streets) | Full synthetic cities on relief, day and night |
| S4a | Landmarks: city file key, footprint clearing, far stand-in, `landmarks()`, `WorldView` instancing with origin shift, distance switch and curvature drop | A cathedral model in Kaunas, its floodlit stand-in from far away |
| S4b | Author edits: edits file, `seed export` / `import-edits` / `--rebase`, re-matching, `debug_building_at`, bench overlay showing the building under the cursor | Kaunas tuned by hand: a taller business tower, a park cell, a real cathedral footprint |
| S5 | Real sources per city: z13 routing to real archives (header bounds), local file backend, several fetchers with one shared cache cap and status aggregation (section 8c), airport precedence over real roads and areas, `set_city_source`, re-delivery, missing-file fallback (section 8b); bench menu "download city" | Kaunas synthetic ↔ real at runtime; Vilnius real |
| S6 | `grid`, `radial`, `linear` layouts; R2 block massing; polish (**layouts moved into the city pipeline phases C0-C8**, `docs/spec-seed-city-pipeline.md` section 13; only R2 block massing remains here) | Klaipėda (linear along the coast), small towns |

Test bench: the two-city test scene of section 11a; `BenchArgs` gains `--world=` (any manifest in the main scene `CitySpike.tscn`, which otherwise keeps the Vilnius archive). A larger Baltic manifest (Klaipėda, Palanga EYPA, Riga, Tallinn) comes after S6.

## 11a. Test scene: two synthetic cities

The bench's main scene shows real Vilnius from `vilnius.pmtiles`. The seed world gets its own scene in which **two synthetic cities replace it**, so every phase from S1 on is tested by flying, by screenshots and by a headless self-test, with no tile server.

### 11a.1 Content

| Item | Choice | Why |
|---|---|---|
| Scene | `low-poly-map/scenes/SeedTwoCities.tscn`: a copy of `CitySpike.tscn` (same camera, overlay, menus, `NightRenderController`) whose `WorldView` has `SeedWorld = "res://world/two-cities/world.json"`, `TileUrl = ""`, terrain on, `CacheDir = ""` (no URL source, so no cache and no lock, section 8c) | Same tools as the main bench; nothing changes for the Vilnius scene |
| Manifest | `low-poly-map/world/two-cities/world.json`, `world_seed` `"bench-two-cities"` | Checked in; the files are the test data |
| City 1 | `vilnius.json`: `old_center_hex`, radius 8 km, cell 800 m, every district type (`old_town`, `business` with `max_height_m` 120, `residential` microdistrict, `industrial`, `park` near the river), `fill` houses, airport `EYVI`; `real.url` = the bench's `vilnius.pmtiles` for the S5 switch test only (off by default) | The real archive exists, so synthetic ↔ real can be compared on the same spot |
| City 2 | `kaunas.json`: `old_center_hex` (`grid` once S6 adds it, to test a second layout), radius 6 km, `port` near the river and the reservoir (Kauno marios), airport `EYKA`; one landmark (a stand-in box with `kind` `cathedral` and a placeholder `.glb`) | Rivers (Nemunas, Neris) and a lake inside a city: bridges, the water rule, the lake level |
| Airports | `airports.json` cut with `seed airports --countries LT`; overrides for EYVI / EYKA if S0 measures offsets | Two real airports with lights, one with crossing grass runways (a small field from the file) |
| Water | `water.json` cut with `seed water` for Lithuania plus the coast, `outside: "sea"` | The coast bookmark tests the sea on the terrain |
| Roads | `roads.json`: A1 Vilnius–Kaunas (motorway, drafted by `seed roads`), one `airport:EYKA` access road | Road entry into both cities, R3 roads |

The same files are the Rust integration fixture: `simworld-core` tests read them from `low-poly-map/world/two-cities/` (relative to the workspace), so the scene and `cargo test` check one world.

### 11a.2 Launch

- Interactive: `$G --path . res://scenes/SeedTwoCities.tscn -- [--seed-spot=<name>] [--time=22:00]`.
- Also from the main scene: `$G --path . -- --world=res://world/two-cities/world.json`.
- `--seed-spot=<name>` places the camera through `update_camera_geodetic` (heading and altitude per spot): `vilnius` (old town, 400 m), `kaunas` (confluence, 400 m), `eyvi` / `eyka` (on final for the main runway: 3 nm out, 300 m, aligned with the runway, so the approach clearance and the lights are in view), `a1` (above the motorway half way, 1 500 m, looking east), `cruise` (between the cities at 3 000 m: both light carpets at night), `coast` (Klaipėda coast without a city, 2 000 m: the sea on the terrain), `lake` (above Kauno marios, 500 m).
- F1 overlay gains a seed line: the source of the tile under the camera (`seed` / `real:<city>`), city plans and district plans cached, `seed_world_info()` warnings.
- Author aid (S4b): a key (B) prints `debug_building_at` for the point under the screen centre.

### 11a.3 Self-test `--seed-selftest` (headless)

`SeedSelfTest` (bench script, like `TerrainSelfTest`): `$G --headless --path . res://scenes/SeedTwoCities.tscn --quit-after 3000 -- --seed-selftest`, prints `SEED SELFTEST PASS` or one `FAIL <check>: <detail>` line per failure and exits with code 1 on failure. Checks are grouped by phase; a group runs only when the library reports that phase built (`seed_world_info()["phases"]`, section 9.1), so the test grows with the work:

| Phase | Checks |
|---|---|
| S1 | `status()` ok, no `degraded` message; `airports()` contains EYVI and EYKA with both runway ends; at 20 points along each runway centreline `ground_height_at` is linear between the threshold heights within 1 cm and equal across the strip; at the `coast` spot `ground_height_at` = `terrain.sea_level_m` at 10 points offshore and every terrain tile delivered there has its sea vertices at sea level; at the `lake` spot the lake is flat; at every spot the first tile arrives within 5 s and the R3 ground is present; at 22:00 the airport light keys are non-empty for tiles over the runways |
| S2 | Both cities' plans built; no district ground area intersects a runway strip; arterial graph connected (`seed_world_info()` per city) |
| S2b | Flying the A1 from city to city at 250 m/s: no frame without vector ground under the camera; the road's last vertex at each city equals a radial's first vertex (from `seed_world_info()` debug data) |
| S3–S4 | Buildings in R0 at `vilnius` and `kaunas`; no building in either strip (`debug_building_at` at 50 points on each strip returns empty); every building inside an approach trapezoid at or below the surface; night: windows and street lamps present |
| S4a | The Kaunas landmark: `landmarks()` lists it, the model instance and the stand-in are never both visible past the dither fade, both follow an origin shift |
| S5 | Vilnius switched to real with `set_city_source` and back: tiles re-delivered under the same keys, the back-switch reproduces the first synthetic hashes (`debug_tiles` content hash), EYVI identical in both modes |
| All | Determinism: two runs of the self-test print the same per-spot tile hash summary (`SEED HASHES <spot> <hash>`); the CI step compares them with a checked-in file |

### 11a.4 Visual acceptance (screenshots)

`--shot=` at each spot, day (13:00) and night (22:00), saved under `low-poly-map/shots/seed/` (git-ignored) and reviewed per phase: S1 sea and coast, fields with tint, both airports lit; S2 district map colours and curved arterials; S2b the A1 from altitude; S4 skylines of both cities and the light carpets from `cruise`; S5 Vilnius synthetic vs real from the same spot.

## 11b. S0 results (2026-10-27)

Built in `rust/simworld-core/src/seed/` and `rust/simworld-cli/src/seed/` (no `World` wiring, no api / profile change; `CHANGELOG.md` Unreleased). Tests: 205 passed, 0 failed, 8 ignored (191 core + 3 godot + 11 CLI; the new ignored test is the water measurement).

- **Hash:** `H(parts…)` with typed parts (tag 1 unsigned, 2 signed, 3 string = byte length + FNV-1a 64, 4 parent seed), part count folded last; start state `H_INIT` = "SIMWORLD", each fold `splitmix64(state ^ word)`; 18 values pinned in tests incl. `H("a",1)`, `H("a1")`, `H(1,"a")`, `H("a",1,0)`. Rust integer literals are `i32`, so they hash as signed.
- **Files:** schemas in `addons/simworld/seed/{world,city,airports,water,roads}.schema.json`; loader order is schema, typed parse, quantize (1e-7°); errors name file, key path and value. Deferred to later phases: road vs runway strip / water crossings (S2b), landmark vs strip / approach (S4a), exact land-cell counts (S2). District style keys are rejected by the strict schema until S3 adds them. The fixture world is `rust/simworld-core/testdata/seed/two-cities/`; section 11a.1's `low-poly-map/world/two-cities/` replaced it in S1 (section 11c).
- **CLI:** `seed airports` (`--bbox` or `--countries`, `--types`, `--source-date`), `seed water` (GeoJSON `--sea` / `--land` / `--lakes` / `--rivers`; **`--land` is the recommended sea source**, the osmdata water polygons come split into pieces), `seed validate`.
- **`geo` → `libm`:** done. The system maths library differs from libm by 1 ulp in 4–38 % of random inputs (sin 3.8 %, cos 4.1 %, tan 38 %, ln 8.3 %, atan2 16 %, hypot 12 %); the terrain's `sphere_point` used the platform `sin_cos`, so terrain heights were not bit-identical across platforms before. Pinned by `libm_bits_are_pinned` and `std_transcendentals_are_not_used`.
- **Water, measured (real Lithuanian lakes and rivers):** 3 716 lakes + 457 rivers, 59 460 points, **1.54 MB** (14 344 features under 2 ha dropped); index 1.89 MiB; rasters 6.72 MiB at 64² / 1.72 MiB at 32². Synthetic coast (1 557 km) + 800 lakes: 835 rasters = 9.85 MiB at 64², 2.5 MiB at 32². No sea yet (osmdata water / land polygons are not on disk).
- **OurAirports cut (LT, LV, EE):** 231 airports, 77 runways, 55 runways with an end missing; 58 airports and 50 approximately placed runways with the default types.
- **Runway ends against OSM** (OurAirports ends are the physical pavement ends; OSM runway ways run threshold to threshold, so the physical ends come from the `displaced_threshold` ways):

| Airport | End offsets | Bearing | Result |
|---|---|---|---|
| EYVI | 3.1 / 6.1 m | 0.18° | no override |
| EYKA | 2.7 / 2.3 m | 0.08° | no coordinate override; displaced thresholds differ (OurAirports 340 / 240 m, OSM about 298 / 300 m), unchecked against the AIP |
| EVRA | 2.7 / 1.4 m | 0.02° | no override |
| EETN | 1.9 / 0.6 m | 0.02° | optional `he_displaced_threshold_ft: 1350` (OSM maps 411 m at the 26 end, OurAirports has none), adopt only after the AIP confirms |
| **EYPA** | **180.5 / 180.5 m** | 0.01° | **override** (section 4.1 example): ends 1 977 m apart against `length_ft` 2 280 m, OSM geometry 2 341 m; stale data from before the extension |

Elevations match the OSM `ele` at all five; no runway is closed; each airport has one runway in both sources. Data source: `ourairports.com/data/` (the `davidmegginson.com` host did not resolve).

- **Still to run on real data:** the sea and coast (osmdata water or land polygons, LV / EE extracts), the full Baltic `water.json`, the S0 flat-Baltic terrain screenshot (needs the S1 wiring), the x86_64 golden check (P6 CI).
- **Open for S1:** sloped river levels (section 5.2a), the coast step on rough shores, `lighted` in `airports.json` (added to the schema; unknown numbers are `null`).

## 11c. S1 results (2026-10-28)

Built in `rust/simworld-core/src/seed/` (`airport`, `countryside`, `terrain`, `world`; `water` extended), wired into `World`, the worker and the terrain; contract **api 1.7** (`docs/contract.md` 3.1 `seed_world`, 3.6, 4, 8, 9, 9a), **profile_version 6** (section `seed`, only the keys S1 uses), `CHANGELOG.md` Unreleased. Tests: 218 passed, 0 failed, 9 ignored (204 core + 3 godot + 11 CLI). The Godot half (terrain shader class branch, ground class 7 branch, `WorldView.SeedWorld`, `SeedTwoCities.tscn`, `--world=`, `--seed-spot`, `SeedSelfTest`) is the next step.

- **Test world** `low-poly-map/world/two-cities/` (the single copy; `cargo test` reads it, `simworld-cli seed validate` validates it): the S0 fixture's cities and roads; the **real Lithuanian OurAirports cut** (77 airports, 28 selected: 3 large, 4 medium, 21 small; 30 runways approximate; EYPA override of section 4.1); the water file = the **Lithuanian Baltic coast** (sea polygon built from the Geofabrik extract's `natural=coastline` ways, clipped to 19.5-21.6 E / 55.272-56.07 N, plus a rectangle for the Latvian coast to 56.46 N, DP 50 m by `seed water --sea`) plus the S0 lakes and rivers (3716 lakes, 457 rivers): 4175 polygons, 59 929 points, 1.5 MB, bbox [19.5, 53.89, 26.84, 56.46], `outside: "land"`. **Missing / approximate:** the sea south of 55.272 N (Kaliningrad coast) and west of the Curonian Spit's Russian part is land in the file (the LT extract's coastline ends at the border); the Latvian coast north of 56.07 N is a rectangle, not the real coast; no Latvian or Estonian water (their extracts are not on disk); osmdata land/water polygons were not downloaded (the extract's coastline sufficed for Lithuania). The Baltic manifest (EYPA override, LV / EE) is still to build.
- **Measured** (dev profile: workspace opt-level 1, dependencies 3; Apple arm64):

| Item | Measured | Budget (10a) |
|---|---|---|
| Load (manifest + 4175 polygons + index + rasters + 28 airports) | 285-294 ms, on the main thread in `configure_world`; a reconfigure with the same files reuses it (0 ms) | — |
| Water index (604 mixed cells) | 2.43 MiB (64 bands; 1.93 MiB with S0's 16) | ≤ 16 MB |
| Water rasters 64² (572) | 6.76 MiB | ≤ 30 MB desktop |
| Runway strip index | 108 KiB | ≤ 256 KiB |
| Airport set (28) | 128 KiB | ≤ 2 MB |
| Height per sample (200 000 random points over Lithuania) | field 217 ns -> 312 ns (**1.44x**); land points 1.39x | ≤ 1.5x |
| Seed tile build (EYVI / countryside), R0 / R1 / R2 / R3 | 0.14 / 0.32 / 0.93 / 0.83 ms; 0.18 / 0.35 / 0.46 / 0.64 ms; open sea 0.01-0.06 ms | — |
| Terrain tile with seed rules + COLOR, z14 / z12 / z10 | 1.6-2.1 / 20 / 20-24 ms (plain field 1.6 / 17.7 / 18 ms); over the sea 0.3 / 4.5 / 6 ms (flat: few triangles) | — |
| First tile at EYVI (World, 300 m AGL) | 429 ms; every band with airport lights, R3 ground and coloured terrain after 1.4 s | < 5 s |

- **Corrections to this spec found in S1:** (1) height order field -> water -> strips (section 5.2a), not "after the strips"; (2) runway slope cap `seed.airport.max_slope` (section 5.1 step 9); (3) rivers have no terrain rule (section 5.2a decision); (4) the water test is evaluated just inside a mixed cell for points on its border (an S0 bug: a terrain lattice row on a z11 boundary read as land and stood out of the sea); (5) ground class 7 has a sub-kind 0 (airfield grass in the fence) and the fence, pavements and runways use stacking levels 8 / 9 / 10 (above every area level; in the road levels' range, which is safe while no road enters a fence); (6) terrain COLOR `r` is class / **8** while the vector ground keeps class / 16 (contract 9a states both); (7) a seed world's countryside re-rolls with `world_seed` (seeded `H(root, "countryside")`); (8) parallel or crossing runways: in their strips' overlap the first key wins, so between close parallel runways the ground steps where the nearest strip changes (accepted; EYSA 14L/32R vs 14R/32L; **superseded by the S1 fix, section 11e:** overlapping strips share one plane, and a step remains only in the fence grass between strips that do not overlap).
- **Decisions:** api 1.7 (next free minor); rivers vector-only; slope cap 1 %; airport lights at terrain + band lift + pavement level x step + 0.5 m (they sit on the drawn runway in every band; `airports()` gives them at ground + 0.5 m); airport lights in every band, never clustered, intensity `seed.airport.lights.intensity` 0.35; seed worlds take the cache lock only with a city `real.url` (two-cities' Vilnius has one: the bench scene passes no `cache_dir`, section 11a.1); the configure-time load is synchronous (about 0.3 s once per file set).
- **Not built in S1 (later phases):** cities (S2: no city ground or buildings yet; city footprints are countryside), roads (S2b), real sources and `set_city_source` (S5), `landmarks()` (S4a), `debug_building_at` (S4b), the approach clearance (buildings come in S4), access roads, coast breaklines in the TIN (only if screenshots show colour bleeding), `debug_stats` `cache_mb_by_source` (S5).

## 11d. S1 Godot results (2026-10-28)

Built in the addon (`addons/simworld/`) and the bench (`low-poly-map/`); no Rust change, no contract change (`api_version` 1.7, facade `LatestKnownMinor` 7). Details of the addon side are in `docs/addon-notes.md` and the bench side in `docs/bench/notes.md` "World seed".

- **Addon:** `WorldView.SeedWorld` (non-empty = seed world: `TileUrl` ignored with a warning, terrain layer forced on, `terrain.enabled` true and `flat_radius_m` / `flat_blend_m` 0 added to the overrides unless the host set them, `cache_dir` empty, `seed_world` passed on every configure and reconfigure so the F3 panel keeps working; a library without 1.7 or a bad manifest gives a `ConfigureError` that the overlay shows), typed facade records `AirportInfo` / `RunwayInfo` / `RunwayEnd` / `SeedWorldInfo` / `SeedCityInfo`, `ReadyTile.AirportLights`, `DebugTile.Source`. Shaders: `seed_palette.gdshaderinc` (class colours, airport surfaces, tint ramp; `WorldRenderSettings.Seed*` write it to the ground and the terrain material), `ground.gdshader` (class 7 by sub-kind, tint on class 0 / 1 areas, roads told apart by r == 8/16 first; level 0 of the tint is the plain class colour so archive and synthetic-city areas are unchanged), `terrain.gdshader` (class from COLOR r * 8 as one-hot weights sharpened by a smoothstep, `r * 8 == 8` = no class = the altitude palette). Airport lights: fourth sprite set per tile with its own material (`TilePool.AirportLightMaterial`, `WorldRenderSettings.AirportLight*`): fixed colours, no window cross-fade, no grazing or altitude thinning (an approach at 300 m AGL looks along the lights at 3 degrees), alpha 0.125 in the buffer (the sprite shader reads anything above as a cluster and would jitter and enlarge them) with the colour scaled by alpha / 0.35. Runway markings (`AirportMarkings`, `airport_marking.gdshader`): edge lines, dashed centreline, threshold bars, aiming-point bars from the `airports()` runway ends of the airports within 4.5 km, rebuilt on origin shifts, dithered out between 2.2 and 3.2 km; no designators, no grass-runway or taxiway markings.
- **Bench:** `--world=`, `--seed-spot=` (+ `--seed-spot-alt/-pitch/-heading/-out`), `--seed-selftest`, `scenes/SeedTwoCities.tscn`, F1 `seed:` line (data source of the tile under the camera from `debug_tiles`, generator, phases, nearest airport, marking mesh, first warnings).
- **Tests:** `--seed-selftest` prints `SEED SELFTEST PASS` in about 50 s (30 fps cap, upload budget lifted for the run, about 1400 of the 3000 frames); two runs print identical `SEED HASHES` lines (vilnius 114ff6cdc35d1b25, kaunas e2bd3ea964863ff2, eyvi 21bc2ba1484eda73, eyka 20c540634ac62b2e, a1 6e963e7b87d64f6d, cruise 532cb8be88010c41, coast 5ef64b418b52b2cc, lake 1342fc04d16a9683; the hash covers keys, rings, quantized meshes and light buffers of every held tile of both layers once everything the library tracks is ready and held, not frame-dependent origins). Measured at the spots: EYVI 01/19 2507 m x 50 m, slope -0.17 %, `ground_height_at` 0.0 mm from the threshold line, 0.1 mm across; EYKA 08/26 3239 m x 45 m, slope 1.00 % (the cap), 0.2 / 0.2 mm; sea 10 points 0.0 mm and 823 offshore terrain vertices at sea level; Kauno marios level 61.48 m, spread 0.0 mm over 25 points; first tile under the camera 5-140 ms after the teleport, 250-285 R3 tiles all with ground; 400-460 vector and 360-400 terrain tiles held per spot. `--settings-selftest` also passes in the seed scene (12 reconfigures keep the seed world).
- **Spec 11a deviations:** the spots are placed with `update_camera_geodetic` in the first frame (hook), but the camera is first put at the spot's local position so the library's re-centre (up to 330 km from the scene origin) shifts it like any flight; the "R3 ground present" check is "every held R3 tile has a ground mesh once quiet" (a camera at cruise height has no R3 tile under it, the R3 band starts at 7 km slant); the coast check is limited to lon 19.8..20.95 because the water file's bbox ends at 19.5 E and the file says "land" outside it (the terrain west of it is relief, 664 m high in tile `t/10/565/319`); the airport-lights check does not depend on the clock (the key is always filled, the shader shows it at night).
- **What the screenshots show (`low-poly-map/shots/seed/`, git-ignored; 13:00 and 22:00):** the countryside tint reads as a field patchwork from cruise and from 2 km; sea and coast are flat and the coast is a clean line from 2 km; Kauno marios is flat; both airports show a dark runway, taxiway and apron, terminal / hangar boxes, the fence as a flat mown-grass rectangle and, from 500 m, the markings; at night the edge, threshold, end, approach and taxiway lights are visible from 3 nm (a bloomed blob with the runway outline) down to the runway. Colours: with the sprite intensity at 3.5 the colours burnt out to white under the filmic tone map; 2.2 keeps the blue taxiway and green threshold readable (`AirportLightIntensity`).
- **Found, for the Rust side (not fixed here):** (1) the drawn runway is not at `ground_height_at`: the vector ground is draped with the lift of its band plus level x step, so a runway lies 1.2 m (R0), 3.6 m (R1), 9.5 m (R2), 25 m (R3) above the height function with the default profile, although the strip is exactly flat (the TIN error there is 0) and the contract says the height function "equals the drawn runway (landing physics, AGL)"; a game rolling on `ground_height_at` sinks 1.2 m into the pavement in R0. The host markings and the airport lights (Rust adds the lift to them) are consistent with the DRAWN runway; a lift of 0 on ground inside strips and fences would remove the discrepancy. (2) Rivers drape on the relief and their ribbons are ragged where terrain pokes through (EYKA, the Nemunas): rivers need at least a terrain rule or a smaller lift (open decision of section 5.2a). (3) Terrain colour bleeds along long TIN triangles at the coast and lakes: thin land-coloured spikes into the sea and hair lines across Kauno marios (coast breaklines, section 5.5). (4) The 450 m default relief puts the levelled runways in trenches with steep embankments (EYKA), and the terrain beyond the water file's bbox (west of 19.5 E) is land.
- **Not done:** designators, grass-runway and taxiway markings, PAPI, S2 and later (cities, roads: the A1 and EYKA access road do not exist yet, so `a1` shows countryside), `SetCitySource`, a night look for countryside beyond the airports (the terrain is nearly black at night: city lights come with S2+).

## 11e. S1 fixes (Rust, 2026-10-28)

The two Rust defects of 11d, fixed in the core. `api_version` stays 1.7 (unreleased; the contract text of sections 2, 3.6, 8, 9, 9a is amended). `SEED_GEN_VERSION` is 2. Non-seed output is unchanged byte for byte (every flat, R3 and terrain golden is untouched). Seed goldens were regenerated: the four EYVI vector tiles and the three seed terrain tiles changed, the countryside, coast and lake vector tiles did not.

- **Runway height (11d finding 1):** no drape lift inside fences, lift weight `k` over the shoulder, fence cut-outs, airport levels 1 / 2 / 3, TIN error scaled by `k`, overlapping strips sharing one plane (sections 5.1 step 9; contract 9 "Airports"). Measured on all 37 runways of the two-cities world (20 centreline points, 45 across each strip, every band, `seed::drape_tests`): runway pavement − `ground_height_at` = R0 0.056-0.069 m, R1 0.176-0.189, R2 0.446-0.459, R3 1.50 m (0.50 where the runway area is under R3's minimum area and the fence shows). The terrain mesh under the EYVI / EYKA runways is within 6 mm of `ground_height_at` (T0..T2). Airport lights sit `height_m` above the drawn surface.
- **Coast colour (11d finding 3):** per-triangle class resolution (section 5.5). Every surface triangle of the coast and Kauno marios tiles (T0..T4) is all water or all land, with the same triangle count as before.
- **Cost (release, Apple arm64, min of 3):**

| Tile | Triangles before -> after | Build ms before -> after |
|---|---|---|
| Terrain coast z14 / z13 / z12 / z11 / z10 | 847 / 904 / 644 / 455 / 326 -> 847 / 904 / 644 / 455 / 391 (z10 holds EYPA) | 2.1 / 5.3 / 11.7 / 9.3 / 6.9 -> 2.1 / 5.9 / 12.9 / 9.3 / 8.7 |
| Terrain Kauno marios z14..z10 | 192 / 797 / 700 / 1047 / 1622 -> same, z10 1776 (holds EYKA) | 1.0 / 5.6 / 18.9 / 21.5 / 26.3 -> 0.9 / 5.5 / 18.6 / 21.1 / 26.2 |
| Terrain EYVI z14..z10 | 669 / 663 / 787 / 677 / 740 -> 1264 / 1949 / 1908 / 1129 / 906 | 1.6 / 5.0 / 18.2 / 17.7 / 19.8 -> 2.4 / 6.9 / 20.7 / 20.1 / 21.2 |
| Terrain EYKA z14..z10 | 678 / 1634 / 1401 / 1639 / 1622 -> 863 / 2359 / 2064 / 1859 / 1776 | 1.9 / 7.0 / 22.5 / 23.0 / 23.8 -> 2.3 / 8.8 / 25.8 / 25.5 / 26.1 |
| Vector EYVI R0..R3 (ground triangles) | 74 / 142 / 542 / 406 -> 129 / 212 / 945 / 909 | 0.12 / 0.30 / 0.83 / 0.74 -> 0.19 / 0.38 / 1.40 / 1.76 |

All terrain tiles stay far under `max_triangles` (4096 / 8192). The worker CPU budget of spec-terrain section 9 (≤ 20 ms for z ≤ 12) was already exceeded at EYKA z12 before the fix (22.5 ms) and is now 25.8 ms. The extra time and triangles come from the fence and the shoulder: the TIN refines along the fence edge, where the smoothstep shoulder bends.
- **Host (Godot, not done here):** `WorldView.ComputeRunwayLifts` and `AirportMarkings` must stop replaying `lift(band)` + 10 × step. The runway surface is `ground_height_at` + 3 × `ground.rN.lift_step_m` (0.06 / 0.18 / 0.45 / 1.5 m by default). `WorldRenderSettings.MarkingRunwayLevel` becomes 3, and the terrain bands / `detail_scale_max` are no longer read. Branch on `seed_world_info()["generator_version"]` (≥ 2 = new rule). The terrain shader needs no change; its class sharpening sees uniform triangles at the coast now.
- **Open:** in the fence grass between strips that do not overlap, the height still steps where the nearest strip changes (no runway is affected). Rivers are unchanged (11d finding 2). The trench / embankment look of levelled runways at 450 m relief is unchanged (11d finding 4).

**11e follow-up, Godot (2026-10-28):** `WorldView.ComputeRunwayLifts` branches on `seed_world_info()["generator_version"]`. Generator >= 2: the drawn runway is `ground_height_at` + `WorldRenderSettings.MarkingRunwayLevel` (now 3) x `ground.rN.lift_step_m` per ring, read from `world_profile()` (0.06 / 0.18 / 0.45 / 1.5 m); `terrain.bands` and `detail_scale_max` are no longer read and the rule does not depend on the general drape lift (airports stay at level x step whatever that lift becomes). Generator 1 or no key: the old replay is kept (level 10 internally, so the setting does not break an old library). `airport_marking.gdshader` / `AirportMarkings` comments updated; `WorldView.RunwayLift` / `RunwayLiftRule` and `AirportMarkings.RingLift` / `Vertices` are exposed for tests. `--seed-selftest` now checks, at EYVI and EYKA, the host lift per ring against 3 x `lift_step_m` within 1 mm and that the marking mesh sits on `ground_height_at` (1 cm) so mesh y + R0 lift is 0.06 +- 0.015 m above it; it passes and prints the same checks as before otherwise (the `SEED HASHES` changed with the Rust fix, not with this host change). Screenshots at 2.5 to 60 m, day and night, on the threshold and in R1 / R2 range: markings on the pavement, no burying, no floating, no flicker; airport lights on the runway.

## 11f. S2-S4 results (2026-10-29)

Built in `rust/simworld-core/src/seed/city/` (`hex`, `place`, `arterials`, `radials`, `district`, `cache`, `tile`, `lines`) and `seed/airport/approach.rs`; profile keys `seed.city`, `seed.styles.<style>`, `seed.airport.approach` (additive, profile_version 7 unchanged); contract **api 1.12**, `SEED_GEN_VERSION` 3, phases `S2`, `S3`, `S4`; `simworld-cli seed preview`. Tests: `seed::city_tests` (plans, request counts, network connectivity after the drop, merged-district cap, shared edge curves, nothing in fences, approach caps, kinds -> light profiles and area classes, one owner per building per zoom, deterministic tiles, the full mesh / light pipeline in every band, terrain colour), the CLI preview test; `--seed-selftest` passes unchanged (S1 group; new `SEED HASHES`).

- **Measured** (two-cities world; dev profile for plans, release for district plans): Vilnius plan 7 ms, 367 land / 7 airport cells, 215 districts (old town 2, business 29, industrial 44, residential 270, park 22 of 29 asked: a warning), 801 arterial edges in 12 components (same before and after the drop), 13 bridges, 5 radials, access to EYVI; Kaunas plan 4 ms, 209 land cells, 119 districts, 446 edges, 8 bridges, 4 radials, access to EYKA. District plans: 576, build p50 1.5 ms / p95 3.1 / max 6.7 ms (release), up to 1044 buildings, 18 KB p50 / 133 KB max. Cold tiles at the Vilnius centre (plans included): z16 7 ms, z15 5 ms, z14 8.5 ms, z13 28 ms (3538 owned buildings for lights).
- **Deviations from sections 5.3 and 6:** (1) a district is a group of merged cells, but each cell keeps its own ground area and district plan; the street lattice is a function of the district, so merged cells still share one network; (2) radials do not cut district polygons: buildings keep `seed.city.clearance_m` from every arterial and radial instead; (3) every style is a warped lattice: old town = strong jitter and dropped segments (not Voronoi), port = a grid of warehouses (no quay road), park = curved footpaths; (4) R3 carries the owned buildings themselves (lights only, no mesh at R3) instead of a lot-only stage (a), so R3 and R2 lights are the same points by construction; (5) radials never turn more than 60 degrees from their start bearing (steering around rivers made U-turns), cross water at most once and otherwise end at the shore; the access road is a steered walk to the fence gate (no A*); (6) the countryside stays under the city at stacking level 0 instead of being cut out; (7) approach caps use the height function at the threshold as the surface base.
- **Screenshots** (bench `--shot`, `SeedTwoCities.tscn`): Vilnius from 400 m by day, 1500 m at night, 120 m at dusk; Kaunas from 400 m. Tuned after the first night shot: the business style's `height_exp` 2.5 -> 6 and `floors` max 40 -> 35, because a quarter of all business buildings were 60 m or more and carried aircraft warning beacons.
- **Not built:** inter-city roads and road-claimed radials (S2b), landmarks and `landmarks()` (S4a), edits and `debug_building_at` (S4b), terrain-aware placement (slope -> park, contour streets), the Voronoi cell experiment, cutting radials through districts, the Godot `SeedSelfTest` S2-S4 group and its F1 seed line counters.

## 12. Tests

- Hash test vector (fixed inputs → fixed u64s), on every CI target.
- Golden hashes: a fixture world (one `old_center_hex` city with every district type, one `grid` city, one airport with two crossing runways and one with an approximate runway) → city plan hash, district plan hashes, per-tile output hashes for all bands; run on x86_64 and aarch64 (P6 CI).
- Locality: editing city A's file (each key in turn) leaves every tile outside A's footprint bit-identical; adding a city leaves the others identical; reordering the manifest changes nothing.
- Airport rule: for every fixture airport, no generated building or street intersects any strip; every building inside an approach trapezoid is at or below the surface.
- Seams: every hex edge curve is identical from both cells; every street graph is connected to its boundary; no building is owned by two tiles at one zoom; the same building has an identical footprint (union of its clipped parts) in z13..z16.
- Water: no arterial or access-road vertex lies in water except on designated bridges; tiles beyond the water bbox are sea when `outside` = `"sea"`.
- Terrain with a seed world (goldens): seed tiles and terrain tiles with the terrain on, for the fixture world, both CPU architectures.
- Runways flat under terrain: along each fixture runway the height is linear between the threshold heights within 1 cm, constant across the strip, and `ground_height_at` equals the drawn runway.
- Sea and lakes: every terrain vertex inside the sea is at `sea_level_m` exactly, inside a lake at its level; no land vertex within `blend_m` of water is below that water; the terrain COLOR class agrees with the R3 ground's water at the vector reach.
- Terrain locality: changing a city file never changes a terrain height (only airports and water modify the function).
- Multi-source: two URL sources filling past `cache.cap_mb` keep the total under the cap with global LRU; one source offline and one ok gives `degraded` with the city named; a world with only `path` sources takes no lock.
- Plan eviction: a district plan evicted from the cache while a job still holds its `Arc` finishes that job with the same output; the next job rebuilds an identical plan.
- Arterials: the arterial graph of every fixture city is connected after the drop; no merged district polygon exceeds `max_district_m2`.
- Kinds: every kind the generator writes resolves through the default profile to its intended light profile and ground class (no `hotel` in office districts, landmark stand-ins on the `landmark` profile).
- Real airports: in a real city fixture no real road enters a runway strip and no real area remains inside a fence.
- Old host: class 7 renders (unknown-class colour), `g` tints and terrain COLOR are ignored without errors on the 1.x addon; the new addon draws them.
- Real switch: a late tile of the old source after `set_city_source` is dropped (generation counter).
- Performance: `cargo bench` per plan level (city plan, district plan, tile assembly) against section 10a, as acceptance for S2–S4.
- Placement: request counts met when candidates suffice; `near` satisfied; contiguity (each request's cells form at most `ceil(count / 8)` components).
- Real routing: a city switched to real and back yields the original synthetic hashes; tiles outside its coverage are never re-delivered.
- Validation: each error and warning of section 4.6 has a failing fixture.
- Roads: the spline passes through every author point; a road's tiles are identical whether or not its cities are loaded; motorways have no wobble; a road entering a city turns the nearest radial to its bearing and the radial's last vertex equals the road's clip point on the footprint union (exactly, after quantization); a crossing longer than `max_bridge_m` fails validation.
- Landmarks: no generated building intersects a landmark footprint + `clear_m`; the stand-in exists exactly in bands ≥ `stand_in_min_ring`; `landmarks()` positions follow an origin shift; adding a landmark changes only the overlapped districts' hashes.
- Edits: pristine output with no edits file equals the golden hashes; removing the edits file restores them exactly; export → import of an unchanged CSV writes an empty diff; a height / kind / delete / cell / add edit changes only tiles of its district; changing one district's style re-matches the edits of a building that moved < 10 m and orphans one that vanished, leaving other districts' edits applied by id; an `add` over a runway strip fails validation.

## 13. Risks and open decisions

| Item | Risk / question | Plan |
|---|---|---|
| OurAirports runway accuracy | Ends missing or off by tens of metres for small fields | Measured in S0 for the five Baltic airports: 4 of 5 within 6.1 m, EYPA 180 m off (override written). Confirmed once in five, so the override mechanism stays (section 11b) |
| Hex look | Districts read as a honeycomb from altitude; every junction a Y | Confirmed in S2-S4 (11f). **Resolved by design** in `docs/spec-seed-city-pipeline.md`: no hex cells, major roads grow by morphology rules, zones follow the superblocks |
| `geo` and platform maths | Positions differ in the last bits between ARM and x86 | Decisions are platform-independent already (section 7); **Done in S0:** `geo` and all core transcendentals go through `math.rs` (`libm`); goldens unchanged on macOS arm64, x86_64 check waits for CI (P6) |
| Generation cost | City plan or district plans too slow for the frame-paced worker pool | Budgets in 5.3/6.1, measured in S2–S4 with `cargo bench` |
| Real/synthetic border | A real city's z13 coverage is square-ish; the synthetic countryside next to it has no streets | Accept for v1; later, real cities fade their outer ring into the synthetic footprint |
| Inter-city roads | Author points too sparse: a spline through few points cuts corners the real road does not | Draft from OSM at 300 m tolerance (section 3.2); the author thins, never invents |
| Landmark models | Draw calls and memory per model; a heavy model costs more than a whole district | Budget per landmark (≤ 20 k triangles, ≤ 4 materials, ≤ 2 MB textures), checked by the author; few per city |
| Class 7 vs class 6 | A new ground class needs a host shader branch; an old host shows runways in the unknown-class colour | Ship the branch with S1; `seed.aeroway_class: 6` fallback (section 6.6) |
| Water file size | 50 m tolerance over the whole Baltic coast and lakes may exceed a few MB | Measure in S0; raise tolerance or min area |
| Terrain-water rule | Raster memory over many lakes; a coast that looks terraced at 175 m raster spacing; cost per height sample | Prototyped and measured in S0 (sections 5.2a, 11b): cost 1.47× at 64², 1.27× at 32²; S1 with strips, water first and a raster fast path: 1.44× (section 11c); 32 × 32 raster on iOS |
| Terrain colour on a TIN | Per-vertex class colour bleeds across long triangles at the coast | Coast breaklines in the TIN if S1 screenshots show it (section 5.5) |
| Real/synthetic airport mismatch | OSM runway and OurAirports runway a few metres apart in a real city | Real roads and areas inside the airport are dropped (section 8) |

## 14. Sources

- OurAirports data dictionary and downloads: https://ourairports.com/data/ (public domain).
- ICAO Annex 14 Vol. I, obstacle limitation surfaces (approach surface: inner edge at 60 m from the threshold, 15 % divergence, 2 % first section, 3000 m), simplified here.
- OSM land/water polygons: https://osmdata.openstreetmap.de/ (ODbL).
- City generation pipeline sources (Shariatpour et al. 2026, Parish and Müller 2001, Vanegas et al. 2012, CGA): `docs/spec-seed-city-pipeline.md` section 16.
- Chen et al. 2008, "Interactive Procedural Street Modeling" (tensor-field streets; a later alternative to the warped grids of section 6.2).
