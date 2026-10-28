//! Determinism / golden hashes (spec-library-packaging.md section 7, audit A16).
//!
//! Pins the output of the real tile build path (`worker::build_tile` on the synthetic city) for
//! a fixed set of tile keys covering all 4 bands, with ground on the R0-R2 tiles. Any change to
//! decoding, meshing, ground, lights or their tuning shows up here as a changed hash.
//!
//! Cross-platform rules (f32/f64 results can differ in the last bits between arm64 and x86_64):
//! positions, UVs and light positions are quantized to 1 mm, colors and normals to 8 bits, and
//! per-building output is hashed per building, sorted by (OSM id, type), so neither the merged
//! buffer order nor index offsets matter. The ground mesh is hashed in output order (its
//! features are sorted deterministically by `GroundFeatures::sort`). The hash is FNV-1a 64 over
//! little-endian integers (stable across Rust versions, unlike `DefaultHasher`).
//!
//! Terrain mesh layer (api 1.5): `golden_terrain_mesh_and_layer_cover` pins the TIN of two tiles
//! per terrain band and the terrain layer's cover (incl. a tall-peak camera).
//!
//! World seed (api 1.7): `golden_seed_world_tiles` pins seed tiles of the two-cities world
//! (`low-poly-map/world/two-cities/`) in every band and three seed terrain tiles with COLOR; a
//! change of those files changes these hashes too.
//!
//! Regenerate after an *intended* output change:
//! `SIMWORLD_UPDATE_GOLDEN=1 cargo test -p simworld-core golden -- --nocapture`
//! prints the new table instead of failing; paste it into `GOLDEN` and say why in the commit.
//! (The variable is read only here, in test code; the library itself reads no environment.)

use crate::geo::{tile_for, Frame, TileKey};
use crate::mesher::{mesh_buildings, ClipRect, MeshData};
use crate::profile::Profile;
use crate::terrain::Terrain;
use crate::worker::{build_tile_on, decode_payload_on, Payload};

const VILNIUS: (f64, f64) = (54.6872, 25.2797);

/// FNV-1a 64.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 = (self.0 ^ x as u64).wrapping_mul(0x100_0000_01b3);
        }
    }
    fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.bytes(&v.to_le_bytes());
    }
    /// 1 mm.
    fn mm(&mut self, v: f32) {
        self.i64((v as f64 * 1000.0).round() as i64);
    }
    /// 8 bits of a 0..1 value (normals: -1..1 also fine, the sign is kept).
    fn q8(&mut self, v: f32) {
        self.i64((v as f64 * 255.0).round() as i64);
    }
}

/// Positions and UVs to 1 mm, normals and colors to 8 bits, indices as is.
fn hash_mesh(h: &mut Fnv, m: &MeshData) {
    h.u64(m.positions.len() as u64);
    for p in &m.positions {
        p.iter().for_each(|&v| h.mm(v));
    }
    for n in &m.normals {
        n.iter().for_each(|&v| h.q8(v));
    }
    for c in &m.colors {
        c.iter().for_each(|&v| h.q8(v));
    }
    h.u64(m.uvs.len() as u64);
    for uv in &m.uvs {
        uv.iter().for_each(|&v| h.mm(v));
    }
    h.u64(m.indices.len() as u64);
    for &i in &m.indices {
        h.i64(i as i64);
    }
}

fn hash_lights(h: &mut Fnv, pos: &[[f32; 3]], col: &[[f32; 4]]) {
    h.u64(pos.len() as u64);
    for (p, c) in pos.iter().zip(col) {
        p.iter().for_each(|&v| h.mm(v));
        c.iter().for_each(|&v| h.q8(v));
    }
}

/// Golden hash of one tile: buildings (per building, sorted by OSM id), ground, single and
/// clustered lights, plus a few counts.
pub(crate) fn tile_hash(key: TileKey, ring: usize, frame: Frame, profile: &Profile) -> u64 {
    tile_hash_on(key, ring, frame, &Terrain::flat(), profile)
}

