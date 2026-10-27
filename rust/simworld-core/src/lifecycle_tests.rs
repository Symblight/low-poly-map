//! Host API and lifecycle (api 0.7): geodetic input and conversions, origin shift reporting,
//! status, shutdown, the cache folder lock, several worlds side by side and teleport.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::geo::TileKey;
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
fn run_until(w: &mut World, want: usize) -> (BTreeSet<TileKey>, Vec<(TileKey, [f64; 3])>) {
    let mut held = BTreeSet::new();
    let mut delivered = Vec::new();
    let t0 = Instant::now();
    while delivered.len() < want && t0.elapsed() < Duration::from_secs(20) {
        w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
        w.pop_ready(64, |t| {
            held.insert(t.key);
            delivered.push((t.key, t.origin));
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
    let unloaded: BTreeSet<TileKey> = w.drain_unload().iter().copied().collect();
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
    let unloaded: BTreeSet<TileKey> = w.drain_unload().iter().copied().collect();
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
        if w.status().state == StatusState::Offline && w.status().failed_fetches >= 8 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let s = w.status();
    assert_eq!((s.state, s.message.as_str()), (StatusState::Offline, MSG_OFFLINE), "{s:?}");
    assert!(s.failed_fetches >= 8 && s.last_error.contains("127.0.0.1:9"), "{s:?}");
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
