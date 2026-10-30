//! Golden hashes of the world seed (api 1.7).

use super::terrain_golden::terrain_cover_signature;
use super::*;

// ---- World seed (api 1.7, docs/spec-world-seed.md section 12) ---------------------------

/// Golden hash of one seed tile (the worker path `build_seed_tile_on` on the two-cities world):
/// merged building mesh, ground, building / street / beacon / airport lights, counts.
fn seed_tile_hash(key: TileKey, ring: usize, frame: Frame) -> u64 {
    let (p, t, w) = crate::seed::world::test_world();
    let d = crate::worker::build_seed_tile_on(key, ring, frame, t, w, p).expect("seed tile");
    let mut h = Fnv::new();
    h.u64(key.z as u64);
    h.u64(key.x as u64);
    h.u64(key.y as u64);
    h.u64(ring as u64);
    h.u64(d.building_count as u64);
    h.mm(d.max_height);
    hash_mesh(&mut h, &d.mesh);
    hash_mesh(&mut h, &d.ground);
    hash_lights(&mut h, &d.lights.positions, &d.lights.colors);
    hash_lights(&mut h, &d.lights.street_positions, &d.lights.street_colors);
    hash_lights(&mut h, &d.lights.beacon_positions, &d.lights.beacon_colors);
    hash_lights(&mut h, &d.lights.airport_positions, &d.lights.airport_colors);
    h.0
}

/// Golden hash of a seed world's terrain tile (TIN on the seed heights + COLOR by class).
fn seed_terrain_hash(key: TileKey, ring: usize, frame: Frame) -> u64 {
    let (p, t, w) = crate::seed::world::test_world();
    let d = crate::worker::build_terrain_data(key, ring, frame, t, p, 1.0, Some(w)).expect("terrain tile");
    let tile = d.terrain.expect("terrain");
    let mut h = Fnv::new();
    h.u64(key.z as u64);
    h.u64(key.x as u64);
    h.u64(key.y as u64);
    h.u64(ring as u64);
    hash_mesh(&mut h, &tile.mesh);
    h.mm(tile.h_min);
    h.mm(tile.h_max);
    h.u64(tile.surface_triangles as u64);
    h.0
}

/// Seed tiles: EYVI in every band (runway, lights, terminal), countryside, the Klaipeda coast,
/// the Kaunas reservoir; terrain tiles over EYVI, the coast and the countryside.
fn seed_golden_keys() -> (Vec<(TileKey, usize)>, Vec<(TileKey, usize)>) {
    let eyvi = (54.6341, 25.2858);
    let mut v: Vec<(TileKey, usize)> = (0..4).map(|r| (tile_for(eyvi.0, eyvi.1, 16 - r as u8), r)).collect();
    v.push((tile_for(55.25, 24.3, 15), 1));
    v.push((tile_for(55.25, 24.3, 13), 3));
    v.push((tile_for(55.72, 21.11, 14), 2));
    v.push((tile_for(54.86, 24.05, 15), 1));
    let t = vec![(tile_for(eyvi.0, eyvi.1, 14), 0), (tile_for(55.6, 21.1, 12), 2), (tile_for(55.3, 23.0, 10), 4)];
    (v, t)
}

