//! Golden hashes on the synthetic relief (terrain phase 1, api 1.4) and of the terrain mesh layer
//! (api 1.5).

use super::*;

/// The terrain golden setup (docs/spec-terrain.md section 11): terrain active (amplitude 120 m,
/// no flat zone so the golden tiles around the origin carry relief), default profile otherwise.
fn golden_terrain() -> (Profile, Terrain) {
    let p = crate::profile::resolve(
        None,
        Some(&serde_json::json!({ "terrain": { "enabled": true, "amplitude_m": 120.0, "flat_radius_m": 0.0, "flat_blend_m": 0.0 } })),
        &crate::NullLog,
    )
    .unwrap();
    let t = Terrain::from_profile(&p, VILNIUS.0, VILNIUS.1);
    (p, t)
}

/// Golden values for `golden_keys()` on the synthetic relief (terrain phase 1, api 1.4; regenerated
/// for Tiles and LOD v2 step T2c, the data-tile path as for `GOLDEN`; before that regenerated
/// 2026-10-28, W0, for the api 1.7 building and drape work of 52105cf that had never been pinned:
/// bases on the highest ground under the footprint, per-vertex wall bottoms with foundations,
/// raised parts stacked on the building under them, the band-sized drape tolerance, lights on the
/// new roofs; the cover table is unchanged): foundations, draped and subdivided ground, lights.
const GOLDEN_TERRAIN: &[(&str, usize, u64)] = &[
    ("16/37370/20827", 0, 0xe5c09f268d2bdc70),
    ("16/37371/20828", 0, 0x47ea0fcd9c76d361),
    ("15/18685/10413", 1, 0x88342516ffd2c7a6),
    ("15/18686/10414", 1, 0x532456df36b4eb51),
    ("14/9342/5206", 2, 0x61903659d117bcb3),
    ("14/9343/5207", 2, 0xe74fa946c2fddcfc),
    ("13/4671/2603", 3, 0x554f1769be17565e),
    ("13/4672/2604", 3, 0x4a1e6aa3833665b3),
];

/// Golden cover on the synthetic relief for `COVER_CAMERAS`.
const GOLDEN_TERRAIN_COVER: &[([usize; 4], u64)] = &[
    ([16, 64, 116, 283], 0x235b86fbbaf4970a),
    ([24, 72, 104, 280], 0xb4ceb134fe1ee729),
    ([0, 56, 124, 282], 0x74762aaf157832a7),
    ([0, 0, 76, 274], 0x1f2d9ad2c294989e),
];

#[test]
fn golden_tile_hashes_with_terrain() {
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let (profile, terrain) = golden_terrain();
    let got: Vec<(String, usize, u64)> =
        golden_keys().into_iter().map(|(k, r)| (k.to_string(), r, tile_hash_on(k, r, frame, &terrain, &profile))).collect();
    // Terrain changes every tile; the flat goldens are untouched (golden_tile_hashes_default_profile).
    for ((k, r), (_, _, h)) in golden_keys().into_iter().zip(&got) {
        assert_ne!(*h, tile_hash(k, r, frame, &profile), "{k}: terrain had no effect");
    }
    let covers: Vec<([usize; 4], u64)> = COVER_CAMERAS
        .iter()
        .map(|&(cam, vel)| {
            let mut s = crate::scheduler::Scheduler::new(frame, false, std::sync::Arc::new(profile.clone())).with_terrain(terrain.clone());
            cover_signature(&mut s, cam, vel)
        })
        .collect();
    let table: String = got.iter().map(|(k, r, h)| format!("    (\"{k}\", {r}, 0x{h:016x}),\n")).collect();
    let ctable: String = covers.iter().map(|(c, h)| format!("    ({c:?}, 0x{h:016x}),\n")).collect();
    if std::env::var("SIMWORLD_UPDATE_GOLDEN").is_ok_and(|v| v == "1") {
        println!("const GOLDEN_TERRAIN: &[(&str, usize, u64)] = &[\n{table}];");
        println!("const GOLDEN_TERRAIN_COVER: &[([usize; 4], u64)] = &[\n{ctable}];");
        return;
    }
    let want: Vec<(String, usize, u64)> = GOLDEN_TERRAIN.iter().map(|(k, r, h)| (k.to_string(), *r, *h)).collect();
    assert_eq!(got, want, "terrain golden tile hashes changed; new table:\n{table}");
    assert_eq!(covers, GOLDEN_TERRAIN_COVER.to_vec(), "terrain golden cover changed; new table:\n{ctable}");
}

