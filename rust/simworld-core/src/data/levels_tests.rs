//! Two-level data tiles (Tiles v2, steps T2b / T2c): the overview level (blocks, light points,
//! landmarks), R2 / R3 chunks cut from it, landmark tiles, and "every building drawn exactly
//! once" across detail + landmarks (R0 / R1) and overview + landmarks (R2 / R3).

use std::collections::BTreeMap;

use super::fixture::{building_ids, detail_v2_bytes, key11, key14, overview_bytes, LANDMARK_IDS, VILNIUS};
use super::*;
use crate::geo::Frame;
use crate::mesher::{resolve, PROFILE_DARK};
use crate::mvt::OsmType;
use crate::rings::children;

fn frame() -> Frame {
    Frame::new(VILNIUS.0, VILNIUS.1)
}

fn p() -> &'static Profile {
    crate::profile::test_default()
}

fn detail() -> DataTile {
    decode_data_tile(key14(), frame(), &Terrain::flat(), &detail_v2_bytes(), p()).unwrap()
}

fn overview() -> DataTile {
    decode_overview_tile(key11(), frame(), &Terrain::flat(), &overview_bytes(), p()).unwrap()
}

fn never() -> bool {
    false
}

fn chunk(dt: &DataTile, lm: Option<&DataTile>, k: TileKey, ring: usize) -> TileData {
    build_chunk_with(dt, lm, k, ring, p(), &never).unwrap().expect("not stopped")
}

/// The chunks of zoom `z` inside the z14 fixture tile.
fn chunks_in_14(z: u8) -> Vec<TileKey> {
    let mut v = vec![key14()];
    while v[0].z < z {
        v = v.into_iter().flat_map(children).collect();
    }
    v
}

/// Upward-facing building triangle area (m², horizontal projection) per building, keyed by the
/// building's COLOR g (its seed): a building drawn twice would show twice its roof.
fn roof_area_by_seed(m: &crate::mesher::MeshData, out: &mut BTreeMap<u32, f64>) {
    for t in m.indices.chunks(3) {
        let [a, b, c] = [t[0] as usize, t[1] as usize, t[2] as usize];
        if m.normals[a][1] < 0.99 {
            continue;
        }
        let (pa, pb, pc) = (m.positions[a], m.positions[b], m.positions[c]);
        let area = 0.5 * (((pb[0] - pa[0]) * (pc[2] - pa[2]) - (pc[0] - pa[0]) * (pb[2] - pa[2])) as f64).abs();
        *out.entry(m.colors[a][1].to_bits()).or_default() += area;
    }
}

#[test]
fn overview_tile_decodes_blocks_lights_and_landmarks() {
    let o = overview();
    let d = detail();
    let ids = building_ids();
    assert_eq!(o.level, DataLevel::Overview);
    assert_eq!(d.level, DataLevel::Detail);
    // One light point per building (landmarks too), OSM id and type from the feature id.
    let mut light_ids: Vec<u64> = o.lights.iter().map(|b| b.id).collect();
    light_ids.sort_unstable();
    let mut want = ids.clone();
    want.sort_unstable();
    assert_eq!(light_ids, want);
    for l in &o.lights {
        let t = if l.id == 6002 { OsmType::Relation } else { OsmType::Way };
        assert_eq!(l.osm_type, t, "{}", l.id);
        assert!(l.polygons.is_empty() && l.owned);
    }
    // The landmark: full geometry, flagged in the lights, not in detail.
    assert_eq!(o.landmarks.iter().map(|b| b.id).collect::<Vec<_>>(), LANDMARK_IDS.to_vec());
    assert!(o.landmarks[0].polygons[0][0].len() >= 4);
    let flagged: Vec<u64> = o.lights.iter().zip(&o.light_landmark).filter(|(_, f)| **f).map(|(b, _)| b.id).collect();
    assert_eq!(flagged, LANDMARK_IDS.to_vec());
    let mut detail_ids: Vec<u64> = d.buildings.iter().map(|b| b.id).collect();
    detail_ids.dedup();
    assert!(detail_ids.iter().all(|id| !LANDMARK_IDS.contains(id)), "a landmark is in detail");
    assert_eq!(detail_ids.len() + LANDMARK_IDS.len(), ids.len(), "every building in exactly one level");
    // Blocks: one per non-landmark building here, heights in 5 m steps; overview ground.
    assert_eq!(o.buildings.len(), ids.len() - LANDMARK_IDS.len());
    assert!(o.buildings.iter().all(|b| b.height.is_some_and(|h| h % 5.0 == 0.0)));
    assert_eq!(o.ground.roads.len(), 1, "primary road only");
    assert!(!o.ground.areas.is_empty());
    assert!(o.bytes > d.bytes / 4 && o.bytes > 0);
}

