# Decision note: direction of the SimWorld library

2026-10-29 · Aleksei (critique by Claude on request; decisions are the author's)

## Why this note exists

The author asked for an honest review of the architecture and the idea: a Rust library that streams and generates a low-poly real-world city at runtime, cheap in CPU and memory, for the SimPlane flight game in Godot. The worry was that the library had become a second game engine and that the work was being done twice. This note records what the review found, the decisions taken from it, and the reasons, so that the same questions are not reopened without new facts.

## State on 2026-10-29

| Part | Lines |
| --- | --- |
| Rust core, binding, CLI (`rust/`) | 47 787 |
| Addon C# runtime (`addons/simworld/runtime/`) | 8 483 |
| Bench host C# (`low-poly-map/scripts/`) | 9 990 |
| SimPlane `World/` (flat plane, box runway, 20 000 cone trees) | 1 308 |

- Contract `api_version` 1.11, eleven additive revisions, 317 Rust tests, golden determinism hashes.
- SimPlane (`../Simplane/simplane`) does not reference SimWorld anywhere: no addon, no import, no doc mention. Its world is `World/TestWorld.cs`, flat ground at y = 0.
- The iOS export of a project that uses the addon has never been run.
- Three world sources exist: real OSM PMTiles, the synthetic test city, and the seed world from JSON (`docs/spec-world-seed.md`). Terrain is procedural, no DEM.
- A road-traffic spec (`docs/spec-traffic.md`) was proposed on 2026-10-28, before the game had ever shown the city.

## Finding 1: this is a scenery engine, not a second game engine

Every flight simulator has a scenery subsystem separate from its rendering engine: X-Plane's scenery system, MSFS's Bing plus Blackshark streaming, Cesium for Unreal and Unity. Godot ships none of it. Tile fetching, MVT decoding, the quadtree LOD cover with the sibling-swap rule, meshing, light extraction, procedural terrain and the floating origin duplicate nothing in Godot. The overlap with engine features (visibility ranges, mesh LOD) is small and does not cover streaming.

What is right and stays: a pure-Rust core with no engine dependency, headless tests with a Godot simulator, golden hashes, tuning as data, screen-space-error LOD, workers that produce GPU-ready buffers, pooled nodes and no per-tile objects. This is standard streaming practice in its cheapest Godot-Mobile form.

## Finding 2: the library has never met its consumer

The feeling of double work comes from process, not from architecture. The library grew eleven contract revisions, validated only against its own bench, while the game it exists for still flies over a flat plane. For a first flight over a city the game needs tiles, buildings, ground, a flat or rough terrain and a day/night switch. Beacons, street lamp pools, the landmark layer, two-level archives, pool trimming, the GC pacer, engine surfaces and traffic are polish on something no player has seen.

The integration work that would have exposed real needs has not started: SimPlane's own `WorldGeoReference`, JSBSim's WGS84 earth, SunshineClouds2, the windsock and the ND all have to receive the origin shift in the same frame. The contract supports it; nothing is wired.

## Finding 3: the premise is unproven on the target

The whole justification is low cost on weak devices. Nothing has been measured on one. C# on iOS in Godot is the least certain part of the stack, and 8 483 lines of host glue sit in that language. If it fails, the Rust core survives and the addon does not.

## Finding 4: three sources, two products

"Curated ETS-style cities" and "MSFS-style real world" are different products. The constraints (free data, no hosting bill, iOS) allow the first. Real OSM PMTiles is feasible for chosen cities (Vilnius is 19 MB as a two-level archive), as an author-time option per city, not as a parallel runtime product kept at feature parity forever.

Procedural terrain has a geographic boundary. It is fine for the Baltics. A curated city in Norway or near the Alps would contradict "airports at real positions" visibly. Until a DEM path exists, the world stays in flat country.

## Finding 5: a server with an API is the wrong model here

The author also considered the architecture as a server holding the data and providing an API, with the library streaming and generating on the client.

- **The cheapest backend already exists and is not a server.** PMTiles over HTTP range requests is serverless by design: one immutable file on any object store or CDN, no process, no API to version (`docs/spike-streamed-osm-city.md`, hosting). A dynamic service adds deployment, monitoring, cost and a new failure mode: server down, no world.
- **The data is tiny.** MSFS streams because its source is petabytes of imagery that cannot live on a client.

  | Data | Size |
  | --- | --- |
  | Seed world, two cities, airports, coast, lakes, rivers | 1.5 MB |
  | Vilnius real OSM, two-level archive | 19 MB |
  | Vilnius real OSM, detail only | 64 MB |

  A whole curated Baltic seed world fits in the app bundle. A real-OSM city is one download, ETS map-expansion style.
- **Server-side generation would undo the library.** Streaming meshes instead of recipes multiplies bandwidth by ten to fifty. The point of a client-side generator is turning kilobytes into a city on the device.
- **A second contract.** The one contract that exists has drifted ahead of its only consumer. A network API is a second one with the harder versioning problem: old clients in the field, data changing underneath them, determinism no longer a client property. The golden hashes depend on immutable data per version.
- **Offline.** Flight games are played on planes and trains. Static archive plus the SQLite disk cache works offline after one download. A live API does not.
- **Where a server is justified**, none of it is world generation: multiplayer, shared live traffic, user content, telemetry, accounts. Live weather is an hourly static JSON on the same CDN. Updating cities without a new build is a manifest file of versioned archives with hashes, plus a download manager in the game.

## Finding 6: against X-Plane, MSFS and Cesium (added 2026-10-29)

| | X-Plane scenery | MSFS + Blackshark | Cesium 3D Tiles | SimWorld |
| --- | --- | --- | --- | --- |
| Data | Pre-baked 1° DSF: terrain mesh, footprints, road networks, placements | Imagery, DEM, vector roads, ML footprints with height / roof / style | Pre-built glTF meshes with a geometric error per tile | Vector MVT in PMTiles, or a seed recipe |
| Geometry made | Offline by tools, assembled at load | On the device from footprint + attributes + regional style library | Offline, streamed as meshes | On the device from footprint + tags |
| Terrain | Real DEM, landclass textures | Real DEM, aerial imagery | Real DEM, imagery draped | Procedural, class colours |
| LOD | Per-asset ranges, DSF grid | Quadtree, morphing | SSE traversal, refine when loaded | SSE quadtree, sibling swap, dither fades |
| Streaming | Local files, 60 GB | CDN + rolling cache | HTTP, SSE priority, byte budget | HTTP range + SQLite cache, memory budget |

SimWorld's closest relative is MSFS (compact per-building data turned into geometry on the device); its seed world is X-Plane's model (authored data assembled by rules). The shape matches the industry; the breadth does not have to. Two things all three have and SimWorld lacks: real terrain data (deferred, decision 3) and **a stable data format as the long-lived contract** (X-Plane's DSF is unchanged for fifteen years). SimWorld's data formats (profile JSON, seed JSON, the PMTiles schema) will outlive any host API and should be treated as the contract that matters.

Worth taking: X-Plane landclass textures for the ground (tiling textures by class, host only); MSFS regional style palettes in the city files (data only); X-Plane autogen blocks and roadside autogen for seeded towns; the authoring tool as a separate program (`simworld-cli seed` is its start). Worth questioning: two sources of terrain truth (height function and TIN, with the lift stack papering over the gap); the dither fade subsystem (the others pop or morph; first to drop if the measurement needs headroom); three runtime data sources (the synthetic city becomes a test fixture only).

## Finding 7: the land between cities (added 2026-10-29)

The question: how to make rivers, lakes, roads and small towns between the cities cheap, without a full-country detail archive. The author's proposal: a small amount of real data plus seeding, three cities with limited real data, everything else generated as Vilnius is today.

The proposal is right, and it is X-Plane's rural model exactly (real roads and water, autogen settlements, landclass ground). The seed world already has most of the skeleton: real water for all of Lithuania is 1.5 MB (`world/two-cities/water.json`), real airports 24 KB, `seed roads` already drafts roads from OSM. What is missing is not a mechanism but data breadth and one small generator:

- **A thin country layer, real, in the seed files.** Roads of class tertiary and above for the whole country (estimate: 20 to 25 thousand km, a few MB quantized; today `roads.json` is four hand-drawn points), settlement points with population from OSM `place` nodes (a few thousand, about 100 KB), and a land-cover mask (forest / field / built from ESA WorldCover or Corine at 100 to 300 m, 1 to 2 MB) that replaces the value-noise countryside so forests and fields fall where they really are. From 1 000 m up, the road network, the rivers and the forest pattern are what makes land recognisable; this layer buys all three. Railways optional, later.
- **Roadside autogen for towns and villages** instead of the hex-city plan: for a settlement point, lots along both sides of the real roads within a radius from the population, houses on the lots, one or two seeded side streets for towns above a few thousand people. Lithuanian settlements are ribbon villages along roads, so this is also the truthful model. It covers the roughly one hundred towns and thousands of villages with one simple generator and defers the full S2 city plan until a city without real data is wanted.
- **Three real cities, clipped.** Vilnius, Kaunas, Klaipėda as two-level archives cut to a radius (19 MB each today), the generator underlay beneath them as built in S1.
- **Size for the whole country:** about 60 MB of city archives, under 15 MB of country layer, 1.5 MB of water. A full-country detail archive would be one to two GB. The proposal is the cheap path by a factor of twenty.

Where the country layer lives: in the seed files (JSON or a compact binary), loaded once and indexed, not as a tile source. It is small, the generator needs it in memory for routing and placement, and it keeps the seed world offline and deterministic.

**The sea line.** The author needs the real coast between sea and land. It is already real: `water.json` carries the OSM coastline, lakes and large rivers for the region, the ground classes and the terrain COLOR follow it in every band, and the height function holds the sea flat at `terrain.sea_level_m` with the land sloping down to it over `seed.water.blend_m` (spec-world-seed 5.2a, built in S1). What remains is quality, not data: the coast is simplified to about 50 m, which is right from altitude and blocky on a low pass over Palanga or the Curonian Spit, so the CLI tolerance should be per band (about 10 m for the R0 / R1 index, 50 m for the rest, still well under a megabyte more); a **shore band** (a sand / beach class a few tens of metres wide along the sea edge, a ground class and a terrain colour, data only in the profile) is what makes the line read as a coast rather than a colour change; and the far terrain bands must not let the TIN error move the shoreline, which the water rule and the sink already bound. No new data source, no contract change beyond a class.

## Decisions

1. **No dynamic backend.** The world is immutable, versioned files on static hosting (or in the bundle), fetched by range request, cached on disk, generated on device. A server appears only for a feature that needs shared live state, and never for world data.
2. **The shipping product is curated seed cities** (`docs/spec-world-seed.md`), airports at real positions, Baltics first. Real OSM PMTiles is an optional per-city source chosen by the author. The synthetic city is test data only and is not extended.
3. **Flat country until a DEM path exists.** No curated city in mountainous terrain before then; the seed spec says so.
4. **Feature freeze on the library** until SimPlane flies over the seed world. No new `api_version` minors, the traffic spec stays shelved. Bug fixes and the integration's own needs are the only changes.
5. **Integrate first.** `WorldView` goes into SimPlane over `low-poly-map/world/two-cities/`, with an aircraft at a real Lithuanian airport, accepting ugliness. The game's needs drive the next changes, not the bench's.
6. **Measure on the weakest available device** with the Mobile renderer before any further optimisation work. The pool trimmer and GC pacer were tuned on a desktop.
7. **Keep the C# host layer, for now.** Only if iOS C# proves fragile does the mesh upload and pooling move into the gdext binding through `RenderingServer`, leaving C# with materials and settings. Not started.
8. **The data formats are the long-lived contract.** Profile JSON, seed files and the PMTiles schema get the versioning care; the host API stays small and changes only for an integration need (Finding 6).
9. **Ground textures: yes, now, host only.** Tiling textures by class on R0 / R1 through a `Texture2DArray`, world-XZ UV, fading to the flat class colour before R2, ASTC on iOS. No contract change, so it is allowed under the freeze and can run next to the integration. Facade textures are judged after the ground.
10. **Traffic: after the first SimPlane measurement, reduced.** `docs/spec-traffic.md` stays shelved. When built: night lamp dots on R0 / R1 first with seam-exact continuation from the start; the box bodies are a separate later decision. The mechanism of the spec (a pure function of time on the GPU) stands.
11. **Between cities: a thin real country layer plus seeding** (Finding 7). Real roads tertiary and above, settlement points with population, a land-cover mask, real water as today; roadside autogen for towns and villages; three real cities clipped to a radius. The hex-city plan (S2) is deferred until a city without real data is wanted. Target size for all of Lithuania under 80 MB.
12. **The coast stays real data** (`water.json`); the work is a per-band simplification tolerance and a shore class, not a new source.

## What would reopen these decisions

- A measured weak-device result that the client-side generator cannot meet the frame or memory budget: reconsider pre-baked meshes per city (still static files, still no server).
- A feature with shared live state (multiplayer): a server for that state only.
- A DEM source with an acceptable licence and size: lifts decision 3.
- A second host besides SimPlane: justifies the contract ceremony at its current weight.
