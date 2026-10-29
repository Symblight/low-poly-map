//! The terrain layer (api 1.5): its shape, the swap guarantees, the horizon cull, the detail
//! scale and the (informative) budgets.

use super::*;

#[test]
fn terrain_layer_shape() {
    let (p, t) = terrain_setup(|_| {});
    let s = Scheduler::new_terrain(Frame::new(VILNIUS.0, VILNIUS.1), p.clone(), t);
    let l = s.layer();
    assert_eq!((l.kind, l.root_zoom, l.leaf_zoom, l.rings()), (LayerKind::Terrain, 10, 14, 5));
    assert_eq!(l.edge_m[..5], [2500.0, 7000.0, 25_000.0, 60_000.0, 130_000.0]);
    assert_eq!((l.max_fetch, l.max_mesh, l.default_tile_height_m), (4, 2, 0.0));
    assert_eq!(l.horizon_margin_m, Some(2000.0));
    assert_eq!(l.hysteresis, p.bands().hysteresis);
    assert_eq!(l.ring_for_zoom(12), Some(2));
    assert_eq!(l.parent(TileKey::new(10, 5, 5)), None);
    // The vector layer is exactly the old constants.
    let w = Scheduler::new(Frame::new(VILNIUS.0, VILNIUS.1), true, dp());
    assert_eq!((w.layer().root_zoom, w.layer().leaf_zoom, w.layer().rings()), (ROOT_ZOOM, LEAF_ZOOM, RING_COUNT));
    assert_eq!(w.layer().edge_m[..4], bands().edge_m);
    assert_eq!(w.layer().horizon_margin_m, None);
}
/// The contract's sibling-swap, fade and unload guarantees hold on the terrain layer
/// (per-layer `GodotSim` with root zoom 10): settle, climb to 8 km, descend, cruise at
/// 250 m/s, every frame checked.
#[test]
fn terrain_layer_keeps_the_sibling_swap_guarantees() {
    let (p, t) = terrain_setup(|_| {});
    let mut h = terrain_harness(p, t.clone());
    let dt = 1.0 / 30.0;
    let mut cam = [0.0, 0.0, 300.0];
    h.settle(cam, 20_000);
    let at300 = held_per_ring(&h);
    let mut camera_holes = 0usize;
    let steps: [([f64; 3], usize, &str); 4] = [
        ([0.0, 0.0, 60.0], (7700.0 / 60.0 / dt) as usize, "climb"),
        ([0.0, 0.0, -60.0], (7700.0 / 60.0 / dt) as usize, "descent"),
        ([0.0, 0.0, 0.0], 300, "hold low"),
        ([250.0, 0.0, 0.0], (60.0 / dt) as usize, "250 m/s"),
    ];
    for (vel, n, name) in steps {
        for _ in 0..n {
            for a in 0..3 {
                cam[a] += vel[a] * dt;
            }
            let (_, un) = h.frame(cam, vel);
            h.check_frame(&un, name);
            let v = h.view(cam);
            let under = h.g.tile_at(v.wx, v.wy);
            let fading_pair = under.len() == 2 && under.iter().all(|k| h.g.fades.contains_key(k));
            if under.len() != 1 && !fading_pair {
                camera_holes += 1;
            }
        }
    }
    h.settle(cam, 20_000);
    assert!(h.g.check().is_empty());
    assert_eq!(h.ever_requested, h.ever_delivered, "every requested terrain tile was delivered");
    assert_eq!(camera_holes, 0, "point under the camera uncovered");
    assert!(h.parent_drops > 0 && h.coarsen_swaps > 0, "refines and coarsens happened");
    assert!(h.refine_fades > 0 && h.coarsen_fades > 0);
    assert_eq!(h.redelivered_total, 0, "no light re-deliveries on the terrain layer");
    eprintln!(
        "terrain flight: held per ring at 300 m {at300:?}, {} deliveries, {} refine / {} coarsen fades",
        h.delivered_total, h.refine_fades, h.coarsen_fades
    );
}

