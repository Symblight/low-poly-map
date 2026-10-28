//! `World`: the engine-free streamer a host drives once per frame.
//!
//! Holds the ENU frame, the floating-origin local space, the scheduler and the worker threads,
//! and mirrors the host contract in plain Rust. A host binding (e.g. `simworld-godot`'s
//! `WorldStreamer`) only converts types.
//!
//! Main thread only. All heavy work runs in `worker` threads; nothing here blocks.
//!
//! Per-frame call order: `update_camera` -> `take_origin_shift` -> `pop_ready` -> `drain_fades`
//! -> `drain_unload`. Fades advance and complete inside `update_camera`; a fade started by a
//! `pop_ready` delivery is already in that frame's `drain_fades`. Re-centering happens inside
//! `update_camera`; every position handed out afterwards (`pop_ready` origins, `debug_tiles`
//! rects) is in the post-shift local frame.
//!
//! Local frame = host axes: x = east, y = up, z = -north, meters relative to the current origin.
//! Heights: local y = meters above MSL (the world is flat in local space; the terrain is at
//! `ground_height_at`: the procedural terrain when the host requested the layer `terrain` and the
//! profile enables it, else 0).
//!
//! Layers (api 1.5): with the terrain active (host layer `terrain` + profile `terrain.enabled`) a
//! second [`Scheduler`] runs the terrain-mesh quadtree; its keys are [`LayerKey`]s with the
//! `t/` prefix, delivered through the same `pop_ready` / `drain_fades` / `drain_unload` in one
//! merged order (fresh before re-deliveries, then slant distance from the camera).
//!
//! Lifecycle (api 0.7): several `World`s may run side by side (no process-global state); each
//! cache folder is held by one world through [`crate::lock::FolderLock`]; `shutdown` stops a
//! world with a bounded wait and makes it inert until the next configure.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::budget::{mb_to_bytes, MemoryBudget, MemoryStats};
use crate::diag::{thread_cpu_us, Diagnostics, Log, WarnLimiter};
use crate::fetch::{DiskCache, FetchParams};
use crate::geo::{Frame, LocalSpace, TileKey, MERCATOR_MAX_LAT};
use crate::jobs::{JobStats, Priority};
use crate::latency::Latency;
use crate::lock::{canonical_dir, FolderLock, LockError};
use crate::mesher::MeshData;
use crate::profile::Profile;
use crate::rings::{LayerKey, LayerKind, LodView, MAX_RINGS, RING_COUNT};
use crate::scheduler::{CpuStats, ReadyTile, Scheduler};
use crate::terrain::Terrain;
use crate::status::{Health, Status, StatusState, MSG_NOT_CONFIGURED, MSG_SHUT_DOWN};
use crate::worker::{Job, Worker, WorkerMsg};

mod call_profile;
pub mod lod;
mod poll_profile;

pub use call_profile::CallTimer;

/// Bounded wait of `shutdown` and of a re-configure that replaces a worker using a disk cache
/// (until the old cache thread has flushed and closed its file).
pub const STOP_WAIT: Duration = Duration::from_secs(1);

// Tuning from the world profile: `origin.recenter_distance_m` (re-center the floating origin
// when the camera is farther than this from it, horizontal) and `streaming.redeliveries_per_frame`
// (light-mode re-deliveries per frame; `update_camera` starts a frame, a host may call
// `pop_ready` several times per frame).

/// Attribution required whenever OSM-derived map data is shown.
pub const OSM_ATTRIBUTION: &str = "\u{a9} OpenStreetMap contributors";

/// Options of [`World::configure_world`] (contract `configure_world(options)`; the host
/// converts its own option format into this).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorldOptions {
    /// World origin (WGS84 degrees).
    pub origin_lat: f64,
    pub origin_lon: f64,
    /// `http(s)://` URL of a `.pmtiles` archive; empty = the offline synthetic city.
    pub tile_url: String,
    /// Disk cache folder; empty = no disk cache.
    pub cache_dir: String,
    /// World profile file read from disk (Rust hosts, CLI). Ignored when `profile_json` is set.
    pub profile_path: Option<String>,
    /// World profile as JSON text: (label for messages, e.g. the path; text). Hosts with their
    /// own virtual file system (Godot `res://`) read the file and pass it here.
    pub profile_json: Option<(String, String)>,
    /// Partial profile merged onto the file (or the defaults).
    pub profile_overrides: Option<serde_json::Value>,
    /// Replaces the diagnostics switches when set.
    pub diagnostics: Option<Diagnostics>,
    /// Layers the host draws (`configure_world` option `layers`, api 1.4): `None` = `["world"]`.
    /// Known names: `world` (the vector world, always on), `terrain` (procedural relief;
    /// active only together with the profile's `terrain.enabled`) and **[1.11]** `landmarks`
    /// (the host draws the landmark layer, `l/z/x/y`; without it landmarks of a two-level
    /// archive are folded into the vector chunks). An unknown name is an error.
    pub layers: Option<Vec<String>>,
    /// Centre `(lat, lon)` of the terrain's flat zone (`configure_world` option `terrain_center`,
    /// api 1.6): `None` = the world origin. A host that reconfigures with a new origin (a
    /// teleport) passes the same centre to keep the relief where it was.
    pub terrain_center: Option<[f64; 2]>,
    /// World seed manifest (`configure_world` option `seed_world`, api 1.7): path of a
    /// `world.json` (`docs/spec-world-seed.md`). `None` / empty = no seed world. Requires an
    /// empty `tile_url`, the layer `terrain` with `terrain.enabled`, and `terrain.flat_radius_m` =
    /// `terrain.flat_blend_m` = 0.
    pub seed_world: Option<String>,
    /// Reads the manifest and the files it names (resolved paths: relative paths are joined to
    /// the manifest's folder). `None` = the file system (Rust hosts, tests, CLI); the Godot
    /// binding reads through `FileAccess` (`res://`, `user://`).
    pub seed_reader: Option<SeedFileReader>,
}

/// Host file reader for the seed world's files (see [`WorldOptions::seed_reader`]).
#[derive(Clone)]
pub struct SeedFileReader(pub Arc<crate::seed::world::SeedReader>);

impl std::fmt::Debug for SeedFileReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SeedFileReader")
    }
}

impl PartialEq for SeedFileReader {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// One runway end of [`AirportInfo`] (contract `airports()`, api 1.7).
#[derive(Debug, Clone, PartialEq)]
pub struct RunwayEndInfo {
    /// Designator of this end (`"08"`, `"26L"`).
    pub ident: String,
    /// Physical end (WGS84) and its local position (`y` = `ground_height_at` there).
    pub lat: f64,
    pub lon: f64,
    pub position: [f64; 3],
    /// Landing threshold (the end moved inward by the displaced threshold).
    pub threshold_lat: f64,
    pub threshold_lon: f64,
    pub threshold_position: [f64; 3],
    pub displaced_threshold_m: f64,
    /// Aiming point of the touchdown zone: 300 m past the threshold (a quarter of the landing
    /// distance on shorter runways).
    pub touchdown_position: [f64; 3],
    /// True heading of a landing on this end (degrees, 0 = north, clockwise).
    pub heading_deg: f64,
}

/// One runway of [`AirportInfo`].
#[derive(Debug, Clone, PartialEq)]
pub struct RunwayInfo {
    /// `"<le>/<he>"`.
    pub designator: String,
    pub le: RunwayEndInfo,
    pub he: RunwayEndInfo,
    pub length_m: f64,
    pub width_m: f64,
    /// Surface as in the airport file, and whether the generator reads it as paved.
    pub surface: String,
    pub paved: bool,
    pub lighted: bool,
    /// An end was missing in the file: placed at the reference point along the heading.
    pub approximate: bool,
    /// Lights of this runway (local; `y` = ground + `seed.airport.lights.height_m`) and their
    /// colors (rgb, a = intensity): the points of the tiles' `airport_lights`.
    pub lights: Vec<[f64; 3]>,
    pub light_colors: Vec<[f32; 4]>,
}

/// One airport of a seed world (contract `airports()`, api 1.7).
#[derive(Debug, Clone, PartialEq)]
pub struct AirportInfo {
    pub ident: String,
    pub name: String,
    /// OurAirports type (`large_airport`, `medium_airport`, `small_airport`).
    pub kind: String,
    /// Reference point (WGS84) and its local position (`y` = `ground_height_at`).
    pub lat: f64,
    pub lon: f64,
    pub position: [f64; 3],
    /// From the airport file; informational (the terrain decides every height).
    pub elevation_ft: Option<f64>,
    pub runways: Vec<RunwayInfo>,
}

/// One city of [`SeedInfo`].
#[derive(Debug, Clone, PartialEq)]
pub struct SeedCityInfo {
    pub id: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub radius_km: f64,
    pub version: i64,
    /// `"synthetic"` (S1 has no real sources; S5 adds `"real:<url>"`, `"synthetic (missing)"`).
    pub source: String,
    pub airports: Vec<String>,
}

/// Contract `seed_world_info()` (api 1.7).
#[derive(Debug, Clone, PartialEq)]
pub struct SeedInfo {
    pub manifest: String,
    pub generator_version: u32,
    pub phases: Vec<String>,
    pub world_version: i64,
    pub world_seed: String,
    pub cities: Vec<SeedCityInfo>,
    pub airports: usize,
    pub water_polygons: usize,
    pub warnings: Vec<String>,
    pub stats: crate::seed::world::SeedStats,
}

/// Layer names `configure_world` accepts in `layers`.
pub const KNOWN_LAYERS: &[&str] = &["world", "terrain", "landmarks"];

/// Warning logged once per configure when the profile enables the terrain but the host did not
/// request the layer.
pub const TERRAIN_IGNORED_WARNING: &str = "terrain.enabled ignored: host did not request layer \"terrain\"";

/// Result of [`World::configure_world`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfigureResult {
    pub ok: bool,
    /// Why it failed (empty when `ok`).
    pub error: String,
    /// Attribution strings the host must show while map data is visible.
    pub attributions: Vec<String>,
}

