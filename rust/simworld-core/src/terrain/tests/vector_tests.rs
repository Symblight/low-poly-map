//! The vector world on the terrain: building walls and bases, clipped pieces, raised parts,
//! the flat world unchanged, the ground drape.

use super::*;

/// Wall quads of a building mesh as (first vertex, is_foundation): side quads (|normal.y| < 0.5)
/// come in pairs on the terrain, the wall then its foundation (u = -1, top = the wall bottom).
fn side_quads(m: &crate::mesher::MeshData) -> Vec<usize> {
    (0..m.indices.len() / 6).map(|q| m.indices[q * 6] as usize).filter(|&v0| m.normals[v0][1].abs() < 0.5).collect()
}

/// Exact terrain height under a mesh vertex of tile `key`.
fn h_at(frame: Frame, key: TileKey, t: &Terrain, v: [f32; 3]) -> f64 {
    let c = frame.tile_center(key);
    let (lat, lon) = frame.lat_lon(c[0] + v[0] as f64, c[1] - v[2] as f64);
    t.height(lat, lon)
}

/// Per-vertex walls (api 1.7, contract section 7): on a slope every wall ends at the exact ground
/// at its vertices (never above it), the foundation hangs `foundation_m` below that, the roof is at
/// the highest ground under the footprint + height (`base_m`), and windowed walls keep a constant
/// floor height: `v = floors` at the roof and `floors - (roof - y) / floor_m` at every bottom
/// (negative downhill), windowless walls and foundations `u = -1`, `v = y - y0` in meters.
#[test]
fn walls_reach_the_exact_ground_at_every_vertex() {
    use crate::mesher::{mesh_buildings, resolve, ClipRect};
    use crate::worker::{decode_payload_on, Payload};
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let t = no_flat();
    let p = Profile::default();
    let key = tile_for(VILNIUS.0 + 0.01, VILNIUS.1 + 0.01, 16);
    let dec = decode_payload_on(key, frame, &t, &Payload::Synthetic, false, false, &p).unwrap();
    let lod = p.lod(0).unwrap();
    let f = t.foundation_m(0);
    let (mut checked, mut windowed, mut negative_rows, mut max_rise) = (0, 0, 0, 0.0f64);
    for b in dec.buildings.iter().filter(|b| b.min_height == 0.0) {
        let r = resolve(b, &p);
        let m = mesh_buildings(std::slice::from_ref(b), &dec.proj, ClipRect::tile(4096.0), lod, &p);
        if m.is_empty() {
            continue;
        }
        let roof = b.base_m + r.height;
        // The base is the highest ground under the decoded outline.
        for &v in &b.polygons[0][0] {
            assert!(dec.proj.ground_at(v) <= b.base_m as f64 + 1e-3, "a footprint vertex above the base");
        }
        for (k, n) in m.normals.iter().enumerate() {
            if n[1] > 0.5 {
                assert_eq!(m.positions[k][1], roof, "roof at base + height");
            }
        }
        let quads = side_quads(&m);
        assert_eq!(quads.len() % 2, 0);
        let wall_h = r.height - r.min_height;
        let floors = (wall_h / r.floor_h.max(0.1)).round().max(1.0);
        for pair in quads.as_chunks::<2>().0 {
            let (w, fq) = (pair[0], pair[1]);
            for k in 0..2 {
                let bot = m.positions[w + k];
                let h = h_at(frame, key, &t, bot);
                assert!(bot[1] as f64 <= h + 2e-3, "wall bottom {} above the exact ground {h}", bot[1]);
                assert!(bot[1] as f64 >= h.min(b.base_m as f64) - 2e-3, "wall bottom {} below the exact ground {h}", bot[1]);
                max_rise = max_rise.max(b.base_m as f64 - h);
                // Foundation: the wall bottom down to `f` below it, windowless.
                let (fb, ft) = (m.positions[fq + k], m.positions[fq + 3 - k]);
                assert_eq!((ft[0], ft[1], ft[2]), (bot[0], bot[1], bot[2]), "foundation top = wall bottom");
                assert!((fb[1] - (bot[1] - f)).abs() < 1e-4);
                assert!(m.uvs[fq + k][0] == -1.0 && m.uvs[fq + 3 - k][0] == -1.0);
                assert!((m.uvs[fq + k][1] - (fb[1] - b.base_m)).abs() < 2e-3, "foundation v in meters from y0");
                // Wall UV.
                let (uv_b, uv_t) = (m.uvs[w + k], m.uvs[w + 3 - k]);
                assert_eq!(m.positions[w + 3 - k][1], roof);
                if uv_b[0] >= 0.0 {
                    windowed += 1;
                    assert_eq!(uv_t[1], floors);
                    let want = floors - (roof - bot[1]) / (wall_h / floors);
                    assert!((uv_b[1] - want).abs() < 2e-3, "v {} want {want}", uv_b[1]);
                    if uv_b[1] < -0.5 {
                        negative_rows += 1;
                    }
                } else {
                    assert_eq!(uv_t[1], wall_h);
                    assert!((uv_b[1] - (bot[1] - b.base_m)).abs() < 2e-3);
                }
            }
        }
        checked += 1;
    }
    eprintln!("per-vertex walls: {checked} buildings, {windowed} windowed bottoms, {negative_rows} with whole rows below y0, largest rise {max_rise:.2} m");
    assert!(checked >= 20 && windowed > 50 && max_rise > 0.5, "the tile must test a slope");
    assert!(negative_rows > 0, "downhill rows continue below the nominal bottom");
}

