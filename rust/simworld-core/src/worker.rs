//! Threads: a small tokio runtime for I/O (fetch; the disk cache has its own thread, owned by
//! [`crate::fetch::DiskCache`]) and one [`JobPool`] (priority queue + fixed worker threads,
//! [`crate::jobs`]) for the CPU work of both layers (MVT decode / synthetic generation /
//! meshing / lights, terrain TINs). Results go back to the main thread over one crossbeam
//! channel. Workers only produce plain Rust structs; they never call Godot APIs, and panics are
//! caught and turned into error messages.
//!
//! Every CPU job ends with exactly one reply: its result (`Decoded` / `TerrainBuilt`), or
//! [`WorkerMsg::Dropped`] when it was cancelled ([`Worker::cancel`]: the scheduler orphaned that
//! generation) before or while it ran. Build stages check the cancel flag in between.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use ahash::AHashMap;
use bytes::Bytes;
use crossbeam_channel::{Receiver, Sender};
use tokio::task::AbortHandle;

use crate::data::{build_chunk_with, build_landmark_tile, decode_level, synthetic_data_tile, DataLevel, DataLevels, DataTile};
use crate::jobs::{worker_threads, JobEnd, JobPool, JobSpec, JobStats, Output, PoolConfig, Priority};
use crate::rings::LayerKind;
use crate::fetch::{DiskCache, FetchError, FetchErrorKind, FetchParams, Fetcher, Note, Source};
use crate::geo::{Frame, TileKey};
use crate::lights::LightSet;
use crate::mesher::{mesh_tile, MeshData};
use crate::ground::mesh_ground;
use crate::mvt::{decode_tile_on, Building, DecodedTile, GroundFeatures, TileProjector};
use crate::seed::SeedWorld;
use crate::terrain::tin::{build_terrain_tile_until, TerrainTile};
use crate::terrain::Terrain;
use crate::profile::Profile;
use crate::synthetic;

mod build;
mod levels_probe;
mod probe;
mod release;
mod submit;
mod surfaces;
#[cfg(test)]
mod tests;

pub use build::{
    build_seed_tile_on, build_terrain_data, build_terrain_data_until, build_tile, build_tile_on, decode_payload, decode_payload_on,
    tile_buildings, tile_buildings_proj,
};

/// Decoded tile, ready for conversion on the main thread. Only flat arrays cross the channel:
/// the per-building structs are dropped on the worker (freeing thousands of small allocations
/// on the main thread cost ~1-3 ms per z13 tile).
#[derive(Debug, Default)]
pub struct TileData {
    /// Buildings decoded in the tile (including buffer copies owned by neighbours).
    pub building_count: usize,
    /// R2/R3: light points (single and clustered), Godot axes relative to the tile origin
    /// (tile center). Empty for R0/R1, which get windows instead.
    pub lights: Arc<LightSet>,
    /// Merged building mesh (tile-root relative), clipped to the tile. Empty for R3. Shared with
    /// the recent-tile cache (`scheduler::recent`).
    pub mesh: Arc<MeshData>,
    /// Merged ground mesh (areas + road strips, tile-root relative, no UV), clipped to the
    /// tile. Empty for tiles without ground features (every band has ground since api 1.3).
    pub ground: Arc<MeshData>,
    /// Highest roof top (m above ground) of any building decoded in the tile, including
    /// buffer copies owned by neighbours; 0 for an empty tile. Height of the slant-distance box.
    pub max_height: f32,
    /// Worker-side CPU time (decode + mesh + lights), microseconds.
    pub cpu_us: u64,
    /// Terrain layer only (api 1.5): the terrain mesh of the tile (`None` for vector tiles).
    pub terrain: Option<Arc<TerrainTile>>,
}

impl TileData {
    pub fn triangle_count(&self) -> usize {
        self.mesh.triangle_count()
    }
    pub fn vertex_count(&self) -> usize {
        self.mesh.vertex_count()
    }
}

