//! Tests of the `lod-terrain-v2` review fixes on a whole `World` (H1 offline levels, M1 tile
//! budget, M3 data tile idle eviction) and the ignored real-archive profiles (M2 / M5), on the
//! in-process archive server of [`crate::data_world_tests`].

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::data_world_tests::{archive, mesh_hash, opts, serve, settle, world, VILNIUS};
use crate::geo::TileKey;
use crate::rings::LayerKind;
use crate::world::{World, WorldOptions};

/// Review H1: a disk cache written before api 1.11 has no `levels` row. Offline, the levels are
/// inferred from its cached tiles (compatibility archive), so the cached tiles render and the
/// status says `degraded` / cached tiles only instead of waiting forever for the archive.
#[test]
fn offline_start_on_a_cache_without_the_levels_row_renders_cached_tiles() {
    let (url, _) = serve(archive());
    let dir = std::env::temp_dir().join(format!("simworld-h1-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let with_cache = |u: &str| WorldOptions { cache_dir: dir.to_string_lossy().into_owned(), ..opts(u) };
    let mut a = world();
    assert!(a.configure_world(with_cache(&url)).ok);
    let (warm, _) = settle(&mut a);
    assert!(a.shutdown());
    drop(a);
    // Make it a pre-1.11 cache (no `levels` row) under an unreachable URL.
    let dead = "http://127.0.0.1:9/t.pmtiles";
    let db = dir.join(crate::fetch::cache_file_name(dead));
    std::fs::rename(dir.join(crate::fetch::cache_file_name(&url)), &db).unwrap();
    {
        let c = rusqlite::Connection::open(&db).unwrap();
        assert_eq!(c.execute("DELETE FROM meta WHERE k = 'levels'", []).unwrap(), 1, "the 1.11 cache recorded its levels");
    }
    let mut b = world();
    assert!(b.configure_world(with_cache(dead)).ok);
    let under = crate::geo::tile_for(VILNIUS.0, VILNIUS.1, 16);
    let mut got = BTreeMap::new();
    let t0 = Instant::now();
    while !got.contains_key(&under) {
        assert!(t0.elapsed() < Duration::from_secs(30), "no cached tile rendered offline: {:?} / {:?}", b.status(), b.stats());
        b.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
        b.pop_ready(64, |t| {
            if t.layer == LayerKind::World {
                got.insert(t.key, (mesh_hash(t.mesh), mesh_hash(t.ground)));
            }
        });
        let _ = b.drain_fades();
        let _ = b.drain_unload();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(got.get(&under), warm.get(&under), "the offline tile equals the online one");
    let st = b.status();
    assert_eq!((st.state, st.message.as_str()), (crate::status::StatusState::Degraded, crate::status::MSG_CACHED_ONLY));
    assert!(b.shutdown());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Review M1: a narrow field of view no longer multiplies the cover. Synthetic city at 300 m,
/// settled per view: fov 60 is the default cover (uncapped), fov 15 stays within
/// `lod.max_tiles` (+ the estimate's slack) and keeps R3; with the cap off it is ~4x.
#[test]
fn narrow_fov_cover_stays_within_the_tile_budget() {
    let run = |fov: f64, max_tiles: Option<u32>| {
        let mut w = world();
        let mut o = opts("");
        if let Some(m) = max_tiles {
            o.profile_overrides = Some(serde_json::json!({ "lod": { "max_tiles": m } }));
        }
        assert!(w.configure_world(o).ok);
        assert!(w.set_view(fov, 1080.0, [0.0, 0.0, -1.0]));
        settle(&mut w);
        let s = w.stats();
        let est: f64 = w.lod_tiles_estimate().iter().sum();
        (s.loaded, est, w.tau_eff())
    };
    let (def, def_est, def_tau) = run(60.0, None);
    let (narrow, narrow_est, narrow_tau) = run(15.0, None);
    let sum = |l: [i32; 4]| l.iter().sum::<i32>();
    eprintln!("fov 60: {def:?} = {} (est {def_est:.0}, tau {def_tau}); fov 15 capped: {narrow:?} = {} (est {narrow_est:.0}, tau {narrow_tau:.2})", sum(def), sum(narrow));
    assert_eq!(def_tau, 4.0, "the default view is not capped");
    assert!(narrow_tau > 4.0);
    assert!(sum(narrow) <= 1100, "capped cover {narrow:?}");
    assert!(narrow[3] > 0, "R3 kept: {narrow:?}");
    if std::env::var("SIMWORLD_M1_UNCAPPED").is_ok() {
        let (raw, raw_est, _) = run(15.0, Some(0));
        eprintln!("fov 15 uncapped: {raw:?} = {} (est {raw_est:.0})", sum(raw));
    }
}

/// Run frames with the camera 300 m over `north_km` north of the fixture origin until the vector
/// layer is idle (or `max` elapses). Geodetic, so origin shifts do not move the target.
fn settle_at(w: &mut World, north_km: f64, max: Duration) {
    let t0 = Instant::now();
    let mut n = 0;
    loop {
        let cam = w.geodetic_to_local(VILNIUS.0 + north_km / 111.32, VILNIUS.1, 300.0).expect("configured");
        w.update_camera(cam, [0.0; 3], 300.0);
        n += w.pop_ready(64, |_| {});
        let _ = w.drain_fades();
        let _ = w.drain_unload();
        let s = w.stats();
        let busy: i32 = s.queued.iter().chain(s.fetching.iter()).chain(s.meshing.iter()).sum();
        if (n > 0 && busy == 0) || t0.elapsed() > max {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Review M3: decoded data tiles that no tile of the cover maps to are dropped after
/// `memory.data_idle_s` (not kept until the 512 MB budget fills), the cover's own tiles stay, and
/// the counters show evictions and re-decodes. Synthetic two-level city, camera moved 60 km
/// away and back.
#[test]
fn unneeded_data_tiles_are_dropped_after_the_idle_time() {
    let run = |idle: f64| {
        let mut w = world();
        let mut o = opts("");
        o.profile_overrides = Some(serde_json::json!({ "memory": { "data_idle_s": idle } }));
        assert!(w.configure_world(o).ok);
        settle_at(&mut w, 0.0, Duration::from_secs(60));
        let home = w.stats();
        assert!(home.data_tiles_loaded > 0 && home.data_tiles_evicted_idle == 0, "{home:?}");
        // Fly away (re-centering moves the origin; the old area leaves the cover).
        for i in 1..=12 {
            settle_at(&mut w, 5.0 * i as f64, Duration::from_millis(200));
        }
        settle_at(&mut w, 60.0, Duration::from_secs(60));
        let away = w.stats();
        (w, home, away)
    };
    // Idle 0: the home tiles are gone once the cover moved; the pool holds the new cover only.
    let (mut w, home, away) = run(0.0);
    eprintln!(
        "idle 0: home {} resident; away {} resident, {} idle evictions, {} budget evictions, {} decodes",
        home.data_tiles_loaded, away.data_tiles_loaded, away.data_tiles_evicted_idle, away.data_tiles_evicted_budget, away.data_tiles_decoded
    );
    assert!(away.data_tiles_evicted_idle >= home.data_tiles_loaded as u64, "{away:?}");
    assert_eq!(away.data_tiles_evicted_budget, 0);
    assert!(away.data_tiles_loaded as u64 <= away.data_tiles_decoded - away.data_tiles_evicted_idle);
    let pool = away.memory.pool(crate::budget::Pool::DataTiles);
    assert_eq!(pool.items, away.data_tiles_loaded, "budget pool = resident tiles");
    // Back home (geodetic: the origin moved): the home tiles are decoded again and counted.
    settle_at(&mut w, 0.0, Duration::from_secs(60));
    let back = w.stats();
    assert!(back.data_tiles_redecoded > 0, "{back:?}");
    // The default idle time keeps them for a while: nothing dropped during the short flight.
    let (_, _, away20) = run(20.0);
    assert_eq!(away20.data_tiles_evicted_idle, 0, "{away20:?}");
}

/// Review M5 profile (ignored; `SIMWORLD_V2_ARCHIVE=<vilnius-v2.pmtiles> cargo test --release -p
/// simworld-core real_overview_chunk_profile -- --ignored --nocapture`): the densest overview
/// tile (11/1167/650 of the Vilnius build): decode time, then every R3 (z13) and R2 (z14) chunk
/// cut from it, with the stages of the slowest chunk timed one by one.
#[test]
#[ignore]
fn real_overview_chunk_profile() {
    let Ok(v2) = std::env::var("SIMWORLD_V2_ARCHIVE") else {
        eprintln!("SIMWORLD_V2_ARCHIVE not set; skipped");
        return;
    };
    let (url, _) = serve(std::fs::read(&v2).expect("v2 archive"));
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let f = crate::fetch::Fetcher::new(&url, "", Arc::new(AtomicU64::new(0)), crate::fetch::FetchParams::default(), Box::new(|_| {})).unwrap();
    let key = TileKey::new(11, 1167, 650);
    let (bytes, _) = rt.block_on(f.fetch(key)).expect("fetch");
    let profile = crate::profile::test_default();
    let frame = crate::geo::Frame::new(VILNIUS.0, VILNIUS.1);
    let terrain = crate::terrain::Terrain::flat();
    let t = Instant::now();
    let dt = crate::data::decode_overview_tile(key, frame, &terrain, &bytes, profile).unwrap();
    eprintln!(
        "{key}: {} KB, decode {:.1} ms: {} blocks, {} lights, {} landmarks, {} areas, {} roads",
        bytes.len() / 1024,
        t.elapsed().as_secs_f64() * 1e3,
        dt.buildings.len(),
        dt.lights.len(),
        dt.landmarks.len(),
        dt.ground.areas.len(),
        dt.ground.roads.len()
    );
    let mut worst = (0.0, key, 0);
    for (z, ring) in [(13u8, 3usize), (14, 2)] {
        let n = 1u32 << (z - 11);
        let (mut sum, mut max) = (0.0f64, 0.0f64);
        for dy in 0..n {
            for dx in 0..n {
                let c = TileKey::new(z, key.x * n + dx, key.y * n + dy);
                let t = Instant::now();
                crate::data::build_chunk_with(&dt, Some(&dt), c, ring, profile, &|| false).unwrap();
                let ms = t.elapsed().as_secs_f64() * 1e3;
                sum += ms;
                max = max.max(ms);
                if ms > worst.0 {
                    worst = (ms, c, ring);
                }
            }
        }
        eprintln!("z{z} chunks (R{ring}): {} chunks, total {sum:.0} ms, mean {:.1} ms, max {max:.1} ms", n * n, sum / (n * n) as f64);
    }
    let (_, c, ring) = worst;
    let proj = dt.proj.cut(c).unwrap();
    let time = |name: &str, f: &mut dyn FnMut()| {
        let t = Instant::now();
        f();
        eprintln!("  {c} {name}: {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
    };
    time("mesh_ground", &mut || {
        crate::ground::mesh_ground(&dt.ground, &proj, ring, profile);
    });
    time("light_points_in (all lights)", &mut || {
        crate::lights::light_points_in(dt.lights.iter(), &proj, profile);
    });
    time("street lamps", &mut || {
        let mut l = crate::lights::LightSet::default();
        l.set_streets(&dt.ground.roads, &proj, profile);
    });
    time("max height over lights", &mut || {
        let _ = dt.lights.iter().filter(|b| proj.place(b).is_some()).map(|b| crate::mesher::resolve(b, profile).height).fold(0.0f32, f32::max);
    });
    time("whole chunk", &mut || {
        crate::data::build_chunk_with(&dt, Some(&dt), c, ring, profile, &|| false).unwrap();
    });
}

/// Review M2 / M5 (ignored; same env as `real_archive_flight_two_level_vs_compat`): cold fill at
/// 300 m over the Vilnius origin on both archives, no disk cache, frames at ~60 Hz until the
/// cover is idle: the time to settle, the first-delivery latency table (new area) and the worst
/// chunk build time per band.
#[test]
#[ignore]
fn real_archive_cold_fill() {
    let (Ok(v2), Ok(compat)) = (std::env::var("SIMWORLD_V2_ARCHIVE"), std::env::var("SIMWORLD_COMPAT_ARCHIVE")) else {
        eprintln!("SIMWORLD_V2_ARCHIVE / SIMWORLD_COMPAT_ARCHIVE not set; skipped");
        return;
    };
    for (name, path) in [("two-level", v2), ("compat", compat)] {
        let (url, requests) = serve(std::fs::read(&path).expect("archive"));
        let mut w = world();
        // `SIMWORLD_COLD_MESH=<n>`: try another `streaming.max_mesh_per_band`.
        let ov = std::env::var("SIMWORLD_COLD_MESH").ok().and_then(|v| v.parse::<u32>().ok());
        let profile_overrides = ov.map(|n| serde_json::json!({ "streaming": { "max_mesh_per_band": n } }));
        assert!(w.configure_world(WorldOptions { layers: Some(vec!["world".into(), "landmarks".into()]), profile_overrides, ..opts(&url) }).ok);
        let t0 = Instant::now();
        let mut first_at = [None::<f64>; 4];
        loop {
            w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
            let el = t0.elapsed().as_secs_f64() * 1e3;
            w.pop_ready(64, |t| {
                if t.layer == LayerKind::World {
                    let r = crate::rings::ring_for_zoom(t.key.z).unwrap();
                    first_at[r].get_or_insert(el);
                }
            });
            let _ = w.drain_fades();
            let _ = w.drain_unload();
            let s = w.stats();
            let busy: i32 = s.queued.iter().chain(s.fetching.iter()).chain(s.meshing.iter()).sum();
            if (busy == 0 && s.loaded.iter().sum::<i32>() > 0) || t0.elapsed() > Duration::from_secs(60) {
                break;
            }
            std::thread::sleep(Duration::from_millis(16));
        }
        let s = w.stats();
        eprintln!(
            "{name}: settled in {:.0} ms, {} requests, loaded {:?}, data tiles {} ({} overview); first tile per band (ms) {:?}\n{}",
            t0.elapsed().as_secs_f64() * 1e3,
            requests.load(Ordering::Relaxed),
            s.loaded,
            s.data_tiles_fetched,
            s.data_tiles_fetched_overview,
            first_at.map(|v| v.map(|x| x.round())),
            w.latency().map(|l| l.report()).unwrap_or_default()
        );
        assert!(w.shutdown());
    }
}