/// An empty mesh (the `terrain` of vector tiles).
static EMPTY_MESH: MeshData = MeshData { positions: Vec::new(), normals: Vec::new(), colors: Vec::new(), uvs: Vec::new(), indices: Vec::new() };

/// Lower bound of `set_terrain_detail`.
pub const TERRAIN_DETAIL_MIN: f64 = 0.25;
/// Range of `set_lod_quality` (the tau multiplier; api 1.10).
pub const LOD_QUALITY_MIN: f64 = 0.25;
pub const LOD_QUALITY_MAX: f64 = 4.0;

/// One finished tile, borrowed for the duration of the `pop_ready` callback.
pub struct ReadyTileView<'a> {
    /// `World` (vector tile, key `z/x/y`) or `Terrain` (terrain tile, key `t/z/x/y`).
    pub layer: LayerKind,
    pub key: TileKey,
    /// Vector: 0..3 (`16 - z`); terrain: index in `terrain.bands` (`near_zoom - z`).
    pub ring: usize,
    /// Tile root in the current local frame (x east, y up, z -north; y is 0).
    pub origin: [f64; 3],
    /// Building mesh relative to `origin` (empty for R3 and empty tiles).
    pub mesh: &'a MeshData,
    /// Ground mesh relative to `origin` (empty for tiles without ground features).
    pub ground: &'a MeshData,
    /// Light sprite positions relative to `origin` (R2/R3 only).
    pub lights: &'a [[f32; 3]],
    /// One rgba per light (rgb = color, a = intensity weight).
    pub light_colors: &'a [[f32; 4]],
    /// Street lamp positions relative to `origin` (bands `lights.street.min_ring..=3`).
    pub street_lights: &'a [[f32; 3]],
    /// One rgba per street lamp (rgb = `lights.street.color`, a = intensity weight).
    pub street_light_colors: &'a [[f32; 4]],
    /// Aircraft warning beacons relative to `origin` (bands `lights.beacons.min_ring..=3`).
    pub beacon_lights: &'a [[f32; 3]],
    /// One rgba per beacon (rgb = color, a = blink phase 0..1 of its building).
    pub beacon_light_colors: &'a [[f32; 4]],
    /// Airport lights of a seed world relative to `origin` (api 1.7; empty elsewhere).
    pub airport_lights: &'a [[f32; 3]],
    /// One rgba per airport light (rgb = color, a = intensity weight).
    pub airport_light_colors: &'a [[f32; 4]],
    /// Terrain tiles: VERTEX + INDEX (+ NORMAL with smooth shading) relative to `origin`; empty
    /// for vector tiles.
    pub terrain: &'a MeshData,
    /// Terrain tiles: lowest / highest surface vertex (m above MSL); 0 for vector tiles.
    pub h_min: f64,
    pub h_max: f64,
}

impl ReadyTileView<'_> {
    pub fn layer_key(&self) -> LayerKey {
        LayerKey { layer: self.layer, key: self.key }
    }
}

/// Counters for a debug overlay.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct WorldStats {
    pub queued: [i32; RING_COUNT],
    pub fetching: [i32; RING_COUNT],
    pub meshing: [i32; RING_COUNT],
    pub loaded: [i32; RING_COUNT],
    /// Disk cache size (MiB).
    pub cache_mb: f64,
    /// WGS84 position of the *current* local origin (moves with re-centering).
    pub origin_lat: f64,
    pub origin_lon: f64,
    /// Terrain layer (0 rings when it is not active): per-ring counters as above and the
    /// triangles of every terrain tile the host holds.
    pub terrain_rings: usize,
    pub terrain_queued: [i32; MAX_RINGS],
    pub terrain_fetching: [i32; MAX_RINGS],
    pub terrain_meshing: [i32; MAX_RINGS],
    pub terrain_loaded: [i32; MAX_RINGS],
    /// Terrain splits the tile budget (`terrain.max_tiles`) left out of the last cover plan.
    pub terrain_budget_skipped: usize,
    pub terrain_tris: u64,
    /// Shared CPU job pool (both layers): jobs queued (ready or waiting on dependencies),
    /// running, and cancelled since the world was configured. Zero when not running.
    pub jobs: JobStats,
    /// Tile-box probes requested and not answered yet, both layers (`scheduler::boxes`).
    pub probes_pending: usize,
    /// Recent-tile cache of both layers (`scheduler::recent`): hits, entries, bytes.
    pub recent: crate::scheduler::RecentStats,
    /// The memory budget of the core's CPU-side pools (`World::memory`).
    pub memory: MemoryStats,
    /// Tiles v2 data tiles (vector layer, archive sources): decoded tiles resident now, and
    /// decodes since the world was configured (a re-decode after an eviction counts again).
    pub data_tiles_loaded: u32,
    pub data_tiles_decoded: u64,
    /// Data tiles dropped after `memory.data_idle_s` unneeded / evicted by the memory budget,
    /// and decodes of a tile evicted before (review M3).
    pub data_tiles_evicted_idle: u64,
    pub data_tiles_evicted_budget: u64,
    pub data_tiles_redecoded: u64,
    /// Data tile fetches completed since configure (disk cache or network).
    pub data_tiles_fetched: u64,
    /// Of those: overview tiles, and tiles absent from the archive (empty).
    pub data_tiles_fetched_overview: u64,
    pub data_tiles_fetched_empty: u64,
    /// The landmark layer (api 1.11; zero unless the host requested it and the source has
    /// landmarks).
    pub landmarks: crate::scheduler::LandmarkStats,
    /// The data levels of the source (`None`: per-tile source, or the archive has not opened yet).
    pub data_levels: Option<crate::data::DataLevels>,
}

fn first4(a: [i32; MAX_RINGS]) -> [i32; RING_COUNT] {
    std::array::from_fn(|i| a[i])
}

pub struct World {
    log: Arc<dyn Log>,
    diag: Diagnostics,
    /// Resolved world profile of the current configuration (defaults before `configure`).
    profile: Arc<Profile>,
    attributions: Vec<String>,
    /// Host-supplied version label (shown in the `configure` log line).
    version: String,

    // World origin (WGS84 degrees) of the fixed ENU frame.
    origin_lat: f64,
    origin_lon: f64,

    frame: Option<Frame>,
    local: LocalSpace,
    pending_shift: [f64; 3],
    scheduler: Option<Scheduler>,
    /// The terrain-mesh layer (only while the terrain is active).
    terrain_sched: Option<Scheduler>,
    worker: Option<Worker>,

    jobs: Vec<Job>,
    /// Reused id buffer of `cancel_orphans`.
    cancel_buf: Vec<u64>,
    unload_buf: Vec<LayerKey>,
    fade_buf: Vec<(LayerKey, f32)>,
    key_tmp: Vec<TileKey>,
    fade_tmp: Vec<(TileKey, f32)>,
    /// Tiles the host still holds from a previous `configure`.
    stale_unloads: Vec<LayerKey>,
    /// `set_terrain_detail` scale (kept across configures, clamped to the profile's range).
    terrain_detail: f64,
    /// The host's view for the screen-space error (`set_view`, api 1.10; kept across configures).
    lod_view: LodView,
    /// `set_lod_quality` scale: tau_eff = `lod.tau_px` x this (kept across configures).
    lod_quality: f64,

    /// Light-mode re-deliveries handed out since the last `update_camera`.
    redelivered_this_frame: usize,
    warn_limiter: WarnLimiter,

