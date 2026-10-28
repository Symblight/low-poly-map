# SimPlane tile data (Milestone 2)

Offline build of the custom vector tiles the Rust `WorldStreamer` reads with HTTP range requests.
Data © OpenStreetMap contributors (ODbL); the attribution is stored in the PMTiles metadata and must be shown in the app.

## Scripts

| File | What it does |
|------|--------------|
| `build_tiles.sh` | Downloads the Geofabrik Lithuania extract into `sources/` (skipped if present), then runs planetiler `0.10.2` in Docker (`generate-custom`, no Java needed) with `schema/simplane.yml`, bounds `24.85,54.45,25.70,54.92` (central Vilnius + ~15-25 km), z13-z16. Output: `out/vilnius.pmtiles`. Takes ~25 s. Override with `BOUNDS=`, `OUT_NAME=`, `JAVA_HEAP=`, `PLANETILER_IMAGE=`. |
| `serve.sh` | Serves `out/` at `http://127.0.0.1:8787/` with `npx http-server@14.1.1`, which answers `Range` requests with `206 Partial Content` (python's `http.server` doesn't). Check: `curl -r 0-126 -i http://127.0.0.1:8787/vilnius.pmtiles`. |
| `schema/simplane.yml` | planetiler custom YAML schema. |

## Layers

All layers exist at z13-z16. Tiles are gzip-compressed MVT, extent 4096, 4 px buffer.
Nothing is merged and small features are not dropped (`min_size: 0`), so every building is in every zoom.

| Layer | Geometry | Attribute | Source / notes |
|-------|----------|-----------|----------------|
| `buildings` | polygon | `id` | OSM way or relation id (number). Ways and relations can share an id, so use it with `osm_type`. |
| | | `osm_type` | `way` or `relation` |
| | | `kind` | raw `building` tag (`yes`, `apartments`, `office`, ...); `building=no` is skipped |
| | | `height` | `height` parsed to metres (double); omitted if missing or not numeric |
| | | `levels` | `building:levels` parsed (double); omitted if missing |
| | | `min_height` | `min_height` parsed, default `0` |
| | | `colour` | `building:colour` as is; omitted if missing |
| | | `historic` | raw `historic` value (`castle`, `ruins`, `city_gate`, ...); omitted if missing |
| | | `man_made` | raw `man_made` value (`tower`, `chimney`, `water_tower`, ...); omitted if missing |
| | | `windows` | bool from `windows=yes/no`, else `window=yes/no`; other values (`many`, ...) and missing tags: omitted |
| | | `ruins`, `disused` | `true` when `ruins=yes` / `disused=yes`; omitted otherwise |
| | | `abandoned` | `true` when any `abandoned:building=*` is present; omitted otherwise |
| `landuse` | polygon | `id`, `kind` | `kind` = the most specific of `landuse`, `leisure` (park, garden, playground, pitch, golf_course, nature_reserve, recreation_ground), `natural` (water, wood, grassland, scrub, wetland) or `riverbank` (`waterway=riverbank`). Rust uses these polygons for the kind and levels fallbacks (point-in-polygon) and, since Milestone 5, for the flat ground mesh (parks, forest, water). A schema can't do spatial joins. |
| `roads` | line | `id`, `kind`, `lit` | `kind` = `highway`; `lit` as is. Roads from motorway down to tertiary (plus their `_link` roads) start at z13; all other roads start at z14. |

The six optional `buildings` attributes (night windows, `docs/spec-night-windows.md`) feed the Rust light-profile resolution (`landmark` / `dark` profiles, `buildings.dark_states` in the world profile); the tables stay in the world profile, so tuning them needs no tile rebuild. Archives built before them still work (every attribute counts as absent). In the Vilnius extract (z16, owned buildings) they are rare: 82 `historic`, 74 `ruins`, 5 `disused`, 2 `abandoned`, ~250 `man_made` (mostly `storage_tank`), no `windows`/`window`.

The MVT feature id (not the `id` attribute) is planetiler's encoding, `osm_id * 10 + type` (2 = way, 3 = relation).
Buildings that cross a tile edge appear in each tile they touch, with the same `id`. The mesher should give each one to a single tile (for example the tile that holds its centroid) when it places light points.
`building:part` is not exported.

## Output (Lithuania extract of 2026-10-26)

`out/vilnius.pmtiles`: **59 MB**, 30,607 addressed tiles. (Rebuilt 2026-10-27 with the night-window attributes: 63.9 MB / 61 MiB on disk, 23 kB more than the build before it; the table below is from the first build.)

| Zoom | Tiles | Total (gz) | Avg (gz) | Max (gz) |
|------|-------|------------|----------|----------|
| z13 | 380 | 8.5 MB | 22 kB | 168 kB |
| z14 | 1,518 | 13.5 MB | 9 kB | 104 kB |
| z15 | 5,903 | 16.6 MB | 2.8 kB | 34 kB |
| z16 | 22,806 | 22.8 MB | 1.0 kB | 13 kB |

Keeping every building at z13 costs size: the densest z13 tile (old town, 13/4671/2603) holds 4,452 buildings and is 290 kB raw / 168 kB gzipped.
To cut z13, emit a centroid point per building there instead of the polygon (`geometry: polygon_centroid`), because R3 only needs one light point per building.