#[test]
fn horizon_line_and_hysteresis() {
    let line = crate::rings::horizon_line_m(300.0, 0.0, 0.0, 0.0, 0.0);
    assert!((line - (2.0 * crate::rings::HORIZON_EARTH_RADIUS_M * 300.0).sqrt()).abs() < 1e-6);
    assert!((line - 61_827.0).abs() < 1.0, "{line}");
    // Just inside / just outside, hysteresis on the way out.
    assert!(crate::rings::above_horizon(line - 10.0, line, 1.15, false));
    assert!(!crate::rings::above_horizon(line + 10.0, line, 1.15, false));
    assert!(crate::rings::above_horizon(line + 10.0, line, 1.15, true));
    assert!(!crate::rings::above_horizon(line * 1.15 + 10.0, line, 1.15, true));
    // A tall tile is seen farther; the reference is the lower of camera ground and tile.
    let hill = crate::rings::horizon_line_m(300.0, 0.0, 0.0, 500.0, 2000.0);
    assert!(hill > line + 2000.0 + 79_000.0);
    let plateau = crate::rings::horizon_line_m(800.0, 500.0, 0.0, 10.0, 0.0);
    assert!((plateau - crate::rings::horizon_line_m(800.0, 0.0, 0.0, 10.0, 0.0)).abs() < 1e-9);
}

/// Horizon cull on the terrain layer: fewer roots than without it, every root above the
/// horizon, the hysteresis keeps roots when the camera sinks a little, groups are culled
/// whole (never a single child).
#[test]
fn horizon_cull_roots_groups_and_hysteresis() {
    // Pinned to the former default relief (160 m): taller relief widens the horizon so far that nothing is culled.
    let (p_on, t) = terrain_setup(|tp| tp.amplitude_m = 160.0);
    let (p_off, _) = terrain_setup(|tp| {
        tp.amplitude_m = 160.0;
        tp.horizon_cull = false;
    });
    let roots = |h: &Harness| -> HashSet<TileKey> { h.s.entries().filter(|e| e.key.z == h.l.root_zoom).map(|e| e.key).collect() };
    let mut on = terrain_harness(p_on.clone(), t.clone());
    on.settle([0.0, 0.0, 300.0], 20_000);
    let mut off = terrain_harness(p_off, t.clone());
    off.settle([0.0, 0.0, 300.0], 20_000);
    let (r_on, r_off) = (roots(&on), roots(&off));
    assert!(r_on.len() < r_off.len() && r_on.is_subset(&r_off), "cull {} vs no cull {}", r_on.len(), r_off.len());
    let check_above = |h: &Harness, cam: [f64; 3]| {
        let (views, ground, _) = h.s.views_ground(cam, [0.0; 3]);
        let top = h.l.rings() - 1;
        for e in h.s.entries().filter(|e| e.key.z == h.l.root_zoom) {
            assert!(h.s.horizon_ok(&views[top], ground[top], e.key, e.terrain_box, true), "{} below the horizon", e.key);
        }
    };
    check_above(&on, [0.0, 0.0, 300.0]);
    // Sink to 250 m: the line moves in by ~5 km, inside the 15 % hysteresis: nothing drops,
    // while a cover built from scratch at 250 m has fewer roots.
    on.settle([0.0, 0.0, 250.0], 20_000);
    let kept = roots(&on);
    assert_eq!(kept, r_on, "hysteresis keeps the roots");
    let mut fresh = terrain_harness(p_on.clone(), t.clone());
    fresh.settle([0.0, 0.0, 250.0], 20_000);
    let r_fresh = roots(&fresh);
    assert!(r_fresh.len() < kept.len() && r_fresh.is_subset(&kept), "fresh {} vs kept {}", r_fresh.len(), kept.len());
    // Down to 20 m AGL: far roots go (past the line * hysteresis), the rest stay above.
    on.settle([0.0, 0.0, 20.0], 20_000);
    assert!(roots(&on).len() < kept.len());
    check_above(&on, [0.0, 0.0, 20.0]);
    eprintln!(
        "horizon: roots at 300 m {} (no cull {}), fresh at 250 m {}, at 20 m {}",
        r_on.len(),
        r_off.len(),
        r_fresh.len(),
        roots(&on).len()
    );
    // Whole groups: a split node has at least one child above the horizon (with the
    // hysteresis), and groups with children that are below the plain line on their own are
    // kept whole. Low relief (20 m) so the line (~40 km at 20 m AGL) cuts through T3 groups.
    let (p_low, t) = terrain_setup(|tp| tp.amplitude_m = 20.0);
    let mut on = terrain_harness(p_low, t.clone());
    let cam = [0.0, 0.0, 20.0];
    on.settle(cam, 20_000);
    let (views, ground, _) = on.s.views_ground(cam, [0.0; 3]);
    let mut mixed = 0usize;
    for e in on.s.entries().filter(|e| e.split) {
        let kids = children(e.key);
        let test = |current: bool| -> Vec<bool> {
            kids.iter()
                .map(|c| {
                    let g = on.s.entries.get(c).map_or_else(|| t.tile_range(*c), |x| x.terrain_box);
                    on.s.horizon_ok(&views[e.ring - 1], ground[e.ring - 1], *c, g, current)
                })
                .collect()
        };
        assert!(test(true).iter().any(|a| *a), "{} split with all children below the horizon", e.key);
        assert!(kids.iter().all(|c| on.s.entries.contains_key(c)), "{}: group not whole", e.key);
        if test(false).iter().any(|a| !*a) {
            mixed += 1;
        }
    }
    eprintln!("horizon, low relief at 20 m: {mixed} groups kept whole with a child below the horizon");
    assert!(mixed > 0, "no sibling group straddles the horizon at 20 m");
}