/// R2 / R3 chunks from the overview tile: block meshes at R2 (none at R3), one light per owned
/// light point (landmarks' only where they are drawn), clusters, street lamps and ground; the
/// 4 R2 siblings partition the R3 chunk's light points.
#[test]
fn r2_r3_chunks_come_from_the_overview_tile() {
    let o = overview();
    let k13 = TileKey::new(13, key14().x >> 1, key14().y >> 1);
    let lit = |b: &Building| resolve(b, p()).profile != PROFILE_DARK;
    let owned_in = |k: TileKey, with_lm: bool| {
        let proj = o.proj.cut(k).unwrap();
        o.lights.iter().zip(&o.light_landmark).filter(|(b, f)| (with_lm || !**f) && proj.place(b).is_some() && lit(b)).count()
    };
    let r2 = chunk(&o, None, key14(), 2);
    let r2l = chunk(&o, Some(&o), key14(), 2);
    assert!(!r2.mesh.is_empty() && r2l.mesh.triangle_count() > r2.mesh.triangle_count(), "landmark folded in at R2");
    assert_eq!(r2.lights.positions.len(), owned_in(key14(), false));
    assert_eq!(r2l.lights.positions.len(), owned_in(key14(), true));
    assert_eq!(r2l.lights.positions.len(), r2.lights.positions.len() + 1, "the tower's light");
    assert!(!r2.lights.clustered_positions.is_empty() && r2.lights.clustered_positions.len() < r2.lights.positions.len());
    assert!(r2.lights.beacon_positions.is_empty() && !r2l.lights.beacon_positions.is_empty(), "beacons from the landmark only");
    assert!(!r2.ground.is_empty() && !r2.lights.street_positions.is_empty());
    let r3 = chunk(&o, None, k13, 3);
    assert!(r3.mesh.is_empty() && !r3.ground.is_empty());
    let sum: usize = children(k13).iter().map(|&c| chunk(&o, None, c, 2).lights.positions.len()).sum();
    assert_eq!(r3.lights.positions.len(), sum, "siblings partition the light points");
    assert_eq!(r3.lights.positions.len(), owned_in(k13, false));
    assert!(r3.max_height >= 20.0, "the R3 box covers the light points' roofs: {}", r3.max_height);
}

/// Landmark tile: full R0 geometry with window UVs, one light point (same in both light modes),
/// beacons; no ground. An overview tile without landmarks gives an empty tile.
#[test]
fn landmark_tile_has_full_geometry_lights_and_beacons() {
    let o = overview();
    let t = build_landmark_tile(&o, p(), &never).unwrap().unwrap();
    assert_eq!(t.building_count, 1);
    assert!(!t.mesh.is_empty() && t.ground.is_empty());
    assert!(t.mesh.uvs.iter().any(|uv| uv[0] >= 0.0 && uv[1] > 0.0), "window UVs (R0 quality)");
    assert_eq!(t.lights.positions.len(), 1);
    assert_eq!((&t.lights.positions, &t.lights.colors), (&t.lights.clustered_positions, &t.lights.clustered_colors));
    assert!(!t.lights.beacon_positions.is_empty());
    assert!(t.max_height >= 95.0);
    let empty = decode_overview_tile(key11(), frame(), &Terrain::flat(), &[], p()).unwrap();
    assert_eq!(build_landmark_tile(&empty, p(), &never).unwrap().unwrap().building_count, 0);
}

