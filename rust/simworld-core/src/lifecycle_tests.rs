//! Host API and lifecycle (api 0.7): geodetic input and conversions, origin shift reporting,
//! status, shutdown, the cache folder lock, several worlds side by side and teleport.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::geo::TileKey;
use crate::rings::LayerKey;
use crate::status::{StatusState, MSG_NOT_CONFIGURED, MSG_OFFLINE, MSG_SHUT_DOWN};
use crate::world::{World, WorldOptions};

const VILNIUS: (f64, f64) = (54.6872, 25.2797);
/// Nothing listens on the discard port: connections are refused at once (no network needed).
const DEAD_URL: &str = "http://127.0.0.1:9/none.pmtiles";

fn world() -> World {
    World::new(Arc::new(crate::NullLog), crate::Diagnostics::default(), "test")
}

fn synthetic(lat: f64, lon: f64) -> WorldOptions {
    WorldOptions { origin_lat: lat, origin_lon: lon, ..WorldOptions::default() }
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("simworld-lifecycle-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn with_cache(url: &str, dir: &std::path::Path) -> WorldOptions {
    WorldOptions {
        origin_lat: VILNIUS.0,
        origin_lon: VILNIUS.1,
        tile_url: url.into(),
        cache_dir: dir.to_string_lossy().into_owned(),
        ..WorldOptions::default()
    }
}

/// Fly a static camera at 300 m AGL until `want` tiles were delivered (or 20 s). Returns the
/// keys held by the host (delivered minus unloaded) and every (key, origin) delivered.
fn run_until(w: &mut World, want: usize) -> (BTreeSet<LayerKey>, Vec<(LayerKey, [f64; 3])>) {
    let mut held = BTreeSet::new();
    let mut delivered = Vec::new();
    let t0 = Instant::now();
    while delivered.len() < want && t0.elapsed() < Duration::from_secs(20) {
        w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
        w.pop_ready(64, |t| {
            held.insert(t.layer_key());
            delivered.push((t.layer_key(), t.origin));
        });
        let _ = w.drain_fades();
        for k in w.drain_unload() {
            held.remove(k);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    (held, delivered)
}

#[test]
fn geodetic_round_trip_is_sub_cm_within_20_km_and_follows_the_origin() {
    let mut w = world();
    assert!(w.geodetic_to_local(VILNIUS.0, VILNIUS.1, 0.0).is_none(), "before configure");
    assert!(w.local_to_geodetic([0.0; 3]).is_none());
    assert!(w.configure_world(synthetic(VILNIUS.0, VILNIUS.1)).ok);
    // Usable right after configure, before the first camera update.
    let o = w.geodetic_to_local(VILNIUS.0, VILNIUS.1, 123.0).unwrap();
    assert!(o[0].abs() < 1e-6 && o[2].abs() < 1e-6 && o[1] == 123.0, "{o:?}");
    let mut worst = 0.0f64;
    for i in -4..=4 {
        for j in -4..=4 {
            let p = [i as f64 * 5000.0, 50.0 * (i + j) as f64, j as f64 * 5000.0];
            if p[0].hypot(p[2]) > 20_000.0 {
                continue;
            }
            let g = w.local_to_geodetic(p).unwrap();
            let back = w.geodetic_to_local(g[0], g[1], g[2]).unwrap();
            let err = ((back[0] - p[0]).powi(2) + (back[1] - p[1]).powi(2) + (back[2] - p[2]).powi(2)).sqrt();
            worst = worst.max(err);
        }
    }
    assert!(worst < 0.01, "round trip error {worst} m");
    // Axes: north is -z, east is +x.
    let n = w.geodetic_to_local(VILNIUS.0 + 0.01, VILNIUS.1, 0.0).unwrap();
    assert!(n[2] < -1100.0 && n[0].abs() < 1.0);
    let e = w.geodetic_to_local(VILNIUS.0, VILNIUS.1 + 0.01, 0.0).unwrap();
    assert!(e[0] > 600.0 && e[2].abs() < 1.0);
    // Invalid input.
    assert!(w.geodetic_to_local(f64::NAN, 0.0, 0.0).is_none());
    assert!(w.geodetic_to_local(91.0, 0.0, 0.0).is_none());
    assert!(w.geodetic_to_local(0.0, 181.0, 0.0).is_none());
    assert!(w.local_to_geodetic([f64::INFINITY, 0.0, 0.0]).is_none());

    // After a re-center the same geodetic point moves by exactly -shift.
    let target = w.local_to_geodetic([7000.0, 0.0, -3000.0]).unwrap();
    let before = w.geodetic_to_local(target[0], target[1], 500.0).unwrap();
    let shift = w.update_camera([7000.0, 500.0, -3000.0], [0.0; 3], 500.0).expect("re-centered");
    assert_eq!(shift, [7000.0, 0.0, -3000.0]);
    let after = w.geodetic_to_local(target[0], target[1], 500.0).unwrap();
    for a in 0..3 {
        assert!((after[a] - (before[a] - shift[a])).abs() < 1e-6, "{after:?} vs {before:?} - {shift:?}");
    }
    let g = w.local_to_geodetic(after).unwrap();
    assert!((g[0] - target[0]).abs() < 1e-9 && (g[1] - target[1]).abs() < 1e-9 && g[2] == 500.0);
}

#[test]
fn geodetic_camera_drives_the_same_cover_and_reports_the_shift() {
    let mut a = world();
    let mut b = world();
    assert!(a.configure_world(synthetic(VILNIUS.0, VILNIUS.1)).ok);
    assert!(b.configure_world(synthetic(VILNIUS.0, VILNIUS.1)).ok);
    // A point 1.2 km east and 0.8 km north, 400 m MSL, flying north-east and climbing.
    let p = [1200.0, 400.0, -800.0];
    let g = a.local_to_geodetic(p).unwrap();
    let vel_ned = [30.0, 40.0, -5.0];
    assert_eq!(a.update_camera_geodetic(g[0], g[1], g[2], vel_ned), Ok(None));
    // Same as local input with x = E, y = -D, z = -N and AGL = MSL - terrain (0).
    b.update_camera(p, [40.0, 5.0, -30.0], 400.0);
    let tiles = |w: &World| {
        let mut v = Vec::new();
        w.debug_tiles(|k, ring, _, r| v.push((k, ring, r.map(|x| (x * 1000.0).round() as i64))));
        v.sort();
        v
    };
    let ta = tiles(&a);
    assert!(!ta.is_empty());
    assert_eq!(ta, tiles(&b));

    // Invalid input: rejected, logged, nothing changes.
    let stats = a.stats();
    for (lat, lon, alt, v) in [
        (f64::NAN, g[1], 400.0, vel_ned),
        (95.0, g[1], 400.0, vel_ned),
        (g[0], -180.5, 400.0, vel_ned),
        (g[0], g[1], f64::INFINITY, vel_ned),
        (g[0], g[1], 400.0, [0.0, f64::NAN, 0.0]),
    ] {
        assert!(a.update_camera_geodetic(lat, lon, alt, v).is_err());
    }
    assert_eq!(a.stats(), stats);
    assert_eq!(tiles(&a), ta);

    // A > 5 km jump re-centers: returned once, and take_origin_shift reports the same vector.
    let far = a.local_to_geodetic([6000.0, 0.0, 2500.0]).unwrap();
    let s = a.update_camera_geodetic(far[0], far[1], 1000.0, [0.0; 3]).unwrap().expect("re-centered");
    assert!((s[0] - 6000.0).abs() < 1e-6 && s[1] == 0.0 && (s[2] - 2500.0).abs() < 1e-6, "{s:?}");
    assert_eq!(a.take_origin_shift(), s);
    assert_eq!(a.take_origin_shift(), [0.0; 3]);
    let here = a.geodetic_to_local(far[0], far[1], 1000.0).unwrap();
    assert!(here[0].abs() < 1e-6 && here[2].abs() < 1e-6);
    assert_eq!(a.update_camera_geodetic(far[0], far[1], 1000.0, [0.0; 3]), Ok(None));
}

#[test]
fn two_synthetic_worlds_run_side_by_side() {
    let mut a = world();
    let mut b = world();
    assert!(a.configure_world(synthetic(VILNIUS.0, VILNIUS.1)).ok);
    assert!(b.configure_world(synthetic(VILNIUS.0, VILNIUS.1)).ok);
    let (_, da) = run_until(&mut a, 30);
    let (_, db) = run_until(&mut b, 30);
    assert!(da.len() >= 30 && db.len() >= 30, "{} / {}", da.len(), db.len());
    // Same inputs, same cover: the first deliveries are the same tiles.
    let ka: BTreeSet<_> = da.iter().map(|d| d.0).collect();
    let kb: BTreeSet<_> = db.iter().map(|d| d.0).collect();
    assert!(ka.intersection(&kb).count() >= 20);
    // Neither holds a cache lock (synthetic city).
    assert!(a.locked_cache_dir().is_none() && b.locked_cache_dir().is_none());
    assert_eq!(a.status().state, StatusState::Ok);
}

#[test]
fn teleport_unloads_every_delivered_tile_and_streams_the_new_origin() {
    let mut w = world();
    assert!(w.configure_world(synthetic(VILNIUS.0, VILNIUS.1)).ok);
    let (held, delivered) = run_until(&mut w, 40);
    assert!(delivered.len() >= 40 && !held.is_empty());
    // Teleport to Kaunas (~90 km away).
    let kaunas = (54.8985, 23.9036);
    assert!(w.configure_world(synthetic(kaunas.0, kaunas.1)).ok);
    let unloaded: BTreeSet<LayerKey> = w.drain_unload().iter().copied().collect();
    assert_eq!(unloaded, held, "every key the host holds is unloaded");
    assert!(w.drain_unload().is_empty(), "only once");
    assert_eq!(w.take_origin_shift(), [0.0; 3]);
    let st = w.stats();
    assert!((st.origin_lat - kaunas.0).abs() < 1e-9 && (st.origin_lon - kaunas.1).abs() < 1e-9);
    let (_, fresh) = run_until(&mut w, 20);
    assert!(fresh.len() >= 20);
    for (k, o) in &fresh {
        assert!(!held.contains(k), "{k} is an old tile");
        assert!(o[0].hypot(o[2]) < 30_000.0, "{k} at {o:?} is not around the new origin");
    }
}

#[test]
fn cache_dir_lock_one_world_per_folder() {
    let d1 = tmp("lock-a");
    let d2 = tmp("lock-b");
    let mut a = world();
    let mut b = world();
    // Folder created on demand; lock taken synchronously.
    assert!(a.configure_world(with_cache(DEAD_URL, &d1)).ok);
    assert!(d1.join(crate::lock::LOCK_FILE_NAME).exists());
    assert!(a.locked_cache_dir().is_some());
    // Second world, same folder: refused, nothing changes for either.
    a.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
    let b_before = b.stats();
    let r = b.configure_world(with_cache(DEAD_URL, &d1));
    assert!(!r.ok);
    assert_eq!(r.error, format!("cache_dir in use: {}", d1.display()));
    assert!(!b.is_running() && b.stats() == b_before);
    assert!(a.is_running());
    // Also through a different spelling of the same folder.
    let alias = d1.join("..").join(d1.file_name().unwrap());
    assert!(!b.configure_world(with_cache(DEAD_URL, &alias)).ok);
    // A different folder is fine; the synthetic city and an empty cache_dir take no lock.
    assert!(b.configure_world(with_cache(DEAD_URL, &d2)).ok);
    let mut c = world();
    assert!(c.configure_world(WorldOptions { tile_url: String::new(), ..with_cache("", &d1) }).ok);
    assert!(c.locked_cache_dir().is_none());
    assert!(c.configure_world(WorldOptions { cache_dir: String::new(), ..with_cache(DEAD_URL, &d1) }).ok);
    assert!(c.locked_cache_dir().is_none());
    // Same-instance re-configure (teleport) with the same folder keeps its lock.
    let held = a.locked_cache_dir().unwrap().to_path_buf();
    let mut o = with_cache(DEAD_URL, &d1);
    o.origin_lat += 0.5;
    assert!(a.configure_world(o).ok, "no self-lockout");
    assert_eq!(a.locked_cache_dir(), Some(held.as_path()));
    // shutdown releases it (and is quick when idle).
    let t0 = Instant::now();
    assert!(a.shutdown());
    assert!(t0.elapsed() < Duration::from_millis(900), "{:?}", t0.elapsed());
    assert!(a.locked_cache_dir().is_none());
    assert!(c.configure_world(with_cache(DEAD_URL, &d1)).ok, "free after shutdown");
    // Dropping a world releases it too.
    drop(c);
    assert!(a.configure_world(with_cache(DEAD_URL, &d1)).ok, "free after drop");
    // Moving to another folder releases the old one.
    assert!(b.shutdown());
    assert!(a.configure_world(with_cache(DEAD_URL, &d2)).ok);
    let mut e = world();
    assert!(e.configure_world(with_cache(DEAD_URL, &d1)).ok, "released on cache_dir change");
    assert!(!e.configure_world(with_cache(DEAD_URL, &d2)).ok, "d2 held by a");
    assert!(e.is_running(), "a failed configure keeps the previous configuration");
    drop((a, b, e));
    let _ = std::fs::remove_dir_all(&d1);
    let _ = std::fs::remove_dir_all(&d2);
}

#[test]
fn shutdown_makes_the_world_inert_until_configured_again() {
    let mut w = world();
    assert!(w.shutdown(), "shutdown before configure is a no-op");
    assert!(w.configure_world(synthetic(VILNIUS.0, VILNIUS.1)).ok);
    let (held, _) = run_until(&mut w, 10);
    assert!(!held.is_empty());
    assert!(w.shutdown());
    assert!(!w.is_running());
    // Tiles the host holds are reported once, then everything is empty.
    let unloaded: BTreeSet<LayerKey> = w.drain_unload().iter().copied().collect();
    assert_eq!(unloaded, held);
    assert_eq!(w.update_camera([9000.0, 0.0, 0.0], [0.0; 3], 300.0), None);
    assert_eq!(w.update_camera_geodetic(VILNIUS.0, VILNIUS.1, 300.0, [0.0; 3]), Ok(None));
    assert_eq!(w.take_origin_shift(), [0.0; 3]);
    assert_eq!(w.pop_ready(10, |_| panic!("no tiles after shutdown")), 0);
    assert!(w.drain_unload().is_empty() && w.drain_fades().is_empty());
    assert!(w.attributions().is_empty());
    assert!(w.geodetic_to_local(VILNIUS.0, VILNIUS.1, 0.0).is_none());
    assert!(w.local_to_geodetic([0.0; 3]).is_none());
    assert_eq!(w.ground_height_at(0.0, 0.0), 0.0);
    let mut n = 0;
    w.debug_tiles(|_, _, _, _| n += 1);
    assert_eq!(n, 0);
    let s = w.status();
    assert_eq!((s.state, s.message.as_str()), (StatusState::Ok, MSG_SHUT_DOWN));
    assert!(w.shutdown(), "twice is fine");
    // Configure again: running.
    assert!(w.configure_world(synthetic(VILNIUS.0, VILNIUS.1)).ok);
    let (_, again) = run_until(&mut w, 5);
    assert!(again.len() >= 5);
}

#[test]
fn status_reports_a_missing_tile_server_as_offline_once() {
    let mut w = world();
    assert_eq!(w.status().message, MSG_NOT_CONFIGURED);
    assert_eq!(w.take_status_change(), None, "initial state is not a change");
    // Synthetic city: ok.
    assert!(w.configure_world(synthetic(VILNIUS.0, VILNIUS.1)).ok);
    w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
    let s = w.take_status_change().expect("not configured -> ok");
    assert_eq!((s.state, s.message.as_str(), s.failed_fetches), (StatusState::Ok, "", 0));
    // A tile server that does not answer and no disk cache: offline, reported once.
    assert!(w.configure_world(WorldOptions { tile_url: DEAD_URL.into(), ..synthetic(VILNIUS.0, VILNIUS.1) }).ok);
    let t0 = Instant::now();
    let mut changes = Vec::new();
    while t0.elapsed() < Duration::from_secs(10) {
        w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
        w.pop_ready(8, |_| {});
        changes.extend(w.take_status_change());
        if w.status().state == StatusState::Offline && w.status().failed_fetches >= 2 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let s = w.status();
    assert_eq!((s.state, s.message.as_str()), (StatusState::Offline, MSG_OFFLINE), "{s:?}");
    // Tiles v2 (T2b): no chunk is requested before the archive's levels are known, so the
    // failures are the archive opens (one per `cache.open_retry_s`).
    assert!(s.failed_fetches >= 2 && s.last_error.contains("127.0.0.1:9"), "{s:?}");
    assert_eq!(changes.iter().map(|c| c.state).collect::<Vec<_>>(), vec![StatusState::Offline]);
    // Shutdown: back to an idle "ok", picked up by the next status check.
    w.shutdown();
    assert_eq!(w.take_status_change().map(|s| s.message), Some(MSG_SHUT_DOWN.to_string()));
}

/// Records every log line (level, text).
#[derive(Default)]
struct CaptureLog(std::sync::Mutex<Vec<(&'static str, String)>>);

impl crate::Log for CaptureLog {
    fn info(&self, m: &str) {
        self.0.lock().unwrap().push(("info", m.to_string()));
    }
    fn warn(&self, m: &str) {
        self.0.lock().unwrap().push(("warn", m.to_string()));
    }
    fn error(&self, m: &str) {
        self.0.lock().unwrap().push(("error", m.to_string()));
    }
}

impl CaptureLog {
    fn count(&self, level: &str, needle: &str) -> usize {
        self.0.lock().unwrap().iter().filter(|(l, m)| *l == level && m.contains(needle)).count()
    }
}

#[test]
fn disk_cache_is_reused_on_teleport_and_closed_on_shutdown() {
    let d = tmp("cache-reuse");
    let log = Arc::new(CaptureLog::default());
    let mut w = World::new(log.clone(), crate::Diagnostics::default(), "test");
    let db = d.join(crate::fetch::cache_file_name(DEAD_URL));
    let opened = format!("disk cache {} :", db.display());
    // Frames until the fetcher has opened the cache (first fetch) and the info line is drained.
    let pump = |w: &mut World, want: usize| {
        let t0 = Instant::now();
        while log.count("info", &opened) < want && t0.elapsed() < Duration::from_secs(10) {
            w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
            w.pop_ready(8, |_| {});
            std::thread::sleep(Duration::from_millis(5));
        }
        log.count("info", &opened)
    };
    assert!(w.configure_world(with_cache(DEAD_URL, &d)).ok);
    assert_eq!(pump(&mut w, 1), 1);
    assert!(db.exists());
    // Teleport, same source and folder: the open file is kept (no second open).
    let mut o = with_cache(DEAD_URL, &d);
    o.origin_lat += 0.3;
    assert!(w.configure_world(o).ok);
    for _ in 0..40 {
        w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
        w.pop_ready(8, |_| {});
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(log.count("info", &opened), 1, "reused, not reopened");
    // Another cap: closed (bounded wait) and reopened by the new workers, never "in use".
    let o = WorldOptions { profile_overrides: Some(serde_json::json!({"cache": {"cap_mb": 50}})), ..with_cache(DEAD_URL, &d) };
    assert!(w.configure_world(o).ok);
    assert_eq!(pump(&mut w, 2), 2, "reopened after the close");
    assert_eq!(log.count("warn", "in use"), 0);
    assert_eq!(log.count("warn", "still closing"), 0);
    // shutdown waits until the file is closed: another handle can open it right away.
    assert!(w.shutdown());
    let c = crate::fetch::DiskCache::open(db.clone(), 1 << 20, Arc::new(std::sync::atomic::AtomicU64::new(0)))
        .expect("cache file closed by shutdown");
    assert!(c.close(Duration::from_secs(2)));
    drop(c);
    // Dropping a world only signals the close; the file is free shortly after.
    let mut w2 = World::new(log.clone(), crate::Diagnostics::default(), "test");
    assert!(w2.configure_world(with_cache(DEAD_URL, &d)).ok);
    assert_eq!(pump(&mut w2, 3), 3);
    drop(w2);
    let t0 = Instant::now();
    let reopened = loop {
        match crate::fetch::DiskCache::open(db.clone(), 1 << 20, Arc::new(std::sync::atomic::AtomicU64::new(0))) {
            Ok(c) => break Some(c),
            Err(_) if t0.elapsed() < Duration::from_secs(5) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => break None,
        }
    };
    assert!(reopened.is_some_and(|c| c.close(Duration::from_secs(2))), "file not released after drop");
    let _ = std::fs::remove_dir_all(&d);
}

/// `buildings.r0.min_height_m` given as a `configure_world` override changes the delivered R0
/// building meshes (fewer vertices, every vertex of a building >= the minimum), while lights and
/// ground are untouched.
#[test]
fn min_height_override_changes_delivered_meshes() {
    fn collect(over: Option<serde_json::Value>) -> std::collections::BTreeMap<TileKey, (usize, f32, usize, usize)> {
        let mut w = world();
        let r = w.configure_world(WorldOptions { profile_overrides: over, ..synthetic(VILNIUS.0, VILNIUS.1) });
        assert!(r.ok, "{}", r.error);
        let mut out = std::collections::BTreeMap::new();
        let t0 = Instant::now();
        while out.len() < 16 && t0.elapsed() < Duration::from_secs(20) {
            w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
            w.pop_ready(64, |t| {
                if t.ring == 0 {
                    let min_a = t.mesh.colors.iter().map(|c| c[3]).fold(f32::INFINITY, f32::min);
                    out.insert(t.key, (t.mesh.vertex_count(), min_a * 400.0, t.lights.len(), t.ground.vertex_count()));
                }
            });
            let _ = w.drain_fades();
            let _ = w.drain_unload();
            std::thread::sleep(Duration::from_millis(2));
        }
        out
    }
    let base = collect(None);
    let filt = collect(Some(serde_json::json!({ "buildings": { "r0": { "min_height_m": 15.0 } } })));
    eprintln!("R0 tiles: {} / {}", base.len(), filt.len());
    let common: Vec<_> = base.keys().filter(|k| filt.contains_key(k)).copied().collect();
    assert!(common.len() >= 4, "too few R0 tiles: {} / {}", base.len(), filt.len());
    let (mut vb, mut vf) = (0, 0);
    for k in &common {
        let (b, f) = (base[k], filt[k]);
        vb += b.0;
        vf += f.0;
        assert!(f.0 <= b.0, "{k}");
        assert!(f.0 == 0 || f.1 >= 15.0 - 1e-3, "{k}: a building of {} m was meshed", f.1);
        assert_eq!((f.2, f.3), (b.2, b.3), "{k}: lights / ground changed");
    }
    assert!(vf < vb, "the override changed nothing ({vb} vertices)");
    // A typo next to the new key is still an error.
    let mut w = world();
    let bad = serde_json::json!({ "buildings": { "r0": { "min_heigth_m": 15.0 } } });
    assert!(!w.configure_world(WorldOptions { profile_overrides: Some(bad), ..synthetic(VILNIUS.0, VILNIUS.1) }).ok);
    let out_of_range = serde_json::json!({ "buildings": { "r1": { "min_height_m": 501.0 } } });
    assert!(!w.configure_world(WorldOptions { profile_overrides: Some(out_of_range), ..synthetic(VILNIUS.0, VILNIUS.1) }).ok);
}

// ---- Terrain layer (api 1.5) ------------------------------------------------------------

/// A small terrain layer for World-level tests (zooms 13..14, 7 km reach) with real TINs.
fn small_terrain(enabled: bool) -> serde_json::Value {
    serde_json::json!({ "terrain": { "enabled": enabled, "near_zoom": 14, "far_zoom": 13, "bands": [
        { "edge_m": 2500.0, "max_error_m": 0.5, "max_edge_m": 0.0, "max_triangles": 4096 },
        { "edge_m": 7000.0, "max_error_m": 1.5, "max_edge_m": 0.0, "max_triangles": 4096 } ] } })
}

fn terrain_opts(lat: f64, lon: f64, layers: Option<&[&str]>, enabled: bool) -> WorldOptions {
    WorldOptions {
        profile_overrides: Some(small_terrain(enabled)),
        layers: layers.map(|l| l.iter().map(|s| s.to_string()).collect()),
        ..synthetic(lat, lon)
    }
}

/// Host side of one frame in contract order at a static camera (300 m AGL over the origin).
#[derive(Default)]
struct Host {
    held: BTreeSet<LayerKey>,
    /// Every delivery: (key, already held = re-delivery).
    deliveries: Vec<(LayerKey, bool)>,
    unloads: Vec<LayerKey>,
    fades: Vec<LayerKey>,
}

impl Host {
    fn frame(&mut self, w: &mut World) {
        w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
        let (held, deliveries) = (&mut self.held, &mut self.deliveries);
        w.pop_ready(4, |t| {
            let k = t.layer_key();
            assert_eq!(LayerKey::parse(&k.to_string()), Some(k));
            match t.layer {
                crate::rings::LayerKind::Terrain => {
                    assert!(k.to_string().starts_with("t/"), "{k}");
                    assert!(t.ring < 2 && t.key.z == 14 - t.ring as u8, "{k}: ring {}", t.ring);
                    assert!(!t.terrain.is_empty() && t.terrain.normals.is_empty());
                    assert!(t.mesh.is_empty() && t.ground.is_empty() && t.lights.is_empty() && t.street_lights.is_empty());
                    assert!(t.h_min <= t.h_max && t.origin[1] == 0.0);
                }
                crate::rings::LayerKind::World => {
                    assert!(t.terrain.is_empty() && !k.to_string().starts_with("t/"));
                }
                crate::rings::LayerKind::Landmarks => panic!("{k}: no landmark layer requested"),
            }
            deliveries.push((k, !held.insert(k)));
        });
        self.fades.extend(w.drain_fades().iter().map(|f| f.0));
        for k in w.drain_unload() {
            self.held.remove(k);
            self.unloads.push(*k);
        }
    }

    /// `t/` keys seen in deliveries, unloads and fades.
    fn terrain_keys(&self) -> usize {
        let t = |k: &LayerKey| k.layer == crate::rings::LayerKind::Terrain;
        self.deliveries.iter().filter(|(k, _)| t(k)).count()
            + self.unloads.iter().filter(|k| t(k)).count()
            + self.fades.iter().filter(|k| t(k)).count()
    }

    /// Run until the terrain layer is idle (nothing queued or meshing, no terrain fade and no
    /// terrain delivery for 40 frames) or 30 s.
    fn settle_terrain(&mut self, w: &mut World) {
        let t0 = Instant::now();
        let mut quiet = 0;
        while quiet < 40 && t0.elapsed() < Duration::from_secs(30) {
            let (nd, nf) = (self.deliveries.len(), self.fades.len());
            self.frame(w);
            let st = w.stats();
            let busy = st.terrain_queued.iter().chain(&st.terrain_meshing).any(|&n| n > 0)
                || self.deliveries[nd..].iter().any(|(k, _)| k.layer == crate::rings::LayerKind::Terrain)
                || self.fades[nf..].iter().any(|k| k.layer == crate::rings::LayerKind::Terrain);
            quiet = if busy { 0 } else { quiet + 1 };
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(quiet >= 40, "terrain layer did not settle");
    }
}

/// Opt-in: without the host layer (or with the profile off) no `t/` key appears anywhere; with
/// both, terrain tiles stream with `t/` keys next to the vector world, and a teleport unloads
/// both layers.
#[test]
fn terrain_layer_needs_the_opt_in_and_teleport_unloads_both_layers() {
    for (layers, enabled) in [(None, true), (Some(&["world", "terrain"][..]), false), (Some(&["world"][..]), true)] {
        let mut w = world();
        assert!(w.configure_world(terrain_opts(VILNIUS.0, VILNIUS.1, layers, enabled)).ok);
        let mut h = Host::default();
        for _ in 0..150 {
            h.frame(&mut w);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!h.deliveries.is_empty(), "the vector world streams");
        assert_eq!(h.terrain_keys(), 0, "{layers:?} / enabled {enabled}: a t/ key reached the host");
        let mut n = 0;
        w.debug_tiles(|k, _, _, _| n += (k.layer == crate::rings::LayerKind::Terrain) as usize);
        assert_eq!(n, 0);
        assert!(!w.terrain_layer_active() && w.stats().terrain_rings == 0 && w.terrain_cpu().is_none());
    }
    let mut w = world();
    assert!(w.configure_world(terrain_opts(VILNIUS.0, VILNIUS.1, Some(&["world", "terrain"]), true)).ok);
    assert!(w.terrain_layer_active());
    let mut h = Host::default();
    let t0 = Instant::now();
    // The vector world's first tiles wait for the generated overview tile (T2c, ~1-2 s in debug).
    while (h.held.iter().filter(|k| k.layer == crate::rings::LayerKind::Terrain).count() < 20
        || !h.held.iter().any(|k| k.layer == crate::rings::LayerKind::World))
        && t0.elapsed() < Duration::from_secs(30)
    {
        h.frame(&mut w);
        std::thread::sleep(Duration::from_millis(2));
    }
    let held_terrain = h.held.iter().filter(|k| k.layer == crate::rings::LayerKind::Terrain).count();
    assert!(held_terrain >= 20, "only {held_terrain} terrain tiles");
    assert!(h.held.iter().any(|k| k.layer == crate::rings::LayerKind::World));
    let st = w.stats();
    assert_eq!(st.terrain_rings, 2);
    assert!(st.terrain_loaded[..2].iter().sum::<i32>() as usize == held_terrain && st.terrain_tris > 0);
    let mut dbg = 0;
    w.debug_tiles(|k, ring, _, _| {
        if k.layer == crate::rings::LayerKind::Terrain {
            dbg += 1;
            assert!(ring < 2);
        }
    });
    assert!(dbg >= held_terrain);
    let cpu = w.terrain_cpu().unwrap();
    assert!(cpu.n[..2].iter().sum::<u32>() as usize >= held_terrain);
    // Teleport: every key of both layers comes back once.
    let held = h.held.clone();
    assert!(w.configure_world(terrain_opts(54.8985, 23.9036, Some(&["world", "terrain"]), true)).ok);
    let unloaded: BTreeSet<LayerKey> = w.drain_unload().iter().copied().collect();
    assert_eq!(unloaded, held, "teleport unloads both layers");
    assert!(w.drain_unload().is_empty());
}

/// api 1.6: `terrain_center` keeps the relief (and its flat zone) where it was across a
/// teleport; without it the flat zone follows the new origin. `status().terrain_active`.
#[test]
fn terrain_center_keeps_the_relief_across_a_reconfigure_and_status_reports_the_layer() {
    let layers = Some(&["world", "terrain"][..]);
    let height_at = |w: &World, lat: f64, lon: f64| {
        let p = w.geodetic_to_local(lat, lon, 0.0).unwrap();
        w.ground_height_at(p[0], p[2])
    };
    let (a, b) = ((VILNIUS.0, VILNIUS.1), (VILNIUS.0 + 0.045, VILNIUS.1 + 0.07)); // ~5 km / 4.5 km away
    let mut w = world();
    assert!(!w.status().terrain_active, "not configured");
    assert!(w.configure_world(terrain_opts(a.0, a.1, layers, true)).ok);
    assert!(w.status().terrain_active);
    let probe = [(a.0, a.1), (a.0 + 0.004, a.1 + 0.003), (b.0, b.1)];
    let first: Vec<f64> = probe.iter().map(|p| height_at(&w, p.0, p.1)).collect();
    // Same centre, new origin: every probe height is the same (up to the local-frame round trip).
    let mut o = terrain_opts(b.0, b.1, layers, true);
    o.terrain_center = Some([a.0, a.1]);
    assert!(w.configure_world(o).ok);
    for (p, h) in probe.iter().zip(&first) {
        assert!((height_at(&w, p.0, p.1) - h).abs() < 1e-6, "height at {p:?} moved");
    }
    // No centre: the flat zone moves to the new origin, so a spot that was inside the old flat
    // zone (probe 1, ~550 m from its centre) gets relief.
    assert!(w.configure_world(terrain_opts(b.0, b.1, layers, true)).ok);
    assert!((height_at(&w, probe[1].0, probe[1].1) - first[1]).abs() > 1.0, "the flat zone should have moved");
    // Out of range or not requested.
    let mut bad = terrain_opts(a.0, a.1, layers, true);
    bad.terrain_center = Some([91.0, 0.0]);
    let r = w.configure_world(bad);
    assert!(!r.ok && r.error.contains("terrain_center"), "{}", r.error);
    assert!(w.status().terrain_active, "a rejected configure changes nothing");
    assert!(w.configure_world(terrain_opts(a.0, a.1, Some(&["world"]), true)).ok);
    assert!(!w.status().terrain_active, "layer not requested: flat");
    w.shutdown();
    assert!(!w.status().terrain_active);
}

/// `set_terrain_detail`: clamped to [0.25, detail_scale_max]; every held terrain key is
/// re-delivered exactly once (same keys, nothing unloaded), the vector world is untouched.
#[test]
fn set_terrain_detail_redelivers_each_held_terrain_key_once() {
    let mut w = world();
    assert_eq!(w.set_terrain_detail(0.1), 0.25);
    assert_eq!(w.set_terrain_detail(10.0), 2.0, "the default profile's detail_scale_max");
    assert_eq!(w.set_terrain_detail(f64::NAN), 2.0, "ignored");
    assert_eq!(w.set_terrain_detail(1.0), 1.0);
    assert!(w.configure_world(terrain_opts(VILNIUS.0, VILNIUS.1, Some(&["world", "terrain"]), true)).ok);
    assert_eq!(w.set_terrain_detail(5.0), 2.0, "terrain.detail_scale_max");
    assert_eq!(w.set_terrain_detail(1.0), 1.0);
    let mut h = Host::default();
    h.settle_terrain(&mut w);
    let held: BTreeSet<LayerKey> = h.held.iter().filter(|k| k.layer == crate::rings::LayerKind::Terrain).copied().collect();
    assert!(held.len() >= 20);
    let (nd, nu) = (h.deliveries.len(), h.unloads.len());
    assert_eq!(w.set_terrain_detail(2.0), 2.0);
    h.settle_terrain(&mut w);
    let re: Vec<(LayerKey, bool)> =
        h.deliveries[nd..].iter().filter(|(k, _)| k.layer == crate::rings::LayerKind::Terrain).copied().collect();
    assert!(re.iter().all(|(_, again)| *again), "only re-deliveries");
    let keys: BTreeSet<LayerKey> = re.iter().map(|r| r.0).collect();
    assert_eq!(keys, held, "every held terrain key");
    assert_eq!(re.len(), held.len(), "each exactly once");
    assert!(h.unloads[nu..].iter().all(|k| k.layer == crate::rings::LayerKind::World), "no terrain unloads");
    assert!(h.deliveries[nd..].iter().filter(|(k, _)| k.layer == crate::rings::LayerKind::World).all(|(_, again)| !*again));
    assert_eq!(w.terrain_detail(), 2.0);
}

#[test]
fn cross_layer_delivery_order() {
    use crate::world::terrain_first;
    assert!(!terrain_first(None, None) && !terrain_first(Some((false, 1.0)), None));
    assert!(terrain_first(None, Some((true, 9e9))));
    // Fresh before re-deliveries, whatever the distance.
    assert!(terrain_first(Some((true, 10.0)), Some((false, 1e6))));
    assert!(!terrain_first(Some((false, 1e6)), Some((true, 10.0))));
    // Then strictly by distance from the camera (no layer weight): 999 m terrain beats 1000 m
    // world, 1001 m does not.
    assert!(terrain_first(Some((false, 1000.0)), Some((false, 999.0))));
    assert!(!terrain_first(Some((false, 1000.0)), Some((false, 1001.0))));
    assert!(!terrain_first(Some((false, 1000.0)), Some((false, 1000.0))), "ties go to the vector world");
}

// ---- First-fill delivery probe ------------------------------------------------------------

/// One first-fill run (static camera at local 0, 300 m AGL, 2 deliveries per frame, ~60 fps
/// wall clock) with the vector world and, optionally, the full default terrain layer.
struct FillProbe {
    /// Every fresh delivery: (frame, key, horizontal distance of the tile from the camera).
    deliveries: Vec<(usize, LayerKey, f64)>,
    /// First frame each key appeared in `poll_fades` with a value < 1 (fading in or out).
    faded: std::collections::BTreeMap<LayerKey, (usize, f32)>,
    /// (frame, key) of the first delivery of the tile under the camera, per layer and zoom.
    under: Vec<(usize, LayerKey)>,
    frames: usize,
}

fn fill_probe(terrain: bool, frames: usize) -> FillProbe {
    let mut w = world();
    let opts = if terrain {
        WorldOptions {
            profile_overrides: Some(serde_json::json!({ "terrain": { "enabled": true } })),
            layers: Some(vec!["world".into(), "terrain".into()]),
            ..synthetic(VILNIUS.0, VILNIUS.1)
        }
    } else {
        synthetic(VILNIUS.0, VILNIUS.1)
    };
    assert!(w.configure_world(opts).ok);
    let mut p = FillProbe { deliveries: Vec::new(), faded: Default::default(), under: Vec::new(), frames };
    let half_w = |z: u8| 40_075_016.7 * VILNIUS.0.to_radians().cos() / f64::from(1u32 << z) / 2.0;
    for f in 0..frames {
        let t0 = Instant::now();
        w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
        let (dl, under) = (&mut p.deliveries, &mut p.under);
        w.pop_ready(2, |t| {
            if t.layer == crate::rings::LayerKind::Terrain && t.terrain.is_empty() {
                return;
            }
            let k = t.layer_key();
            let d = t.origin[0].hypot(t.origin[2]);
            if !dl.iter().any(|x| x.1 == k) {
                dl.push((f, k, d));
                let hw = half_w(t.key.z);
                if t.origin[0].abs() <= hw && t.origin[2].abs() <= hw {
                    under.push((f, k));
                }
            }
        });
        for &(k, v) in w.drain_fades() {
            if v < 1.0 {
                p.faded.entry(k).or_insert((f, v));
            }
        }
        let _ = w.drain_unload();
        std::thread::sleep(Duration::from_millis(16).saturating_sub(t0.elapsed()));
    }
    p
}

impl FillProbe {
    fn first(&self, layer: crate::rings::LayerKind, z: u8) -> Option<usize> {
        self.deliveries.iter().find(|d| d.1.layer == layer && d.1.key.z == z).map(|d| d.0)
    }
    fn under_at(&self, layer: crate::rings::LayerKind, z: u8) -> Option<usize> {
        self.under.iter().find(|d| d.1.layer == layer && d.1.key.z == z).map(|d| d.0)
    }
    /// Longest run of frames without a terrain delivery between the first terrain delivery and
    /// the 60th one.
    fn terrain_gap(&self) -> usize {
        let t: Vec<usize> =
            self.deliveries.iter().filter(|d| d.1.layer == crate::rings::LayerKind::Terrain).map(|d| d.0).take(60).collect();
        t.windows(2).map(|w| w[1] - w[0]).max().unwrap_or(0)
    }
    fn summary(&self) -> String {
        use crate::rings::LayerKind::{Terrain, World};
        let mut s = String::new();
        for z in 13..=16 {
            s += &format!("world z{z}: first {:?} under-cam {:?}\n", self.first(World, z), self.under_at(World, z));
        }
        for z in 10..=14 {
            s += &format!("terrain z{z}: first {:?} under-cam {:?}\n", self.first(Terrain, z), self.under_at(Terrain, z));
        }
        let fresh_faded_in = self
            .deliveries
            .iter()
            .filter(|d| self.faded.get(&d.1).is_some_and(|&(f, v)| f <= d.0 + 1 && v == 0.0))
            .count();
        s += &format!(
            "deliveries {} in {} frames, terrain max gap (first 60) {} frames, fresh tiles that faded in from 0: {}\n",
            self.deliveries.len(),
            self.frames,
            self.terrain_gap(),
            fresh_faded_in
        );
        for d in self.deliveries.iter().filter(|d| std::env::var("PROBE_TERRAIN").is_err() || d.1.layer == crate::rings::LayerKind::Terrain).take(48) {
            s += &format!("  f{:>3} {:<16} {:>7.0} m\n", d.0, d.1.to_string(), d.2);
        }
        s
    }
}

/// Probe for the delivery order at a first fill (prints only): `cargo test -p simworld-core
/// fill_probe_print -- --ignored --nocapture`.
#[test]
#[ignore]
fn fill_probe_print() {
    println!("== vector only ==\n{}", fill_probe(false, 240).summary());
    println!("== vector + terrain ==\n{}", fill_probe(true, 360).summary());
}

/// Cover churn of one run: deliveries, unloads, keys delivered again after an unload (`again`),
/// of those within 180 frames (`soon`), how many of the re-deliveries were coarsen swaps (the
/// 4 children unloaded within 90 frames after), and examples of the others.
#[derive(Debug)]
struct Churn {
    deliveries: usize,
    unloads: usize,
    again: usize,
    soon: usize,
    coarsen_swaps: usize,
    other: Vec<String>,
}

fn churn_probe(terrain: bool, frames: usize, cam: impl Fn(usize) -> ([f64; 3], [f64; 3], f64)) -> Churn {
    let mut w = world();
    let opts = WorldOptions {
        profile_overrides: terrain.then(|| serde_json::json!({ "terrain": { "enabled": true } })),
        layers: terrain.then(|| vec!["world".into(), "terrain".into()]),
        ..synthetic(VILNIUS.0, VILNIUS.1)
    };
    assert!(w.configure_world(opts).ok);
    let mut unloads: Vec<(usize, LayerKey)> = Vec::new();
    let mut last_unload: std::collections::HashMap<LayerKey, usize> = Default::default();
    let mut again: Vec<(usize, usize, LayerKey, usize, [f64; 2], f64)> = Vec::new();
    let mut c = Churn { deliveries: 0, unloads: 0, again: 0, soon: 0, coarsen_swaps: 0, other: Vec::new() };
    let mut shift = [0.0f64; 3];
    for f in 0..frames {
        let t0 = Instant::now();
        let (p, v, agl) = cam(f);
        let local = [p[0] - shift[0], p[1], p[2] - shift[2]];
        if let Some(s) = w.update_camera(local, v, agl) {
            shift = [shift[0] + s[0], 0.0, shift[2] + s[2]];
        }
        let mut got = Vec::new();
        w.pop_ready(2, |t| got.push((t.layer_key(), t.ring)));
        for (k, ring) in got {
            c.deliveries += 1;
            if let Some(&u) = last_unload.get(&k) {
                again.push((u, f, k, ring, [p[0].round(), p[2].round()], agl));
            }
        }
        let _ = w.drain_fades();
        for k in w.drain_unload() {
            c.unloads += 1;
            last_unload.insert(*k, f);
            unloads.push((f, *k));
        }
        std::thread::sleep(Duration::from_millis(16).saturating_sub(t0.elapsed()));
    }
    for (u, f, k, ring, p, agl) in again {
        c.again += 1;
        if f - u <= 180 {
            c.soon += 1;
        }
        let kids = crate::rings::children(k.key);
        let swap = kids.iter().all(|ch| unloads.iter().any(|&(uf, uk)| uk.layer == k.layer && uk.key == *ch && uf >= f && uf <= f + 90));
        if swap {
            c.coarsen_swaps += 1;
        } else if c.other.len() < 12 {
            c.other.push(format!("{k} ring {ring}: unloaded f{u}, delivered again f{f} (cam {p:?} agl {agl:.0})"));
        }
    }
    c
}

/// Churn probe (prints only): `cargo test -p simworld-core churn_probe_print -- --ignored --nocapture`.
#[test]
#[ignore]
fn churn_probe_print() {
    for terrain in [false, true] {
        let r = churn_probe(terrain, 900, |_| ([0.0, 300.0, 0.0], [0.0; 3], 300.0));
        println!("terrain {terrain} static 300 m: {r:#?}");
        for speed in [80.0, 250.0] {
            let r = churn_probe(terrain, 900, |f| ([0.0, 300.0, -speed * f as f64 / 60.0], [0.0, 0.0, -speed], 300.0));
            println!("terrain {terrain} level north {speed} m/s: {r:#?}");
            let r = churn_probe(terrain, 900, |f| {
                let t = f as f64 / 60.0;
                let agl = (300.0 + 180.0 * t).min(3000.0);
                ([0.0, agl, -speed * t], [0.0, if agl < 3000.0 { 180.0 } else { 0.0 }, -speed], agl)
            });
            println!("terrain {terrain} north {speed} m/s climbing: {r:#?}");
        }
    }
}

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
                assert_eq!(mesh, want.mesh, "{k}: delivered mesh differs from a fresh build");
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
