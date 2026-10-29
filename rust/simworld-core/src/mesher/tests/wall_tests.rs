//! Wall UV: corners, runs, window columns, floors (docs/spec-night-windows.md W3).

use super::*;
use crate::mesher::walls::{is_corner, ring_runs};

/// A ring given in ENU meters (x east, y north), offset to pixel (1500, 1500).
fn ring_px(mpp: f64, pts: &[[f64; 2]]) -> Vec<[f64; 2]> {
    pts.iter().map(|p| [1500.0 + p[0] / mpp, 1500.0 - p[1] / mpp]).collect()
}

/// Wall quads of a mesh: (u0, u1, v_top, length in m, quad's first vertex index).
fn wall_quads(m: &MeshData) -> Vec<(f32, f32, f32, f32, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < m.positions.len() {
        if m.normals[i][1] == 0.0 {
            let (p, q) = (m.positions[i], m.positions[i + 1]);
            let len = ((q[0] - p[0]).powi(2) + (q[2] - p[2]).powi(2)).sqrt();
            // Quad layout: [u0, 0], [u1, 0], [u1, top], [u0, top].
            assert_eq!(m.uvs[i][1], 0.0);
            assert_eq!((m.uvs[i + 3][0], m.uvs[i + 2][0]), (m.uvs[i][0], m.uvs[i + 1][0]));
            assert_eq!(m.uvs[i + 2][1], m.uvs[i + 3][1]);
            out.push((m.uvs[i][0], m.uvs[i + 1][0], m.uvs[i + 2][1], len, i));
            i += 4;
        } else {
            i += 1;
        }
    }
    out
}

fn is_int(x: f32) -> bool {
    x == x.round()
}

/// Regular n-gon of radius r (ENU meters), counter-clockwise.
fn ngon(n: usize, r: f64) -> Vec<[f64; 2]> {
    (0..n)
        .map(|i| {
            let a = std::f64::consts::TAU * i as f64 / n as f64;
            [r * a.cos(), r * a.sin()]
        })
        .collect()
}

/// Polyline turn at `v`: incoming along +x with length `la`, outgoing turned by `deg`.
fn turn(deg: f64, la: f64, lb: f64, w: f64) -> bool {
    let a = deg.to_radians();
    is_corner([-la, 0.0], [0.0, 0.0], [lb * a.cos(), lb * a.sin()], w)
}

#[test]
fn corner_rule_angles_and_edge_lengths() {
    // > 35 deg is a corner on any edge length.
    assert!(turn(40.0, 4.0, 4.0, 3.0));
    assert!(turn(40.0, 1.0, 1.0, 3.0));
    assert!(turn(90.0, 4.0, 4.0, 3.0));
    assert!(turn(-40.0, 4.0, 4.0, 3.0), "either turn direction");
    // 20..35 deg: a corner only with both edges >= 2 x width.
    assert!(turn(25.0, 8.0, 8.0, 3.0), "25 deg on two 8 m edges (2 x 3 m = 6 m)");
    assert!(!turn(25.0, 8.0, 5.0, 3.0));
    assert!(!turn(25.0, 5.0, 8.0, 3.0));
    assert!(!turn(22.5, 3.9, 3.9, 3.0), "16-gon vertex on short edges");
    // <= 20 deg never.
    assert!(!turn(15.0, 50.0, 50.0, 3.0));
    assert!(!turn(0.0, 10.0, 10.0, 3.0));
    // 16-gon of radius 10 m: 22.5 deg per vertex, 3.9 m edges -> one run from vertex 0.
    let ring = ngon(16, 10.0);
    let mut runs = Vec::new();
    ring_runs(&ring, &[false; 16], 3.0, &mut runs);
    assert_eq!(runs.len(), 1);
    assert_eq!((runs[0].first, runs[0].edges), (0, 16));
    // The same 16-gon with radius 40 m (15.6 m edges >= 6 m): every vertex is a corner.
    ring_runs(&ngon(16, 40.0), &[false; 16], 3.0, &mut runs);
    assert_eq!(runs.len(), 16);
    // A 40 deg turn between short edges splits a run.
    let ring = vec![[0.0, 0.0], [4.0, 0.0], [4.0 + 4.0 * 40f64.to_radians().cos(), 4.0 * 40f64.to_radians().sin()], [0.0, 12.0]];
    ring_runs(&ring, &[false; 4], 3.0, &mut runs);
    assert_eq!(runs.len(), 4, "{runs:?}");
}

