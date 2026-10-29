//! The disk cache handle ([`DiskCache`]) and its tuning; the connection lives on the cache
//! thread (`cache_thread`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use bytes::Bytes;

use crate::geo::TileKey;
use crate::mvt::splitmix64;

use super::cache_thread::CacheThread;
use super::Note;

// ---------------------------------------------------------------------------------------
// Disk cache: one SQLite database per source (docs/spec-tile-cache.md)
// ---------------------------------------------------------------------------------------
//
// One dedicated thread owns the connection; `DiskCache` handles talk to it over a channel.
// Writes (puts and access times) are batched in memory and committed in one transaction every
// `FLUSH_INTERVAL` or after `FLUSH_PENDING` puts. A hit only records its access time in memory.
// The byte total of the tile data is read once at open and then kept in memory; going over the
// cap evicts the oldest `atime` first down to 90 % and gives the pages back with
// `incremental_vacuum`.

/// Commit pending writes at least this often.
pub(super) const FLUSH_INTERVAL: Duration = Duration::from_millis(250);
/// ... or as soon as this many puts are pending.
pub(super) const FLUSH_PENDING: usize = 64;
/// Eviction low-water mark (fraction of the cap).
pub(super) const EVICT_LOW_WATER: f64 = 0.9;
/// After a failed write, evict down to this fraction of the cap.
pub(super) const EVICT_ON_FAILURE: f64 = 0.5;
/// Eviction candidates read per query.
pub(super) const EVICT_BATCH: i64 = 256;
/// Pages released per `incremental_vacuum` call (keeps one call short).
pub(super) const MAX_VACUUM_PAGES: i64 = 8192;
/// File size above cap x this = fragmentation: release every free page.
pub(super) const FRAGMENTATION_LIMIT: f64 = 1.25;
/// Pages in use (file minus free pages) may exceed the cap by this fraction plus
/// `FILE_SLACK`; beyond that more tiles are evicted. Tiles arrive in flight order, i.e. at
/// random places of the key B-tree, so pages end up ~80 % full: without this bound a cache
/// holding 300 MB of tile data occupies ~360 MB (T3 benchmark). With it the file (+ WAL)
/// stays within ~110 % of the cap and `cache_mb` (tile bytes) peaks around 85 % of it.
pub(super) const FILE_OVER_CAP: f64 = 0.05;
/// Fixed allowance for the bound above, so tiny caps (tests, a few MB) are bound by tile
/// bytes only (schema and index pages dominate there).
pub(super) const FILE_SLACK: u64 = 1 << 20;
/// Consecutive failed writes that disable the disk cache for the session.
pub(super) const MAX_WRITE_FAILURES: u32 = 3;
/// Info line of a corrupt database that was deleted and recreated.
pub const MSG_CACHE_RECREATED: &str = "disk cache was corrupt, recreated";

/// Sink for cache / fetcher messages (shared with the cache thread).
pub type Notify = Arc<dyn Fn(Note) + Send + Sync>;

/// Packed tile key, the rowid of `tiles`: `(z << 58) | (x << 29) | y`.
pub fn pack_key(k: TileKey) -> i64 {
    ((k.z as i64) << 58) | ((k.x as i64) << 29) | k.y as i64
}

/// Hash of a source URL (16 hex digits); names the cache file so different archives never mix.
pub fn source_hash(url: &str) -> String {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in url.as_bytes() {
        h = (h ^ *b as u64).wrapping_mul(0x100_0000_01b3);
    }
    format!("{:016x}", splitmix64(h))
}

/// Cache database file name of a source: `tiles-{hash}.sqlite`.
pub fn cache_file_name(url: &str) -> String {
    format!("tiles-{}.sqlite", source_hash(url))
}

