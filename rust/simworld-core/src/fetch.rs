//! Tile bytes from a PMTiles v3 archive over HTTP(S) range requests, behind a disk LRU cache.
//!
//! * Network: the `pmtiles` crate (`AsyncPmTilesReader<HttpBackend, HashMapCache>`): header and
//!   root directory are read once when the reader opens, leaf directories are cached in memory,
//!   then each tile is one range request. reqwest uses rustls only; plain http:// also works.
//! * Disk: raw tile bytes exactly as stored in the archive (gzip MVT for planetiler) in one
//!   SQLite database per source, `{cache_dir}/tiles-{source-hash}.sqlite` (rows keyed by packed
//!   `z/x/y`; a zero-length blob records "no tile here"). Owned by one cache thread, writes
//!   batched, LRU by access time with a byte cap (world profile `cache.cap_mb`, default
//!   300 MiB), survives restarts (docs/spec-tile-cache.md). The cache is consulted before the
//!   archive is even opened, so a warm cache works fully offline.
//! * Staleness: the database's `meta` table holds the archive fingerprint (hash of the archive
//!   header) and the cache schema version. Once the archive has been opened, a different
//!   fingerprint wipes the cached tiles, so rebuilding the archive at the same URL never serves
//!   old tiles. While the archive is unreachable the cache is served as is (offline use) and
//!   validated as soon as it can be opened.
//!
//! * Tileset contract (audit A18, [`check_tileset`]): when the archive opens it must be either a
//!   two-level archive (metadata `simworld_levels` = `{"detail": 14, "overview": z}`, exactly
//!   those two zooms, layers `buildings` / `landuse` / `roads` + `blocks` / `lights`, optional
//!   `landmarks`; Tiles and LOD v2) or a compatibility archive (no `simworld_levels`, zooms
//!   covering 13..16, layers `buildings`, `landuse`, `roads`); otherwise every fetch fails with
//!   that error (reported once through [`Note::Error`]). The accepted levels
//!   ([`crate::data::DataLevels`]) are recorded in the disk cache's `meta` table, so a warm
//!   cache knows them offline ([`Fetcher::cached_levels`]).
//!
//! Everything here runs on tokio threads and the cache thread and never touches Godot.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use pmtiles::{AsyncPmTilesReader, Compression, HashMapCache, HttpBackend, TileCoord};

use crate::data::DataLevels;
use crate::geo::TileKey;
use crate::mvt::{splitmix64, BLOCKS_LAYER, BUILDINGS_LAYER, LANDMARKS_LAYER, LANDUSE_LAYER, LIGHTS_LAYER, ROADS_LAYER};
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
    /// Something the host should notice but that does not stop streaming (e.g. the disk cache
    /// is held by another process; running without it).
    Warn(String),
    /// The disk cache was disabled for the session (repeated write failures): status degraded.
    CacheDisabled(String),
    /// Configuration problem that will not go away by retrying (e.g. a tileset mismatch).
    Error(String),
}

/// Why a fetch failed (for the host status, contract `status()`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchErrorKind {
    /// The archive could not be opened (server unreachable, HTTP error on the header, ...).
    Unreachable,
    /// The archive opened but does not match the tileset contract (permanent).
    Contract,
    /// One tile request failed (or anything else).
    Tile,
    /// Aborted because the scheduler no longer wants the tile (`Worker::cancel`); not a failure.
    Cancelled,
}

/// A failed fetch: what kind of failure and the message that is logged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchError {
    pub kind: FetchErrorKind,
    pub message: String,
}

