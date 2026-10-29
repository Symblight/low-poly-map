//! The settled cover, the sibling-swap guarantees in flight, hysteresis at the band edges, the
//! screen-space-error edges, light-mode switches and first fill / churn.

use super::*;

#[test]
fn settled_cover_is_exact_at_reference_altitudes() {
    for (alt, ring_below) in [(300.0, 0usize), (1000.0, 1), (3000.0, 2), (7000.0, 2), (8000.0, 3)] {
        let mut h = Harness::new();
        let cam = [0.0, 0.0, alt];
        let frames = h.settle(cam, 20_000);
        let per = check_settled_cover(&h, cam);
        let v = h.view(cam);
        let under = h.g.tile_at(v.wx, v.wy)[0];
        assert_eq!(ring_for_zoom(under.z), Some(ring_below), "alt {alt}: below is {under}");
        eprintln!("alt {alt}: settled in {frames} frames, visible per ring {per:?}, uploaded {}", h.g.uploaded.len());
        // Requested == delivered (guarantee 2): every tile ever requested was delivered.
        assert_eq!(h.ever_requested, h.ever_delivered, "alt {alt}");
        // No R0 at 1000 m and above; nothing finer than R2 at 3000 m.
        if alt >= 1000.0 {
            assert_eq!(per[0], 0);
        }
        if alt >= 3000.0 {
            assert_eq!(per[1], 0);
        }
    }
}

#[test]
fn scripted_flight_keeps_the_sibling_swap_guarantees() {
    let mut h = Harness::new();
    let dt = 1.0 / 30.0;
    let mut cam = [0.0, 0.0, 300.0];
    h.settle(cam, 20_000);
    let mut camera_holes = 0usize;
    let mut unloads = 0usize;
    // Climb 300 -> 8000 m at 60 m/s, hold, descend at 60 m/s, then 250 m/s east at 300 m.
    let steps: [([f64; 3], usize, &str); 5] = [
        ([0.0, 0.0, 60.0], (7700.0 / 60.0 / dt) as usize, "climb"),
        ([0.0, 0.0, 0.0], 300, "hold"),
        ([0.0, 0.0, -60.0], (7700.0 / 60.0 / dt) as usize, "descent"),
        ([0.0, 0.0, 0.0], 300, "hold low"),
        ([250.0, 0.0, 0.0], (60.0 / dt) as usize, "250 m/s"),
    ];
    let mut r0_under = 0usize;
    let mut cruise_frames = 0usize;
    for (vel, n, name) in steps {
        for _ in 0..n {
            for a in 0..3 {
                cam[a] += vel[a] * dt;
            }
            let (_, un) = h.frame(cam, vel);
            unloads += un.len();
            h.check_frame(&un, name);
            let v = h.view(cam);
            let under = h.g.tile_at(v.wx, v.wy);
            // One tile, or the two sides of a fade.
            let fading_pair = under.len() == 2 && under.iter().all(|k| h.g.fades.contains_key(k));
            if under.len() != 1 && !fading_pair {
                camera_holes += 1;
            }
            if name == "250 m/s" {
                cruise_frames += 1;
                if under.iter().any(|k| k.z == LEAF_ZOOM) {
                    r0_under += 1;
                }
            }
        }
    }
    h.settle(cam, 20_000);
    check_settled_cover(&h, cam);
    assert_eq!(h.ever_requested, h.ever_delivered);
    assert_eq!(camera_holes, 0, "point under the camera uncovered");
    eprintln!(
        "flight: {} deliveries ({} light re-deliveries), {unloads} unloads ({} refined parents dropped, {} coarsening swaps), R0 under camera {r0_under}/{cruise_frames} frames at 250 m/s",
        h.delivered_total, h.redelivered_total, h.parent_drops, h.coarsen_swaps
    );
    eprintln!("flight: {} refine fades, {} coarsen fades", h.refine_fades, h.coarsen_fades);
    assert!(h.parent_drops > 0 && h.coarsen_swaps > 0);
    assert!(h.refine_fades > 0 && h.coarsen_fades > 0);
    assert!(h.redelivered_total > 0, "crossing 3300 m re-delivers R2/R3 lights");
    assert!(r0_under * 10 >= cruise_frames * 9);
}

