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
//! `ground_height`, 0 until a DEM exists).
//!
//! Lifecycle (api 0.7): several `World`s may run side by side (no process-global state); each
//! cache folder is held by one world through [`crate::lock::FolderLock`]; `shutdown` stops a
//! world with a bounded wait and makes it inert until the next configure.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::diag::{thread_cpu_us, Diagnostics, Log, WarnLimiter};
use crate::fetch::{DiskCache, FetchParams};
use crate::geo::{Frame, LocalSpace, TileKey, MERCATOR_MAX_LAT};
use crate::lock::{canonical_dir, FolderLock, LockError};
use crate::mesher::MeshData;
use crate::profile::Profile;
use crate::rings::RING_COUNT;
use crate::scheduler::Scheduler;
use crate::status::{Health, Status, StatusState, MSG_NOT_CONFIGURED, MSG_SHUT_DOWN};
use crate::worker::{Job, Worker, WorkerMsg};

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
}

/// Result of [`World::configure_world`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfigureResult {
    pub ok: bool,
    /// Why it failed (empty when `ok`).
    pub error: String,
    /// Attribution strings the host must show while map data is visible.
    pub attributions: Vec<String>,
}

/// One finished tile, borrowed for the duration of the `pop_ready` callback.
pub struct ReadyTileView<'a> {
    pub key: TileKey,
    pub ring: usize,
    /// Tile root in the current local frame (x east, y up, z -north; y is 0).
    pub origin: [f64; 3],
    /// Building mesh relative to `origin` (empty for R3 and empty tiles).
    pub mesh: &'a MeshData,
    /// Ground mesh relative to `origin` (empty for R3 and tiles without ground features).
    pub ground: &'a MeshData,
    /// Light sprite positions relative to `origin` (R2/R3 only).
    pub lights: &'a [[f32; 3]],
    /// One rgba per light (rgb = color, a = intensity weight).
    pub light_colors: &'a [[f32; 4]],
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
    worker: Option<Worker>,

    jobs: Vec<Job>,
    unload_buf: Vec<TileKey>,
    fade_buf: Vec<(TileKey, f32)>,
    /// Tiles the host still holds from a previous `configure`.
    stale_unloads: Vec<TileKey>,

    /// Light-mode re-deliveries handed out since the last `update_camera`.
    redelivered_this_frame: usize,
    warn_limiter: WarnLimiter,

    /// Exclusive lock on the cache folder in use (tile source + non-empty `cache_dir`).
    folder_lock: Option<FolderLock>,
    /// Data-source health (contract `status()`).
    health: Health,
    /// State and message last returned by `take_status_change`.
    reported: (StatusState, String),
}

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
            local: LocalSpace::default(),
            pending_shift: [0.0; 3],
            scheduler: None,
            worker: None,
            jobs: Vec::new(),
            unload_buf: Vec::new(),
            fade_buf: Vec::new(),
            stale_unloads: Vec::new(),
            redelivered_this_frame: 0,
            warn_limiter: WarnLimiter::default(),
            folder_lock: None,
            health: Health::idle(MSG_NOT_CONFIGURED),
            reported: (StatusState::Ok, MSG_NOT_CONFIGURED.to_string()),
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
        self.health.status_at(Instant::now())
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
                m => {
                    match &m {
                        WorkerMsg::Fetched { result, .. } => self.health.record_fetch(result, now),
                        WorkerMsg::Decoded { result: Err(e), .. } => self.health.record_decode_failure(e, now),
                        _ => {}
                    }
                    warns.extend(sched.handle(m, now));
                }
            }
        }
        for w in warns {
            self.warn(&w);
        }
    }

    fn submit_jobs(&mut self) {
        if let Some(w) = &self.worker {
            for j in self.jobs.drain(..) {
                w.submit(j);
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

    /// Check options and resolve the profile without touching the running world.
    fn prepare(&self, o: &WorldOptions) -> Result<Profile, String> {
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
        crate::profile::resolve(file, o.profile_overrides.as_ref(), &*self.log)
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
        let profile = match self.prepare(&o) {
            Ok(p) => p,
            Err(e) => return fail(self, e),
        };
        // Cache folder lock, synchronously and before anything changes or opens.
        let lock = match self.plan_lock(&o) {
            Ok(l) => l,
            Err(e) => return fail(self, e),
        };
        let custom = profile != Profile::default();
        self.profile = Arc::new(profile);
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

        self.attributions = if url.is_empty() { Vec::new() } else { vec![OSM_ATTRIBUTION.to_string()] };
        let hold = Duration::from_secs_f64(self.profile.streaming.retry_max_s + 5.0);
        self.health = Health::running(hold);
        let mut error = String::new();
        match Worker::with_cache(frame, &url, cache_dir, self.profile.clone(), reuse) {
            Ok(w) => {
                self.scheduler = Some(Scheduler::new(frame, w.is_synthetic(), self.profile.clone()));
                self.worker = Some(w);
            }
            Err(e) => {
                error = format!("cannot start workers: {e}");
                self.log.error(&format!("WorldStreamer: {error}"));
                self.health.set_fatal(&error);
                self.folder_lock = None;
            }
        }
        let source = if url.is_empty() { "synthetic city".to_string() } else { url };
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
    fn plan_lock(&self, o: &WorldOptions) -> Result<LockPlan, String> {
        if o.tile_url.trim().is_empty() || o.cache_dir.is_empty() {
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
        if let Some(s) = &self.scheduler {
            self.stale_unloads.extend(s.delivered_keys());
        }
        self.scheduler = None;
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
        let agl = alt_msl_m - crate::geo::ground_height(lat, lon);
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
        let profile = self.diag.profile;
        let t_start = Instant::now();
        self.redelivered_this_frame = 0;
        let cpu_start = if profile { thread_cpu_us() } else { 0 };
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
        if x.hypot(z) > self.profile.origin.recenter_distance_m {
            self.local.offset_e += x;
            self.local.offset_n -= z;
            self.pending_shift[0] += x;
            self.pending_shift[2] += z;
            shift = Some([x, 0.0, z]);
        }

        let now = Instant::now();
        if let Some(s) = &mut self.scheduler {
            s.update(cam, vel, now, &mut self.jobs);
        }
        let t_updated = Instant::now();
        self.submit_jobs();
        if profile {
            let us = |a: Instant, b: Instant| b.duration_since(a).as_micros();
            let end = Instant::now();
            if us(t_start, end) > 1000 {
                self.log.info(&format!(
                    "WorldStreamer profile: update_camera {}us wall, {}us thread CPU (drain {}us, scheduler {}us, submit {}us)",
                    us(t_start, end),
                    thread_cpu_us().saturating_sub(cpu_start),
                    us(t_start, t_drained),
                    us(t_drained, t_updated),
                    us(t_updated, end)
                ));
            }
        }
        shift
    }

    /// Hand at most `max` finished tiles to `f`, in priority order. Returns how many were
    /// handed out. The host converts each tile inside `f` (its time counts in the profile line).
    pub fn pop_ready(&mut self, max: usize, mut f: impl FnMut(ReadyTileView<'_>)) -> usize {
        let t_start = Instant::now();
        self.drain_worker();
        let t_drained = Instant::now();
        let mut tris = 0usize;
        let local = self.local;
        let per_frame = self.profile.streaming.redeliveries_per_frame as usize;
        let Some(sched) = self.scheduler.as_mut() else { return 0 };
        let mut n = 0;
        while n < max {
            // Light-mode re-deliveries are rate-limited per frame, not per call.
            let Some(t) = sched.pop_ready(self.redelivered_this_frame < per_frame) else { break };
            n += 1;
            if t.redelivery {
                self.redelivered_this_frame += 1;
            }
            tris += t.mesh.triangle_count() + t.ground.triangle_count();
            let [ox, oz] = local.to_local(t.center[0], t.center[1]);
            let (lp, lc) = t.light_arrays();
            f(ReadyTileView {
                key: t.key,
                ring: t.ring,
                origin: [ox, 0.0, oz],
                mesh: &t.mesh,
                ground: &t.ground,
                lights: lp,
                light_colors: lc,
            });
            // The tile's flat buffers (a handful of Vecs) are freed here; cheap.
        }
        if self.diag.profile {
            let end = Instant::now();
            let total = end.duration_since(t_start).as_micros();
            if total > 1000 {
                self.log.info(&format!(
                    "WorldStreamer profile: poll_ready {}us (drain {}us, convert {} tiles / {} tris {}us)",
                    total,
                    t_drained.duration_since(t_start).as_micros(),
                    n,
                    tris,
                    end.duration_since(t_drained).as_micros()
                ));
            }
        }
        n
    }

    /// Tile keys the host must remove (including tiles left over from a previous `configure`).
    /// The slice is valid until the next call.
    pub fn drain_unload(&mut self) -> &[TileKey] {
        self.unload_buf.clear();
        self.unload_buf.append(&mut self.stale_unloads);
        if let Some(s) = &mut self.scheduler {
            s.drain_unload(&mut self.unload_buf);
        }
        &self.unload_buf
    }

    /// Every tile in a LOD transition: key -> visible fraction `fade` (0..1). The incoming side
    /// ramps 0 -> 1, the outgoing side 1 -> 0, the two sum to 1. Tiles absent have fade 1.
    /// Finished fades report their final values once (incoming 1.0; outgoing 0.0, then unloaded
    /// by this frame's `drain_unload`). Finals come first; a later entry for the same key wins.
    /// The slice is valid until the next call.
    pub fn drain_fades(&mut self) -> &[(TileKey, f32)] {
        self.fade_buf.clear();
        if let Some(s) = &mut self.scheduler {
            s.drain_fades(&mut self.fade_buf);
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
        let cache_mb = self.worker.as_ref().map_or(0.0, |w| w.cache_bytes() as f64 / 1_048_576.0);
        // Lat/lon of the *current* local origin (moves with re-centering).
        let (origin_lat, origin_lon) = match &self.frame {
            Some(f) if self.local != LocalSpace::default() => f.lat_lon(self.local.offset_e, self.local.offset_n),
            _ => (self.origin_lat, self.origin_lon),
        };
        WorldStats {
            queued: st.queued,
            fetching: st.fetching,
            meshing: st.meshing,
            loaded: st.loaded,
            cache_mb,
            origin_lat,
            origin_lon,
        }
    }

    /// Terrain height (m) under a point in current local x/z meters: `geo::ground_height` at the
    /// point's lat/lon (through the current floating origin). 0 until a DEM spike; a few µs.
    pub fn ground_height_at(&self, local_x: f64, local_z: f64) -> f64 {
        let Some(f) = &self.frame else { return 0.0 };
        if !local_x.is_finite() || !local_z.is_finite() {
            return 0.0;
        }
        let [e, n] = self.local.to_enu(local_x, local_z);
        let (lat, lon) = f.lat_lon(e, n);
        crate::geo::ground_height(lat, lon)
    }

    /// Visit every tracked tile: key, ring, state name, rect `[x0, z0, x1, z1]` in current local
    /// x/z meters.
    pub fn debug_tiles(&self, mut f: impl FnMut(TileKey, usize, &'static str, [f64; 4])) {
        let Some(s) = &self.scheduler else { return };
        let local = self.local;
        s.for_each_tile(|key, ring, state, rect| f(key, ring, state, local.rect_to_local(rect)));
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