/// Golden values for `seed_golden_keys()` (generator version 2: S1 fixes; regenerated 2026-10-28
/// for the intended change "no drape lift inside airport fences, fences cut out of countryside and
/// water, overlapping strips share one plane, coast-resolved terrain COLOR"; regenerated again
/// 2026-10-28, W0: every vector tile for the 52105cf building / drape work (as `GOLDEN_TERRAIN`),
/// the three terrain tiles for the TIN sink with the sea and lake vertices pinned at their level;
/// regenerated 2026-10-29 for generator 3 (cities S2-S4, countryside at level 0): every vector
/// tile and the EYVI terrain tile, whose COLOR now carries Vilnius' district classes);
/// regenerated 2026-10-30 for the approach-cap fix (seed C1 review: the cap reads the mesher's
/// base rule and keeps `BASE_MARGIN_M` = 5 cm headroom for the per-tile edge split), which lowers
/// the capped buildings by 5 cm and moves `13/4671/2605` (the cities' buildings stage below too).
const GOLDEN_SEED: &[(&str, usize, u64)] = &[
    ("16/37371/20844", 0, 0x248394d192e0e4a6),
    ("15/18685/10422", 1, 0x2c4c0df28ccaccae),
    ("14/9342/5211", 2, 0xb285af8a17cbb129),
    ("13/4671/2605", 3, 0x692151ee5d17cb1c),
    ("15/18595/10324", 1, 0xc122b1a81d060a49),
    ("13/4648/2581", 3, 0x300ce454d6b94ee5),
    ("14/9152/5124", 2, 0x1ef7a6cb069822de),
    ("15/18573/10386", 1, 0xbf6ef8fe7ff530af),
];
/// Seed terrain tiles; the T2 and T4 rows regenerated for perf review 2 item 3 (128-cell height
/// patch on coarse bands; neither tile touches an airport, which keeps the zoom's lattice).
const GOLDEN_SEED_TERRAIN: &[(&str, usize, u64)] = &[
    ("14/9342/5211", 0, 0x3e0338e41001d92c),
    ("12/2288/1283", 2, 0xc7e6da76e1921356),
    ("10/577/322", 4, 0xca25f5228e785486),
];

#[test]
fn golden_seed_world_tiles() {
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let (vk, tk) = seed_golden_keys();
    let got: Vec<(String, usize, u64)> = vk.iter().map(|&(k, r)| (k.to_string(), r, seed_tile_hash(k, r, frame))).collect();
    let got_t: Vec<(String, usize, u64)> = tk.iter().map(|&(k, r)| (k.to_string(), r, seed_terrain_hash(k, r, frame))).collect();
    // Deterministic across threads.
    let (k, r) = vk[0];
    assert_eq!(std::thread::spawn(move || seed_tile_hash(k, r, frame)).join().unwrap(), got[0].2);
    let table: String = got.iter().map(|(k, r, h)| format!("    (\"{k}\", {r}, 0x{h:016x}),\n")).collect();
    let ttable: String = got_t.iter().map(|(k, r, h)| format!("    (\"{k}\", {r}, 0x{h:016x}),\n")).collect();
    if std::env::var("SIMWORLD_UPDATE_GOLDEN").is_ok_and(|v| v == "1") {
        println!("const GOLDEN_SEED: &[(&str, usize, u64)] = &[\n{table}];");
        println!("const GOLDEN_SEED_TERRAIN: &[(&str, usize, u64)] = &[\n{ttable}];");
        return;
    }
    let want: Vec<(String, usize, u64)> = GOLDEN_SEED.iter().map(|(k, r, h)| (k.to_string(), *r, *h)).collect();
    assert_eq!(got, want, "seed golden tile hashes changed; new table:\n{table}");
    let want_t: Vec<(String, usize, u64)> = GOLDEN_SEED_TERRAIN.iter().map(|(k, r, h)| (k.to_string(), *r, *h)).collect();
    assert_eq!(got_t, want_t, "seed golden terrain hashes changed; new table:\n{ttable}");
}

/// Seed terrain cover (Terrain v2 G2): the terrain layer's SSE cover of the two-cities seed
/// world after one `update` (tiles per terrain ring, hash of the sorted keys) for cameras
/// over Vilnius, at EYKA and over the Baltic coast, 300 and 3000 m AGL. A pure function of
/// camera, view and profile: two fresh schedulers give the same table.
const GOLDEN_SEED_TERRAIN_COVER: &[(&[usize], u64)] = &[
    (&[60, 72, 108, 88, 128], 0xd7dabd71fc1d0b55),
    (&[36, 60, 108, 88, 128], 0xaa81df653f1ddec5),
    (&[56, 72, 104, 96, 128], 0x712e2c3eeae11d01),
    (&[44, 72, 104, 96, 128], 0x7b1a8f387fbb0fc9),
    (&[32, 52, 64, 96, 129], 0xa4da94d2d884ddaf),
    (&[24, 52, 60, 96, 133], 0x6110cb4d863cbbf5),
];