/// Delete the file-per-tile folders of earlier cache versions of the same source next to the
/// database `db` (`pmtiles-v{N}-{hash}/`, and the unversioned `pmtiles-{hash}/` of the first
/// spike builds, with the hash of `tiles-{hash}.sqlite`). Their content is not migrated.
/// Returns how many were removed.
pub fn remove_legacy_folders(db: &Path) -> usize {
    let (Some(parent), Some(name)) = (db.parent(), db.file_name().and_then(|n| n.to_str())) else { return 0 };
    let Some(hash) = name.strip_prefix("tiles-").and_then(|n| n.strip_suffix(".sqlite")) else { return 0 };
    let Ok(rd) = fs::read_dir(parent) else { return 0 };
    let mut n = 0;
    for e in rd.flatten() {
        let fname = e.file_name();
        let Some(f) = fname.to_str() else { continue };
        let Some(rest) = f.strip_prefix("pmtiles-") else { continue };
        let legacy = rest == hash
            || rest
                .strip_prefix('v')
                .and_then(|r| r.split_once('-'))
                .is_some_and(|(v, h)| h == hash && v.parse::<u32>().is_ok());
        if legacy && e.path().is_dir() && fs::remove_dir_all(e.path()).is_ok() {
            n += 1;
        }
    }
    n
}

/// State shared by the handles and the cache thread.
pub(super) struct Shared {
    /// Byte total of the tile data (also `debug_stats().cache_mb`).
    pub(super) bytes: Arc<AtomicU64>,
    pub(super) count: AtomicU64,
    /// Disabled for the session (repeated write failures, or a failed re-create).
    pub(super) disabled: AtomicBool,
    /// Committed write batches (test hook: reads must not commit).
    pub(super) commits: AtomicU64,
    pub(super) notify: Mutex<Option<Notify>>,
    /// Test hooks: a fixed clock (UNIX seconds; 0 = the real clock); make batch writes fail.
    pub(super) test_clock_s: AtomicI64,
    pub(super) fail_writes: AtomicBool,
}

impl Shared {
    pub(super) fn note(&self, n: Note) {
        if let Ok(g) = self.notify.lock() {
            if let Some(f) = g.as_ref() {
                f(n);
            }
        }
    }

    pub(super) fn now(&self) -> i64 {
        match self.test_clock_s.load(Ordering::Relaxed) {
            0 => SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64),
            t => t,
        }
    }
}

