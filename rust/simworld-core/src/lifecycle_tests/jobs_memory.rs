//! Orphaned jobs, shutdown with jobs in flight, the job pool measurement, the memory budget and
//! the view / LOD quality across configures.

use super::*;

/// Step J1 (one job scheduler): a camera that jumps between two spots 4 km apart every few
/// frames orphans mesh jobs while they are queued or running. They are cancelled
/// (`stats().jobs.cancelled` rises, their slots come back) and nothing stale reaches the host:
/// every delivered tile is in the current cover at delivery, never delivered twice while held
/// (no light-mode flip at this height), and its mesh equals a fresh build of that key.
#[test]
fn orphaned_mesh_jobs_are_cancelled_and_nothing_stale_is_delivered() {
    let mut w = world();
    assert!(w.configure_world(synthetic(VILNIUS.0, VILNIUS.1)).ok);
    let frame = crate::geo::Frame::new(VILNIUS.0, VILNIUS.1);
    let profile = crate::profile::Profile::default();
    let mut held: BTreeSet<LayerKey> = BTreeSet::new();
    let mut checked = 0usize;
    let mut delivered = 0usize;
    let t0 = Instant::now();
    for frame_no in 0..2000 {
        let x = if frame_no % 2 == 0 { 0.0 } else { 4000.0 };
        w.update_camera([x, 120.0, 0.0], [0.0; 3], 120.0);
        let mut cover: BTreeSet<LayerKey> = BTreeSet::new();
        w.debug_tiles(|k, _, _, _| {
            cover.insert(k);
        });
        let mut fresh = Vec::new();
        w.pop_ready(8, |t| {
            let k = t.layer_key();
            assert!(cover.contains(&k), "{k:?} delivered but not in the cover");
            assert!(!held.contains(&k), "{k:?} delivered twice while held");
            fresh.push((t.key, t.ring, t.mesh.clone()));
            held.insert(k);
        });
        delivered += fresh.len();
        for (k, ring, mesh) in fresh {
            if checked < 40 {
                let want = crate::data::synthetic_chunk(k, ring, frame, &crate::terrain::Terrain::flat(), &profile, false).unwrap();
                assert_eq!(mesh, *want.mesh, "{k}: delivered mesh differs from a fresh build");
                checked += 1;
            }
        }
        let _ = w.drain_fades();
        for k in w.drain_unload() {
            held.remove(k);
        }
        // No sleep: the next jump comes while the last frame's jobs are still queued / running.
        if w.stats().jobs.cancelled >= 10 && delivered >= 60 {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(60));
    }
    let st = w.stats().jobs;
    eprintln!("jobs: {st:?}, delivered {delivered}, checked {checked}");
    assert!(st.cancelled > 0, "the jumping camera orphaned queued / running mesh jobs");
    assert!(delivered > 0 && checked > 0);
    // Settle at one spot: every slot comes back (no leaked in-flight counter), the queue drains.
    let t1 = Instant::now();
    loop {
        w.update_camera([0.0, 120.0, 0.0], [0.0; 3], 120.0);
        w.pop_ready(64, |t| {
            held.insert(t.layer_key());
        });
        let _ = w.drain_fades();
        for k in w.drain_unload() {
            held.remove(k);
        }
        let s = w.stats();
        if s.queued.iter().sum::<i32>() == 0 && s.meshing.iter().sum::<i32>() == 0 && s.jobs.queued == 0 && s.jobs.running == 0 {
            break;
        }
        assert!(t1.elapsed() < Duration::from_secs(60), "never settled: {:?}", s.jobs);
        std::thread::sleep(Duration::from_millis(2));
    }
    let mut ready: BTreeSet<LayerKey> = BTreeSet::new();
    w.debug_tiles(|k, _, state, _| {
        if state == "ready" {
            ready.insert(k);
        }
    });
    assert!(held.is_subset(&ready), "the host holds only tiles of the cover");
}

