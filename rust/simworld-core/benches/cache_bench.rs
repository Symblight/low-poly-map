//! Disk tile cache benchmark: the SQLite `DiskCache` (one file per source) against the old
//! file-per-tile cache it replaced (docs/spec-tile-cache.md, "Benchmark and acceptance").
//!
//! Run from `rust/` (release build, needs `data/out/vilnius.pmtiles`, see `data/README.md`):
//!
//! ```text
//! cargo bench -p simworld-core --bench cache_bench
//! ```
//!
//! Environment: `SIMWORLD_BENCH_PMTILES` (archive path, default `../data/out/vilnius.pmtiles`
//! relative to the workspace), `SIMWORLD_BENCH_DIR` (scratch folder, default the system temp
//! folder; it needs about 2 GB free for the 300 MB file-per-tile fill), `SIMWORLD_BENCH_SKIP_300=1`
//! (skip the slow 300 MB fills), `SIMWORLD_BENCH_KEEP=1` (keep the scratch folder).
//!
//! The tile bytes are the archive's own (gzip MVT), read once into memory. `OldCache` is a
//! minimal copy of the file-per-tile `DiskCache` of `simworld-core` 0.5.0 (git `41f414d`,
//! `fetch.rs`): `{root}/{z}/{x}/{y}.mvt`, tmp + rename writes, mtime touched on a hit, LRU index
//! rebuilt by a directory walk on open, eviction by unlink. Caller-side timings use the same
//! call path as `Fetcher::fetch` then and now: the old cache through `spawn_blocking`, the new
//! one through its async `get` / fire-and-forget `put`.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use simworld_core::fetch::{cache_file_name, DiskCache};
use simworld_core::geo::TileKey;

// ---------------------------------------------------------------------------------------
// The old file-per-tile cache (minimal copy; `lru::LruCache` replaced by a seq-ordered map)
// ---------------------------------------------------------------------------------------

#[derive(Default)]
struct OldInner {
    /// key -> (seq, len); `order` is seq -> key (oldest first).
    map: HashMap<TileKey, (u64, u64)>,
    order: BTreeMap<u64, TileKey>,
    seq: u64,
    total: u64,
}

impl OldInner {
    fn touch(&mut self, k: TileKey, len: u64) -> Option<u64> {
        self.seq += 1;
        let old = self.map.insert(k, (self.seq, len));
        if let Some((s, _)) = old {
            self.order.remove(&s);
        }
        self.order.insert(self.seq, k);
        old.map(|(_, l)| l)
    }
}

struct OldCache {
    root: PathBuf,
    cap: u64,
    inner: Mutex<OldInner>,
    tmp_counter: AtomicU64,
}

impl OldCache {
    fn open(root: PathBuf, cap: u64) -> io::Result<Self> {
        fs::create_dir_all(&root)?;
        let mut found: Vec<(SystemTime, TileKey, u64)> = Vec::new();
        for zd in fs::read_dir(&root)?.flatten() {
            let Some(z) = zd.file_name().to_str().and_then(|s| s.parse::<u8>().ok()) else { continue };
            for xd in fs::read_dir(zd.path()).into_iter().flatten().flatten() {
                let Some(x) = xd.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
                for fe in fs::read_dir(xd.path()).into_iter().flatten().flatten() {
                    let name = fe.file_name();
                    let Some(name) = name.to_str() else { continue };
                    if let Some(y) = name.strip_suffix(".mvt").and_then(|s| s.parse::<u32>().ok()) {
                        if let Ok(md) = fe.metadata() {
                            let t = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                            found.push((t, TileKey::new(z, x, y), md.len()));
                        }
                    } else if name.contains(".tmp") {
                        let _ = fs::remove_file(fe.path());
                    }
                }
            }
        }
        found.sort();
        let mut inner = OldInner::default();
        for (_, k, len) in found {
            inner.touch(k, len);
            inner.total += len;
        }
        let c = Self { root, cap, inner: Mutex::new(inner), tmp_counter: AtomicU64::new(0) };
        c.evict();
        Ok(c)
    }

    fn len(&self) -> usize {
        self.inner.lock().unwrap().map.len()
    }

    fn total_bytes(&self) -> u64 {
        self.inner.lock().unwrap().total
    }

