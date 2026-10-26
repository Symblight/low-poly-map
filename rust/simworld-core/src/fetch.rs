//! Tile bytes from a PMTiles v3 archive over HTTP(S) range requests, behind a disk LRU cache.
//!
//! * Network: the `pmtiles` crate (`AsyncPmTilesReader<HttpBackend, HashMapCache>`): header and
//!   root directory are read once when the reader opens, leaf directories are cached in memory,
//!   then each tile is one range request. reqwest uses rustls only; plain http:// also works.
//! * Disk: raw tile bytes exactly as stored in the archive (gzip MVT for planetiler) at
//!   `{cache_dir}/{source-hash}/{z}/{x}/{y}.mvt`; a zero-length file records "no tile here".
//!   LRU by file mtime (touched on hit), capped (world profile `cache.cap_mb`, default 300 MiB),
//!   survives restarts. The cache is
//!   consulted before the archive is even opened, so a warm cache works fully offline.
//! * Staleness: the cache folder holds a `fingerprint` file (hash of the archive header). Once
//!   the archive has been opened, a different fingerprint wipes the cached tiles, so rebuilding
//!   the archive at the same URL never serves old tiles. While the archive is unreachable the
//!   cache is served as is (offline use) and validated as soon as it can be opened.
//!
//! * Tileset contract (audit A18): when the archive opens, its header zoom range must cover
//!   13..16 and its metadata must list the `buildings`, `landuse` and `roads` layers; otherwise
//!   every fetch fails with that error (reported once through [`Note::Error`]).
//!
//! Everything here runs on tokio / blocking threads and never touches Godot.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use lru::LruCache;
use pmtiles::{AsyncPmTilesReader, Compression, HashMapCache, HttpBackend, TileCoord};

use crate::geo::TileKey;
use crate::mvt::{splitmix64, BUILDINGS_LAYER, LANDUSE_LAYER, ROADS_LAYER};
use crate::profile::Profile;
use crate::rings::{LEAF_ZOOM, ROOT_ZOOM};

/// Fetch / cache tuning (world profile `cache.*`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FetchParams {
    /// Disk cache LRU cap (bytes).
    pub cache_cap_bytes: u64,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    /// Minimum wait before re-opening an archive that failed to open.
    pub open_retry: Duration,
    /// While the archive cannot be opened, try validating the cache this often.
    pub validate_retry: Duration,
}

impl FetchParams {
    pub fn from_profile(p: &Profile) -> Self {
        let c = &p.cache;
        Self {
            cache_cap_bytes: (c.cap_mb * 1024.0 * 1024.0).round() as u64,
            connect_timeout: Duration::from_secs_f64(c.connect_timeout_s),
            request_timeout: Duration::from_secs_f64(c.request_timeout_s),
            open_retry: Duration::from_secs_f64(c.open_retry_s),
            validate_retry: Duration::from_secs_f64(c.validate_retry_s),
        }
    }
}

impl Default for FetchParams {
    fn default() -> Self {
        Self::from_profile(&Profile::default())
    }
}

/// Messages from the fetcher to the main thread (logged there).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Note {
    Info(String),
    /// Configuration problem that will not go away by retrying (e.g. a tileset mismatch).
    Error(String),
}

