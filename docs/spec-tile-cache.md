# Spec: Disk Tile Cache in One SQLite File

2026-10-27 · Aleksei

## Purpose and scope

Replace the file-per-tile disk cache in `rust/simworld-core/src/fetch.rs` (`DiskCache`) with a single SQLite database per tile source. The cache keeps its job and its contract (section 10 of [contract.md](contract.md)): raw tile bytes as stored in the archive, keyed by `z/x/y`, LRU with a byte cap, survives restarts, works offline when warm, and is wiped when the archive fingerprint or the cache schema version changes. Meshes stay uncached.

Out of scope: an imagery cache (M7; see "Later"), changing the compression of the archive, and the network side of `Fetcher`.

**Owner:** `rust-world-streamer` (everything here is in `simworld-core`). No C# or shader change.

## Status and measured results

**Built (T1-T3), 2026-10-27.** `DiskCache` in `rust/simworld-core/src/fetch.rs`; benchmark `rust/simworld-core/benches/cache_bench.rs`. Acceptance 1-4 met on the dev Mac (Apple Silicon, APFS); 5 is unverified (no iOS target or SDK on the dev Mac).

**Benchmark.** From `rust/`: `cargo bench -p simworld-core --bench cache_bench` (release build, about 2 min; needs `data/out/vilnius.pmtiles` and ~1 GB free in the temp folder; `SIMWORLD_BENCH_SKIP_300=1` skips the 300 MB phase, `SIMWORLD_BENCH_DIR=` moves the scratch folder, `SIMWORLD_BENCH_KEEP=1` keeps it). Tile set: every tile of the archive, read into memory (30,726 tiles, 61.0 MiB; z13 380, z14 1,518, z15 5,906, z16 22,922; the "30,607" below is an older count). The old cache is a minimal copy of the file-per-tile `DiskCache` of git `41f414d` inside the bench. Caller timings use each cache's real call path in `Fetcher::fetch`: old `spawn_blocking` (awaited), new async `get` and fire-and-forget `put`. All numbers MiB, page cache warm (no `purge` without sudo). Ranges are over three runs.

| Measure | Old (file per tile) | New (SQLite) |
|---|---|---|
| Cold fill, 30,726 tiles, random order | 4.1-5.9 s (put p50 123-167 µs, p99 217-417 µs) | 1.7-2.1 s incl. final flush, 481 commits (put call < 0.1 µs) |
| Disk, full tile set (61 MiB data) | 151 MiB allocated in 30,726 files | 79 MiB open (db + WAL), 74 MiB closed |
| Warm random reads, every tile once | p50 126-148 µs, p99 320-1,440 µs | p50 7.2-7.6 µs, p99 22-25 µs |
| Warm reads in flight order (1,110 tiles, autopilot north from the bench origin) | p50 46-118 µs, p99 210-850 µs | p50 6-8 µs, p99 25-36 µs |
| Eviction: fill all tiles with a 50 MB cap | 4.5-5.7 s; 50 MiB data in 124 MiB allocated | 1.8-2.0 s; 43 MiB data, 57 MiB file open / 53 MiB closed; reads after eviction p50 6 µs |
| Open, 30,726 tiles | 70-78 ms | 1.7-1.8 ms |
| Open, full 300 MB cache | 340-383 ms (151,564 files) | 5-9 ms (127,071 tiles) |
| Disk, full 300 MB cache | 746 MiB allocated for 300 MiB data | file + WAL peak 321 MiB over 750 MiB of churn (data 236-262 MiB); 303 MiB closed |

Acceptance:

