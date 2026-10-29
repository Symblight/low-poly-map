//! SQLite helpers of the disk cache: files, error kinds, opening, eviction and the write batch.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bytes::Bytes;

use super::disk::*;
use super::CACHE_SCHEMA_VERSION;

pub(super) fn sidecar(db: &Path, suffix: &str) -> PathBuf {
    let mut s = db.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Delete a database and its `-wal` / `-shm` files.
pub(super) fn delete_db_files(db: &Path) {
    for p in [db.to_path_buf(), sidecar(db, "-wal"), sidecar(db, "-shm"), sidecar(db, "-journal")] {
        let _ = fs::remove_file(p);
    }
}

pub(super) fn sqlite_code(e: &rusqlite::Error) -> Option<rusqlite::ErrorCode> {
    e.sqlite_error_code()
}

pub(super) fn is_corrupt(e: &rusqlite::Error) -> bool {
    matches!(sqlite_code(e), Some(rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase))
}

pub(super) fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(sqlite_code(e), Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
}

/// Run a statement to completion, ignoring rows (pragmas that return rows, `incremental_vacuum`
/// which frees pages step by step).
pub(super) fn run_all(conn: &rusqlite::Connection, sql: &str) -> rusqlite::Result<()> {
    let mut st = conn.prepare(sql)?;
    let mut rows = st.query([])?;
    while rows.next()?.is_some() {}
    Ok(())
}

pub(super) fn pragma_i64(conn: &rusqlite::Connection, name: &str) -> rusqlite::Result<i64> {
    conn.query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))
}

/// Open (or create) the database with the cache settings, take the exclusive lock, create the
/// schema, wipe tiles of another schema version. Returns the connection, byte total and count.
pub(super) fn open_conn(path: &Path) -> rusqlite::Result<(rusqlite::Connection, u64, u64)> {
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
pub(super) fn evict_to(conn: &rusqlite::Connection, target: u64, total: &mut u64, count: &mut u64) -> rusqlite::Result<u64> {
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


/// One write batch: puts (with their size delta), access times, eviction to the low-water
/// mark when over the cap. Returns the new (total, count, freed bytes); nothing changes in
/// memory unless it commits.
pub(super) fn write_batch(
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
