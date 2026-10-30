# Seed generation: performance rules

Scope: all code under `rust/simworld-core/src/seed/`, above all generator 4 (`docs/spec-seed-city-pipeline.md`, phases C0-C8). Read this file whole (it is short) before any seed phase. The architect puts the budget rows a phase touches into its brief, the implementer reports the numbers, and the critic checks against them. Budgets and rules here win over "it works".

## Target

- **Weak device:** iPhone-class, Mobile renderer, 2-4 GB RAM, thermal throttling. Worker threads: performance cores − 1 (`spec-terrain-v2.md` section 5). The dev Mac is only the measuring host.
- **Until the device is measured** (decision 6 of `docs/decision-library-direction.md`), assume one iPhone core is **3× slower** than one dev-Mac core and plan with that headroom. This is an assumption: replace it with the measured factor once there is one.
- **Main thread:** no seed work at all. Seed output arrives like any tile; `update_camera` / `poll_*` stay < 0.5 ms.

## Budgets (release build, dev Mac, one thread)

| Level | Time | Memory | Source |
|---|---|---|---|
| City plan (context, major roads, zoning), once per city and session | < 40 ms (8 km radius) | ≤ 2 MB per city; all cities ≤ 16 MB | pipeline 8; world-seed 10a |
| Superblock plan (a): local streets, blocks, lots | < 1 ms p50, < 3 ms max | ≤ 60 KB | pipeline 8 |
| Superblock plan (b): buildings | < 2 ms p50, < 6 ms max | ≤ 150 KB | pipeline 8 |
| Plan cache | — | iOS ≤ 32 MB (`plan_cache` cap) | world-seed 10a |
| Tile assembly | ≤ generator 3 (`city_cost`) | — | pipeline 8 |
| All seed memory on iOS | — | ≤ 64 MB above today's budgets | world-seed 10a |
| Triangles per tile, max (buildings + ground, no terrain, no lights) | R0 8 000 + 2 500; R1 16 000 + 4 500; R2 34 000 + 4 600; R3 0 + 4 000 | R2 mesh up to 3.7 MB / tile: the cost to watch | Generator 3 baseline below |

**Generator 3 baseline** (2026-10-30, after the C0 review fixes and the approach-cap fix):
- **How it was measured:** `seed gallery` on two-cities, release, dev Mac. Every plan was warmed first, each timing is the median of 5 repetitions, and the figures agree across 3 runs within 2-7 %. The tile figures cover **every tile** over the city's bounding box.

| | Vilnius | Kaunas |
|---|---|---|
| City plan | 6.0 ms / 671 KB | 3.6 ms / 375 KB |
| District plan (one 800 m hex cell) p50 / p95 / max | 0.75 / 2.5 / 7.0 ms | 2.35 / 3.3 / 7.5 ms |
| Stage a (streets + lots) p50 / max | 0.64 / 2.5 ms | 1.70 / 2.3 ms |
| Stage b (buildings) p50 / max | 0.14 / 4.7 ms | 0.57 / 5.5 ms |
| Centre tile assembly z13 / z14 / z15 / z16 | 1.33 / 0.62 / 0.28 / 0.15 ms | 1.8 / 0.65 / 0.34 / 0.13 ms |
| Building tris p95 / max: R0, R1, R2 | 1.5k / 4.8k, 5.3k / 13.0k, 17.3k / 31.3k | 2.5k / 4.7k, 9.1k / 14.7k, 31.9k / 33.6k |
| Ground tris max: R0, R1, R2, R3 | 1.6k, 2.6k, 2.2k, 2.1k | 2.5k, 4.4k, 4.5k, 3.6k |
| Tile ms (assembly + meshing) p95: R0 / R1 / R2 / R3 | 1.4 / 3.8 / 10.5 / 14.1 | 2.7 / 8.3 / 24.2 / 30.2 |

- **Plan cache** after the run: 576 plans, 17 MB.
- **Noise:** max values over hundreds of tiles catch single disturbed tiles. Compare p50 and p95, not max.

**How the triangle budget was set** (each band's budget ≥ generator 3's max, so the baseline itself is in budget):
- R0 keeps headroom for parts and pitched roofs.
- R1 is about generator 3, so it relies on the levers of rule 10.
- R2 stays at generator 3: one box per building, rule 1. Growth there multiplies across the many R2 tiles in view.
- Known miss in the baseline: stage a p50 for Kaunas is 1.7 ms against the 1 ms superblock budget. The unit is a 0.55 km² hex cell, not a superblock.

A phase that misses a budget is not done. Either fix it or record the miss and the decision in the spec's results section.

## Rules