/// `shutdown()` with CPU jobs queued and running returns at once and the world configures again.
#[test]
fn shutdown_with_jobs_in_flight_does_not_block() {
    let mut w = world();
    for _ in 0..3 {
        assert!(w.configure_world(synthetic(VILNIUS.0, VILNIUS.1)).ok);
        // Deterministic: every pool thread is busy with a job that ignores its cancel flag
        // (a long build stage), so the mesh jobs of the cover stay queued behind them. Before,
        // a fast machine could finish the synthetic jobs before the check (flaky under load).
        let threads = w.worker().unwrap().cpu_threads();
        let gate = w.worker().unwrap().hold_cpu_threads();
        for _ in 0..3 {
            w.update_camera([0.0, 50.0, 0.0], [0.0; 3], 50.0);
        }
        let j = w.stats().jobs;
        assert_eq!(j.running, threads, "jobs in flight: {j:?}");
        assert!(j.queued > 0, "jobs in flight: {j:?}");
        let t0 = Instant::now();
        assert!(w.shutdown());
        assert!(t0.elapsed() < Duration::from_millis(500), "shutdown took {:?}", t0.elapsed());
        assert_eq!(w.stats().jobs, crate::jobs::JobStats::default());
        // The held threads exit once released (they never touch the world).
        drop(gate);
    }
    assert!(w.configure_world(synthetic(VILNIUS.0, VILNIUS.1)).ok);
    let (_, d) = run_until(&mut w, 5);
    assert!(d.len() >= 5);
}

/// CPU pool measurement (step J1, prints only): cold fill time at a fixed spot (mesh throughput:
/// every job of the cover, both layers) and a 250 m/s flight (stage latencies). Run before and
/// after a pool change: `cargo test --release -p simworld-core job_pool_measure -- --ignored
/// --nocapture`.
#[test]
#[ignore]
fn job_pool_measure() {
    let opts = || WorldOptions {
        profile_overrides: Some(serde_json::json!({ "terrain": { "enabled": true } })),
        layers: Some(vec!["world".into(), "terrain".into()]),
        ..synthetic(VILNIUS.0, VILNIUS.1)
    };
    let busy = |w: &World| {
        let s = w.stats();
        s.queued.iter().chain(s.meshing.iter()).chain(s.terrain_queued.iter()).chain(s.terrain_meshing.iter()).sum::<i32>()
    };
    let mut fills = Vec::new();
    for rep in 0..5 {
        let mut w = world();
        assert!(w.configure_world(opts()).ok);
        let t0 = Instant::now();
        let mut n = 0usize;
        loop {
            w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
            n += w.pop_ready(1024, |_| {});
            let _ = w.drain_fades();
            let _ = w.drain_unload();
            if n > 0 && busy(&w) == 0 {
                break;
            }
            std::thread::sleep(Duration::from_micros(200));
            assert!(t0.elapsed() < Duration::from_secs(120));
        }
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        println!("fill rep {rep}: {n} tiles in {ms:.0} ms ({:.1} tiles/s)", n as f64 / ms * 1000.0);
        fills.push(ms);
        w.shutdown();
    }
    fills.sort_by(f64::total_cmp);
    println!("fill median {:.0} ms", fills[fills.len() / 2]);
    let mut w = world();
    assert!(w.configure_world(opts()).ok);
    let mut n = 0usize;
    let mut z = 0.0f64;
    for _ in 0..(60 * 20) {
        let t0 = Instant::now();
        z -= 250.0 / 60.0;
        let shift = w.update_camera([0.0, 300.0, z], [0.0, 0.0, -250.0], 300.0);
        if let Some(s) = shift {
            z -= s[2];
        }
        n += w.pop_ready(2, |_| {});
        let _ = w.drain_fades();
        let _ = w.drain_unload();
        std::thread::sleep(Duration::from_millis(16).saturating_sub(t0.elapsed()));
    }
    println!("flight 250 m/s 20 s: {n} deliveries");
    if let Some(l) = w.latency() {
        println!("{}", l.report());
    }
}