#[test]
fn holding_at_band_edges_does_not_flip_flop() {
    // Altitudes right at an edge for the tile below (fake heights 20..110 m).
    for alt in [650.0, 2560.0, 7050.0] {
        hold_at_edge_without_flip_flop(Harness::new, alt);
    }
}

/// api 1.10: the same hold with a 30 deg view: the SSE edges move out by tan 30 / tan 15 and the
/// hysteresis keeps holding the cover steady at the moved edges.
#[test]
fn holding_at_sse_edges_of_a_narrow_view_does_not_flip_flop() {
    let view = crate::rings::LodView::new(30.0, 1080.0, [0.0, 0.0, -1.0]).unwrap();
    let edges = bands().edges_for(&view, 1.0);
    let narrow = || {
        let mut h = Harness::new();
        h.s.set_lod_view(&view, 1.0);
        h
    };
    for edge in &edges[..3] {
        hold_at_edge_without_flip_flop(narrow, edge + 50.0);
    }
}

/// Circle 60 m around a point at `alt` with +-20 m altitude jitter: after one settle lap no tile
/// enters and leaves the cover, and the hold exercises the hysteresis band.
fn hold_at_edge_without_flip_flop(make: impl Fn() -> Harness, alt: f64) {
    {
        let mut h = make();
        let pos = |t: f64| {
            let a = t * 0.2;
            [60.0 * a.cos(), 60.0 * a.sin(), alt + 20.0 * (t * 0.7).sin()]
        };
        let dt = 1.0 / 30.0;
        let lap = (2.0 * std::f64::consts::PI / 0.2 / dt) as usize;
        let mut t = 0.0;
        // One lap to settle the cover (plus whatever loading needs).
        for _ in 0..lap {
            t += dt;
            h.frame(pos(t), [0.0; 3]);
        }
        for _ in 0..2000 {
            if h.s.settled() {
                break;
            }
            t += dt;
            h.frame(pos(t), [0.0; 3]);
        }
        let tracked = |h: &Harness| h.s.entries().map(|e| e.key).collect::<HashSet<_>>();
        let before = tracked(&h);
        // A flip-flop is a tile that enters and leaves (or leaves and re-enters) the cover.
        let mut events: std::collections::HashMap<TileKey, Vec<&str>> = Default::default();
        let mut prev = before.clone();
        let mut held_by_hysteresis = 0usize;
        let edge_m = h.s.layer().edge_m;
        for _ in 0..3 * lap {
            t += dt;
            let (_, un) = h.frame(pos(t), [0.0; 3]);
            h.check_frame(&un, "hold");
            let views = h.s.views(pos(t), [0.0; 3]);
            for e in h.s.entries().filter(|e| e.split) {
                let d = views[e.ring - 1].slant_m(e.key, e.height.map_or(bands().default_tile_height_m, |x| x as f64));
                if d >= edge_m[e.ring - 1] {
                    held_by_hysteresis += 1;
                }
            }
            let now = tracked(&h);
            for k in now.difference(&prev) {
                events.entry(*k).or_default().push("in");
            }
            for k in prev.difference(&now) {
                events.entry(*k).or_default().push("out");
            }
            prev = now;
        }
        let flips: Vec<_> = events.iter().filter(|(_, v)| v.len() > 1).collect();
        eprintln!(
            "alt {alt}: {} tiles changed once after the settle lap, {} flip-flops, {held_by_hysteresis} split-node frames past the nominal edge",
            events.len(),
            flips.len()
        );
        assert!(held_by_hysteresis > 0, "alt {alt}: the hold never exercised the hysteresis band");
        assert!(flips.is_empty(), "alt {alt}: flip-flop {flips:?}");
    }
}