// ---- Terrain mesh layer (api 1.5) ------------------------------------------------------

/// Golden hash of one terrain tile (the worker path `build_terrain_tile`, detail 1): positions
/// to 1 mm, indices, normals (none with flat shading), height range, surface triangle count and
/// skirt depth. Terrain output is final on first delivery, so this is the settled output.
pub(crate) fn terrain_tile_hash(key: TileKey, ring: usize, frame: Frame, terrain: &Terrain, profile: &Profile) -> u64 {
    let t = crate::terrain::tin::build_terrain_tile(key, ring, frame, terrain, profile, 1.0).expect("terrain tile");
    let mut h = Fnv::new();
    h.u64(key.z as u64);
    h.u64(key.x as u64);
    h.u64(key.y as u64);
    h.u64(ring as u64);
    hash_mesh(&mut h, &t.mesh);
    h.mm(t.h_min);
    h.mm(t.h_max);
    h.u64(t.surface_triangles as u64);
    h.mm(t.skirt_m);
    h.0
}

/// Two tiles per terrain band around the origin (z14 = ring 0 .. z10 = ring 4).
pub(crate) fn terrain_golden_keys(profile: &Profile) -> Vec<(TileKey, usize)> {
    let mut v = Vec::new();
    for ring in 0..profile.terrain.bands.len() {
        let z = profile.terrain.near_zoom as u8 - ring as u8;
        let k = tile_for(VILNIUS.0, VILNIUS.1, z);
        v.push((k, ring));
        v.push((TileKey::new(z, k.x + 1, k.y + 1), ring));
    }
    v
}

/// The terrain layer's cover after one `update` at a fixed camera: tiles per terrain ring and an
/// FNV hash of the sorted keys.
pub(crate) fn terrain_cover_signature(s: &mut crate::scheduler::Scheduler, cam: [f64; 3], vel: [f64; 3]) -> (Vec<usize>, u64) {
    let mut jobs = Vec::new();
    s.update(cam, vel, std::time::Instant::now(), &mut jobs);
    let l = *s.layer();
    let mut keys: Vec<TileKey> = s.entries().map(|e| e.key).collect();
    keys.sort();
    let mut counts = vec![0usize; l.rings()];
    let mut h = Fnv::new();
    for k in &keys {
        counts[l.ring_for_zoom(k.z).expect("layer zoom")] += 1;
        h.u64(k.z as u64);
        h.u64(k.x as u64);
        h.u64(k.y as u64);
    }
    (counts, h.0)
}

/// Golden TIN hashes for `terrain_golden_keys()` on the golden relief (`golden_terrain()`).
/// Regenerated 2026-10-28 (W0) for the intended change "the TIN is sunk under the height function
/// at every lattice sample" (api 1.7, contract 9a, `Delatin::vertex_sinks`): same triangles,
/// lower vertex heights; the layer cover is unchanged. Regenerated again (perf review 2 item 3)
/// for "bands T2..T4 build their TIN from a 128-cell height patch" (`terrain.bands[].patch_cells`):
/// the T2..T4 rows changed, T0 / T1 and the layer cover did not.
const GOLDEN_TERRAIN_MESH: &[(&str, usize, u64)] = &[
    ("14/9342/5206", 0, 0x831052e5382e64f9),
    ("14/9343/5207", 0, 0x3fca0ce2d2f3c3b7),
    ("13/4671/2603", 1, 0xf6a4fd69438511a5),
    ("13/4672/2604", 1, 0xb79cfbdf74e7dafc),
    ("12/2335/1301", 2, 0xc9aece0b8b22073a),
    ("12/2336/1302", 2, 0xcacdf9a99a0ae78e),
    ("11/1167/650", 3, 0x33b4632322acd229),
    ("11/1168/651", 3, 0xb1767289cc53351c),
    ("10/583/325", 4, 0x72c8ba309806ce72),
    ("10/584/326", 4, 0xc0baa5bdbc4259b0),
];

