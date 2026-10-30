# Spec: Seed city generation as a staged pipeline

Status: **proposal, revision 2 (2026-10-30), spec only (no code yet).** It replaces the city-generation design of `docs/spec-world-seed.md` sections 5.3 (city plan) and 6.1-6.4 (district plan, street styles, blocks / lots / buildings, curves). The rest of that spec still applies: files, airports, water, terrain coupling, roads outside cities, lighting (6.5), ground classes (6.6), landmarks (6.7), determinism (7), real data (8), edits (8a, amended in section 10), storage, contract. What is built today (S2-S4, `SEED_GEN_VERSION` 3, `rust/simworld-core/src/seed/city/`, results in `spec-world-seed.md` 11f) is called **generator 3** below. This spec targets **generator 4**.

Revision 2 follows the paper more closely than revision 1: **no hex cells anywhere** (not even as a zoning raster); the three morphologies of the paper (organic, grid, radial) are the city layouts; zoning is assigned to blocks after the road network, as in the paper's stage 3; the paper's parameter ranges (its section 3.10) are the profile defaults; pitched roofs (LoD2) and fenestration (LoD3) are in scope; the inspection tools reproduce the paper's figures and table.

Freeze note: `docs/decision-library-direction.md` item 4 freezes library features until SimPlane flies over the seed world. This spec is design work. Phases C0 (inspection tools) and C1 (code split with identical output) change no output and fit under the freeze. C2 onward changes the generated cities and starts after the integration, or earlier if the author decides that the seed cities are part of the integration. The profile keys of section 11 change keys that api 1.12 introduced. While 1.12 is unreleased they fold into it. After its release they need a new minor, which the freeze defers.

## Basis

Shariatpour et al., *Computational Urban Science* (2026) 6:31, "Procedural generation of urban morphologies". Its framework (Fig. 1) has four stages:

1. **Data collection and preprocessing**: GIS layers, DEMs and historical maps are cleaned and become the constraints and inputs of the later stages.
2. **Road network generation** with parametric L-systems (after Parish and Müller 2001): a hierarchy of primary, secondary and local streets grown from seeds, with global goals (the morphology) and local constraints (obstacle avoidance, terrain, snapping to existing roads).
3. **Block subdivision** with graph grammars and **zoning rules**: the closed faces of the network become blocks, the blocks are zoned and split into lots.
4. **Building geometry** with shape grammars driven by **lot attributes**, at LoD0-LoD3 (Fig. 3: footprint, extruded mass, roof form, facade with windows).

It is tested on three morphologies: **organic** ("Siena-like": irregular, curving, T junctions, small blocks), **grid** ("Barcelona-like": orthogonal blocks with avenues) and **radial** ("Paris-like": spokes from a centre and concentric rings). Each runs in a **default** and a **dense** scenario. Results: Fig. 2 shows each case decomposed into full layout / streets only / lots only; Figs. 4-6 show the 3D models, with parks as whole green superblocks; Table 2 compares time, RAM, CPU-seconds, file size and polygon count of default, dense and traditional modelling. Run times: organic about 15 min, radial about 12 min, grid under 10 min per case. For the radial case the paper says the hierarchy comes from the inputs, not emergent, and checks it with plausibility indicators: ring continuity and spacing, angular separation and connectivity of the radials, and alignment of parcels to streets. The method is refined in a pilot cycle: small extents first, adjust parameters, then the full run.

The paper's section 3.10 parameter ranges are the defaults here (section 11.1): branch angles 30-90°, segment lengths 10-100 m, road widths 8-15 m, blocks 25 × 25 to 125 × 125 m, lot frontage 5-20 m, floor height 3 m, 3-12 storeys, roof pitch 15-45°, fenestration 25-50 %.

Two further sources shape the data model, not the method:

- **Beil, Ruhdorfer, Coduro, Kolbe (2020)**, "Detailed Streetspace Modelling for Multiple Applications: Discussions on the Proposed CityGML 3.0 Transportation Model", *ISPRS IJGI* 9(10):603, with its modelling guideline Road2CityGML3 (TUM, 2023). A road is a chain of **Sections** joined by **Intersections** (classed T, 4-way, roundabout, dead end); its surface is decomposed at three **granularities**, area (the whole width), way (carriageway, sidewalks, cycle path, parking bays separately) and lane; surfaces carry a **function** (driving lane, footpath, cyclepath, square, parking lay-by, crosswalk) or an auxiliary function (kerbstone, green area, traffic island, raised median); **Squares** (plazas, parking lots) are not segmented; markings are separate objects; traffic spaces link to predecessors / successors. Section 3.6 uses this as the output model of stage 2 and to pick the road detail per band.
- **Adreani, Bellini, Fanfani, Nesi, Pantaleo (2024)**, "Smart City Digital Twin Framework for Real-Time Multi-Data Integration and Wide Public Distribution", *IEEE Access* (doi 10.1109/ACCESS.2024.3406795; arXiv 2309.13394), the Snap4City twin of Florence. It builds its city from real data (cadastre footprints, DSM, orthophotos), not procedurally, but confirms four choices made here: LoD1 buildings are footprints extruded to the **eave height** and pitched roofs sit on top (7.3); each building belongs to exactly one tile by the **centroid** of its footprint while its geometry may cross tiles (section 6, our anchor rule); the terrain is a tiled RGB-encoded DTM meshed with Martini and every object is placed on it (our Delatin terrain and drape); a fixed-zoom building tiling must be **regrouped into coarser tiles** for wide views, or the client drowns in tiles (our R2 / R3 bands and merged sprite cells). Its street trees are 3-4 model categories instanced hundreds of times with a scale per instance, the cheap way to add vegetation later (section 15).

What differs is the runtime, not the method: the paper runs CityEngine offline for minutes per 2 km². Here every stage is a pure Rust function that runs in milliseconds per tile on a phone, bit-identical on every platform, and local (a tile builds only what it needs). Section 1 lists what is taken and what is adapted.

## 0. Why change generator 3

Measured and seen in S2-S4 (`spec-world-seed.md` 11f, screenshots):

1. **The city is a honeycomb.** Districts are 800 m hex cells and every arterial is a warped hex edge, so from altitude the city reads as a hex mosaic with a uniform grain. The drop share only hides the Y junctions. The paper's cities have no cell grid at all: the roads come first and the zones follow the blocks they make.
2. **One morphology.** `old_center_hex` is the only layout. None of the paper's three (organic, grid, radial) exists, and a city cannot have an organic old town inside a grid or radial city.
3. **Every style is a warped lattice.** The old town is a jittered grid, not organic. Radials random-walk with a 60° cap. They never meet a lattice street except by chance.
4. **Streets, lots and buildings are one step.** A lattice quad becomes two rows of lots and each lot one rectangle or L. Lots have no attributes, so buildings cannot depend on them. Lots are 10-120 m wide, far from the paper's 5-20 m, and buildings 1-35 floors with flat roofs.
5. **Special cases instead of seeds.** Roads entering a city "claim" a radial (S2b design). The airport access road is a steered walk.
6. **No stage can be inspected alone.** `seed preview` draws the city plan. Streets, lots and buildings exist only inside tiles.
7. **R3 needs every building.** Lights come from buildings, not from lots (deviation 4 of 11f).

