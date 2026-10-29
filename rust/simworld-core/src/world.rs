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
mod camera;
mod configure;
mod delivery;
pub mod lod;
mod poll_profile;
mod pump;
mod query;
mod seed_info;
mod stats;

pub use call_profile::CallTimer;
pub use delivery::ReadyTileView;
pub use seed_info::{AirportInfo, RunwayEndInfo, RunwayInfo, SeedCityInfo, SeedFileReader, SeedInfo};
pub use stats::WorldStats;

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
    /// Deliver meshes as engine surfaces instead of arrays (`configure_world` option
    /// `engine_surfaces`, api 1.11; `crate::surface`).
    pub engine_surfaces: bool,
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
/// Lower bound of `set_terrain_detail`.
pub const TERRAIN_DETAIL_MIN: f64 = 0.25;
/// Range of `set_lod_quality` (the tau multiplier; api 1.10).
pub const LOD_QUALITY_MIN: f64 = 0.25;
pub const LOD_QUALITY_MAX: f64 = 4.0;

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