#[derive(Debug)]
pub enum Payload {
    Bytes(Bytes),
    Synthetic,
}

#[derive(Debug)]
pub enum Job {
    Fetch { key: TileKey, ring: usize, job: u64 },
    Decode { key: TileKey, ring: usize, job: u64, payload: Payload },
    /// Terrain layer (api 1.5): build the TIN of a terrain tile at `detail` times the band's
    /// `max_error_m`. No fetch stage (the terrain is a function).
    Terrain { key: TileKey, ring: usize, job: u64, detail: f64 },
    /// Tiles v2 (T2a): fetch the raw bytes of data tile `key` (I/O runtime, disk cache first).
    FetchData { key: TileKey, job: u64 },
    /// Tiles v2: decode data tile `key` of `level` into an `Arc<DataTile>` (CPU pool); the output
    /// goes to the dependent [`Job::MeshChunk`]s and back to the main thread
    /// ([`WorkerMsg::DataDecoded`]). The synthetic city (no tile source) generates it instead
    /// (`bytes` empty, step T2c).
    DecodeData { key: TileKey, job: u64, bytes: Bytes, level: DataLevel },
    /// Tiles v2: cut render chunk `key` (band `ring`) from its data tile, with the landmarks of
    /// `lm` folded in (T2b, no landmark layer). Replies like a [`Job::Decode`] (`Decoded` or
    /// `Dropped`).
    MeshChunk { key: TileKey, ring: usize, job: u64, data: DataRef, lm: LmRef },
    /// Landmark layer (api 1.11): mesh the landmarks of overview data tile `key` (the `l/` key).
    /// Replies [`WorkerMsg::LandmarksBuilt`] or `Dropped { layer: Landmarks }`.
    MeshLandmarks { key: TileKey, job: u64, data: DataRef },
}

/// Which landmarks a [`Job::MeshChunk`] draws.
#[derive(Debug, Clone)]
pub enum LmRef {
    /// None (the landmark layer draws them, or the source has none).
    None,
    /// Those of the chunk's own data tile (an overview chunk).
    Same,
    /// Those of another data tile (the overview tile of a detail chunk).
    Other(DataRef),
}

/// The data tile a [`Job::MeshChunk`] is cut from.
#[derive(Debug, Clone)]
pub enum DataRef {
    /// Already decoded (resident on the main thread).
    Ready(Arc<DataTile>),
    /// The output of the pending [`Job::DecodeData`] with this job id (a pool dependency: a
    /// failed or cancelled decode cancels the chunk job).
    After(u64),
}

impl Job {
    /// Tile key of the job (the data tile for `FetchData` / `DecodeData`).
    pub fn key(&self) -> TileKey {
        match *self {
            Job::Fetch { key, .. }
            | Job::Decode { key, .. }
            | Job::Terrain { key, .. }
            | Job::FetchData { key, .. }
            | Job::DecodeData { key, .. }
            | Job::MeshChunk { key, .. }
            | Job::MeshLandmarks { key, .. } => key,
        }
    }
    /// A data-tile job (`FetchData` / `DecodeData`) rather than a job of a tracked tile.
    pub fn is_data(&self) -> bool {
        matches!(self, Job::FetchData { .. } | Job::DecodeData { .. })
    }
}