/// Check an opened archive against the tileset contract: zooms 13..16 and the layers the
/// decoder needs (`metadata` = the archive's JSON metadata).
pub fn check_tileset(min_zoom: u8, max_zoom: u8, metadata: &str) -> Result<(), String> {
    if min_zoom > ROOT_ZOOM || max_zoom < LEAF_ZOOM {
        return Err(format!(
            "tileset zoom range z{min_zoom}-{max_zoom} does not cover z{ROOT_ZOOM}-{LEAF_ZOOM} (the library's bands need every zoom from {ROOT_ZOOM} to {LEAF_ZOOM}; rebuild the archive with data/build_tiles.sh)"
        ));
    }
    let meta: serde_json::Value = serde_json::from_str(metadata).map_err(|e| format!("tileset metadata is not JSON: {e}"))?;
    let Some(layers) = meta.get("vector_layers").and_then(|v| v.as_array()) else {
        return Err("tileset metadata has no `vector_layers`: cannot verify the required layers (buildings, landuse, roads)".into());
    };
    let ids: Vec<&str> = layers.iter().filter_map(|l| l.get("id").and_then(|i| i.as_str())).collect();
    let missing: Vec<&str> =
        [BUILDINGS_LAYER, LANDUSE_LAYER, ROADS_LAYER].into_iter().filter(|l| !ids.contains(l)).collect();
    if !missing.is_empty() {
        return Err(format!(
            "tileset lacks required layer(s) {} (has: {}); build it with the planetiler profile in data/",
            missing.join(", "),
            ids.join(", ")
        ));
    }
    Ok(())
}
/// Bump when the tile schema or the decoder's expectations change without a new archive URL.
/// v2 (M5 Ground): `landuse` gained leisure/natural/riverbank kinds and `roads` is decoded;
/// folders of older versions for the same URL are deleted when the cache opens.
pub const CACHE_SCHEMA_VERSION: u32 = 2;
const FINGERPRINT_FILE: &str = "fingerprint";

// ---------------------------------------------------------------------------------------
// Disk LRU cache
// ---------------------------------------------------------------------------------------

struct Inner {
    lru: LruCache<TileKey, u64>,
    total: u64,
}

pub struct DiskCache {
    root: PathBuf,
    cap: u64,
    inner: Mutex<Inner>,
    /// Shared byte counter for `debug_stats().cache_mb`.
    bytes: Arc<AtomicU64>,
    tmp_counter: AtomicU64,
}