/// `set_detail` (G2) scales the terrain cover's tau: 2 coarsens it through ordinary swaps (the
/// contract checks hold every frame, nothing is re-delivered, jobs keep detail 1), 1 restores
/// the cover; a no-op elsewhere.
#[test]
fn terrain_detail_scales_the_cover() {
    let (p, t) = terrain_setup(|_| {});
    let mut h = terrain_harness(p, t.clone());
    let cam = [0.0, 0.0, 300.0];
    h.settle(cam, 20_000);
    let held: HashSet<TileKey> = h.s.delivered_keys().into_iter().collect();
    assert!(held.len() > 50);
    h.deliveries.clear();
    assert!(h.s.set_detail(2.0));
    assert!(!h.s.set_detail(2.0), "same scale: nothing to do");
    assert_eq!(h.s.detail(), 2.0);
    let mut unloads = 0;
    for _ in 0..20_000 {
        let (_, un) = h.frame(cam, [0.0; 3]);
        unloads += un.len();
        h.check_frame(&un, "detail");
        if h.s.settled() {
            break;
        }
    }
    assert!(h.s.settled() && h.g.check().is_empty());
    let coarse: HashSet<TileKey> = h.s.delivered_keys().into_iter().collect();
    assert!(unloads > 0 && coarse.len() < held.len(), "coarser: {} vs {}", coarse.len(), held.len());
    assert!(h.deliveries.iter().all(|(k, re)| !*re && !held.contains(k)), "only fresh deliveries of coarser keys");
    assert!(coarse.iter().all(|k| held.contains(k) || held.iter().any(|c| c.z > k.z && (c.x >> (c.z - k.z)) == k.x && (c.y >> (c.z - k.z)) == k.y)));
    assert!(h.s.set_detail(1.0));
    h.settle(cam, 20_000);
    let back: HashSet<TileKey> = h.s.delivered_keys().into_iter().collect();
    assert_eq!(back, held, "scale 1: the cover of before");
    // The vector layer has no detail.
    assert!(!Scheduler::new(Frame::new(VILNIUS.0, VILNIUS.1), true, dp()).set_detail(2.0));
    eprintln!("detail 2.0: {} -> {} held terrain tiles", held.len(), coarse.len());
}