pub(super) enum Cmd {
    Get(i64, tokio::sync::oneshot::Sender<Option<Vec<u8>>>),
    Put(i64, Bytes),
    Sync(String, tokio::sync::oneshot::Sender<io::Result<bool>>),
    /// Read / write one `meta` key (the archive's data levels).
    GetMeta(&'static str, tokio::sync::oneshot::Sender<Option<String>>),
    SetMeta(&'static str, String),
    /// Whether any tile is cached (offline level inference).
    HasTiles(tokio::sync::oneshot::Sender<bool>),
    Clear,
    Flush(crossbeam_channel::Sender<()>),
    /// Flush, checkpoint, close the file and end the thread; reply when done (if asked).
    Close(Option<crossbeam_channel::Sender<()>>),
}

/// Handle to the disk cache thread. Cheap calls: everything is a message to the thread. The
/// last handle dropped signals `Close` (no wait); [`DiskCache::close`] waits, bounded.
pub struct DiskCache {
    tx: crossbeam_channel::Sender<Cmd>,
    pub(super) shared: Arc<Shared>,
    path: PathBuf,
}

impl DiskCache {
    /// Open (or create) the cache database at `path` with a byte cap. Blocks until the cache
    /// thread has opened the file (call it off the main thread). A file held by another
    /// connection fails with `ErrorKind::ResourceBusy` ("in use").
    pub fn open(path: PathBuf, cap: u64, bytes: Arc<AtomicU64>) -> io::Result<Self> {
        Self::open_with(path, cap, bytes, None)
    }

    /// [`DiskCache::open`] with a message sink (corruption recovery, write failures, legacy
    /// folder removal).
    pub fn open_with(path: PathBuf, cap: u64, bytes: Arc<AtomicU64>, notify: Option<Notify>) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let shared = Arc::new(Shared {
            bytes,
            count: AtomicU64::new(0),
            disabled: AtomicBool::new(false),
            commits: AtomicU64::new(0),
            notify: Mutex::new(notify),
            test_clock_s: AtomicI64::new(0),
            fail_writes: AtomicBool::new(false),
        });
        let (tx, rx) = crossbeam_channel::unbounded();
        let (ready_tx, ready_rx) = crossbeam_channel::bounded::<io::Result<()>>(1);
        let (p, sh) = (path.clone(), shared.clone());
        std::thread::Builder::new().name("simworld-cache".into()).spawn(move || {
            match CacheThread::open(p, cap, sh) {
                Ok(mut t) => {
                    let _ = ready_tx.send(Ok(()));
                    t.run(rx);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        })?;
        ready_rx.recv().map_err(|_| io::Error::other("disk cache thread ended during open"))??;
        Ok(Self { tx, shared, path })
    }

    /// The database file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The byte counter this cache updates (`debug_stats().cache_mb`).
    pub fn bytes_counter(&self) -> Arc<AtomicU64> {
        self.shared.bytes.clone()
    }

    /// Route messages to a new sink (a re-configured world reusing this cache).
    pub fn set_notify(&self, notify: Option<Notify>) {
        if let Ok(mut g) = self.shared.notify.lock() {
            *g = notify;
        }
    }

    /// Disabled for this session after repeated write failures (gets miss, puts are dropped).
    pub fn is_disabled(&self) -> bool {
        self.shared.disabled.load(Ordering::Relaxed)
    }

    /// Cached bytes (empty = known-absent tile). A miss on any error.
    pub async fn get(&self, k: TileKey) -> Option<Vec<u8>> {
        if self.is_disabled() {
            return None;
        }
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tx.send(Cmd::Get(pack_key(k), tx)).ok()?;
        rx.await.ok().flatten()
    }

    /// Store tile bytes (fire and forget; committed with the next batch, visible to `get` at once).
    pub fn put(&self, k: TileKey, data: Bytes) {
        if !self.is_disabled() {
            let _ = self.tx.send(Cmd::Put(pack_key(k), data));
        }
    }

    /// Record `fp` as the archive this cache belongs to. If the cache was filled from a
    /// different archive (or from an unknown one, with tiles present) every tile is deleted.
    /// Returns true when tiles were wiped.
    pub async fn sync_fingerprint(&self, fp: &str) -> io::Result<bool> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tx.send(Cmd::Sync(fp.to_string(), tx)).map_err(|_| io::Error::other("disk cache closed"))?;
        rx.await.map_err(|_| io::Error::other("disk cache closed"))?
    }

    /// A value of the `meta` table (`None` when absent or on any error).
    pub async fn get_meta(&self, k: &'static str) -> Option<String> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.tx.send(Cmd::GetMeta(k, tx)).ok()?;
        rx.await.ok().flatten()
    }

    /// Whether the cache holds any tile (pending writes included).
    pub async fn has_tiles(&self) -> bool {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self.tx.send(Cmd::HasTiles(tx)).is_err() {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    /// Set a value of the `meta` table (fire and forget).
    pub fn set_meta(&self, k: &'static str, v: String) {
        if !self.is_disabled() {
            let _ = self.tx.send(Cmd::SetMeta(k, v));
        }
    }

    /// Delete every cached tile (asynchronously).
    pub fn clear(&self) {
        let _ = self.tx.send(Cmd::Clear);
    }

    /// Commit pending writes now; waits at most `wait`. False on timeout or a closed cache.
    pub fn flush(&self, wait: Duration) -> bool {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.tx.send(Cmd::Flush(tx)).is_ok() && rx.recv_timeout(wait).is_ok()
    }

    /// Flush, checkpoint and close the file, waiting at most `wait`. True when the file is
    /// closed (or already was). Later calls on any handle are misses / no-ops.
    pub fn close(&self, wait: Duration) -> bool {
        let (tx, rx) = crossbeam_channel::bounded(1);
        if self.tx.send(Cmd::Close(Some(tx))).is_err() {
            return true;
        }
        match rx.recv_timeout(wait) {
            Ok(()) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => true,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => false,
        }
    }

    /// Ask the thread to flush and close, without waiting.
    pub fn signal_close(&self) {
        let _ = self.tx.send(Cmd::Close(None));
    }

    /// Bytes of tile data (committed).
    pub fn total_bytes(&self) -> u64 {
        self.shared.bytes.load(Ordering::Relaxed)
    }

    /// Number of tiles (committed).
    pub fn len(&self) -> usize {
        self.shared.count.load(Ordering::Relaxed) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Committed write batches since open (test hook).
    #[doc(hidden)]
    pub fn commit_count(&self) -> u64 {
        self.shared.commits.load(Ordering::Relaxed)
    }
}

impl Drop for DiskCache {
    fn drop(&mut self) {
        // Background stop: pending writes are committed by the thread if it gets to them.
        self.signal_close();
    }
}
