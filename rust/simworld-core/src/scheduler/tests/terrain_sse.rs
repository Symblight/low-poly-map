//! The terrain SSE cover (Terrain v2 G2).

use super::*;

/// The SSE rule itself: every split node's look-ahead distance is inside its refine distance
/// (x hysteresis), every held leaf above ring 0 is not (a fresh cover: no hysteresis), and the
/// refine distance is `max(proxy x H / (2 tan(fov/2) tau), guard)`.
#[test]
fn terrain_cover_refines_by_screen_space_error() {
    let (p, t) = terrain_setup(|_| {});
    let mut h = terrain_harness(p.clone(), t);
    let cam = [0.0, 0.0, 300.0];
    h.settle(cam, 20_000);
    let k_m = crate::rings::sse_refine_distance(1.0, &crate::rings::LodView::default(), p.terrain.tau_px);
    let guard = h.s.foundation_guard();
    let entries: Vec<(TileKey, usize, bool, bool, f64, f64)> =
        h.s.entries().map(|e| (e.key, e.ring, e.split, e.held, e.split_d, e.refine_m)).collect();
    let mut leaves = 0;
    for (k, ring, split, held, d, refine) in entries {
        if ring == 0 {
            continue;
        }
        let e = h.s.terrain_error_m(k);
        assert_eq!(refine, (e * k_m).max(guard[ring]), "{k}");
        if split {
            assert!(d <= refine * h.l.hysteresis, "{k} split at {d:.0} m, refine distance {refine:.0} m");
        } else if held {
            assert!(d >= refine, "{k} held unsplit at {d:.0} m inside its refine distance {refine:.0} m");
            leaves += 1;
        }
    }
    assert!(leaves > 50);
}

/// Flatter relief holds fewer terrain tiles from the same camera; inside one world the flat sea
/// stays coarser than the land at the same distance.
#[test]
fn terrain_cover_flat_holds_fewer_tiles_than_hilly() {
    let mut totals = Vec::new();
    for amp in [450.0, 160.0, 40.0] {
        let (p, t) = terrain_setup(|tp| {
            tp.amplitude_m = amp;
            tp.flat_radius_m = 0.0;
            tp.flat_blend_m = 0.0;
        });
        let mut h = terrain_harness(p, t);
        h.settle([0.0, 0.0, 300.0], 20_000);
        totals.push(held_per_ring(&h).iter().sum::<usize>());
    }
    eprintln!("terrain tiles at amplitude 450 / 160 / 40: {totals:?}");
    assert!(totals[0] > totals[1] && totals[1] > totals[2], "{totals:?}");
    // Seed world on the Baltic coast (lon ~21.06): the camera over the shore, sea to the west.
    let mut h = seed_terrain_harness(55.85, 21.06, |_| {});
    h.settle([0.0, 0.0, 300.0], 20_000);
    let f = Frame::new(55.85, 21.06);
    let (mut sea, mut land) = (0usize, 0usize);
    for e in h.s.entries().filter(|e| e.held && (6_000.0..20_000.0).contains(&e.cam_d)) {
        let (_, lon) = f.lat_lon(e.center[0], e.center[1]);
        if lon < 20.95 {
            sea += 1;
        } else if lon > 21.2 {
            land += 1;
        }
    }
    eprintln!("seed coast, 6..20 km: {sea} sea tiles, {land} land tiles");
    assert!(sea < land, "open sea {sea} vs land {land}");
}

/// A narrower field of view refines farther (more tiles); the refine distance scales with
/// 1 / tan(fov / 2) exactly.
#[test]
fn terrain_cover_narrow_fov_refines_farther() {
    let (p, t) = terrain_setup(|_| {});
    let mut wide = terrain_harness(p.clone(), t.clone());
    wide.settle([0.0, 0.0, 300.0], 20_000);
    let mut narrow = terrain_harness(p, t);
    let v30 = crate::rings::LodView::new(30.0, 1080.0, [0.0, 0.0, -1.0]).unwrap();
    narrow.s.set_lod_view(&v30, 1.0);
    narrow.settle([0.0, 0.0, 300.0], 20_000);
    let (a, b) = (held_per_ring(&wide).iter().sum::<usize>(), held_per_ring(&narrow).iter().sum::<usize>());
    eprintln!("terrain tiles at fov 60: {a}, fov 30: {b}");
    assert!(b > a);
    let k = TileKey::new(12, 2335, 1301);
    let (dw, dn) = (wide.s.terrain_refine_m(k, 2), narrow.s.terrain_refine_m(k, 2));
    let ratio = crate::math::tan(30f64.to_radians()) / crate::math::tan(15f64.to_radians());
    assert!(dn > dw && ((dn / dw) - ratio).abs() < 1e-9, "{dw} {dn}");
}

