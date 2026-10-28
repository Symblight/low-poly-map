//! Streaming perf harness (perf review 2, ignored; release, `--nocapture --test-threads=1`):
//! `cargo test --release -p simworld-core perf_stream -- --ignored --nocapture --test-threads=1`.
//! With `SIMWORLD_V2_ARCHIVE=<vilnius-v2.pmtiles>` the world runs on the real two-level archive
//! (in-process range server, no disk cache), else on the synthetic city. Terrain on throughout.
//!
//! - `perf_stream_main_thread`: first `update_camera` after `configure_world`, the worst frame of
//!   the settle, then a level flight at 250 m/s, 300 m (60 Hz real time): p50 / max of
//!   `update_camera` and of `pop_ready` per delivered tile, frames over 3 ms, and keys delivered
//!   again within 10 s of their unload (churn).
//! - `perf_stream_cold_fill`: 8 s at the origin after `configure_world`: first and last delivery
//!   per band and the latency table of the fill (`mesh_wait` per band).
//!
//! `SIMWORLD_PERF_SECONDS` = flight seconds (default 30); `SIMWORLD_PERF_SET` = JSON profile
//! overrides merged in (e.g. `{"streaming":{"worker_threads":8}}`).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::rings::LayerKey;
use crate::world::{World, WorldOptions};

const VILNIUS: (f64, f64) = (54.6872, 25.2797);
const FRAME: Duration = Duration::from_micros(16_667);

pub(crate) fn source() -> (String, &'static str) {
    match std::env::var("SIMWORLD_V2_ARCHIVE") {
        Ok(p) => (crate::data_world_tests::serve(std::fs::read(p).expect("v2 archive")).0, "v2"),
        Err(_) => (String::new(), "synthetic"),
    }
}

pub(crate) fn options(url: &str) -> WorldOptions {
    let mut ov = serde_json::json!({ "terrain": { "enabled": true } });
    if let Some(extra) = std::env::var("SIMWORLD_PERF_SET").ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()) {
        merge(&mut ov, extra);
    }
    WorldOptions {
        origin_lat: VILNIUS.0,
        origin_lon: VILNIUS.1,
        tile_url: url.into(),
        profile_overrides: Some(ov),
        layers: Some(vec!["world".into(), "terrain".into(), "landmarks".into()]),
        ..WorldOptions::default()
    }
}

fn merge(a: &mut serde_json::Value, b: serde_json::Value) {
    match (a, b) {
        (serde_json::Value::Object(a), serde_json::Value::Object(b)) => {
            for (k, v) in b {
                merge(a.entry(k).or_insert(serde_json::Value::Null), v);
            }
        }
        (a, b) => *a = b,
    }
}