/// A long wall over a dip is subdivided so its bottom follows the ground (no gap under the
/// windowed wall bigger than the band's drape margin at the tested points, and never more than
/// the foundation anywhere along it); the base also sees a hill inside a large footprint.
#[test]
fn long_walls_follow_the_ground_and_the_base_sees_the_interior() {
    use crate::mesher::{mesh_buildings, ClipRect};
    use crate::mvt::{BuildingTags, OsmType, RawBuilding, TileProjector};
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let t = active(TerrainProfile { enabled: true, flat_radius_m: 0.0, flat_blend_m: 0.0, amplitude_m: 900.0, ..TerrainProfile::default() });
    let p = Profile::default();
    let key = tile_for(VILNIUS.0 + 0.03, VILNIUS.1 - 0.05, 16);
    let proj = TileProjector::new(key, frame, 4096.0).with_terrain(t.clone());
    let lod = p.lod(0).unwrap();
    let tol = t.drape_margin_m(0) as f64;
    let mut worst_gap = 0.0f64;
    let mut interior_wins = 0;
    // A grid of 200 m x 40 m slabs across the tile (long walls on varied relief).
    for (i, (x0, y0)) in [(300.0, 300.0), (1500.0, 900.0), (2600.0, 2400.0), (600.0, 3000.0), (2000.0, 1600.0)].into_iter().enumerate() {
        let (w, h) = (2100.0, 420.0);
        let ring = vec![[x0, y0], [x0 + w, y0], [x0 + w, y0 + h], [x0, y0 + h]];
        let raw = RawBuilding {
            osm_type: OsmType::Way,
            id: 900 + i as u64,
            kind: "industrial".into(),
            height: Some(12.0),
            levels: None,
            min_height: 0.0,
            landuse: None,
            tags: BuildingTags::default(),
            polygons: vec![vec![ring.clone()]],
        };
        let b = proj.finish(raw).unwrap();
        let corner_max = ring.iter().map(|&v| proj.ground_at(v)).fold(f64::MIN, f64::max);
        if b.base_m as f64 > corner_max + 1e-3 {
            interior_wins += 1;
        }
        let m = mesh_buildings(std::slice::from_ref(&b), &proj, ClipRect::tile(4096.0), lod, &p);
        let f = t.foundation_m(0) as f64;
        for pair in side_quads(&m).as_chunks::<2>().0 {
            let (a, c) = (m.positions[pair[0]], m.positions[pair[0] + 1]);
            let len = ((c[0] - a[0]).hypot(c[2] - a[2])) as f64;
            for k in 1..16 {
                let s = k as f64 / 16.0;
                let v = [a[0] + (c[0] - a[0]) * s as f32, a[1] + (c[1] - a[1]) * s as f32, a[2] + (c[2] - a[2]) * s as f32];
                let gap = v[1] as f64 - h_at(frame, key, &t, v);
                assert!(gap < f, "wall bottom {gap:.2} m above the ground: the foundation does not reach");
                if len > t.drape_step_m() && [4, 8, 12].contains(&k) {
                    assert!(gap <= tol + 1e-3, "long wall bottom {gap:.3} m above the ground (tolerance {tol})");
                }
                worst_gap = worst_gap.max(gap);
            }
        }
    }
    eprintln!("long walls: worst bottom gap {worst_gap:.3} m (margin {tol}), base above every corner on {interior_wins} of 5 slabs");
}

