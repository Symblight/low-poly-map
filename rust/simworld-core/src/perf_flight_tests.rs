//! Long-flight main-thread harness (perf review 3 item 2, ignored; release, `--nocapture`):
//! `cargo test --release -p simworld-core perf_flight -- --ignored --nocapture --test-threads=1`.
//!
//! `perf_flight_long`: a level flight north at 250 m/s, 300 m, 60 Hz real time, terrain and
//! landmark layers on (the bench's host loop: `update_camera`, `pop_ready(2)`, fades, unloads),
//! for `SIMWORLD_PERF_SECONDS` (default 300). Prints `update_camera` thread CPU and wall time
//! p50 / p99 / max per 60 s window and over the run, plus the recent-tile cache size, so work
//! that grows with the run length shows as rising window percentiles. `SIMWORLD_V2_ARCHIVE`
//! selects the real archive (in-process range server), else the synthetic city.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::perf_stream_tests::{options, source};
use crate::world::World;

const FRAME: Duration = Duration::from_micros(16_667);
const WINDOW_FRAMES: usize = 60 * 60;

/// (p50, p99, max) of `v` (ms).
fn p50_p99_max(v: &[f64]) -> (f64, f64, f64) {
    if v.is_empty() {
        return (0.0, 0.0, 0.0);
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let at = |q: f64| s[((s.len() - 1) as f64 * q).round() as usize];
    (at(0.5), at(0.99), s[s.len() - 1])
}

fn line(label: &str, cpu: &[f64], wall: &[f64]) -> String {
    let (c50, c99, cmax) = p50_p99_max(cpu);
    let (w50, w99, wmax) = p50_p99_max(wall);
    format!("{label}: update_camera CPU p50 {c50:.3} / p99 {c99:.3} / max {cmax:.2} ms, wall p50 {w50:.3} / p99 {w99:.3} / max {wmax:.2} ms")
}

#[test]
#[ignore]
fn perf_flight_long() {
    let (url, name) = source();
    let seconds = std::env::var("SIMWORLD_PERF_SECONDS").ok().and_then(|s| s.parse::<f64>().ok()).unwrap_or(300.0);
    let mut w = World::new(Arc::new(crate::NullLog), crate::Diagnostics::default(), "perf");
    assert!(w.configure_world(options(&url)).ok);
    // Settle at the origin (at most 30 s).
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(30) {
        let ft = Instant::now();
        w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
        w.pop_ready(2, |_| {});
        let _ = w.drain_fades();
        let _ = w.drain_unload();
        std::thread::sleep(FRAME.saturating_sub(ft.elapsed()));
    }
    let (speed, mut z) = (250.0, 0.0f64);
    let (mut cpu, mut wall) = (Vec::new(), Vec::new());
    // Frames that started with a box probe on the way (the terrain plan may wait on it).
    let mut pending_frames = 0usize;
    let frames = (seconds * 60.0) as usize;
    for i in 0..frames {
        let ft = Instant::now();
        z -= speed / 60.0;
        pending_frames += (w.stats().probes_pending > 0) as usize;
        let (t, c) = (Instant::now(), crate::diag::thread_cpu_us());
        if let Some(s) = w.update_camera([0.0, 300.0, z], [0.0, 0.0, -speed], 300.0) {
            z -= s[2];
        }
        wall.push(t.elapsed().as_secs_f64() * 1e3);
        cpu.push((crate::diag::thread_cpu_us() - c) as f64 / 1e3);
        w.pop_ready(2, |_| {});
        let _ = w.drain_fades();
        let _ = w.drain_unload();
        if (i + 1) % WINDOW_FRAMES == 0 {
            let a = i + 1 - WINDOW_FRAMES;
            let r = w.stats().recent;
            eprintln!("[{name}] {} recent-tile cache {r:?}", line(&format!("{:>3}-{:>3} s", a / 60, (i + 1) / 60), &cpu[a..], &wall[a..]));
        }
        std::thread::sleep(FRAME.saturating_sub(ft.elapsed()));
    }
    eprintln!("[{name}] {}; {pending_frames} of {frames} frames began with a probe pending", line(&format!("whole {seconds:.0} s"), &cpu, &wall));
    assert!(w.shutdown());
}