/// [`tile_hash`] on `terrain` (the worker path `build_tile_on`).
pub(crate) fn tile_hash_on(key: TileKey, ring: usize, frame: Frame, terrain: &Terrain, profile: &Profile) -> u64 {
    let payload = Payload::Synthetic;
    let data = build_tile_on(key, ring, frame, terrain, &payload, profile).expect("build_tile");
    let dec = decode_payload_on(key, frame, terrain, &payload, false, false, profile).expect("decode");
    let mut h = Fnv::new();
    h.u64(key.z as u64);
    h.u64(key.x as u64);
    h.u64(key.y as u64);
    h.u64(ring as u64);
    h.u64(data.building_count as u64);
    h.mm(data.max_height);

    // Buildings: the merged mesh is `mesh_buildings` over the decoded buildings in order, so
    // meshing each building alone (same function, same rect, same LOD) yields exactly the
    // merged buffer's per-building runs. Checked below, then hashed in (id, type) order.
    let mut per_building: Vec<((u64, u64), MeshData)> = Vec::new();
    if let Some(lod) = profile.lod(ring) {
        let rect = ClipRect::tile(dec.proj.extent());
        for b in &dec.buildings {
            let m = mesh_buildings(std::slice::from_ref(b), &dec.proj, rect, lod, profile);
            if !m.is_empty() {
                per_building.push(((b.id, b.osm_type.tag()), m));
            }
        }
    }
    let (nv, ni): (usize, usize) =
        per_building.iter().fold((0, 0), |(v, i), (_, m)| (v + m.positions.len(), i + m.indices.len()));
    assert_eq!((nv, ni), (data.mesh.positions.len(), data.mesh.indices.len()), "{key}: per-building split");
    let mut base = 0usize;
    for (_, m) in &per_building {
        let n = m.positions.len();
        assert_eq!(&data.mesh.positions[base..base + n], &m.positions[..], "{key}: per-building positions");
        base += n;
    }
    per_building.sort_by_key(|(id, _)| *id);
    h.u64(per_building.len() as u64);
    for (id, m) in &per_building {
        h.u64(id.0);
        h.u64(id.1);
        hash_mesh(&mut h, m);
    }

    // Ground on every tile (R3 since api 1.3), buildings on R0-R2, lights on R1..R3 (default lights.min_ring = 1; the set
    // must exercise every output kind).
    assert!(!data.ground.is_empty(), "{key}: ground");
    assert_eq!(data.mesh.is_empty(), ring == 3, "{key}: buildings");
    assert_eq!(data.lights.positions.is_empty(), ring < 1, "{key}: lights");
    assert!(!data.lights.street_positions.is_empty(), "{key}: street lamps (default lights.street.min_ring = 0)");

    // Ground, in output order.
    hash_mesh(&mut h, &data.ground);

    // Lights: one per owned building in decode order, none for `dark` buildings -> pair with
    // the building, sort by id.
    let owned: Vec<(u64, u64)> = dec
        .buildings
        .iter()
        .filter(|b| b.owned && crate::mesher::resolve(b, profile).profile != crate::mesher::PROFILE_DARK)
        .map(|b| (b.id, b.osm_type.tag()))
        .collect();
    let l = &data.lights;
    if !l.positions.is_empty() {
        assert_eq!(owned.len(), l.positions.len(), "{key}: one light per owned non-dark building");
    }
    type Single = ((u64, u64), [f32; 3], [f32; 4]);
    let mut singles: Vec<Single> =
        l.positions.iter().zip(&l.colors).zip(&owned).map(|((p, c), id)| (*id, *p, *c)).collect();
    singles.sort_by_key(|s| s.0);
    h.u64(singles.len() as u64);
    for (id, p, c) in &singles {
        h.u64(id.0);
        h.u64(id.1);
        hash_lights(&mut h, std::slice::from_ref(p), std::slice::from_ref(c));
    }
    // Clusters are sorted by cell index already (independent of building order).
    hash_lights(&mut h, &l.clustered_positions, &l.clustered_colors);
    // Street lamps (roads sorted, so output order is deterministic) and their clusters.
    hash_lights(&mut h, &l.street_positions, &l.street_colors);
    hash_lights(&mut h, &l.clustered_street_positions, &l.clustered_street_colors);
    // Beacons on tall buildings (never clustered).
    hash_lights(&mut h, &l.beacon_positions, &l.beacon_colors);
    h.0
}

