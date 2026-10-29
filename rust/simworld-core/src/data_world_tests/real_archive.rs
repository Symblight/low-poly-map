//! Real archives (ignored): `data/out/vilnius-v2.pmtiles` against `data/out/vilnius.pmtiles`,
//! and the data levels in the disk cache.

use super::*;

/// Roof area per building seed (COLOR g) of a mesh's upward triangles.
fn roofs(m: &crate::mesher::MeshData, out: &mut BTreeMap<u32, f64>) {
    for t in m.indices.chunks(3) {
        let [a, b, c] = [t[0] as usize, t[1] as usize, t[2] as usize];
        if m.normals[a][1] < 0.99 {
            continue;
        }
        let (pa, pb, pc) = (m.positions[a], m.positions[b], m.positions[c]);
        *out.entry(m.colors[a][1].to_bits()).or_default() += 0.5 * (((pb[0] - pa[0]) * (pc[2] - pa[2]) - (pc[0] - pa[0]) * (pb[2] - pa[2])) as f64).abs();
    }
}

/// One fixed flight through a served archive: 10 km south of the Vilnius origin to 10 km north,
/// 250 m/s, 300 m AGL, 60 frames per second in real time, no disk cache. Checks the host's
/// visibility rule every frame on the vector keys (no overlap, no hole under a present root;
/// `GodotSim`), every landmark tile at most once while held. Returns (frames whose camera point
/// had no visible tile after the first 5 s, report, landmark tiles delivered).
fn real_flight(url: &str, layers: Vec<String>, per_tile: bool) -> (u64, String, usize) {
    use crate::scheduler::sim::GodotSim;
    let mut w = world();
    w.set_per_tile_archive(per_tile);
    let r = w.configure_world(WorldOptions { layers: Some(layers), ..opts(url) });
    assert!(r.ok, "{}", r.error);
    let mut g = GodotSim::default();
    let mut lm_held = std::collections::BTreeSet::new();
    let (mut lm_delivered, mut errors, mut misses) = (0usize, 0usize, 0u64);
    let speed = 250.0;
    let frames = (20_000.0 / speed * 60.0) as usize;
    let mut z = 10_000.0f64; // local z = -north: start 10 km south
    let t0 = Instant::now();
    for f in 0..frames + 120 {
        let ft = Instant::now();
        let moving = f < frames;
        if moving {
            z -= speed / 60.0;
        }
        if let Some(s) = w.update_camera([0.0, 300.0, z], [0.0, 0.0, if moving { -speed } else { 0.0 }], 300.0) {
            z -= s[2];
        }
        w.pop_ready(4, |t| match t.layer {
            LayerKind::Landmarks => {
                assert!(lm_held.insert(t.key), "landmark tile {} delivered twice while held", t.key);
                lm_delivered += 1;
            }
            _ => g.upload(t.key),
        });
        let fades: Vec<(TileKey, f32)> = w.drain_fades().iter().filter(|(k, _)| k.layer == LayerKind::World).map(|(k, v)| (k.key, *v)).collect();
        g.set_fades(&fades);
        for k in w.drain_unload().to_vec() {
            match k.layer {
                LayerKind::Landmarks => assert!(lm_held.remove(&k.key), "unload of a landmark tile not held"),
                _ => g.unload(k.key),
            }
        }
        let errs = g.check();
        if !errs.is_empty() {
            errors += 1;
            if errors < 5 {
                eprintln!("frame {f}: {:?}", &errs[..errs.len().min(3)]);
            }
        }
        if t0.elapsed() > Duration::from_secs(5) {
            let geo = w.local_to_geodetic([0.0, 300.0, z]).unwrap();
            let v = crate::rings::ViewPoint::new(geo[0], geo[1], 300.0);
            if g.tile_at(v.wx, v.wy).is_empty() {
                misses += 1;
            }
        }
        std::thread::sleep(Duration::from_millis(16).saturating_sub(ft.elapsed()));
    }
    assert_eq!(errors, 0, "visibility rule broken on {errors} frames");
    let st = w.stats();
    let report = format!(
        "loaded per band {:?}, data tiles fetched {} (overview {}, empty {}) decoded {}, landmark tiles delivered {lm_delivered} (held {}), levels {:?}\n  latency {}",
        st.loaded,
        st.data_tiles_fetched,
        st.data_tiles_fetched_overview,
        st.data_tiles_fetched_empty,
        st.data_tiles_decoded,
        lm_held.len(),
        st.data_levels,
        w.latency().map(|l| l.report()).unwrap_or_default()
    );
    w.shutdown();
    (misses, report, lm_delivered)
}