impl FetchError {
    pub fn new(kind: FetchErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Plain messages are tile-level failures.
impl From<String> for FetchError {
    fn from(message: String) -> Self {
        Self::new(FetchErrorKind::Tile, message)
    }
}

impl From<&str> for FetchError {
    fn from(message: &str) -> Self {
        Self::new(FetchErrorKind::Tile, message)
    }
}

/// The detail zoom a two-level archive must have (R0 z16 / R1 z15 are cut from it, R2 z14 from
/// the overview level).
pub const DETAIL_ZOOM: u8 = 14;
/// Overview zooms a two-level archive may use (R2 z14 / R3 z13 are cut from it).
pub const OVERVIEW_ZOOMS: std::ops::RangeInclusive<u8> = 8..=13;

/// Check an opened archive against the tileset contract and return its data levels (`metadata`
/// = the archive's JSON metadata): a two-level archive (`simworld_levels`, exactly its two
/// zooms, the detail and overview layers; `landmarks` optional) or a compatibility archive
/// (zooms 13..16, layers `buildings`, `landuse`, `roads`).
pub fn check_tileset(min_zoom: u8, max_zoom: u8, metadata: &str) -> Result<DataLevels, String> {
    let meta: serde_json::Value = serde_json::from_str(metadata).map_err(|e| format!("tileset metadata is not JSON: {e}"))?;
    let Some(layers) = meta.get("vector_layers").and_then(|v| v.as_array()) else {
        return Err("tileset metadata has no `vector_layers`: cannot verify the required layers (buildings, landuse, roads)".into());
    };
    let ids: Vec<&str> = layers.iter().filter_map(|l| l.get("id").and_then(|i| i.as_str())).collect();
    let need = |want: &[&'static str]| -> Result<(), String> {
        let missing: Vec<&str> = want.iter().copied().filter(|l| !ids.contains(l)).collect();
        if missing.is_empty() {
            return Ok(());
        }
        Err(format!(
            "tileset lacks required layer(s) {} (has: {}); build it with the planetiler profile in data/ or `simworld-cli bake`",
            missing.join(", "),
            ids.join(", ")
        ))
    };
    if let Some(l) = meta.get("simworld_levels") {
        let zoom = |k: &str| l.get(k).and_then(|v| v.as_u64()).filter(|z| *z <= 30).map(|z| z as u8);
        let (Some(detail), Some(overview)) = (zoom("detail"), zoom("overview")) else {
            return Err(format!("tileset metadata simworld_levels must be {{\"detail\": <zoom>, \"overview\": <zoom>}}, got {l}"));
        };
        if detail != DETAIL_ZOOM || !OVERVIEW_ZOOMS.contains(&overview) {
            return Err(format!(
                "tileset simworld_levels detail z{detail} / overview z{overview} not supported (the library needs detail z{DETAIL_ZOOM} and overview z{}..z{})",
                OVERVIEW_ZOOMS.start(),
                OVERVIEW_ZOOMS.end()
            ));
        }
        if (min_zoom, max_zoom) != (overview, detail) {
            return Err(format!(
                "tileset zoom range z{min_zoom}-{max_zoom} does not match its simworld_levels (exactly z{overview} and z{detail}); rebuild it with `simworld-cli bake`"
            ));
        }
        need(&[BUILDINGS_LAYER, LANDUSE_LAYER, ROADS_LAYER, BLOCKS_LAYER, LIGHTS_LAYER])?;
        return Ok(DataLevels::two_level(detail, overview, ids.contains(&LANDMARKS_LAYER)));
    }
    if min_zoom > ROOT_ZOOM || max_zoom < LEAF_ZOOM {
        return Err(format!(
            "tileset zoom range z{min_zoom}-{max_zoom} does not cover z{ROOT_ZOOM}-{LEAF_ZOOM} (the library's bands need every zoom from {ROOT_ZOOM} to {LEAF_ZOOM}, or a two-level archive with simworld_levels; rebuild the archive with data/build_tiles.sh or `simworld-cli bake`)"
        ));
    }
    need(&[BUILDINGS_LAYER, LANDUSE_LAYER, ROADS_LAYER])?;
    Ok(DataLevels::COMPAT)
}

/// `DataLevels` as stored in the disk cache's `meta` table (`levels`).
fn levels_text(l: DataLevels) -> String {
    format!("{},{},{},{}", l.detail, l.overview, l.two_level as u8, l.landmarks as u8)
}

fn parse_levels(s: &str) -> Option<DataLevels> {
    let v: Vec<u8> = s.split(',').map(|x| x.parse().ok()).collect::<Option<_>>()?;
    match v[..] {
        [detail, overview, two, lm] => Some(DataLevels { detail, overview, two_level: two == 1, landmarks: lm == 1 }),
        _ => None,
    }
}
/// Bump when the tile schema or the decoder's expectations change without a new archive URL.
/// v2 (M5 Ground): `landuse` gained leisure/natural/riverbank kinds and `roads` is decoded.
/// v3: one SQLite file per source (`tiles-{hash}.sqlite`, version in `meta.schema_version`);
/// the file-per-tile folders `pmtiles-v{N}-{hash}/` of the same source are deleted on open.
pub const CACHE_SCHEMA_VERSION: u32 = 3;

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
const FLUSH_INTERVAL: Duration = Duration::from_millis(250);
/// ... or as soon as this many puts are pending.
const FLUSH_PENDING: usize = 64;
/// Eviction low-water mark (fraction of the cap).
const EVICT_LOW_WATER: f64 = 0.9;
/// After a failed write, evict down to this fraction of the cap.
const EVICT_ON_FAILURE: f64 = 0.5;
/// Eviction candidates read per query.
const EVICT_BATCH: i64 = 256;
/// Pages released per `incremental_vacuum` call (keeps one call short).
const MAX_VACUUM_PAGES: i64 = 8192;
/// File size above cap x this = fragmentation: release every free page.
const FRAGMENTATION_LIMIT: f64 = 1.25;
/// Pages in use (file minus free pages) may exceed the cap by this fraction plus
/// `FILE_SLACK`; beyond that more tiles are evicted. Tiles arrive in flight order, i.e. at
/// random places of the key B-tree, so pages end up ~80 % full: without this bound a cache
/// holding 300 MB of tile data occupies ~360 MB (T3 benchmark). With it the file (+ WAL)
/// stays within ~110 % of the cap and `cache_mb` (tile bytes) peaks around 85 % of it.
const FILE_OVER_CAP: f64 = 0.05;
/// Fixed allowance for the bound above, so tiny caps (tests, a few MB) are bound by tile
/// bytes only (schema and index pages dominate there).
const FILE_SLACK: u64 = 1 << 20;
/// Consecutive failed writes that disable the disk cache for the session.
const MAX_WRITE_FAILURES: u32 = 3;
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

fn sidecar(db: &Path, suffix: &str) -> PathBuf {
    let mut s = db.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Delete a database and its `-wal` / `-shm` files.
fn delete_db_files(db: &Path) {
    for p in [db.to_path_buf(), sidecar(db, "-wal"), sidecar(db, "-shm"), sidecar(db, "-journal")] {
        let _ = fs::remove_file(p);
    }
}

fn sqlite_code(e: &rusqlite::Error) -> Option<rusqlite::ErrorCode> {
    e.sqlite_error_code()
}

fn is_corrupt(e: &rusqlite::Error) -> bool {
    matches!(sqlite_code(e), Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase))
}

fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(sqlite_code(e), Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
}

/// Run a statement to completion, ignoring rows (pragmas that return rows, `incremental_vacuum`
/// which frees pages step by step).
fn run_all(conn: &rusqlite::Connection, sql: &str) -> rusqlite::Result<()> {
    let mut st = conn.prepare(sql)?;
    let mut rows = st.query([])?;
    while rows.next()?.is_some() {}
    Ok(())
}

fn pragma_i64(conn: &rusqlite::Connection, name: &str) -> rusqlite::Result<i64> {
    conn.query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))
}