/// The golden tile set: around the Vilnius origin, two tiles per band (z16 = R0 .. z13 = R3).
pub(crate) fn golden_keys() -> Vec<(TileKey, usize)> {
    let mut v = Vec::new();
    for ring in 0..4usize {
        let z = 16 - ring as u8;
        let k = tile_for(VILNIUS.0, VILNIUS.1, z);
        v.push((k, ring));
        v.push((TileKey::new(z, k.x + 1, k.y + 1), ring));
    }
    v
}

/// Golden values for `golden_keys()` with `Profile::default()` (key, ring, hash). Last regenerated
/// for the R3 ground (api 1.3, `ground.r3`: only the two R3 hashes changed), before that for night-windows W4 (default `lights.min_ring` 2 -> 1: R1 tiles now carry lights, only the R1 hashes
/// changed), before that for night-windows W3 (api 1.0): wall UVs only (window columns / floors on windowed R0/R1
/// walls, `u = -1` + meters on windowless and all R2 walls); positions, normals, colors, indices,
/// ground and lights unchanged (R3 hashes identical). Before that for W2 (profile_version 4):
/// churches moved from civic to landmark (COLOR r and light color), and the synthetic castle /
/// tower / ruin lots in the origin's z16 tile.
const GOLDEN: &[(&str, usize, u64)] = &[
    ("16/37370/20827", 0, 0x3b765a5900178657),
    ("16/37371/20828", 0, 0x467ce64ac1254619),
    ("15/18685/10413", 1, 0xf9fda743f16d6b50),
    ("15/18686/10414", 1, 0x3afc10b1968d2be8),
    ("14/9342/5206", 2, 0x9e78d8f4a8c62bb2),
    ("14/9343/5207", 2, 0xa549392f72cf056d),
    ("13/4671/2603", 3, 0x3de346522f977b7c),
    ("13/4672/2604", 3, 0x9a24caee5797d041),
];

#[test]
fn golden_tile_hashes_default_profile() {
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let profile = Profile::default();
    let got: Vec<(String, usize, u64)> =
        golden_keys().into_iter().map(|(k, r)| (k.to_string(), r, tile_hash(k, r, frame, &profile))).collect();
    // Deterministic within a process (same input, same output).
    for (k, r) in golden_keys().into_iter().take(2) {
        assert_eq!(tile_hash(k, r, frame, &profile), tile_hash(k, r, frame, &profile));
    }
    let table: String = got.iter().map(|(k, r, h)| format!("    (\"{k}\", {r}, 0x{h:016x}),\n")).collect();
    if std::env::var("SIMWORLD_UPDATE_GOLDEN").is_ok_and(|v| v == "1") {
        println!("const GOLDEN: &[(&str, usize, u64)] = &[\n{table}];");
        return;
    }
    let want: Vec<(String, usize, u64)> = GOLDEN.iter().map(|(k, r, h)| (k.to_string(), *r, *h)).collect();
    assert_eq!(
        got, want,
        "golden tile hashes changed. If the output change is intended, regenerate with \
         SIMWORLD_UPDATE_GOLDEN=1 cargo test -p simworld-core golden -- --nocapture\nnew table:\n{table}"
    );
}

/// The set of tiles the scheduler requests (its whole cover) after one `update` at a fixed
/// camera: counts per ring and an FNV hash of the sorted keys.
pub(crate) fn cover_signature(s: &mut crate::scheduler::Scheduler, cam: [f64; 3], vel: [f64; 3]) -> ([usize; 4], u64) {
    let mut jobs = Vec::new();
    s.update(cam, vel, std::time::Instant::now(), &mut jobs);
    let mut keys: Vec<TileKey> = s.entries().map(|e| e.key).collect();
    keys.sort();
    let mut counts = [0usize; 4];
    let mut h = Fnv::new();
    for k in &keys {
        counts[16 - k.z as usize] += 1;
        h.u64(k.z as u64);
        h.u64(k.x as u64);
        h.u64(k.y as u64);
    }
    (counts, h.0)
}