/// Budget measurement (informative): `cargo test --release -p simworld-core terrain_budget --
/// --ignored --nocapture`. Default profile with the terrain on (amplitude 160 m, flat zone
/// at the origin), real TINs built on one thread: worker CPU per tile per band and the
/// resident (held) terrain tiles and triangles at 300 m and 3000 m AGL, settled.
#[test]
#[ignore]
fn terrain_budget() {
    let (p, t) = terrain_setup(|_| {});
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    for alt in [300.0, 3000.0] {
        let mut h = terrain_harness(p.clone(), t.clone());
        h.w.real = Some(frame);
        h.w.profile = Some(p.clone());
        h.w.terrain = Some(t.clone());
        let t0 = Instant::now();
        h.settle([0.0, 0.0, alt], 20_000);
        let held = held_per_ring(&h);
        let mut tris = vec![0u64; h.l.rings()];
        for e in h.s.entries().filter(|e| e.held) {
            tris[e.ring] += e.tris as u64;
        }
        eprintln!(
            "terrain at {alt} m AGL: resident per ring {held:?} (total {}), triangles per ring {tris:?} (total {}), settled in {:.1} s",
            held.iter().sum::<usize>(),
            tris.iter().sum::<u64>(),
            t0.elapsed().as_secs_f64()
        );
        eprintln!("  {}", h.s.cpu_stats().report("terrain", h.l.rings()));
    }
}
/// Measurement (informative): terrain tiles held / visible (= terrain draw calls) per ring at
/// 300 m and 3000 m AGL, settled, and over the scheduler test flight (climb to 7.7 km, descent,
/// 250 m/s), at amplitude 450 (default) and 160.
/// `cargo test --release -p simworld-core terrain_cover_counts -- --ignored --nocapture`
#[test]
#[ignore]
fn terrain_cover_counts() {
    for amp in [450.0, 160.0] {
        let (p, t) = terrain_setup(|tp| tp.amplitude_m = amp);
        for alt in [300.0, 3000.0] {
            let mut h = terrain_harness(p.clone(), t.clone());
            h.settle([0.0, 0.0, alt], 20_000);
            eprintln!(
                "amp {amp} alt {alt}: held per ring {:?} (total {}), visible {}",
                held_per_ring(&h),
                held_per_ring(&h).iter().sum::<usize>(),
                h.g.visible_tiles().len()
            );
        }
        let mut h = terrain_harness(p.clone(), t.clone());
        let dt = 1.0 / 30.0;
        let mut cam = [0.0, 0.0, 300.0];
        h.settle(cam, 20_000);
        let (mut max_vis, mut sum_vis, mut n) = (0usize, 0usize, 0usize);
        let steps: [([f64; 3], usize); 3] = [
            ([0.0, 0.0, 60.0], (7700.0 / 60.0 / dt) as usize),
            ([0.0, 0.0, -60.0], (7700.0 / 60.0 / dt) as usize),
            ([250.0, 0.0, 0.0], (60.0 / dt) as usize),
        ];
        let d0 = h.delivered_total;
        for (vel, k) in steps {
            for _ in 0..k {
                for a in 0..3 {
                    cam[a] += vel[a] * dt;
                }
                h.frame(cam, vel);
                let v = h.g.visible_tiles().len();
                max_vis = max_vis.max(v);
                sum_vis += v;
                n += 1;
            }
        }
        eprintln!(
            "amp {amp} flight: visible mean {:.0} max {max_vis}, deliveries {}",
            sum_vis as f64 / n as f64,
            h.delivered_total - d0
        );
    }
    // The two-cities seed world (no flat zone, coast, lakes, airports).
    let (p, t, _) = crate::seed::world::test_world();
    for (name, lat, lon) in [("vilnius", 54.6872, 25.2797), ("eyka", 54.9639, 24.0848), ("coast", 55.85, 21.06)] {
        for alt in [300.0, 3000.0] {
            let frame = Frame::new(lat, lon);
            let mut h = Harness::new();
            h.s = Scheduler::new_terrain(frame, Arc::new(p.clone()), t.clone());
            h.l = *h.s.layer();
            h.g = GodotSim::for_zooms(h.l.root_zoom, h.l.leaf_zoom);
            h.settle([0.0, 0.0, alt], 20_000);
            eprintln!("seed {name} alt {alt}: held per ring {:?} (total {})", held_per_ring(&h), held_per_ring(&h).iter().sum::<usize>());
        }
    }
}
