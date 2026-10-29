//! Determinism / golden hashes (spec-library-packaging.md section 7, audit A16).
//!
//! Pins the output of the real tile build path for a fixed set of tile keys covering all 4
//! bands, with ground on every band. Since Tiles and LOD v2 step T2c that is the data-tile path
//! on the synthetic city: the generated detail z14 / overview z11 data tiles
//! (`data::synthetic_data_tile`) cut into chunks by `data::build_chunk_with`, landmarks folded
//! into the chunks (the default without the landmark layer). Any change to decoding, meshing,
//! ground, lights or their tuning shows up here as a changed hash.
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
use crate::data::{bbox, build_chunk_with, synthetic_data_tile, DataLevel, DataLevels, DataTile};
use crate::mesher::{mesh_building_iter, MeshData};
use crate::mvt::Building;
use crate::profile::Profile;
use crate::terrain::Terrain;

mod seed_golden;
mod terrain_golden;

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

/// [`tile_hash`] on `terrain` (the streaming path: synthetic data tiles cut into the chunk).
pub(crate) fn tile_hash_on(key: TileKey, ring: usize, frame: Frame, terrain: &Terrain, profile: &Profile) -> u64 {
    let levels = DataLevels::SYNTHETIC;
    let dk = levels.data_of(key).expect("a chunk key");
    let dt = synthetic_data_tile(dk, levels.level_of(dk), frame, terrain, profile);
    let lk = levels.landmarks_of(key).expect("synthetic landmarks");
    let lt = (lk != dk).then(|| synthetic_data_tile(lk, DataLevel::Overview, frame, terrain, profile));
    let lm: &DataTile = lt.as_ref().unwrap_or(&dt);
    let data = build_chunk_with(&dt, Some(lm), key, ring, profile, &|| false).expect("build_chunk").expect("not stopped");
    let mut h = Fnv::new();
    h.u64(key.z as u64);
    h.u64(key.x as u64);
    h.u64(key.y as u64);
    h.u64(ring as u64);
    h.u64(data.building_count as u64);
    h.mm(data.max_height);

    // Buildings: the merged mesh is the chunk's buildings (blocks) in order, then its landmarks,
    // each run meshed by `mesh_building_iter` with the chunk's projector and rectangle, so
    // meshing each building alone yields exactly the merged buffer's per-building runs. Checked
    // below, then hashed in (id, type) order.
    let proj = dt.proj.cut(key).expect("inside");
    let lproj = lm.proj.cut(key).expect("inside");
    let meets = |b: &Building, r: crate::mesher::ClipRect| {
        let bb = bbox(b);
        !(bb[2] <= r.x0 || bb[0] >= r.x1 || bb[3] <= r.y0 || bb[1] >= r.y1)
    };
    let mut per_building: Vec<((u64, u64), MeshData)> = Vec::new();
    if let Some(lod) = profile.lod(ring) {
        // The chunk's buildings with their bases resolved (open on synthetic overview blocks).
        let idx: Vec<usize> = (0..dt.buildings.len()).filter(|&i| meets(&dt.buildings[i], proj.rect())).collect();
        let own: Vec<Building> = dt.lazy_bases.based(&dt.buildings, &idx, &dt.proj).into_iter().map(|b| b.into_owned()).collect();
        for (list, p) in [(&own, &proj), (&lm.landmarks, &lproj)] {
            for b in list.iter().filter(|b| meets(b, p.rect())) {
                let m = mesh_building_iter(std::iter::once(b), p, p.rect(), lod, profile);
                if !m.is_empty() {
                    per_building.push(((b.id, b.osm_type.tag()), m));
                }
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

    // Lights: one per building owned by the chunk (detail: footprints then landmarks; overview:
    // the light points), none for `dark` buildings -> pair with the building, sort by id.
    let lit = |b: &&Building| crate::mesher::resolve(b, profile).profile != crate::mesher::PROFILE_DARK;
    let owned: Vec<(u64, u64)> = if dt.level == DataLevel::Overview {
        dt.lights.iter().filter(|b| proj.place(b).is_some()).filter(lit).map(|b| (b.id, b.osm_type.tag())).collect()
    } else {
        dt.buildings
            .iter()
            .filter(|b| proj.place(b).is_some())
            .chain(lm.landmarks.iter().filter(|b| lproj.place(b).is_some()))
            .filter(lit)
            .map(|b| (b.id, b.osm_type.tag()))
            .collect()
    };
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
/// for perf review 3 item 3 (`lights.street.spacing_scale` default [1, 1, 1, 2]: R3 street lamps
/// every 70 m at intensity(2); only the two R3 hashes changed, here and in `GOLDEN_TERRAIN`).
/// Before that for Tiles and LOD v2 step T2c (every hash changed once, intended): the synthetic city streams
/// through the data-tile path as a two-level source; R0 / R1 are cut from a generated detail z14
/// tile (the same lots unthinned, in z14 pixels; buildings with a tagged roof >= 60 m left out),
/// R2 / R3 from a generated overview z11 tile (street-block blocks with 5 m height steps instead
/// of merged z14 footprints, one light point per building with rounded roofs instead of the
/// thinned z13 / z14 buildings, secondary streets only), the >= 60 m landmarks folded into
/// every chunk from the overview (appended after the chunk's own buildings). Before that
/// for the R3 ground (api 1.3, `ground.r3`: only the two R3 hashes changed), before that for night-windows W4 (default `lights.min_ring` 2 -> 1: R1 tiles now carry lights, only the R1 hashes
/// changed), before that for night-windows W3 (api 1.0): wall UVs only (window columns / floors on windowed R0/R1
/// walls, `u = -1` + meters on windowless and all R2 walls); positions, normals, colors, indices,
/// ground and lights unchanged (R3 hashes identical). Before that for W2 (profile_version 4):
/// churches moved from civic to landmark (COLOR r and light color), and the synthetic castle /
/// tower / ruin lots in the origin's z16 tile.
const GOLDEN: &[(&str, usize, u64)] = &[
    ("16/37370/20827", 0, 0x764e69740c8a69eb),
    ("16/37371/20828", 0, 0x01c6838b3197bb93),
    ("15/18685/10413", 1, 0xd90a708cfbcc6d0e),
    ("15/18686/10414", 1, 0x88bb442529ea9bdb),
    ("14/9342/5206", 2, 0xd7f24dee1cf3f31b),
    ("14/9343/5207", 2, 0x8c7f7504ede9e2a7),
    ("13/4671/2603", 3, 0x8ff0d8ad5edc69dd),
    ("13/4672/2604", 3, 0x2b1cb8a827e3ef4a),
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
