//! Main-thread and idle-CPU measurements (ignored; run in release with `--nocapture`):
//! `cargo test --release -p simworld-core perf_ -- --ignored --nocapture --test-threads=1`.
//!
//! - `perf_update_camera_flat_vs_terrain`: synthetic city, 250 m/s at 300 m AGL, 60 Hz real time,
//!   the host loop of the bench (update_camera, pop_ready 2, fades, unloads); prints p50 / p95 /
//!   max of `update_camera` for the flat world and with the terrain layer.
//! - `perf_idle_worker_cpu`: after the world settled, the CPU the other threads use per frame
//!   while nothing is queued.
//! - `perf_synthetic_cold_fill`: time from `configure_world` until the cover at the origin is
//!   delivered and nothing is queued (flat and terrain).

use crate::diag::thread_cpu_us;
use crate::world::{World, WorldOptions};
use std::sync::Arc;
use std::time::{Duration, Instant};

const VILNIUS: (f64, f64) = (54.6872, 25.2797);

fn world() -> World {
    World::new(Arc::new(crate::NullLog), crate::Diagnostics::default(), "perf")
}

fn options(terrain: bool) -> WorldOptions {
    WorldOptions {
        origin_lat: VILNIUS.0,
        origin_lon: VILNIUS.1,
        profile_overrides: terrain.then(|| serde_json::json!({ "terrain": { "enabled": true } })),
        layers: terrain.then(|| vec!["world".into(), "terrain".into()]),
        ..WorldOptions::default()
    }
}

/// One host frame without the camera update.
fn host_rest(w: &mut World) -> usize {
    let n = w.pop_ready(2, |_| {});
    let _ = w.drain_fades();
    let _ = w.drain_unload();
    n
}

/// Percentiles (p50, p95, max) of `v` in ms.
fn pcts(v: &mut [f64]) -> (f64, f64, f64) {
    v.sort_by(f64::total_cmp);
    let at = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
    (at(0.5), at(0.95), *v.last().unwrap())
}

/// Wait until the world at the origin is delivered and idle; returns the seconds it took.
fn settle(w: &mut World, timeout_s: f64) -> f64 {
    let t0 = Instant::now();
    let mut n = 0;
    loop {
        let ft = Instant::now();
        w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
        n += w.pop_ready(64, |_| {});
        let _ = w.drain_fades();
        let _ = w.drain_unload();
        let s = w.stats();
        let busy: i32 = s.queued.iter().chain(s.fetching.iter()).chain(s.meshing.iter()).sum();
        let tb: i32 = s.terrain_queued.iter().chain(s.terrain_fetching.iter()).chain(s.terrain_meshing.iter()).sum();
        if n > 0 && busy == 0 && tb == 0 {
            return t0.elapsed().as_secs_f64();
        }
        assert!(t0.elapsed().as_secs_f64() < timeout_s, "did not settle: {s:?}");
        std::thread::sleep(Duration::from_millis(1).saturating_sub(ft.elapsed()));
    }
}

/// A flight's measurements: `update_camera` wall and thread-CPU times (ms) per frame, and the
/// CPU the other threads used per frame (ms).
struct Flight {
    wall: Vec<f64>,
    cpu: Vec<f64>,
    others_ms: f64,
}

/// Fly north at `speed` for `seconds` (60 Hz, real time).
fn fly(terrain: bool, speed: f64, seconds: f64) -> Flight {
    let mut w = world();
    assert!(w.configure_world(options(terrain)).ok);
    settle(&mut w, 60.0);
    let (mut wall, mut cpu) = (Vec::new(), Vec::new());
    let mut z = 0.0f64;
    let frames = (seconds * 60.0) as usize;
    let (p0, m0) = (process_cpu_us(), thread_cpu_us());
    for _ in 0..frames {
        let ft = Instant::now();
        z -= speed / 60.0;
        let (t, c) = (Instant::now(), thread_cpu_us());
        if let Some(s) = w.update_camera([0.0, 300.0, z], [0.0, 0.0, -speed], 300.0) {
            z -= s[2];
        }
        wall.push(t.elapsed().as_secs_f64() * 1000.0);
        cpu.push((thread_cpu_us() - c) as f64 / 1000.0);
        host_rest(&mut w);
        std::thread::sleep(Duration::from_micros(16_667).saturating_sub(ft.elapsed()));
    }
    let (p1, m1) = (process_cpu_us(), thread_cpu_us());
    w.shutdown();
    Flight { wall, cpu, others_ms: (p1 - p0).saturating_sub(m1 - m0) as f64 / 1000.0 / frames as f64 }
}