/// api 1.10 equivalence: the SSE cover at the default view (1080 px, 60 deg, tau 4) delivers and
/// unloads exactly what the fixed 600 / 2500 / 7000 / 25000 m edges did, frame by frame, on the
/// settle + climb / descent / 250 m/s flight.
#[test]
fn sse_cover_at_the_default_view_equals_the_fixed_edges() {
    let mut a = Harness::new();
    a.s.set_lod_view(&crate::rings::LodView::default(), 1.0);
    let mut b = Harness::new();
    b.s.layer.edge_m[..RING_COUNT].copy_from_slice(&[600.0, 2500.0, 7000.0, 25_000.0]);
    let dt = 1.0 / 30.0;
    let mut cam = [0.0, 0.0, 300.0];
    let steps: [([f64; 3], usize); 6] = [
        ([0.0, 0.0, 0.0], 600),
        ([0.0, 0.0, 60.0], (7700.0 / 60.0 / dt) as usize),
        ([0.0, 0.0, -60.0], (7700.0 / 60.0 / dt) as usize),
        ([250.0, 0.0, 0.0], (40.0 / dt) as usize),
        ([-120.0, 90.0, 5.0], (30.0 / dt) as usize),
        ([0.0, 0.0, 0.0], 300),
    ];
    let mut frames = 0usize;
    for (vel, n) in steps {
        for _ in 0..n {
            for i in 0..3 {
                cam[i] += vel[i] * dt;
            }
            let (ga, ua) = a.frame(cam, vel);
            let (gb, ub) = b.frame(cam, vel);
            assert_eq!(ga, gb, "frame {frames}: deliveries differ");
            let sorted = |mut v: Vec<TileKey>| {
                v.sort();
                v
            };
            assert_eq!(sorted(ua), sorted(ub), "frame {frames}: unloads differ");
            let fades = |h: &Harness| {
                let mut f: Vec<(TileKey, u32)> = h.fades.iter().map(|&(k, x)| (k, x.to_bits())).collect();
                f.sort();
                f
            };
            assert_eq!(fades(&a), fades(&b), "frame {frames}: fades differ");
            frames += 1;
        }
    }
    assert_eq!(a.deliveries, b.deliveries);
    assert!(a.delivered_total > 1000, "the flight exercised the cover ({} deliveries)", a.delivered_total);
}

/// api 1.10: the view moves the cover. At 1000 m over the city the default view has no R0 under
/// the camera (R0 edge 600 m); a 30 deg view (R0 edge ~1293 m) has; quality 2 (R0 edge 300 m)
/// at 400 m has none where the default has R0.
#[test]
fn set_lod_view_moves_the_settled_cover() {
    let under_ring = |h: &mut Harness, cam: [f64; 3]| {
        h.settle(cam, 20_000);
        let v = h.view(cam);
        let under = h.g.tile_at(v.wx, v.wy);
        assert_eq!(under.len(), 1);
        ring_for_zoom(under[0].z).unwrap()
    };
    let narrow = crate::rings::LodView::new(30.0, 1080.0, [0.0, 0.0, -1.0]).unwrap();
    let mut h = Harness::new();
    assert_eq!(under_ring(&mut h, [0.0, 0.0, 1000.0]), 1);
    h.s.set_lod_view(&narrow, 1.0);
    assert_eq!(under_ring(&mut h, [0.0, 0.0, 1000.0]), 0, "fov 30: R0 reaches past 1000 m");
    let mut h = Harness::new();
    assert_eq!(under_ring(&mut h, [0.0, 0.0, 400.0]), 0);
    h.s.set_lod_view(&crate::rings::LodView::default(), 2.0);
    assert_eq!(under_ring(&mut h, [0.0, 0.0, 400.0]), 1, "quality 2: R0 edge 300 m");
    // Back to the default: the edges are exactly the profile's again.
    h.s.set_lod_view(&crate::rings::LodView::default(), 1.0);
    assert_eq!(h.s.layer().edge_m[..RING_COUNT], bands().edge_m);
    // The terrain layer keeps its fixed bands.
    let (p, t) = terrain_setup(|_| {});
    let mut ts = Scheduler::new_terrain(Frame::new(VILNIUS.0, VILNIUS.1), p, t);
    let before = ts.layer().edge_m;
    ts.set_lod_view(&narrow, 2.0);
    assert_eq!(ts.layer().edge_m, before);
}