    fn path(&self, k: TileKey) -> PathBuf {
        self.root.join(k.z.to_string()).join(k.x.to_string()).join(format!("{}.mvt", k.y))
    }

    fn get(&self, k: TileKey) -> Option<Vec<u8>> {
        {
            let mut i = self.inner.lock().ok()?;
            let len = i.map.get(&k)?.1;
            i.touch(k, len);
        }
        let path = self.path(k);
        match fs::read(&path) {
            Ok(b) => {
                if let Ok(f) = fs::File::options().write(true).open(&path) {
                    let _ = f.set_modified(SystemTime::now());
                }
                Some(b)
            }
            Err(_) => None,
        }
    }

    fn put(&self, k: TileKey, data: &[u8]) -> io::Result<()> {
        let path = self.path(k);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let n = self.tmp_counter.fetch_add(1, Ordering::Relaxed);
        let tmp = path.with_extension(format!("mvt.tmp{n}"));
        fs::write(&tmp, data)?;
        fs::rename(&tmp, &path)?;
        if let Ok(mut i) = self.inner.lock() {
            if let Some(old) = i.touch(k, data.len() as u64) {
                i.total -= old;
            }
            i.total += data.len() as u64;
        }
        self.evict();
        Ok(())
    }

    fn evict(&self) {
        let Ok(mut i) = self.inner.lock() else { return };
        while i.total > self.cap {
            let Some((&s, &k)) = i.order.iter().next() else { break };
            i.order.remove(&s);
            if let Some((_, len)) = i.map.remove(&k) {
                i.total -= len;
            }
            let _ = fs::remove_file(self.path(k));
        }
    }
}

// ---------------------------------------------------------------------------------------
// Tile set: every tile of the archive, read into memory
// ---------------------------------------------------------------------------------------

struct MemBackend(Bytes);

impl pmtiles::AsyncBackend for MemBackend {
    fn read(
        &self,
        offset: usize,
        length: usize,
    ) -> impl std::future::Future<Output = pmtiles::PmtResult<pmtiles::BackendResponse>> + Send {
        let end = (offset + length).min(self.0.len());
        let b = self.0.slice(offset.min(end)..end);
        async move { Ok(pmtiles::BackendResponse::new(b)) }
    }
}

fn lon_to_x(lon: f64, z: u8) -> f64 {
    (lon + 180.0) / 360.0 * (1u64 << z) as f64
}

fn lat_to_y(lat: f64, z: u8) -> f64 {
    let r = lat.to_radians();
    (1.0 - (r.tan() + 1.0 / r.cos()).ln() / std::f64::consts::PI) / 2.0 * (1u64 << z) as f64
}

fn load_tiles(rt: &tokio::runtime::Runtime, path: &Path) -> Vec<(TileKey, Bytes)> {
    let data = Bytes::from(fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())));
    rt.block_on(async {
        let r = pmtiles::AsyncPmTilesReader::try_from_source(MemBackend(data)).await.expect("pmtiles header");
        let h = r.get_header();
        let mut out = Vec::new();
        for z in h.min_zoom..=h.max_zoom {
            let (x0, x1) = (lon_to_x(h.min_longitude, z) as u32, lon_to_x(h.max_longitude, z) as u32);
            let (y0, y1) = (lat_to_y(h.max_latitude, z) as u32, lat_to_y(h.min_latitude, z) as u32);
            for x in x0..=x1 {
                for y in y0..=y1 {
                    let c = pmtiles::TileCoord::new(z, x, y).unwrap();
                    if let Some(b) = r.get_tile(c).await.expect("tile") {
                        out.push((TileKey::new(z, x, y), b));
                    }
                }
            }
        }
        out
    })
}

