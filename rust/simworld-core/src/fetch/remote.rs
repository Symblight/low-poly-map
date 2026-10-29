//! The PMTiles fetcher: archive reader, tileset check, cache validation and the tile fetch.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use pmtiles::{AsyncPmTilesReader, Compression, HashMapCache, HttpBackend, TileCoord};

use crate::data::DataLevels;
use crate::geo::TileKey;
use crate::mvt::splitmix64;

use super::disk::{DiskCache, Notify};
use super::*;

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

    pub(super) async fn disk(&self) -> Option<Arc<DiskCache>> {
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
    ///
    /// A cache written before api 1.11 has no `levels` row; its tiles come from a
    /// compatibility archive (the only kind those libraries accepted). Since 1.11 the row is
    /// written before the first tile of a source (the archive must be open to fetch one), so
    /// tiles without the row mean a pre-1.11 cache: reported as [`DataLevels::COMPAT`], so an
    /// offline start on it still renders the cached tiles (review H1).
    pub async fn cached_levels(&self) -> Option<DataLevels> {
        let d = self.disk().await?;
        if let Some(v) = d.get_meta("levels").await {
            return parse_levels(&v);
        }
        d.has_tiles().await.then_some(DataLevels::COMPAT)
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
