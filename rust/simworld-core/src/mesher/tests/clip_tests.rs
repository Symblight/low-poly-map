//! Clipping and Douglas–Peucker: quadrant partition, seams, courtyards across the clip rect.

use super::*;

#[test]
fn douglas_peucker_tolerances_and_small_drop() {
    let (p, mpp) = setup();
    // A square with a 1 m bump in the middle of one side (in pixels).
    let bump = 1.0 / mpp;
    let ring = vec![[1000.0, 1000.0], [1100.0, 1000.0 - bump], [1200.0, 1000.0], [1200.0, 1200.0], [1000.0, 1200.0]];
    let b = bld("yes", Some(10.0), None, vec![vec![ring]]);
    let r0 = mesh_buildings(std::slice::from_ref(&b), &p, ClipRect::tile(4096.0), lod0(), &dp());
    let r1 = mesh_buildings(std::slice::from_ref(&b), &p, ClipRect::tile(4096.0), lod1(), &dp());
    assert_eq!(r0.triangle_count(), 5 * 2 + 3, "0.5 m keeps the 1 m bump");
    assert_eq!(r1.triangle_count(), 4 * 2 + 2, "2 m removes it");
    // < 20 m2 dropped at R1 only.
    let side = (16.0f64).sqrt() / mpp;
    let tiny = bld("yes", Some(10.0), None, vec![vec![sq(2000.0, 2000.0, side)]]);
    assert_eq!(mesh_buildings(std::slice::from_ref(&tiny), &p, ClipRect::tile(4096.0), lod1(), &dp()).triangle_count(), 0);
    assert_eq!(mesh_buildings(&[tiny], &p, ClipRect::tile(4096.0), lod0(), &dp()).triangle_count(), 10);
}

#[test]
fn clipping_partitions_buildings_across_quadrants_and_skips_seams() {
    let (p, _) = setup();
    // Straddles the vertical child seam at x = 2048.
    let b = bld("yes", Some(10.0), None, vec![vec![sq(1948.0, 1000.0, 200.0)]]);
    let whole = mesh_buildings(std::slice::from_ref(&b), &p, ClipRect::tile(4096.0), lod0(), &dp());
    let nw = mesh_buildings(std::slice::from_ref(&b), &p, ClipRect::quadrant(4096.0, 0), lod0(), &dp());
    let ne = mesh_buildings(std::slice::from_ref(&b), &p, ClipRect::quadrant(4096.0, 1), lod0(), &dp());
    let sw = mesh_buildings(std::slice::from_ref(&b), &p, ClipRect::quadrant(4096.0, 2), lod0(), &dp());
    assert!(sw.is_empty());
    // Each half: 3 outer walls (seam wall skipped) + 2 roof triangles.
    assert_eq!(nw.triangle_count(), 8);
    assert_eq!(ne.triangle_count(), 8);
    assert_winding(&nw);
    assert_winding(&ne);
    let roof_area = |m: &MeshData| -> f32 {
        m.indices
            .as_chunks::<3>()
            .0
            .iter()
            .filter(|t| m.normals[t[0] as usize][1] > 0.5)
            .map(|t| {
                let [a, b, c] = [t[0] as usize, t[1] as usize, t[2] as usize];
                let g = cross(sub(m.positions[c], m.positions[a]), sub(m.positions[b], m.positions[a]));
                dot(g, g).sqrt() / 2.0
            })
            .sum()
    };
    let total = roof_area(&whole);
    assert!((roof_area(&nw) + roof_area(&ne) - total).abs() / total < 1e-4);
}

