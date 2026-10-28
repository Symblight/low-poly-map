//! Tiles v2 (step T2a) end to end: a `World` on a real `.pmtiles` archive served over HTTP by a
//! small in-process server (Range requests), so fetch, decode, chunk jobs, the job pool's
//! dependencies, the memory budget and `stats()` run exactly as in the host.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::data::fixture::{key14, tile_bytes};
use crate::geo::TileKey;
use crate::rings::{children, LayerKind};
use crate::world::{World, WorldOptions};

pub(crate) const VILNIUS: (f64, f64) = crate::data::fixture::VILNIUS;

/// A `.pmtiles` (v3) of today's layout (zooms 13..16, no `simworld_levels`): the fixture area in
/// the z14 tile under the origin and in its z15 / z16 children, an empty z13 parent.
pub(crate) fn archive() -> Vec<u8> {
    use pmtiles::{TileCoord, TileId};
    let k14 = key14();
    let mut tiles: BTreeMap<u64, (TileKey, Vec<u8>)> = BTreeMap::new();
    let mut add = |k: TileKey, b: Vec<u8>| {
        let id = TileId::from(TileCoord::new(k.z, k.x, k.y).unwrap()).value();
        tiles.insert(id, (k, b));
    };
    add(TileKey::new(13, k14.x / 2, k14.y / 2), crate::mvt::test_mvt::encode_layer("buildings", &[]));
    add(k14, tile_bytes(k14));
    for k15 in children(k14) {
        add(k15, tile_bytes(k15));
        for k16 in children(k15) {
            add(k16, tile_bytes(k16));
        }
    }
    let meta = serde_json::json!({ "vector_layers": [{ "id": "buildings" }, { "id": "landuse" }, { "id": "roads" }] });
    pmtiles_of(tiles.into_values().collect(), 13, 16, meta)
}

/// A two-level archive (Tiles v2 T2b, as `simworld-cli bake` writes it): the fixture's detail z14
/// tile (landmarks left out) and its overview z11 tile; `zooms` = the header's zoom range.
fn archive_v2(zooms: (u8, u8)) -> Vec<u8> {
    use crate::data::fixture::{detail_v2_bytes, key11, overview_bytes};
    let meta = serde_json::json!({
        "simworld_levels": {"detail": 14, "overview": 11},
        "vector_layers": [{"id": "buildings"}, {"id": "landuse"}, {"id": "roads"}, {"id": "blocks"}, {"id": "landmarks"}, {"id": "lights"}]
    });
    pmtiles_of(vec![(key11(), overview_bytes()), (key14(), detail_v2_bytes())], zooms.0, zooms.1, meta)
}