#[test]
#[ignore]
fn perf_update_camera_flat_vs_terrain() {
    let secs: f64 = std::env::var("SIMWORLD_PERF_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(40.0);
    let only = std::env::var("SIMWORLD_PERF_ONLY").unwrap_or_default();
    for terrain in [false, true] {
        if (only == "flat" && terrain) || (only == "terrain" && !terrain) {
            continue;
        }
        let mut f = fly(terrain, 250.0, secs);
        let n = f.wall.len();
        let over = |v: &[f64], ms: f64| v.iter().filter(|&&t| t > ms).count();
        let (o1, o3) = (over(&f.cpu, 1.0), over(&f.cpu, 3.0));
        let slowest: Vec<usize> = {
            let mut i: Vec<usize> = (0..n).collect();
            i.sort_by(|a, b| f.cpu[*b].total_cmp(&f.cpu[*a]));
            i.truncate(3);
            i
        };
        let (w50, w95, wmax) = pcts(&mut f.wall);
        let (c50, c95, cmax) = pcts(&mut f.cpu);
        eprintln!(
            "update_camera {:7} 250 m/s 300 m, {n} frames: wall p50 {w50:.3} / p95 {w95:.3} / max {wmax:.3} ms; \
             thread CPU p50 {c50:.3} / p95 {c95:.3} / max {cmax:.3} ms ({o1} frames > 1 ms, {o3} > 3 ms; slowest frames {slowest:?}); other threads {:.2} ms CPU / frame",
            if terrain { "terrain" } else { "flat" },
            f.others_ms
        );
    }
}

/// CPU time of the whole process (user + system), microseconds.
fn process_cpu_us() -> u64 {
    // SAFETY: valid out-pointer.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    let tv = |t: libc::timeval| t.tv_sec as u64 * 1_000_000 + t.tv_usec as u64;
    tv(ru.ru_utime) + tv(ru.ru_stime)
}

#[test]
#[ignore]
fn perf_idle_worker_cpu() {
    for terrain in [false, true] {
        let mut w = world();
        assert!(w.configure_world(options(terrain)).ok);
        settle(&mut w, 60.0);
        // Let in-flight work finish (idle eviction timers and the like are not in this window).
        std::thread::sleep(Duration::from_millis(300));
        let frames = 180;
        let (p0, m0) = (process_cpu_us(), thread_cpu_us());
        for _ in 0..frames {
            let ft = Instant::now();
            w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
            host_rest(&mut w);
            std::thread::sleep(Duration::from_micros(16_667).saturating_sub(ft.elapsed()));
        }
        let (p1, m1) = (process_cpu_us(), thread_cpu_us());
        let others = (p1 - p0).saturating_sub(m1 - m0);
        eprintln!(
            "idle {:7}: other threads {:.3} ms CPU per frame, main thread {:.3} ms per frame ({frames} frames)",
            if terrain { "terrain" } else { "flat" },
            others as f64 / 1000.0 / frames as f64,
            (m1 - m0) as f64 / 1000.0 / frames as f64
        );
        w.shutdown();
    }
}

#[test]
#[ignore]
fn perf_synthetic_cold_fill() {
    for terrain in [false, true] {
        let mut v = Vec::new();
        for _ in 0..3 {
            let mut w = world();
            assert!(w.configure_world(options(terrain)).ok);
            v.push(settle(&mut w, 60.0));
            w.shutdown();
        }
        eprintln!("cold fill {:7}: {:?} s", if terrain { "terrain" } else { "flat" }, v.iter().map(|s| (s * 1000.0).round() / 1000.0).collect::<Vec<_>>());
    }
}

/// The job pool between jobs: 4 worker threads, 6 tiny jobs (about 20 us each) per 60 Hz frame,
/// the pattern of a streaming frame. Prints the CPU of the pool threads per frame beyond the
/// jobs' own work (spinning or waking between jobs).
#[test]
#[ignore]
fn perf_job_pool_trickle() {
    use crate::jobs::{JobEnd, JobPool, JobSpec, PoolConfig, Priority};
    let pool = JobPool::new(PoolConfig::new(4, "perf-jobs")).unwrap();
    let work_us = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let frames = 300u64;
    let (p0, m0) = (process_cpu_us(), thread_cpu_us());
    let mut id = 0;
    for _ in 0..frames {
        let ft = Instant::now();
        for _ in 0..6 {
            id += 1;
            let w = work_us.clone();
            pool.submit(
                JobSpec::new(id, Priority::on_demand(0.0)),
                move |_| {
                    let c = thread_cpu_us();
                    let t = Instant::now();
                    while t.elapsed() < Duration::from_micros(20) {
                        std::hint::spin_loop();
                    }
                    w.fetch_add(thread_cpu_us() - c, std::sync::atomic::Ordering::Relaxed);
                    JobEnd::Done(None)
                },
                || {},
            );
        }
        std::thread::sleep(Duration::from_micros(16_667).saturating_sub(ft.elapsed()));
    }
    let (p1, m1) = (process_cpu_us(), thread_cpu_us());
    let others = (p1 - p0).saturating_sub(m1 - m0);
    let work = work_us.load(std::sync::atomic::Ordering::Relaxed);
    eprintln!(
        "job pool trickle: pool threads {:.3} ms CPU per frame, of which job work {:.3} ms, overhead {:.3} ms",
        others as f64 / 1000.0 / frames as f64,
        work as f64 / 1000.0 / frames as f64,
        others.saturating_sub(work) as f64 / 1000.0 / frames as f64
    );
    assert!(pool.shutdown(Duration::from_secs(1)));
}