/// Cameras for the cover golden: (east, north, AGL) and velocity (east, north, up).
pub(crate) const COVER_CAMERAS: [([f64; 3], [f64; 3]); 4] = [
    ([0.0, 0.0, 300.0], [0.0, 0.0, 0.0]),
    ([0.0, 0.0, 300.0], [250.0, 0.0, 0.0]),
    ([1500.0, -800.0, 1500.0], [0.0, 120.0, -10.0]),
    ([0.0, 0.0, 5000.0], [0.0, 0.0, 0.0]),
];

/// Golden cover per `COVER_CAMERAS` entry with the default profile (tiles per ring R0..R3, hash).
const GOLDEN_COVER: &[([usize; 4], u64)] = &[
    ([16, 64, 116, 283], 0x235b86fbbaf4970a),
    ([24, 72, 100, 280], 0x9d21ff3fafa4f0d9),
    ([0, 56, 124, 282], 0x74762aaf157832a7),
    ([0, 0, 76, 274], 0x1f2d9ad2c294989e),
];

#[test]
fn golden_cover_default_profile() {
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let got: Vec<([usize; 4], u64)> = COVER_CAMERAS
        .iter()
        .map(|&(cam, vel)| {
            cover_signature(&mut crate::scheduler::Scheduler::new(frame, false, std::sync::Arc::new(Profile::default())), cam, vel)
        })
        .collect();
    let table: String = got.iter().map(|(c, h)| format!("    ({c:?}, 0x{h:016x}),\n")).collect();
    if std::env::var("SIMWORLD_UPDATE_GOLDEN").is_ok_and(|v| v == "1") {
        println!("const GOLDEN_COVER: &[([usize; 4], u64)] = &[\n{table}];");
        return;
    }
    assert_eq!(got, GOLDEN_COVER.to_vec(), "golden cover changed; new table:\n{table}");
}

/// P3 acceptance: a profile override changes the cover; the default profile reproduces the
/// pinned cover (`GOLDEN_COVER`). Scheduler level (deterministic) and through `World`
/// (`configure_world` with `profile_overrides`, synthetic city).
#[test]
fn profile_override_changes_the_cover() {
    use crate::world::{World, WorldOptions};
    use std::sync::Arc;
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let (cam, vel) = COVER_CAMERAS[0]; // 300 m AGL over the origin, hovering
    let small_r0 = crate::profile::resolve(None, Some(&serde_json::json!({"lod": {"error_m": {"r1": 1.07}}})), &crate::NullLog)
        .unwrap();
    let mut s_def = crate::scheduler::Scheduler::new(frame, false, Arc::new(Profile::default()));
    let mut s_ovr = crate::scheduler::Scheduler::new(frame, false, Arc::new(small_r0));
    let def = cover_signature(&mut s_def, cam, vel);
    let ovr = cover_signature(&mut s_ovr, cam, vel);
    println!("cover at 300 m AGL, tiles per ring R0..R3: default {:?}, lod.error_m.r1 1.07 (R0 edge ~250 m) {:?}", def.0, ovr.0);
    assert_eq!(def, GOLDEN_COVER[0], "default profile = pre-P3 cover");
    assert_ne!(def.1, ovr.1);
    assert!(ovr.0[0] < def.0[0], "smaller R0 radius -> fewer z16 tiles: {:?} vs {:?}", ovr.0, def.0);
    assert!(ovr.0[0] > 0);
    assert_eq!(ovr.0[3], def.0[3], "R3 roots unchanged");

    // Same through World: the set of tracked tiles after the first update_camera.
    let tiles = |overrides: Option<serde_json::Value>| {
        let mut w = World::new(Arc::new(crate::NullLog), crate::Diagnostics::default(), "test");
        let r = w.configure_world(WorldOptions {
            origin_lat: VILNIUS.0,
            origin_lon: VILNIUS.1,
            profile_overrides: overrides,
            ..WorldOptions::default()
        });
        assert!(r.ok, "{}", r.error);
        assert!(r.attributions.is_empty(), "synthetic city: no OSM data");
        w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
        let mut per_ring = [0usize; 4];
        let mut keys = Vec::new();
        w.debug_tiles(|k, ring, _, _| {
            per_ring[ring] += 1;
            keys.push(k);
        });
        keys.sort();
        (per_ring, keys)
    };
    let (d_rings, d_keys) = tiles(None);
    let (o_rings, o_keys) = tiles(Some(serde_json::json!({"lod": {"error_m": {"r1": 1.07}}})));
    assert_eq!(d_rings, def.0, "World cover = scheduler cover (default)");
    assert_eq!(o_rings, ovr.0, "World cover = scheduler cover (override)");
    assert_ne!(d_keys, o_keys);
}