fn seconds() -> f64 {
    std::env::var("SIMWORLD_PERF_SECONDS").ok().and_then(|s| s.parse().ok()).unwrap_or(30.0)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// p50 and max of `v`.
fn p50_max(v: &mut [f64]) -> (f64, f64) {
    if v.is_empty() {
        return (0.0, 0.0);
    }
    v.sort_by(f64::total_cmp);
    (v[v.len() / 2], v[v.len() - 1])
}

fn busy(w: &World) -> i32 {
    let s = w.stats();
    let tiles: i32 = s.queued.iter().chain(&s.fetching).chain(&s.meshing).chain(&s.terrain_queued).chain(&s.terrain_fetching).chain(&s.terrain_meshing).sum();
    tiles + s.probes_pending as i32
}

/// Per-frame measurements of a run.
#[derive(Default)]
struct Run {
    update_ms: Vec<f64>,
    /// Thread CPU time of `update_camera` (robust against preemption on a busy machine).
    update_cpu_ms: Vec<f64>,
    pop_ms_per_tile: Vec<f64>,
    unloaded_at: HashMap<LayerKey, Instant>,
    churn: usize,
    /// Churned keys by (layer, zoom, behind the camera when delivered again).
    churn_kinds: std::collections::BTreeMap<(String, u8, bool), usize>,
    /// Camera north (m, absolute ENU of the configure origin) for the classification.
    north: f64,
    delivered: usize,
}

impl Run {
    /// One host frame at `pos`, velocity `vel`; returns the origin shift.
    fn frame(&mut self, w: &mut World, pos: [f64; 3], vel: [f64; 3]) -> Option<[f64; 3]> {
        let (t, c) = (Instant::now(), crate::diag::thread_cpu_us());
        let shift = w.update_camera(pos, vel, pos[1]);
        self.update_ms.push(ms(t.elapsed()));
        self.update_cpu_ms.push((crate::diag::thread_cpu_us() - c) as f64 / 1e3);
        let t = Instant::now();
        let mut keys = Vec::new();
        let n = w.pop_ready(2, |v| keys.push(v.layer_key()));
        if n > 0 {
            self.pop_ms_per_tile.push(ms(t.elapsed()) / n as f64);
        }
        let now = Instant::now();
        for k in keys {
            self.delivered += 1;
            if self.unloaded_at.get(&k).is_some_and(|u| now.duration_since(*u) < Duration::from_secs(10)) {
                self.churn += 1;
                let tile_north = crate::geo::Frame::new(VILNIUS.0, VILNIUS.1).tile_center(k.key)[1];
                *self.churn_kinds.entry((k.layer.name().to_string(), k.key.z, tile_north < self.north)).or_default() += 1;
            }
        }
        let _ = w.drain_fades();
        for &k in w.drain_unload() {
            self.unloaded_at.insert(k, now);
        }
        shift
    }
}

#[test]
#[ignore]
fn perf_stream_main_thread() {
    let (url, name) = source();
    for run in 0..3 {
        let mut w = World::new(Arc::new(crate::NullLog), crate::Diagnostics::default(), "perf");
        assert!(w.configure_world(options(&url)).ok);
        let mut r = Run::default();
        // Settle at the origin (first frame = the cold one).
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(60) {
            let ft = Instant::now();
            r.frame(&mut w, [0.0, 300.0, 0.0], [0.0; 3]);
            if r.delivered > 0 && busy(&w) == 0 {
                break;
            }
            std::thread::sleep(FRAME.saturating_sub(ft.elapsed()));
        }
        let first = (r.update_ms[0], r.update_cpu_ms[0]);
        let settle_max = r.update_cpu_ms[1..].iter().cloned().fold(0.0, f64::max);
        let settle_s = t0.elapsed().as_secs_f64();
        let settle_upd: f64 = r.update_cpu_ms.iter().sum();
        let (sp50, spmax) = p50_max(&mut r.pop_ms_per_tile);
        // Level flight north.
        let mut f = Run::default();
        let (speed, mut z) = (250.0, 0.0f64);
        for _ in 0..(seconds() * 60.0) as usize {
            let ft = Instant::now();
            z -= speed / 60.0;
            f.north += speed / 60.0;
            if let Some(s) = f.frame(&mut w, [0.0, 300.0, z], [0.0, 0.0, -speed]) {
                z -= s[2];
            }
            std::thread::sleep(FRAME.saturating_sub(ft.elapsed()));
        }
        eprintln!("[{name} run {run}] churn by (layer, zoom, behind the camera): {:?}", f.churn_kinds);
        let over3 = f.update_cpu_ms.iter().filter(|&&t| t > 3.0).count();
        let (u50, umax) = p50_max(&mut f.update_ms);
        let (c50, cmax) = p50_max(&mut f.update_cpu_ms);
        let (p50, pmax) = p50_max(&mut f.pop_ms_per_tile);
        eprintln!(
            "[{name} run {run}] first update_camera {:.2} ms wall / {:.2} ms CPU, settle max {settle_max:.2} ms CPU, all frames {settle_upd:.1} ms CPU, pop_ready per tile p50 {sp50:.3} / max {spmax:.2} ms ({settle_s:.1} s); flight {:.0} s: \
             update_camera wall p50 {u50:.3} / max {umax:.2} ms, CPU p50 {c50:.3} / max {cmax:.2} ms ({over3} frames > 3 ms CPU), pop_ready per tile p50 {p50:.3} / max {pmax:.2} ms, \
             delivered {}, re-delivered within 10 s of unload {} ({:.1} / min), recent-tile cache {:?}",
            first.0,
            first.1,
            seconds(),
            f.delivered,
            f.churn,
            f.churn as f64 / seconds() * 60.0,
            w.stats().recent
        );
        assert!(w.shutdown());
    }
}

#[test]
#[ignore]
fn perf_stream_cold_fill() {
    let (url, name) = source();
    let mut w = World::new(Arc::new(crate::NullLog), crate::Diagnostics::default(), "perf");
    assert!(w.configure_world(options(&url)).ok);
    let threads = w.worker().map_or(0, |x| x.cpu_threads());
    // A fixed 8 s at 60 Hz (an idle check misses data tiles in flight): first and last delivery
    // per band (vector R0..R3, then terrain), tiles held at the end.
    let t0 = Instant::now();
    let (mut first, mut last) = ([f64::NAN; 5], [f64::NAN; 5]);
    while t0.elapsed() < Duration::from_secs(8) {
        let ft = Instant::now();
        let mut got = Vec::new();
        w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
        w.pop_ready(2, |v| got.push((v.layer, v.ring)));
        let _ = w.drain_fades();
        let _ = w.drain_unload();
        let el = ms(t0.elapsed());
        for (layer, ring) in got {
            let b = if layer == crate::rings::LayerKind::Terrain { 4 } else { ring.min(3) };
            if first[b].is_nan() {
                first[b] = el;
            }
            last[b] = el;
        }
        std::thread::sleep(FRAME.saturating_sub(ft.elapsed()));
    }
    let terrain_loaded: i32 = w.stats().terrain_loaded.iter().sum();
    let r = |v: [f64; 5]| v.map(|x| x.round());
    eprintln!(
        "[{name}, {threads} worker threads] cold fill 8 s: first delivery per band R0..R3, terrain (ms) {:?}; last delivery {:?}; vector tiles held {:?}, terrain tiles held {terrain_loaded}\n{}",
        r(first),
        r(last),
        w.stats().loaded,
        w.latency().map(|l| l.report()).unwrap_or_default()
    );
    assert!(w.shutdown());
}

/// Overview data tiles of the real archive (`SIMWORLD_V2_ARCHIVE`) decoded with the terrain on
/// (perf review 2 item 7): thread CPU of the decode per tile (best of 3) for the 9 z11 tiles
/// around the densest one, the time before any of its 64 chunks can mesh.
#[test]
#[ignore]
fn perf_stream_overview_decode() {
    let Ok(v2) = std::env::var("SIMWORLD_V2_ARCHIVE") else {
        eprintln!("SIMWORLD_V2_ARCHIVE not set; skipped");
        return;
    };
    let (url, _) = crate::data_world_tests::serve(std::fs::read(v2).expect("v2 archive"));
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let f = crate::fetch::Fetcher::new(&url, "", Arc::new(std::sync::atomic::AtomicU64::new(0)), crate::fetch::FetchParams::default(), Box::new(|_| {})).unwrap();
    let mut w = crate::profile::WorldProfile::default();
    w.terrain.enabled = true;
    let profile = crate::profile::Profile::new(w).unwrap();
    let frame = crate::geo::Frame::new(VILNIUS.0, VILNIUS.1);
    let terrain = crate::Terrain::from_profile(&profile, VILNIUS.0, VILNIUS.1);
    let (mut total, mut worst, mut blocks) = (0.0f64, (0.0f64, crate::geo::TileKey::new(0, 0, 0)), 0usize);
    for dy in 0..3u32 {
        for dx in 0..3u32 {
            let key = crate::geo::TileKey::new(11, 1166 + dx, 649 + dy);
            let Ok((bytes, _)) = rt.block_on(f.fetch(key)) else { continue };
            let mut best = f64::INFINITY;
            for _ in 0..3 {
                let c = crate::diag::thread_cpu_us();
                let dt = crate::data::decode_overview_tile(key, frame, &terrain, &bytes, &profile).unwrap();
                best = best.min((crate::diag::thread_cpu_us() - c) as f64 / 1e3);
                blocks = blocks.max(dt.buildings.len());
            }
            total += best;
            if best > worst.0 {
                worst = (best, key);
            }
        }
    }
    eprintln!("overview decode with terrain, 9 tiles around 11/1167/650: total {total:.0} ms CPU, worst {:.0} ms ({}), most blocks in a tile {blocks}", worst.0, worst.1);
}
