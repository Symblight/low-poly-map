//! Data tiles (Tiles v2, step T2a): chunks cut from a detail z14 tile against the per-tile path.

use super::fixture::{key14, tile_bytes, tile_bytes_ext, VILNIUS};
use super::*;
use crate::geo::Frame;
use crate::mesher::MeshData;
use crate::rings::children;
use crate::worker::{build_tile, Payload};

fn frame() -> Frame {
    Frame::new(VILNIUS.0, VILNIUS.1)
}

fn data14(p: &Profile) -> DataTile {
    decode_data_tile(key14(), frame(), &Terrain::flat(), &tile_bytes(key14()), p).unwrap()
}

fn chunk(dt: &DataTile, k: TileKey, ring: usize, p: &Profile) -> TileData {
    build_chunk(dt, k, ring, p, &|| false).unwrap().expect("not stopped")
}

/// Largest position difference of two meshes with the same layout (panics on a different
/// layout, colors, or normals / UVs more than a millimetre apart).
fn mesh_diff(a: &MeshData, b: &MeshData, what: &str) -> f32 {
    assert_eq!(a.indices, b.indices, "{what}: index buffers differ");
    assert_eq!(a.positions.len(), b.positions.len(), "{what}: vertex counts differ");
    let mut d = 0.0f32;
    for (x, y) in a.positions.iter().zip(&b.positions) {
        for i in 0..3 {
            d = d.max((x[i] - y[i]).abs());
        }
    }
    for (x, y) in a.normals.iter().zip(&b.normals) {
        for i in 0..3 {
            assert!((x[i] - y[i]).abs() < 1e-3, "{what}: normals differ");
        }
    }
    assert_eq!(a.colors, b.colors, "{what}: colors differ");
    assert_eq!(a.uvs.len(), b.uvs.len());
    for (x, y) in a.uvs.iter().zip(&b.uvs) {
        assert!((x[0] - y[0]).abs() < 1e-3 && (x[1] - y[1]).abs() < 1e-3, "{what}: uvs differ {x:?} {y:?}");
    }
    d
}

fn points_diff(a: &[[f32; 3]], b: &[[f32; 3]]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| (0..3).map(|i| (x[i] - y[i]).abs()).fold(0.0, f32::max)).fold(0.0, f32::max)
}

/// An R0 chunk cut from the detail z14 tile equals today's z16 tile of the same key (the
/// archive's z16 holds the same features), and an R1 chunk today's z15 tile: same triangles,
/// vertices within a millimetre (the two paths differ only in floating point; the
/// simplification makes the same decisions), same building lights and beacons, same ground. All
/// 16 R0 chunks and all 4 R1 chunks of the tile are checked; the fixture puts buildings across
/// the chunk edges, a jagged outline that Douglas-Peucker simplifies, a hole and a tower.
#[test]
fn r0_and_r1_chunks_cut_from_z14_match_the_per_tile_meshes() {
    let p = crate::profile::resolve(None, Some(&serde_json::json!({ "lights": { "min_ring": 0 } })), &crate::NullLog).unwrap();
    let dt = data14(&p);
    assert!(dt.buildings.len() > 150 && !dt.ground.areas.is_empty() && !dt.ground.roads.is_empty());
    let mut checked = 0;
    let mut tris = 0;
    for k15 in children(key14()) {
        for (k, ring) in std::iter::once((k15, 1)).chain(children(k15).into_iter().map(|k| (k, 0))) {
            let cut = chunk(&dt, k, ring, &p);
            let own = build_tile(k, ring, frame(), &Payload::Bytes(tile_bytes(k).into()), &p).unwrap();
            let what = format!("{k} R{ring}");
            assert!(!cut.mesh.is_empty(), "{what}: no buildings");
            let dm = mesh_diff(&cut.mesh, &own.mesh, &format!("{what} buildings"));
            let dg = mesh_diff(&cut.ground, &own.ground, &format!("{what} ground"));
            assert!(dm < 1e-3 && dg < 1e-3, "{what}: building {dm} m, ground {dg} m apart");
            assert!(!cut.lights.positions.is_empty(), "{what}: no lights");
            assert!(points_diff(&cut.lights.positions, &own.lights.positions) < 1e-3, "{what}: lights");
            assert_eq!(cut.lights.colors, own.lights.colors);
            assert!(points_diff(&cut.lights.beacon_positions, &own.lights.beacon_positions) < 1e-3, "{what}: beacons");
            // The box height: the tallest building reaching into the chunk (the per-tile path
            // takes every decoded building, buffer copies included: the fixture's tiles are
            // unclipped, so there it is the tallest of the whole area).
            assert!(cut.max_height > 0.0 && cut.max_height <= own.max_height, "{what}: box height");
            checked += 1;
            tris += cut.mesh.triangle_count();
        }
    }
    assert_eq!(checked, 20);
    eprintln!("20 chunks, {tris} building triangles equal to the per-tile path");
}

