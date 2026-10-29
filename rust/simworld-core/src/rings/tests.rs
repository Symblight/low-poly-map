use super::*;
use crate::geo::{tile_for, Frame};

const VILNIUS: (f64, f64) = (54.6872, 25.2797);

fn bands() -> BandParams {
    *crate::profile::Profile::default().bands()
}

#[test]
fn ring_zoom_mapping_and_family() {
    assert_eq!(ring_for_zoom(16), Some(0));
    assert_eq!(ring_for_zoom(13), Some(3));
    assert_eq!(ring_for_zoom(12), None);
    assert_eq!(ring_for_zoom(17), None);
    assert_eq!(zoom_for_ring(2), 14);
    let k = TileKey::new(14, 9342, 5207);
    let c = children(k);
    assert_eq!(c[0], TileKey::new(15, 18684, 10414));
    assert_eq!(c[3], TileKey::new(15, 18685, 10415));
    for ch in c {
        assert_eq!(parent(ch), Some(k));
        assert_eq!(siblings(ch), c);
    }
    assert_eq!(parent(TileKey::new(13, 1, 1)), None);
}

#[test]
fn slant_distance_matches_enu_geometry() {
    let f = Frame::new(VILNIUS.0, VILNIUS.1);
    let v = ViewPoint::new(VILNIUS.0, VILNIUS.1, 300.0);
    // Containing tile: horizontal 0; slant = altitude above the box top.
    let k = tile_for(VILNIUS.0, VILNIUS.1, 16);
    assert_eq!(v.horizontal_m(k), 0.0);
    assert!((v.slant_m(k, 60.0) - 240.0).abs() < 1e-9);
    assert!((v.slant_m(k, 500.0)).abs() < 1e-9, "camera inside the box height");
    // Far tiles: compare against the exact ENU rectangle distance (< 1 % error).
    for (dx, dy) in [(5i64, 0i64), (0, 7), (-6, -3), (2, 4)] {
        let k13 = tile_for(VILNIUS.0, VILNIUS.1, 13);
        let t = TileKey::new(13, (k13.x as i64 + dx) as u32, (k13.y as i64 + dy) as u32);
        let r = f.tile_rect(t);
        let ex = r.min_e.max(-r.max_e).max(0.0);
        let ey = r.min_n.max(-r.max_n).max(0.0);
        let exact = ex.hypot(ey);
        let approx = v.horizontal_m(t);
        assert!((approx - exact).abs() / exact < 0.01, "{t}: {approx} vs {exact}");
    }
}

#[test]
fn look_ahead_includes_vertical_and_clamps_3d() {
    // 250 m/s east * 20 s = 5 km, clamped to 300 m (half of R0's 600 m).
    let b = bands();
    let p = look_ahead([10.0, 20.0, 300.0], [250.0, 0.0, 0.0], b.look_ahead_clamp_m(0), b.look_ahead_s);
    assert!((p[0] - 310.0).abs() < 1e-9 && (p[1] - 20.0).abs() < 1e-9 && p[2] == 300.0);
    // Climbing 10 m/s: 200 m up, not clamped for R1.
    let p = look_ahead([0.0, 0.0, 300.0], [0.0, 0.0, 10.0], b.look_ahead_clamp_m(1), b.look_ahead_s);
    assert!((p[2] - 500.0).abs() < 1e-9);
    // Diagonal: the 3D length is clamped, direction kept.
    let p = look_ahead([0.0, 0.0, 1000.0], [30.0, 0.0, -40.0], 250.0, b.look_ahead_s);
    assert!((p[0] - 150.0).abs() < 1e-9 && (p[2] - 800.0).abs() < 1e-9);
    // Diving below ground clamps to 0.
    let p = look_ahead([0.0, 0.0, 50.0], [0.0, 0.0, -100.0], 1000.0, b.look_ahead_s);
    assert_eq!(p[2], 0.0);
}

#[test]
fn hysteresis_band() {
    let b = bands();
    assert!(b.inside(599.0, 600.0, false));
    assert!(!b.inside(600.0, 600.0, false));
    assert!(b.inside(680.0, 600.0, true));
    assert!(!b.inside(691.0, 600.0, true));
}

#[test]
fn split_rules_by_altitude_directly_below() {
    let c13 = tile_for(VILNIUS.0, VILNIUS.1, 13);
    let c14 = tile_for(VILNIUS.0, VILNIUS.1, 14);
    let c15 = tile_for(VILNIUS.0, VILNIUS.1, 15);
    let at = |alt| ViewPoint::new(VILNIUS.0, VILNIUS.1, alt);
    let b = bands();
    let h = b.default_tile_height_m;
    // 300 m: everything below splits down to z16.
    assert!(split_wanted(&b, &at(300.0), c15, 1, h, false));
    // 1000 m: no R0 (slant >= 940 m), R1 yes.
    assert!(!split_wanted(&b, &at(1000.0), c15, 1, h, false));
    assert!(split_wanted(&b, &at(1000.0), c14, 2, h, false));
    // 3000 m: R2 below, no R1.
    assert!(!split_wanted(&b, &at(3000.0), c14, 2, h, false));
    assert!(split_wanted(&b, &at(3000.0), c13, 3, h, false));
    // 8000 m: lights only.
    assert!(!split_wanted(&b, &at(8000.0), c13, 3, h, false));
    // R0 never splits.
    assert!(!split_wanted(&b, &at(0.0), tile_for(VILNIUS.0, VILNIUS.1, 16), 0, h, true));
    // Roots: 20 km up still in, 26 km out.
    assert!(root_wanted(&b, &at(20_000.0), c13, h, false));
    assert!(!root_wanted(&b, &at(26_000.0), c13, h, false));
    assert!(root_wanted(&b, &at(26_000.0), c13, h, true));
}