/// Hysteresis: a camera circling (radius 40 m, 30 m up and down, one period = 315 frames, no
/// velocity input) may
/// change the cover during the first lap (tiles just outside their refine distance split), but
/// after that every tile sits inside its hysteresis band: three more laps change nothing.
#[test]
fn terrain_cover_steady_under_small_motion() {
    let (p, t) = terrain_setup(|_| {});
    let mut h = terrain_harness(p, t);
    h.settle([40.0, 0.0, 300.0], 20_000);
    let lap = |h: &mut Harness, laps: usize| -> (usize, usize) {
        let (d0, mut unloads) = (h.delivered_total, 0usize);
        for i in 0..315 * laps {
            let a = i as f64 * (std::f64::consts::TAU / 315.0);
            let cam = [40.0 * a.cos(), 40.0 * a.sin(), 300.0 + 30.0 * a.sin()];
            // No velocity: the look-ahead (`bands.look_ahead_s`) would swing the view points by
            // hundreds of meters around the circle; this checks the distance hysteresis alone.
            let vel = [0.0; 3];
            let (_, un) = h.frame(cam, vel);
            h.check_frame(&un, "circle");
            unloads += un.len();
        }
        (h.delivered_total - d0, unloads)
    };
    let first = lap(&mut h, 2);
    let held: HashSet<TileKey> = h.s.delivered_keys().into_iter().collect();
    assert_eq!(lap(&mut h, 3), (0, 0), "cover changed after the first laps (first: {first:?})");
    assert_eq!(h.s.delivered_keys().into_iter().collect::<HashSet<_>>(), held);
    eprintln!("circling: first two laps {first:?} (deliveries, unloads), then steady");
}

/// The foundation guard (`terrain::foundation_guard_m`): at the default view the guard of the
/// coarse rings is the reach of the vector bands they would sink under; on low relief at
/// detail 2 (where the SSE alone would bring coarse tiles close) no held tile of a guarded
/// ring is nearer than its guard.
#[test]
fn terrain_cover_foundation_guard() {
    let (p, t) = terrain_setup(|tp| tp.amplitude_m = 160.0);
    let mut h = terrain_harness(p.clone(), t);
    let g = h.s.foundation_guard();
    let hy = p.bands().hysteresis;
    let e = p.bands().edges_for(&crate::rings::LodView::default(), 1.0);
    // z13 1.5 m > R0 allowance 1 m; z12 4 m > R1 3 m; z11 10 m > R2 8 m; z10 25 m > R3 20 m.
    assert_eq!(&g[..5], &[0.0, e[0] * hy, e[1] * hy, e[2] * hy, e[3] * hy]);
    h.s.set_detail(2.0);
    let cam = [0.0, 0.0, 300.0];
    h.settle(cam, 20_000);
    let mut active = 0;
    for e in h.s.entries().filter(|e| e.held && !e.split && e.ring > 0) {
        assert!(e.split_d >= g[e.ring], "{} at {:.0} m inside its guard {:.0} m", e.key, e.split_d, g[e.ring]);
        if e.refine_m == g[e.ring] {
            active += 1;
        }
    }
    let k_m = crate::rings::sse_refine_distance(1.0, &crate::rings::LodView::default(), p.terrain.tau_px * 2.0);
    let z10 = h.s.entries().filter(|e| e.ring == 4).map(|e| e.key).next().unwrap();
    let sse = h.s.terrain_error_m(z10) * k_m;
    eprintln!("guard {:?}, z10 SSE refine {sse:.0} m, {active} leaves held back by the guard", &g[..5]);
    assert!(sse < g[4], "the guard is what keeps z10 out of the R3 reach here");
}