/// Every building is drawn exactly once per band: R0 / R1 = detail chunks + the landmarks
/// (folded into the chunks, or the landmark tile); R2 = overview blocks + landmarks. Measured
/// as the roof area per building (COLOR g = its seed) summed over all chunks of the fixture
/// tile and the landmark tile: the same in both landmark modes, each building's roof within
/// 20 % of its footprint (twice = drawn twice), the landmark exactly where it should be. Light
/// points: one per lit building in each mode.
#[test]
fn every_building_is_drawn_exactly_once_across_detail_overview_and_landmarks() {
    let (d, o) = (detail(), overview());
    let lt = build_landmark_tile(&o, p(), &never).unwrap().unwrap();
    let mut lt_roof = BTreeMap::new();
    roof_area_by_seed(&lt.mesh, &mut lt_roof);
    let tower = o.landmarks[0].seed01().to_bits();
    let tower_m2 = o.landmarks[0].footprint_m2 as f64;
    assert!((lt_roof[&tower] / tower_m2 - 1.0).abs() < 0.02, "landmark tile roof {} vs {tower_m2}", lt_roof[&tower]);
    for (z, ring, src) in [(16u8, 0usize, &d), (15, 1, &d), (14, 2, &o)] {
        let ks = chunks_in_14(z);
        let (mut folded, mut layer) = (BTreeMap::new(), BTreeMap::new());
        let (mut lights_folded, mut lights_layer) = (0, 0);
        for &k in &ks {
            let a = chunk(src, Some(&o), k, ring);
            let b = chunk(src, None, k, ring);
            roof_area_by_seed(&a.mesh, &mut folded);
            roof_area_by_seed(&b.mesh, &mut layer);
            lights_folded += a.lights.positions.len();
            lights_layer += b.lights.positions.len();
        }
        assert!(folded.contains_key(&tower) && !layer.contains_key(&tower), "z{z}: the landmark only where folded in");
        assert!((folded[&tower] / tower_m2 - 1.0).abs() < 0.05, "z{z}: landmark roof {} vs {tower_m2}", folded[&tower]);
        // Every other building: the same roof in both modes, once.
        let footprint: BTreeMap<u32, f64> = src.buildings.iter().map(|b| (b.seed01().to_bits(), b.footprint_m2 as f64)).collect();
        for (seed, &m2) in &layer {
            assert_eq!(folded.get(seed), Some(&m2), "z{z}: a building differs between the modes");
            let fp = footprint[seed];
            assert!(m2 <= fp * 1.2 + 1.0, "z{z}: roof {m2:.0} m² over footprint {fp:.0} m²: drawn twice?");
            assert!(m2 >= fp * 0.6, "z{z}: roof {m2:.0} m² under footprint {fp:.0} m²: missing a piece?");
        }
        if ring >= p().lights.min_ring as usize {
            assert_eq!(lights_folded, lights_layer + lt.lights.positions.len(), "z{z}: one light per building");
        }
        eprintln!("z{z}: {} buildings + the landmark, {lights_folded} lights", layer.len());
    }
}

/// The synthetic city as a two-level source (T2c): no landmark in detail, every landmark in the
/// overview with its light flagged, one light point per building of the overview area (the
/// detail tiles of the area hold the rest), blocks in 5 m steps.
#[test]
fn synthetic_city_emits_both_levels() {
    let f = frame();
    let k11 = crate::geo::tile_for(VILNIUS.0, VILNIUS.1, 11);
    let o = synthetic_data_tile(k11, DataLevel::Overview, f, &Terrain::flat(), p());
    assert!(!o.landmarks.is_empty() && !o.buildings.is_empty() && o.lights.len() > o.buildings.len());
    assert_eq!(o.light_landmark.iter().filter(|x| **x).count(), o.landmarks.len());
    assert!(o.buildings.iter().all(|b| b.height.is_some_and(|h| h % 5.0 == 0.0)));
    assert!(o.landmarks.iter().all(crate::synthetic::is_landmark));
    let k14 = crate::geo::tile_for(VILNIUS.0, VILNIUS.1, 14);
    let d = synthetic_data_tile(k14, DataLevel::Detail, f, &Terrain::flat(), p());
    assert!(!d.buildings.is_empty() && d.buildings.iter().all(|b| !crate::synthetic::is_landmark(b)));
    // The detail tile's buildings + the overview's landmarks inside it = its light points.
    let proj = o.proj.cut(k14).unwrap();
    let in14 = o.lights.iter().filter(|b| proj.place(b).is_some()).count();
    let lm14 = o.landmarks.iter().filter(|b| proj.place(b).is_some()).count();
    assert_eq!(d.buildings.len() + lm14, in14);
    // Same data, same tile (deterministic).
    let o2 = synthetic_data_tile(k11, DataLevel::Overview, f, &Terrain::flat(), p());
    assert_eq!((o.buildings.clone(), o.landmarks.clone(), o.lights.clone()), (o2.buildings, o2.landmarks, o2.lights));
}

