//! Opt-in integration tests against a served `.pmtiles` (not run by default):
//! `SIMPLANE_TEST_URL=http://127.0.0.1:8787/vilnius.pmtiles cargo test -- --ignored --nocapture`

use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::Instant;

use crate::fetch::{Fetcher, Source};
use crate::geo::{Frame, TileKey};
use crate::profile::Profile;
use crate::worker::{build_tile, tile_buildings, Payload};

fn url() -> Option<String> {
    std::env::var("SIMPLANE_TEST_URL").ok().filter(|s| !s.is_empty())
}

#[test]
#[ignore]
fn fetch_decode_reference_tiles_and_cache_offline() {
    let Some(url) = url() else {
        eprintln!("SIMPLANE_TEST_URL not set; skipping");
        return;
    };
    let cache = std::env::temp_dir().join(format!("simplane-it-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&cache);
    let frame = Frame::new(54.6872, 25.2797);
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let keys = [TileKey::new(16, 37370, 20827), TileKey::new(13, 4671, 2603)];

    let mk = |u: &str| {
        let _g = rt.enter();
        Fetcher::new(u, cache.to_str().unwrap(), Arc::new(AtomicU64::new(0)), crate::fetch::FetchParams::default(), Box::new(|s| eprintln!("note: {s:?}")))
            .unwrap()
    };
    let f = mk(&url);
    for k in keys {
        let t0 = Instant::now();
        let (bytes, src) = rt.block_on(f.fetch(k)).unwrap();
        let t_fetch = t0.elapsed();
        let t1 = Instant::now();
        let ring = crate::rings::ring_for_zoom(k.z).unwrap();
        let data = build_tile(k, ring, frame, &Payload::Bytes(bytes.clone()), &Profile::default()).unwrap();
        let lights = crate::lights::LightSet::from_buildings(&tile_buildings(k, frame, &Payload::Bytes(bytes.clone()), &Profile::default()).unwrap(), &Profile::default());
        let t_decode = t1.elapsed();
        let buildings = tile_buildings(k, frame, &Payload::Bytes(bytes.clone()), &Profile::default()).unwrap();
        let owned = buildings.iter().filter(|b| b.owned).count();
        eprintln!(
            "{k}: {} bytes from {src:?} in {t_fetch:?}; decode {:?}: {} features, {} owned, {} lights",
            bytes.len(),
            t_decode,
            data.building_count,
            owned,
            data.lights.positions.len()
        );
        assert_eq!(src, Source::Network);
        assert!(data.building_count > 0);
        assert_eq!(owned, lights.positions.len());
        // Lights only for R2/R3 (z16 = R0 has windows instead).
        assert_eq!(data.lights.is_empty(), ring < 2);
        // Seeds come from the `id` attribute + osm_type, not the MVT feature id.
        let ids: std::collections::HashSet<_> = buildings.iter().map(|b| (b.osm_type, b.id)).collect();
        assert!(ids.len() * 10 >= buildings.len() * 9, "ids mostly unique");
        assert!(buildings.iter().all(|b| b.osm_type != crate::mvt::OsmType::Unknown));
    }
    // A fresh fetcher (as after a restart) serves the same tiles from disk without opening
    // the archive (no "opened" info line is printed for it). The first one closes the cache
    // file first (one open handle per file; this flushes the pending writes).
    assert!(f.close_disk(std::time::Duration::from_secs(2)));
    drop(f);
    let f2 = mk(&url);
    for k in keys {
        let (_, src) = rt.block_on(f2.fetch(k)).unwrap();
        assert_eq!(src, Source::Disk);
    }
    let _ = std::fs::remove_dir_all(&cache);
}

/// Main-thread cost of `Scheduler::update` flying 250 m/s at 300 m, and a climb to 8 km.
/// `cargo test --release bench_update -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_update_camera_cost() {
    use crate::scheduler::Scheduler;
    use crate::worker::{Job, WorkerMsg};
    let frame = Frame::new(54.6872, 25.2797);
    for (name, vel) in [("250 m/s @300 m", [250.0, 0.0, 0.0]), ("climb 60 m/s", [0.0, 0.0, 60.0])] {
        let mut s = Scheduler::new(frame, false, Arc::new(Profile::default()));
        let mut jobs = Vec::new();
        let mut times = Vec::new();
        let t_start = Instant::now();
        let mut cam = [0.0, 0.0, 300.0];
        for _ in 0..3000 {
            for a in 0..3 {
                cam[a] += vel[a] / 60.0;
            }
            let t0 = Instant::now();
            s.update(cam, vel, t_start, &mut jobs);
            times.push(t0.elapsed().as_secs_f64() * 1e6);
            // Instant empty tiles (the decode cost is not on the main thread).
            for j in jobs.drain(..) {
                match j {
                    Job::Fetch { key, ring, job } => {
                        let _ = s.handle(
                            WorkerMsg::Fetched { key, ring, job, result: Ok((bytes::Bytes::new(), Source::Network)) },
                            t_start,
                        );
                    }
                    Job::Decode { key, ring, job, payload } => {
                        let result = build_tile(key, ring, frame, &payload, &Profile::default());
                        let _ = s.handle(WorkerMsg::Decoded { key, ring, job, result }, t_start);
                    }
                }
            }
            for _ in 0..2 {
                let _ = s.pop_ready(true);
            }
            let mut u = Vec::new();
            s.drain_unload(&mut u);
        }
        let steady = &mut times[60..];
        steady.sort_by(|a, b| a.total_cmp(b));
        let n = steady.len();
        eprintln!(
            "{name}: update over {n} frames: avg {:.0}us p50 {:.0}us p99 {:.0}us max {:.0}us; tracked {}",
            steady.iter().sum::<f64>() / n as f64,
            steady[n / 2],
            steady[n * 99 / 100],
            steady[n - 1],
            s.entries().count()
        );
    }
}

/// M4 budget: settle the cover over the Vilnius origin at 300 / 1000 / 3000 / 7000 m with real
/// tiles and the real worker, then report per band: visible tiles, triangles, lights, draw
/// calls (all visible, and inside a 106 deg horizontal view wedge), plus winding checks.
/// `SIMPLANE_TEST_URL=... cargo test --release m4_budget -- --ignored --nocapture`
#[test]
#[ignore]
fn m4_budget() {
    use crate::rings::{ring_for_zoom, RING_COUNT};
    use crate::scheduler::{sim::GodotSim, Scheduler};
    use crate::worker::Worker;
    use std::collections::HashMap;
    let Some(url) = url() else {
        eprintln!("SIMPLANE_TEST_URL not set; skipping");
        return;
    };
    let cache = std::env::temp_dir().join("simplane-m4-budget-cache");
    let frame = Frame::new(54.6872, 25.2797);
    let worker = Worker::new(frame, &url, cache.to_str().unwrap(), Arc::new(Profile::default())).unwrap();
    let mut s = Scheduler::new(frame, false, Arc::new(Profile::default()));
    let mut g = GodotSim::default();
    let sequential = std::env::var("SIMPLANE_BUDGET_SEQ").is_ok_and(|v| v == "1");
    eprintln!("mode: {}", if sequential { "sequential climb (hysteresis carries over)" } else { "fresh cover per altitude" });
    // key -> (tris, single lights, clustered lights)
    let mut info: HashMap<TileKey, (usize, usize, usize)> = HashMap::new();
    // key -> ground (verts, tris)
    let mut ground_info: HashMap<TileKey, (usize, usize)> = HashMap::new();
    // Ground sizes (verts, tris) per first delivery, per ring.
    let mut ground_sizes: [Vec<(usize, usize)>; RING_COUNT] = Default::default();
    // R2 sizes: (verts, tris, bytes) per delivered tile.
    let mut r2_sizes: Vec<(usize, usize, usize)> = Vec::new();
    let mut bad_winding = 0usize;
    let mut checked = 0usize;
    let mut cpu: [Vec<u64>; RING_COUNT] = Default::default();
    let mut jobs = Vec::new();
    for alt in [300.0, 1000.0, 3000.0, 7000.0] {
        let cam = [0.0, 0.0, alt];
        let t0 = Instant::now();
        if !sequential {
            // Stale worker replies are ignored by job id.
            s = Scheduler::new(frame, false, Arc::new(Profile::default()));
            g = GodotSim::default();
        }
        loop {
            while let Some(m) = worker.try_recv() {
                if let crate::worker::WorkerMsg::Decoded { ring, result: Ok(d), .. } = &m {
                    cpu[*ring].push(d.cpu_us);
                }
                if let Some(w) = s.handle(m, Instant::now()) {
                    eprintln!("{w}");
                }
            }
            s.update(cam, [0.0; 3], Instant::now(), &mut jobs);
            for j in jobs.drain(..) {
                worker.submit(j);
            }
            while let Some(t) = s.pop_ready(true) {
                for m in [&t.mesh, &t.ground] {
                for tri in m.indices.chunks_exact(3) {
                    let p = |i: i32| m.positions[i as usize];
                    let (a, b, c) = (p(tri[0]), p(tri[1]), p(tri[2]));
                    let u = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
                    let w = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                    let gx = [u[1] * w[2] - u[2] * w[1], u[2] * w[0] - u[0] * w[2], u[0] * w[1] - u[1] * w[0]];
                    let l = (gx[0] * gx[0] + gx[1] * gx[1] + gx[2] * gx[2]).sqrt();
                    let n = m.normals[tri[0] as usize];
                    if l > 1e-6 {
                        checked += 1;
                        if (gx[0] * n[0] + gx[1] * n[1] + gx[2] * n[2]) / l < 0.9 {
                            bad_winding += 1;
                        }
                    }
                }
                }
                let m = &t.mesh;
                if t.ring < 3 && !t.redelivery {
                    ground_sizes[t.ring].push((t.ground.vertex_count(), t.ground.triangle_count()));
                }
                ground_info.insert(t.key, (t.ground.vertex_count(), t.ground.triangle_count()));
                info.insert(t.key, (m.triangle_count(), t.lights.positions.len(), t.lights.clustered_positions.len()));
                if t.ring == 2 && !t.redelivery {
                    r2_sizes.push((m.vertex_count(), m.triangle_count(), m.bytes()));
                }
                g.upload(t.key);
            }
            let mut un = Vec::new();
            s.drain_unload(&mut un);
            for k in un {
                g.unload(k);
            }
            let errs = g.check();
            assert!(errs.is_empty(), "{errs:?}");
            if s.settled() {
                break;
            }
            assert!(t0.elapsed().as_secs() < 600, "not settled at {alt}");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let vis = g.visible_tiles();
        let mut tiles = [0usize; RING_COUNT];
        let mut tris = [0usize; RING_COUNT];
        let mut lights = [0usize; RING_COUNT];
        let mut mesh_draws = [0usize; RING_COUNT];
        let mut light_draws = [0usize; RING_COUNT];
        let mut wedge_draws = [0usize; RING_COUNT];
        let mut wedge_tiles = [0usize; RING_COUNT];
        let mut wedge_tris = 0usize;
        let mut ground_tris = [0usize; RING_COUNT];
        let mut ground_draws = [0usize; RING_COUNT];
        let mut wedge_ground = [0usize; RING_COUNT];
        let clustered = s.clustered();
        // Horizontal view wedge: heading north, 106 deg (75 deg vertical fov at 16:9).
        let half = 53f64.to_radians();
        let in_wedge = |k: TileKey| {
            let r = frame.tile_rect(k);
            if r.min_e <= 0.0 && r.max_e >= 0.0 && r.min_n <= 0.0 && r.max_n >= 0.0 {
                return true;
            }
            (0..=8).any(|i| {
                (0..=8).any(|j| {
                    let e = r.min_e + r.width() * i as f64 / 8.0;
                    let n = r.min_n + r.height() * j as f64 / 8.0;
                    e.atan2(n).abs() <= half
                })
            })
        };
        for &k in &vis {
            let r = ring_for_zoom(k.z).unwrap();
            let (t, l1, lc) = info[&k];
            let l = if clustered { lc } else { l1 };
            tiles[r] += 1;
            tris[r] += t;
            lights[r] += l;
            let gt = ground_info.get(&k).map_or(0, |x| x.1);
            ground_tris[r] += gt;
            ground_draws[r] += (gt > 0) as usize;
            let draws = (t > 0) as usize + (l > 0) as usize + (gt > 0) as usize;
            mesh_draws[r] += (t > 0) as usize;
            light_draws[r] += (l > 0) as usize;
            if in_wedge(k) {
                wedge_draws[r] += draws;
                wedge_tiles[r] += 1;
                wedge_tris += t + gt;
                wedge_ground[r] += (gt > 0) as usize;
            }
        }
        let held_hidden = g.uploaded.len() - vis.len();
        let hidden_tris: usize = g.uploaded.iter().filter(|k| !g.visible(**k)).map(|k| info[k].0).sum();
        eprintln!(
            "ALT {alt} m (settled in {:.1}s, lights {}): visible tiles per ring {tiles:?} (total {}), tris {tris:?} (total {}), lights {lights:?}",
            t0.elapsed().as_secs_f64(),
            if clustered { "clustered" } else { "single" },
            vis.len(),
            tris.iter().sum::<usize>()
        );
        eprintln!(
            "    ground: tris per ring {ground_tris:?} (total {}), draws {ground_draws:?}",
            ground_tris.iter().sum::<usize>()
        );
        eprintln!(
            "    draw calls: mesh {mesh_draws:?} + ground {ground_draws:?} + lights {light_draws:?} = {} all around; in 106deg wedge: tiles {wedge_tiles:?}, draws {wedge_draws:?} = {} (of which ground {}), wedge tris (buildings + ground) {wedge_tris}; hidden uploaded parents {held_hidden} ({hidden_tris} building tris resident)",
            mesh_draws.iter().sum::<usize>() + ground_draws.iter().sum::<usize>() + light_draws.iter().sum::<usize>(),
            wedge_draws.iter().sum::<usize>(),
            wedge_ground.iter().sum::<usize>()
        );
        assert!(wedge_draws.iter().sum::<usize>() <= 300, "draw-call cap");
        let r2: Vec<usize> = vis.iter().filter(|k| k.z == 14).map(|k| info[k].0).collect();
        if !r2.is_empty() {
            let mut r2s = r2.clone();
            r2s.sort();
            eprintln!(
                "    R2 tris per visible tile: n {} min {} median {} max {} sum {}",
                r2s.len(),
                r2s[0],
                r2s[r2s.len() / 2],
                r2s[r2s.len() - 1],
                r2s.iter().sum::<usize>()
            );
        }
    }
    for (r, c) in cpu.iter_mut().enumerate() {
        if c.is_empty() {
            continue;
        }
        c.sort();
        eprintln!("worker cpu R{r}: n {} median {}us p90 {}us max {}us", c.len(), c[c.len() / 2], c[c.len() * 9 / 10], c[c.len() - 1]);
    }
    r2_sizes.sort();
    if !r2_sizes.is_empty() {
        let n = r2_sizes.len();
        let q = |p: f64| r2_sizes[((n - 1) as f64 * p) as usize];
        eprintln!(
            "R2 tile sizes (verts, tris, bytes) over {n} deliveries: median {:?} p90 {:?} max {:?}; > 8k verts: {}, > 16k verts: {}",
            q(0.5),
            q(0.9),
            r2_sizes[n - 1],
            r2_sizes.iter().filter(|x| x.0 > 8192).count(),
            r2_sizes.iter().filter(|x| x.0 > 16384).count()
        );
    }
    for (r, g) in ground_sizes.iter_mut().enumerate() {
        if g.is_empty() {
            continue;
        }
        g.sort();
        let n = g.len();
        eprintln!(
            "ground R{r} per tile (verts, tris) over {n} deliveries: median {:?} p90 {:?} max {:?}; empty {}",
            g[n / 2],
            g[n * 9 / 10],
            g[n - 1],
            g.iter().filter(|x| x.1 == 0).count()
        );
    }
    eprintln!("winding checked {checked} bad {bad_winding}");
    assert_eq!(bad_winding, 0);
}