#[test]
fn light_mode_switch_redelivers_with_hysteresis_and_rate_limit() {
    let mut h = Harness::new();
    h.settle([0.0, 0.0, 3000.0], 20_000);
    assert!(!h.s.clustered());
    let r23 = h.s.entries().filter(|e| e.ring >= 2).count();
    // 3200 m: inside the hysteresis band, nothing happens.
    h.settle([0.0, 0.0, 3200.0], 20_000);
    assert_eq!(h.redelivered_total, 0);
    // 3400 m: clustered; every held R2/R3 tile comes again, at most one per frame.
    let cam = [0.0, 0.0, 3400.0];
    let mut frames = 0;
    let base = h.redelivered_total;
    loop {
        let before = h.redelivered_total;
        let (_, un) = h.frame(cam, [0.0; 3]);
        h.check_frame(&un, "relight");
        assert!(h.redelivered_total - before <= 1);
        frames += 1;
        if h.s.settled() {
            break;
        }
        assert!(frames < 20_000);
    }
    assert!(h.s.clustered());
    let re = h.redelivered_total - base;
    let still = h.s.entries().filter(|e| e.ring >= 2 && e.held).count();
    assert!(re >= still.min(r23) && re > 0, "re-delivered {re}, held R2/R3 {still}");
    assert!(h.s.entries().filter(|e| e.held && e.ring >= 2).all(|e| e.lights_mode));
    // Back to 2800 m: still clustered (hysteresis); 2600 m: single lights again.
    h.settle([0.0, 0.0, 2800.0], 20_000);
    assert!(h.s.clustered());
    h.settle([0.0, 0.0, 2600.0], 20_000);
    assert!(!h.s.clustered());
    assert!(h.s.entries().filter(|e| e.held && e.ring >= 2).all(|e| !e.lights_mode));
}
/// First fill at a static camera, both layers: (1) every fresh delivery is the nearest
/// deliverable ready tile (group distance from the camera, see `order_key`), whatever its
/// ring; (2) no fade at all (each stand-in swaps out when its 4 children are in), so the
/// finest tile under the camera arrives within a few frames of its parent instead of one
/// fade cap per level; (3) the visibility rule holds on every frame and the fill settles
/// with an exact cover.
#[test]
fn first_fill_is_nearest_first_without_fades() {
    let (p, t) = terrain_setup(|_| {});
    for (name, mut h) in [("world", Harness::new()), ("terrain", terrain_harness(p, t.clone()))] {
        let cam = [0.0, 0.0, 300.0];
        let mut under_at: Vec<(u8, usize)> = Vec::new();
        let mut frames = 0;
        h.check_order = true;
        for f in 0..20_000 {
            let (got, un) = h.frame(cam, [0.0; 3]);
            h.check_frame(&un, name);
            assert_eq!(h.s.fades().count(), 0, "{name} f{f}: a first fill has no fades");
            assert!(h.fades.is_empty(), "{name} f{f}: poll_fades lists {:?}", &h.fades[..h.fades.len().min(4)]);
            for k in got {
                if k == crate::geo::tile_for(VILNIUS.0, VILNIUS.1, k.z) {
                    under_at.push((k.z, f));
                }
            }
            frames = f;
            if h.s.settled() {
                break;
            }
        }
        assert!(h.s.settled(), "{name}: did not settle");
        assert!(h.g.check().is_empty());
        assert!(h.first_fill_swaps > 0 && h.refine_fades == 0 && h.coarsen_fades == 0, "{name}");
        let leaf = under_at.iter().find(|u| u.0 == h.l.leaf_zoom).map(|u| u.1).expect("leaf under the camera");
        let root = under_at.iter().find(|u| u.0 == h.l.root_zoom).map(|u| u.1).expect("root under the camera");
        let cap = (fade_cap().as_secs_f64() / DT).round() as usize;
        let levels = (h.l.leaf_zoom - h.l.root_zoom) as usize;
        eprintln!(
            "{name} first fill: root under the camera at frame {root}, leaf at {leaf} ({levels} levels; one fade cap per level = {} frames more), settled at {frames}, {} deliveries",
            levels * cap,
            h.delivered_total
        );
        // FakeWorker: fetch 3 + decode 2 frames per tile, 2 deliveries per frame; the old
        // rule waited a fade cap (0.75 s = 22 frames) per level on top.
        assert!(leaf - root <= levels * 8, "{name}: leaf {leaf} vs root {root}");
    }
}