/// `configure_world` validates before it touches anything; `configure` stays a wrapper.
#[test]
fn configure_world_rejects_bad_options_and_keeps_running() {
    use crate::world::{World, WorldOptions};
    use std::sync::Arc;
    let mut w = World::new(Arc::new(crate::NullLog), crate::Diagnostics::default(), "test");
    w.configure(VILNIUS.0, VILNIUS.1, "", "");
    w.update_camera([0.0; 3], [0.0; 3], 300.0);
    let before = w.stats();
    let bad = |o: WorldOptions| {
        let mut o = o;
        if o.origin_lat == 0.0 && o.origin_lon == 0.0 {
            o.origin_lat = VILNIUS.0;
            o.origin_lon = VILNIUS.1;
        }
        o
    };
    for (o, needle) in [
        (bad(WorldOptions { profile_overrides: Some(serde_json::json!({"bands": {"r0_edge": 1}})), ..Default::default() }), "unknown key `bands.r0_edge`"),
        (bad(WorldOptions { profile_overrides: Some(serde_json::json!({"fades": {"cap_s": 0}})), ..Default::default() }), "fades.cap_s = 0 is out of range"),
        (WorldOptions { origin_lat: 91.0, origin_lon: 0.0, ..Default::default() }, "origin_lat"),
        (bad(WorldOptions { tile_url: "file:///x.pmtiles".into(), ..Default::default() }), "http"),
        (bad(WorldOptions { profile_path: Some("/nonexistent/p.json".into()), ..Default::default() }), "/nonexistent/p.json"),
    ] {
        let r = w.configure_world(o);
        assert!(!r.ok && r.error.contains(needle), "{needle}: {}", r.error);
    }
    // Still the first configuration (same scheduler, same tiles).
    assert_eq!(w.stats().queued, before.queued);
    assert_eq!(**w.profile(), Profile::default());
    // A good one with a real URL reports the OSM attribution (no network needed to configure).
    let r = w.configure_world(WorldOptions {
        origin_lat: VILNIUS.0,
        origin_lon: VILNIUS.1,
        tile_url: "http://127.0.0.1:9/none.pmtiles".into(),
        profile_overrides: Some(serde_json::json!({"origin": {"recenter_distance_m": 2000}})),
        ..Default::default()
    });
    assert!(r.ok, "{}", r.error);
    assert_eq!(r.attributions, vec![crate::OSM_ATTRIBUTION.to_string()]);
    assert_eq!(w.profile().origin.recenter_distance_m, 2000.0);
    // The profile drives the floating origin.
    w.update_camera([2500.0, 0.0, 0.0], [0.0; 3], 300.0);
    assert_eq!(w.take_origin_shift(), [2500.0, 0.0, 0.0]);
    // configure() resets to the default profile.
    w.configure(VILNIUS.0, VILNIUS.1, "", "");
    assert_eq!(**w.profile(), Profile::default());
    assert!(w.attributions().is_empty());
}

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
/// 2026-10-28, W0, for the api 1.7 building and drape work of 52105cf that had never been pinned:
/// bases on the highest ground under the footprint, per-vertex wall bottoms with foundations,
/// raised parts stacked on the building under them, the band-sized drape tolerance, lights on the
/// new roofs; the cover table is unchanged): foundations, draped and subdivided ground, lights.
const GOLDEN_TERRAIN: &[(&str, usize, u64)] = &[
    ("16/37370/20827", 0, 0x155ac2db53c2d852),
    ("16/37371/20828", 0, 0x1817e6555a7f7be5),
    ("15/18685/10413", 1, 0x58c1ba0a83f9984e),
    ("15/18686/10414", 1, 0xac344ce7b5b058c4),
    ("14/9342/5206", 2, 0xe150c1114f7c81f7),
    ("14/9343/5207", 2, 0x96f51734e1402bb1),
    ("13/4671/2603", 3, 0x60cd6e1fa6ae196c),
    ("13/4672/2604", 3, 0xe6c37ba8ea77bb5f),
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
/// lower vertex heights; the layer cover is unchanged.
const GOLDEN_TERRAIN_MESH: &[(&str, usize, u64)] = &[
    ("14/9342/5206", 0, 0x831052e5382e64f9),
    ("14/9343/5207", 0, 0x3fca0ce2d2f3c3b7),
    ("13/4671/2603", 1, 0xf6a4fd69438511a5),
    ("13/4672/2604", 1, 0xb79cfbdf74e7dafc),
    ("12/2335/1301", 2, 0x8c619113d321f45c),
    ("12/2336/1302", 2, 0x0b1a368c9acf8114),
    ("11/1167/650", 3, 0x0f4205821900a1fd),
    ("11/1168/651", 3, 0xa7fd20e71317dfb3),
    ("10/583/325", 4, 0xc1ab9933e9ed03f5),
    ("10/584/326", 4, 0x0cdbc8bd6429e05e),
];

/// Golden terrain-layer cover for `COVER_CAMERAS` on the golden relief, then a tall-peak relief
/// (amplitude 1200 m, ridges, peaks near 2 km) seen from 3000 m AGL.
const GOLDEN_TERRAIN_LAYER_COVER: &[(&[usize], u64)] = &[
    (&[28, 36, 108, 140, 101], 0x0ad674628ca4762d),
    (&[36, 36, 100, 140, 100], 0x4c9258a8f3cc96e1),
    (&[24, 44, 104, 144, 130], 0x2a26054e873e2b3e),
    (&[0, 32, 100, 140, 128], 0x606dd9c7be01c641),
    (&[4, 36, 108, 140, 128], 0x59c971a44de2df01),
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
        Some(&serde_json::json!({ "terrain": { "enabled": true, "amplitude_m": 1200.0, "ridge_share": 1.0, "flat_radius_m": 0.0, "flat_blend_m": 0.0 } })),
        &crate::NullLog,
    )
    .unwrap();
    let peak = Terrain::from_profile(&peak_profile, VILNIUS.0, VILNIUS.1);
    let arc = std::sync::Arc::new(profile.clone());
    let mut covers: Vec<(Vec<usize>, u64)> = COVER_CAMERAS
        .iter()
        .map(|&(cam, vel)| {
            let mut s = crate::scheduler::Scheduler::new_terrain(frame, arc.clone(), terrain.clone());
            terrain_cover_signature(&mut s, cam, vel)
        })
        .collect();
    let mut s = crate::scheduler::Scheduler::new_terrain(frame, std::sync::Arc::new(peak_profile), peak);
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
/// the three terrain tiles for the TIN sink with the sea and lake vertices pinned at their level).
const GOLDEN_SEED: &[(&str, usize, u64)] = &[
    ("16/37371/20844", 0, 0x3cf706f5b2f2b688),
    ("15/18685/10422", 1, 0x99c3583cb3b78c18),
    ("14/9342/5211", 2, 0xc937de90c6feb02d),
    ("13/4671/2605", 3, 0xf1dca27a4e703c5f),
    ("15/18595/10324", 1, 0x2f8342746dbc499f),
    ("13/4648/2581", 3, 0x456d6ea0837839a3),
    ("14/9152/5124", 2, 0xa057805df42b8677),
    ("15/18573/10386", 1, 0x0ec008066bc95dc3),
];
const GOLDEN_SEED_TERRAIN: &[(&str, usize, u64)] = &[
    ("14/9342/5211", 0, 0x29aab6a9ec11be0c),
    ("12/2288/1283", 2, 0x5c30c2a36b2c07bd),
    ("10/577/322", 4, 0xb3df9045bf89b761),
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