## Two-level archive (Tiles and LOD v2, step T1b)

`simworld-cli bake` re-tiles `out/vilnius.pmtiles` into the two data levels of the Tiles and LOD v2 spec, in one PMTiles v3 file (gzip MVT). No OSM / Docker step: it reads the z16 footprints (stitched back into whole buildings across the z16 tile edges), the z14 landuse / roads and the z13 major roads / landuse of the existing archive. From `rust/`:

```
cargo run --release -p simworld-cli -- bake ../data/out/vilnius.pmtiles ../data/out/vilnius-v2.pmtiles
cargo run --release -p simworld-cli -- bake --verify ../data/out/vilnius-v2.pmtiles [--input ../data/out/vilnius.pmtiles]
```

Options: `--block-min-area <m²>` (default 150), `--block-gap <m>` (4), `--landmark-m <m>` (60). The bake takes ~2 s, prints the sizes below, then verifies its output against the source (exit 1 on a failure); `--verify` runs the same checks on any archive (with `--input`, also the per-building ones). Output is deterministic (same bytes on every run).

| Level | Zoom | Layer (extent) | Content |
|-------|------|----------------|---------|
| detail | z14 | `buildings` (8192) | every non-landmark building, simplified 0.5 m, >= 20 m² after simplification, all source attributes (`id`, `osm_type`, `kind`, `height`, `levels`, `min_height`, `colour`, `historic`, `man_made`, `windows`, `ruins`, `disused`, `abandoned`). No `building:part` (the source has none). Buffer 4/256. |
| | | `landuse`, `roads` (8192) | the source's z14 layers as they are (all road classes) |
| overview | z11 | `blocks` (4096) | non-landmark buildings within 4 m merged (buffer 2 m, union, shrink 2 m), simplified 5 m, >= 150 m²; `id` (smallest member OSM id), `kind` (by area), `height` (area-weighted mean of the resolved roof heights, rounded to 5 m, min 5), `count` |
| | | `landmarks` (16384) | buildings whose tagged roof (height, else levels x 3 m) is >= 60 m: full footprint (0.1 m simplification), all source attributes; they are not in detail |
| | | `lights` (4096) | one point per building (landmarks too) at its footprint centroid, in exactly one tile. OSM id / type only in the feature id (`id * 10 + 2` way, `+ 3` relation); `kind`, the state tags; `height` (rounded to 5 m) and `area` (m²) only for tagged heights; `landuse` = the fallback landuse kind at the centroid |
| | | `roads` (4096) | motorway, trunk, primary, secondary (+ links) from z13, cut at the z13 edges, simplified 3 m |
| | | `landuse` (4096) | z13 polygons, pieces of one id unioned; water >= 2000 m², the rest >= 1 ha; simplified 5 m |

Every building is in exactly one of detail `buildings` / `landmarks`, or in neither when its footprint is under 20 m² (it keeps its light point); blocks and lights are derived. Metadata: `simworld_levels` = `{"detail":14,"overview":11}`, `attribution` (© OpenStreetMap contributors), `simworld_bake` (all parameters, read back by `--verify`), `vector_layers`.

Checks (`--verify`): only zooms 11 and 14; layer names and extents per level; metadata keys; no id in both detail and landmarks; landmarks >= 60 m and detail < 60 m; detail footprints >= 20 m² and blocks >= the minimum (area summed over tiles inside each tile proper, minus the grid rounding tolerance); block and light heights in 5 m steps; one light point per building, inside its tile; with `--input`: every source building in exactly one level (or dropped by the minimum area) with exactly one light, no unknown ids. `cargo test -p simworld-cli` runs the bake end to end on a synthetic source (and the runtime's `mvt::decode_tile` on a detail tile); `SIMWORLD_BAKE_INPUT=<path> cargo test --release -p simworld-cli vilnius -- --ignored` runs it on the real archive and checks the stitching.

Result for `vilnius.pmtiles` (247,629 buildings, 2026-10-28):

| Level | Tiles | Total (gz) | p50 | p95 | Max |
|-------|-------|------------|-----|-----|-----|
| detail z14 | 1,518 | 14.74 MB | 5.1 kB | 37.7 kB | 115 kB (14/9342/5207) |
| overview z11 | 36 | 4.28 MB | 53 kB | 459 kB | 757 kB (11/1167/650, central Vilnius) |

Archive 19.0 MB (29.8 % of the 63.9 MB source). 232,471 detail footprints, 18 landmarks, 15,140 under 20 m² (lights only). Blocks: 72,877 from 196,804 clusters; 45.2 % of the buildings (77.9 % of the footprint area) end in a kept block (min area 200 m²: 33.1 % / 67.8 %, 100 m²: 61.7 % / 88.6 %). The `lights` layer is 42.6 % of the overview gz bytes (1.82 MB).

## Hosting

`serve.sh` is only for local development. We haven't chosen the production host yet (any static host or CDN with range-request support, such as S3 + CloudFront or Cloudflare R2). Never use public OSM tile servers.