    /// Exclusive lock on the cache folder in use (tile source + non-empty `cache_dir`).
    folder_lock: Option<FolderLock>,
    /// Data-source health (contract `status()`).
    health: Health,
    /// State and message last returned by `take_status_change`.
    reported: (StatusState, String),
    /// Last streaming latency report in the profile log (every [`LATENCY_LOG_EVERY`]).
    latency_logged: Instant,
    /// `diagnostics.profile`: per-tile host conversion time of `poll_ready`.
    poll_profile: poll_profile::PollProfile,
    /// Terrain of the current configuration (flat unless the host requested the layer and the
    /// profile enables it). `ground_height_at`, geodetic AGL, the cover and every job use it.
    terrain: Terrain,
    /// The seed world of the current configuration (api 1.7).
    seed: Option<Arc<crate::seed::SeedWorld>>,
    /// The last loaded seed world, kept across configures (and a failed or seed-less one): a
    /// reconfigure with the same files and profile keys reuses it instead of rebuilding.
    seed_cache: Option<Arc<crate::seed::SeedWorld>>,
    /// One byte budget for the core's CPU-side pools (profile `memory.budget_mb`; Terrain v2
    /// section 6). Kept across configures (the budget follows the new profile; owners clear
    /// their own pool when their data becomes stale); cleared by `shutdown`.
    memory: MemoryBudget,
    /// Tests: an archive source is read per tile in every band, like api 1.7 (no data tiles).
    per_tile_archive: bool,
}

/// With `diagnostics.profile` on, the streaming latency table is logged this often.
pub const LATENCY_LOG_EVERY: Duration = Duration::from_secs(10);

impl World {
    /// An unconfigured world: every call is a no-op until `configure`.
    pub fn new(log: Arc<dyn Log>, diagnostics: Diagnostics, version: impl Into<String>) -> Self {
        Self {
            log,
            diag: diagnostics,
            profile: Arc::new(Profile::default()),
            attributions: Vec::new(),
            version: version.into(),
            origin_lat: 0.0,
            origin_lon: 0.0,
            frame: None,
            terrain: Terrain::flat(),
            local: LocalSpace::default(),
            pending_shift: [0.0; 3],
            scheduler: None,
            terrain_sched: None,
            worker: None,
            jobs: Vec::new(),
            cancel_buf: Vec::new(),
            unload_buf: Vec::new(),
            fade_buf: Vec::new(),
            key_tmp: Vec::new(),
            fade_tmp: Vec::new(),
            stale_unloads: Vec::new(),
            terrain_detail: 1.0,
            lod_view: LodView::default(),
            lod_quality: 1.0,
            redelivered_this_frame: 0,
            warn_limiter: WarnLimiter::default(),
            folder_lock: None,
            health: Health::idle(MSG_NOT_CONFIGURED),
            reported: (StatusState::Ok, MSG_NOT_CONFIGURED.to_string()),
            latency_logged: Instant::now(),
            poll_profile: Default::default(),
            seed: None,
            seed_cache: None,
            memory: MemoryBudget::new(mb_to_bytes(crate::profile::Memory::default().budget_mb)),
            per_tile_archive: false,
        }
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn diagnostics(&self) -> &Diagnostics {
        &self.diag
    }

    /// The resolved world profile (the defaults until a `configure_world` with a profile).
    pub fn profile(&self) -> &Arc<Profile> {
        &self.profile
    }

    /// Attribution strings for the current data source (empty for the synthetic city, before
    /// configure and after `shutdown`).
    pub fn attributions(&self) -> &[String] {
        &self.attributions
    }

    /// Stage latencies of the last fresh deliveries per band (`None` before configure and after
    /// `shutdown`; a re-configure starts empty).
    pub fn latency(&self) -> Option<&Latency> {
        self.scheduler.as_ref().map(|s| s.latency())
    }

    /// Whether a configuration is running (false before configure and after `shutdown`).
    pub fn is_running(&self) -> bool {
        self.scheduler.is_some()
    }

    /// Canonical path of the cache folder this world holds locked, if any.
    pub fn locked_cache_dir(&self) -> Option<&std::path::Path> {
        self.folder_lock.as_ref().map(|l| l.dir())
    }

    /// Data-source health (contract `status()`).
    pub fn status(&self) -> Status {
        let mut s = self.health.status_at(Instant::now());
        s.terrain_active = self.terrain.is_active();
        s
    }

    /// The status, if its state or message changed since the last call (the host emits
    /// `status_changed` with it). Call on the main thread after `update_camera` / `pop_ready`.
    pub fn take_status_change(&mut self) -> Option<Status> {
        let s = self.status();
        if self.reported.0 == s.state && self.reported.1 == s.message {
            return None;
        }
        self.reported = (s.state, s.message.clone());
        Some(s)
    }

    fn warn(&mut self, msg: &str) {
        self.warn_limiter.warn(&*self.log, self.diag.warnings_per_second, msg);
    }

    /// Apply worker results (cheap: moves boxes around). Bounded per call.
    fn drain_worker(&mut self) {
        let (Some(worker), Some(sched)) = (&self.worker, &mut self.scheduler) else { return };
        let mut tsched = self.terrain_sched.as_mut();
        let now = Instant::now();
        let mut warns: Vec<String> = Vec::new();
        for _ in 0..256 {
            let Some(msg) = worker.try_recv() else { break };
            match msg {
                WorkerMsg::Info(s) => self.log.info(&format!("WorldStreamer: {s}")),
                WorkerMsg::Warn(s) => warns.push(format!("WorldStreamer: {s}")),
                WorkerMsg::CacheDisabled(s) => {
                    warns.push(format!("WorldStreamer: {s}"));
                    self.health.set_cache_disabled(&s);
                }
                WorkerMsg::Error(s) => {
                    self.log.error(&format!("WorldStreamer: {s}"));
                    self.health.set_fatal(&s);
                }
                WorkerMsg::Levels(l) => {
                    if sched.set_data_levels(l) {
                        let what = if l.two_level {
                            format!(
                                "two-level archive (detail z{}, overview z{}{})",
                                l.detail,
                                l.overview,
                                match (l.landmarks, sched.landmark_layer_active()) {
                                    (false, _) => "",
                                    (true, true) => ", landmark layer",
                                    (true, false) => ", landmarks folded into the chunks",
                                }
                            )
                        } else {
                            "archive without simworld_levels (zooms 13..16, compatibility levels)".to_string()
                        };
                        self.log.info(&format!("WorldStreamer: data levels: {what}"));
                    }
                }
                WorkerMsg::OpenFailed(e) => self.health.record_fetch(&Err(e), now),
                m @ (WorkerMsg::TerrainBuilt { .. } | WorkerMsg::Dropped { layer: LayerKind::Terrain, .. }) => {
                    if let WorkerMsg::TerrainBuilt { result: Err(e), .. } = &m {
                        self.health.record_decode_failure(e, now);
                    }
                    if let Some(t) = tsched.as_deref_mut() {
                        warns.extend(t.handle(m, now));
                    }
                }
                m => {
                    match &m {
                        WorkerMsg::Fetched { result, .. } | WorkerMsg::DataFetched { result, .. } => self.health.record_fetch(result, now),
                        WorkerMsg::Decoded { result: Err(e), .. } | WorkerMsg::DataDecoded { result: Err(e), .. } => {
                            self.health.record_decode_failure(e, now)
                        }
                        _ => {}
                    }
                    warns.extend(sched.handle(m, now));
                }
            }
        }
        sched.sync_memory(&mut self.memory);
        for w in warns {
            self.warn(&w);
        }
    }

    /// Both layers' new jobs in one order for the shared worker pool (FIFO): nearest to the
    /// camera first, so the terrain's jobs do not queue behind the vector world's backlog.
    /// Each layer already lists its jobs that way; this merges the two lists (stable).
    fn order_jobs(&mut self) {
        let (Some(w), Some(t)) = (&self.scheduler, &self.terrain_sched) else { return };
        if self.jobs.len() < 2 {
            return;
        }
        self.jobs.sort_by(|a, b| {
            let d = |j: &Job| match *j {
                Job::Terrain { key, .. } => t.camera_distance(key),
                Job::FetchData { key, .. } | Job::DecodeData { key, .. } => w.data_distance(key),
                Job::MeshLandmarks { key, .. } => w.landmark_distance(key),
                Job::Fetch { key, .. } | Job::Decode { key, .. } | Job::MeshChunk { key, .. } => w.camera_distance(key),
            };
            d(a).total_cmp(&d(b))
        });
    }

    /// Cancel the jobs (fetches and mesh jobs) both layers' schedulers orphaned.
    fn cancel_orphans(&mut self) {
        let Some(w) = &self.worker else { return };
        let mut ids = std::mem::take(&mut self.cancel_buf);
        if let Some(s) = &mut self.scheduler {
            w.release(s.take_released_data()); // decoded data tiles are freed off the main thread
            w.release(s.take_released_tiles());
            s.drain_cancelled(&mut ids);
            w.cancel(LayerKind::World, &ids);
            ids.clear();
        }
        if let Some(t) = &mut self.terrain_sched {
            w.release(t.take_released_tiles());
            t.drain_cancelled(&mut ids);
            w.cancel(LayerKind::Terrain, &ids);
            ids.clear();
        }
        self.cancel_buf = ids;
    }

    /// Hand both layers' cold tile-box probes to the CPU pool (`scheduler::boxes`).
    fn submit_probes(&mut self) {
        let Some(w) = &self.worker else { return };
        for s in [self.scheduler.as_mut(), self.terrain_sched.as_mut()].into_iter().flatten() {
            if let Some(b) = s.take_probe_batch() {
                w.probe(b);
            }
        }
    }

    /// Hand the new jobs to the worker; CPU jobs queue by [`Scheduler::job_priority`] (fresh
    /// before re-deliveries, then nearest group first, both layers in one heap).
    fn submit_jobs(&mut self) {
        if let Some(w) = &self.worker {
            for j in self.jobs.drain(..) {
                let prio = match &j {
                    Job::Terrain { key, .. } => self.terrain_sched.as_ref().map(|t| t.job_priority(*key)),
                    Job::FetchData { key, .. } | Job::DecodeData { key, .. } => self.scheduler.as_ref().map(|s| s.data_job_priority(*key)),
                    Job::MeshLandmarks { key, .. } => self.scheduler.as_ref().map(|s| Priority::on_demand(s.landmark_distance(*key))),
                    Job::Fetch { key, .. } | Job::Decode { key, .. } | Job::MeshChunk { key, .. } => {
                        self.scheduler.as_ref().map(|s| s.job_priority(*key))
                    }
                };
                w.submit(j, prio.unwrap_or(Priority::prefetch(f64::INFINITY)));
            }
        } else {
            self.jobs.clear();
        }
    }

    /// Set world origin and data source with the default profile (contract `configure`): a
    /// wrapper over [`World::configure_world`]. Errors are logged.
    pub fn configure(&mut self, origin_lat: f64, origin_lon: f64, tile_url: &str, cache_dir: &str) {
        self.configure_world(WorldOptions {
            origin_lat,
            origin_lon,
            tile_url: tile_url.to_string(),
            cache_dir: cache_dir.to_string(),
            ..WorldOptions::default()
        });
    }

    /// Check options, resolve the profile and load the seed world (if any) without touching the
    /// running world.
    fn prepare(&self, o: &WorldOptions) -> Result<(Profile, Option<Arc<crate::seed::SeedWorld>>), String> {
        if !(o.origin_lat.is_finite() && o.origin_lat.abs() <= MERCATOR_MAX_LAT) {
            return Err(format!("origin_lat = {} is out of range [-{MERCATOR_MAX_LAT:.4}, {MERCATOR_MAX_LAT:.4}]", o.origin_lat));
        }
        if !(o.origin_lon.is_finite() && o.origin_lon.abs() <= 180.0) {
            return Err(format!("origin_lon = {} is out of range [-180, 180]", o.origin_lon));
        }
        let url = o.tile_url.trim();
        let lower = url.to_ascii_lowercase();
        if !url.is_empty() && !(lower.starts_with("http://") || lower.starts_with("https://")) {
            return Err(format!("tile_url `{url}` must be an http:// or https:// URL of a .pmtiles archive (or empty for the synthetic city)"));
        }
        if let Some(d) = &o.diagnostics {
            if d.warnings_per_second == 0 {
                return Err("diagnostics.warnings_per_second = 0 is out of range [1, 1000]".into());
            }
        }
        let file_text;
        let file = match (&o.profile_json, &o.profile_path) {
            (Some((label, text)), _) => Some((label.as_str(), text.as_str())),
            (None, Some(path)) if !path.is_empty() => {
                file_text = std::fs::read_to_string(path).map_err(|e| format!("profile_path {path}: {e}"))?;
                Some((path.as_str(), file_text.as_str()))
            }
            _ => None,
        };
        if let Some([lat, lon]) = o.terrain_center {
            if !(lat.is_finite() && lat.abs() <= MERCATOR_MAX_LAT && lon.is_finite() && lon.abs() <= 180.0) {
                return Err(format!(
                    "terrain_center = ({lat}, {lon}) is out of range (latitude within +-{MERCATOR_MAX_LAT:.4}, longitude within +-180)"
                ));
            }
        }
        if let Some(layers) = &o.layers {
            if let Some(bad) = layers.iter().find(|l| !KNOWN_LAYERS.contains(&l.as_str())) {
                return Err(format!("layers: unknown layer \"{bad}\" (known: {})", KNOWN_LAYERS.join(", ")));
            }
        }
        let profile = crate::profile::resolve(file, o.profile_overrides.as_ref(), &*self.log)?;
        let seed = match o.seed_world.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            None => None,
            Some(path) => Some(self.prepare_seed(o, path, &profile)?),
        };
        Ok((profile, seed))
    }