/// Every building is drawn exactly once per band group on the real two-level archive, checked on
/// the densest detail tile (14/9342/5207, central Vilnius) and on the detail tile holding the
/// first landmark of their overview tile (11/1167/650): no landmark in detail; summed over all
/// R0 / R1 / R2 chunks, each building's roof the same with the landmarks folded in and with the
/// landmark layer, and never about twice its footprint; landmarks only where they are drawn
/// (folded chunks or the landmark tile); the light points of the two modes differ by exactly the
/// landmarks' lights.
fn real_buildings_drawn_once(url: &str) {
    use crate::data::{build_chunk_with, build_landmark_tile, decode_data_tile, decode_overview_tile};
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let f = crate::fetch::Fetcher::new(url, "", Arc::new(AtomicU64::new(0)), crate::fetch::FetchParams::default(), Box::new(|_| {})).unwrap();
    let k11 = TileKey::new(11, 1167, 650);
    let levels = rt.block_on(f.open_levels()).unwrap();
    assert_eq!(levels, crate::data::DataLevels::two_level(14, 11, true));
    let ob = rt.block_on(f.fetch(k11)).unwrap().0;
    let p = crate::profile::test_default();
    let frame = crate::geo::Frame::new(VILNIUS.0, VILNIUS.1);
    let flat = crate::terrain::Terrain::flat();
    let t0 = Instant::now();
    let o = decode_overview_tile(k11, frame, &flat, &ob, p).unwrap();
    eprintln!(
        "real: overview {k11} {} KB gz: {} blocks, {} landmarks, {} lights, decoded in {:.0} ms",
        ob.len() / 1024,
        o.buildings.len(),
        o.landmarks.len(),
        o.lights.len(),
        t0.elapsed().as_secs_f64() * 1e3
    );
    let lt = build_landmark_tile(&o, p, &|| false).unwrap().unwrap();
    assert!(lt.building_count == o.landmarks.len() && !lt.mesh.is_empty());
    let lm_ids: std::collections::BTreeSet<u64> = o.landmarks.iter().map(|b| b.id).collect();
    let lm_seeds: std::collections::BTreeSet<u32> = o.landmarks.iter().map(|b| b.seed01().to_bits()).collect();
    let c = o.landmarks[0].centroid_px;
    let ext = o.proj.extent();
    let lm14 = TileKey::new(14, k11.x * 8 + (c[0] / ext * 8.0) as u32, k11.y * 8 + (c[1] / ext * 8.0) as u32);
    let never = || false;
    for k14 in [TileKey::new(14, 9342, 5207), lm14] {
        let db = rt.block_on(f.fetch(k14)).unwrap().0;
        let d = decode_data_tile(k14, frame, &flat, &db, p).unwrap();
        assert!(d.buildings.iter().all(|b| !lm_ids.contains(&b.id)), "{k14}: a landmark is in detail");
        let in_tile = |b: &crate::mvt::Building| o.proj.cut(k14).unwrap().place(b).is_some();
        let lm_here = o.landmarks.iter().filter(|b| in_tile(b)).count();
        eprintln!("real: detail {k14} {} KB: {} footprints, {lm_here} landmarks here", db.len() / 1024, d.buildings.len());
        let not_dark = |b: &&crate::mvt::Building| crate::mesher::resolve(b, p).profile != crate::mesher::PROFILE_DARK;
        for (z, ring, src) in [(16u8, 0usize, &d), (15, 1, &d), (14, 2, &o)] {
            let mut ks = vec![k14];
            while ks[0].z < z {
                ks = ks.into_iter().flat_map(crate::rings::children).collect();
            }
            let (mut folded, mut layer) = (BTreeMap::new(), BTreeMap::new());
            let (mut lf, mut ll) = (0usize, 0usize);
            for &k in &ks {
                let a = build_chunk_with(src, Some(&o), k, ring, p, &never).unwrap().unwrap();
                let b = build_chunk_with(src, None, k, ring, p, &never).unwrap().unwrap();
                roofs(&a.mesh, &mut folded);
                roofs(&b.mesh, &mut layer);
                lf += a.lights.positions.len();
                ll += b.lights.positions.len();
            }
            // Footprint per seed (seeds are 24 bits: several buildings can share one), outer
            // rings only (a courtyard cut by a chunk edge is roofed over, known).
            let mut fp: BTreeMap<u32, f64> = BTreeMap::new();
            for (b, mpp) in src.buildings.iter().map(|b| (b, src.proj.m_per_px())).chain(o.landmarks.iter().map(|b| (b, o.proj.m_per_px()))) {
                let outer: f64 = b.polygons.iter().filter_map(|p| p.first()).map(|r| crate::mvt::ring_area2(r).abs() * 0.5).sum();
                *fp.entry(b.seed01().to_bits()).or_default() += outer * mpp * mpp;
            }
            // Doubling = about 2x the footprint. A concave footprint cut by a chunk edge gets
            // its notch roofed over by the clipper (known, both paths, ~1.3-1.4x; the clipping
            // fix of another step): counted and printed, not failed.
            let (mut doubled, mut clip_artifacts) = (Vec::new(), 0usize);
            for (seed, m2) in &folded {
                let want = fp.get(seed).copied().unwrap_or(0.0);
                if *m2 > want * 1.9 + 2.0 {
                    doubled.push((*seed, *m2, want));
                } else if *m2 > want * 1.2 + 2.0 {
                    clip_artifacts += 1;
                }
                if !lm_seeds.contains(seed) {
                    assert_eq!(layer.get(seed), Some(m2), "z{z} {k14}: a building differs between the landmark modes");
                }
            }
            assert!(doubled.is_empty(), "z{z} {k14}: {} buildings drawn twice: {:?}", doubled.len(), &doubled[..doubled.len().min(5)]);
            let shared = |sd: &u32| src.buildings.iter().any(|b| b.seed01().to_bits() == *sd);
            for sd in lm_seeds.iter().filter(|sd| !shared(sd)) {
                assert!(!layer.contains_key(sd), "z{z} {k14}: a landmark drawn by a chunk with the landmark layer on");
                assert!(lt.mesh.colors.iter().any(|c| c[1].to_bits() == *sd), "landmark tile lacks a landmark");
            }
            if ring >= p.lights.min_ring as usize {
                let lm_lights = if ring == 2 {
                    o.lights.iter().zip(&o.light_landmark).filter(|(b, f)| **f && in_tile(b)).map(|(b, _)| b).filter(not_dark).count()
                } else {
                    o.landmarks.iter().filter(|b| in_tile(b)).filter(not_dark).count()
                };
                assert_eq!(lf, ll + lm_lights, "z{z} {k14}: lights folded = landmark layer mode + the landmarks here");
            }
            eprintln!(
                "real {k14} z{z}: {} roofs folded ({} with the landmark layer), lights {lf} / {ll}, {clip_artifacts} concave footprints roofed over at a chunk edge (known clipping issue)",
                folded.len(),
                layer.len()
            );
        }
    }
}