#[test]
fn ring_with_vertex_0_mid_wall_is_rotated_and_seams_end_runs() {
    // A 10 x 8 m rectangle whose vertex 0 lies in the middle of the south wall.
    let ring = vec![[5.0, 0.0], [10.0, 0.0], [10.0, 8.0], [0.0, 8.0], [0.0, 0.0]];
    let mut runs = Vec::new();
    ring_runs(&ring, &[false; 5], 3.0, &mut runs);
    assert_eq!(runs.len(), 4);
    // Iteration starts at the first corner (vertex 1); the south wall is one run of two
    // edges (4 -> 0 -> 1), 10 m long.
    assert_eq!((runs[0].first, runs[0].edges), (1, 1));
    let south = runs.iter().find(|r| r.edges == 2).expect("south run");
    assert_eq!(south.first, 4);
    assert!((south.len - 10.0).abs() < 1e-9);
    // A clip seam (edge 2: (10,8) -> (0,8)) has no wall and ends the runs around it.
    let mut seam = [false; 5];
    seam[2] = true;
    ring_runs(&ring, &seam, 3.0, &mut runs);
    assert_eq!(runs.len(), 3);
    assert!(runs.iter().all(|r| r.first != 2));
    // A cornerless ring with a seam: the runs start next to the seam.
    let circle = ngon(16, 10.0);
    let mut seam = [false; 16];
    seam[5] = true;
    ring_runs(&circle, &seam, 3.0, &mut runs);
    assert_eq!(runs.len(), 1);
    assert_eq!((runs[0].first, runs[0].edges), (6, 15));

    // Through the mesher: the two halves of the south wall share one run of
    // round(10 / 3) = 3 columns, split at 1.5 (not two runs of round(5 / 3) = 2).
    let (p, mpp) = setup();
    let b = bld("apartments", Some(9.0), None, vec![vec![ring_px(mpp, &ring)]]);
    let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), lod0(), &dp());
    let q = wall_quads(&m);
    assert_eq!(q.len(), 5);
    let halves: Vec<_> = q.iter().filter(|w| (w.3 - 5.0).abs() < 0.1).collect();
    assert_eq!(halves.len(), 2);
    let (lo, hi) = (halves[0].0.min(halves[1].0), halves[0].1.max(halves[1].1));
    assert_eq!(hi - lo, 3.0);
    assert!(halves.iter().any(|w| (w.1 - (lo + 1.5)).abs() < 0.02), "{halves:?}");
}