/// Write `tiles` into a `.pmtiles` (v3, uncompressed tiles) with this zoom range and metadata.
fn pmtiles_of(tiles: Vec<(TileKey, Vec<u8>)>, min_zoom: u8, max_zoom: u8, meta: serde_json::Value) -> Vec<u8> {
    use pmtiles::{Compression, PmTilesWriter, TileCoord, TileId, TileType};
    let mut tiles: Vec<(u64, TileKey, Vec<u8>)> =
        tiles.into_iter().map(|(k, b)| (TileId::from(TileCoord::new(k.z, k.x, k.y).unwrap()).value(), k, b)).collect();
    tiles.sort_by_key(|t| t.0);
    let path = std::env::temp_dir().join(format!("simworld-t2a-{}-{}.pmtiles", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let file = std::fs::File::create(&path).unwrap();
    let mut w = PmTilesWriter::new(TileType::Mvt)
        .tile_compression(Compression::None)
        .internal_compression(Compression::Gzip)
        .min_zoom(min_zoom)
        .max_zoom(max_zoom)
        .bounds(VILNIUS.1 - 0.1, VILNIUS.0 - 0.1, VILNIUS.1 + 0.1, VILNIUS.0 + 0.1)
        .center(VILNIUS.1, VILNIUS.0)
        .center_zoom(14)
        .metadata(meta.to_string())
        .create(std::io::BufWriter::new(file))
        .unwrap();
    for (_, k, b) in tiles {
        w.add_tile(TileCoord::new(k.z, k.x, k.y).unwrap(), &b).unwrap();
    }
    w.finalize().unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    bytes
}

static NEXT: AtomicU64 = AtomicU64::new(0);

/// Serve `bytes` at `http://127.0.0.1:<port>/t.pmtiles` with HTTP/1.1 Range support (206), one
/// thread per connection, keep-alive. Returns the URL and the request counter. The server lives
/// until the test process ends.
pub(crate) fn serve(bytes: Vec<u8>) -> (String, Arc<AtomicU64>) {
    let (url, n, _) = serve_counted(bytes);
    (url, n)
}

/// [`serve`] that also counts the body bytes sent.
fn serve_counted(bytes: Vec<u8>) -> (String, Arc<AtomicU64>, Arc<AtomicU64>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/t.pmtiles", listener.local_addr().unwrap());
    let bytes = Arc::new(bytes);
    let count = Arc::new(AtomicU64::new(0));
    let sent = Arc::new(AtomicU64::new(0));
    let (c2, s2) = (count.clone(), sent.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let (bytes, count, sent) = (bytes.clone(), c2.clone(), s2.clone());
            std::thread::spawn(move || {
                let mut out = stream.try_clone().unwrap();
                let mut r = BufReader::new(stream);
                loop {
                    let mut line = String::new();
                    if r.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let mut range = None;
                    let mut body_len = 0usize;
                    loop {
                        let mut h = String::new();
                        if r.read_line(&mut h).unwrap_or(0) == 0 {
                            return;
                        }
                        let h = h.trim_end();
                        if h.is_empty() {
                            break;
                        }
                        let lower = h.to_ascii_lowercase();
                        if let Some(v) = lower.strip_prefix("range: bytes=") {
                            let (a, b) = v.split_once('-').unwrap();
                            range = Some((a.parse::<usize>().unwrap(), b.parse::<usize>().unwrap()));
                        } else if let Some(v) = lower.strip_prefix("content-length: ") {
                            body_len = v.parse().unwrap_or(0);
                        }
                    }
                    let mut skip = vec![0u8; body_len];
                    let _ = r.read_exact(&mut skip);
                    count.fetch_add(1, Ordering::Relaxed);
                    let (a, b) = range.unwrap_or((0, bytes.len() - 1));
                    let b = b.min(bytes.len() - 1);
                    let body = &bytes[a..=b];
                    sent.fetch_add(body.len() as u64, Ordering::Relaxed);
                    let head = format!(
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {a}-{b}/{}\r\nAccept-Ranges: bytes\r\nETag: \"t2a\"\r\n\r\n",
                        body.len(),
                        bytes.len()
                    );
                    if out.write_all(head.as_bytes()).and_then(|_| out.write_all(body)).is_err() {
                        return;
                    }
                }
            });
        }
    });
    (url, count, sent)
}

pub(crate) fn world() -> World {
    World::new(Arc::new(crate::NullLog), crate::Diagnostics::default(), "test")
}

pub(crate) fn opts(url: &str) -> WorldOptions {
    WorldOptions { origin_lat: VILNIUS.0, origin_lon: VILNIUS.1, tile_url: url.into(), ..WorldOptions::default() }
}

/// Bits of a mesh (positions, colors, indices) for equality across runs.
pub(crate) fn mesh_hash(m: &crate::mesher::MeshData) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut eat = |v: u64| h = (h ^ v).wrapping_mul(0x100_0000_01b3);
    for p in m.positions.iter().flatten().chain(m.colors.iter().flatten()) {
        eat(p.to_bits() as u64);
    }
    for i in &m.indices {
        eat(*i as u64);
    }
    h
}