/// Golden terrain-layer cover for `COVER_CAMERAS` on the golden relief, then a tall-peak relief
/// (amplitude 1200 m, ridges, peaks near 2 km) seen from 3000 m AGL. Regenerated 2026-10-28
/// (Terrain v2 G2, api 1.10) for the intended change "the terrain cover refines by screen-space
/// error" (`terrain.tau_px`, error proxy without edit floors, foundation guard, 2:1 balance): the low golden
/// relief (120 m) holds far fewer tiles, the tall peaks far more; the TIN hashes above are
/// unchanged.
const GOLDEN_TERRAIN_LAYER_COVER: &[(&[usize], u64)] = &[
    (&[8, 20, 20, 48, 101], 0x2df0d8c81d2e9aa9),
    (&[8, 16, 16, 48, 100], 0xbee2ccd02070c305),
    (&[0, 12, 20, 48, 130], 0xc365cf6aa4595fc6),
    (&[0, 0, 16, 48, 128], 0x0b994d3c0d1b91ad),
    (&[516, 736, 908, 480, 128], 0xbd2b2be67d839825),
    // Added for review M1: the peaks above under the default `terrain.max_tiles` (1500 leaves).
    (&[332, 500, 600, 396, 128], 0xe75464ccb6e91af9),
];

#[test]
fn golden_terrain_mesh_and_layer_cover() {
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let (profile, terrain) = golden_terrain();
    let keys = terrain_golden_keys(&profile);
    let got: Vec<(String, usize, u64)> =
        keys.iter().map(|&(k, r)| (k.to_string(), r, terrain_tile_hash(k, r, frame, &terrain, &profile))).collect();
    // Deterministic: a second build (another thread) gives the same hash.
    let (k, r) = keys[2];
    let (p2, t2) = (profile.clone(), terrain.clone());
    let again = std::thread::spawn(move || terrain_tile_hash(k, r, frame, &t2, &p2)).join().unwrap();
    assert_eq!(again, got[2].2);
    let peak_profile = crate::profile::resolve(
        None,
        Some(&serde_json::json!({ "terrain": { "enabled": true, "amplitude_m": 1200.0, "ridge_share": 1.0, "flat_radius_m": 0.0, "flat_blend_m": 0.0, "max_tiles": 0 } })),
        &crate::NullLog,
    )
    .unwrap();
    let peak = Terrain::from_profile(&peak_profile, VILNIUS.0, VILNIUS.1);
    // The same peaks under the default tile budget (`terrain.max_tiles`, review M1); the row
    // above pins the uncapped plan (the ordered plan is the old set when the budget is off).
    let mut capped_json = peak_profile.world().clone();
    capped_json.terrain.max_tiles = crate::profile::WorldProfile::default().terrain.max_tiles;
    let capped_profile = std::sync::Arc::new(crate::profile::Profile::new(capped_json).unwrap());
    let arc = std::sync::Arc::new(profile.clone());
    let mut covers: Vec<(Vec<usize>, u64)> = COVER_CAMERAS
        .iter()
        .map(|&(cam, vel)| {
            let mut s = crate::scheduler::Scheduler::new_terrain(frame, arc.clone(), terrain.clone());
            terrain_cover_signature(&mut s, cam, vel)
        })
        .collect();
    let mut s = crate::scheduler::Scheduler::new_terrain(frame, std::sync::Arc::new(peak_profile), peak.clone());
    covers.push(terrain_cover_signature(&mut s, [0.0, 0.0, 3000.0], [0.0; 3]));
    let mut s = crate::scheduler::Scheduler::new_terrain(frame, capped_profile, peak.clone());
    covers.push(terrain_cover_signature(&mut s, [0.0, 0.0, 3000.0], [0.0; 3]));
    let table: String = got.iter().map(|(k, r, h)| format!("    (\"{k}\", {r}, 0x{h:016x}),\n")).collect();
    let ctable: String = covers.iter().map(|(c, h)| format!("    (&{c:?}, 0x{h:016x}),\n")).collect();
    if std::env::var("SIMWORLD_UPDATE_GOLDEN").is_ok_and(|v| v == "1") {
        println!("const GOLDEN_TERRAIN_MESH: &[(&str, usize, u64)] = &[\n{table}];");
        println!("const GOLDEN_TERRAIN_LAYER_COVER: &[(&[usize], u64)] = &[\n{ctable}];");
        return;
    }
    let want: Vec<(String, usize, u64)> = GOLDEN_TERRAIN_MESH.iter().map(|(k, r, h)| (k.to_string(), *r, *h)).collect();
    assert_eq!(got, want, "terrain mesh golden hashes changed; new table:\n{table}");
    let want_c: Vec<(Vec<usize>, u64)> = GOLDEN_TERRAIN_LAYER_COVER.iter().map(|(c, h)| (c.to_vec(), *h)).collect();
    assert_eq!(covers, want_c, "terrain layer golden cover changed; new table:\n{ctable}");
}