/// R0 / R1 synthetic chunks from the detail level match the per-tile path's output: the same
/// buildings, triangle for triangle, within a millimetre after sorting (the landmarks are
/// appended after the detail buildings), the same ground and street lamps.
#[test]
fn synthetic_r0_r1_chunks_match_the_per_tile_path() {
    let f = frame();
    for (k, ring) in [(crate::geo::tile_for(VILNIUS.0, VILNIUS.1, 16), 0usize), (crate::geo::tile_for(VILNIUS.0, VILNIUS.1, 15), 1)] {
        let new = synthetic_chunk(k, ring, f, &Terrain::flat(), p(), false).unwrap();
        let old = crate::worker::build_tile(k, ring, f, &crate::worker::Payload::Synthetic, p()).unwrap();
        assert_eq!(new.mesh.triangle_count(), old.mesh.triangle_count(), "{k}");
        let sorted = |m: &crate::mesher::MeshData| {
            let mut v: Vec<[i64; 3]> = m.positions.iter().map(|q| q.map(|x| (x as f64 * 100.0).round() as i64)).collect();
            v.sort_unstable();
            v
        };
        let (a, b) = (sorted(&new.mesh), sorted(&old.mesh));
        let off = a.iter().zip(&b).filter(|(x, y)| x.iter().zip(y.iter()).any(|(p, q)| (p - q).abs() > 1)).count();
        assert!(off * 100 <= a.len(), "{k}: {off} of {} vertices more than 1 cm apart", a.len());
        // Street lamps run from each line's start: a z14 line starts elsewhere than a z16 one.
        let (sa, sb) = (new.lights.street_positions.len() as i64, old.lights.street_positions.len() as i64);
        assert!((sa - sb).abs() <= 1 + sb / 4, "{k}: {sa} vs {sb} street lamps");
        assert_eq!(new.lights.beacon_positions.len(), old.lights.beacon_positions.len(), "{k}");
        // Ground: the same area (roads cut from longer z14 lines triangulate differently).
        let area = |m: &crate::mesher::MeshData| -> f64 {
            m.indices
                .chunks(3)
                .map(|t| {
                    let (a, b, c) = (m.positions[t[0] as usize], m.positions[t[1] as usize], m.positions[t[2] as usize]);
                    0.5 * (((b[0] - a[0]) * (c[2] - a[2]) - (c[0] - a[0]) * (b[2] - a[2])) as f64).abs()
                })
                .sum()
        };
        let (ga, gb) = (area(&new.ground), area(&old.ground));
        // (The per-tile path drops the half strip of a street on its far edge line, which the
        // neighbour owns; a chunk cut from z14 keeps it: up to a few % more road area.)
        assert!((ga / gb - 1.0).abs() < 0.03, "{k}: ground {ga:.0} vs {gb:.0} m²");
    }
}

/// Overview block bases with the terrain on (perf review 2 item 7): the decode leaves them open
/// and the R2 chunk resolves them. The fixture's R2 chunk on a hilly relief (no flat zone) keeps
/// the bits of the eager decode: the hashes below were printed by the code before the change.
#[test]
fn overview_block_bases_on_terrain_match_the_eager_decode() {
    let mut w = crate::profile::WorldProfile::default();
    w.terrain.enabled = true;
    w.terrain.flat_radius_m = 0.0;
    w.terrain.flat_blend_m = 0.0;
    let prof = crate::profile::Profile::new(w).unwrap();
    let t = Terrain::from_profile(&prof, VILNIUS.0, VILNIUS.1);
    let dt = decode_overview_tile(key11(), frame(), &t, &overview_bytes(), &prof).unwrap();
    let d = build_chunk_with(&dt, Some(&dt), key14(), 2, &prof, &never).unwrap().expect("not stopped");
    let got = (crate::data_world_tests::mesh_hash(&d.mesh), crate::data_world_tests::mesh_hash(&d.ground), d.max_height.to_bits());
    eprintln!("OVERVIEW TERRAIN CHUNK {got:x?}");
    assert!(dt.buildings.iter().any(|b| b.polygons.len() == 1), "the fixture has blocks");
    assert_eq!(got, PINNED_EAGER);
}

/// Printed by the eager decode (commit ed04907) for the test above.
const PINNED_EAGER: (u64, u64, u32) = (0xd4c60ca7838c76e1, 0xe231304024d6b071, 0x42be0000);