#[derive(Debug)]
pub enum WorkerMsg {
    Fetched { key: TileKey, ring: usize, job: u64, result: Result<(Bytes, Source), FetchError> },
    Decoded { key: TileKey, ring: usize, job: u64, result: Result<TileData, String> },
    /// Result of a [`Job::Terrain`] (routed to the terrain layer's scheduler).
    TerrainBuilt { key: TileKey, ring: usize, job: u64, result: Result<TileData, String> },
    /// A `Decode` / `MeshChunk` (`layer` World) or `Terrain` job that was cancelled before or
    /// while it ran (a stale generation, a cancelled dependency, or shutdown): frees its slot,
    /// carries no result.
    Dropped { layer: LayerKind, key: TileKey, ring: usize, job: u64 },
    /// Result of a [`Job::FetchData`].
    DataFetched { key: TileKey, job: u64, result: Result<(Bytes, Source), FetchError> },
    /// Result of a [`Job::DecodeData`] (sent before the dependent chunk jobs run).
    DataDecoded { key: TileKey, job: u64, result: Result<Arc<DataTile>, String> },
    /// A [`Job::DecodeData`] that was cancelled before or while it ran.
    DataDropped { key: TileKey, job: u64 },
    /// Result of a [`Job::MeshLandmarks`] (an empty `TileData` = the tile has no landmarks).
    LandmarksBuilt { key: TileKey, job: u64, result: Result<TileData, String> },
    /// The archive opened and passed the tileset check with these data levels (or, offline,
    /// the levels a warm disk cache recorded). Sent once per worker.
    Levels(DataLevels),
    /// Opening the archive failed while its levels are still unknown (status).
    OpenFailed(FetchError),
    Info(String),
    /// Logged as a (rate-limited) warning on the main thread.
    Warn(String),
    /// The disk cache was disabled for the session: logged as a warning, status degraded.
    CacheDisabled(String),
    /// A configuration problem (logged as an error on the main thread).
    Error(String),
}

/// The fetch map stays usable after a panic elsewhere (it only holds abort handles).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Pool job id of a layer's job (the two schedulers number their jobs independently).
fn pool_id(layer: LayerKind, job: u64) -> u64 {
    match layer {
        LayerKind::World | LayerKind::Landmarks => job & !TERRAIN_TAG,
        LayerKind::Terrain => job | TERRAIN_TAG,
    }
}

const TERRAIN_TAG: u64 = 1 << 63;

pub struct Worker {
    rt: Option<tokio::runtime::Runtime>,
    /// CPU work of both layers (decode / mesh / terrain), one priority queue.
    pool: JobPool,
    tx: Sender<WorkerMsg>,
    rx: Receiver<WorkerMsg>,
    fetcher: Option<Arc<Fetcher>>,
    frame: Frame,
    /// Terrain every decode job meshes on (cloned into the job).
    terrain: Terrain,
    /// Seed world (api 1.7): synthetic-payload tiles come from its generator, terrain tiles get
    /// its COLOR.
    seed: Option<Arc<SeedWorld>>,
    profile: Arc<Profile>,
    cache_bytes: Arc<AtomicU64>,
    /// A disk cache folder is in use (tile source + non-empty `cache_dir`).
    disk_cache: bool,
    /// Source and cache settings, to decide whether a re-configure may reuse the open cache.
    url: String,
    cache_dir: String,
    cache_cap: u64,
    /// Fetches in flight by job id, for [`Worker::cancel`] (each task removes its own entry).
    fetches: Arc<Mutex<AHashMap<u64, AbortHandle>>>,
    /// Ids of [`Worker::release`] jobs.
    release_seq: AtomicU64,
    /// Pack built meshes into engine surfaces (`surfaces`, api 1.11).
    engine_surfaces: bool,
}

impl Worker {
    /// `url` empty = synthetic city (no runtime / fetcher needed).
    pub fn new(frame: Frame, url: &str, cache_dir: &str, profile: Arc<Profile>) -> Result<Self, String> {
        Self::with_cache(frame, url, cache_dir, profile, None)
    }

    /// The terrain decode jobs mesh on (flat unless set; set before the first job).
    pub fn set_terrain(&mut self, terrain: Terrain) {
        self.terrain = terrain;
    }

    /// The seed world tiles are generated from (set before the first job; `None` = the old
    /// synthetic city for an empty `tile_url`).
    pub fn set_seed(&mut self, seed: Option<Arc<SeedWorld>>) {
        self.seed = seed;
    }