What generator 3 got right stays: the counter-based hash, the plan caches with `OnceLock`, the request semantics of district placement (order, share, `near`, approach penalty, region growing; now applied to blocks, section 5.4), the airport and water rules, the tile assembly and ownership, kinds resolved through the profile, the approach caps. The hex code (`seed::city::hex`, the hex parts of `place`, `arterials`, `radials`) is deleted in C3.

## 1. The pipeline

```
 city file, world files, profile
        │
 ┌──────▼─────────────────────────────────┐  city plan, once per city (session cache)
 │ 1 CONTEXT   footprint, constraint map,  │
 │             centrality, growth seeds    │
 │ 2 ROADS     primary + secondary growth  │
 │  (major)    by the city's morphology;   │
 │             faces → superblocks         │
 │ 3a ZONING   zone per superblock         │
 └──────┬─────────────────────────────────┘
        │ superblock polygon + boundary roads + zone
 ┌──────▼─────────────────────────────────┐  superblock plan, stage (a) (LRU cache)
 │ 2 ROADS     local streets inside the    │
 │  (local)    superblock, same rules      │
 │ 3b BLOCKS   blocks, lots, attributes    │  ← R3 stops here (lights from lots)
 │   & LOTS                                │
 └──────┬─────────────────────────────────┘
 ┌──────▼─────────────────────────────────┐  superblock plan, stage (b), R2 and finer
 │ 4 BUILDINGS shape rules per lot →       │
 │             LoD1 mass, LoD2 roof,       │
 │             LoD3 fenestration           │
 └──────┬─────────────────────────────────┘
 ┌──────▼─────────────────────────────────┐  per tile (unchanged)
 │ ASSEMBLY    ground, roads, buildings →  │
 │             tile pixels                 │
 └────────────────────────────────────────┘
```

| Stage | Paper | Here | Runs in | Output |
|---|---|---|---|---|
| 1 Context | GIS, DEM, historical maps, cleaned | Seed files are already clean. The stage builds the **footprint**, a **constraint map** (water, airports, slope, approach caps) and a **centrality field**, and collects the **growth seeds**. Author **guides** stand in for historical maps | City plan | `CityContext` |
| 2 Roads | Parametric L-system, hierarchy, obstacle avoidance | Parish-Müller segment growth with the morphology as the global goal and local constraints. Primary and secondary roads grow over the whole city; the closed faces become **superblocks**. Local streets grow inside each superblock with the same rules | City plan (major), superblock plan (local) | `MajorNetwork`, `Superblock[]`, `StreetGraph` |
| 3 Blocks, zoning, lots | Graph grammars, zoning rules | **Zoning** assigns a land use to each superblock (3a). Blocks are the faces of the street graph; lots by frontage or OBB splits; every lot gets **attributes** (3b) | City plan (3a), superblock plan (3b) | `Zone` per superblock, `Block[]`, `Lot[]` |
| 4 Buildings | CGA shape grammars, lot attributes, LoD0-3 | Shape rules per land use turn a lot into **parts** (LoD1), give them a **roof** (LoD2) and a **fenestration** ratio (LoD3) | Superblock plan (b) | `Building[]` |
| Assembly | — | `seed::city::tile` as today | Tile job | `Building`, `GroundFeatures` per tile |

Taken from the paper: the four stages with typed hand-offs; the three morphologies as the city layouts; the road hierarchy grown by one L-system-style process; zoning applied to blocks after the network; lot subdivision by rules; buildings from lot attributes; LoD0-LoD3; parks as whole superblocks (Figs. 4-6); default and dense scenarios; its parameter ranges; decomposed previews and a metrics table per morphology and scenario; plausibility indicators; the pilot refinement cycle.

Adapted, with the reason:

- CityEngine and CGA are replaced by small Rust rule sets (runtime, determinism, no tool on the phone).
- Local streets grow per superblock, not in one global pass, so a tile builds only the superblocks it touches (section 4).
- Historical maps are author **guide lines** in the city file (section 2.3).
- LoD3 windows are a fenestration ratio drawn by the window shader, not modelled openings (section 7.4): a window quad per opening would multiply the triangles of R0 by ten.
- One optional extension beyond the paper: a city may have an **organic core** inside a grid or radial city (the old town inside the newer city, like Kaunas or Vilnius; section 3.3). A fourth layout, `linear`, exists only for roadside settlements (section 12).

## 2. Stage 1: context

Input: the city file, the world's water index, airports (fences, strips, approach surfaces), the height function, the city's landmarks and edits, the inter-city roads that enter it (S2b) and the profile's `seed` section. Output `CityContext`, in the city plane (`LocalFrame` at the centre, f64 metres, x east, y north).

### 2.1 Footprint

The city footprint is a closed polygon, no cell union:

1. **Outline.** A star-shaped polygon around `center`: `r(θ) = radius × (1 + edge_noise × n(θ))` sampled every `360° / n` with `n = clamp(2π radius / 200 m, 48, 512)`, where `n(θ)` is a periodic 1D value noise of `H(city, "edge", k)` with 6 and 13 lobes (amplitudes 0.7 and 0.3, `edge_noise` default 0.12). No trig in the hash path: the sample directions are a fixed table of unit vectors rotated by exact complex multiplication.
2. **Cut.** Water polygons, airport fences and strips (with `clearance_m`) are subtracted. A remaining piece smaller than `min_piece_km2` (0.1) is dropped; the piece holding the centre is the city, other pieces stay when they hold at least `min_piece_share` (5 %) of the area (a city across a river).
3. **Guides.** A guide `edge` (an old wall line) or an `axis` may extend outside the outline by up to 1 km; the outline is not grown for it.

### 2.2 Constraint map

A raster over the footprint's bounding box at `seed.context.cell_m` (default 50 m; an 8 km radius city is 320 × 320 = 102 k cells, 4 bytes each = 410 KB). It is a square lookup grid for speed only, never a layout unit and never visible in the output:

| Layer | Bits | Meaning | Used by |
|---|---|---|---|
| `block` | 3 | 0 free, 1 water, 2 fence / strip, 3 landmark clearance, 4 outside the footprint | growth, quick reject |
| `cap_dm` | 16 | approach surface height above the threshold base (dm), 0xFFFF = none | zoning penalty, envelopes |
| `slope` | 8 | max slope of the height function over the cell (%) | growth, zoning |
| `near` | 5 | flags within `near_m` (default 800 m): water, coast, river, airport, entry road | zoning (`near`) |

**Hard rules are always rechecked on exact geometry** (water polygons, fence and strip rectangles, landmark footprints) before a road segment, lot or building is accepted, so the raster resolution never decides whether a road crosses a runway strip.

### 2.3 Centrality field

A smooth scalar field `c(p)` in 0..1: 1 at the centre and at every `business` request's `at` point (section 10), falling off by `seed.context.centrality_km` (2.5). A sum of quintic falloffs, no trig, exact. Road step length, block size, lot frontage and building height read it, so the core is denser and taller without a rule per ring. It also steers zoning (section 5.4).

### 2.4 Growth seeds and guides