/// Up-facing area (m²) of the triangles whose vertices are all at or above `min_y`, without
/// road strips (a strip ends square to the road, not along the chunk edge, so strips overlap
/// or leave slivers at a seam; that is the per-tile behaviour too and not a partition).
fn up_area(m: &MeshData, min_y: f32) -> f64 {
    let road = crate::mvt::GROUND_ROAD as f32 / 16.0;
    let mut a = 0.0;
    for t in m.indices.chunks(3) {
        let [p, q, r] = [m.positions[t[0] as usize], m.positions[t[1] as usize], m.positions[t[2] as usize]];
        if p[1] < min_y || q[1] < min_y || r[1] < min_y {
            continue;
        }
        if m.colors.get(t[0] as usize).is_some_and(|c| c[0] == road) {
            continue;
        }
        let ny = m.normals[t[0] as usize][1];
        if ny < 0.9 {
            continue;
        }
        let (u, v) = ([q[0] - p[0], q[2] - p[2]], [r[0] - p[0], r[2] - p[2]]);
        a += 0.5 * ((u[0] * v[1] - u[1] * v[0]) as f64).abs();
    }
    a
}

fn sort_pts(v: &mut [[f32; 3]]) {
    v.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[2].total_cmp(&b[2])).then(a[1].total_cmp(&b[1])));
}

/// Points of the 4 children, moved to the parent's origin, sorted (for multiset equality).
fn moved_sorted(parts: &[(TileKey, Vec<[f32; 3]>)], parent: TileKey) -> Vec<[f32; 3]> {
    let pc = frame().tile_center(parent);
    let mut v: Vec<[f32; 3]> = parts
        .iter()
        .flat_map(|(k, pts)| {
            let c = frame().tile_center(*k);
            let (dx, dz) = ((c[0] - pc[0]) as f32, -(c[1] - pc[1]) as f32);
            pts.iter().map(move |p| [p[0] + dx, p[1], p[2] + dz])
        })
        .collect();
    sort_pts(&mut v);
    v
}

/// The 4 R0 chunks of an R1 chunk partition it exactly: with the same meshing parameters in
/// both bands, their roof area and ground area (areas; road strips end square to the road)
/// add up to the parent's, every building light,
/// beacon and street lamp of the parent is in exactly one child (no doubles, no holes), and the
/// R1 chunks together light every building the data tile owns once.
#[test]
fn four_r0_chunks_partition_their_r1_parent() {
    let same = serde_json::json!({
        "buildings": { "r1": { "dp_m": 0.5, "min_area_m2": 0.0, "min_height_m": 0.0 } },
        "ground": { "r1": { "area_dp_m": 0.5, "min_area_m2": 0.0, "road_dp_m": 0.5, "max_road_class": "other", "minor_paths": true, "lift_step_m": 0.02 } },
        "lights": { "min_ring": 0, "street": { "min_ring": 0 }, "beacons": { "min_ring": 0 } }
    });
    let p = crate::profile::resolve(None, Some(&same), &crate::NullLog).unwrap();
    let dt = data14(&p);
    let (mut lamps, mut lights, mut beacons) = (0, 0, 0);
    type Get = fn(&TileData) -> Vec<[f32; 3]>;
    let gets: [(&str, Get); 3] = [
        ("lights", |d| d.lights.positions.clone()),
        ("beacons", |d| d.lights.beacon_positions.clone()),
        ("street lamps", |d| d.lights.street_positions.clone()),
    ];
    for k15 in children(key14()) {
        let parent = chunk(&dt, k15, 1, &p);
        let kids: Vec<(TileKey, TileData)> = children(k15).into_iter().map(|k| (k, chunk(&dt, k, 0, &p))).collect();
        let sum_roof: f64 = kids.iter().map(|(_, d)| up_area(&d.mesh, 1.0)).sum();
        let pr = up_area(&parent.mesh, 1.0);
        assert!(pr > 1000.0 && (sum_roof - pr).abs() < 1e-4 * pr, "{k15}: roofs {sum_roof} vs {pr}");
        let sum_ground: f64 = kids.iter().map(|(_, d)| up_area(&d.ground, -1.0)).sum();
        let pg = up_area(&parent.ground, -1.0);
        assert!(pg > 1000.0 && (sum_ground - pg).abs() < 1e-4 * pg, "{k15}: ground {sum_ground} vs {pg}");
        for (name, get) in gets {
            let parts: Vec<(TileKey, Vec<[f32; 3]>)> = kids.iter().map(|(k, d)| (*k, get(d))).collect();
            let a = moved_sorted(&parts, k15);
            let mut b = get(&parent);
            sort_pts(&mut b);
            assert_eq!(a.len(), b.len(), "{k15}: {name} doubled or lost");
            assert!(points_diff(&a, &b) < 2e-3, "{k15}: {name} moved");
        }
        lights += parent.lights.positions.len();
        lamps += parent.lights.street_positions.len();
        beacons += parent.lights.beacon_positions.len();
    }
    // Every owned building of the data tile is lit by exactly one R1 chunk.
    let owned = dt.buildings.iter().filter(|b| b.owned).count();
    assert_eq!(lights, owned);
    assert!(lamps > 10 && beacons > 0, "lamps {lamps}, beacons {beacons}");
}