/// Memory budget (Terrain v2 step M1): owned by the world, sized from `memory.budget_mb` on
/// every configure, visible in `stats().memory`, cleared by `shutdown`. Fake items stand in for
/// the decoded data tiles and HeightTiles that will register later.
#[test]
fn memory_budget_follows_the_profile_and_evicts_fake_items() {
    use crate::budget::{ItemKey, Pool, MIB};
    let mib = MIB as u64;
    let mut w = world();
    assert_eq!(w.stats().memory.budget_bytes, 512 * mib, "default budget before configure");
    let mut o = synthetic(VILNIUS.0, VILNIUS.1);
    o.profile_overrides = Some(serde_json::json!({ "memory": { "budget_mb": 64 } }));
    assert!(w.configure_world(o).ok);
    assert_eq!(w.stats().memory.budget_bytes, 64 * mib);

    // 20 data tiles of 2 MiB (priority = -distance) and 30 height tiles of 1 MiB: 70 MiB.
    let m = w.memory_mut();
    for i in 0..20u64 {
        m.insert(ItemKey::new(Pool::DataTiles, i), 2 * mib, -(i as f64) * 100.0);
    }
    m.pin(ItemKey::new(Pool::DataTiles, 19)); // the farthest, but a job needs it
    for i in 0..30u64 {
        m.insert(ItemKey::new(Pool::HeightTiles, i), mib, -(i as f64) * 50.0 - 25.0);
    }
    let mut gone = Vec::new();
    m.take_evicted(Pool::DataTiles, &mut gone);
    // Each overflow evicts the lowest priority across both pools: D18 (-1800) at H24, D17 at
    // H26, D16 at H28; every height tile ranks higher than those.
    assert_eq!(gone, vec![18, 17, 16], "farthest unpinned data tiles first");
    gone.clear();
    m.take_evicted(Pool::HeightTiles, &mut gone);
    assert!(gone.is_empty());
    let s = w.stats().memory;
    assert_eq!(s.total_bytes, 64 * mib);
    assert_eq!((s.pool(Pool::DataTiles).items, s.pool(Pool::HeightTiles).items), (17, 30));
    assert_eq!(s.pool(Pool::DataTiles).pinned_items, 1);

    // A reconfigure with a smaller budget evicts at once; the pinned tile stays.
    let mut o = synthetic(VILNIUS.0, VILNIUS.1);
    o.profile_overrides = Some(serde_json::json!({ "memory": { "budget_mb": 32 } }));
    assert!(w.configure_world(o).ok);
    let s = w.stats().memory;
    assert_eq!((s.budget_bytes, s.over_budget_bytes), (32 * mib, 0));
    assert!(s.total_bytes <= 32 * mib && s.total_bytes >= 31 * mib, "{}", s.total_bytes);
    assert!(w.memory().contains(ItemKey::new(Pool::DataTiles, 19)));
    // The nearest of each pool survive.
    assert!(w.memory().contains(ItemKey::new(Pool::DataTiles, 0)) && w.memory().contains(ItemKey::new(Pool::HeightTiles, 0)));
    assert!(!w.memory().contains(ItemKey::new(Pool::DataTiles, 15)) && !w.memory().contains(ItemKey::new(Pool::HeightTiles, 29)));

    // A rejected profile changes nothing.
    let mut o = synthetic(VILNIUS.0, VILNIUS.1);
    o.profile_overrides = Some(serde_json::json!({ "memory": { "budget_mb": 1 } }));
    assert!(!w.configure_world(o).ok);
    assert_eq!(w.stats().memory.budget_bytes, 32 * mib);

    assert!(w.shutdown());
    let s = w.stats().memory;
    assert_eq!((s.total_bytes, s.pool(Pool::DataTiles).items), (0, 0));
}