/// Clipping does not move a building (api 1.7): the base comes from the decoded outline, so the
/// four quarter rectangles of a tile (the children's pieces) put the roof of a building cut by
/// them at exactly the same height as the whole tile, and a wall bottom on a clip line sits on
/// the exact ground at that clipped point.
#[test]
fn clipped_pieces_agree_on_the_roof() {
    use crate::mesher::{mesh_buildings, ClipRect};
    use crate::worker::{decode_payload_on, Payload};
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let t = no_flat();
    let p = Profile::default();
    let key = tile_for(VILNIUS.0 + 0.01, VILNIUS.1 + 0.01, 16);
    let dec = decode_payload_on(key, frame, &t, &Payload::Synthetic, false, false, &p).unwrap();
    let lod = p.lod(0).unwrap();
    let mut cut = 0;
    for b in dec.buildings.iter().filter(|b| b.min_height == 0.0 && !b.clipped) {
        // Cut every building through the middle of its bounding box.
        let xs = b.polygons[0][0].iter().map(|v| v[0]);
        let half = (xs.clone().fold(f64::MAX, f64::min) + xs.fold(f64::MIN, f64::max)) * 0.5;
        let whole = mesh_buildings(std::slice::from_ref(b), &dec.proj, ClipRect::tile(4096.0), lod, &p);
        let roof = |m: &crate::mesher::MeshData| m.positions.iter().zip(&m.normals).filter(|(_, n)| n[1] > 0.5).map(|(v, _)| v[1]).next();
        let Some(r0) = roof(&whole) else { continue };
        for rect in [ClipRect { x0: 0.0, y0: 0.0, x1: half, y1: 4096.0 }, ClipRect { x0: half, y0: 0.0, x1: 4096.0, y1: 4096.0 }] {
            let m = mesh_buildings(std::slice::from_ref(b), &dec.proj, rect, lod, &p);
            if let Some(r) = roof(&m) {
                assert_eq!(r, r0, "a clipped piece moved the roof");
            }
            for pair in side_quads(&m).as_chunks::<2>().0 {
                for k in 0..2 {
                    let v = m.positions[pair[0] + k];
                    let h = h_at(frame, key, &t, v);
                    assert!(v[1] as f64 <= h + 2e-3 && v[1] as f64 >= h.min(b.base_m as f64) - 2e-3, "bottom {} vs ground {h}", v[1]);
                }
            }
        }
        cut += 1;
    }
    assert!(cut > 20, "{cut} buildings cut");
}

/// A raised part stands on the building under it: it takes that building's base (its own
/// smaller footprint would give a lower one on a slope), so its bottom cap stays `min_height`
/// above the lower part's base; walls of a raised part are not stretched to the ground.
#[test]
fn raised_parts_stand_on_their_lower_part() {
    use crate::mesher::{mesh_buildings, ClipRect};
    use crate::mvt::{stack_raised_parts, BuildingTags, OsmType, RawBuilding, TileProjector};
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let t = no_flat();
    let p = Profile::default();
    let key = tile_for(VILNIUS.0 + 0.01, VILNIUS.1 + 0.01, 16);
    let proj = TileProjector::new(key, frame, 4096.0).with_terrain(t.clone());
    let sq = |x: f64, y: f64, s: f64| vec![vec![vec![[x, y], [x + s, y], [x + s, y + s], [x, y + s]]]];
    let raw = |id: u64, min_h: f32, polys| RawBuilding {
        osm_type: OsmType::Way,
        id,
        kind: "apartments".into(),
        height: Some(30.0),
        levels: None,
        min_height: min_h,
        landuse: None,
        tags: BuildingTags::default(),
        polygons: polys,
    };
    // Find a spot where the lower part's base is above the part's own (a slope under it).
    let mut found = false;
    for k in 0..40 {
        let (x, y) = (200.0 + 90.0 * k as f64, 200.0 + 85.0 * k as f64);
        let lower = proj.finish(raw(1, 0.0, sq(x, y, 400.0))).unwrap();
        let part = proj.finish(raw(2, 12.0, sq(x + 20.0, y + 20.0, 100.0))).unwrap();
        if part.base_m >= lower.base_m - 0.05 {
            continue;
        }
        let mut bs = vec![lower.clone(), part];
        stack_raised_parts(&mut bs);
        assert_eq!(bs[1].base_m, lower.base_m);
        let m = mesh_buildings(&bs[1..], &proj, ClipRect::tile(4096.0), p.lod(0).unwrap(), &p);
        let low = m.positions.iter().map(|v| v[1]).fold(f32::MAX, f32::min);
        assert_eq!(low, lower.base_m + 12.0, "the raised part's bottom cap / walls start at min_height over the lower part");
        found = true;
        break;
    }
    assert!(found, "no slope found for the raised-part test");
}