    /// [`Worker::new`], reusing a disk cache that is already open (from
    /// [`Worker::shutdown`] with `keep_cache`; same source, folder and cap).
    pub fn with_cache(
        frame: Frame,
        url: &str,
        cache_dir: &str,
        profile: Arc<Profile>,
        reuse: Option<Arc<DiskCache>>,
    ) -> Result<Self, String> {
        let (tx, rx) = crossbeam_channel::unbounded();
        // Profile `streaming.worker_threads` (0 = cores - 2, clamped to 2..=8). iOS will set a
        // QoS class per thread through `PoolConfig::on_thread_start` (not built).
        let pool = JobPool::new(PoolConfig::new(worker_threads(profile.streaming.worker_threads), "simplane-cpu"))?;
        let cache_cap = FetchParams::from_profile(&profile).cache_cap_bytes;
        let cache_bytes = reuse.as_ref().map_or_else(|| Arc::new(AtomicU64::new(0)), |c| c.bytes_counter());
        let (rt, fetcher) = if url.is_empty() {
            (None, None)
        } else {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .max_blocking_threads(4)
                .thread_name("simplane-io")
                .enable_all()
                .build()
                .map_err(|e| format!("tokio runtime: {e}"))?;
            let _g = rt.enter();
            let info_tx = tx.clone();
            let f = Fetcher::new(
                url,
                cache_dir,
                cache_bytes.clone(),
                FetchParams::from_profile(&profile),
                Box::new(move |n| {
                    let _ = info_tx.send(match n {
                        Note::Info(s) => WorkerMsg::Info(s),
                        Note::Warn(s) => WorkerMsg::Warn(s),
                        Note::CacheDisabled(s) => WorkerMsg::CacheDisabled(s),
                        Note::Error(s) => WorkerMsg::Error(s),
                    });
                }),
            )?;
            let f = match reuse {
                Some(c) if !cache_dir.is_empty() => f.with_disk_cache(c),
                _ => f,
            };
            let f = Arc::new(f);
            Self::spawn_levels_probe(&rt, f.clone(), tx.clone());
            (Some(rt), Some(f))
        };
        let disk_cache = fetcher.is_some() && !cache_dir.is_empty();
        Ok(Self {
            rt,
            pool,
            tx,
            rx,
            fetcher,
            frame,
            terrain: Terrain::flat(),
            seed: None,
            profile,
            cache_bytes,
            disk_cache,
            url: url.to_string(),
            cache_dir: cache_dir.to_string(),
            cache_cap,
            fetches: Arc::new(Mutex::new(AHashMap::new())),
            release_seq: AtomicU64::new(0),
            engine_surfaces: false,
        })
    }

    pub fn is_synthetic(&self) -> bool {
        self.fetcher.is_none()
    }

    /// Whether this worker may write to a disk cache folder.
    pub fn uses_disk_cache(&self) -> bool {
        self.disk_cache
    }

    /// Whether a configuration with this source, cache folder and cap may take over this
    /// worker's open disk cache (the file stays open; no close / reopen).
    pub fn can_reuse_cache(&self, url: &str, cache_dir: &str, cap: u64) -> bool {
        self.disk_cache
            && self.url == url
            && self.cache_dir == cache_dir
            && self.cache_cap == cap
            && self.fetcher.as_ref().and_then(|f| f.disk_handle()).is_some_and(|d| !d.is_disabled())
    }

