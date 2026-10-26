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
| `landuse` | polygon | `id`, `kind` | `kind` = the most specific of `landuse`, `leisure` (park, garden, playground, pitch, golf_course, nature_reserve, recreation_ground), `natural` (water, wood, grassland, scrub, wetland) or `riverbank` (`waterway=riverbank`). Rust uses these polygons for the kind and levels fallbacks (point-in-polygon) and, since Milestone 5, for the flat ground mesh (parks, forest, water). A schema can't do spatial joins. |
| `roads` | line | `id`, `kind`, `lit` | `kind` = `highway`; `lit` as is. Roads from motorway down to tertiary (plus their `_link` roads) start at z13; all other roads start at z14. |

The MVT feature id (not the `id` attribute) is planetiler's encoding, `osm_id * 10 + type` (2 = way, 3 = relation).
Buildings that cross a tile edge appear in each tile they touch, with the same `id`. The mesher should give each one to a single tile (for example the tile that holds its centroid) when it places light points.
`building:part` is not exported.

## Output (Lithuania extract of 2026-10-26)

`out/vilnius.pmtiles`: **59 MB**, 30,607 addressed tiles.

| Zoom | Tiles | Total (gz) | Avg (gz) | Max (gz) |
|------|-------|------------|----------|----------|
| z13 | 380 | 8.5 MB | 22 kB | 168 kB |
| z14 | 1,518 | 13.5 MB | 9 kB | 104 kB |
| z15 | 5,903 | 16.6 MB | 2.8 kB | 34 kB |
| z16 | 22,806 | 22.8 MB | 1.0 kB | 13 kB |

Keeping every building at z13 costs size: the densest z13 tile (old town, 13/4671/2603) holds 4,452 buildings and is 290 kB raw / 168 kB gzipped.
To cut z13, emit a centroid point per building there instead of the polygon (`geometry: polygon_centroid`), because R3 only needs one light point per building.

## Hosting

`serve.sh` is only for local development. We haven't chosen the production host yet (any static host or CDN with range-request support, such as S3 + CloudFront or Cloudflare R2). Never use public OSM tile servers.