/// Overlapping runway strips (the 2.28 m height step at EYKA, `ErrorProxy` known limit): the
/// cover proxy has no edit floor, so the step does not drive refinement; the cover settles,
/// stays below `near_zoom` + 0 and does not change over further updates, also with the
/// terrain layer reaching z16.
#[test]
fn terrain_cover_terminates_over_overlapping_strips() {
    // Tile 16/37159/20784 holds the step (G1 measurement): camera 60 m above it.
    let step_tile = TileKey::new(16, 37159, 20784);
    let (lat, lon) = step_tile.center_lat_lon();
    for near in [14u32, 16] {
        let mut h = seed_terrain_harness(lat, lon, |w| {
            if near == 16 {
                // Two more rings (bands copied from the finest).
                let b0 = w.terrain.bands[0].clone();
                w.terrain.near_zoom = 16;
                let mut b = vec![
                    crate::profile::TerrainBand { edge_m: 600.0, max_error_m: 0.2, ..b0.clone() },
                    crate::profile::TerrainBand { edge_m: 1200.0, max_error_m: 0.3, ..b0 },
                ];
                b.extend(w.terrain.bands.drain(..));
                w.terrain.bands = b;
            }
        });
        let cam = [0.0, 0.0, 60.0];
        h.settle(cam, 40_000);
        let held: HashSet<TileKey> = h.s.delivered_keys().into_iter().collect();
        assert!(held.iter().all(|k| k.z <= near as u8));
        for _ in 0..120 {
            let (got, un) = h.frame(cam, [0.0; 3]);
            assert!(got.is_empty() && un.is_empty(), "cover not steady over the strips");
        }
        let total: usize = held_per_ring(&h).iter().sum();
        // The cover proxy ignores the edits: below the edit-floor proxy at every zoom.
        let mut floor = crate::terrain::height::ErrorProxy::new(h.s.terrain(), h.l.root_zoom);
        for z in h.l.root_zoom..=near as u8 {
            let k = TileKey::new(z, step_tile.x >> (16 - z), step_tile.y >> (16 - z));
            assert!(h.s.terrain_error_m(k) <= floor.error_m(k), "{k}");
        }
        eprintln!("strips at EYKA, near_zoom {near}: {total} terrain tiles, held per ring {:?}", held_per_ring(&h));
    }
}

/// The contract checks (GodotSim) hold every frame while the terrain cover's inputs change
/// mid-flight: detail 1 -> 2 -> 0.5, fov 60 -> 30 -> 90, at 250 m/s with climbs, over the
/// default relief and the seed world.
#[test]
fn terrain_sse_cover_keeps_the_guarantees_under_live_changes() {
    let (p, t) = terrain_setup(|_| {});
    for (name, mut h) in [("plain", terrain_harness(p, t)), ("seed", seed_terrain_harness(54.6872, 25.2797, |_| {}))] {
        let dt = 1.0 / 30.0;
        let mut cam = [0.0, 0.0, 300.0];
        h.settle(cam, 20_000);
        let mut holes = 0;
        for i in 0..2400usize {
            match i {
                300 => assert!(h.s.set_detail(2.0)),
                700 => {
                    h.s.set_lod_view(&crate::rings::LodView::new(30.0, 1080.0, [0.0, 0.0, -1.0]).unwrap(), 1.0);
                }
                1100 => assert!(h.s.set_detail(0.5)),
                1500 => {
                    h.s.set_lod_view(&crate::rings::LodView::new(90.0, 1440.0, [0.0, 0.0, -1.0]).unwrap(), 1.0);
                }
                _ => {}
            }
            let vel = if i < 1200 { [250.0, 0.0, 20.0] } else { [0.0, 250.0, -20.0] };
            for a in 0..3 {
                cam[a] += vel[a] * dt;
            }
            let (_, un) = h.frame(cam, vel);
            h.check_frame(&un, name);
            let v = h.view(cam);
            let under = h.g.tile_at(v.wx, v.wy);
            if under.len() != 1 && !(under.len() == 2 && under.iter().all(|k| h.g.fades.contains_key(k))) {
                holes += 1;
            }
        }
        h.settle(cam, 40_000);
        assert!(h.g.check().is_empty());
        assert_eq!(holes, 0, "{name}: point under the camera uncovered");
        assert_eq!(h.redelivered_total, 0, "{name}: no re-deliveries");
        eprintln!("{name}: {} deliveries, {} refine / {} coarsen fades", h.delivered_total, h.refine_fades, h.coarsen_fades);
    }
}