#[test]
fn simplify_keeps_locked_vertices() {
    let pts = vec![[0.0, 0.0], [5.0, 0.01], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
    let (mut keep, mut out) = (Vec::new(), Vec::new());
    simplify_ring(&pts, &[false; 5], 1.0, &mut keep, &mut out);
    assert_eq!(out, vec![0, 2, 3, 4]);
    simplify_ring(&pts, &[false, true, false, false, false], 1.0, &mut keep, &mut out);
    assert!(out.contains(&1));
}

/// Roof area (m2, flat roofs facing up) and wall area (m2) of a mesh.
fn roof_wall_m2(m: &MeshData) -> (f64, f64) {
    let (mut roof, mut wall) = (0.0f64, 0.0f64);
    for t in m.indices.as_chunks::<3>().0 {
        let [a, b, c] = [t[0] as usize, t[1] as usize, t[2] as usize];
        let g = cross(sub(m.positions[c], m.positions[a]), sub(m.positions[b], m.positions[a]));
        let area = (dot(g, g) as f64).sqrt() / 2.0;
        if m.normals[a][1] > 0.5 {
            roof += area;
        } else if m.normals[a][1].abs() < 0.5 {
            wall += area;
        }
    }
    (roof, wall)
}

/// Holes reaching the clip rect (courtyards cut by a tile or chunk edge): the roof is
/// outer ∩ rect − holes ∩ rect, courtyard walls stay, and the four z16 chunks of a z15 tile
/// add up to the tile (roof and walls).
#[test]
fn courtyards_crossing_the_clip_rect_stay_open() {
    use crate::polyclip::fixtures;
    let key = tile_for(VILNIUS.0, VILNIUS.1, 15);
    let (lat, lon) = key.center_lat_lon();
    let parent = TileProjector::new(key, Frame::new(lat, lon), 4096.0);
    for (name, outer, holes) in fixtures::cases() {
        let b = bld("yes", Some(30.0), None, vec![fixtures::rings(outer, &holes)]);
        let whole = mesh_buildings(std::slice::from_ref(&b), &parent, parent.rect(), lod1(), &dp());
        let (roof, wall) = roof_wall_m2(&whole);
        let expect = fixtures::expected_m2(&parent, [0.0, 0.0, 4096.0, 4096.0], outer, &holes);
        assert!((roof - expect).abs() / expect < 1e-6, "{name}: tile roof {roof} vs {expect}");
        assert_winding(&whole);
        let (mut roof4, mut wall4) = (0.0, 0.0);
        for q in 0..4u32 {
            let child = crate::geo::TileKey::new(key.z + 1, key.x * 2 + (q & 1), key.y * 2 + (q >> 1));
            let p = parent.cut(child).unwrap();
            let r = p.rect();
            let m = mesh_buildings(std::slice::from_ref(&b), &p, r, lod0(), &dp());
            let (rf, wl) = roof_wall_m2(&m);
            let e = fixtures::expected_m2(&p, [r.x0, r.y0, r.x1, r.y1], outer, &holes);
            assert!((rf - e).abs() <= 1e-6 * e.max(1.0), "{name} child {q}: roof {rf} vs {e}");
            assert_winding(&m);
            roof4 += rf;
            wall4 += wl;
        }
        // Exact in tile pixels (`expected_m2` checks it); in ENU the tile lacks the chunk seam
        // vertices and a straight pixel edge bends slightly (~2e-6 of the area at z15): 1e-5.
        assert!((roof4 - roof).abs() / roof < 1e-5, "{name}: chunks {roof4} vs tile {roof}");
        // Known limit (as before for outer walls): a wall lying exactly on a chunk seam is
        // treated as the seam and dropped in both chunks, so a courtyard edge on x = 2048
        // has no wall in the chunks; the walls only add up where no hole edge is on a seam.
        let edge_on_seam = holes.iter().any(|h| h.contains(&2048.0));
        if !edge_on_seam {
            assert!((wall4 - wall).abs() / wall < 1e-5, "{name}: chunk walls {wall4} vs tile {wall}");
        } else {
            assert!(wall4 < wall, "{name}");
        }
    }
    // The "one edge" courtyard (300 x 200 px) has its full walls in the tile (1 % allows
    // for the scale change across the tile); the chunk sum above equals the tile, so its
    // walls stay in both chunks where x = 2048 cuts it, and no seam gets one.
    let (outer, holes) = (fixtures::cases()[0].1, fixtures::cases()[0].2.clone());
    let b = bld("yes", Some(30.0), None, vec![fixtures::rings(outer, &holes)]);
    let solid = bld("yes", Some(30.0), None, vec![fixtures::rings(outer, &[])]);
    let ring_m = 2.0 * (300.0 + 200.0) * parent.m_per_px();
    let (_, w_hole) = roof_wall_m2(&mesh_buildings(std::slice::from_ref(&b), &parent, parent.rect(), lod1(), &dp()));
    let (_, w_solid) = roof_wall_m2(&mesh_buildings(std::slice::from_ref(&solid), &parent, parent.rect(), lod1(), &dp()));
    assert!(((w_hole - w_solid) - ring_m * 30.0).abs() / (ring_m * 30.0) < 1e-2, "{w_hole} - {w_solid}");
}

#[test]
fn courtyard_clip_is_deterministic() {
    use crate::polyclip::fixtures;
    let (p, _) = setup();
    for (_, outer, holes) in fixtures::cases() {
        let b = bld("yes", Some(30.0), None, vec![fixtures::rings(outer, &holes)]);
        for q in 0..4 {
            let r = ClipRect::quadrant(4096.0, q);
            assert_eq!(mesh_buildings(std::slice::from_ref(&b), &p, r, lod0(), &dp()), mesh_buildings(std::slice::from_ref(&b), &p, r, lod0(), &dp()));
        }
    }
}