    /// The seed world rules of `configure_world` (contract 3.1, `docs/spec-world-seed.md` 9.1),
    /// then the load (or the cached world when nothing changed).
    fn prepare_seed(&self, o: &WorldOptions, path: &str, profile: &Profile) -> Result<Arc<crate::seed::SeedWorld>, String> {
        let url = o.tile_url.trim();
        if !url.is_empty() {
            return Err(format!("seed_world: tile_url must be empty (a seed world generates every tile), got `{url}`"));
        }
        if !o.layers.as_ref().is_some_and(|l| l.iter().any(|x| x == "terrain")) {
            return Err("seed_world: needs the terrain layer: pass layers [\"world\", \"terrain\"]".into());
        }
        let t = &profile.terrain;
        if !t.enabled {
            return Err("seed_world: needs terrain.enabled = true in the world profile".into());
        }
        for (k, v) in [("flat_radius_m", t.flat_radius_m), ("flat_blend_m", t.flat_blend_m)] {
            if v != 0.0 {
                return Err(format!(
                    "seed_world: terrain.{k} = {v} must be 0 (the flat zone follows the configure origin; a seed world levels only its runways)"
                ));
            }
        }
        let field = Terrain::from_profile(profile, o.origin_lat, o.origin_lon);
        let fs_reader = |p: &str| -> Result<String, String> {
            if p.starts_with("res://") || p.starts_with("user://") {
                return Err(format!("{p}: res:// and user:// paths need the host's file reader"));
            }
            std::fs::read_to_string(p).map_err(|e| e.to_string())
        };
        let reader: &crate::seed::world::SeedReader = match &o.seed_reader {
            Some(r) => &*r.0,
            None => &fs_reader,
        };
        crate::seed::SeedWorld::load(path, reader, profile, &field, self.seed_cache.as_ref())
    }