/// Cover churn, both layers: a static camera never unloads a tile and delivers it again
/// (first fill and 10 s after it: nothing but the stand-ins leaves, nothing comes back, and
/// once settled nothing moves at all); in level flight north at 80 and 250 m/s every key
/// delivered again after an unload is a coarsen swap (guarantee 4: its 4 children are
/// unloaded within the fade cap), never a flip-flop of the cover.
#[test]
fn no_cover_churn_static_or_level_flight() {
    let (p, t) = terrain_setup(|_| {});
    for terrain in [false, true] {
        let mk = || if terrain { terrain_harness(p.clone(), t.clone()) } else { Harness::new() };
        let name = if terrain { "terrain" } else { "world" };
        // Static: first fill, then 10 s more.
        let mut h = mk();
        let cam = [0.0, 0.0, 300.0];
        let mut unloaded: HashSet<TileKey> = HashSet::new();
        let mut settled_at = None;
        for f in 0..(20_000usize) {
            let (got, un) = h.frame(cam, [0.0; 3]);
            h.check_frame(&un, name);
            for k in &got {
                assert!(!unloaded.contains(k), "{name} static f{f}: {k} delivered again after an unload");
            }
            for k in &un {
                // Only first-fill stand-ins leave (their children are uploaded).
                assert!(children(*k).iter().all(|c| h.g.uploaded.contains(c)), "{name} static f{f}: {k} unloaded");
            }
            unloaded.extend(un.iter().copied());
            if settled_at.is_none() && h.s.settled() {
                settled_at = Some(f);
            }
            if let Some(s0) = settled_at {
                if f > s0 {
                    assert!(got.is_empty() && un.is_empty(), "{name} static f{f}: settled cover moved ({got:?} / {un:?})");
                }
                if f >= s0 + (10.0 / DT) as usize {
                    break;
                }
            }
        }
        assert!(settled_at.is_some(), "{name}: static camera never settled");
        // Level flight north.
        for speed in [80.0, 250.0] {
            let mut h = mk();
            let mut cam = [0.0, 0.0, 300.0];
            h.settle(cam, 20_000);
            let cap = (fade_cap().as_secs_f64() / DT).ceil() as usize + 2;
            let mut unloaded_at: AHashMap<TileKey, usize> = AHashMap::new();
            let mut expect: Vec<(TileKey, usize)> = Vec::new();
            let (mut again, mut swaps) = (0, 0);
            let frames = (20.0 / DT) as usize;
            for f in 0..frames {
                cam[1] += speed * DT;
                let (got, un) = h.frame(cam, [0.0, speed, 0.0]);
                h.check_frame(&un, name);
                for k in got {
                    if h.fresh.contains(&k) && unloaded_at.contains_key(&k) {
                        again += 1;
                        expect.push((k, f));
                    }
                }
                for k in un {
                    unloaded_at.insert(k, f);
                }
                expect.retain(|&(k, f0)| {
                    let done = children(k).iter().all(|c| unloaded_at.get(c).is_some_and(|&u| u >= f0));
                    if done {
                        swaps += 1;
                        return false;
                    }
                    assert!(f <= f0 + cap, "{name} {speed} m/s: {k} delivered again at f{f0} without a coarsen swap");
                    true
                });
            }
            eprintln!("{name} level flight {speed} m/s: {again} keys delivered again after an unload, {swaps} of them coarsen swaps");
        }
    }
}