/// Per-layer extents: a tile whose layers use different extents (the overview level mixes
/// 4096 and 16384) decodes to the same geometry as the all-4096 tile, in one pixel space.
#[test]
fn per_layer_extents_decode_to_one_pixel_space() {
    let p = Profile::default();
    let a = data14(&p);
    let mixed = decode_data_tile(key14(), frame(), &Terrain::flat(), &tile_bytes_ext(key14(), [8192, 4096, 16384]), &p).unwrap();
    assert_eq!(mixed.proj.extent(), 16384.0);
    assert_eq!(a.buildings.len(), mixed.buildings.len());
    for (x, y) in a.buildings.iter().zip(&mixed.buildings) {
        assert_eq!(x.id, y.id);
        assert_eq!(x.landuse, y.landuse, "landuse lookup in the common space");
        for (p, q) in x.polygons.iter().flatten().flatten().zip(y.polygons.iter().flatten().flatten()) {
            assert_eq!([p[0] * 4.0, p[1] * 4.0], *q);
        }
    }
    for (x, y) in a.ground.roads.iter().zip(&mixed.ground.roads) {
        assert_eq!(x.lines[0][1].map(|v| v * 4.0), y.lines[0][1]);
    }
    // The meshes of a chunk agree.
    let k = children(children(key14())[1])[2];
    let m1 = chunk(&a, k, 0, &p);
    let m2 = chunk(&mixed, k, 0, &p);
    assert!(mesh_diff(&m1.mesh, &m2.mesh, "mixed extents") < 1e-3);
    assert!(mesh_diff(&m1.ground, &m2.ground, "mixed extents ground") < 1e-3);
}

/// Edge cases: an empty data tile cuts empty chunks; a chunk outside the tile is an error; a
/// stopped build; the detail tile of a chunk; the byte estimate is plausible; cut chunks are
/// deterministic.
#[test]
fn chunk_edge_cases_and_levels() {
    let p = Profile::default();
    let empty = decode_data_tile(key14(), frame(), &Terrain::flat(), &[], &p).unwrap();
    let k16 = children(children(key14())[0])[0];
    let d = chunk(&empty, k16, 0, &p);
    assert!(d.mesh.is_empty() && d.ground.is_empty() && d.lights.is_empty() && d.building_count == 0);
    assert!(build_chunk(&empty, TileKey::new(16, 0, 0), 0, &p, &|| false).is_err());
    assert!(build_chunk(&empty, k16, 0, &p, &|| true).unwrap().is_none(), "stopped");
    let l = DataLevels::COMPAT;
    assert_eq!(l.detail_of(k16), Some(key14()));
    assert_eq!(l.detail_of(children(key14())[3]), Some(key14()));
    assert_eq!(l.detail_of(key14()), None, "R2 keeps its own tile");
    assert_eq!(l.detail_of(TileKey::new(13, 1, 1)), None);
    let dt = data14(&p);
    assert!(dt.bytes > 20_000 && dt.bytes < 5_000_000, "{}", dt.bytes);
    let a = chunk(&dt, k16, 0, &p);
    let b = chunk(&data14(&p), k16, 0, &p);
    assert_eq!((a.mesh, a.ground, a.lights), (b.mesh, b.ground, b.lights));
}


/// Timing of the synthetic data levels (T2c) and the cost of an overview tile.
#[test]
#[ignore]
fn synthetic_levels_timing() {
    let p = crate::profile::test_default();
    let f = frame();
    for terrain in [false, true] {
        let t = if terrain {
            let q = crate::profile::resolve(None, Some(&serde_json::json!({"terrain": {"enabled": true}})), &crate::NullLog).unwrap();
            Terrain::from_profile(&q, VILNIUS.0, VILNIUS.1)
        } else {
            Terrain::flat()
        };
        for z in [11u8, 14] {
            let k = crate::geo::tile_for(VILNIUS.0, VILNIUS.1, z);
            let t0 = std::time::Instant::now();
            let d = synthetic_data_tile(k, DataLevels::SYNTHETIC.level_of(k), f, &t, p);
            println!(
                "terrain {terrain} z{z}: {:.1} ms, {} buildings/blocks, {} landmarks, {} lights, {:.1} MB",
                t0.elapsed().as_secs_f64() * 1e3,
                d.buildings.len(),
                d.landmarks.len(),
                d.lights.len(),
                d.bytes as f64 / 1048576.0
            );
        }
    }
}