/// Open (or create) the database with the cache settings, take the exclusive lock, create the
/// schema, wipe tiles of another schema version. Returns the connection, byte total and count.
fn open_conn(path: &Path) -> rusqlite::Result<(rusqlite::Connection, u64, u64)> {
    use rusqlite::OptionalExtension;
    let conn = rusqlite::Connection::open(path)?;
    // rusqlite waits up to 5 s on a locked file by default; a held file means "in use" at once.
    conn.busy_timeout(Duration::ZERO)?;
    // Order matters: locking mode before WAL (no shared-memory index then), page size and
    // auto_vacuum before the first table exists (no-ops afterwards).
    run_all(&conn, "PRAGMA locking_mode = EXCLUSIVE")?;
    run_all(&conn, "PRAGMA page_size = 4096")?;
    run_all(&conn, "PRAGMA auto_vacuum = INCREMENTAL")?;
    run_all(&conn, "PRAGMA journal_mode = WAL")?;
    run_all(&conn, "PRAGMA synchronous = NORMAL")?;
    run_all(&conn, "PRAGMA temp_store = MEMORY")?;
    run_all(&conn, "PRAGMA cache_size = -8192")?;
    run_all(&conn, "PRAGMA mmap_size = 67108864")?;
    // Take (and, in EXCLUSIVE locking mode, keep) the write lock now: a second opener gets
    // SQLITE_BUSY here instead of later on a write.
    conn.execute_batch("BEGIN EXCLUSIVE")?;
    let setup = (|| -> rusqlite::Result<(u64, u64)> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL) WITHOUT ROWID;
             CREATE TABLE IF NOT EXISTS tiles (
               key   INTEGER PRIMARY KEY,
               data  BLOB    NOT NULL,
               size  INTEGER NOT NULL,
               atime INTEGER NOT NULL
             );
             DROP INDEX IF EXISTS tiles_atime;
             CREATE INDEX IF NOT EXISTS tiles_lru ON tiles(atime, key, size);",
        )?;
        // `tiles_lru` covers the byte total below and the eviction scan, so neither reads a
        // table page (the blobs): opening a full cache reads a few MB of index instead of the
        // whole file (T3 benchmark, 300 MB cache: 75-85 ms warm / ~0.9 s cold -> a few ms).
        // It replaced `tiles_atime(atime)` of the first T1/T2 builds (dropped above, same
        // schema version: nothing else changed).
        let version: Option<String> =
            conn.query_row("SELECT v FROM meta WHERE k = 'schema_version'", [], |r| r.get(0)).optional()?;
        let want = CACHE_SCHEMA_VERSION.to_string();
        if version.as_deref() != Some(want.as_str()) {
            conn.execute("DELETE FROM tiles", [])?;
            conn.execute("DELETE FROM meta WHERE k = 'fingerprint'", [])?;
            conn.execute("INSERT OR REPLACE INTO meta (k, v) VALUES ('schema_version', ?1)", [&want])?;
        }
        conn.query_row("SELECT COALESCE(SUM(size), 0), COUNT(*) FROM tiles", [], |r| {
            Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64))
        })
    })();
    match setup {
        Ok((total, count)) => {
            conn.execute_batch("COMMIT")?;
            Ok((conn, total, count))
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Evict the oldest `atime` first until `*total <= target` (inside the caller's transaction).
/// Returns the freed bytes.
fn evict_to(conn: &rusqlite::Connection, target: u64, total: &mut u64, count: &mut u64) -> rusqlite::Result<u64> {
    let mut freed = 0;
    let mut sel = conn.prepare_cached("SELECT key, size FROM tiles ORDER BY atime ASC, key ASC LIMIT ?1")?;
    let mut del = conn.prepare_cached("DELETE FROM tiles WHERE key = ?1")?;
    while *total > target {
        let rows: Vec<(i64, i64)> =
            sel.query_map([EVICT_BATCH], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        if rows.is_empty() {
            *total = 0;
            *count = 0;
            break;
        }
        for (k, size) in rows {
            if *total <= target {
                break;
            }
            del.execute([k])?;
            let size = size.max(0) as u64;
            *total = total.saturating_sub(size);
            *count = count.saturating_sub(1);
            freed += size;
        }
    }
    Ok(freed)
}

/// State shared by the handles and the cache thread.
struct Shared {
    /// Byte total of the tile data (also `debug_stats().cache_mb`).
    bytes: Arc<AtomicU64>,
    count: AtomicU64,
    /// Disabled for the session (repeated write failures, or a failed re-create).
    disabled: AtomicBool,
    /// Committed write batches (test hook: reads must not commit).
    commits: AtomicU64,
    notify: Mutex<Option<Notify>>,
    /// Test hooks: a fixed clock (UNIX seconds; 0 = the real clock); make batch writes fail.
    test_clock_s: AtomicI64,
    fail_writes: AtomicBool,
}

impl Shared {
    fn note(&self, n: Note) {
        if let Ok(g) = self.notify.lock() {
            if let Some(f) = g.as_ref() {
                f(n);
            }
        }
    }

    fn now(&self) -> i64 {
        match self.test_clock_s.load(Ordering::Relaxed) {
            0 => SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64),
            t => t,
        }
    }
}

enum Cmd {
    Get(i64, tokio::sync::oneshot::Sender<Option<Vec<u8>>>),
    Put(i64, Bytes),
    Sync(String, tokio::sync::oneshot::Sender<io::Result<bool>>),
    /// Read / write one `meta` key (the archive's data levels).
    GetMeta(&'static str, tokio::sync::oneshot::Sender<Option<String>>),
    SetMeta(&'static str, String),
    Clear,
    Flush(crossbeam_channel::Sender<()>),
    /// Flush, checkpoint, close the file and end the thread; reply when done (if asked).
    Close(Option<crossbeam_channel::Sender<()>>),
}

/// Handle to the disk cache thread. Cheap calls: everything is a message to the thread. The
/// last handle dropped signals `Close` (no wait); [`DiskCache::close`] waits, bounded.
pub struct DiskCache {
    tx: crossbeam_channel::Sender<Cmd>,
    shared: Arc<Shared>,
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

/// The cache thread: owns the connection.
struct CacheThread {
    path: PathBuf,
    cap: u64,
    shared: Arc<Shared>,
    /// `None` = disabled for the session.
    conn: Option<rusqlite::Connection>,
    pending: HashMap<i64, Bytes>,
    atimes: HashMap<i64, i64>,
    /// When the oldest pending write was queued.
    since: Option<Instant>,
    total: u64,
    count: u64,
    page_size: i64,
    failures: u32,
}

impl CacheThread {
    fn open(path: PathBuf, cap: u64, shared: Arc<Shared>) -> io::Result<Self> {
        let legacy = remove_legacy_folders(&path);
        if legacy > 0 {
            shared.note(Note::Info(format!("disk cache: removed {legacy} folder(s) of an older cache format")));
        }
        let opened = match open_conn(&path) {
            Err(e) if is_corrupt(&e) => {
                delete_db_files(&path);
                shared.note(Note::Info(format!("{MSG_CACHE_RECREATED} ({})", path.display())));
                open_conn(&path)
            }
            r => r,
        };
        let (conn, total, count) = match opened {
            Ok(c) => c,
            Err(e) if is_busy(&e) => {
                return Err(io::Error::new(io::ErrorKind::ResourceBusy, format!("{} is in use: {e}", path.display())))
            }
            Err(e) => return Err(io::Error::other(format!("{}: {e}", path.display()))),
        };
        let page_size = pragma_i64(&conn, "page_size").unwrap_or(4096).max(512);
        let mut t = Self {
            path,
            cap,
            shared,
            conn: Some(conn),
            pending: HashMap::new(),
            atimes: HashMap::new(),
            since: None,
            total,
            count,
            page_size,
            failures: 0,
        };
        t.publish();
        // A smaller cap than last session: evict now.
        if t.total > t.cap {
            t.evict_now(EVICT_LOW_WATER);
        }
        t.bound_file();
        Ok(t)
    }

    fn publish(&self) {
        self.shared.bytes.store(self.total, Ordering::Relaxed);
        self.shared.count.store(self.count, Ordering::Relaxed);
    }

    fn run(&mut self, rx: crossbeam_channel::Receiver<Cmd>) {
        use crossbeam_channel::RecvTimeoutError;
        loop {
            let msg = match self.since {
                Some(t) => rx.recv_timeout((t + FLUSH_INTERVAL).saturating_duration_since(Instant::now())),
                None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match msg {
                Ok(Cmd::Close(reply)) => {
                    self.close();
                    if let Some(r) = reply {
                        let _ = r.send(());
                    }
                    return;
                }
                Ok(cmd) => self.handle(cmd),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    self.close();
                    return;
                }
            }
            if self.pending.len() >= FLUSH_PENDING || self.since.is_some_and(|t| t.elapsed() >= FLUSH_INTERVAL) {
                self.commit();
            }
        }
    }

    fn mark_pending(&mut self) {
        if self.since.is_none() {
            self.since = Some(Instant::now());
        }
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Get(k, reply) => {
                let _ = reply.send(self.get(k));
            }
            Cmd::Put(k, data) => {
                if self.conn.is_some() {
                    self.atimes.remove(&k);
                    self.pending.insert(k, data);
                    self.mark_pending();
                }
            }
            Cmd::Sync(fp, reply) => {
                let _ = reply.send(self.sync(&fp));
            }
            Cmd::GetMeta(k, reply) => {
                use rusqlite::OptionalExtension;
                let v = self.conn.as_ref().and_then(|c| c.query_row("SELECT v FROM meta WHERE k = ?1", [k], |r| r.get(0)).optional().ok().flatten());
                let _ = reply.send(v);
            }
            Cmd::SetMeta(k, v) => {
                if let Some(c) = self.conn.as_ref() {
                    let _ = c.execute("INSERT OR REPLACE INTO meta (k, v) VALUES (?1, ?2)", [k, v.as_str()]);
                }
            }
            Cmd::Clear => {
                let _ = self.clear();
            }
            Cmd::Flush(reply) => {
                self.commit();
                let _ = reply.send(());
            }
            Cmd::Close(_) => unreachable!("handled in run"),
        }
    }

    fn get(&mut self, k: i64) -> Option<Vec<u8>> {
        use rusqlite::OptionalExtension;
        if let Some(b) = self.pending.get(&k) {
            return Some(b.to_vec());
        }
        let conn = self.conn.as_ref()?;
        let r = conn
            .prepare_cached("SELECT data, atime FROM tiles WHERE key = ?1")
            .and_then(|mut st| st.query_row([k], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?))).optional());
        match r {
            Ok(Some((data, atime))) => {
                // Reads never write: remember the access, flushed with the next batch.
                let now = self.shared.now();
                if atime != now {
                    self.atimes.insert(k, now);
                    self.mark_pending();
                }
                Some(data)
            }
            Ok(None) => None,
            Err(e) => {
                if is_corrupt(&e) {
                    self.recover();
                }
                None
            }
        }
    }

    /// Commit pending puts and access times in one transaction, evicting if over the cap.
    fn commit(&mut self) {
        self.since = None;
        if self.pending.is_empty() && self.atimes.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.pending);
        let atimes = std::mem::take(&mut self.atimes);
        let Some(conn) = self.conn.as_ref() else { return };
        let now = self.shared.now();
        let result = if self.shared.fail_writes.load(Ordering::Relaxed) {
            Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
                Some("test hook: write failure".into()),
            ))
        } else {
            write_batch(conn, &pending, &atimes, now, self.total, self.count, self.cap)
        };
        match result {
            Ok((total, count, freed)) => {
                self.failures = 0;
                self.total = total;
                self.count = count;
                self.shared.commits.fetch_add(1, Ordering::Relaxed);
                self.publish();
                if freed > 0 {
                    self.vacuum();
                }
                self.bound_file();
            }
            Err(e) if is_corrupt(&e) => self.recover(),
            Err(e) => self.write_failed(&e),
        }
    }

    /// A batch could not be written (disk full, I/O error): it is dropped; evict harder; after
    /// `MAX_WRITE_FAILURES` in a row the cache is disabled for the session.
    fn write_failed(&mut self, e: &rusqlite::Error) {
        self.failures += 1;
        if self.failures >= MAX_WRITE_FAILURES {
            self.disable();
            self.shared.note(Note::CacheDisabled(format!(
                "disk cache {} disabled for this session after {} failed writes: {e}",
                self.path.display(),
                self.failures
            )));
            return;
        }
        if self.total as f64 > self.cap as f64 * EVICT_ON_FAILURE {
            self.evict_now(EVICT_ON_FAILURE);
        }
    }

    fn disable(&mut self) {
        self.pending.clear();
        self.atimes.clear();
        self.since = None;
        self.conn = None;
        self.shared.disabled.store(true, Ordering::Relaxed);
    }

    /// Evict down to `fraction` of the cap in its own transaction, then vacuum.
    fn evict_now(&mut self, fraction: f64) {
        self.evict_bytes((self.cap as f64 * fraction) as u64);
    }

    /// Keep the pages in use within the cap plus `FILE_OVER_CAP` and `FILE_SLACK`: when the
    /// file is over, evict about the excess (tile bytes shrink roughly with the pages they use),
    /// aiming for the low-water mark of that bound.
    fn bound_file(&mut self) {
        let Some(conn) = self.conn.as_ref() else { return };
        let (Ok(pages), Ok(free)) = (pragma_i64(conn, "page_count"), pragma_i64(conn, "freelist_count")) else {
            return;
        };
        let used = (pages - free).max(0) as u64 * self.page_size as u64;
        let limit = (self.cap as f64 * (1.0 + FILE_OVER_CAP)) as u64 + FILE_SLACK;
        if used <= limit || self.total == 0 {
            return;
        }
        let target = self.total as f64 * (limit as f64 * EVICT_LOW_WATER / used as f64);
        self.evict_bytes(target as u64);
    }

    /// Evict down to `target` tile bytes in its own transaction, then vacuum.
    fn evict_bytes(&mut self, target: u64) {
        let Some(conn) = self.conn.as_ref() else { return };
        let (mut total, mut count) = (self.total, self.count);
        let r = conn.unchecked_transaction().and_then(|tx| {
            let freed = evict_to(&tx, target, &mut total, &mut count)?;
            tx.commit()?;
            Ok(freed)
        });
        match r {
            Ok(freed) => {
                self.total = total;
                self.count = count;
                self.publish();
                if freed > 0 {
                    self.vacuum();
                }
            }
            Err(e) if is_corrupt(&e) => self.recover(),
            Err(_) => {}
        }
    }

    /// Give freed pages back to the file system (bounded per call; everything when the file is
    /// far above the cap), then checkpoint so the main file shrinks.
    fn vacuum(&mut self) {
        let Some(conn) = self.conn.as_ref() else { return };
        let r = (|| -> rusqlite::Result<()> {
            let free = pragma_i64(conn, "freelist_count")?;
            if free > 0 {
                run_all(conn, &format!("PRAGMA incremental_vacuum({})", free.min(MAX_VACUUM_PAGES)))?;
            }
            let pages = pragma_i64(conn, "page_count")?;
            if (pages * self.page_size) as f64 > self.cap as f64 * FRAGMENTATION_LIMIT
                && pragma_i64(conn, "freelist_count")? > 0
            {
                run_all(conn, "PRAGMA incremental_vacuum")?;
            }
            run_all(conn, "PRAGMA wal_checkpoint(TRUNCATE)")
        })();
        if let Err(e) = r {
            if is_corrupt(&e) {
                self.recover();
            }
        }
    }

    fn sync(&mut self, fp: &str) -> io::Result<bool> {
        use rusqlite::OptionalExtension;
        let Some(conn) = self.conn.as_ref() else { return Ok(false) };
        let stored: Option<String> = match conn
            .query_row("SELECT v FROM meta WHERE k = 'fingerprint'", [], |r| r.get(0))
            .optional()
        {
            Ok(s) => s,
            Err(e) => {
                if is_corrupt(&e) {
                    self.recover();
                }
                return Err(io::Error::other(e.to_string()));
            }
        };
        if stored.as_deref() == Some(fp) {
            return Ok(false);
        }
        let wiped = self.clear()?;
        let conn = self.conn.as_ref().ok_or_else(|| io::Error::other("disk cache disabled"))?;
        conn.execute("INSERT OR REPLACE INTO meta (k, v) VALUES ('fingerprint', ?1)", [fp])
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(wiped)
    }

    /// Delete every tile (pending ones too) and release the pages. Returns whether there was any.
    fn clear(&mut self) -> io::Result<bool> {
        let any = self.count > 0 || !self.pending.is_empty();
        self.pending.clear();
        self.atimes.clear();
        self.since = None;
        let Some(conn) = self.conn.as_ref() else { return Ok(any) };
        if let Err(e) = conn.execute("DELETE FROM tiles", []) {
            if is_corrupt(&e) {
                self.recover();
                return Ok(any);
            }
            return Err(io::Error::other(e.to_string()));
        }
        self.total = 0;
        self.count = 0;
        self.publish();
        self.vacuum();
        Ok(any)
    }

    /// The database is corrupt: delete and recreate it (one info line). The cache is disposable.
    fn recover(&mut self) {
        self.conn = None;
        self.pending.clear();
        self.atimes.clear();
        self.since = None;
        delete_db_files(&self.path);
        match open_conn(&self.path) {
            Ok((c, total, count)) => {
                self.conn = Some(c);
                self.total = total;
                self.count = count;
                self.shared.note(Note::Info(format!("{MSG_CACHE_RECREATED} ({})", self.path.display())));
            }
            Err(e) => {
                self.total = 0;
                self.count = 0;
                self.disable();
                self.shared.note(Note::CacheDisabled(format!(
                    "disk cache {} was corrupt and could not be recreated: {e}",
                    self.path.display()
                )));
            }
        }
        self.publish();
    }

    /// Flush pending writes, checkpoint and close the file.
    fn close(&mut self) {
        self.commit();
        if let Some(c) = self.conn.take() {
            let _ = run_all(&c, "PRAGMA wal_checkpoint(TRUNCATE)");
            let _ = c.close();
        }
    }
}