1. **Pass.** Full 300 MB cache: file + WAL peaked at 320.7 MiB (≤ 330) while 750 MiB of tiles were written through it; the old cache takes 746 MiB of disk for the same cap. This needed the page bound below: without it the same fill gave 343 MiB closed / 348 MiB open for 285 MiB of data.
2. **Pass.** Warm reads p50 ~7 µs vs ~126 µs, p99 ~25 µs vs 320+ µs (random and flight order). One outlier seen once: a 255 ms max while one batch updated ~30,000 access times at once (every tile read within 250 ms); no flight produces that burst (a few hundred reads per second).
3. **Pass.** 5-9 ms for a full cache. This needs the covering index below: with `tiles_atime(atime)` the open took 74-86 ms warm, and the `SUM` alone took ~0.9 s with a cold page cache (measured with the `sqlite3` CLI).
4. **Pass, with the limits stated here.** Test bench, headless (`--headless --max-fps 60 --quit-after N -- --autopilot=250`), fresh `user://tile_cache`: pass 1 online 151 s (37 km north from the origin, past the archive's north edge; 2,043 tiles cached, 804 of them empty markers, 17.1 MiB), then `data/serve.sh` stopped (connection refused checked) and 4 more flights of 110 s over the same path, 593 s in total. No errors, no warnings, status `ok` throughout, the same cover each pass (`cover consistent`, 509-510 tiles), cache unchanged at 2,043 tiles / 17.1 MiB, `cache_mb` 17.07 ≤ 300. Limits: the autopilot flies a straight line and never revisits ground, so "after the first pass" was done as separate process runs over the first pass's path (each offline run is shorter than the online one, so it only needs tiles the first run fetched), not as one 10-minute flight; it ran headless (no GPU upload or rendering); `cache_mb` is logged once per run (the `debug_stats` line), so it was checked at each start and in the file after each exit, not continuously. The Vilnius path holds 17 MiB, far below the 300 MiB cap, so the cap was exercised in an extra online run with `--set=cache.cap_mb=8` (150 s): the cache was evicted from 17 to 7.2 MiB on open, file + WAL sampled every 2 s stayed at 7.5-12.5 MiB (the ~4 MiB WAL dominates at such a small cap), 7.2 MiB of tile data at exit. With the server down, status stays `ok` (not `degraded`) as long as every tile comes from the cache: the archive is only opened on a miss.
5. **Unverified.** `rustup target list --installed` shows only `aarch64-apple-darwin`, and `xcrun --sdk iphoneos` finds no SDK; nothing was installed. `cargo tree -p simworld-core | grep -i godot` prints nothing.

**Deviations from the design below (T1-T3):**

- `rusqlite` also enables the `cache` feature (`prepare_cached`, so the cache thread prepares each statement once).
- Eviction reads 256 `(key, size)` rows oldest first and deletes them one by one until under 90 % of the cap, instead of `DELETE ... WHERE key IN (SELECT ... LIMIT 256)`: it stops at the low-water mark and keeps the byte total exact without reading blobs.
- Vacuum: `incremental_vacuum(N)` with N = min(freelist pages, 8192), then `wal_checkpoint(TRUNCATE)` so the main file actually shrinks.
- Only puts count toward the 64-write batch trigger; access times wait for the 250 ms interval.
- `busy_timeout(0)`: a file held by someone else is "in use" at once (rusqlite waits 5 s by default).
- A re-configure reuses the open file only when `tile_url`, `cache_dir` **and** `cache.cap_mb` are unchanged; a new cap closes and reopens (which evicts to the new cap).
- Legacy cleanup also removes the unversioned `pmtiles-{hash}/` folder of the first spike builds.
- `meta.source_url` is not written (the file name already carries the URL hash).
- `mmap_size` = 64 MiB on all platforms; the iOS question under "Open questions" is still open.
- (T3) Index `tiles_lru(atime, key, size)` instead of `tiles_atime(atime)`: it covers `SUM(size)` at open and the eviction scan, so neither reads table pages. Files from the first T1/T2 builds get it on open (`DROP INDEX IF EXISTS tiles_atime`), same schema version.
- (T3) Page bound: after each commit the pages in use (`page_count - freelist_count`) may exceed the cap by 5 % + 1 MiB; beyond that the cache evicts in proportion, aiming for 90 % of that bound. Tiles arrive at random places of the key B-tree, so pages are only ~80 % full and a cache with 300 MiB of tile data would take ~360 MiB. The byte cap and `cache_mb` keep their meaning (tile bytes); a full cache now holds ~80-87 % of the cap in tile data. The 1 MiB slack keeps small caps (tests) bound by tile bytes only. Test `file_stays_near_cap_with_random_order_puts`.
- WAL size is not limited separately: it grows to ~4 MiB (auto-checkpoint at 1,000 pages) and is truncated after each eviction.

## Why: problems with today's cache

Today's `DiskCache` writes each tile to `{cache_dir}/pmtiles-v{N}-{hash}/{z}/{x}/{y}.mvt`, does LRU by file mtime (touched on hit), caps the total at 300 MB and keeps the archive fingerprint in a `fingerprint` file.

| # | Problem | Effect |
|---|---------|--------|
| C1 | **Block padding.** Tiles are small (z16 ≈ 1 kB, z15 ≈ 2.8 kB gzip on average; see `data/README.md`), but every file takes at least one 4 kB block on APFS/ext4. | Real disk use is several times what `cache_mb` reports and the 300 MB cap enforces. A 300 MB cache of z16 tiles can occupy over 1 GB. |
| C2 | **Syscalls per tile.** A hit is `open` + `read` + `close` + an mtime touch; a miss write is create tmp + write + rename (+ directory updates). | Wasted I/O on the tokio blocking pool, worst on iOS and Windows. |
| C3 | **Startup scan.** `DiskCache::open` walks every `z/x/` folder and `stat`s every file to rebuild the LRU. | Startup time grows with the number of tiles (tens of thousands of `stat` calls per launch). |
| C4 | **Inode and directory churn.** Tens of thousands of files in a deep tree; eviction is one `unlink` per tile. | Slow eviction and clears; more filesystem metadata to sync and back up. |
| C5 | **No locking.** Two processes or two worlds can use the same folder. | The planned 0.7 rule "one `cache_dir` per instance, exclusive lock" ([contract.md](contract.md), P4) has nothing to build on today. P4 adds a folder lock that this cache keeps (see "Locking and lifecycle"). |
| C6 | **State spread over files.** Fingerprint file, versioned folder names, `.tmp` leftovers. | Several places to keep consistent; cleanup code for each. |

## What other streaming engines do

| Project | Cache design | What we take | What we avoid |
|---------|-------------|--------------|---------------|
| **Cesium Native** (Cesium for Unreal / Unity) | One SQLite file (`cesium-request-cache.sqlite`), one row per HTTP response: `key TEXT PRIMARY KEY`, `expiryTime`, `lastAccessedTime`, headers as JSON, `responseData BLOB`. `journal_mode=WAL`, `synchronous=OFF`. Prune by **item count**: expired rows first, then oldest `lastAccessedTime`. On `SQLITE_CORRUPT` the file is deleted and recreated. One connection behind a mutex, prepared statements reused. ([source](https://raw.githubusercontent.com/CesiumGS/cesium-native/main/CesiumAsync/src/SqliteCache.cpp), [settings](https://cesium.com/learn/cesium-unreal/ref-doc/classUCesiumRuntimeSettings.html)) | One SQLite file, WAL, delete-and-recreate on corruption, reused prepared statements | Count-based cap (users report the file growing past 20 GB, [forum](https://community.cesium.com/t/why-does-loading-certain-3d-tiles-services-in-cesium-take-so-long-and-is-there-a-maximum-size-limit-for-the-cache-file/44514)); a disk write on every hit (`lastAccessedTime` update); `synchronous=OFF` (reported disk I/O errors on Android, [forum](https://community.cesium.com/t/sqlitecache-cpp-609-disk-i-o-error/44230)); mutex destroyed while held in `destroyDatabase()` |
| **MapLibre / Mapbox Native** (offline maps) | One SQLite database for everything. The "ambient" cache is evicted to a **byte budget**; offline regions are outside that budget. The file is packed to give space back. Moving to WAL gave 2-4x depending on device age. ([guide](https://maplibre.org/maplibre-native-ffi/guides/work-offline/), [API](https://maplibre.org/maplibre-native/cpp/api/classmbgl_1_1DatabaseFileSource.html), [WAL PR](https://github.com/mapbox/mapbox-gl-native/pull/5796)) | Byte budget, space reclaim after eviction, WAL; room for pinned regions later ("pre-download the test area", `simworld-cli bake`) | — |
| **MSFS** (`ROLLINGCACHE.CCC`) | One pre-allocated container file of fixed size (16 GB default in 2024), older data overwritten by newer. Format not public. Users report I/O from constantly rearranging contents. ([overview](https://flyawaysimulation.com/ask/answers/microsoft-flight-simulator-rolling-cache/), [forum](https://forums.flightsimulator.com/t/rollingcache-ccc-performance-debugging-and-tuning-how/670564)) | Nothing now: the design pays off for gigabytes of large photogrammetry/imagery blobs, not 1 kB vector tiles | Own container format, compaction, crash safety, all hand-written |

**SQLite's own numbers** ([35% Faster Than The Filesystem](https://www2.sqlite.org/fasterthanfs.html)): blobs around 10 kB are read and written about 35 % faster from one database than from separate files (one `open`/`close` instead of one per blob), and the database uses about 20 % less disk space because files are padded to block size. Our tiles are about 10x smaller, so the padding saving is larger; the advantage shifts to plain files only for blobs in the hundreds of kB and up.

**Alternatives considered**

| Option | Verdict |
|--------|---------|
| SQLite via `rusqlite` (`bundled`) | **Chosen.** Proven in both reference engines, byte-exact crash safety in WAL, built from source by `cc` (no system library; works for iOS static builds), file locking included. Cost: a C compile in the build and roughly 1 MB of binary. |
| `redb` (pure Rust) | No C toolchain needed, but self-described beta and a third-party benchmark shows a much larger file on disk ([redb](https://docs.rs/crate/redb/2.0.0-beta0), [lkv benchmark](https://docs.rs/crate/lkv/0.2.1)). Revisit only if the C build becomes a problem on a target. |
| `sled` | Unmaintained in practice; no advantage over the two above. |
| Own segmented log (64 MB append-only segments + in-memory index, evict whole segments) | Fastest possible writes and trivial eviction, but we would own crash recovery, compaction and locking. Worth it only for multi-GB caches (imagery, see "Later"). |
| Keep files, add pack files per z/x | Fixes C1/C3 partly, keeps C2/C5, still custom. No. |

## Design

### File and location

- One database per source: `{cache_dir}/tiles-{hash}.sqlite` (+ `-wal`, `-shm` while open). `{hash}` is today's `source_dir_name(url)` hash, so different archives never mix.
- `cache_dir` stays a host argument. Hosts must pass a purgeable, non-backed-up folder: `OS.GetCacheDir()` (`Library/Caches` on iOS, the platform cache folder elsewhere). Never `user://` on iOS: Godot maps it to `Documents`, which iCloud backs up (Apple's data storage guidelines reject re-downloadable data there). The addon's `WorldView.CacheDir` defaults to `cache://simworld/tiles` = `OS.GetCacheDir()/<project name>/simworld/tiles` (`docs/spec-world-seed.md` section 8b; before 2026-10-27 it was `user://simworld_cache`). The test bench keeps `user://tile_cache` on desktop. The library never opens a database it did not create here (no game saves, no shared files).
- On first open, delete legacy `pmtiles-v*-{hash}/` folders for the same source (today's `remove_stale_versions` generalized). No migration of their content: the cache refills from the network.

### Schema

```sql
-- Set before the first table exists (they are no-ops afterwards):
PRAGMA page_size = 4096;
PRAGMA auto_vacuum = INCREMENTAL;     -- lets incremental_vacuum give pages back after eviction

CREATE TABLE IF NOT EXISTS meta (
  k TEXT PRIMARY KEY,
  v TEXT NOT NULL
) WITHOUT ROWID;
-- rows: schema_version (CACHE_SCHEMA_VERSION), fingerprint (archive header hash), source_url

CREATE TABLE IF NOT EXISTS tiles (
  key   INTEGER PRIMARY KEY,          -- packed z/x/y (below); is the rowid, no extra index
  data  BLOB    NOT NULL,             -- bytes as archived (gzip MVT); zero length = "no tile here"
  size  INTEGER NOT NULL,             -- length(data), kept so eviction never reads blobs
  atime INTEGER NOT NULL              -- last access, seconds since UNIX epoch
);
CREATE INDEX IF NOT EXISTS tiles_atime ON tiles(atime);
```

Key packing: `key = (z << 58) | (x << 29) | y` as `i64` (z ≤ 31, x and y < 2^29 cover z ≤ 29). One integer key keeps lookups on the rowid B-tree, the fastest path SQLite has.

### Connection settings (every open)

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;          -- in WAL this cannot corrupt the database; a crash loses at most the last commits
PRAGMA locking_mode = EXCLUSIVE;      -- one process per cache file; second safeguard behind the P4 folder lock (C5)
PRAGMA temp_store = MEMORY;
PRAGMA cache_size = -8192;            -- 8 MiB page cache
PRAGMA mmap_size = 67108864;          -- 64 MiB read mapping (disable if a platform misbehaves)
```

`synchronous = OFF` (Cesium's choice) is not used: the gain over `NORMAL` in WAL is small and it allows corruption on power loss.

### Threading

- **One cache thread owns the connection.** It is a dedicated `std::thread`, not a tokio task. `Fetcher` sends it requests over a channel (`Get(key, oneshot)`, `Put(key, bytes)`, `Sync(fingerprint, oneshot)`, `Clear`, `Flush(oneshot)`, `Close`). No mutex around the connection, so no lock-ordering or poisoned-mutex paths.
- Prepared statements are created once on the cache thread and reused.
- **Writes are batched.** `Put`s and access-time updates collect in memory and are committed in one transaction every 250 ms, or after 64 pending writes, whichever comes first. A `Get` of a key with a pending `Put` is answered from the pending buffer.
- **Reads never write.** A hit only records `key -> now` in an in-memory map; the map is flushed as `UPDATE tiles SET atime=? WHERE key=?` with the next batch. This removes Cesium's write-per-hit.
- `Close` flushes pending writes, runs `PRAGMA wal_checkpoint(TRUNCATE)` and closes. It has two callers with different waiting rules (see "Locking and lifecycle"): `Worker` drop is a background stop with no wait (pending writes may be lost, which is acceptable for a cache), while `shutdown()` and re-configure wait, bounded, until the thread has closed the file.

### Size cap and eviction

- The cap stays in bytes of tile data (300 MB default, from the profile key `cache.cap_mb` since P3). The running total is `SELECT SUM(size) FROM tiles` once at open, then kept in memory. It also feeds `debug_stats().cache_mb`, so that value keeps its meaning.
- After a commit pushes the total above the cap, evict down to 90 % of the cap, oldest `atime` first, in batches:
  `DELETE FROM tiles WHERE key IN (SELECT key FROM tiles ORDER BY atime ASC LIMIT 256)`, repeated until under the low-water mark, inside the same transaction.
- Afterwards `PRAGMA incremental_vacuum(N)` gives the freed pages back to the file system (N = freed bytes / page size, capped per batch so one call stays short).
- The real file size (`page_count * page_size`) is checked too: if it exceeds the cap by more than 25 % (fragmentation), run a full `incremental_vacuum`. A full `VACUUM` is never run on the cache thread during a flight.

### Fingerprint and schema version

- `meta.schema_version` replaces the version in the folder name; `meta.fingerprint` replaces the `fingerprint` file.
- Open: if `schema_version` differs from `CACHE_SCHEMA_VERSION`, `DELETE FROM tiles`, store the new version.
- `Sync(fingerprint)` keeps today's rule: once the archive could be opened, a different fingerprint deletes all tiles (`DELETE FROM tiles` + `incremental_vacuum`) and stores the new one. While the archive is unreachable, the cache is served as is and the check is retried every 30 s.

### Errors and corruption

- `SQLITE_CORRUPT` or `SQLITE_NOTADB` on open or on any statement: close, delete the database and its `-wal`/`-shm`, recreate, log one `info` line ("disk cache was corrupt, recreated"). The cache is disposable; never ask the user.
- `SQLITE_BUSY`/`SQLITE_LOCKED` on open (something other than this instance holds the file): run without a disk cache and log a warning. With P4 the normal "in use" case never reaches this point: the folder lock fails `configure_world` with "cache_dir in use" first, so a BUSY here means a foreign process ignoring the lock.
- `SQLITE_FULL` / I/O errors on write: drop the pending batch, evict harder (to 50 % of the cap), keep running. Three consecutive failures disable the disk cache for the session with one warning.
- Any error answers pending `Get`s with "miss", so fetching falls back to the network and never stalls.
- Disk-cache state reaches the host as follows: "recreated after corruption" is logged only; "disabled for the session" (three write failures) sets the world status to `degraded` ([contract.md](contract.md), `status_changed`, 0.7).

### Locking and lifecycle

- **Folder lock (P4, before this cache exists).** `configure_world` takes an exclusive lock on `{cache_dir}/.simworld.lock` synchronously, before the asynchronous archive and cache open, so "cache_dir in use" can be returned as its error. It is per folder, not per source, and absent when there is no disk cache. This cache keeps it unchanged; nothing here depends on SQLite's file lock for that error.
- **Re-configure (teleport) must not lock out its own instance.** `Close` of the old cache completes (bounded wait, e.g. 1 s, then the open falls back to "no disk cache" with a warning) before the new `DiskCache::open` of the same file; or, when `tile_url` and `cache_dir` are unchanged, the open handle is reused and no close/open happens. The folder lock stays held in both cases.
- **`shutdown()`** waits, bounded, for `Close` to finish, then releases the folder lock. **Dropping the node** only signals `Close` and returns.

### API inside `simworld-core`

`DiskCache` keeps its public shape so `Fetcher` changes little:

```rust
pub struct DiskCache { tx: Sender<Cmd>, bytes: Arc<AtomicU64>, ... }

impl DiskCache {
    pub fn open(path: PathBuf, cap: u64, bytes: Arc<AtomicU64>) -> io::Result<Self>;
    pub async fn get(&self, k: TileKey) -> Option<Vec<u8>>;     // was sync + spawn_blocking
    pub fn put(&self, k: TileKey, data: Bytes);                  // fire and forget
    pub async fn sync_fingerprint(&self, fp: &str) -> io::Result<bool>;
    pub fn total_bytes(&self) -> u64;
    pub fn len(&self) -> usize;
}
```

`Fetcher::fetch` drops its `spawn_blocking` wrappers around the cache. `put` takes `Bytes` so the network buffer is shared, not copied.

### Dependency

```toml
# simworld-core/Cargo.toml
rusqlite = { version = "<current>", default-features = false, features = ["bundled"] }
```

`bundled` compiles the amalgamation with `cc`; no system SQLite, no OpenSSL. `cargo tree -p simworld-core | grep -i godot` must still print nothing. The iOS target (`aarch64-apple-ios`, staticlib) must build in CI before this merges.

## Contract and docs impact

- **No API change.** `configure`, `cache_dir` and `debug_stats().cache_mb` keep their meaning. No `api_version` bump is required; a CHANGELOG line under "Internal" records the new on-disk format.
- [contract.md](contract.md) section 10: replace "raw tile bytes keyed by `z/x/y`" with the same plus "in one SQLite file per source". The 0.7 folder lock and the `shutdown()`/teleport rules are already specified there and in [spec-library-packaging.md](spec-library-packaging.md); this cache implements the "flush and close" half.
- `CACHE_SCHEMA_VERSION` goes to 3 so old folders are recognized and removed.
- [spec-library-packaging.md](spec-library-packaging.md): `simworld-cli bake` writes this same database format (pre-filled caches can be shipped or copied).

## Tests

All in `fetch.rs` (`cargo test`, temp directories, no network).

| Test | Checks |
|------|--------|
| `put_get_survives_reopen` | Bytes and the zero-length "no tile" marker round-trip across close/open (replaces `disk_cache_put_get_survives_reopen`) |
| `pending_put_visible_to_get` | A `get` right after `put`, before the batch commits, returns the bytes |
| `evicts_oldest_by_bytes` | With a 64 kB cap, inserting 100 tiles of 1 kB keeps the total ≤ cap and keeps the most recently *read* tiles, not only the most recently written |
| `file_shrinks_after_eviction` | After evicting half the data, the database file size drops (incremental vacuum works) |
| `fingerprint_change_wipes` | `sync_fingerprint("a")`, put, `sync_fingerprint("b")` → empty, returns true; same fingerprint again → false |
| `schema_version_change_wipes` | A database written with version N-1 is emptied on open |
| `corrupt_file_is_recreated` | Garbage bytes in the file → open succeeds, cache empty, one info log |
| `second_open_is_rejected` | Opening the same file twice: the second open reports "in use" and the fetcher runs without disk cache |
| `legacy_folders_removed` | `pmtiles-v2-{hash}/` next to the new file is deleted on open |
| `reads_do_not_write` | A burst of gets on a warm cache commits nothing until the flush interval (count commits through a test hook) |

## Benchmark and acceptance

A `cargo bench` (or `simworld-cli bench-cache` once P6 exists) compares the old and new caches on the same data: the real `vilnius.pmtiles` tile set (30,607 tiles), cold fill, warm random reads, warm reads in flight order (autopilot path), eviction under a 50 MB cap, and open time with a full cache.

Accept when, on the dev Mac:

1. Disk use for a full 300 MB cache is ≤ 330 MB (file + WAL), against the measured file-per-tile footprint today.
2. Warm reads are at least as fast as today (p50 and p99 per tile).
3. Opening a full cache takes ≤ 50 ms (today: directory walk; measure).
4. The test bench flies `--autopilot=250` for 10 minutes over Vilnius with the tile server stopped after the first pass: no tile errors, `cache_mb` stays ≤ cap.
5. The iOS staticlib builds in CI.

## Phases

| Phase | Work | Done when |
|-------|------|-----------|
| T1 | `rusqlite` dependency, new `DiskCache` behind the same API, cache thread, schema, batching | Unit tests above pass; `cargo test` baseline + new tests green |
| T2 | Eviction by bytes, incremental vacuum, corruption and lock handling, legacy folder removal | Eviction, corruption and lock tests pass |
| T3 | Benchmark old vs new, test-bench soak flight, contract/CHANGELOG/doc updates | Acceptance 1-5 met |

## Later

- **Pinned regions.** A `pinned INTEGER` column (or a second table) for tiles downloaded on purpose (`bake`, "download this route"); eviction skips them, as MapLibre's offline regions sit outside the ambient budget.
- **Imagery (M7).** Raster tiles are 20-100 kB, where the per-file overhead matters less and SQLite's advantage shrinks. Start with a second table in the same file with its own byte cap (500 MB in the design doc); move imagery to a segmented log only if measurements demand it.
- **Recompressed tiles.** If the archive moves to zstd with a dictionary, the cache stores those bytes unchanged; the dictionary goes in `meta`.

## Open questions

- Exact Godot `user://` location on iOS, and whether the host should pass `Library/Caches` explicitly (owned by `godot-csharp-integrator` and the facade's default `cache_dir`).
- Whether `mmap_size` behaves on iOS under memory pressure; keep it configurable and default it to 0 on iOS if in doubt.
- Whether one shared cache across several world instances (main view + map viewport) is worth a non-exclusive mode later, or whether each instance keeps its own folder as the 0.7 rules say. A shared cache would also need a different folder lock.