/// Terrain off: buildings are meshed exactly as before api 1.7 (walls from the base, no wall
/// below it, base 0); the golden hashes pin the bytes.
#[test]
fn flat_world_walls_are_unchanged() {
    use crate::mesher::{mesh_buildings, ClipRect};
    use crate::worker::{decode_payload_on, Payload};
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let p = Profile::default();
    let key = tile_for(VILNIUS.0 + 0.01, VILNIUS.1 + 0.01, 16);
    let flat = decode_payload_on(key, frame, &Terrain::flat(), &Payload::Synthetic, false, false, &p).unwrap();
    assert!(flat.buildings.iter().all(|b| b.base_m == 0.0));
    let m = mesh_buildings(&flat.buildings, &flat.proj, ClipRect::tile(4096.0), p.lod(0).unwrap(), &p);
    assert!(m.uvs.iter().zip(&m.normals).all(|(uv, n)| n[1].abs() > 0.5 || uv[1] >= 0.0), "no wall below its base");
    assert!(m.positions.iter().all(|v| v[1] >= 0.0));
}

/// Ground drape: area vertices sit on the terrain + band drape margin + level lift, water vertices at
/// (or below) the snapped height, roads follow the slope; long edges are subdivided on relief.
#[test]
fn ground_is_draped_and_subdivided() {
    use crate::mvt::{GROUND_ROAD, GROUND_WATER};
    use crate::worker::{build_tile_on, Payload};
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let t = no_flat();
    let p = Profile::default();
    let step = p.ground_params(1).unwrap().step_m as f64;
    let key = tile_for(VILNIUS.0 + 0.02, VILNIUS.1 - 0.03, 15);
    let draped = build_tile_on(key, 1, frame, &t, &Payload::Synthetic, &p).unwrap().ground;
    let flat = build_tile_on(key, 1, frame, &Terrain::flat(), &Payload::Synthetic, &p).unwrap().ground;
    assert!(draped.vertex_count() > flat.vertex_count(), "subdivided: {} vs {}", draped.vertex_count(), flat.vertex_count());
    let c = frame.tile_center(key);
    let lift = t.drape_margin_m(1) as f64;
    let (mut areas, mut water) = (0, 0);
    for (pos, col) in draped.positions.iter().zip(&draped.colors) {
        let class = (col[0] * 16.0).round() as u8;
        let (lat, lon) = frame.lat_lon(c[0] + pos[0] as f64, c[1] - pos[2] as f64);
        let y = pos[1] as f64;
        if class == GROUND_WATER {
            assert!(y - lift - 7.0 * step <= t.height(lat, lon) + 0.02, "water above its snapped level");
            water += 1;
        } else if class != GROUND_ROAD {
            let d = y - lift - t.height(lat, lon);
            assert!((-0.02..=7.0 * step + 0.02).contains(&d), "area vertex {d} m off the terrain + margin");
            areas += 1;
        }
    }
    assert!(areas > 50, "{areas} area vertices");
    eprintln!("draped z15 ground: {} vertices (flat {}), {water} water vertices", draped.vertex_count(), flat.vertex_count());
    // Deterministic.
    assert_eq!(draped, build_tile_on(key, 1, frame, &t, &Payload::Synthetic, &p).unwrap().ground);
}