    /// Set world origin, data source, world profile and diagnostics (contract
    /// `configure_world`). Everything is validated first: on an error nothing changes (the
    /// previous configuration keeps running) and the error is returned and logged. On success
    /// the old workers are dropped and tiles delivered before are reported by the next
    /// `drain_unload` (teleport); the disk cache is kept (the open file is reused when the tile
    /// source, `cache_dir` and cap are unchanged, else closed with a bounded wait first).
    pub fn configure_world(&mut self, o: WorldOptions) -> ConfigureResult {
        let fail = |w: &Self, e: String| {
            w.log.error(&format!("WorldStreamer: configure_world failed: {e}"));
            ConfigureResult { ok: false, error: e, attributions: w.attributions.clone() }
        };
        let (profile, seed) = match self.prepare(&o) {
            Ok(p) => p,
            Err(e) => return fail(self, e),
        };
        // Cache folder lock, synchronously and before anything changes or opens. A seed world
        // takes it when a real city source could become active (a city file with `real.url`).
        let seed_urls = seed.as_ref().is_some_and(|s| s.cities().iter().any(|c| c.real.as_ref().is_some_and(|r| r.url.is_some())));
        let lock = match self.plan_lock(&o, seed_urls) {
            Ok(l) => l,
            Err(e) => return fail(self, e),
        };
        let custom = profile != Profile::default();
        self.profile = Arc::new(profile);
        self.memory.set_budget_bytes(mb_to_bytes(self.profile.memory.budget_mb));
        if let Some(d) = o.diagnostics.clone() {
            self.diag = d;
        }
        let (origin_lat, origin_lon, cache_dir) = (o.origin_lat, o.origin_lon, o.cache_dir.as_str());
        let url = o.tile_url.trim().to_string();
        // Old workers stop before the new ones start (and before an old lock is released).
        let cap = FetchParams::from_profile(&self.profile).cache_cap_bytes;
        let (_, reuse) = self.stop_running(Some((&url, cache_dir, cap)));
        match lock {
            LockPlan::Keep => {}
            LockPlan::New(l) => self.folder_lock = Some(l),
            LockPlan::None => self.folder_lock = None,
        }

        self.origin_lat = origin_lat;
        self.origin_lon = origin_lon;
        let frame = Frame::new(origin_lat, origin_lon);
        self.frame = Some(frame);
        self.local = LocalSpace::default();
        self.pending_shift = [0.0; 3];
        let wants_terrain = o.layers.as_ref().is_some_and(|l| l.iter().any(|x| x == "terrain"));
        let wants_landmarks = o.layers.as_ref().is_some_and(|l| l.iter().any(|x| x == "landmarks"));
        // Validation warnings once per load (a reused world logged them already).
        let fresh_seed = seed.as_ref().is_some_and(|s| !self.seed_cache.as_ref().is_some_and(|c| Arc::ptr_eq(c, s)));
        if let Some(s) = &seed {
            self.seed_cache = Some(s.clone());
        }
        self.seed = seed;
        self.terrain = if wants_terrain && self.profile.terrain.enabled {
            let [c_lat, c_lon] = o.terrain_center.unwrap_or([origin_lat, origin_lon]);
            let t = Terrain::from_profile(&self.profile, c_lat, c_lon);
            match &self.seed {
                Some(s) => t.with_seed(s.terrain.clone()),
                None => t,
            }
        } else {
            if self.profile.terrain.enabled {
                self.log.warn(&format!("WorldStreamer: {TERRAIN_IGNORED_WARNING}"));
            }
            Terrain::flat()
        };

        self.attributions = match &self.seed {
            Some(s) => s.attributions.clone(),
            None if url.is_empty() => Vec::new(),
            None => vec![OSM_ATTRIBUTION.to_string()],
        };
        let hold = Duration::from_secs_f64(self.profile.streaming.retry_max_s + 5.0);
        self.health = Health::running(hold);
        let mut error = String::new();
        match Worker::with_cache(frame, &url, cache_dir, self.profile.clone(), reuse) {
            Ok(mut w) => {
                w.set_terrain(self.terrain.clone());
                w.set_seed(self.seed.clone());
                let mut s =
                    Scheduler::new(frame, w.is_synthetic(), self.profile.clone()).with_terrain(self.terrain.clone()).with_async_boxes();
                s.set_lod_view(&self.lod_view, self.cover_quality());
                // Tiles v2: chunks are cut from data tiles. An archive's levels are known once it
                // opened (`WorkerMsg::Levels`: two-level, or the compatibility levels of an archive
                // without `simworld_levels`); the synthetic city is a two-level source generated
                // in memory (T2c); a seed world keeps its per-tile generator.
                if self.seed.is_none() {
                    s = if w.is_synthetic() {
                        s.with_data_levels(crate::data::DataLevels::SYNTHETIC)
                    } else if self.per_tile_archive {
                        s
                    } else {
                        s.with_data_pending()
                    };
                    s = s.with_landmark_layer(wants_landmarks);
                }
                self.scheduler = Some(s);
                if self.terrain.is_active() {
                    self.terrain_detail = self.clamp_detail(self.terrain_detail);
                    let mut t = Scheduler::new_terrain(frame, self.profile.clone(), self.terrain.clone()).with_async_boxes();
                    t.set_detail(self.terrain_detail);
                    t.set_lod_view(&self.lod_view, self.cover_quality());
                    self.terrain_sched = Some(t);
                }
                self.worker = Some(w);
            }
            Err(e) => {
                error = format!("cannot start workers: {e}");
                self.log.error(&format!("WorldStreamer: {error}"));
                self.health.set_fatal(&error);
                self.folder_lock = None;
                // A fatal world has no relief: `ground_height_at` answers 0 like a world without terrain.
                self.terrain = Terrain::flat();
                self.seed = None;
            }
        }
        let source = match &self.seed {
            Some(s) => format!(
                "seed world {} ({} cities, {} airports, {} water polygons, loaded in {:.0} ms)",
                s.manifest,
                s.cities().len(),
                s.airports.len(),
                s.files.water.polygons.len(),
                s.stats.build_ms
            ),
            None if url.is_empty() => "synthetic city".to_string(),
            None => url,
        };
        if let Some(s) = self.seed.as_ref().filter(|s| fresh_seed && !s.warnings.is_empty()) {
            // At most a few lines: the full list is in `seed_world_info()["warnings"]`.
            const SHOWN: usize = 5;
            for w in s.warnings.iter().take(SHOWN) {
                self.log.warn(&format!("WorldStreamer: seed world: {w}"));
            }
            if s.warnings.len() > SHOWN {
                self.log.warn(&format!(
                    "WorldStreamer: seed world: {} more warnings (seed_world_info().warnings lists all {})",
                    s.warnings.len() - SHOWN,
                    s.warnings.len()
                ));
            }
        }
        self.log.info(&format!(
            "WorldStreamer configured origin=({:.6}, {:.6}) source={} cache_dir={} [{}]",
            origin_lat, origin_lon, source, cache_dir, self.version
        ));
        if custom || o.profile_json.is_some() || o.profile_path.is_some() {
            let label = match (&o.profile_json, &o.profile_path) {
                (Some((l, _)), _) => l.clone(),
                (None, Some(p)) => p.clone(),
                _ => "defaults".to_string(),
            };
            let ov = if o.profile_overrides.as_ref().is_some_and(|v| v.as_object().is_none_or(|m| !m.is_empty())) {
                " + profile_overrides"
            } else {
                ""
            };
            self.log.info(&format!(
                "WorldStreamer world profile: {label}{ov}{}",
                if custom { "" } else { " (equal to the defaults)" }
            ));
        }
        ConfigureResult { ok: error.is_empty(), error, attributions: self.attributions.clone() }
    }

    /// Decide the cache folder lock for `o` without changing anything: keep the held lock (same
    /// folder), take a new one, or none (synthetic city / no `cache_dir`). Errors when another
    /// world or process holds the folder. Other I/O problems (read-only volume, no lock support)
    /// are logged and the world runs without a lock (the cache itself will likely be disabled).
    fn plan_lock(&self, o: &WorldOptions, seed_urls: bool) -> Result<LockPlan, String> {
        if (o.tile_url.trim().is_empty() && !seed_urls) || o.cache_dir.is_empty() {
            return Ok(LockPlan::None);
        }
        let dir = match canonical_dir(std::path::Path::new(&o.cache_dir)) {
            Ok(d) => d,
            Err(e) => {
                self.log.warn(&format!("WorldStreamer: cache_dir {}: {e}; running without the folder lock", o.cache_dir));
                return Ok(LockPlan::None);
            }
        };
        if self.folder_lock.as_ref().is_some_and(|l| l.dir() == dir) {
            return Ok(LockPlan::Keep);
        }
        match FolderLock::try_acquire(&dir) {
            Ok(l) => Ok(LockPlan::New(l)),
            Err(LockError::InUse) => Err(format!("cache_dir in use: {}", o.cache_dir)),
            Err(LockError::Io(e)) => {
                self.log.warn(&format!(
                    "WorldStreamer: cannot lock cache_dir {}: {e}; running without the folder lock",
                    o.cache_dir
                ));
                Ok(LockPlan::None)
            }
        }
    }

    /// Stop the running configuration: delivered tiles go to the next `drain_unload`, the
    /// worker stops (bounded wait if it uses a disk cache, else in the background: nothing to
    /// flush, and a blocked DNS lookup must not stall the frame). The disk cache is closed
    /// (bounded wait) unless `next` (url, cache_dir, cap of the configuration that follows)
    /// can take it over open; then it is returned. Keeps the folder lock (the caller decides).
    /// Returns false if the bounded wait timed out.
    fn stop_running(&mut self, next: Option<(&str, &str, u64)>) -> (bool, Option<Arc<DiskCache>>) {
        // The host keeps whatever we handed out before: unload it with the next drain_unload.
        if let Some(s) = &mut self.scheduler {
            self.stale_unloads.extend(s.delivered_keys().into_iter().map(LayerKey::world));
            self.stale_unloads.extend(s.delivered_landmark_keys().into_iter().map(LayerKey::landmarks));
            // The decoded data tiles go with the scheduler: out of the memory budget.
            s.release_memory(&mut self.memory);
        }
        if let Some(t) = &self.terrain_sched {
            self.stale_unloads.extend(t.delivered_keys().into_iter().map(LayerKey::terrain));
        }
        self.scheduler = None;
        self.terrain_sched = None;
        self.jobs.clear();
        self.pending_shift = [0.0; 3];
        let Some(w) = self.worker.take() else { return (true, None) };
        if !w.uses_disk_cache() {
            drop(w);
            return (true, None);
        }
        let keep = next.is_some_and(|(url, dir, cap)| w.can_reuse_cache(url, dir, cap));
        let (done, kept) = w.shutdown(STOP_WAIT, keep);
        if !done {
            self.log.warn(&format!(
                "WorldStreamer: disk cache still closing after {:.1} s; the next open may run without a disk cache",
                STOP_WAIT.as_secs_f32()
            ));
        }
        (done, kept)
    }