#[test]
fn root_candidates_cover_the_slant_disk() {
    for alt in [300.0, 7000.0, 20_000.0] {
        let v = ViewPoint::new(VILNIUS.0, VILNIUS.1, alt);
        let mut c = Vec::new();
        v.root_candidates(&bands(), &mut c);
        let set: std::collections::HashSet<_> = c.iter().copied().collect();
        // Every z13 tile within reach whose slant distance is <= 28.75 km is a candidate.
        let ct = tile_for(VILNIUS.0, VILNIUS.1, 13);
        for dy in -12i64..=12 {
            for dx in -12i64..=12 {
                let k = TileKey::new(13, (ct.x as i64 + dx) as u32, (ct.y as i64 + dy) as u32);
                if v.slant_m(k, 0.0) <= bands().edge_m[3] * bands().hysteresis {
                    assert!(set.contains(&k), "alt {alt}: {k} missing");
                }
            }
        }
    }
    let mut c = Vec::new();
    ViewPoint::new(VILNIUS.0, VILNIUS.1, 30_000.0).root_candidates(&bands(), &mut c);
    assert!(c.is_empty());
}

/// api 1.10: the SSE refine distances. The default view and profile give the old fixed
/// edges; fov 30 deg pushes them out by tan 30 / tan 15, H 540 and quality 2 halve them; the
/// outer limit stays and clamps.
#[test]
fn sse_refine_distances_follow_view_and_tau() {
    let b = bands();
    let def = LodView::default();
    let e = b.edges_for(&def, 1.0);
    assert_eq!(e, [600.0, 2500.0, 7000.0, 25_000.0], "bit for bit the old edges");
    assert_eq!(e, b.edge_m, "BandParams::edge_m = the default view");
    // The formula itself: SSE at the refine distance is exactly tau.
    let d = sse_refine_distance(10.0, &def, 4.0);
    let sse = 10.0 * 1080.0 / (2.0 * d * (30f64).to_radians().tan());
    assert!((sse - 4.0).abs() < 1e-9);
    let narrow = LodView::new(30.0, 1080.0, [0.0, 0.0, -1.0]).unwrap();
    let k = (30f64).to_radians().tan() / (15f64).to_radians().tan();
    let en = b.edges_for(&narrow, 1.0);
    for r in 0..3 {
        let ratio = en[r] / e[r];
        assert!((ratio - k).abs() / k < 0.01, "ring {r}: {ratio} vs {k}");
    }
    assert_eq!(en[3], 25_000.0, "outer limit unchanged");
    let small = LodView::new(60.0, 540.0, [0.0, 0.0, -1.0]).unwrap();
    let q2 = b.edges_for(&def, 2.0);
    for (r, eh) in b.edges_for(&small, 1.0).iter().enumerate().take(3) {
        assert!((eh / e[r] - 0.5).abs() < 1e-12, "H 540 halves ring {r}");
        assert!((q2[r] / e[r] - 0.5).abs() < 1e-12, "quality 2 halves ring {r}");
    }
    // Clamped to the outer limit (a 5 deg zoom would put R2's edge at ~85 km).
    let zoom = LodView::new(5.0, 1080.0, [1.0, 0.0, 0.0]).unwrap();
    assert_eq!(b.edges_for(&zoom, 1.0)[2], 25_000.0);
    // Validation and normalization.
    assert!(LodView::new(0.5, 1080.0, [0.0, 0.0, -1.0]).is_none());
    assert!(LodView::new(180.0, 1080.0, [0.0, 0.0, -1.0]).is_none());
    assert!(LodView::new(f64::NAN, 1080.0, [0.0, 0.0, -1.0]).is_none());
    assert!(LodView::new(60.0, 0.0, [0.0, 0.0, -1.0]).is_none());
    assert!(LodView::new(60.0, 1080.0, [0.0, 0.0, 0.0]).is_none());
    assert!(LodView::new(60.0, 1080.0, [f64::INFINITY, 0.0, 0.0]).is_none());
    assert_eq!(LodView::new(60.0, 1080.0, [0.0, 3.0, 4.0]).unwrap().forward, [0.0, 0.6, 0.8]);
}

#[test]
fn priority_finer_band_first_then_distance() {
    let a = (0usize, 900.0, TileKey::new(16, 1, 1));
    let b = (1usize, 10.0, TileKey::new(15, 1, 1));
    let c = (0usize, 100.0, TileKey::new(16, 2, 2));
    let mut v = vec![a, b, c];
    v.sort_by(|x, y| priority_cmp(*x, *y));
    assert_eq!(v, vec![c, a, b]);
}