/// api 1.10: `set_view` / `set_lod_quality` work before configure, survive `configure_world`,
/// ignore invalid input, and move the cover from the next `update_camera` (no reconfigure).
#[test]
fn set_view_and_lod_quality_persist_across_configure() {
    let mut w = world();
    let default_edges = [600.0, 2500.0, 7000.0, 25_000.0];
    assert_eq!(w.lod_edges_m(), default_edges);
    assert_eq!(w.tau_eff(), 4.0);
    // Before configure.
    assert!(w.set_view(30.0, 1080.0, [0.0, 0.0, -2.0]));
    assert_eq!(w.lod_view().forward, [0.0, 0.0, -1.0]);
    let narrow = w.lod_edges_m();
    let k = (30f64).to_radians().tan() / (15f64).to_radians().tan();
    for r in 0..3 {
        assert!((narrow[r] / default_edges[r] - k).abs() / k < 0.01, "{narrow:?}");
    }
    // Invalid input: ignored, no change.
    for (fov, h, f) in [(0.0, 1080.0, [0.0, 0.0, -1.0]), (60.0, -5.0, [0.0, 0.0, -1.0]), (60.0, 1080.0, [0.0; 3]), (f64::NAN, 1080.0, [0.0, 0.0, -1.0])] {
        assert!(!w.set_view(fov, h, f));
    }
    assert_eq!(w.lod_view().fov_y_deg, 30.0);
    // Configure: the view is kept and reaches the scheduler.
    assert!(w.configure_world(synthetic(VILNIUS.0, VILNIUS.1)).ok);
    assert_eq!(w.lod_view().fov_y_deg, 30.0);
    assert_eq!(w.scheduler().unwrap().layer().edge_m[..4], narrow);
    // At 1000 m the narrow view puts R0 tiles in the cover (default: none above 940 m slant).
    w.update_camera([0.0, 1000.0, 0.0], [0.0; 3], 1000.0);
    let rings = |w: &World| {
        let mut per = [0usize; 4];
        w.debug_tiles(|k: LayerKey, ring, _, _| {
            if k.layer == crate::rings::LayerKind::World {
                per[ring] += 1;
            }
        });
        per
    };
    assert!(rings(&w)[0] > 0, "fov 30: R0 at 1000 m");
    // Quality: live tau multiplier, clamped, kept across a reconfigure.
    assert_eq!(w.set_lod_quality(2.0), 2.0);
    assert_eq!(w.tau_eff(), 8.0);
    let q2 = w.lod_edges_m();
    for r in 0..3 {
        assert!((q2[r] / narrow[r] - 0.5).abs() < 1e-12);
    }
    assert_eq!(q2[3], 25_000.0);
    assert_eq!(w.set_lod_quality(f64::NAN), 2.0, "non-finite ignored");
    assert_eq!(w.set_lod_quality(100.0), 4.0);
    assert_eq!(w.set_lod_quality(0.0), 0.25);
    assert_eq!(w.set_lod_quality(2.0), 2.0);
    assert!(w.configure_world(synthetic(54.70, 25.30)).ok);
    assert_eq!((w.lod_quality(), w.lod_view().fov_y_deg), (2.0, 30.0));
    assert_eq!(w.scheduler().unwrap().layer().edge_m[..4], q2);
    // A viewport of half the height halves the edges (quality 1, 60 deg).
    w.set_lod_quality(1.0);
    assert!(w.set_view(60.0, 540.0, [1.0, 0.0, 0.0]));
    let half = w.lod_edges_m();
    for (got, want) in half.iter().zip([300.0, 1250.0, 3500.0, 25_000.0]) {
        assert!((got - want).abs() < 1e-9, "{half:?}");
    }
    // The default view again: the default cover (GOLDEN_COVER-equivalent edges).
    assert!(w.set_view(60.0, 1080.0, [0.0, 0.0, -1.0]));
    assert_eq!(w.scheduler().unwrap().layer().edge_m[..4], default_edges);
}