    /// Stop the workers, wait (at most 1 s) until the disk cache has flushed its pending writes
    /// and closed its file, release the cache folder lock. Afterwards the world is inert (every call is a no-op returning
    /// empty values) until the next `configure` / `configure_world`. The tiles delivered
    /// before are still reported once by the next `drain_unload`. Returns false if the wait
    /// timed out. Dropping a `World` instead stops in the background without waiting.
    pub fn shutdown(&mut self) -> bool {
        let (done, _) = self.stop_running(None);
        self.folder_lock = None;
        self.frame = None;
        self.terrain = Terrain::flat();
        self.seed = None;
        self.seed_cache = None;
        self.memory.clear();
        self.local = LocalSpace::default();
        self.origin_lat = 0.0;
        self.origin_lon = 0.0;
        self.attributions.clear();
        self.health = Health::idle(MSG_SHUT_DOWN);
        done
    }

    /// Camera from geodetic input (contract `update_camera_geodetic`): `lat`/`lon` WGS84
    /// degrees, `alt_msl_m` meters above mean sea level, `vel_ned` m/s (north, east, down).
    /// Converted to the local frame through the current origin (AGL = altitude - terrain,
    /// velocity x = E, y = -D, z = -N), then the same path as [`World::update_camera`],
    /// re-centering included. Invalid input (non-finite, |lat| > 90, |lon| > 180) is logged
    /// and ignored (no state change); returns `Err` with the reason then.
    pub fn update_camera_geodetic(
        &mut self,
        lat: f64,
        lon: f64,
        alt_msl_m: f64,
        vel_ned: [f64; 3],
    ) -> Result<Option<[f64; 3]>, String> {
        if self.scheduler.is_none() {
            return Ok(None);
        }
        let checked = check_geodetic(lat, lon, alt_msl_m).and_then(|()| {
            if vel_ned.iter().all(|v| v.is_finite()) {
                Ok(())
            } else {
                Err(format!("vel_ned = {vel_ned:?} is not finite"))
            }
        });
        if let Err(e) = checked {
            let msg = format!("WorldStreamer: update_camera_geodetic ignored: {e}");
            self.warn(&msg);
            return Err(e);
        }
        let Some(p) = self.geodetic_to_local(lat, lon, alt_msl_m) else { return Ok(None) };
        let agl = alt_msl_m - self.terrain.height(lat, lon);
        let vel = [vel_ned[1], -vel_ned[2], -vel_ned[0]];
        Ok(self.update_camera(p, vel, agl))
    }

    /// WGS84 point (degrees, meters above MSL) -> current local frame (x east, y = altitude
    /// above MSL, z -north), through the current floating origin. `None` before configure,
    /// after `shutdown` and for invalid input.
    pub fn geodetic_to_local(&self, lat: f64, lon: f64, alt_msl_m: f64) -> Option<[f64; 3]> {
        let f = self.frame.as_ref()?;
        check_geodetic(lat, lon, alt_msl_m).ok()?;
        let [e, n] = f.enu(lat, lon);
        let [x, z] = self.local.to_local(e, n);
        Some([x, alt_msl_m, z])
    }

    /// Current local frame point -> `[lat, lon, alt_msl]` (degrees, meters). Inverse of
    /// [`World::geodetic_to_local`]. `None` before configure, after `shutdown` and for
    /// non-finite input.
    pub fn local_to_geodetic(&self, p: [f64; 3]) -> Option<[f64; 3]> {
        let f = self.frame.as_ref()?;
        if !p.iter().all(|v| v.is_finite()) {
            return None;
        }
        let [e, n] = self.local.to_enu(p[0], p[2]);
        let (lat, lon) = f.lat_lon(e, n);
        Some([lat, lon, p[1]])
    }

    /// Call every frame. `local_pos` and `velocity` are in the local frame (x east, y up,
    /// z -north). Re-centers the floating origin when needed, recomputes the wanted set and
    /// queues jobs. Returns the origin shift applied by this call (also reported once by
    /// `take_origin_shift`, which this does not consume), `None` if there was none.
    pub fn update_camera(&mut self, local_pos: [f64; 3], velocity: [f64; 3], alt_agl_m: f64) -> Option<[f64; 3]> {
        self.scheduler.as_ref()?;
        let timer = self.call_timer();
        self.redelivered_this_frame = 0;
        self.drain_worker();
        let t_drained = Instant::now();
        let (x, z) = (local_pos[0], local_pos[2]);
        let cam_en = self.local.to_enu(x, z);
        let alt = if alt_agl_m.is_finite() { alt_agl_m.max(0.0) } else { 0.0 };
        let cam = [cam_en[0], cam_en[1], alt];
        // Local velocity (x east, y up, z -north) -> (east, north, up).
        let vel = [velocity[0], -velocity[2], velocity[1]];

        // Floating origin: translate (never rotate) the local frame onto the camera.
        let mut shift = None;
        if crate::math::hypot(x, z) > self.profile.origin.recenter_distance_m {
            self.local.offset_e += x;
            self.local.offset_n -= z;
            self.pending_shift[0] += x;
            self.pending_shift[2] += z;
            shift = Some([x, 0.0, z]);
        }

        let now = Instant::now();
        // `fades.max_active` counts the swaps of both layers.
        if let Some(s) = &mut self.scheduler {
            s.set_fades_elsewhere(self.terrain_sched.as_ref().map_or(0, |t| t.active_fades()));
            s.update(cam, vel, now, &mut self.jobs);
            s.sync_memory(&mut self.memory);
        }
        if let Some(t) = &mut self.terrain_sched {
            t.set_fades_elsewhere(self.scheduler.as_ref().map_or(0, |s| s.active_fades()));
            t.update(cam, vel, now, &mut self.jobs);
        }
        let t_updated = Instant::now();
        self.cancel_orphans();
        self.order_jobs();
        self.submit_jobs();
        self.submit_probes();
        if let Some(t) = timer {
            let end = Instant::now();
            self.end_call_with("update_camera", timer, || {
                let (a, b) = (t.us_until(t_drained), t.us_until(t_updated));
                format!("(drain {a}us, scheduler {}us, submit {}us)", b - a, t.us_until(end) - b)
            });
            if end.duration_since(self.latency_logged) >= LATENCY_LOG_EVERY {
                self.latency_logged = end;
                if let Some(l) = self.latency() {
                    self.log.info(&format!("WorldStreamer profile: {}", l.report()));
                }
                if let Some(p) = self.poll_profile.take_summary() {
                    self.log.info(&format!("WorldStreamer profile: {p}"));
                }
                if let Some(t) = &self.terrain_sched {
                    self.log.info(&format!("WorldStreamer profile: {}", t.cpu_stats().report("terrain", t.layer().rings())));
                }
            }
        }
        shift
    }

