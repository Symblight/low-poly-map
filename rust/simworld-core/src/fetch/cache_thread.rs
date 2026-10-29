//! The cache thread: owns the SQLite connection, batches writes, evicts and recovers.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;

use super::disk::*;
use super::sqlite::*;
use super::Note;

/// The cache thread: owns the connection.
pub(super) struct CacheThread {
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
    pub(super) fn open(path: PathBuf, cap: u64, shared: Arc<Shared>) -> io::Result<Self> {
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

    pub(super) fn run(&mut self, rx: crossbeam_channel::Receiver<Cmd>) {
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
            Cmd::HasTiles(reply) => {
                use rusqlite::OptionalExtension;
                let stored = !self.pending.is_empty()
                    || self.conn.as_ref().is_some_and(|c| c.query_row("SELECT 1 FROM tiles LIMIT 1", [], |_| Ok(())).optional().ok().flatten().is_some());
                let _ = reply.send(stored);
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