Seeds are points with a heading, a level and a goal:

| Seed | Where | Goal |
|---|---|---|
| Centre | `center`, n headings from the morphology (organic 3-5, grid 4 along the axes, radial `spokes`) | Outward to the footprint edge |
| Road entry (S2b) | Where each inter-city road crosses the footprint, heading = the road's bearing | Inward to the first network node, else the centre. **Replaces "a road claims a radial"** |
| Airport gate | The fence gate of each linked airport (`spec-world-seed.md` 5.1 step 6) | Toward the centre until it snaps to the network. **Replaces the steered access walk** |
| Guide | Author lines (`guides`, section 10): `axis` (a boulevard), `ring` (a closed ring road), `edge` (an old-town wall line: growth inside it uses the core morphology) | Fixed geometry, inserted before growth as primary or secondary roads (Catmull-Rom through the points) |

Guides are the analogue of the paper's historical maps: an author who knows the real city draws two or three lines and the generator fills in the rest. A city with no guides is fully procedural.

## 3. Stage 2: road network

### 3.1 Hierarchy

| Level | Seed road class | Width (right of way) | Cross-section (3.6) | Made by |
|---|---|---|---|---|
| Primary | `primary` | 15 m | sidewalk 2.5 · carriageway 10 · sidewalk 2.5; boulevard: 2 · 4.5 · median 2 · 4.5 · 2 | Seeds (centre, entries, gates), `axis` guides, radial spokes, grid avenues |
| Secondary | `secondary` | 12 m | 2 · 8 · 2 | Major growth branches, `ring` guides, radial rings, superblock splits (3.5) |
| Local | `residential` | 8 m | 1.5 · 5 · 1.5 | Local growth inside superblocks (section 4) |
| Path | path | 3 m | footpath 3 | Park paths, courtyard passages |

The widths are the paper's 8-15 m for the whole right of way, sidewalks included. How the width splits into carriageway, sidewalks and an optional median is the level's **cross-section** (3.6). The carriageway is drawn with the ground road class of the same name (colour, lamps, per-band drop). Seed widths and cross-sections live in `seed.network.levels`, not in `ground.roads`, so real OSM roads keep their table. Boulevards (`dual` cross-section) are the default for radial spokes and grid avenues (Paris and Barcelona both have them) and off elsewhere.

### 3.2 Growth

One deterministic growth procedure after Parish and Müller (2001), used for major roads over the city and for local streets inside a superblock:

1. **Queue.** Proposed segments wait in a priority queue keyed by `(t, level, id)`: `t` is the step count from the seed (breadth-first), `id = H(parent_id, "branch", k)`, a seed's id `H(city, "seed", kind, index)`. Plan building is single-threaded per city or superblock, so the order is fixed by the keys.
2. **Global goals.** The morphology rule (3.3) proposes a segment's successors: a continuation and zero to two branches, each with heading, length and level. Goal-directed seeds blend their heading toward the goal with weight `clamp(1 − remaining / goal_blend_m, 0, 1)`.
3. **Local constraints**, in this order, on the proposed segment `a → b`:
   1. **Hard block.** If the segment enters water, a fence, a strip or a landmark clearance (exact test), try the rule's alternative headings (±`steer_deg` steps, at most `max_turn_deg` from the parent). Water is crossed as a **bridge** when the wet run is ≤ `bridge_max_m` and no bridge of the same or a higher level lies within `bridge_spacing_m`; otherwise the segment is cut at the obstacle and ends there.
   2. **Intersection.** A crossing with an accepted segment cuts the segment there, makes a node, and growth stops (T or X junction).
   3. **Snap to node.** `b` within `snap_m` of an accepted node moves to it, growth stops.
   4. **Extend to segment.** If a straight extension by up to `extend_m` reaches an accepted segment, extend and stop (closes a block instead of a dead end).
   5. **Angle and length.** Reject if it meets an existing segment at its start node at less than `min_angle_deg` (30°), or if it is shorter than `min_seg_m` (10 m) after the cuts.
   6. **Footprint.** Cut where it leaves the footprint (or the superblock, for local streets).
4. **Slope.** Each candidate's cost is the mean slope along it (constraint map, then the exact height function at its ends). The morphology says whether to steer to the least cost or stop at `max_slope`.
5. **Spatial index.** Accepted segments sit in a uniform bucket grid of `snap_m × 4`, so every check is local.
6. **Stop.** Queue empty or `max_segments` reached (a warning).
7. **Prune.** Dead ends shorter than `prune_m` are removed, repeatedly, except at the footprint edge, an obstacle or a seed.