/// Held terrain tiles that touch a held tile two or more zooms coarser (edge or corner).
fn level_jumps(h: &Harness) -> (usize, usize) {
    let held: Vec<TileKey> = h.s.entries().filter(|e| e.held && !e.split).map(|e| e.key).collect();
    let set: HashSet<TileKey> = held.iter().copied().collect();
    let (mut jumps, mut max) = (0usize, 0usize);
    for &k in &held {
        // Edge neighbours at the same zoom; find the held tile covering each.
        for (dx, dy) in [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)] {
            let (nx, ny) = (k.x as i64 + dx, k.y as i64 + dy);
            if nx < 0 || ny < 0 {
                continue;
            }
            for z in (h.l.root_zoom..k.z).rev() {
                let s = k.z - z;
                let p = TileKey::new(z, (nx >> s) as u32, (ny >> s) as u32);
                if set.contains(&p) {
                    let d = (k.z - z) as usize;
                    if d >= 2 {
                        jumps += 1;
                    }
                    max = max.max(d);
                    break;
                }
            }
        }
    }
    (jumps, max)
}

/// 2:1 balance (`plan_terrain_splits`): in a settled terrain cover no leaf shares an edge with
/// a leaf two or more zooms coarser (the skirts close one level), on smooth, low and tall relief
/// and in the seed world; the balance does add splits where the SSE alone would not.
#[test]
fn terrain_cover_is_balanced() {
    let mut forced = 0;
    for (amp, ridge) in [(450.0, 0.5), (160.0, 0.5), (1200.0, 1.0)] {
        let (p, t) = terrain_setup(|tp| {
            tp.amplitude_m = amp;
            tp.ridge_share = ridge;
        });
        for alt in [300.0, 3000.0] {
            let mut h = terrain_harness(p.clone(), t.clone());
            h.settle([0.0, 0.0, alt], 40_000);
            forced += h.s.balance_forced();
            assert_eq!(level_jumps(&h).0, 0, "amplitude {amp} at {alt} m: unbalanced seams");
        }
    }
    for (lat, lon) in [(55.85, 21.06), (54.9639, 24.0848)] {
        let mut h = seed_terrain_harness(lat, lon, |_| {});
        h.settle([0.0, 0.0, 300.0], 40_000);
        forced += h.s.balance_forced();
        assert_eq!(level_jumps(&h).0, 0, "seed {lat},{lon}: unbalanced seams");
    }
    eprintln!("balance added {forced} splits over the settled covers");
    assert!(forced > 0, "the balance never acted");
}

/// Purity (G2): the terrain cover depends on the camera, the view, the scale and the profile,
/// not on which tiles were built: the split set of a fresh scheduler's first `update` equals
/// the settled cover's (every tile built and delivered, boxes not widened by the TINs), with
/// real TINs, on the default relief at two cameras.
#[test]
fn terrain_cover_does_not_depend_on_build_history() {
    let (p, t) = terrain_setup(|_| {});
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    for cam in [[0.0, 0.0, 300.0], [3000.0, -2000.0, 1500.0]] {
        let mut fresh = Scheduler::new_terrain(frame, p.clone(), t.clone());
        let mut jobs = Vec::new();
        fresh.update(cam, [0.0; 3], Instant::now(), &mut jobs);
        let first: std::collections::BTreeSet<TileKey> = fresh.entries().filter(|e| e.split).map(|e| e.key).collect();
        let mut h = terrain_harness(p.clone(), t.clone());
        h.w.real = Some(frame);
        h.w.profile = Some(p.clone());
        h.w.terrain = Some(t.clone());
        h.settle(cam, 20_000);
        let settled: std::collections::BTreeSet<TileKey> = h.s.entries().filter(|e| e.split).map(|e| e.key).collect();
        assert_eq!(first, settled, "camera {cam:?}");
    }
}