/// One write batch: puts (with their size delta), access times, eviction to the low-water
/// mark when over the cap. Returns the new (total, count, freed bytes); nothing changes in
/// memory unless it commits.
fn write_batch(
    conn: &rusqlite::Connection,
    pending: &HashMap<i64, Bytes>,
    atimes: &HashMap<i64, i64>,
    now: i64,
    total: u64,
    count: u64,
    cap: u64,
) -> rusqlite::Result<(u64, u64, u64)> {
    use rusqlite::OptionalExtension;
    let tx = conn.unchecked_transaction()?;
    let (mut total, mut count) = (total, count);
    {
        let mut old = tx.prepare_cached("SELECT size FROM tiles WHERE key = ?1")?;
        let mut ins = tx.prepare_cached("INSERT OR REPLACE INTO tiles (key, data, size, atime) VALUES (?1, ?2, ?3, ?4)")?;
        for (k, data) in pending {
            let prev: Option<i64> = old.query_row([k], |r| r.get(0)).optional()?;
            ins.execute(rusqlite::params![k, &data[..], data.len() as i64, now])?;
            total = (total + data.len() as u64).saturating_sub(prev.unwrap_or(0).max(0) as u64);
            if prev.is_none() {
                count += 1;
            }
        }
        let mut up = tx.prepare_cached("UPDATE tiles SET atime = ?2 WHERE key = ?1")?;
        for (k, t) in atimes {
            up.execute([k, t])?;
        }
    }
    let freed = if total > cap {
        evict_to(&tx, (cap as f64 * EVICT_LOW_WATER) as u64, &mut total, &mut count)?
    } else {
        0
    };
    tx.commit()?;
    Ok((total, count, freed))
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
    /// The data levels the opened archive passed the tileset check with.
    levels: Option<DataLevels>,
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
    /// Cache database file (`None` = no disk cache).
    cache_path: Option<PathBuf>,
    disk: tokio::sync::OnceCell<Option<Arc<DiskCache>>>,
    /// The opened cache, set by the opening task itself, so a stop can close it even if the
    /// task awaiting the open was cancelled.
    opened: Arc<Mutex<Option<Arc<DiskCache>>>>,
    /// Whether the disk cache has been checked against the archive header.
    validation: tokio::sync::Mutex<Validation>,
    bytes: Arc<AtomicU64>,
    params: FetchParams,
    /// Messages for the main thread (logged there).
    notify: Notify,
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
        let cache_path = (!cache_dir.is_empty()).then(|| Path::new(cache_dir).join(cache_file_name(url)));
        Ok(Self {
            url: url.to_string(),
            client,
            reader: tokio::sync::Mutex::new(ReaderSlot { reader: None, levels: None, last_failure: None, fatal: None }),
            cache_path,
            disk: tokio::sync::OnceCell::new(),
            opened: Arc::new(Mutex::new(None)),
            validation: tokio::sync::Mutex::new(Validation::default()),
            bytes,
            params,
            notify: Arc::from(notify),
        })
    }

    /// Use an already open disk cache (a re-configure with the same source and folder keeps
    /// the handle instead of closing and reopening the file). Its messages now go to this
    /// fetcher's sink and its byte counter replaces the one passed to `new`.
    pub fn with_disk_cache(mut self, cache: Arc<DiskCache>) -> Self {
        cache.set_notify(Some(self.notify.clone()));
        self.bytes = cache.bytes_counter();
        if let Ok(mut g) = self.opened.lock() {
            *g = Some(cache.clone());
        }
        self.disk = tokio::sync::OnceCell::new_with(Some(Some(cache)));
        self
    }

    /// The disk cache, if it has been opened.
    pub fn disk_handle(&self) -> Option<Arc<DiskCache>> {
        self.opened.lock().ok().and_then(|g| g.clone())
    }

    /// Close the disk cache (flush, checkpoint, close the file), waiting at most `wait`. True
    /// when closed or never opened.
    pub fn close_disk(&self, wait: Duration) -> bool {
        self.disk_handle().is_none_or(|d| d.close(wait))
    }

    fn info(&self, s: String) {
        (self.notify)(Note::Info(s));
    }

    async fn disk(&self) -> Option<Arc<DiskCache>> {
        self.disk
            .get_or_init(|| async {
                let path = self.cache_path.clone()?;
                let bytes = self.bytes.clone();
                let cap = self.params.cache_cap_bytes;
                let (notify, slot) = (self.notify.clone(), self.opened.clone());
                // The open blocks until the cache thread has the file (fast; SUM over one
                // table, no directory walk).
                let r = tokio::task::spawn_blocking(move || {
                    let c = Arc::new(DiskCache::open_with(path, cap, bytes, Some(notify))?);
                    if let Ok(mut g) = slot.lock() {
                        *g = Some(c.clone());
                    }
                    Ok::<_, io::Error>(c)
                })
                .await;
                match r {
                    Ok(Ok(c)) => {
                        self.info(format!(
                            "disk cache {} : {} tiles, {:.1} MB",
                            c.path().display(),
                            c.len(),
                            c.total_bytes() as f64 / 1048576.0
                        ));
                        Some(c)
                    }
                    Ok(Err(e)) if e.kind() == io::ErrorKind::ResourceBusy => {
                        (self.notify)(Note::Warn(format!("disk cache {e}; running without a disk cache")));
                        None
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

    async fn reader(&self) -> Result<Arc<Reader>, FetchError> {
        use FetchErrorKind::{Contract, Unreachable};
        let mut slot = self.reader.lock().await;
        if let Some(e) = &slot.fatal {
            return Err(FetchError::new(Contract, e.clone()));
        }
        if let Some(r) = &slot.reader {
            return Ok(r.clone());
        }
        if let Some((t, e)) = &slot.last_failure {
            if t.elapsed() < self.params.open_retry {
                return Err(FetchError::new(Unreachable, e.clone()));
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
                        return Err(FetchError::new(Unreachable, msg));
                    }
                };
                let levels = match check_tileset(min_zoom, max_zoom, &meta) {
                    Ok(l) => l,
                    Err(e) => {
                        let msg = format!("{}: {e}", self.url);
                        (self.notify)(Note::Error(msg.clone()));
                        slot.fatal = Some(msg.clone());
                        return Err(FetchError::new(Contract, msg));
                    }
                };
                if levels.two_level {
                    self.info(format!(
                        "two-level archive: detail z{}, overview z{}{}",
                        levels.detail,
                        levels.overview,
                        if levels.landmarks { ", landmarks" } else { "" }
                    ));
                }
                let r = Arc::new(r);
                slot.reader = Some(r.clone());
                slot.levels = Some(levels);
                slot.last_failure = None;
                Ok(r)
            }
            Err(e) => {
                let msg = format!("open {}: {e}", self.url);
                slot.last_failure = Some((Instant::now(), msg.clone()));
                Err(FetchError::new(Unreachable, msg))
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
        match disk.sync_fingerprint(&fp).await {
            Ok(true) => self.info("archive changed: disk cache cleared".to_string()),
            Ok(false) => {}
            Err(e) => self.info(format!("disk cache fingerprint: {e}")),
        }
        v.done = true;
    }

    /// Open the archive (if not yet) and return the data levels it passed the tileset check
    /// with; recorded in the disk cache for offline starts ([`Fetcher::cached_levels`]).
    pub async fn open_levels(&self) -> Result<DataLevels, FetchError> {
        self.reader().await?;
        let levels = self.reader.lock().await.levels.ok_or_else(|| FetchError::new(FetchErrorKind::Unreachable, "archive not open"))?;
        if let Some(d) = self.disk().await {
            if d.get_meta("levels").await.as_deref() != Some(levels_text(levels).as_str()) {
                d.set_meta("levels", levels_text(levels));
            }
        }
        Ok(levels)
    }

    /// The data levels recorded by an earlier session in the disk cache (offline start).
    pub async fn cached_levels(&self) -> Option<DataLevels> {
        let d = self.disk().await?;
        parse_levels(&d.get_meta("levels").await?)
    }

    /// Minimum wait before re-opening an archive that failed to open.
    pub fn open_retry(&self) -> Duration {
        self.params.open_retry
    }

    /// Raw tile bytes (as archived; empty = no tile in the archive) and where they came from.
    pub async fn fetch(&self, key: TileKey) -> Result<(Bytes, Source), FetchError> {
        let disk = self.disk().await;
        if let Some(d) = &disk {
            self.validate_cache(d).await;
        }
        if let Some(d) = &disk {
            if let Some(b) = d.get(key).await {
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
                return Err(format!("{key}: {msg}").into());
            }
        };
        if let Some(d) = disk {
            d.put(key, tile.clone());
        }
        Ok((tile, Source::Network))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("simworld-cache-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn db_in(dir: &Path) -> PathBuf {
        dir.join(cache_file_name("http://127.0.0.1:8787/vilnius.pmtiles"))
    }

    fn open(path: &Path, cap: u64) -> DiskCache {
        DiskCache::open(path.to_path_buf(), cap, Arc::new(AtomicU64::new(0))).unwrap()
    }

    /// Run a future to completion (the cache's async calls never need a runtime's I/O driver).
    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
    }

    fn get(c: &DiskCache, k: TileKey) -> Option<Vec<u8>> {
        block(c.get(k))
    }

    fn file_bytes(path: &Path) -> u64 {
        [path.to_path_buf(), sidecar(path, "-wal")].iter().filter_map(|p| fs::metadata(p).ok()).map(|m| m.len()).sum()
    }

    const WAIT: Duration = Duration::from_secs(5);

    #[test]
    fn put_get_survives_reopen() {
        let dir = tmpdir("reopen");
        let db = db_in(&dir);
        let ctr = Arc::new(AtomicU64::new(0));
        let k = TileKey::new(16, 37370, 20827);
        let absent = TileKey::new(16, 1, 1);
        {
            let c = DiskCache::open(db.clone(), 1 << 20, ctr.clone()).unwrap();
            assert!(get(&c, k).is_none());
            c.put(k, Bytes::from_static(b"hello"));
            c.put(absent, Bytes::new()); // known-absent tile
            assert!(c.flush(WAIT));
            assert_eq!(get(&c, k).as_deref(), Some(&b"hello"[..]));
            assert_eq!(ctr.load(Ordering::Relaxed), 5);
            assert_eq!(c.len(), 2);
            assert!(c.close(WAIT));
        }
        assert!(db.exists() && !sidecar(&db, "-wal").exists(), "close checkpoints and removes the WAL");
        let c = DiskCache::open(db.clone(), 1 << 20, ctr.clone()).unwrap();
        assert_eq!((c.len(), c.total_bytes()), (2, 5));
        assert_eq!(get(&c, k).as_deref(), Some(&b"hello"[..]));
        assert_eq!(get(&c, absent).as_deref(), Some(&b""[..]));
        assert!(get(&c, TileKey::new(16, 1, 2)).is_none());
        assert!(c.close(WAIT));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pending_put_visible_to_get() {
        let dir = tmpdir("pending");
        let c = open(&db_in(&dir), 1 << 20);
        let k = TileKey::new(15, 100, 200);
        c.put(k, Bytes::from_static(b"fresh"));
        // Not committed yet (250 ms batch), but answered from the pending buffer.
        assert_eq!(get(&c, k).as_deref(), Some(&b"fresh"[..]));
        assert_eq!(c.commit_count(), 0);
        assert_eq!(c.len(), 0, "len counts committed tiles");
        // A newer put of the same key wins.
        c.put(k, Bytes::from_static(b"newer"));
        assert_eq!(get(&c, k).as_deref(), Some(&b"newer"[..]));
        assert!(c.flush(WAIT));
        assert_eq!((c.len(), c.total_bytes()), (1, 5));
        assert!(c.close(WAIT));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn evicts_oldest_by_bytes() {
        let dir = tmpdir("evict");
        let cap = 64 * 1024;
        let c = open(&db_in(&dir), cap);
        let key = |i: u32| TileKey::new(16, 1000 + i, 2000);
        let one_kb = Bytes::from(vec![7u8; 1024]);
        // One second per 10 tiles written; after each 10, tiles 0..10 are read again (kept
        // hot). Written-only tiles age; the hot ones were written first but read last.
        for i in 0..100u32 {
            c.shared.test_clock_s.store(1_000_000 + i as i64 / 10, Ordering::Relaxed);
            c.put(key(i), one_kb.clone());
            if i % 10 == 9 {
                assert!(c.flush(WAIT));
                for h in 0..10 {
                    assert!(get(&c, key(h)).is_some(), "hot tile {h} gone after {i} puts");
                }
                assert!(c.flush(WAIT));
                assert!(c.total_bytes() <= cap, "{} > {cap}", c.total_bytes());
            }
        }
        assert!(c.total_bytes() <= cap, "{} > {cap}", c.total_bytes());
        assert!(c.total_bytes() >= cap * 8 / 10, "evicts to 90 %, not more: {}", c.total_bytes());
        assert_eq!(c.len() as u64 * 1024, c.total_bytes());
        // The recently read tiles survived although they were written first; the written-only
        // old ones went first.
        for i in 0..10 {
            assert!(get(&c, key(i)).is_some(), "recently read tile {i} was evicted");
        }
        for i in 10..40 {
            assert!(get(&c, key(i)).is_none(), "old unread tile {i} survived");
        }
        for i in 60..100 {
            assert!(get(&c, key(i)).is_some(), "recent tile {i} was evicted");
        }
        assert!(get(&c, key(99)).is_some());
        assert!(c.close(WAIT));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_stays_near_cap_with_random_order_puts() {
        // Tiles in random key order leave B-tree pages partly empty, so the file outgrows the
        // tile bytes; the cache also bounds the pages in use (FILE_OVER_CAP + FILE_SLACK).
        let dir = tmpdir("filebound");
        let db = db_in(&dir);
        let cap = 4u64 << 20;
        // Literal (not the constants), so the test notices if the bound is lost.
        let limit = cap + cap / 20 + (1 << 20);
        let c = open(&db, cap);
        let mut s = 0x2545_f491_4f6c_dd1du64;
        for i in 0..12_000u32 {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            let len = 900 + (s % 1800) as usize;
            c.put(TileKey::new(16, (s >> 20) as u32 & 0xffff, (s >> 40) as u32 & 0xffff), Bytes::from(vec![(i % 251) as u8; len]));
            if i % 1000 == 999 {
                assert!(c.flush(WAIT));
                assert!(c.total_bytes() <= cap, "{} > {cap}", c.total_bytes());
            }
        }
        assert!(c.close(WAIT));
        let file = file_bytes(&db);
        assert!(file <= limit, "file {file} > bound {limit}");
        let c = open(&db, cap);
        assert!(c.total_bytes() >= cap / 2, "bound evicted far too much: {}", c.total_bytes());
        assert!(c.close(WAIT));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_shrinks_after_eviction() {
        let dir = tmpdir("shrink");
        let db = db_in(&dir);
        let c = open(&db, 64 << 20);
        for i in 0..400u32 {
            c.put(TileKey::new(16, i, 7), Bytes::from(vec![(i % 251) as u8; 8 * 1024]));
        }
        assert!(c.close(WAIT));
        let full = file_bytes(&db);
        assert!(full >= 400 * 8 * 1024, "{full}");
        // Reopen with half the data's size as cap: evicts to 90 % of it and vacuums.
        let cap = 200 * 8 * 1024;
        let c = open(&db, cap);
        assert!(c.total_bytes() <= cap);
        assert!(c.close(WAIT));
        let after = file_bytes(&db);
        assert!(after < full * 6 / 10, "file did not shrink: {full} -> {after}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn fingerprint_change_wipes() {
        let dir = tmpdir("fingerprint");
        let db = db_in(&dir);
        let ctr = Arc::new(AtomicU64::new(0));
        let k = TileKey::new(16, 5, 6);
        let c = DiskCache::open(db.clone(), 1 << 20, ctr.clone()).unwrap();
        assert!(!block(c.sync_fingerprint("a")).unwrap(), "empty cache: nothing wiped");
        c.put(k, Bytes::from_static(b"old"));
        assert!(c.flush(WAIT));
        assert!(!block(c.sync_fingerprint("a")).unwrap(), "same fingerprint");
        assert!(block(c.sync_fingerprint("b")).unwrap(), "rebuilt archive wipes");
        assert!(get(&c, k).is_none() && c.is_empty());
        assert_eq!(ctr.load(Ordering::Relaxed), 0);
        assert!(!block(c.sync_fingerprint("b")).unwrap());
        // Pending (uncommitted) tiles are wiped too.
        c.put(k, Bytes::from_static(b"x"));
        assert!(block(c.sync_fingerprint("c")).unwrap());
        assert!(get(&c, k).is_none());
        // The fingerprint survives a restart.
        c.put(k, Bytes::from_static(b"new"));
        assert!(c.close(WAIT));
        drop(c);
        let c = open(&db, 1 << 20);
        assert!(!block(c.sync_fingerprint("c")).unwrap());
        assert_eq!(get(&c, k).as_deref(), Some(&b"new"[..]));
        assert!(c.close(WAIT));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn schema_version_change_wipes() {
        let dir = tmpdir("schema");
        let db = db_in(&dir);
        let k = TileKey::new(14, 9, 9);
        let c = open(&db, 1 << 20);
        c.put(k, Bytes::from_static(b"tile"));
        assert!(block(c.sync_fingerprint("fp")).unwrap());
        c.put(k, Bytes::from_static(b"tile"));
        assert!(c.close(WAIT));
        drop(c);
        // Same version: kept.
        let c = open(&db, 1 << 20);
        assert_eq!(c.len(), 1);
        assert!(c.close(WAIT));
        drop(c);
        // Written by the previous version.
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute(
                "UPDATE meta SET v = ?1 WHERE k = 'schema_version'",
                [(CACHE_SCHEMA_VERSION - 1).to_string()],
            )
            .unwrap();
        }
        let c = open(&db, 1 << 20);
        assert!(c.is_empty() && c.total_bytes() == 0);
        assert!(get(&c, k).is_none());
        // The fingerprint went with the tiles; the next sync records it without a wipe.
        assert!(!block(c.sync_fingerprint("fp")).unwrap());
        assert!(c.close(WAIT));
        let conn = rusqlite::Connection::open(&db).unwrap();
        let v: String = conn.query_row("SELECT v FROM meta WHERE k = 'schema_version'", [], |r| r.get(0)).unwrap();
        assert_eq!(v, CACHE_SCHEMA_VERSION.to_string());
        drop(conn);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_is_recreated() {
        let dir = tmpdir("corrupt");
        let db = db_in(&dir);
        fs::write(&db, vec![0xA5u8; 8192]).unwrap();
        let notes = Arc::new(Mutex::new(Vec::<Note>::new()));
        let n2 = notes.clone();
        let notify: Notify = Arc::new(move |n| n2.lock().unwrap().push(n));
        let c = DiskCache::open_with(db.clone(), 1 << 20, Arc::new(AtomicU64::new(0)), Some(notify)).unwrap();
        assert!(c.is_empty());
        let k = TileKey::new(16, 3, 4);
        c.put(k, Bytes::from_static(b"ok"));
        assert!(c.flush(WAIT));
        assert_eq!(get(&c, k).as_deref(), Some(&b"ok"[..]));
        let notes = notes.lock().unwrap().clone();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(matches!(&notes[0], Note::Info(m) if m.starts_with(MSG_CACHE_RECREATED)), "{notes:?}");
        assert!(c.close(WAIT));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn second_open_is_rejected() {
        let dir = tmpdir("busy");
        let url = "http://127.0.0.1:9/none.pmtiles";
        let db = dir.join(cache_file_name(url));
        let first = open(&db, 1 << 20);
        let k = TileKey::new(16, 1, 2);
        first.put(k, Bytes::from_static(b"held"));
        assert!(first.flush(WAIT));
        let e = DiskCache::open(db.clone(), 1 << 20, Arc::new(AtomicU64::new(0))).err().expect("second open must fail");
        assert_eq!(e.kind(), io::ErrorKind::ResourceBusy, "{e}");
        assert!(e.to_string().contains("in use"), "{e}");
        // A fetcher on the same file runs without a disk cache and warns once.
        let notes = Arc::new(Mutex::new(Vec::<Note>::new()));
        let n2 = notes.clone();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let f = {
            let _g = rt.enter();
            Fetcher::new(url, dir.to_str().unwrap(), Arc::new(AtomicU64::new(0)), FetchParams::default(), Box::new(move |n| n2.lock().unwrap().push(n)))
                .unwrap()
        };
        assert!(rt.block_on(f.disk()).is_none());
        let notes = notes.lock().unwrap().clone();
        assert!(notes.iter().any(|n| matches!(n, Note::Warn(m) if m.contains("in use") && m.contains("without a disk cache"))), "{notes:?}");
        // The first handle is unaffected; once it is closed the file opens again.
        assert_eq!(get(&first, k).as_deref(), Some(&b"held"[..]));
        assert!(first.close(WAIT));
        let again = open(&db, 1 << 20);
        assert_eq!(get(&again, k).as_deref(), Some(&b"held"[..]));
        assert!(again.close(WAIT));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_folders_removed() {
        let dir = tmpdir("legacy");
        let url = "http://127.0.0.1:8787/vilnius.pmtiles";
        let hash = source_hash(url);
        let db = dir.join(cache_file_name(url));
        let old2 = dir.join(format!("pmtiles-v2-{hash}"));
        let old1 = dir.join(format!("pmtiles-v1-{hash}"));
        let old0 = dir.join(format!("pmtiles-{hash}"));
        let other = dir.join("pmtiles-v2-0123456789abcdef");
        let other0 = dir.join("pmtiles-0123456789abcdef");
        for d in [&old0, &old1, &old2, &other, &other0] {
            fs::create_dir_all(d.join("16/1")).unwrap();
            fs::write(d.join("16/1/2.mvt"), b"x").unwrap();
        }
        let c = open(&db, 1 << 20);
        assert!(!old0.exists() && !old1.exists() && !old2.exists(), "same-source folders are removed");
        assert!(other.exists() && other0.exists(), "other sources are left alone");
        assert!(db.exists());
        assert!(c.close(WAIT));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_do_not_write() {
        let dir = tmpdir("reads");
        let c = open(&db_in(&dir), 1 << 20);
        let keys: Vec<TileKey> = (0..32).map(|i| TileKey::new(16, i, 1)).collect();
        for &k in &keys {
            c.put(k, Bytes::from_static(b"warm"));
        }
        assert!(c.flush(WAIT));
        let commits = c.commit_count();
        assert_eq!(commits, 1);
        // Later reads record a newer access time (in memory only).
        c.shared.test_clock_s.store(i64::from(u32::MAX), Ordering::Relaxed);
        let t0 = Instant::now();
        for _ in 0..4 {
            for &k in &keys {
                assert!(get(&c, k).is_some());
            }
        }
        let burst = t0.elapsed();
        if burst < Duration::from_millis(200) {
            assert_eq!(c.commit_count(), commits, "a read burst committed before the flush interval");
        }
        // The access times go out with the next timed batch: exactly one commit.
        std::thread::sleep(FLUSH_INTERVAL + Duration::from_millis(250));
        assert_eq!(c.commit_count(), commits + 1);
        // Reads at the same access time record nothing new.
        for &k in &keys {
            assert!(get(&c, k).is_some());
        }
        assert!(c.flush(WAIT));
        assert_eq!(c.commit_count(), commits + 1);
        assert!(c.close(WAIT));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn repeated_write_failures_disable_the_cache() {
        let dir = tmpdir("fail");
        let notes = Arc::new(Mutex::new(Vec::<Note>::new()));
        let n2 = notes.clone();
        let notify: Notify = Arc::new(move |n| n2.lock().unwrap().push(n));
        let c = DiskCache::open_with(db_in(&dir), 1 << 20, Arc::new(AtomicU64::new(0)), Some(notify)).unwrap();
        let k = TileKey::new(16, 8, 8);
        c.put(k, Bytes::from_static(b"kept"));
        assert!(c.flush(WAIT));
        c.shared.fail_writes.store(true, Ordering::Relaxed);
        for i in 0..2 {
            c.put(TileKey::new(16, 9, i), Bytes::from_static(b"lost"));
            assert!(c.flush(WAIT));
            assert!(!c.is_disabled());
            assert!(get(&c, TileKey::new(16, 9, i)).is_none(), "a failed batch is dropped");
        }
        // A success resets the count.
        c.shared.fail_writes.store(false, Ordering::Relaxed);
        c.put(TileKey::new(16, 9, 5), Bytes::from_static(b"ok"));
        assert!(c.flush(WAIT));
        c.shared.fail_writes.store(true, Ordering::Relaxed);
        for i in 0..3 {
            assert!(!c.is_disabled(), "disabled after {i} consecutive failures");
            c.put(TileKey::new(16, 10, i), Bytes::from_static(b"lost"));
            assert!(c.flush(WAIT));
        }
        assert!(c.is_disabled());
        assert!(get(&c, k).is_none(), "a disabled cache misses");
        let notes = notes.lock().unwrap().clone();
        assert_eq!(notes.iter().filter(|n| matches!(n, Note::CacheDisabled(_))).count(), 1, "{notes:?}");
        assert!(c.close(WAIT));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_files_differ_per_url_and_keys_pack() {
        assert_ne!(cache_file_name("http://a/x.pmtiles"), cache_file_name("http://a/y.pmtiles"));
        assert_eq!(cache_file_name("u"), cache_file_name("u"));
        assert!(cache_file_name("u").starts_with("tiles-") && cache_file_name("u").ends_with(".sqlite"));
        let a = pack_key(TileKey::new(16, 37370, 20827));
        assert_eq!(a, (16i64 << 58) | (37370i64 << 29) | 20827);
        assert_ne!(pack_key(TileKey::new(15, 1, 2)), pack_key(TileKey::new(16, 1, 2)));
        assert_ne!(pack_key(TileKey::new(16, 2, 1)), pack_key(TileKey::new(16, 1, 2)));
        assert!(pack_key(TileKey::new(29, (1 << 29) - 1, (1 << 29) - 1)) > 0);
    }

    #[test]
    fn tileset_contract_check() {
        let meta = r#"{"vector_layers":[{"id":"buildings"},{"id":"landuse"},{"id":"roads"}],"name":"x"}"#;
        assert_eq!(check_tileset(13, 16, meta), Ok(DataLevels::COMPAT));
        assert!(check_tileset(0, 18, meta).is_ok());
        let e = check_tileset(14, 16, meta).unwrap_err();
        assert!(e.contains("z14-16") && e.contains("z13-16"), "{e}");
        assert!(check_tileset(13, 15, meta).is_err());
        let e = check_tileset(13, 16, r#"{"vector_layers":[{"id":"buildings"},{"id":"water"}]}"#).unwrap_err();
        assert!(e.contains("landuse, roads") && e.contains("has: buildings, water"), "{e}");
        assert!(check_tileset(13, 16, "{}").unwrap_err().contains("vector_layers"));
        assert!(check_tileset(13, 16, "not json").is_err());
        // Two-level archives (Tiles and LOD v2): exactly the two zooms, overview layers.
        let v2 = |levels: &str, extra: &str| {
            format!(
                r#"{{"simworld_levels":{levels},"vector_layers":[{{"id":"buildings"}},{{"id":"landuse"}},{{"id":"roads"}},{{"id":"blocks"}},{{"id":"lights"}}{extra}]}}"#
            )
        };
        let full = v2(r#"{"detail":14,"overview":11}"#, r#",{"id":"landmarks"}"#);
        assert_eq!(check_tileset(11, 14, &full), Ok(DataLevels::two_level(14, 11, true)));
        assert_eq!(check_tileset(11, 14, &v2(r#"{"detail":14,"overview":11}"#, "")), Ok(DataLevels::two_level(14, 11, false)));
        assert_eq!(check_tileset(12, 14, &v2(r#"{"detail":14,"overview":12}"#, "")), Ok(DataLevels::two_level(14, 12, false)));
        let e = check_tileset(11, 16, &full).unwrap_err();
        assert!(e.contains("z11-16") && e.contains("exactly z11 and z14"), "{e}");
        assert!(check_tileset(13, 16, &full).is_err(), "levels win over the compat zooms");
        let e = check_tileset(11, 15, &v2(r#"{"detail":15,"overview":11}"#, "")).unwrap_err();
        assert!(e.contains("detail z15"), "{e}");
        let e = check_tileset(11, 14, &v2(r#"{"detail":"x"}"#, "")).unwrap_err();
        assert!(e.contains("simworld_levels must be"), "{e}");
        let e = check_tileset(11, 14, r#"{"simworld_levels":{"detail":14,"overview":11},"vector_layers":[{"id":"buildings"},{"id":"landuse"},{"id":"roads"}]}"#)
            .unwrap_err();
        assert!(e.contains("blocks, lights"), "{e}");
        for l in [DataLevels::COMPAT, DataLevels::two_level(14, 11, true), DataLevels::two_level(14, 12, false)] {
            assert_eq!(parse_levels(&levels_text(l)), Some(l));
        }
        assert_eq!(parse_levels("14,11"), None);
        assert_eq!(FetchParams::default().cache_cap_bytes, 300 * 1024 * 1024);
        assert_eq!(FetchParams::default().open_retry, Duration::from_millis(1500));
    }
}