    /// Hand at most `max` finished tiles to `f`, in priority order. Returns how many were
    /// handed out. The host converts each tile inside `f` (its time counts in the profile line).
    /// With the terrain layer, both layers share the budget in one order: fresh tiles before
    /// re-deliveries, then strictly by slant distance from the camera (ties: vector world);
    /// re-deliveries of both layers share the per-frame limit.
    pub fn pop_ready(&mut self, max: usize, mut f: impl FnMut(ReadyTileView<'_>)) -> usize {
        let timer = self.call_timer();
        self.drain_worker();
        let t_drained = Instant::now();
        let mut tris = 0usize;
        let local = self.local;
        let per_frame = self.profile.streaming.redeliveries_per_frame as usize;
        let Some(sched) = self.scheduler.as_mut() else { return 0 };
        let mut tsched = self.terrain_sched.as_mut();
        let mut n = 0;
        while n < max {
            // Re-deliveries are rate-limited per frame, not per call.
            let allow = self.redelivered_this_frame < per_frame;
            let w = sched.best_ready(allow);
            let t = tsched.as_deref_mut().and_then(|s| s.best_ready(allow));
            let pick_terrain = terrain_first(w.map(|(_, r, d)| (r, d)), t.map(|(_, r, d)| (r, d)));
            let tile = if pick_terrain {
                let (k, re, _) = t.expect("picked");
                let elsewhere = sched.active_fades();
                tsched.as_deref_mut().and_then(|s| {
                    s.set_fades_elsewhere(elsewhere);
                    s.take_ready(k, re)
                })
            } else {
                let Some((k, re, _)) = w else { break };
                sched.set_fades_elsewhere(tsched.as_deref().map_or(0, |s| s.active_fades()));
                sched.take_ready(k, re)
            };
            let Some(t) = tile else { break };
            n += 1;
            if t.redelivery {
                self.redelivered_this_frame += 1;
            }
            let tile_tris = t.mesh.triangle_count() + t.ground.triangle_count() + t.terrain.as_ref().map_or(0, |x| x.triangle_count());
            tris += tile_tris;
            let cpu0 = if self.diag.profile { thread_cpu_us() } else { 0 };
            f(view_of(&t, local));
            drop(t); // shared with the recent-tile cache: usually a reference count
            if self.diag.profile {
                self.poll_profile.record(thread_cpu_us().saturating_sub(cpu0), tile_tris);
            }
        }
        if let Some(t) = timer {
            self.end_call_with("poll_ready", timer, || {
                let d = t.us_until(t_drained);
                format!("(drain {d}us, convert {n} tiles / {tris} tris {}us)", t.us_until(Instant::now()) - d)
            });
        }
        // A delivery can drop a refined parent whose re-fetch was in flight (`try_drop`).
        self.cancel_orphans();
        n
    }

    /// The worker (tests).
    #[cfg(test)]
    pub(crate) fn worker(&self) -> Option<&Worker> {
        self.worker.as_ref()
    }

    /// The vector layer's scheduler (tests and tools).
    pub fn scheduler(&self) -> Option<&Scheduler> {
        self.scheduler.as_ref()
    }

    /// Tile keys the host must remove (including tiles left over from a previous `configure`).
    /// The slice is valid until the next call.
    pub fn drain_unload(&mut self) -> &[LayerKey] {
        self.unload_buf.clear();
        self.unload_buf.append(&mut self.stale_unloads);
        for (s, layer) in [(&mut self.scheduler, LayerKind::World), (&mut self.terrain_sched, LayerKind::Terrain)] {
            if let Some(s) = s {
                self.key_tmp.clear();
                s.drain_unload(&mut self.key_tmp);
                self.unload_buf.extend(self.key_tmp.iter().map(|&key| LayerKey { layer, key }));
            }
        }
        if let Some(s) = &mut self.scheduler {
            self.key_tmp.clear();
            s.drain_landmark_unload(&mut self.key_tmp);
            self.unload_buf.extend(self.key_tmp.iter().map(|&key| LayerKey::landmarks(key)));
        }
        &self.unload_buf
    }

    /// Every tile in a LOD transition: key -> visible fraction `fade` (0..1). The incoming side
    /// ramps 0 -> 1, the outgoing side 1 -> 0, the two sum to 1. Tiles absent have fade 1.
    /// Finished fades report their final values once (incoming 1.0; outgoing 0.0, then unloaded
    /// by this frame's `drain_unload`). Finals come first; a later entry for the same key wins.
    /// The slice is valid until the next call.
    pub fn drain_fades(&mut self) -> &[(LayerKey, f32)] {
        self.fade_buf.clear();
        for (s, layer) in [(&mut self.scheduler, LayerKind::World), (&mut self.terrain_sched, LayerKind::Terrain)] {
            if let Some(s) = s {
                self.fade_tmp.clear();
                s.drain_fades(&mut self.fade_tmp);
                self.fade_buf.extend(self.fade_tmp.iter().map(|&(key, f)| (LayerKey { layer, key }, f)));
            }
        }
        &self.fade_buf
    }

    /// Non-zero once after a re-center (local x/y/z meters); the host subtracts it from the
    /// camera and all tile roots.
    pub fn take_origin_shift(&mut self) -> [f64; 3] {
        std::mem::replace(&mut self.pending_shift, [0.0; 3])
    }

    pub fn stats(&self) -> WorldStats {
        let st = self.scheduler.as_ref().map(|s| s.stats()).unwrap_or_default();
        let data = self.scheduler.as_ref().map(|s| s.data_stats()).unwrap_or_default();
        let cache_mb = self.worker.as_ref().map_or(0.0, |w| w.cache_bytes() as f64 / 1_048_576.0);
        // Lat/lon of the *current* local origin (moves with re-centering).
        let (origin_lat, origin_lon) = match &self.frame {
            Some(f) if self.local != LocalSpace::default() => f.lat_lon(self.local.offset_e, self.local.offset_n),
            _ => (self.origin_lat, self.origin_lon),
        };
        let ts = self.terrain_sched.as_ref().map(|t| (t.layer().rings(), t.stats()));
        let (terrain_rings, tst) = ts.unwrap_or_default();
        WorldStats {
            queued: first4(st.queued),
            fetching: first4(st.fetching),
            meshing: first4(st.meshing),
            loaded: first4(st.loaded),
            cache_mb,
            origin_lat,
            origin_lon,
            terrain_rings,
            terrain_queued: tst.queued,
            terrain_fetching: tst.fetching,
            terrain_meshing: tst.meshing,
            terrain_loaded: tst.loaded,
            terrain_budget_skipped: self.terrain_sched.as_ref().map_or(0, |t| t.budget_skipped()),
            terrain_tris: tst.tris,
            jobs: self.worker.as_ref().map(|w| w.job_stats()).unwrap_or_default(),
            probes_pending: [self.scheduler.as_ref(), self.terrain_sched.as_ref()].into_iter().flatten().map(|s| s.probes_pending()).sum(),
            recent: [self.scheduler.as_ref(), self.terrain_sched.as_ref()].into_iter().flatten().map(|s| s.recent_stats()).fold(
                crate::scheduler::RecentStats::default(),
                |a, b| crate::scheduler::RecentStats { hits: a.hits + b.hits, entries: a.entries + b.entries, bytes: a.bytes + b.bytes },
            ),
            memory: self.memory.stats(),
            data_tiles_loaded: data.loaded,
            data_tiles_decoded: data.decoded,
            data_tiles_evicted_idle: data.evicted_idle,
            data_tiles_evicted_budget: data.evicted_budget,
            data_tiles_redecoded: data.redecoded,
            data_tiles_fetched: data.fetched,
            data_tiles_fetched_overview: data.fetched_overview,
            data_tiles_fetched_empty: data.fetched_empty,
            landmarks: self.scheduler.as_ref().map(|s| s.landmark_stats()).unwrap_or_default(),
            data_levels: self.scheduler.as_ref().and_then(|s| s.data_levels()),
        }
    }

    /// Tests and benches: read an archive per tile in every band (the api 1.7 behaviour; a
    /// two-level archive then shows nothing). Takes effect at the next configure.
    #[cfg(test)]
    pub(crate) fn set_per_tile_archive(&mut self, on: bool) {
        self.per_tile_archive = on;
    }

    /// The memory budget (Terrain v2 section 6): pool owners register their items with a byte
    /// size and priority, pin them while a job depends on them, and drain what was evicted.
    pub fn memory(&self) -> &MemoryBudget {
        &self.memory
    }

    pub fn memory_mut(&mut self) -> &mut MemoryBudget {
        &mut self.memory
    }

    fn clamp_detail(&self, scale: f64) -> f64 {
        scale.clamp(TERRAIN_DETAIL_MIN, self.profile.terrain.detail_scale_max.max(TERRAIN_DETAIL_MIN))
    }

    /// Contract `set_terrain_detail(scale)` (api 1.5; meaning changed in 1.10, Terrain v2 G2):
    /// the terrain cover's SSE threshold is `terrain.tau_px` x `scale`, clamped to
    /// `[0.25, terrain.detail_scale_max]` (1 = the profile; 2 = coarser, fewer tiles). The cover
    /// follows from the next `update_camera` through ordinary refines and coarsens (new keys,
    /// fades, unloads); held terrain tiles are NOT re-meshed or re-delivered any more (a tile's
    /// TIN uses its band's `max_error_m` at every scale). The vector world is untouched. The
    /// scale is kept across `configure_world` (re-clamped to the new profile). Non-finite input
    /// is ignored. Returns the scale in effect.
    pub fn set_terrain_detail(&mut self, scale: f64) -> f64 {
        if scale.is_finite() {
            self.terrain_detail = self.clamp_detail(scale);
            if let Some(t) = &mut self.terrain_sched {
                t.set_detail(self.terrain_detail);
            }
        }
        self.terrain_detail
    }

    /// The terrain detail scale in effect (see [`World::set_terrain_detail`]).
    pub fn terrain_detail(&self) -> f64 {
        self.terrain_detail
    }

    /// Whether the terrain-mesh layer runs (terrain active and configured).
    pub fn landmark_layer_active(&self) -> bool {
        self.scheduler.as_ref().is_some_and(|s| s.landmark_layer_active())
    }

    /// The terrain-mesh layer runs (api 1.5).
    pub fn terrain_layer_active(&self) -> bool {
        self.terrain_sched.is_some()
    }

    /// Worker CPU per terrain ring since configure (`None` without the terrain layer).
    pub fn terrain_cpu(&self) -> Option<&CpuStats> {
        self.terrain_sched.as_ref().map(|t| t.cpu_stats())
    }

    /// Terrain height (m) under a point in current local x/z meters: `Terrain::height` at the
    /// point's lat/lon (through the current floating origin). 0 with the terrain off (the default); a few µs.
    pub fn ground_height_at(&self, local_x: f64, local_z: f64) -> f64 {
        let Some(f) = &self.frame else { return 0.0 };
        if !local_x.is_finite() || !local_z.is_finite() {
            return 0.0;
        }
        let [e, n] = self.local.to_enu(local_x, local_z);
        let (lat, lon) = f.lat_lon(e, n);
        self.terrain.height(lat, lon)
    }

    /// The terrain of the current configuration (flat when off, before configure and after
    /// `shutdown`). A copy can be sampled from any thread.
    pub fn terrain(&self) -> &Terrain {
        &self.terrain
    }

    /// The seed world of the current configuration (api 1.7).
    pub fn seed_world(&self) -> Option<&Arc<crate::seed::SeedWorld>> {
        self.seed.as_ref()
    }

    /// Where the tiles of a layer come from (`debug_tiles` `source`, api 1.7): vector tiles
    /// `"seed"` (seed world), `"archive"` (`tile_url`) or `"synthetic"` (the offline city);
    /// terrain tiles `"terrain"`. S5 adds `"real:<city>"` per z13 tile.
    pub fn tile_source(&self, layer: LayerKind) -> &'static str {
        match layer {
            LayerKind::Terrain => "terrain",
            LayerKind::World if self.seed.is_some() => "seed",
            LayerKind::World | LayerKind::Landmarks if self.worker.as_ref().is_some_and(|w| w.is_synthetic()) => "synthetic",
            LayerKind::World | LayerKind::Landmarks => "archive",
        }
    }