1. **Do work once, at the coarsest level that can do it.** City-wide work goes in the city plan, block work in the superblock plan. Tile assembly only clips, projects and emits. R3 stops at lots and never runs stage (b). R2 gets one box per building. Build stage (b) only when a tile at R2 or finer asks for it.
2. **Locality.** A tile builds only the superblocks it touches, and a superblock never reads a neighbour. Never run a pass over the whole city per tile.
3. **No new mechanism.** Reuse what exists:
   - `jobs` (priority, generation cancel): check `JobCtx::is_cancelled` between superblocks and between growth rounds;
   - `gen_cache::GenCache` (bounded memo of pure functions);
   - the `Arc<OnceLock>` plan cache (`seed/city/cache.rs`);
   - `MemoryBudget` (`budget.rs`);
   - the recent-tile cache.

   Seed plans are not yet a `budget::Pool`. Add one before generator 4's cache grows, rather than a second budget.
4. **Data layout.**
   - Plans are flat arrays (struct of arrays, `Vec` with capacity, `u32` indices into them). No `Vec<Vec<_>>` per lot, no `Rc` / `Arc` graph, no `String` ids: ids are hashes (`u64`).
   - Graph nodes are quantized to an `i64` mm lattice. Junction and side tests are exact integer predicates: cheaper than f64 tolerances, and they cannot flip between platforms.
   - Every plan reports `bytes()` (the generator 3 pattern).
5. **Allocation.**
   - No allocation per segment, per lot or per vertex inside loops: give each job reusable scratch buffers. Don't use `collect()` into temporaries, and don't call `format!` or `to_string` in a build path.
   - No `HashMap` in a hot path: use a sorted `Vec` with binary search, or a bucket grid. This also keeps the iteration order deterministic.
6. **Spatial queries.**
   - A uniform bucket grid of accepted segments (`snap_m × 4`).
   - The constraint raster (50 m) for quick rejects.
   - Exact polygon tests only for the candidates that survive a bounding-box check.
   - Never test a segment against every segment or a lot against every polygon.
7. **Math.**
   - No trig in the hash path: use fixed tables (as the pipeline spec's pitch table does) and exact complex rotation for outline directions; `libm` through `math.rs` only where it is unavoidable.
   - f64 for plan coordinates, f32 only at emission.
8. **Bounded work.**
   - Every loop that grows something has a cap from the profile (`max_segments`, number of Chaikin passes, split depth), and hitting a cap logs one warning.
   - Drop slivers below `min_lot_m2` as early as possible.
   - No geometry library that allocates per call in a per-lot loop.
9. **Parallelism.** A plan is built single-threaded, which keeps its order fixed and its output deterministic. Parallelism comes from many plans and tiles on the job pool; never nest parallelism inside a plan.
10. **Output size.**
    - Bytes per tile and triangles per band are pinned per phase.
    - Levers in order:
      - R1 with roofs only on the main part;
      - a merged mass per row of houses at R1;
      - fewer parts.
    - Never relax the paper's lot ranges first.
11. **Cut, don't stack (measure first).** Way surfaces that tile a section, and intersections that do not overlap their sections, avoid overdraw and z-fighting (Beil et al. 2020, section 5.7). Overdraw costs fill on a tile-based mobile GPU, while cutting costs CPU on the workers. Compare both with `/bench-measure` before choosing for R0.
12. **Detail by rule, not stored.** Keep only the recipe (hash, rules, profile). Explicit street geometry is huge: NYC's curbs alone are 2 GB and its sidewalks 1.3 GB (Beil et al. 2020, Table 3). Kerbs, holes, markings, clearance spaces and lanes are not generated (pipeline 3.6).

## Measuring (every phase, release build, quiet machine)

- **Today (generator 3):** `cargo test -p simworld-core --release city_cost -- --ignored --nocapture` gives district plans p50 / p95 / max ms, KB, and tile ms at z13-z16.
- **From C0:**
  - `simworld-cli seed gallery` (pipeline section 9): ms per stage, peak bytes, triangles at R0 / R2, counts.
  - Each phase reports a delta table against the previous phase and replaces `city_cost` with per-stage numbers.
- **Host cost:** `/bench-measure` has no seed-world benchmark yet; every path uses the Vilnius archive. Add a seed path (low pass over a generated city, `--world=` two-cities) before C3 changes the cities. Until then, host cost for seed work is a gap to state, not a pass.
- **Report line** (implementer, in the phase report):
  `seed perf <phase>: city plan <ms>/<KB>; superblock a p50/max <ms>, b p50/max <ms>; tile z13..16 <ms>; tris R0/R2 <n>; vs budget <ok | MISS: row>`

## Review checklist (code-critic, section 4)

- A seed loop allocates, hashes into a `HashMap`, formats strings or calls trig per item.
- Work sits at a finer level than it needs (per tile instead of per plan), or a stage runs for a band that does not need it.
- A superblock reads a neighbour, or a tile triggers a city-wide pass.
- A long build has no cancel point.
- A new cache, pool or scheduler duplicates an existing one.
- A growth loop has no cap.
- The phase report has no `seed perf` line, or no measurement backs a budget claim: MAJOR.