/// Tile keys in the order an autopilot flight north from the test-bench origin first wants
/// them: camera steps of 100 m, at each step every tile whose center lies inside the band
/// radius (R0 z16 0.6 km ... R3 z13 25 km), finer band first; each key once. Only keys present
/// in `have`.
fn flight_order(have: &HashMap<TileKey, usize>) -> Vec<TileKey> {
    const ORIGIN: (f64, f64) = (54.6872, 25.2797);
    const BANDS: [(u8, f64); 4] = [(16, 600.0), (15, 2500.0), (14, 7000.0), (13, 25000.0)];
    const M_PER_DEG_LAT: f64 = 111_320.0;
    let m_per_deg_lon = M_PER_DEG_LAT * ORIGIN.0.to_radians().cos();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let max_lat = 54.92;
    let mut lat = ORIGIN.0;
    while lat <= max_lat {
        for (z, r) in BANDS {
            let n = (1u64 << z) as f64;
            let dlat = r / M_PER_DEG_LAT;
            let dlon = r / m_per_deg_lon;
            let (x0, x1) = (lon_to_x(ORIGIN.1 - dlon, z) as u32, lon_to_x(ORIGIN.1 + dlon, z) as u32);
            let (y0, y1) = (lat_to_y(lat + dlat, z) as u32, lat_to_y(lat - dlat, z) as u32);
            let mut ring = Vec::new();
            for x in x0..=x1 {
                for y in y0..=y1 {
                    let clon = (x as f64 + 0.5) / n * 360.0 - 180.0;
                    let cy = (y as f64 + 0.5) / n;
                    let clat = (std::f64::consts::PI * (1.0 - 2.0 * cy)).sinh().atan().to_degrees();
                    let d = (((clat - lat) * M_PER_DEG_LAT).powi(2) + ((clon - ORIGIN.1) * m_per_deg_lon).powi(2)).sqrt();
                    if d <= r {
                        ring.push((d, TileKey::new(z, x, y)));
                    }
                }
            }
            ring.sort_by(|a, b| a.0.total_cmp(&b.0));
            for (_, k) in ring {
                if have.contains_key(&k) && seen.insert(k) {
                    out.push(k);
                }
            }
        }
        lat += 100.0 / M_PER_DEG_LAT;
    }
    out
}

// ---------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            v.swap(i, (self.next() % (i as u64 + 1)) as usize);
        }
    }
}

/// (apparent bytes, allocated bytes, files) of everything under `p`.
fn disk_use(p: &Path) -> (u64, u64, u64) {
    let Ok(md) = fs::symlink_metadata(p) else { return (0, 0, 0) };
    #[cfg(unix)]
    let alloc = {
        use std::os::unix::fs::MetadataExt;
        md.blocks() * 512
    };
    #[cfg(not(unix))]
    let alloc = md.len();
    if md.is_dir() {
        let mut t = (0, alloc, 0);
        for e in fs::read_dir(p).into_iter().flatten().flatten() {
            let (a, b, c) = disk_use(&e.path());
            t = (t.0 + a, t.1 + b, t.2 + c);
        }
        t
    } else {
        (md.len(), alloc, 1)
    }
}

/// Database + `-wal` + `-shm`.
fn db_use(db: &Path) -> (u64, u64, u64) {
    let mut t = (0, 0, 0);
    for suffix in ["", "-wal", "-shm"] {
        let mut s = db.as_os_str().to_owned();
        s.push(suffix);
        let (a, b, c) = disk_use(Path::new(&s));
        t = (t.0 + a, t.1 + b, t.2 + c);
    }
    t
}

fn mb(b: u64) -> f64 {
    b as f64 / 1_048_576.0
}

fn pct(v: &mut [Duration], p: f64) -> Duration {
    v.sort();
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

fn stats(name: &str, mut v: Vec<Duration>) {
    let total: Duration = v.iter().sum();
    println!(
        "    {name:<34} n={:<6} p50={:>7.1} us  p99={:>7.1} us  max={:>8.1} us  total={:>8.1} ms",
        v.len(),
        pct(&mut v, 0.5).as_secs_f64() * 1e6,
        pct(&mut v, 0.99).as_secs_f64() * 1e6,
        pct(&mut v, 1.0).as_secs_f64() * 1e6,
        total.as_secs_f64() * 1e3
    );
}

fn fresh(dir: &Path) {
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir).unwrap();
}

const URL: &str = "http://127.0.0.1:8787/vilnius.pmtiles";

fn new_open(db: &Path, cap: u64) -> DiskCache {
    DiskCache::open(db.to_path_buf(), cap, Arc::new(AtomicU64::new(0))).expect("open sqlite cache")
}

fn old_put_all(rt: &tokio::runtime::Runtime, c: &Arc<OldCache>, tiles: &[(TileKey, Bytes)]) -> Vec<Duration> {
    rt.block_on(async {
        let mut lat = Vec::with_capacity(tiles.len());
        for (k, b) in tiles {
            let (c, k, b) = (c.clone(), *k, b.clone());
            let t = Instant::now();
            tokio::task::spawn_blocking(move || c.put(k, &b)).await.unwrap().unwrap();
            lat.push(t.elapsed());
        }
        lat
    })
}