#[test]
fn wall_runs_get_round_len_over_width_columns_ending_on_integers() {
    let (p, mpp) = setup();
    // 20 x 7.4 m residential block (3 m windows): 20 / 3 -> 7, 7.4 / 3 -> 2 columns.
    let ring = ring_px(mpp, &[[0.0, 0.0], [20.0, 0.0], [20.0, 7.4], [0.0, 7.4]]);
    let b = bld("apartments", Some(15.0), None, vec![vec![ring]]);
    let res = resolve(&b, &dp());
    assert_eq!((res.profile, res.windows), (PROFILE_RESIDENTIAL, true));
    let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), lod0(), &dp());
    let mut q = wall_quads(&m);
    assert_eq!(q.len(), 4);
    q.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut next = 0.0;
    for &(u0, u1, v_top, len, _) in &q {
        assert!(is_int(u0) && is_int(u1), "{u0} {u1}");
        assert_eq!(u0, next, "columns continue around the ring");
        assert_eq!(u1 - u0, (len / 3.0).round(), "len {len}");
        assert_eq!(v_top, 5.0, "15 m / 3 m = 5 floors");
        next = u1;
    }
    assert_eq!(next, 2.0 * 7.0 + 2.0 * 2.0);
    // Office (2 m windows): 20 / 2 = 10, 7.4 / 2 -> 4.
    let b = bld("office", Some(15.0), None, vec![vec![ring_px(mpp, &[[0.0, 0.0], [20.0, 0.0], [20.0, 7.4], [0.0, 7.4]])]]);
    let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), lod0(), &dp());
    let total: f32 = wall_quads(&m).iter().map(|w| w.1 - w.0).sum();
    assert_eq!(total, 28.0);
    // A wall shorter than half a window gets no columns: u = -1, v in meters, and the
    // count of the next run is not shifted.
    let b = bld("apartments", Some(12.0), None, vec![vec![ring_px(mpp, &[[0.0, 0.0], [20.0, 0.0], [20.0, 1.2], [0.0, 1.2]])]]);
    let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), lod0(), &dp());
    let q = wall_quads(&m);
    let short: Vec<_> = q.iter().filter(|w| w.3 < 2.0).collect();
    assert_eq!(short.len(), 2);
    assert!(short.iter().all(|w| w.0 == -1.0 && w.1 == -1.0 && w.2 == 12.0), "{short:?}");
    let long: Vec<_> = q.iter().filter(|w| w.3 > 2.0).collect();
    assert!(long.iter().all(|w| w.1 - w.0 == 7.0 && w.2 == 4.0), "{long:?}");
}

#[test]
fn hole_columns_continue_after_the_outer_ring() {
    let (p, mpp) = setup();
    let outer = ring_px(mpp, &[[0.0, 0.0], [30.0, 0.0], [30.0, 30.0], [0.0, 30.0]]);
    let hole = ring_px(mpp, &[[10.0, 10.0], [10.0, 20.0], [20.0, 20.0], [20.0, 10.0]]);
    let b = bld("apartments", Some(9.0), None, vec![vec![outer, hole]]);
    let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), lod0(), &dp());
    let q = wall_quads(&m);
    assert_eq!(q.len(), 8);
    // Emission order: outer ring first (4 x round(30 / 3) = 40 columns), then the hole.
    let outer_max = q[..4].iter().map(|w| w.1).fold(0.0, f32::max);
    let hole_min = q[4..].iter().map(|w| w.0).fold(f32::INFINITY, f32::min);
    let hole_max = q[4..].iter().map(|w| w.1).fold(0.0, f32::max);
    assert_eq!(outer_max, 40.0);
    assert_eq!(hole_min, 40.0, "the hole's first column continues after the outer ring's last");
    assert_eq!(hole_max, 40.0 + 4.0 * 3.0);
}

#[test]
fn floors_are_stretched_to_the_wall_height() {
    let (p, mpp) = setup();
    let ring = ring_px(mpp, &[[0.0, 0.0], [12.0, 0.0], [12.0, 12.0], [0.0, 12.0]]);
    // 10.4 m, floor height 3 m: round(3.47) = 3 floors, top v exactly 3.0.
    let b = bld("apartments", Some(10.4), None, vec![vec![ring.clone()]]);
    assert_eq!(resolve(&b, &dp()).floor_h, 3.0);
    let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), lod0(), &dp());
    assert!(wall_quads(&m).iter().all(|w| w.2 == 3.0));
    // At least one floor: 2.2 m (>= min_window_wall_m 2 m) -> 1 floor.
    let b = bld("apartments", Some(2.2), None, vec![vec![ring.clone()]]);
    let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), lod0(), &dp());
    assert!(wall_quads(&m).iter().all(|w| w.2 == 1.0 && w.0 >= 0.0));
    // A raised part counts its floors from its own bottom: 20 - 8 = 12 m -> 4 floors.
    let mut b = bld("apartments", Some(20.0), None, vec![vec![ring]]);
    b.min_height = 8.0;
    let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), lod0(), &dp());
    assert!(wall_quads(&m).iter().all(|w| w.2 == 4.0));
}

