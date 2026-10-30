# Plan: seed generator 4, by subagent

2026-10-30. Implementation plan for `docs/spec-seed-city-pipeline.md` (phases C0-C8), split into briefs for the subagents of `CLAUDE.md` "Subagents and token budget". Plan only: no code has changed. Every brief below names the files, the spec / contract sections and the acceptance tests, so an agent reads nothing else.

## 1. Specs to keep in mind (verified 2026-10-30)

| Document | Role | Sections that apply |
|---|---|---|
| `docs/spec-seed-city-pipeline.md` | **Primary.** Generator 4 design | All; phases 13, tests 14, budgets 8 |
| `docs/perf-seed.md` | **Primary.** Budgets, cheap-path rules, `seed perf` report line, critic checklist | All (short) |
| `docs/spec-world-seed.md` | Still valid outside the superseded parts (5.3 and 6.1-6.4 = generator 3 only) | 4 files (4.2 amended by pipeline 10), 5.1 airports (step 3 approach caps, step 6 gate: replaced by gate seeds), 5.2 / 5.2a water and terrain, 5.4 roads outside cities (S2b, needed by C8), 6.1a R3 cost, 6.3 step 4 ownership, 6.5-6.7 lighting / ground classes / landmarks, 7 determinism, 8a edits (8a.1 amended), 9.2 profile (amended by pipeline 11), 10a budgets, 11a.3 `--seed-selftest`, 11a.4 visual acceptance, 11f generator 3 results |
| `docs/contract.md` | Contract (the single source) | 3.2 profile, 3.6 world seed, 4 ready tile, 7 building mesh (COLOR channels, wall UVs: C6b, C7), 8 lights (R3 lights from lots: C5), 9 ground (classes, airports; C4 way surfaces), 11 determinism |
| `docs/decision-library-direction.md` | Gates | Decision 4 (freeze), 5 (integrate first), 6 (measure on a weak device), 8 (data formats = the long-lived contract: city file and profile keys), 11 (roadside towns first, hex city deferred) |
| `docs/spec-simplane-integration.md` | Freeze state | 7b and 8: step 3 (world frame, S1) and the first measurement are not done; the freeze holds |
| `docs/spec-terrain-v2.md` | Terrain rules the cities stand on | Review decisions T1 (`ground_height_at`, drape and bases on the exact height function), T4 (no procedural detail in seed worlds); G4 `city_flatten` is not built |
| `docs/spec-night-windows.md` | Window shader and light profiles | Design 1-4 (which buildings get windows, layout, shader, profile keys): C6 (kinds, floors), C6b (roofs and wall UVs), C7 (fenestration) |
| `docs/spec-tiles-lod-v2.md` | Bands R0-R3 by screen-space error | What each band carries (pipeline 3.6, 7.5) |
| Beil et al. 2020 + Road2CityGML3 | Road objects (pipeline 3.6) | Section, Intersection, Square, Track, granularity, function codes, non-overlapping surfaces |