/// Run the camera at 300 m AGL over the origin until nothing is queued, fetching or meshing.
/// Returns (key -> mesh hash of its last delivery, deliveries).
pub(crate) fn settle(w: &mut World) -> (BTreeMap<TileKey, (u64, u64)>, usize) {
    let mut got = BTreeMap::new();
    let mut n = 0;
    let t0 = Instant::now();
    loop {
        w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
        n += w.pop_ready(64, |t| {
            if t.layer == LayerKind::World {
                got.insert(t.key, (mesh_hash(t.mesh), mesh_hash(t.ground)));
            }
        });
        let _ = w.drain_fades();
        let _ = w.drain_unload();
        let s = w.stats();
        let busy: i32 = s.queued.iter().chain(s.fetching.iter()).chain(s.meshing.iter()).sum();
        if n > 0 && busy == 0 {
            return (got, n);
        }
        assert!(t0.elapsed() < Duration::from_secs(60), "did not settle: {s:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The host path on a real archive: R0 / R1 chunks are cut from z14 data tiles, each fetched and
/// decoded once (`stats().data_tiles_*`, the `debug_stats` keys); the chunks under the camera
/// carry the archive's buildings; the decoded tiles are in the memory budget. Then under a
/// budget smaller than one tile, evicted tiles are fetched and decoded again and every chunk
/// comes out byte-identical.
#[test]
fn archive_chunks_decode_each_data_tile_once_and_survive_eviction() {
    let (url, requests) = serve(archive());
    let mut w = world();
    let r = w.configure_world(opts(&url));
    assert!(r.ok, "{}", r.error);
    let (a, n) = settle(&mut w);
    let s = w.stats();
    let chunks: Vec<TileKey> = a.keys().copied().filter(|k| k.z >= 15).collect();
    let tiles: std::collections::BTreeSet<TileKey> = chunks.iter().map(|k| TileKey::new(14, k.x >> (k.z - 14), k.y >> (k.z - 14))).collect();
    assert!(tiles.len() >= 2 && chunks.len() >= 4 * tiles.len(), "{} chunks, {} data tiles", chunks.len(), tiles.len());
    assert_eq!(s.data_tiles_decoded, tiles.len() as u64, "one decode per data tile");
    assert_eq!(s.data_tiles_fetched, tiles.len() as u64, "one fetch per data tile");
    assert_eq!(s.data_tiles_loaded as usize, tiles.len());
    let pool = s.memory.pool(crate::budget::Pool::DataTiles);
    assert_eq!((pool.items as usize, pool.pinned_items), (tiles.len(), 0));
    let under = crate::geo::tile_for(VILNIUS.0, VILNIUS.1, 16);
    let empty = mesh_hash(&Default::default());
    assert!(a.get(&under).is_some_and(|h| h.0 != empty), "R0 under the camera has the archive's buildings");
    let req_a = requests.load(Ordering::Relaxed);
    eprintln!(
        "archive: {n} deliveries, {} chunks from {} data tiles ({} KB decoded), {req_a} HTTP requests",
        chunks.len(),
        tiles.len(),
        pool.bytes / 1024
    );

    // Tiny budget: evicted as soon as unpinned, fetched and decoded again, same chunks.
    let mut w2 = world();
    assert!(w2.configure_world(opts(&url)).ok);
    w2.memory_mut().set_budget_bytes(1);
    let (b, _) = settle(&mut w2);
    let s2 = w2.stats();
    assert!(s2.data_tiles_decoded > tiles.len() as u64, "{} decodes", s2.data_tiles_decoded);
    assert_eq!(s2.data_tiles_fetched, s2.data_tiles_decoded);
    let pool2 = s2.memory.pool(crate::budget::Pool::DataTiles);
    assert_eq!((pool2.items, pool2.pinned_items, s2.memory.over_budget_bytes), (0, 0, 0));
    for k in &chunks {
        assert_eq!(a.get(k), b.get(k), "{k}: chunk differs after eviction");
    }
    eprintln!("budget 1 byte: {} decodes / fetches, {} evictions", s2.data_tiles_decoded, pool2.evictions);
    // Shutdown releases the pool.
    assert!(w.shutdown() && w2.shutdown());
    assert_eq!(w.stats().memory.pool(crate::budget::Pool::DataTiles).items, 0);
}

// ---------------------------------------------------------------------------------------------
// Tiles v2, step T2b: two-level archives end to end

/// Run the camera at 300 m AGL over the origin until the vector layer and the landmark layer are
/// idle. Returns (vector deliveries by key, landmark deliveries in order).
fn settle_v2(w: &mut World) -> (BTreeMap<TileKey, u64>, Vec<TileKey>) {
    let mut got = BTreeMap::new();
    let mut lms = Vec::new();
    let mut n = 0;
    let t0 = Instant::now();
    loop {
        w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
        n += w.pop_ready(64, |t| match t.layer {
            LayerKind::Landmarks => lms.push(t.key),
            _ => {
                got.insert(t.key, mesh_hash(t.mesh));
            }
        });
        let _ = w.drain_fades();
        let _ = w.drain_unload();
        let s = w.stats();
        let busy: i32 = s.queued.iter().chain(s.fetching.iter()).chain(s.meshing.iter()).sum();
        if n > 0 && busy == 0 && s.landmarks.pending == 0 && s.data_levels.is_some() {
            return (got, lms);
        }
        assert!(t0.elapsed() < Duration::from_secs(60), "did not settle: {s:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// A served two-level archive: the levels come from its metadata (`stats().data_levels`), every
/// band streams from the two data levels (no request for a z13 / z15 / z16 tile; one fetch and
/// one decode per data tile), R2 over the fixture carries its blocks. With `layers` containing
/// `"landmarks"` the one overview tile with landmarks arrives once as `l/11/x/y` (layer
/// `Landmarks`, `debug_tiles` lists it); without it no `l/` key exists anywhere. An archive whose
/// zoom range does not match its `simworld_levels` is refused (status error).
#[test]
fn two_level_archive_streams_every_band_and_the_landmark_layer() {
    use crate::data::fixture::key11;
    let (url, requests) = serve(archive_v2((11, 14)));
    for landmarks in [true, false] {
        let mut w = world();
        let layers = if landmarks { vec!["world".to_string(), "landmarks".to_string()] } else { vec!["world".to_string()] };
        let r = w.configure_world(WorldOptions { layers: Some(layers), ..opts(&url) });
        assert!(r.ok, "{}", r.error);
        let req0 = requests.load(Ordering::Relaxed);
        let (got, lms) = settle_v2(&mut w);
        let s = w.stats();
        assert_eq!(s.data_levels, Some(crate::data::DataLevels::two_level(14, 11, true)));
        assert_eq!(w.landmark_layer_active(), landmarks);
        assert!(got.keys().any(|k| k.z == 13) && got.keys().any(|k| k.z == 16), "every band delivered");
        assert_ne!(got[&key14()], mesh_hash(&Default::default()), "R2 over the fixture has its blocks");
        assert_eq!(s.data_tiles_fetched, s.data_tiles_decoded, "one decode per fetch");
        let mut tiles = 0;
        let mut l_keys = Vec::new();
        w.debug_tiles(|k, _, _, _| match k.layer {
            LayerKind::Landmarks => l_keys.push(k.to_string()),
            _ => tiles += 1,
        });
        if landmarks {
            assert_eq!(lms, vec![key11()], "one landmark tile, delivered once");
            assert!(l_keys.contains(&format!("l/{}", key11())), "{l_keys:?}");
            assert_eq!(s.landmarks.loaded, 1);
        } else {
            assert!(lms.is_empty() && l_keys.is_empty());
        }
        let req = requests.load(Ordering::Relaxed) - req0;
        eprintln!(
            "two-level archive, landmark layer {landmarks}: {} chunks, {} data tiles fetched, {req} HTTP requests, {tiles} tracked",
            got.len(),
            s.data_tiles_fetched
        );
        // Teleport: the landmark tile goes with the rest.
        let r = w.configure_world(WorldOptions { layers: None, ..opts(&url) });
        assert!(r.ok);
        let un: Vec<String> = w.drain_unload().iter().map(|k| k.to_string()).collect();
        assert_eq!(un.iter().any(|k| k.starts_with("l/")), landmarks);
        w.shutdown();
    }
    // Zooms that do not match `simworld_levels`: refused.
    let (bad, _) = serve(archive_v2((11, 16)));
    let mut w = world();
    assert!(w.configure_world(opts(&bad)).ok);
    let t0 = Instant::now();
    while w.status().state != crate::status::StatusState::Error {
        w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
        assert!(t0.elapsed() < Duration::from_secs(20), "{:?}", w.status());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(w.status().last_error.contains("simworld_levels"), "{:?}", w.status());
    assert_eq!(w.stats().data_levels, None);
}

/// `layers` accepts `"landmarks"`; unknown names are still refused.
#[test]
fn layers_option_knows_landmarks() {
    let mut w = world();
    let r = w.configure_world(WorldOptions { layers: Some(vec!["world".into(), "landmarks".into()]), ..opts("") });
    assert!(r.ok, "{}", r.error);
    let r = w.configure_world(WorldOptions { layers: Some(vec!["landmark".into()]), ..opts("") });
    assert!(!r.ok && r.error.contains("known: world, terrain, landmarks"), "{}", r.error);
}

/// The synthetic city with the landmark layer: `l/11/x/y` tiles (one per overview tile with
/// landmarks in range), each delivered once; without the layer, none.
#[test]
fn synthetic_city_streams_landmark_tiles_on_request() {
    for landmarks in [true, false] {
        let mut w = world();
        let layers = if landmarks { Some(vec!["world".into(), "landmarks".into()]) } else { None };
        assert!(w.configure_world(WorldOptions { layers, ..opts("") }).ok);
        let (got, lms) = settle_v2(&mut w);
        assert_eq!(w.stats().data_levels, Some(crate::data::DataLevels::SYNTHETIC));
        let distinct: std::collections::BTreeSet<TileKey> = lms.iter().copied().collect();
        assert_eq!(distinct.len(), lms.len(), "a landmark tile delivered twice");
        assert_eq!(!lms.is_empty(), landmarks, "{lms:?}");
        assert!(lms.iter().all(|k| k.z == 11));
        eprintln!("synthetic, landmark layer {landmarks}: {} chunks, {} landmark tiles", got.len(), lms.len());
    }
}

// ---------------------------------------------------------------------------------------------
// Real archives (ignored): `data/out/vilnius-v2.pmtiles` against `data/out/vilnius.pmtiles`

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