/// Real-archive acceptance (Tiles v2 T2b): `SIMWORLD_V2_ARCHIVE=<vilnius-v2.pmtiles>
/// SIMWORLD_COMPAT_ARCHIVE=<vilnius.pmtiles> cargo test --release -p simworld-core
/// real_archive -- --ignored --nocapture`. Serves both files in process and flies the same
/// 20 km through each (`real_flight`: visibility rule every frame, landmark tiles once): the
/// two-level archive, the compatibility archive (R0 / R1 cut from z14 data tiles since T2a) and
/// the compatibility archive with every band fetched per tile (the api 1.7 behaviour, the
/// spec's baseline). Checks "drawn once" on the two-level archive's densest tiles
/// (`real_buildings_drawn_once`). Prints requests, bytes and latency tables. Asserts at least
/// 10x fewer HTTP requests than the api 1.7 baseline (the spec's acceptance) and at least 5x
/// fewer than the T2a compatibility path.
#[test]
#[ignore]
fn real_archive_flight_two_level_vs_compat() {
    let (Ok(v2), Ok(compat)) = (std::env::var("SIMWORLD_V2_ARCHIVE"), std::env::var("SIMWORLD_COMPAT_ARCHIVE")) else {
        eprintln!("SIMWORLD_V2_ARCHIVE / SIMWORLD_COMPAT_ARCHIVE not set; skipped");
        return;
    };
    let (u2, r2, b2) = serve_counted(std::fs::read(&v2).expect("v2 archive"));
    real_buildings_drawn_once(&u2);
    let (base_r, base_b) = (r2.load(Ordering::Relaxed), b2.load(Ordering::Relaxed));
    let (miss2, rep2, lm2) = real_flight(&u2, vec!["world".into(), "landmarks".into()], false);
    let (req2, bytes2) = (r2.load(Ordering::Relaxed) - base_r, b2.load(Ordering::Relaxed) - base_b);
    let compat_bytes = std::fs::read(&compat).expect("compat archive");
    let (uc, rc, bc) = serve_counted(compat_bytes.clone());
    let (missc, repc, lmc) = real_flight(&uc, vec!["world".into(), "landmarks".into()], false);
    let (reqc, bytesc) = (rc.load(Ordering::Relaxed), bc.load(Ordering::Relaxed));
    let (u7, r7, b7) = serve_counted(compat_bytes);
    let (miss7, rep7, _) = real_flight(&u7, vec!["world".into()], true);
    let (req7, bytes7) = (r7.load(Ordering::Relaxed), b7.load(Ordering::Relaxed));
    let mb = |b: u64| b as f64 / 1048576.0;
    eprintln!("two-level:          {req2} requests, {:.2} MB, {miss2} frames with no tile under the camera after 5 s\n  {rep2}", mb(bytes2));
    eprintln!("compat (T2a):       {reqc} requests, {:.2} MB, {missc} frames with no tile under the camera after 5 s\n  {repc}", mb(bytesc));
    eprintln!("compat per tile 1.7: {req7} requests, {:.2} MB, {miss7} frames with no tile under the camera after 5 s\n  {rep7}", mb(bytes7));
    eprintln!(
        "requests: {:.1}x fewer than 1.7, {:.1}x fewer than T2a compat; bytes: {:.2}x / {:.2}x fewer",
        req7 as f64 / req2 as f64,
        reqc as f64 / req2 as f64,
        bytes7 as f64 / bytes2 as f64,
        bytesc as f64 / bytes2 as f64
    );
    assert!(lm2 > 0 && lmc == 0, "landmark tiles: two-level {lm2}, compat {lmc}");
    assert!(req7 >= 10 * req2, "{req7} api-1.7 requests vs {req2}: less than 10x");
    assert!(reqc >= 5 * req2, "{reqc} T2a compat requests vs {req2}: less than 5x");
}