fn new_put_all(c: &DiskCache, tiles: &[(TileKey, Bytes)]) -> Vec<Duration> {
    let mut lat = Vec::with_capacity(tiles.len());
    for (k, b) in tiles {
        let t = Instant::now();
        c.put(*k, b.clone());
        lat.push(t.elapsed());
    }
    lat
}

fn old_reads(rt: &tokio::runtime::Runtime, c: &Arc<OldCache>, keys: &[TileKey]) -> (Vec<Duration>, usize) {
    rt.block_on(async {
        let (mut lat, mut hits) = (Vec::with_capacity(keys.len()), 0);
        for k in keys {
            let (c, k) = (c.clone(), *k);
            let t = Instant::now();
            let r = tokio::task::spawn_blocking(move || c.get(k)).await.unwrap();
            lat.push(t.elapsed());
            hits += r.is_some() as usize;
        }
        (lat, hits)
    })
}

fn new_reads(rt: &tokio::runtime::Runtime, c: &DiskCache, keys: &[TileKey]) -> (Vec<Duration>, usize) {
    rt.block_on(async {
        let (mut lat, mut hits) = (Vec::with_capacity(keys.len()), 0);
        for k in keys {
            let t = Instant::now();
            let r = c.get(*k).await;
            lat.push(t.elapsed());
            hits += r.is_some() as usize;
        }
        (lat, hits)
    })
}

