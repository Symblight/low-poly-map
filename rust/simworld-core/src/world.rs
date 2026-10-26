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

use std::sync::Arc;
use std::time::Instant;

use crate::diag::{thread_cpu_us, Diagnostics, Log, WarnLimiter};
use crate::geo::{Frame, LocalSpace, TileKey};
use crate::mesher::MeshData;
use crate::rings::RING_COUNT;
use crate::scheduler::Scheduler;
use crate::worker::{Job, Worker, WorkerMsg};

/// Re-center the floating origin when the camera is farther than this from it (m, horizontal).
pub const RECENTER_DISTANCE_M: f64 = 5000.0;
/// At most this many light-mode re-deliveries per frame (`update_camera` starts a frame; a host
/// may call `pop_ready` several times per frame).
const REDELIVERIES_PER_FRAME: usize = 1;

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
}

impl World {
    /// An unconfigured world: every call is a no-op until `configure`.
    pub fn new(log: Arc<dyn Log>, diagnostics: Diagnostics, version: impl Into<String>) -> Self {
        Self {
            log,
            diag: diagnostics,
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
        }
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn diagnostics(&self) -> &Diagnostics {
        &self.diag
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
                m => warns.extend(sched.handle(m, now)),
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

    /// Set world origin and data source. Empty `tile_url` = synthetic city. Re-configuring
    /// drops the old workers; tiles delivered before are reported by the next `drain_unload`.
    pub fn configure(&mut self, origin_lat: f64, origin_lon: f64, tile_url: &str, cache_dir: &str) {
        // The host keeps whatever we handed out before: unload it with the next drain_unload.
        if let Some(s) = &self.scheduler {
            self.stale_unloads.extend(s.delivered_keys());
        }
        self.scheduler = None;
        self.worker = None; // shuts the old runtime down in the background

        self.origin_lat = origin_lat;
        self.origin_lon = origin_lon;
        let frame = Frame::new(origin_lat, origin_lon);
        self.frame = Some(frame);
        self.local = LocalSpace::default();
        self.pending_shift = [0.0; 3];

        let url = tile_url.trim().to_string();
        match Worker::new(frame, &url, cache_dir) {
            Ok(w) => {
                self.scheduler = Some(Scheduler::new(frame, w.is_synthetic()));
                self.worker = Some(w);
            }
            Err(e) => {
                self.log.error(&format!("WorldStreamer: cannot start workers: {e}"));
            }
        }
        let source = if url.is_empty() { "synthetic city".to_string() } else { url };
        self.log.info(&format!(
            "WorldStreamer configured origin=({:.6}, {:.6}) source={} cache_dir={} [{}]",
            origin_lat, origin_lon, source, cache_dir, self.version
        ));
    }

    /// Call every frame. `local_pos` and `velocity` are in the local frame (x east, y up,
    /// z -north). Re-centers the floating origin when needed, recomputes the wanted set and
    /// queues jobs.
    pub fn update_camera(&mut self, local_pos: [f64; 3], velocity: [f64; 3], alt_agl_m: f64) {
        if self.scheduler.is_none() {
            return;
        }
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
        if x.hypot(z) > RECENTER_DISTANCE_M {
            self.local.offset_e += x;
            self.local.offset_n -= z;
            self.pending_shift[0] += x;
            self.pending_shift[2] += z;
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
    }

    /// Hand at most `max` finished tiles to `f`, in priority order. Returns how many were
    /// handed out. The host converts each tile inside `f` (its time counts in the profile line).
    pub fn pop_ready(&mut self, max: usize, mut f: impl FnMut(ReadyTileView<'_>)) -> usize {
        let t_start = Instant::now();
        self.drain_worker();
        let t_drained = Instant::now();
        let mut tris = 0usize;
        let local = self.local;
        let Some(sched) = self.scheduler.as_mut() else { return 0 };
        let mut n = 0;
        while n < max {
            // Light-mode re-deliveries are rate-limited per frame, not per call.
            let Some(t) = sched.pop_ready(self.redelivered_this_frame < REDELIVERIES_PER_FRAME) else { break };
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