/// The archive's data levels are recorded in the disk cache when it opens, so a start with the
/// server unreachable still knows them from a warm cache (and streams the cached tiles).
#[test]
fn data_levels_are_recorded_in_the_disk_cache_for_offline_starts() {
    let (url, _) = serve(archive_v2((11, 14)));
    let dir = std::env::temp_dir().join(format!("simworld-t2b-levels-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let fetcher = |u: &str| {
        crate::fetch::Fetcher::new(u, &dir.to_string_lossy(), Arc::new(AtomicU64::new(0)), crate::fetch::FetchParams::default(), Box::new(|_| {})).unwrap()
    };
    let a = fetcher(&url);
    let want = crate::data::DataLevels::two_level(14, 11, true);
    assert_eq!(rt.block_on(a.open_levels()).unwrap(), want);
    assert_eq!(rt.block_on(a.cached_levels()), Some(want));
    assert!(a.close_disk(Duration::from_secs(2)));
    drop(a);
    // The same cache file under an unreachable URL (the cache is per source URL).
    let dead = "http://127.0.0.1:9/t.pmtiles";
    std::fs::rename(dir.join(crate::fetch::cache_file_name(&url)), dir.join(crate::fetch::cache_file_name(dead))).unwrap();
    let b = fetcher(dead);
    assert!(rt.block_on(b.open_levels()).is_err());
    assert_eq!(rt.block_on(b.cached_levels()), Some(want));
    assert!(b.close_disk(Duration::from_secs(2)));
    let _ = std::fs::remove_dir_all(&dir);
}