Not relevant: `spec-traffic.md` (shelved; only reuses C4's section links later), `spec-terrain.md` DEM parts, `spec-tile-cache.md`.

## 2. Gates (the author decides)

| Gate | State | Phases |
|---|---|---|
| G-freeze | Freeze holds (`spec-simplane-integration.md` 8: step 3 and the first measurement are next) | **C0, C1 allowed now** (no output change). C2-C8 change generated cities: need the author to lift the freeze or to declare seed cities part of the integration |
| Hold | **Author, 2026-10-30 (later): C2, C3 and C4 start only with the author's explicit approval**, each separately; the "start after" decisions below are on hold | C2-C4 |
| G-freeze, C2 | Author decision 2026-10-30: start C2 after C1 (on hold, see above). C2 keeps the generated output identical; its new city-file and profile keys fold into the unreleased 1.12 | C2 |
| G-freeze, C3 | **Author decision 2026-10-30: start C3 after C2.** First phase that changes the generated cities (`SEED_GEN_VERSION` 4, goldens regenerated, hex code deleted). Before it starts: H1 passed, the `seed-low` generator 3 baseline recorded, D3 (triangle budget from C0) and D7 (seed plan memory pool) written | C3 |
| G-freeze, C4 | **Author decision 2026-10-30: start C4 after C3.** Before it starts: D4 written into pipeline 3.6 (Track for `path`, surface material, shoulder, large roundabouts); a new ground class or sub-kind for R0 sidewalks needs an architect brief and a minor api bump, else straight to the implementer | C4 |
| G-1.12 | api 1.12 unreleased (`CHANGELOG.md` "Unreleased") | Profile and city-file keys of C2 fold into 1.12 if they land before its release; after release they need 1.13 |
| G-order | Decision 11 puts roadside towns and villages before full cities; the pipeline puts `linear` last (C8) | Author: keep C8 last, or pull `linear` settlements forward after C3 |
| G-device | No weak-device measurement exists (decision 6) | Budgets in `perf-seed.md` are dev-Mac numbers with an assumed 3× factor |

## 3. Spec fixes before the phase that needs them (main session, docs only)

| # | Fix | Before |
|---|---|---|
| D1 | Pipeline 11: full old → new key table. `seed.city.cell_size_m` does not exist: it is the city file's `layout.cell_size_m`. `bridge_spacing_km` → `bridge_spacing_m`; `floor_m` → `seed.floor_m`; `bridge_max_m`, `approach_*` move to their groups; `seed.countryside.edge_warp` stays | C2 |
| D2 | Pipeline 10: `addons/simworld/seed/city.schema.json` changes (today `layout` requires `type` and `cell_size_m`) | C2 |
| D3 | Pipeline 8 / `perf-seed.md`: set the triangle budget per band from the C0 baseline | C3 (fixed in C0) |
| D4 | Pipeline 3.6: `path` level = Track (not Road); surface material per way surface (cobblestone old town, pavement sidewalks, asphalt) as profile data; `shoulder` on rural cross-sections (C8); large roundabouts may split; source note 16 now read from the paper | C4 |
| D5 | Pipeline 7.4: all four building COLOR channels are taken (contract 7: r profile, g seed, b floor height, a roof height) and UV holds the window grid, so fenestration needs CUSTOM0 or a packed channel: a contract decision, minor bump | C7 |
| D6 | Pipeline 7.3: pitched roofs meet the 1.7 per-vertex walls and the W3 wall UVs (contract 7): gable triangles are windowless walls (`u = -1`), the window grid stops at the eaves; contract 7 gets a note | C6b |
| D8 | Review m5 (2026-10-30, for `library-architect` before C2): the building `kind` is a `String` per building (one allocation each, in the 17 MB plan cache); make it a `u8` index into the kinds table. `CellInput` hands stage b the whole `CityPlan` and `SeedWorld` and reads neighbour cells; rule 2 needs a narrow input type | C2 |
| D7 | `perf-seed.md`: a `budget::Pool` for seed plans (today only a plan count caps them) | C3 |

## 4. Phases, owners and briefs

Legend: **main** = do it in the main session (small, one module); **RWS** = `rust-world-streamer`; **GCI** = `godot-csharp-integrator`; **LA** = `library-architect` (brief only); **CC** = `code-critic` (after every phase). Every phase ends with the pilot cycle (pipeline 9) and the `seed perf` line (`perf-seed.md`).

### C0: inspection on generator 3 (allowed now)

- **Owner:** RWS (CLI only). No LA needed: no contract, no profile.
- **Files:** `rust/simworld-cli/src/seed/preview.rs` (extend), new `rust/simworld-cli/src/seed/gallery.rs`; reads `rust/simworld-core/src/seed/city*`.
- **Read:** pipeline 9, 13 (C0 row); `perf-seed.md` "Measuring".
- **Deliver:**
  - `seed preview --layers` with the generator 3 layers that exist (`footprint` = cells, `major` = arterials and radials, `zones` = districts, `local`, `buildings`) plus `--extent`.
  - `seed gallery` with `old_center_hex` only (the three morphologies come in C3).
  - `gallery.json` / `gallery.md`: build ms per level, bytes, triangles at R0 and R2 (mesh the tiles through the existing pipeline), counts.
  - The baseline numbers for D3.
- **Accept:** CLI tests green (`cargo test -p simworld-cli`), the gallery runs in < 10 s release, goldens unchanged.
- **Then:** CC (cost of the gallery itself is irrelevant; check it does not leak into the core).

### C1: code split by stage, identical output (allowed now)

- **Owner:** RWS.
- **Files:** `rust/simworld-core/src/seed/city.rs`, `seed/city/*` → `seed::city::{context, network, zoning, lots, buildings}` around the generator 3 code; stage hash per output.
- **Read:** pipeline 1 (table), 8 (stage hashes), 13 (C1); `perf-seed.md` rules 1-5.
- **Accept:** `golden_tests/seed_golden.rs` and every seed golden **bit-identical**; `cargo test -p simworld-core seed::` green; `city_cost` within ±5 % (report line); clippy clean.
- **Then:** CC; `/bench-check rust`.

### Host prerequisites (before C3; GCI; host only, allowed under the freeze)

- **H1:** the Godot S2-S4 group of `--seed-selftest` (spec-world-seed 11a.3; status: not built): cities deliver in every band, one owner per building, lights present, no building in a fence.
- **H2:** a seed-world benchmark for `/bench-measure` (`--benchmark=seed-low`: low pass over Vilnius of `world/two-cities`; files `low-poly-map/scripts/Diagnostics/BenchmarkDef.cs`, `BenchArgs.cs`, the skill's argument list). Baseline recorded on generator 3.
- **Accept:** `/bench-check seed`; one `/bench-measure seed-low baseline` run.

### C2: stage 1, context and the city file (gate: G-freeze; G-1.12)

- **Brief first:** LA (city file format and profile keys are long-lived contract, decision 8). Input: D1, D2.
- **Owner:** RWS; then `/contract-bump` (profile keys, schema, `default.json`, CHANGELOG).
- **Files:** `seed/files/city.rs`, `addons/simworld/seed/city.schema.json`, `profile/seed_city_schema.rs`, `profile/ranges.rs`, `default.json`, new `seed/city/context.rs` (from C1).
- **Read:** pipeline 2, 10, 11 (`seed.context`), 14 (generator 3 files); spec-world-seed 4.2, 8a.1.
- **Deliver:** footprint outline and cuts, constraint raster, centrality, seeds (centre, gate), guides; new keys; old files load with one warning each; `--layers footprint,constraints`.
- **Accept:** output unchanged (generator 3 still builds the cities: goldens identical); new tests: old files map as pipeline 10 says, guide validation errors, raster bytes ≤ 410 KB at 8 km; `seed perf` context ms.

### C3: major roads, superblocks, zoning (gate: G-freeze)

- **Owner:** RWS (biggest phase; split into two runs: C3a growth + morphology rules, C3b faces / superblocks / zoning + hex deletion).
- **Files:** `seed/city/network.rs`, `zoning.rs`, delete `hex.rs` and the hex parts of `place`, `arterials`, `radials`; the two test cities in `low-poly-map/world/two-cities/cities/` rewritten; `SEED_GEN_VERSION` 4.
- **Read:** pipeline 3.1-3.5, 5.4, 13 (C3), 14 (determinism, no cells, network, morphology metrics, plausibility, zoning); `perf-seed.md` rules 4-9; D7.
- **Accept:** goldens regenerated **on purpose** (commit says why); all pipeline 14 tests for these stages; city plan < 40 ms and ≤ 2 MB on the two-cities world; gallery Fig. 2 streets row for organic / grid / radial; `/bench-check seed` (H1) and `/bench-measure seed-low compare=<H2 baseline>`.
- **Then:** CC (planar-graph robustness and allocation in growth loops are the risks).

### C4: local streets and road objects (gate: G-freeze)

- **Brief first:** LA only if R0 needs a new ground class or sub-kind (contract 9 encodings, minor bump); otherwise straight to RWS. Input: D4.
- **Owner:** RWS; GCI only if a ground class / sub-kind is added (shader colour).
- **Files:** `seed/city/network.rs` (local growth), new `seed/city/roads.rs` (roads, sections, intersections, squares, surfaces), `seed/city/tile.rs` (emission per band).
- **Read:** pipeline 3.6, 4, 7.5 band table; contract 9 (ground classes, stacking levels); paper notes in D4; `perf-seed.md` rule 11 (cut vs stack: measure both).
- **Accept:** pipeline 14 "Road objects"; superblock (a) < 1 ms p50; R0 ground bytes per tile reported; `/bench-measure seed-low` overdraw / frame compare.

### C5: blocks, lots, attributes, R3 lights from lots (gate: G-freeze)

- **Owner:** RWS.
- **Files:** `seed/city/lots.rs`; R3 path in `seed/city/tile.rs`; lights.
- **Read:** pipeline 5.1-5.3, 6; spec-world-seed 6.1a; contract 8.
- **Accept:** pipeline 14 "Lots and buildings" (anchor, frontage, lot alignment ≥ 85 %), "R3 light = R1 light"; R3 tile time below generator 3's (`seed perf`).

### C6: shape rules, LoD1, storeys, dense scenario (gate: G-freeze)

- **Owner:** RWS.
- **Files:** `seed/city/buildings.rs`; profile `seed.zones.*`, `seed.scenarios.dense` (`/contract-bump` if 1.12 is released).
- **Read:** pipeline 7.1, 7.2, 7.5, 8 (triangle levers), 11.1; spec-night-windows 1 (kinds → windows); `perf-seed.md` rule 10.
- **Accept:** paper ranges test (pipeline 14); triangles per band within the D3 budget, else the levers of pipeline 8 in order; gallery Table 2 complete.

### C6b: pitched roofs in the mesher (gate: G-freeze; also changes real OSM output)

- **Brief first:** LA (mesher feature shared with OSM `roof:shape`; contract 7 note). Input: D6.
- **Owner:** RWS (mesher, `mvt` roof tags); GCI checks `building_windows.gdshader` on gable walls (`/bench-check window`).
- **Files:** `mesher.rs`, `mesher/emit.rs`, `mvt/building.rs`.
- **Read:** pipeline 7.3; contract 7 (per-vertex walls, wall UVs, roof UV); spec-night-windows 2.
- **Accept:** OSM goldens change only where `roof:shape` is tagged (stated in the commit); OBB fill ≥ 0.85 rule; triangle delta per band in the `seed perf` line; `/bench-check window lod`.

### C7: fenestration channel and day windows (gate: G-freeze; api minor)

- **Brief first:** LA (vertex channel choice, D5). Then RWS (Rust half) → `/contract-bump` → GCI (shader, gated on `api_version`) → CC.
- **Read:** pipeline 7.4; contract 7; spec-night-windows 3.
- **Accept:** `/bench-check window`; `/bench-measure seed-low` (fragment cost of day windows on the Mobile renderer).

### C8: `linear` settlements and road-entry seeds (gate: G-freeze, G-order; needs S2b)

- **Brief first:** LA (settlement file format belongs to the country layer, decision 11; pipeline 12).
- **Owner:** RWS.
- **Read:** pipeline 2.4, 12; spec-world-seed 5.4 (S2b roads outside cities); decision note finding 7.
- **Accept:** entry seeds end on the network (pipeline 14 "Network"); a ribbon town in the gallery.

## 5. Brief template (copy per run)

```
Phase <Cx> — <one line>
Files: <paths>
Read only: pipeline <sections>; perf-seed.md; <contract / other sections>
Do: <deliverables>
Do not: touch <out of scope>; change goldens unless named here
Accept: <cargo test filter>; <pipeline 14 test names>; seed perf line vs budgets <rows>; <bench-check / bench-measure>
Report: files changed, test counts, seed perf line, what is not built
```

## 6. Cost of the plan in agent runs

C0, C1: one RWS run each, plus CC. H1 + H2: one GCI run. C2, C4, C6b, C7, C8: LA brief + implementer(s) + CC. C3: two RWS runs + CC. C5, C6: one RWS run + CC each. Small follow-ups (doc fixes D1-D7, a failing test) stay in the main session.
