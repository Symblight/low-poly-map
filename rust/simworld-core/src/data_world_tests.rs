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

mod real_archive;
mod two_level;

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