/// The tile set repeated under shifted x until it holds at least `bytes` (real size mix).
fn inflate(tiles: &[(TileKey, Bytes)], bytes: u64) -> Vec<(TileKey, Bytes)> {
    let mut out = Vec::new();
    let mut total = 0u64;
    let mut pass = 0u32;
    while total < bytes {
        for (k, b) in tiles {
            let shift = pass << (k.z - 6); // 1/64 of the world width per pass: no overlap
            out.push((TileKey::new(k.z, k.x + shift, k.y), b.clone()));
            total += b.len() as u64;
            if total >= bytes {
                break;
            }
        }
        pass += 1;
    }
    out
}

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let archive = std::env::var_os("SIMWORLD_BENCH_PMTILES")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../data/out/vilnius.pmtiles"));
    let scratch = std::env::var_os("SIMWORLD_BENCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("simworld-cache-bench-{}", std::process::id()));
    let skip_300 = std::env::var_os("SIMWORLD_BENCH_SKIP_300").is_some();
    // `cargo bench` passes `--bench`; ignore arguments.
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).build().unwrap();

    let tiles = load_tiles(&rt, &archive);
    let total: u64 = tiles.iter().map(|t| t.1.len() as u64).sum();
    let mut by_z = [0usize; 32];
    for (k, _) in &tiles {
        by_z[k.z as usize] += 1;
    }
    println!("tile set {}: {} tiles, {:.1} MB of tile data", archive.display(), tiles.len(), mb(total));
    for (z, n) in by_z.iter().enumerate().filter(|(_, n)| **n > 0) {
        println!("    z{z}: {n}");
    }
    println!("scratch {}", scratch.display());
    let index: HashMap<TileKey, usize> = tiles.iter().enumerate().map(|(i, t)| (t.0, i)).collect();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut fill_order = tiles.clone();
    rng.shuffle(&mut fill_order);
    let mut random_keys: Vec<TileKey> = tiles.iter().map(|t| t.0).collect();
    rng.shuffle(&mut random_keys);
    let flight = flight_order(&index);
    println!("flight order: {} keys (north from the bench origin, 100 m steps)", flight.len());

    let cap300 = 300 * 1_048_576u64;
    let old_dir = scratch.join("old");
    let new_dir = scratch.join("new");
    let db = new_dir.join(cache_file_name(URL));

    // 1. Cold fill, 300 MB cap (no eviction).
    println!("\n[1] cold fill: all tiles, random order, cap 300 MB");
    fresh(&old_dir);
    let t = Instant::now();
    let oc = Arc::new(OldCache::open(old_dir.clone(), cap300).unwrap());
    let lat = old_put_all(&rt, &oc, &fill_order);
    let old_fill = t.elapsed();
    println!("  old: {:.2} s total ({:.1} us/tile)", old_fill.as_secs_f64(), old_fill.as_secs_f64() * 1e6 / tiles.len() as f64);
    stats("old put (spawn_blocking, awaited)", lat);
    fresh(&new_dir);
    let t = Instant::now();
    let nc = new_open(&db, cap300);
    let lat = new_put_all(&nc, &fill_order);
    assert!(nc.flush(Duration::from_secs(120)), "flush");
    let new_fill = t.elapsed();
    println!(
        "  new: {:.2} s total incl. final flush ({:.1} us/tile), {} commits",
        new_fill.as_secs_f64(),
        new_fill.as_secs_f64() * 1e6 / tiles.len() as f64,
        nc.commit_count()
    );
    stats("new put (fire and forget)", lat);
    let (a, b, f) = disk_use(&old_dir);
    println!("  disk, full tile set: old {:.1} MB apparent / {:.1} MB allocated in {f} files", mb(a), mb(b));
    let (a, b, _) = db_use(&db);
    println!("  disk, full tile set: new {:.1} MB apparent / {:.1} MB allocated (db + wal + shm, open)", mb(a), mb(b));

    // 2. Warm random reads (page cache warm for both: the files / db were just written).
    println!("\n[2] warm random reads: every tile once, shuffled");
    let (lat, hits) = old_reads(&rt, &oc, &random_keys);
    println!("  old hits {hits}");
    stats("old get (spawn_blocking)", lat);
    let (lat, hits) = new_reads(&rt, &nc, &random_keys);
    println!("  new hits {hits}");
    stats("new get (async, cache thread)", lat);
    // Direct (no thread hop) for the old cache, as a floor.
    let mut lat = Vec::new();
    for k in &random_keys {
        let t = Instant::now();
        let _ = oc.get(*k);
        lat.push(t.elapsed());
    }
    stats("old get (direct call, no hop)", lat);

    // 3. Warm reads in flight order.
    println!("\n[3] warm reads in flight order");
    let (lat, _) = old_reads(&rt, &oc, &flight);
    stats("old get (spawn_blocking)", lat);
    let (lat, _) = new_reads(&rt, &nc, &flight);
    stats("new get (async, cache thread)", lat);

    // 5. Open time with the full tile set.
    println!("\n[5] open time, full tile set ({} tiles)", tiles.len());
    drop(oc);
    assert!(nc.close(Duration::from_secs(30)));
    drop(nc);
    for run in 0..3 {
        let t = Instant::now();
        let oc = OldCache::open(old_dir.clone(), cap300).unwrap();
        let ot = t.elapsed();
        let t = Instant::now();
        let nc = new_open(&db, cap300);
        let nt = t.elapsed();
        println!(
            "  run {run}: old {:.1} ms ({} tiles), new {:.1} ms ({} tiles)",
            ot.as_secs_f64() * 1e3,
            oc.len(),
            nt.as_secs_f64() * 1e3,
            nc.len()
        );
        nc.close(Duration::from_secs(30));
    }
    let (a, b, _) = db_use(&db);
    println!("  disk after close: new {:.1} MB apparent / {:.1} MB allocated", mb(a), mb(b));

    // 4. Eviction under a 50 MB cap.
    let cap50 = 50 * 1_048_576u64;
    println!("\n[4] eviction: cold fill of all tiles (random order) with a 50 MB cap");
    fresh(&old_dir);
    let oc = Arc::new(OldCache::open(old_dir.clone(), cap50).unwrap());
    let t = Instant::now();
    let lat = old_put_all(&rt, &oc, &fill_order);
    let el = t.elapsed();
    let (a, b, f) = disk_use(&old_dir);
    println!(
        "  old: {:.2} s, {} tiles, {:.1} MB data; disk {:.1} MB apparent / {:.1} MB allocated, {f} files",
        el.as_secs_f64(),
        oc.len(),
        mb(oc.total_bytes()),
        mb(a),
        mb(b)
    );
    stats("old put", lat);
    drop(oc);
    fresh(&new_dir);
    let nc = new_open(&db, cap50);
    let t = Instant::now();
    let lat = new_put_all(&nc, &fill_order);
    assert!(nc.flush(Duration::from_secs(120)));
    let el = t.elapsed();
    let (a, b, _) = db_use(&db);
    println!(
        "  new: {:.2} s incl. flush, {} tiles, {:.1} MB data; disk {:.1} MB apparent / {:.1} MB allocated (open)",
        el.as_secs_f64(),
        nc.len(),
        mb(nc.total_bytes()),
        mb(a),
        mb(b)
    );
    stats("new put", lat);
    // Eviction while reading: random reads of the evicting cache keep working.
    let (lat, hits) = new_reads(&rt, &nc, &random_keys[..5000]);
    println!("  new: {hits}/5000 hits after eviction");
    stats("new get (evicted cache)", lat);
    nc.close(Duration::from_secs(30));
    let (a, b, _) = db_use(&db);
    println!("  new after close: {:.1} MB apparent / {:.1} MB allocated", mb(a), mb(b));

    // 6. A full 300 MB cache: disk use and open time (tile set repeated under shifted x).
    if skip_300 {
        println!("\n[6] skipped (SIMWORLD_BENCH_SKIP_300)");
    } else {
        let big = inflate(&fill_order, cap300 * 5 / 2);
        let n_full = {
            let mut acc = 0u64;
            big.iter().take_while(|t| {
                acc += t.1.len() as u64;
                acc <= cap300 + cap300 / 20
            })
            .count()
        };
        println!(
            "\n[6] full 300 MB cache: old gets the first {} tiles (315 MB written), new all {} tiles ({:.0} MB written, steady-state churn); cap 300 MB",
            n_full,
            big.len(),
            mb(big.iter().map(|t| t.1.len() as u64).sum())
        );
        fresh(&old_dir);
        let oc = Arc::new(OldCache::open(old_dir.clone(), cap300).unwrap());
        let t = Instant::now();
        let _ = old_put_all(&rt, &oc, &big[..n_full]);
        println!("  old fill {:.1} s: {} tiles, {:.1} MB data", t.elapsed().as_secs_f64(), oc.len(), mb(oc.total_bytes()));
        let (a, b, f) = disk_use(&old_dir);
        println!("  old disk: {:.1} MB apparent / {:.1} MB allocated, {f} files + dirs", mb(a), mb(b));
        drop(oc);
        fresh(&new_dir);
        let nc = new_open(&db, cap300);
        let t = Instant::now();
        let (mut max_file, mut max_data) = (0u64, 0u64);
        let mut written = 0u64;
        for chunk in big.chunks(4096) {
            let _ = new_put_all(&nc, chunk);
            assert!(nc.flush(Duration::from_secs(300)));
            written += chunk.iter().map(|t| t.1.len() as u64).sum::<u64>();
            let (a, _, _) = db_use(&db);
            max_file = max_file.max(a);
            max_data = max_data.max(nc.total_bytes());
            if written >= cap300 * 9 / 10 {
                println!(
                    "    after {:>6.1} MB written: data {:>6.1} MB ({} tiles), file + wal {:>6.1} MB",
                    mb(written),
                    mb(nc.total_bytes()),
                    nc.len(),
                    mb(a)
                );
            }
        }
        println!(
            "  new fill {:.1} s; peak data {:.1} MB, peak file + wal {:.1} MB",
            t.elapsed().as_secs_f64(),
            mb(max_data),
            mb(max_file)
        );
        let (a, b, _) = db_use(&db);
        println!("  new disk (open, after flush): {:.1} MB apparent / {:.1} MB allocated (db + wal + shm)", mb(a), mb(b));
        nc.close(Duration::from_secs(60));
        let (a, b, _) = db_use(&db);
        println!("  new disk (closed): {:.1} MB apparent / {:.1} MB allocated", mb(a), mb(b));
        for run in 0..3 {
            let t = Instant::now();
            let oc = OldCache::open(old_dir.clone(), cap300).unwrap();
            let ot = t.elapsed();
            let t = Instant::now();
            let nc = new_open(&db, cap300);
            let nt = t.elapsed();
            println!(
                "  open run {run}: old {:.1} ms ({} tiles), new {:.1} ms ({} tiles)",
                ot.as_secs_f64() * 1e3,
                oc.len(),
                nt.as_secs_f64() * 1e3,
                nc.len()
            );
            nc.close(Duration::from_secs(30));
        }
    }

    if std::env::var_os("SIMWORLD_BENCH_KEEP").is_none() {
        let _ = fs::remove_dir_all(&scratch);
    }
    println!("\ndone");
}
