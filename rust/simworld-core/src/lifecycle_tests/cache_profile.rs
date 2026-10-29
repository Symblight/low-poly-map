//! The disk cache across teleports and shutdown, and profile overrides through `configure_world`.

use super::*;

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