    /// Contract `airports()` (api 1.7): every airport of the seed world with its runways, in the
    /// current local frame (call again after an origin shift). Empty without a seed world.
    pub fn airports(&self) -> Vec<AirportInfo> {
        let Some(s) = &self.seed else { return Vec::new() };
        let h_light = s.params.airport.lights.height_m;
        let local = |lat: f64, lon: f64, up: f64| -> [f64; 3] {
            let h = self.terrain.height(lat, lon) + up;
            self.geodetic_to_local(lat, lon, h).unwrap_or([0.0; 3])
        };
        s.airports
            .iter()
            .map(|g| {
                let runways = g
                    .runways
                    .iter()
                    .enumerate()
                    .map(|(ri, r)| {
                        let end = |ident: &str, ll: (f64, f64), thr_s: f64, toward: f64, disp: f64, heading: f64| {
                            let (tlat, tlon) = g.frame.ll(r.at(thr_s, 0.0));
                            let land = (r.he_thr_s - r.le_thr_s).abs();
                            let td = 300.0f64.min(land * 0.25);
                            let (dlat, dlon) = g.frame.ll(r.at(thr_s + toward * td, 0.0));
                            RunwayEndInfo {
                                ident: ident.to_string(),
                                lat: ll.0,
                                lon: ll.1,
                                position: local(ll.0, ll.1, 0.0),
                                threshold_lat: tlat,
                                threshold_lon: tlon,
                                threshold_position: local(tlat, tlon, 0.0),
                                displaced_threshold_m: disp,
                                touchdown_position: local(dlat, dlon, 0.0),
                                heading_deg: heading,
                            }
                        };
                        let back = (r.heading_deg + 180.0) % 360.0;
                        let lights: Vec<&crate::seed::airport::AirportLight> = g.lights.iter().filter(|l| l.runway == ri).collect();
                        RunwayInfo {
                            designator: r.key.clone(),
                            le: end(&r.le, r.le_ll, r.le_thr_s, 1.0, r.le_thr_s, r.heading_deg),
                            he: end(&r.he, r.he_ll, r.he_thr_s, -1.0, r.length_m - r.he_thr_s, back),
                            length_m: r.length_m,
                            width_m: r.width_m,
                            surface: r.surface.clone(),
                            paved: r.paved,
                            lighted: r.lighted,
                            approximate: r.approximate,
                            lights: lights
                                .iter()
                                .map(|l| {
                                    let (lat, lon) = g.frame.ll(l.p);
                                    local(lat, lon, h_light)
                                })
                                .collect(),
                            light_colors: lights.iter().map(|l| l.color).collect(),
                        }
                    })
                    .collect();
                AirportInfo {
                    ident: g.ident.clone(),
                    name: g.name.clone(),
                    kind: g.kind.clone(),
                    lat: g.lat,
                    lon: g.lon,
                    position: local(g.lat, g.lon, 0.0),
                    elevation_ft: g.elevation_ft,
                    runways,
                }
            })
            .collect()
    }

    /// Contract `seed_world_info()` (api 1.7); `None` without a seed world.
    pub fn seed_world_info(&self) -> Option<SeedInfo> {
        let s = self.seed.as_ref()?;
        Some(SeedInfo {
            manifest: s.manifest.clone(),
            generator_version: crate::seed::SEED_GEN_VERSION,
            phases: crate::seed::SEED_PHASES.iter().map(|p| p.to_string()).collect(),
            world_version: s.files.manifest.world_version,
            world_seed: s.files.manifest.world_seed.clone(),
            cities: s
                .cities()
                .iter()
                .map(|c| SeedCityInfo {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    lat: c.center.lat,
                    lon: c.center.lon,
                    radius_km: c.radius_km,
                    version: c.version,
                    source: "synthetic".into(),
                    airports: c.airports.clone(),
                })
                .collect(),
            airports: s.airports.len(),
            water_polygons: s.files.water.polygons.len(),
            warnings: s.warnings.clone(),
            stats: s.stats,
        })
    }

    /// Visit every tracked tile: key, ring, state name, rect `[x0, z0, x1, z1]` in current local
    /// x/z meters.
    pub fn debug_tiles(&self, mut f: impl FnMut(LayerKey, usize, &'static str, [f64; 4])) {
        let local = self.local;
        for (s, layer) in [(&self.scheduler, LayerKind::World), (&self.terrain_sched, LayerKind::Terrain)] {
            if let Some(s) = s {
                s.for_each_tile(|key, ring, state, rect| f(LayerKey { layer, key }, ring, state, local.rect_to_local(rect)));
            }
        }
        if let Some(s) = &self.scheduler {
            s.for_each_landmark(|key, state, rect| f(LayerKey::landmarks(key), 0, state, local.rect_to_local(rect)));
        }
    }
}

/// What `configure_world` does with the cache folder lock.
enum LockPlan {
    /// Same folder as the held lock: keep the handle (no self-lockout).
    Keep,
    /// A newly taken lock (replaces the held one after the old workers stopped).
    New(FolderLock),
    /// No lock (synthetic city, no `cache_dir`, or locking unavailable).
    None,
}

/// Geodetic input check: finite, |lat| <= 90, |lon| <= 180.
fn check_geodetic(lat: f64, lon: f64, alt: f64) -> Result<(), String> {
    if !(lat.is_finite() && lat.abs() <= 90.0) {
        return Err(format!("lat = {lat} is out of range [-90, 90]"));
    }
    if !(lon.is_finite() && lon.abs() <= 180.0) {
        return Err(format!("lon = {lon} is out of range [-180, 180]"));
    }
    if !alt.is_finite() {
        return Err(format!("alt_msl_m = {alt} is not finite"));
    }
    Ok(())
}

/// Cross-layer delivery order (`docs/spec-terrain.md` 5.1) between the best candidate of each
/// layer, `(re-delivery?, slant distance from the camera)`: fresh before re-deliveries, then the
/// nearer; ties go to the vector world.
/// True = hand out the terrain candidate first.
pub(crate) fn terrain_first(world: Option<(bool, f64)>, terrain: Option<(bool, f64)>) -> bool {
    match (world, terrain) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some((wr, wd)), Some((tr, td))) => (tr, td) < (wr, wd),
    }
}

/// A scheduler tile as the host view (origin in the current local frame).
fn view_of(t: &ReadyTile, local: LocalSpace) -> ReadyTileView<'_> {
    let [ox, oz] = local.to_local(t.center[0], t.center[1]);
    let (lp, lc) = t.light_arrays();
    let (sp, sc) = t.street_light_arrays();
    let (bp, bc) = t.beacon_arrays();
    let (ap, ac) = t.airport_arrays();
    let (terrain, h_min, h_max) = match &t.terrain {
        Some(x) => (&x.mesh, x.h_min as f64, x.h_max as f64),
        None => (&EMPTY_MESH, 0.0, 0.0),
    };
    ReadyTileView {
        layer: t.layer,
        key: t.key,
        ring: t.ring,
        origin: [ox, 0.0, oz],
        mesh: &t.mesh,
        ground: &t.ground,
        lights: lp,
        light_colors: lc,
        street_lights: sp,
        street_light_colors: sc,
        beacon_lights: bp,
        beacon_light_colors: bc,
        airport_lights: ap,
        airport_light_colors: ac,
        terrain,
        h_min,
        h_max,
    }
}