impl DiskCache {
    /// Open (and index) a cache directory. Scans existing files ordered by mtime.
    pub fn open(root: PathBuf, cap: u64, bytes: Arc<AtomicU64>) -> io::Result<Self> {
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
        let mut lru = LruCache::unbounded();
        let mut total = 0;
        for (_, k, len) in found {
            lru.put(k, len);
            total += len;
        }
        let c = Self {
            root,
            cap,
            inner: Mutex::new(Inner { lru, total }),
            bytes,
            tmp_counter: AtomicU64::new(0),
        };
        c.bytes.store(total, Ordering::Relaxed);
        c.evict();
        Ok(c)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Record `fp` as the archive this cache belongs to. If the cache was filled from a
    /// different archive (or from an unknown one, with tiles present) every tile is deleted.
    /// Returns true when tiles were wiped.
    pub fn sync_fingerprint(&self, fp: &str) -> io::Result<bool> {
        let path = self.root.join(FINGERPRINT_FILE);
        let stored = fs::read_to_string(&path).ok();
        if stored.as_deref().map(str::trim) == Some(fp) {
            return Ok(false);
        }
        let wiped = self.clear()?;
        fs::write(&path, fp)?;
        Ok(wiped)
    }

    /// Delete every cached tile. Returns whether there was any.
    fn clear(&self) -> io::Result<bool> {
        let mut i = self.inner.lock().map_err(|_| io::Error::other("cache lock poisoned"))?;
        let any = !i.lru.is_empty();
        for entry in fs::read_dir(&self.root)?.flatten() {
            let is_zoom_dir = entry.file_name().to_str().is_some_and(|s| s.parse::<u8>().is_ok());
            if is_zoom_dir && entry.path().is_dir() {
                fs::remove_dir_all(entry.path())?;
            }
        }
        i.lru.clear();
        i.total = 0;
        self.bytes.store(0, Ordering::Relaxed);
        Ok(any)
    }

    pub fn total_bytes(&self) -> u64 {
        self.inner.lock().map(|i| i.total).unwrap_or(0)
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map(|i| i.lru.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn path(&self, k: TileKey) -> PathBuf {
        self.root.join(k.z.to_string()).join(k.x.to_string()).join(format!("{}.mvt", k.y))
    }

    /// Cached bytes (empty = known-absent tile), promoting the entry.
    pub fn get(&self, k: TileKey) -> Option<Vec<u8>> {
        let known = self.inner.lock().ok()?.lru.get(&k).is_some();
        if !known {
            return None;
        }
        let path = self.path(k);
        match fs::read(&path) {
            Ok(b) => {
                // Persist recency for the next session (best effort).
                if let Ok(f) = fs::File::options().write(true).open(&path) {
                    let _ = f.set_modified(SystemTime::now());
                }
                Some(b)
            }
            Err(_) => {
                // File vanished: forget it.
                if let Ok(mut i) = self.inner.lock() {
                    if let Some(len) = i.lru.pop(&k) {
                        i.total -= len;
                        self.bytes.store(i.total, Ordering::Relaxed);
                    }
                }
                None
            }
        }
    }

    /// Store bytes atomically (tmp + rename) and evict down to the cap.
    pub fn put(&self, k: TileKey, data: &[u8]) -> io::Result<()> {
        let path = self.path(k);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let n = self.tmp_counter.fetch_add(1, Ordering::Relaxed);
        let tmp = path.with_extension(format!("mvt.tmp{n}"));
        fs::write(&tmp, data)?;
        fs::rename(&tmp, &path)?;
        if let Ok(mut i) = self.inner.lock() {
            if let Some(old) = i.lru.put(k, data.len() as u64) {
                i.total -= old;
            }
            i.total += data.len() as u64;
            self.bytes.store(i.total, Ordering::Relaxed);
        }
        self.evict();
        Ok(())
    }

    fn evict(&self) {
        let Ok(mut i) = self.inner.lock() else { return };
        while i.total > self.cap {
            let Some((k, len)) = i.lru.pop_lru() else { break };
            i.total -= len;
            let _ = fs::remove_file(self.path(k));
        }
        self.bytes.store(i.total, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------------------
// Fetcher
// ---------------------------------------------------------------------------------------

type Reader = AsyncPmTilesReader<HttpBackend, HashMapCache>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Disk,
    Network,
}

struct ReaderSlot {
    reader: Option<Arc<Reader>>,
    last_failure: Option<(Instant, String)>,
    /// The archive does not match the tileset contract: permanent for this configuration.
    fatal: Option<String>,
}

#[derive(Default)]
struct Validation {
    done: bool,
    next_try: Option<Instant>,
}

pub struct Fetcher {
    url: String,
    client: reqwest::Client,
    reader: tokio::sync::Mutex<ReaderSlot>,
    cache_root: Option<PathBuf>,
    disk: tokio::sync::OnceCell<Option<Arc<DiskCache>>>,
    /// Whether the disk cache has been checked against the archive header.
    validation: tokio::sync::Mutex<Validation>,
    bytes: Arc<AtomicU64>,
    params: FetchParams,
    /// Messages for the main thread (logged there).
    notify: Box<dyn Fn(Note) + Send + Sync>,
}

/// Per-source cache subfolder so different archives never mix.
pub fn source_dir_name(url: &str) -> String {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in url.as_bytes() {
        h = (h ^ *b as u64).wrapping_mul(0x100_0000_01b3);
    }
    format!("pmtiles-v{CACHE_SCHEMA_VERSION}-{:016x}", splitmix64(h))
}

/// Delete cache folders of older schema versions of the same source next to `root`
/// (`pmtiles-v{N}-{hash}` with the same hash); they are outside the LRU cap otherwise.
/// Returns how many were removed.
pub fn remove_stale_versions(root: &Path) -> usize {
    let (Some(parent), Some(name)) = (root.parent(), root.file_name().and_then(|n| n.to_str())) else { return 0 };
    let Some(hash) = name.rsplit('-').next() else { return 0 };
    let Ok(rd) = std::fs::read_dir(parent) else { return 0 };
    let mut n = 0;
    for e in rd.flatten() {
        let fname = e.file_name();
        let Some(f) = fname.to_str() else { continue };
        let stale = f != name
            && f.strip_prefix("pmtiles-v")
                .and_then(|rest| rest.split_once('-'))
                .is_some_and(|(v, h)| h == hash && v.parse::<u32>().is_ok_and(|v| v < CACHE_SCHEMA_VERSION));
        if stale && e.path().is_dir() && std::fs::remove_dir_all(e.path()).is_ok() {
            n += 1;
        }
    }
    n
}

/// Identity of an archive build: a hash of its whole header (root directory offset/length,
/// data length, tile counts, bounds...), which changes whenever planetiler writes a new file.
fn header_fingerprint(h: &pmtiles::Header) -> String {
    let mut x = 0xcbf2_9ce4_8422_2325u64;
    for b in format!("{h:?}").as_bytes() {
        x = (x ^ *b as u64).wrapping_mul(0x100_0000_01b3);
    }
    format!("{:016x}", splitmix64(x))
}

impl Fetcher {
    pub fn new(
        url: &str,
        cache_dir: &str,
        bytes: Arc<AtomicU64>,
        params: FetchParams,
        notify: Box<dyn Fn(Note) + Send + Sync>,
    ) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .connect_timeout(params.connect_timeout)
            .timeout(params.request_timeout)
            .user_agent(concat!("simplane_world/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        let cache_root =
            (!cache_dir.is_empty()).then(|| Path::new(cache_dir).join(source_dir_name(url)));
        Ok(Self {
            url: url.to_string(),
            client,
            reader: tokio::sync::Mutex::new(ReaderSlot { reader: None, last_failure: None, fatal: None }),
            cache_root,
            disk: tokio::sync::OnceCell::new(),
            validation: tokio::sync::Mutex::new(Validation::default()),
            bytes,
            params,
            notify,
        })
    }

    fn info(&self, s: String) {
        (self.notify)(Note::Info(s));
    }

    async fn disk(&self) -> Option<Arc<DiskCache>> {
        self.disk
            .get_or_init(|| async {
                let root = self.cache_root.clone()?;
                let bytes = self.bytes.clone();
                let cap = self.params.cache_cap_bytes;
                let r = tokio::task::spawn_blocking(move || {
                    let stale = remove_stale_versions(&root);
                    DiskCache::open(root, cap, bytes).map(|c| (c, stale))
                })
                .await;
                match r {
                    Ok(Ok((c, stale))) => {
                        if stale > 0 {
                            self.info(format!("disk cache: removed {stale} folder(s) of an older cache schema"));
                        }
                        self.info(format!(
                            "disk cache {} : {} tiles, {:.1} MB",
                            c.root().display(),
                            c.len(),
                            c.total_bytes() as f64 / 1048576.0
                        ));
                        Some(Arc::new(c))
                    }
                    Ok(Err(e)) => {
                        self.info(format!("disk cache disabled: {e}"));
                        None
                    }
                    Err(e) => {
                        self.info(format!("disk cache disabled: {e}"));
                        None
                    }
                }
            })
            .await
            .clone()
    }

    async fn reader(&self) -> Result<Arc<Reader>, String> {
        let mut slot = self.reader.lock().await;
        if let Some(e) = &slot.fatal {
            return Err(e.clone());
        }
        if let Some(r) = &slot.reader {
            return Ok(r.clone());
        }
        if let Some((t, e)) = &slot.last_failure {
            if t.elapsed() < self.params.open_retry {
                return Err(e.clone());
            }
        }
        match Reader::new_with_cached_url(HashMapCache::default(), self.client.clone(), &self.url).await {
            Ok(r) => {
                let h = r.get_header();
                let (min_zoom, max_zoom) = (h.min_zoom, h.max_zoom);
                self.info(format!(
                    "opened {} : z{}-{} tiles={:?} compression={:?}",
                    self.url, h.min_zoom, h.max_zoom, h.n_addressed_tiles(), h.tile_compression
                ));
                // Tileset contract (A18). A metadata read error is transient (retry); a
                // mismatch is permanent for this configuration.
                let meta = match r.get_metadata().await {
                    Ok(m) => m,
                    Err(e) => {
                        let msg = format!("open {}: metadata: {e}", self.url);
                        slot.last_failure = Some((Instant::now(), msg.clone()));
                        return Err(msg);
                    }
                };
                if let Err(e) = check_tileset(min_zoom, max_zoom, &meta) {
                    let msg = format!("{}: {e}", self.url);
                    (self.notify)(Note::Error(msg.clone()));
                    slot.fatal = Some(msg.clone());
                    return Err(msg);
                }
                let r = Arc::new(r);
                slot.reader = Some(r.clone());
                slot.last_failure = None;
                Ok(r)
            }
            Err(e) => {
                let msg = format!("open {}: {e}", self.url);
                slot.last_failure = Some((Instant::now(), msg.clone()));
                Err(msg)
            }
        }
    }

    /// Compare the disk cache with the archive that is currently served and wipe it if they
    /// differ. Needs the archive open; when it is unreachable the cache is used as is and
    /// the check is retried every `FetchParams::validate_retry`.
    async fn validate_cache(&self, disk: &Arc<DiskCache>) {
        let mut v = self.validation.lock().await;
        if v.done || v.next_try.is_some_and(|t| Instant::now() < t) {
            return;
        }
        let Ok(reader) = self.reader().await else {
            v.next_try = Some(Instant::now() + self.params.validate_retry);
            return;
        };
        let fp = header_fingerprint(reader.get_header());
        let d = disk.clone();
        match tokio::task::spawn_blocking(move || d.sync_fingerprint(&fp)).await {
            Ok(Ok(true)) => self.info("archive changed: disk cache cleared".to_string()),
            Ok(Ok(false)) => {}
            Ok(Err(e)) => self.info(format!("disk cache fingerprint: {e}")),
            Err(e) => self.info(format!("disk cache fingerprint: {e}")),
        }
        v.done = true;
    }

    /// Raw tile bytes (as archived; empty = no tile in the archive) and where they came from.
    pub async fn fetch(&self, key: TileKey) -> Result<(Bytes, Source), String> {
        let disk = self.disk().await;
        if let Some(d) = &disk {
            self.validate_cache(d).await;
        }
        if let Some(d) = disk.clone() {
            let hit = tokio::task::spawn_blocking(move || d.get(key)).await.ok().flatten();
            if let Some(b) = hit {
                return Ok((Bytes::from(b), Source::Disk));
            }
        }
        let reader = self.reader().await?;
        let coord = TileCoord::new(key.z, key.x, key.y).map_err(|e| format!("{key}: {e}"))?;
        let tile = match reader.get_header().tile_compression {
            Compression::Gzip | Compression::None | Compression::Unknown => reader.get_tile(coord).await,
            _ => reader.get_tile_decompressed(coord).await,
        };
        let tile = match tile {
            Ok(t) => t.unwrap_or_default(),
            Err(e) => {
                let msg = e.to_string();
                // A changed archive (new ETag) or broken connection: reopen next time.
                if msg.contains("changed") {
                    self.reader.lock().await.reader = None;
                    self.validation.lock().await.done = false;
                }
                return Err(format!("{key}: {msg}"));
            }
        };
        if let Some(d) = disk {
            let data = tile.clone();
            let _ = tokio::task::spawn_blocking(move || d.put(key, &data)).await;
        }
        Ok((tile, Source::Network))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("simplane-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn disk_cache_put_get_survives_reopen() {
        let dir = tmpdir("reopen");
        let ctr = Arc::new(AtomicU64::new(0));
        let k = TileKey::new(16, 37370, 20827);
        {
            let c = DiskCache::open(dir.clone(), 1 << 20, ctr.clone()).unwrap();
            assert!(c.get(k).is_none());
            c.put(k, b"hello").unwrap();
            c.put(TileKey::new(16, 1, 1), b"").unwrap(); // known-absent tile
            assert_eq!(c.get(k).as_deref(), Some(&b"hello"[..]));
            assert_eq!(ctr.load(Ordering::Relaxed), 5);
        }
        let c = DiskCache::open(dir.clone(), 1 << 20, ctr.clone()).unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(c.get(k).as_deref(), Some(&b"hello"[..]));
        assert_eq!(c.get(TileKey::new(16, 1, 1)).as_deref(), Some(&b""[..]));
        assert!(dir.join("16/37370/20827.mvt").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn disk_cache_evicts_least_recently_used() {
        let dir = tmpdir("evict");
        let ctr = Arc::new(AtomicU64::new(0));
        let c = DiskCache::open(dir.clone(), 250, ctr.clone()).unwrap();
        let a = TileKey::new(14, 1, 1);
        let b = TileKey::new(14, 1, 2);
        let d = TileKey::new(14, 1, 3);
        c.put(a, &[0u8; 100]).unwrap();
        c.put(b, &[0u8; 100]).unwrap();
        assert!(c.get(a).is_some()); // a is now most recent
        c.put(d, &[0u8; 100]).unwrap(); // over cap -> evict b
        assert!(c.get(b).is_none());
        assert!(c.get(a).is_some() && c.get(d).is_some());
        assert_eq!(c.total_bytes(), 200);
        assert_eq!(ctr.load(Ordering::Relaxed), 200);
        assert!(!dir.join("14/1/2.mvt").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn older_schema_folders_of_the_same_source_are_removed() {
        let base = std::env::temp_dir().join(format!("simplane-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let url = "http://127.0.0.1:8787/vilnius.pmtiles";
        let cur = source_dir_name(url);
        let hash = cur.rsplit('-').next().unwrap();
        let old = base.join(format!("pmtiles-v1-{hash}"));
        let other = base.join("pmtiles-v1-0123456789abcdef");
        for d in [&old, &other, &base.join(&cur)] {
            std::fs::create_dir_all(d.join("16/1")).unwrap();
        }
        assert!(cur.starts_with(&format!("pmtiles-v{CACHE_SCHEMA_VERSION}-")));
        assert_eq!(remove_stale_versions(&base.join(&cur)), 1);
        assert!(!old.exists() && other.exists() && base.join(&cur).exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn fingerprint_change_wipes_cache() {
        let dir = tmpdir("fingerprint");
        let ctr = Arc::new(AtomicU64::new(0));
        let k = TileKey::new(16, 5, 6);
        let c = DiskCache::open(dir.clone(), 1 << 20, ctr.clone()).unwrap();
        c.put(k, b"old").unwrap();
        // Tiles from before fingerprints existed are of unknown origin.
        assert!(c.sync_fingerprint("aaaa").unwrap());
        assert!(c.get(k).is_none() && c.is_empty());
        assert_eq!(ctr.load(Ordering::Relaxed), 0);
        c.put(k, b"new").unwrap();
        assert!(!c.sync_fingerprint("aaaa").unwrap());
        assert_eq!(c.get(k).as_deref(), Some(&b"new"[..]));
        // Survives a restart with the same archive, wiped for a rebuilt one.
        drop(c);
        let c = DiskCache::open(dir.clone(), 1 << 20, ctr.clone()).unwrap();
        assert!(!c.sync_fingerprint("aaaa").unwrap());
        assert!(c.get(k).is_some());
        assert!(c.sync_fingerprint("bbbb").unwrap());
        assert!(c.get(k).is_none());
        assert!(!dir.join("16").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn source_dirs_differ_per_url() {
        assert_ne!(source_dir_name("http://a/x.pmtiles"), source_dir_name("http://a/y.pmtiles"));
        assert_eq!(source_dir_name("u"), source_dir_name("u"));
    }

    #[test]
    fn tileset_contract_check() {
        let meta = r#"{"vector_layers":[{"id":"buildings"},{"id":"landuse"},{"id":"roads"}],"name":"x"}"#;
        assert!(check_tileset(13, 16, meta).is_ok());
        assert!(check_tileset(0, 18, meta).is_ok());
        let e = check_tileset(14, 16, meta).unwrap_err();
        assert!(e.contains("z14-16") && e.contains("z13-16"), "{e}");
        assert!(check_tileset(13, 15, meta).is_err());
        let e = check_tileset(13, 16, r#"{"vector_layers":[{"id":"buildings"},{"id":"water"}]}"#).unwrap_err();
        assert!(e.contains("landuse, roads") && e.contains("has: buildings, water"), "{e}");
        assert!(check_tileset(13, 16, "{}").unwrap_err().contains("vector_layers"));
        assert!(check_tileset(13, 16, "not json").is_err());
        assert_eq!(FetchParams::default().cache_cap_bytes, 300 * 1024 * 1024);
        assert_eq!(FetchParams::default().open_retry, Duration::from_millis(1500));
    }
}