#[test]
fn windowless_walls_get_u_minus_1_and_meters() {
    use crate::mvt::BuildingTags;
    let (p, mpp) = setup();
    let ring = ring_px(mpp, &[[0.0, 0.0], [24.0, 0.0], [24.0, 15.0], [0.0, 15.0]]);
    let windowless = |m: &MeshData, top: f32| {
        let q = wall_quads(m);
        assert_eq!(q.len(), 4);
        for w in q {
            assert_eq!((w.0, w.1), (-1.0, -1.0), "{w:?}");
            assert!((w.2 - top).abs() < 1e-5, "v in meters: {w:?}");
        }
    };
    let mk = |kind: &str, h: f32| bld(kind, Some(h), None, vec![vec![ring.clone()]]);
    // Wall part < 2 m (a raised part 10..11.5 m).
    let mut low = mk("apartments", 11.5);
    low.min_height = 10.0;
    windowless(&mesh_buildings(&[low], &p, ClipRect::tile(4096.0), lod0(), &dp()), 1.5);
    // Dark and landmark profiles.
    let garage = mk("garage", 6.0);
    assert_eq!(resolve(&garage, &dp()).profile, PROFILE_DARK);
    windowless(&mesh_buildings(&[garage], &p, ClipRect::tile(4096.0), lod0(), &dp()), 6.0);
    let castle = mk("castle", 18.0);
    assert_eq!(resolve(&castle, &dp()).profile, PROFILE_LANDMARK);
    windowless(&mesh_buildings(&[castle], &p, ClipRect::tile(4096.0), lod0(), &dp()), 18.0);
    // Industrial has no `windows` block either.
    windowless(&mesh_buildings(&[mk("warehouse", 9.0)], &p, ClipRect::tile(4096.0), lod1(), &dp()), 9.0);
    // windows = no.
    let mut no = mk("apartments", 15.0);
    no.tags = BuildingTags { windows: Some(false), ..Default::default() };
    windowless(&mesh_buildings(&[no], &p, ClipRect::tile(4096.0), lod0(), &dp()), 15.0);
    // Every R2 wall, while the same building has windows at R0 and R1.
    let flats = mk("apartments", 15.0);
    for ring_ix in [0, 1] {
        let m = mesh_tile(std::slice::from_ref(&flats), &p, ring_ix, &dp());
        assert!(wall_quads(&m).iter().all(|w| w.0 >= 0.0 && w.2 == 5.0), "R{ring_ix}");
    }
    windowless(&mesh_tile(std::slice::from_ref(&flats), &p, 2, &dp()), 15.0);
    assert!(!dp().lod(2).unwrap().windows && dp().lod(1).unwrap().windows && dp().lod(0).unwrap().windows);
    // R2 of the whole synthetic city: no wall vertex has u >= 0.
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let k14 = tile_for(VILNIUS.0, VILNIUS.1, 14);
    let p14 = TileProjector::new(k14, frame, 4096.0);
    let r2 = mesh_tile(&crate::synthetic::generate(k14, frame), &p14, 2, &dp());
    let walls = wall_quads(&r2);
    assert!(!walls.is_empty());
    assert!(walls.iter().all(|w| w.0 == -1.0 && w.1 == -1.0));
}

#[test]
fn roof_and_bottom_cap_uv_stay_tile_local_meters() {
    let (p, mpp) = setup();
    let ring = ring_px(mpp, &[[0.0, 0.0], [24.0, 0.0], [24.0, 15.0], [0.0, 15.0]]);
    for (kind, lod) in [("apartments", lod0()), ("castle", lod0()), ("apartments", dp().lod(2).unwrap())] {
        let mut b = bld(kind, Some(18.0), None, vec![vec![ring.clone()]]);
        b.min_height = 6.0;
        let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), lod, &dp());
        let mut caps = [0, 0];
        for i in 0..m.positions.len() {
            let n = m.normals[i][1];
            if n != 0.0 {
                assert_eq!(m.uvs[i], [m.positions[i][0], m.positions[i][2]], "{kind}");
                caps[(n < 0.0) as usize] += 1;
            }
        }
        assert_eq!(caps, [4, 4], "roof and bottom cap of a raised part");
    }
}