#[test]
fn golden_seed_terrain_cover() {
    let (p, t, _) = crate::seed::world::test_world();
    let arc = std::sync::Arc::new(p.clone());
    let spots = [(54.6872, 25.2797), (54.9639, 24.0848), (55.85, 21.06)];
    let run = move || -> Vec<(Vec<usize>, u64)> {
        let mut out = Vec::new();
        for &(lat, lon) in &spots {
            for alt in [300.0, 3000.0] {
                let mut s = crate::scheduler::Scheduler::new_terrain(Frame::new(lat, lon), arc.clone(), t.clone());
                out.push(terrain_cover_signature(&mut s, [0.0, 0.0, alt], [0.0; 3]));
            }
        }
        out
    };
    let covers = run.clone()();
    assert_eq!(covers, std::thread::spawn(run).join().unwrap(), "seed terrain cover depends on the run");
    let ctable: String = covers.iter().map(|(c, h)| format!("    (&{c:?}, 0x{h:016x}),\n")).collect();
    if std::env::var("SIMWORLD_UPDATE_GOLDEN").is_ok_and(|v| v == "1") {
        println!("const GOLDEN_SEED_TERRAIN_COVER: &[(&[usize], u64)] = &[\n{ctable}];");
        return;
    }
    let want: Vec<(Vec<usize>, u64)> = GOLDEN_SEED_TERRAIN_COVER.iter().map(|(c, h)| (c.to_vec(), *h)).collect();
    assert_eq!(covers, want, "seed terrain golden cover changed; new table:\n{ctable}");
}

/// Per-stage content hashes of the generated cities (`docs/spec-seed-city-pipeline.md` section
/// 8, phase C1) on the two-cities world: per city the city-plan stages (context, zoning, major
/// network), then the district-plan stages (local network, lots, buildings) folded over every
/// land cell. A stage change shows as "this stage and the ones after it changed, the ones before
/// identical".
/// Column order: context, zoning, network, local, lots, buildings.
const GOLDEN_SEED_CITY_STAGES: &[(&str, [u64; 6])] = &[
    ("vilnius", [0x1dc17f560fca86d5, 0xe62796906102be0a, 0xf08bb39a8f38e366, 0x3ab79b1e10775a2f, 0xe7c51ba6ea0d7356, 0xdb6ee28f774c4d26]),
    ("kaunas", [0x2dcb1f340a82c9d2, 0x0f747cb3c2d5d48d, 0x181fdc078bc68c65, 0x14102aa440ad7c8b, 0x6a9951449cec9089, 0x4cae8430bfbcfee3]),
];

#[test]
fn golden_seed_city_stages() {
    use crate::seed::city::{buildings, lots, network, CellInput, CityPlan, StageHash};
    let w = &crate::seed::world::test_world().2;
    let mut got: Vec<(String, [u64; 6])> = Vec::new();
    for (i, city) in w.cities().iter().enumerate() {
        let (ctx, zoning, net) = CityPlan::stages(city, w);
        let plan = w.city_plan(i);
        let (mut hl, mut ho, mut hb) = (StageHash::new("local"), StageHash::new("lots"), StageHash::new("buildings"));
        for c in plan.cells.values() {
            let Some(inp) = CellInput::new(&plan, c.ax, w) else { continue };
            let l = network::local::build(&inp);
            let o = lots::build(&inp, &l);
            let b = buildings::build(&inp, &l, &o);
            hl.ax(c.ax).u(l.hash());
            ho.ax(c.ax).u(o.hash());
            hb.ax(c.ax).u(buildings::hash(&b));
        }
        got.push((city.id.clone(), [ctx.hash(), zoning.hash(), net.hash(), hl.finish(), ho.finish(), hb.finish()]));
    }
    let table: String = got
        .iter()
        .map(|(id, h)| format!("    (\"{id}\", [{}]),\n", h.iter().map(|x| format!("0x{x:016x}")).collect::<Vec<_>>().join(", ")))
        .collect();
    if std::env::var("SIMWORLD_UPDATE_GOLDEN").is_ok_and(|v| v == "1") {
        println!("const GOLDEN_SEED_CITY_STAGES: &[(&str, [u64; 6])] = &[\n{table}];");
        return;
    }
    let want: Vec<(String, [u64; 6])> = GOLDEN_SEED_CITY_STAGES.iter().map(|(id, h)| (id.to_string(), *h)).collect();
    assert_eq!(got, want, "seed city stage hashes changed (columns: context, zoning, network, local, lots, buildings); new table:\n{table}");
}