The result is a planar graph: nodes quantized to 1 mm, edges with level, id, `lit` (by level and `H(edge, "lit")`) and the bridge flag. A road polyline is a chain of 10-100 m segments (the paper's segment length); organic chains get a Chaikin pass with fixed nodes for the drawn curve.

### 3.3 Morphology rules

A rule is data (profile `seed.network.<morphology>`, section 11). The city's `layout.morphology` selects one; an optional `core` (a disc of `radius_km`, or the inside of an `edge` guide) uses another. The rule applying to a segment is the one at its start point.

| Morphology | Major roads | Local streets (section 4) | Blocks | Junctions |
|---|---|---|---|---|
| `organic` (Siena-like) | From the centre seeds: continue with heading noise ±`curve_deg` (15°) per segment, segment 40-100 m × (1 − 0.4 c); branch with probability `branch_p` (0.35) per segment at **30..90°** off the continuation (either side, uniform by hash), never two branches within `min_branch_m` (60). Of three candidate headings (±`slope_steer_deg` 20°) it takes the least slope, so roads follow contours | Same rule, segment 10-40 m, `branch_p` 0.5, `extend_m` large | Irregular, 25-80 m | Mostly T |
| `grid` (Barcelona-like) | One orientation for the city (`layout.rotation_deg`, else aligned to the longest water edge in the footprint, else `H(city, "rot")`). Avenues (primary) every `avenue_every` blocks (5) along both axes, from the centre seeds; streets straight (±1°), branching at exactly 90° every block pitch `block_m + width`. Optional diagonals: guides `axis` (Barcelona's Diagonal). Stops at `max_slope` (12 %) instead of bending | The grid continues inside: one local street per block pitch, so the superblock is split into the regular blocks | Square, `block_m` 125 m default, 90 m dense; corners chamfered by `chamfer_m` (Eixample "xamfrans", default 0 = off) | X |
| `radial` (Paris-like) | Rings first: concentric closed polylines at radii `r_k = r0 + k × ring_spacing_m` (`r0` 400 m, spacing 600 m × (1 + 0.5 (1 − c))), drawn as a circle sampled every 50 m, Chaikin-smoothed and cut by obstacles (cuts leave arcs). Then spokes: `spokes` (8) primaries from the centre at equal angles + `H` jitter (±5°), straight; where the arc between two spokes on a ring exceeds `max_spoke_gap_m` (900), a secondary spoke starts on that ring outward. Spokes and rings snap to each other | Wedge streets: arcs parallel to the nearer ring every block depth and sub-spokes toward the centre (section 4) | Trapezoidal wedges, 60-125 m | X at ring × spoke, T inside |

As the paper notes for its radial case, the hierarchy (number of spokes, ring radii) is an input here, not emergent. The plausibility checks of section 14 verify that the rules produced what they declare.

`organic` and the core rule: an organic core inside a grid or radial city grows first from the centre; the outer rule's seeds start on the core boundary (where the core's major roads leave it) and at the centre headings outside the core. So the old town's lanes meet the newer grid at its edge, as in the paper's comparison of organic and planned fabric.

### 3.4 Water and airports

- **Water.** Bridges only where 3.2 step 3.1 allows them; rivers get a few bridges at the shortest crossings, chosen during growth.
- **Airports.** Fences and strips are hard blocks. Approach surfaces do not block roads (`spec-world-seed.md` 5.1 step 3); they cap heights (section 6).

### 3.5 Faces → superblocks

1. **Faces.** The bounded faces of the major graph, closed by the footprint boundary, water edges and fences. Half-edges are walked with an **exact pseudo-angle** (quadrant plus cross product, no `atan2`), ties by edge id. A face with an island keeps the hole.
2. **Cap.** A face larger than `max_superblock_m2` (0.4 km² × (1 − 0.5 c)) is split by a secondary road across its OBB's long axis at `0.5 ± 0.15 (2 unit(H(face, "split")) − 1)`, clipped to the face (in `grid`, the split snaps to the nearest grid line), until every part is under the cap.
3. **Merge.** A face smaller than `min_superblock_m2` (0.01 km²) merges with the neighbour across its longest secondary edge, which is removed unless that disconnects the graph.
4. **Superblock.** `id = H(city, "superblock", min boundary half-edge id)`.

The superblock is the unit of the plan cache and of zoning. Vilnius (8 km radius) at about 0.15 km² is about 1300 superblocks.

### 3.6 Road objects (after CityGML 3.0)

The network graph is the topology. What the tiles draw is a set of road objects built from it, following the CityGML 3.0 Transportation model (Beil et al. 2020, Road2CityGML3):

1. **Roads.** A chain of edges of one level that continue through their nodes (deviation < 30° at a degree-2 node, or the straightest pair through a junction, ties by edge id) is one `Road`, `road id = H(city, "road", min edge id)`. Guides and radial spokes / rings are one road each.
2. **Intersections.** Every node of degree ≥ 3 becomes an `Intersection` polygon: the minimal area shared by its roads, i.e. the convex hull of the points where each road's right-of-way edges meet its neighbours' (corner radius `kerb_radius_m`, 3 m local, 6 m primary, as a fixed 4-segment arc). Class: `t` (degree 3, one pair within 30° of straight), `y` (degree 3, otherwise), `x` (degree 4), `multi` (≥ 5), `roundabout`. A radial city's centre and every ring × spoke junction of two primaries is a `roundabout` (a ring of radius `roundabout_m`, 25 m, around a green island: the Étoile / Nation place type); a dead end gets a turning circle and the class `dead_end`. The intersection count per class is the junction histogram of the metrics (section 9).
3. **Sections.** The part of a road between two intersections (or an intersection and a dead end, a footprint cut or a bridge end) is a `Section`, ending exactly on the intersection polygon, so ribbons never overlap at junctions. `section id = H(road, "section", k)`; sections record predecessor / successor sections, which a later traffic layer can use.
4. **Squares.** A `civic` superblock's single block, and an organic core's widened junctions (a node whose block corners are more than `square_m`, 30 m, apart), become a `Square`: one paved area, not split into sections.
5. **Surfaces by granularity.** A section's surface is cut lengthwise by its cross-section into **way** surfaces with a CityGML function: carriageway (`driving_lane`), sidewalk (`footpath`), median (`green_area` or `raised_median`), traffic island (`traffic_island`, the roundabout centre). **Lane** granularity is not generated (no lane objects; lane markings are a later shader decal).

**Per band**, the granularity follows the LoD:

| Band | Granularity | Drawn |
|---|---|---|
| R3 | — | No roads (as today: R3 ground has no road ribbons below the profile's drop) |
| R2 | area | One ribbon per section at the carriageway width, intersections filled, primary and secondary only |
| R1 | area | As R2 plus local streets |
| R0 | way | Carriageway, sidewalks (pavement colour), medians and islands (green), intersections and squares (pavement), as ground polygons |

The function maps onto the existing ground classes: `driving_lane` → the road class of the level, `footpath` / square → pavement (the area class used for pedestrian areas), `green_area` / `traffic_island` → the park class. If R0 needs a distinct sidewalk colour, that is a ground class or sub-kind addition, decided in C4 against the contract's ground encodings (a minor api bump if it adds a value). The surfaces are emitted per tile like today's road lines, clipped by the mesher; their geometry is in plan coordinates, so neighbouring tiles cut the same polygon.

## 4. Local streets (superblock plan, stage a)

The plan of one superblock reads only its polygon (with holes), its boundary edges (levels, ids, widths), its zone, the morphology at its centroid and the constraint map's window. **It never reads a neighbouring superblock.** Local streets start on the boundary every block pitch (position `H(edge, "local", k)` in `organic`, the grid phase in `grid`, the ring / spoke phase in `radial`) and grow inward with the morphology's local rule (3.3). Street id `H(superblock, "street", seed k, step)`.

The zone (section 5.4) adjusts the local rule:

| Zone | Local streets |
|---|---|
| `old_town`, `residential`, `commercial` | The morphology's local rule as is |
| `industrial` | Block size × 2: every second local street of the rule is skipped |
| `park` | No streets. Two to four curved paths between boundary points (Figs. 4-6: parks fill a whole superblock) |
| `civic` | One block for the superblock (a square, a church, a market) |

## 5. Stage 3: blocks, zoning and lots

### 5.1 Blocks

Blocks are the faces of the superblock's street graph. Each is inset by half the right of way of each bounding street (the sidewalk is inside the right of way; a polygon offset with mitred joins capped at 2 × the inset; a collapsed block becomes ground). `block id = H(superblock, "block", min boundary edge id)`. Defaults give the paper's 25 × 25 to 125 × 125 m blocks.

### 5.2 Lot subdivision (graph grammar)

Two productions, chosen by the zone, after Vanegas et al. (2012):

| Production | How | Zones |
|---|---|---|
| `frontage` | Offset the block inward by the lot depth → a ring strip along the streets. Cut the strip at right angles to each street every `frontage_m` (jittered by `H(block, "cut", edge, k)`). Each convex corner gets one corner lot with two frontages. The interior is a courtyard (ground) or, in a shallow block (< 2 × depth), is split among the back lots | `old_town`, `residential`, `commercial` |
| `obb` | Recursive split across the long axis of the lot's OBB at `0.5 ± jitter` until frontage and area are in range; a lot without frontage merges into the neighbour with the longest shared edge | `industrial`, `civic` |
| `none` | The block stays one green lot | `park` |

Ranges (profile per zone, paper defaults): `frontage_m` 10-20 (dense 5-12), `depth_m` 15-30 (dense 12-20), `min_lot_m2` 60. Every lot must front a street. `lot id = H(block, "lot", split path)`.

### 5.3 Lot attributes

Decided from the lot alone:

| Attribute | Source |
|---|---|
| `zone`, `superblock` | 5.4 |
| `frontage_m`, `depth_m`, `area_m2`, `corner` | Geometry |
| `street_level` | Highest level among its frontage streets |
| `centrality` | `c(anchor)` |
| `storeys` | From the zone's range (3-12 for the paper's default; per zone below), skewed up by centrality and `street_level`, by `H(lot, "storeys")` |
| `coverage` | Zone range; corner and main-street lots higher |
| `keep` | `unit(H(lot, "keep")) < keep` |
| `envelope_m` | `min(storeys × floor_m, zone max_height_m, approach cap at the lot's highest-cap point (exact), landmark clearance)` for the eaves; the roof ridge must also fit (section 7.3) |
| `roof`, `pitch_deg`, `fenestration` | Zone rules by hash (section 7) |
| `kind` | The zone's kinds by hash; main-street lots prefer `retail` / `commercial` |
| `anchor` | The lot's pole of inaccessibility on a 1 m grid: the light point and the ownership point |

**R3 stops here (LoD0).** One light point per kept lot: `anchor`, at the lowest terrain height under the lot + `envelope_m`. Stage 4 keeps the anchor inside the main part and the main part's eaves at `envelope_m`, so the R3 light is the R2 / R1 light of that building. This restores `spec-world-seed.md` 6.1a.

### 5.4 Zoning (stage 3a, city plan)

Zones are assigned to **superblocks**, after the network, as the paper's zoning rules apply to blocks. No cells.

Zone types: `old_town`, `commercial`, `residential`, `industrial`, `park`, `civic` (plus `port` as a sub-type of industrial next to water). The city file's district requests become **zoning rules** (section 10), applied in file order to the superblocks that are still unassigned:

1. **Core.** Superblocks inside the `core` get `old_town` (a city without a core has none unless requested).
2. **Requests.** Each request picks superblocks by score: within `within_km` of the centre (or of its `at` point), `near` flags (water, river, coast, airport, entry road), centrality (`commercial` prefers high c, `industrial` and `park` low c), approach penalty (tall zones avoid approach caps), slope (`park` takes slopes above `max_slope`). It takes the best seed superblock, then grows over the superblock adjacency (shared boundary road) until it holds its `share` or `area_km2`, ties by superblock id. That is generator 3's region growing, on superblocks instead of hex cells.
3. **Parks.** `park_share` (default 0.08) of the area, in whole superblocks, spread by a minimum distance `park_spacing_m` (1.5 km) between park seeds: the paper's park superblocks.
4. **Fill.** The rest gets `fill` (default `residential`).

A zone is a land use, not a geometry: it sets the local street adjustments (section 4), the lot production and ranges (5.2), the building rules (7) and the ground class. Its outline is the superblock boundary, which is a road, so zone borders always run along streets, never across blocks.

## 6. Ownership and seams

As today (`spec-world-seed.md` 6.3 step 4), and as the Snap4City tiling does with footprint centroids: a building is emitted into every tile it overlaps, `owned` only in the tile holding its anchor. Keep / drop and ownership decisions are taken in plan coordinates before projection. Superblock boundaries are city-plan roads, so no lot or building crosses one. Local streets end exactly on boundary edges, computed on the boundary polyline from its quantized vertices.

## 7. Stage 4: buildings (superblock plan, stage b)

The paper's Fig. 3 builds one building in four levels of detail. Here they are steps of one rule set and also the band LoDs.

### 7.1 LoD1: mass (shape rules)

A zone has a small weighted set of **shape rules** (profile `seed.zones.<zone>.shapes`). Each turns the lot into one to four **parts**: a footprint ring (possibly with holes) plus `min_height` and `height`; the mesher already extrudes from `min_height` (`mesher/emit.rs`).

| Rule | Parts | Zones |
|---|---|---|
| `row` | Lot inset by `setback_m` (0 on the street side in old town), full lot width: terraced houses wall to wall | old_town, residential |
| `box` | Lot inset by `setback_m` and `side_gap_m`, cut to `coverage` | residential, industrial |
| `l`, `u` | Box with one or two wings along the frontage | residential, corner lots |
| `courtyard` | Ring along the frontage edges with a hole | old_town / residential corner and perimeter lots |
| `stepped` | 2-3 stacked boxes, each set back `step_m` and lower | commercial |
| `podium_tower` | Podium 1-3 storeys over the coverage area, a tower over `tower_share` of it set back from the main street | commercial (only where `storeys` > 8) |
| `hall` | Low wide box plus an optional office annex on the street side | industrial, port |
| `pavilion` | One small box | park (by hash, few) |

The choice is `H(lot, "shape")` weighted, filtered by the rule's minimum frontage and depth. Old-town landmarks (`landmarks` count): the most central kept old-town lots take `church` or `tower`. Real landmarks remain author models (`spec-world-seed.md` 6.7).

Constraints: every part inside the lot after setbacks; the **main part** contains the anchor and has its eaves at `envelope_m`; no part (ridge included) above the approach cap at its highest-cap point; parts of one building do not overlap in plan unless stacked; coverage within ±10 % or fall back to `box`. Floors = eaves height / `floor_m`, written as `levels`.

### 7.2 Storeys

Floor height `floor_m` = 3 m (the paper's value; night windows count floors from it). Storeys per zone, default (dense):

| Zone | Storeys |
|---|---|
| old_town | 3-5 (4-6) |
| residential | 3-6 (5-9) |
| commercial | 4-12 (6-12; towers up to `max_height_m` of the zone when set) |
| industrial | 1-3 (1-3), halls 6-12 m |
| civic | 2-4 |

The paper's range is 3-12. Industrial goes below it (halls), and a city may raise a `commercial` request's `max_height_m` above 12 storeys for a skyline; both are outside the paper and stated in the city file.

### 7.3 LoD2: roofs

Each part gets a roof:

| Roof | Pitch | Zones (weights by hash) |
|---|---|---|
| `gable` | 15-45° (`pitch_deg`), ridge along the part's OBB long axis | old_town 0.6, residential 0.5 |
| `hip` | 15-45° | old_town 0.3, residential 0.3 |
| `flat` | — | commercial, industrial, civic, residential 0.2, towers and courtyard rings always |
| `shed` | 10-15° | industrial halls 0.3 |

Ridge height = `half span × tan(pitch)`; the pitch is a hashed value in the range and the tangent comes from a fixed table (no platform trig). A roof that would exceed the approach cap or `max_height_m` lowers its pitch to fit, down to 15°, then becomes flat.

This needs a **roof type in the mesher**: a `roof_shape` (flat / gable / hip / shed) and `roof_height_m` on the building feature, meshed for a convex or rectangular-ish part from its OBB (a non-convex part, a courtyard ring or an L, gets a flat roof). Real OSM data uses the same path with `roof:shape` and `roof:height`, so it is a mesher feature, not a seed one. Roof faces keep the roof UV of today; wall UVs (W3) stop at the eaves, the gable triangles are windowless walls (`u = -1`). It changes geometry only, no ready-tile key, so no contract change; the contract's buildings section gets a note.

### 7.4 LoD3: fenestration

The paper's LoD3 models facades with windows at 25-50 % fenestration. Here each building gets a **fenestration ratio** (window area / wall area) from its zone's range (old town 0.25-0.35, residential 0.3-0.4, commercial 0.4-0.5, industrial 0.1-0.2, by `H(building, "fen")`), and the window shader draws window openings at that ratio **by day too** (dark glass, frame colour), as well as the lit windows at night. No window geometry.

The ratio has to reach the shader per building: one byte per vertex in a channel the building mesh does not use yet. The choice (a COLOR channel or CUSTOM) is made in C7 against the contract's vertex encodings and is a **minor api bump** (contract, CHANGELOG). Real OSM buildings get the default ratio of their kind's light profile.

### 7.5 LoD by band

| Band | Paper LoD | Tile content |
|---|---|---|
| R3 | LoD0 | Lots only: light points (5.3), no meshes |
| R2 | LoD1 | One box per building: the main part's footprint at its eaves height, flat |
| R1 | LoD2 | All parts with roofs |
| R0 | LoD3 | All parts with roofs, day windows at the fenestration ratio (the shader's grid is already per-window at R0) |

Ids: building id = `H(lot, "building")` in the OSM id space (2^40 offset as today). Part *k* is its own `Building` feature with id = building id + *k* × 2^56; only the main part is `owned` and carries the light.

## 8. Caching and budgets

| Level | Cache | Budget (release, Apple arm64) | Memory |
|---|---|---|---|
| City plan (1, major roads, 3a zoning) | Per city, session (`Arc<OnceLock>`) | < 40 ms for an 8 km radius city | ≤ 2 MB |
| Superblock plan (a) (local streets, 3b) | LRU, `seed.city.plan_cache` | < 1 ms p50, < 3 ms max | ≤ 60 KB |
| Superblock plan (b) (4) | Same entry, on demand | < 2 ms p50, < 6 ms max | ≤ 150 KB |
| Tile assembly | — | generator 3 or better at R3 (no stage b) | — |

The paper's frontage (5-20 m against generator 3's 10-120 m) means about three to five times more buildings. R2 keeps one box per building; the R0 / R1 triangle cost is measured per superblock in the gallery metrics (section 9) and checked against the band budgets before C6 ends. If it is over, the first lever is R1 = LoD1 + roofs only on the main part, the second a per-block merge of row houses into one mass at R1.

Stage hashes: each stage output has a content hash; goldens pin them per stage, so a stage 4 rule change shows as "stage 4 changed, stages 1-3 identical". Cache keys stay `(manifest hash, seed profile hash, city id, superblock id)`.

## 9. Inspection tools (the paper's figures and table)

- **`simworld-cli seed preview <world.json> --city <id> --layers footprint,constraints,major,zones,local,blocks,lots,buildings -o out.svg`**: any subset of the stage outputs as SVG layers. `--extent <lon,lat,km>` crops and builds only the superblocks inside.
- **`simworld-cli seed gallery -o dir/`**: the pilot fixture. A built-in world of three 2 km² test cities on flat land (no water, no airport), one per paper morphology: organic, grid, radial; each in `default` and `dense`. Outputs:
  - **Fig. 2 analogue:** per case three SVGs, full layout / streets only / lots only, plus one sheet with the 3 × 3 grid (morphology × view).
  - **Fig. 3 analogue:** one lot of each zone drawn at LoD0, LoD1, LoD2, LoD3 (SVG elevations; LoD3 shows the fenestration rectangles).
  - **Table 2 analogue:** `gallery.json` and `gallery.md` with, per case and scenario: build time (ms, per stage), peak memory (bytes), CPU time, output size (bytes of the meshed tiles), polygons (triangles at R0 and R2), and counts: major / local segments, superblocks, blocks, lots, buildings, parts; per morphology: mean block area, junction degree histogram (T / X / Y share), dead-end share, mean frontage, and the plausibility indicators of section 14.
- **Figs. 4-6 analogue:** bench shots of the gallery cities in 3D (`--world=` the gallery world, `--seed-spot` per city, day and night), taken at the end of each phase.
- **Pilot refinement cycle** (the paper's step list): change a rule → `seed gallery` (small extents, seconds) → compare figures and table with the last run → adjust parameters → only then the two-cities world and bench shots. Every phase of section 13 ends with this cycle recorded.
- **Bench:** the F1 `seed:` line adds superblock, zone and lot counts.

## 10. City file and edits (amends `spec-world-seed.md` 4.2 and 8a)

```json
"layout": {
  "morphology": "radial",
  "core": { "morphology": "organic", "radius_km": 1.0 },
  "rotation_deg": null,
  "spokes": 8
},
"scenario": "default",
"guides": [
  { "id": "laisves", "kind": "axis", "level": "primary", "points": [[23.900, 54.897], [23.930, 54.899]] },
  { "id": "wall",    "kind": "edge", "points": [[...], [...], [...]] }
],
"zoning": [
  { "zone": "commercial", "at": [25.270, 54.690], "area_km2": 2.0, "max_height_m": 120 },
  { "zone": "industrial", "near": ["river"], "share": 0.12 },
  { "zone": "residential", "within_km": [2, 6], "share": 0.4, "scenario": "dense" },
  { "zone": "park", "near": ["river"], "area_km2": 1.5 }
],
"fill": "residential"
```

- `layout.morphology`: `organic` | `grid` | `radial`. Morphology keys of 3.3 may be overridden here (`spokes`, `block_m`, `ring_spacing_m`, ...).
- `layout.core`: optional organic (or other) core, a disc or `"edge": "<guide id>"`.
- `guides`: optional (2.4). Ids `[a-z0-9_-]+`; a guide crossing a runway strip, or leaving the footprint by more than 1 km, is a validation error.
- `scenario`: `default` or `dense`; a zoning rule may override it.
- `zoning`: the old `districts` list, renamed; requests in file order (5.4). Keys: `zone`, `share` | `area_km2`, `within_km`, `at`, `near`, `max_height_m`, `scenario`, `keep`.
- **Generator 3 files**: `layout.type: "old_center_hex"` loads as `{morphology: radial, core: {organic, 1.0 km}}`; `cell_size_m` is ignored; `districts` loads as `zoning` with the styles mapped (`old_town` → old_town, `business` → commercial, `microdistrict` / `houses` → residential, `industrial` / `port` → industrial / port, `park` → park) and `cells: n` converted to `area_km2` = n × the old cell area; `rings` become `within_km` = ring × old cell size. Each is logged once as deprecated. The two shipped test cities are rewritten in C3.

Edits (`spec-world-seed.md` 8a.1):

- `cells` (axial `q,r`) becomes **`zones`**: `[{ "at": [lon, lat], "radius_m": 300, "zone": "park" }]` or `{ "polygon": [...], "zone": ... }`. Applied in 5.4 before the requests: superblocks whose centroid lies inside take the zone. `cells` edits are rejected with a message naming `zones` (no shipped edits file exists).
- Building edits are keyed by building id = `H(lot, ...)`, so stage 4 changes never orphan an edit. Changes in stages 1-3 can; re-matching by anchor (`spec-world-seed.md` 8a.3) is unchanged, with the lot anchor.
- The 8a.3 table gains: "a stage 4 parameter (shape rules, roofs, fenestration, setbacks) → none".

## 11. Profile (amends `spec-world-seed.md` 9.2)

Grouped by stage:

- `seed.context`: `cell_m` (50), `centrality_km` (2.5), `edge_noise` (0.12), `near_m` (800), `min_piece_km2`, `min_piece_share`.
- `seed.network.common`: `snap_m`, `extend_m`, `min_angle_deg` (30), `min_seg_m` (10), `max_seg_m` (100), `prune_m`, `steer_deg`, `max_turn_deg`, `bridge_max_m`, `bridge_spacing_m`, `goal_blend_m`, `max_segments`, `max_superblock_m2`, `min_superblock_m2`, `max_slope`, `sidewalk_m` (2).
- `seed.network.levels`: per level `primary` / `secondary` / `local` / `path`: right-of-way width (15 / 12 / 8 / 3), cross-section (`[sidewalk, carriageway, sidewalk]` or the `dual` form with a median, 3.6), `dual_for` (which morphology roads get the boulevard form), `kerb_radius_m`, ground class, `lit` share.
- `seed.network.objects`: `roundabout_m` (25), `square_m` (30), `continue_deg` (30, road chaining).
- `seed.network.organic | grid | radial`: the rule parameters of 3.3 (`linear` in C8).
- `seed.zoning`: `park_share`, `park_spacing_m`, `approach_penalty`, `approach_tall_m`, `max_slope`.
- `seed.zones.<zone>`: `streets` (local-rule adjustments), `lots` (production, `frontage_m`, `depth_m`, `min_lot_m2`, `keep`), `buildings` (`storeys`, `coverage`, `setback_m`, `side_gap_m`, `kinds`, `shapes` with weights, `roofs` with weights and pitch range, `fenestration`), `landuse` (ground class), `max_height_m`.
- `seed.floor_m` (3).
- `seed.scenarios.dense`: per-key overrides or multipliers applied after the zone values: segment length 0.6, `block_m` 0.72 (125 → 90), frontage and depth ranges as in 5.2, storeys as in 7.2, `branch_p` × 1.3, `keep` 1.0. It is the paper's dense scenario: shorter segments, more branching, smaller lots, more floors.

### 11.1 Parameter ranges against the paper

| Parameter | Paper (section 3.10) | Default here | Dense here |
|---|---|---|---|
| Branch angle | 30-90° | organic 30-90° off the continuation; grid 90°; radial spoke / ring ≈ 90°; `min_angle_deg` 30 | same |
| Segment length | 10-100 m | major 40-100 m, local 10-40 m (organic); grid one segment per block pitch | major 25-60 m, local 10-25 m |
| Road width | 8-15 m | primary 15, secondary 12, local 8 m (right of way, sidewalks included) | same |
| Block size | 25 × 25 to 125 × 125 m | grid 125 m; radial wedges 60-125 m; organic 25-80 m | grid 90 m; radial 40-90 m; organic 25-60 m |
| Lot frontage | 5-20 m | 10-20 m | 5-12 m |
| Floor height | 3 m | 3 m | 3 m |
| Height | 3-12 storeys | per zone, 3-12 (7.2) | per zone, shifted up |
| Roof pitch | 15-45° | 15-45° (gable / hip) | same |
| Fenestration | 25-50 % | 25-50 % by zone (industrial 10-20 %) | same |

Every default is inside the paper's range except two stated exceptions: industrial halls (1-3 storeys, fenestration 10-20 %) and a city's explicit `max_height_m` above 12 storeys for a business skyline. Generator 3's larger Baltic values (300-450 m microdistrict superblocks, 50-120 m frontages) are gone; a Soviet-era microdistrict can still be made with a `residential` rule that sets `shapes: slab` and larger lots in the city file, as an author choice.

Generator 3 keys that disappear: `seed.city.cell_size_m` and every hex key, `arterial_drop`, `edge_warp`, `max_district_m2`, the radial walk keys (`radial_step_m`, `radial_turn_deg`), `access_max_km` (gate seeds), and `seed.styles.*` (replaced by `seed.zones.*`). `plan_cache`, `clearance_m` and `landmark_height_m` move to the groups above.

## 12. Towns and villages (beyond the paper)

`docs/decision-library-direction.md` (finding 7, item 11) asks for roadside autogen for towns and villages. A **settlement** is a city plan with a fourth morphology, `linear`: major roads along a spine (an inter-city road, a river bank or the coast, offset by `spine_offset_m`), local streets perpendicular every block pitch to `ribbon_depth_m` on each side; zoning one `residential` zone with `row` / `box` houses and gable roofs, plus a small centre above a population threshold. Stages 3-4 are unchanged. The settlement file format belongs to the country-layer design. This spec only requires that stage 1 accept external road lines as seeds and that `linear` exist (phase C8).

## 13. Phases

| Phase | Content | Output change | Visible result |
|---|---|---|---|
| C0 | Inspection on generator 3: `seed preview --layers`, `seed gallery` (with `old_center_hex` only), metrics table. Baseline numbers | none | Decomposed previews and the metrics table for today's cities |
| C1 | Code split by stage (`seed::city::{context, network, zoning, lots, buildings}`) with typed hand-offs and stage hashes, generator 3 behind them | none (goldens identical) | — |
| C2 | Stage 1: footprint outline, constraint map, centrality, seeds (centre, gates), guides, new city file keys and the generator 3 file mapping | none until C3 | `--layers footprint,constraints` |
| C3 | Stage 2 major growth with the three morphologies and the core; faces, cap, merge, superblocks; zoning on superblocks with parks; **hex code deleted**; superblocks filled by the generator 3 lattice for now. **`SEED_GEN_VERSION` 4**, goldens regenerated, the two test cities rewritten | yes | No honeycomb; organic / grid / radial in the gallery (Fig. 2 streets row) |
| C4 | Local streets per morphology and zone (organic lanes, grid blocks, radial wedges, park paths); road objects (3.6): roads, sections, intersection polygons with classes, roundabouts, squares, way surfaces at R0 | yes | Fig. 2 full layout row; boulevards, places, clean junctions |
| C5 | Blocks by offset, `frontage` / `obb` lots, attributes, anchors; R3 lights from lots | yes | Fig. 2 lots row; R3 cost down |
| C6 | Shape rules, parts, LoD1 at R2, storeys, dense scenario | yes | Table 2 analogue complete |
| C6b | Mesher roof types (gable / hip / shed, OSM `roof:shape` too), LoD2 | yes (real OSM roofs too) | Pitched old towns and houses (Fig. 3 LoD2) |
| C7 | Fenestration channel (minor api bump), day windows in the shader, LoD3 at R0 | yes | Fig. 3 LoD3; facades read by day |
| C8 | `linear`, road-entry seeds (with `spec-world-seed.md` S2b), settlements | yes | Entering roads as primaries; a ribbon town |

C3-C7 bump `SEED_GEN_VERSION` once per release if they land together. Each phase ends with the pilot cycle of section 9: gallery figures and table against the previous phase, then bench shots at the `vilnius` / `kaunas` / `cruise` spots (`spec-world-seed.md` 11a.4).

## 14. Tests

In `simworld-core` (extending `seed::city_tests`):

- **Determinism.** Each stage hash is identical across two builds, thread counts, and city order in the manifest. The gallery world is pinned by goldens per stage, morphology and scenario.
- **Locality.** A superblock plan built alone equals the same plan built after its neighbours. Changing a zone's stage 4 keys leaves stages 1-3 hashes identical. Editing city A changes nothing outside A.
- **No cells.** Footprint, superblock and zone boundaries share no orientation histogram peak at 60° multiples beyond what a random network has (a guard against a hex layout coming back through a default), and no code path reads a cell index.
- **Hard rules.** No road (except bridges), block, lot or part in water, fences, strips or landmark clearances (exact tests). No part, ridge included, above the approach cap. Bridges respect length and spacing.
- **Network.** The major graph is connected per land mass; every superblock within the size limits; no junction angle below `min_angle_deg`; no dead end shorter than `prune_m`; entry and gate seeds end on the network.
- **Paper ranges.** On the gallery, per scenario: branch angles within 30-90° (organic), segment lengths within 10-100 m, block sizes and lot frontages within the ranges of 11.1 (with the documented corner and merge exceptions), storeys within the zone range, roof pitches within 15-45°, fenestration within the zone range.
- **Morphology metrics.** organic: T share > 0.5, mean curvature above grid's; grid: X share > 0.6 and ≥ 90 % of headings within 3° of the two axes, block side within 5 % of `block_m`; radial: every primary spoke within 5° of its bearing from the centre, ring radii within 2 % of `r_k`.
- **Plausibility indicators** (the paper's radial checks, reported for all morphologies, asserted where stated): ring continuity ≥ 0.6 on flat gallery land (share of each ring's length present after cuts); ring spacing within 10 % of `ring_spacing_m`; angular separation of neighbouring spokes ≤ `max_spoke_gap_m` at every ring; connectivity (every superblock reachable from the centre over the major graph); **lot alignment**: ≥ 85 % of lots have their frontage edge within 10° of the street they face.
- **Road objects.** Every edge belongs to exactly one section; every node of degree ≥ 3 has one intersection polygon and every section ends on it (no overlap of section and intersection surfaces beyond 1 cm²); a section's way surfaces tile its area with no gap and no overlap; predecessor / successor links are symmetric; the intersection classes agree with the node degrees; a radial gallery city has a roundabout at its centre.
- **Zoning.** Every superblock has exactly one zone; request shares within one superblock of the target; park superblocks at least `park_spacing_m` apart; zone borders lie on roads.
- **Lots and buildings.** Every kept lot fronts a street, the anchor is inside it; every part inside its lot; the main part contains the anchor with eaves at `envelope_m`; coverage within ±10 % or the `box` fallback; one owner per building per zoom; the R3 light equals the R1 light of that building.
- **Kinds.** Every kind resolves through the default profile to its intended light profile and ground class.
- **Budgets.** `cargo bench` per level against section 8 on the two-cities world and the gallery.
- **Generator 3 files.** `old_center_hex` files load, warn once and produce a radial city with an organic core.

The bench `--seed-selftest` S2-S4 group (`spec-world-seed.md` 11a.3) is unchanged in intent and runs on generator 4 output.

## 15. Risks and open decisions

| Item | Risk | Plan |
|---|---|---|
| Triangle count | Paper-sized lots give 3-5 × more buildings than generator 3, plus roofs | Measured per superblock in the gallery (C6); R2 one flat box per building; fallbacks in section 8 before relaxing the ranges |
| Growth order sensitivity | A small parameter change re-grows the major network, superblock ids change, building edits orphan | Edits re-match by lot anchor; "layout first, buildings second" (`spec-world-seed.md` 8a.2); stage 4 changes never orphan |
| Planar graph robustness | f64 intersections, near-collinear segments, slivers | 1 mm node quantization, exact predicates with ties by id, `min_angle_deg`, `min_seg_m`; a fuzz test for planarity and face closure |
| City plan cost | Growth over a 30 km radius city (schema maximum) | `max_segments` with a warning; measured in C3 |
| Offset polygons | Mitred insets of concave blocks self-intersect | Clip by the original polygon, drop slivers under `min_lot_m2`; library choice in C5 under the determinism rules (no platform trig) |
| Roof meshing | Hip / gable on non-rectangular parts | Only parts whose OBB fill is ≥ 0.85 get a pitched roof, others flat; shared with OSM `roof:shape` |
| Fenestration channel | A free vertex channel may not exist without a contract change | Minor api bump in C7; until then the shader's fixed ratio |
| Wider roads | 15 m seed primaries against 10 m OSM primaries where a seed city meets real data | The drawn carriageway is 10 m, the rest is sidewalk (3.6); the S5 switch compares at the city edge |
| Intersection polygons | Convex hulls of skewed junctions (organic) can swallow a short neighbouring section | A section shorter than its two intersections' overlap merges the two intersections into one (class `multi`); fuzz-tested with the graph |
| Vegetation | Parks and medians read as flat green from low altitude | Later, outside this spec: instanced trees of 3-4 model categories with a per-instance scale (the Snap4City approach), placed from park lots and medians by hash; a host draw-call cost to measure first |

## 16. Sources

- Shariatpour et al. (2026), "Procedural generation of urban morphologies…", *Computational Urban Science* 6:31: four-stage framework (Fig. 1), organic / grid / radial cases decomposed (Fig. 2), LoD0-3 (Fig. 3), 3D models with park superblocks (Figs. 4-6), parameter ranges (section 3.10, table S1), metrics of default / dense / traditional (Table 2), plausibility indicators of the radial case.
- Beil, Ruhdorfer, Coduro, Kolbe (2020), "Detailed Streetspace Modelling for Multiple Applications: Discussions on the Proposed CityGML 3.0 Transportation Model", *ISPRS International Journal of Geo-Information* 9(10):603, doi 10.3390/ijgi9100603; and the Road2CityGML3 guideline (TUM, 2023, https://tum-gis.github.io/road2citygml3/): roads, sections, intersections, squares, granularity, function codes (section 3.6). The MDPI page refused automated access; the content was read from the guideline, which presents the same concepts.
- Adreani, Bellini, Fanfani, Nesi, Pantaleo (2024), "Smart City Digital Twin Framework for Real-Time Multi-Data Integration and Wide Public Distribution", *IEEE Access*, doi 10.1109/ACCESS.2024.3406795 (IEEE Xplore 10540577; read from arXiv 2309.13394): eave-height extrusion and roof primitives, one tile per building by centroid, tiled DTM with Martini, regrouping fixed-zoom tiles for wide views, instanced vegetation.
- Parish, Müller (2001), "Procedural Modeling of Cities", SIGGRAPH: road growth with global goals and local constraints.
- Vanegas et al. (2012), "Procedural Generation of Parcels in Urban Modeling": OBB and frontage lot subdivision.
- Wonka et al. (2003), "Instant Architecture"; Müller et al. (2006), "Procedural Modeling of Buildings" (CGA shape): shape grammars.
- Kelly, McCabe (2006), "A Survey of Procedural Techniques for City Generation".