    /// Stop with a bounded wait (`wait` in total): the I/O runtime stops first (async tasks
    /// such as HTTP requests are cancelled at their next await; a cache open in progress
    /// finishes), then the disk cache either is handed back open (`keep_cache`, for a
    /// re-configure with the same source and folder) or is closed (pending writes flushed,
    /// WAL checkpointed, file closed). Returns whether everything finished within `wait`, and
    /// the kept cache. CPU jobs are cancelled (queued ones at once, running ones at their next
    /// safe point) but not waited for; they never touch the disk and their results are
    /// discarded. Dropping a `Worker` instead only signals the stop and the cache close,
    /// without waiting.
    pub fn shutdown(mut self, wait: std::time::Duration, keep_cache: bool) -> (bool, Option<Arc<DiskCache>>) {
        let t0 = std::time::Instant::now();
        self.pool.shutdown(std::time::Duration::ZERO);
        if let Some(rt) = self.rt.take() {
            rt.shutdown_timeout(wait);
        }
        let disk = self.fetcher.take().and_then(|f| f.disk_handle());
        let Some(disk) = disk else { return (t0.elapsed() < wait, None) };
        if keep_cache && !disk.is_disabled() {
            return (t0.elapsed() < wait, Some(disk));
        }
        let closed = disk.close(wait.saturating_sub(t0.elapsed()));
        (closed, None)
    }

    pub fn cache_bytes(&self) -> u64 {
        self.cache_bytes.load(Ordering::Relaxed)
    }

    /// Cancel these jobs of `layer` (generations the scheduler orphaned: the tile left the
    /// cover or its pending work became moot). A fetch is aborted and still gets its `Fetched`
    /// reply, with [`FetchErrorKind::Cancelled`]; a CPU job that has not finished replies
    /// [`WorkerMsg::Dropped`] (queued: at once, running: at its next safe point). Either way
    /// the in-flight slot is freed early; a job that already finished replied normally.
    /// Unknown ids are ignored.
    pub fn cancel(&self, layer: LayerKind, jobs: &[u64]) {
        if jobs.is_empty() {
            return;
        }
        if layer == LayerKind::World {
            let mut map = lock(&self.fetches);
            for j in jobs {
                if let Some(h) = map.remove(j) {
                    h.abort();
                }
            }
        }
        let ids: Vec<u64> = jobs.iter().map(|&j| pool_id(layer, j)).collect();
        self.pool.cancel(&ids);
    }

    /// CPU job counters (`debug_stats` `jobs_*`).
    pub fn job_stats(&self) -> JobStats {
        self.pool.stats()
    }

    /// Worker threads of the CPU pool.
    pub fn cpu_threads(&self) -> usize {
        self.pool.threads()
    }

    /// Tests: occupy every CPU pool thread with a job that ignores cancellation and blocks until
    /// the returned sender is dropped; returns once all of them run. Jobs submitted meanwhile
    /// stay queued, so "jobs in flight" is a fact, not a race.
    #[cfg(test)]
    pub(crate) fn hold_cpu_threads(&self) -> crossbeam_channel::Sender<()> {
        let (gate_tx, gate_rx) = crossbeam_channel::unbounded::<()>();
        let (started_tx, started_rx) = crossbeam_channel::unbounded::<()>();
        for i in 0..self.pool.threads() {
            let (gate, started) = (gate_rx.clone(), started_tx.clone());
            // Ids far above the scheduler's generation numbers.
            self.pool.submit(
                JobSpec::new((1 << 62) + i as u64, Priority::on_demand(f64::NEG_INFINITY)),
                move |_| {
                    let _ = started.send(());
                    let _ = gate.recv();
                    JobEnd::Done(None)
                },
                || {},
            );
        }
        for _ in 0..self.pool.threads() {
            started_rx.recv_timeout(std::time::Duration::from_secs(5)).expect("hold job started");
        }
        gate_tx
    }

    pub fn try_recv(&self) -> Option<WorkerMsg> {
        self.rx.try_recv().ok()
    }
}


impl Drop for Worker {
    fn drop(&mut self) {
        // Never block the main thread on in-flight HTTP requests (`shutdown` is the bounded,
        // waiting stop).
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
        // Signal only: the cache thread flushes and closes the file on its own.
        if let Some(d) = self.fetcher.take().and_then(|f| f.disk_handle()) {
            d.signal_close();
        }
    }
}
