//! `WorldStreamer` node: the contract between Rust (world) and Godot/C# (display).
//!
//! Main thread only. All heavy work runs in `worker` threads; this file converts plain Rust
//! results to Godot Packed arrays inside `poll_ready` and never blocks.
//!
//! Per-frame call order from C#: `update_camera` -> `take_origin_shift` -> `poll_ready(n)` ->
//! `poll_fades` -> `poll_unload`. Fades advance and complete inside `update_camera`; a fade
//! started by a `poll_ready` delivery is already in that frame's `poll_fades`. Re-centering happens inside `update_camera`; every position handed out
//! afterwards (`poll_ready` origins, `debug_tiles` rects) is in the post-shift local frame.

use std::time::Instant;

use godot::classes::mesh::ArrayType;
use godot::prelude::*;

use crate::geo::{Frame, LocalSpace, TileKey};
use crate::mesher::MeshData;
use crate::rings::RING_COUNT;
use crate::scheduler::Scheduler;
use crate::version_string;
use crate::worker::{Job, Worker, WorkerMsg};

/// Re-center the floating origin when the camera is farther than this from it (m, horizontal).
pub const RECENTER_DISTANCE_M: f64 = 5000.0;
/// At most this many light-mode re-deliveries per frame (`update_camera` starts a frame; the
/// C# side calls `poll_ready(1)` several times per frame).
const REDELIVERIES_PER_FRAME: usize = 1;
/// At most this many `godot_warn!` per second (the rest are counted and summarized).
const WARNINGS_PER_SECOND: u32 = 5;

#[derive(GodotClass)]
#[class(base = Node, init)]
pub struct WorldStreamer {
    // World origin (WGS84 degrees) of the fixed ENU frame.
    origin_lat: f64,
    origin_lon: f64,
    tile_url: GString,
    cache_dir: GString,

    frame: Option<Frame>,
    local: LocalSpace,
    pending_shift: [f64; 3],
    scheduler: Option<Scheduler>,
    worker: Option<Worker>,

    jobs: Vec<Job>,
    unload_buf: Vec<TileKey>,
    fade_buf: Vec<(TileKey, f32)>,
    /// Tiles Godot still holds from a previous `configure`.
    stale_unloads: Vec<TileKey>,

    /// `SIMPLANE_PROFILE=1`: print a phase breakdown for main-thread calls slower than 1 ms.
    profile: bool,
    /// Light-mode re-deliveries handed out since the last `update_camera`.
    redelivered_this_frame: usize,
    warn_window: Option<Instant>,
    warns_in_window: u32,
    warns_suppressed: u32,

    base: Base<Node>,
}

impl WorldStreamer {
    fn warn(&mut self, msg: &str) {
        let now = Instant::now();
        match self.warn_window {
            Some(t) if now.duration_since(t).as_secs_f32() < 1.0 => {}
            _ => {
                if self.warns_suppressed > 0 {
                    godot_warn!("WorldStreamer: {} more warnings suppressed", self.warns_suppressed);
                }
                self.warn_window = Some(now);
                self.warns_in_window = 0;
                self.warns_suppressed = 0;
            }
        }
        if self.warns_in_window < WARNINGS_PER_SECOND {
            self.warns_in_window += 1;
            godot_warn!("{msg}");
        } else {
            self.warns_suppressed += 1;
        }
    }

    /// Apply worker results (cheap: moves boxes around). Bounded per call.
    fn drain_worker(&mut self) {
        let (Some(worker), Some(sched)) = (&self.worker, &mut self.scheduler) else { return };
        let now = Instant::now();
        let mut warns: Vec<String> = Vec::new();
        for _ in 0..256 {
            let Some(msg) = worker.try_recv() else { break };
            match msg {
                WorkerMsg::Info(s) => godot_print!("WorldStreamer: {s}"),
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
}

// Plain `[f32; N]` buffers from the workers are bulk-copied into Packed arrays. The Godot
// vector/color types are `#[repr(C)]` structs of f32 (no `double-precision` feature), so the
// slices can be reinterpreted without a per-element loop.
const _: () = assert!(std::mem::size_of::<Vector3>() == 12 && std::mem::align_of::<Vector3>() == 4);
const _: () = assert!(std::mem::size_of::<Vector2>() == 8 && std::mem::align_of::<Vector2>() == 4);
const _: () = assert!(std::mem::size_of::<Color>() == 16 && std::mem::align_of::<Color>() == 4);

fn as_v3(s: &[[f32; 3]]) -> &[Vector3] {
    // SAFETY: Vector3 is repr(C) { x, y, z: f32 } (size/align asserted above).
    unsafe { std::slice::from_raw_parts(s.as_ptr().cast::<Vector3>(), s.len()) }
}
fn as_v2(s: &[[f32; 2]]) -> &[Vector2] {
    // SAFETY: Vector2 is repr(C) { x, y: f32 } (size/align asserted above).
    unsafe { std::slice::from_raw_parts(s.as_ptr().cast::<Vector2>(), s.len()) }
}
fn as_col(s: &[[f32; 4]]) -> &[Color] {
    // SAFETY: Color is repr(C) { r, g, b, a: f32 } (size/align asserted above).
    unsafe { std::slice::from_raw_parts(s.as_ptr().cast::<Color>(), s.len()) }
}

/// `Mesh.ARRAY_MAX`-length Array (VERTEX, NORMAL, COLOR, TEX_UV, INDEX; other slots nil), or
/// an empty Array when there is no geometry. TEX_UV stays nil when the mesh has no UVs (ground).
fn mesh_array(m: &MeshData) -> VarArray {
    let mut a = VarArray::new();
    if m.is_empty() {
        return a;
    }
    a.resize(ArrayType::MAX.ord() as usize, &Variant::nil());
    a.set(ArrayType::VERTEX.ord() as usize, &PackedVector3Array::from(as_v3(&m.positions)).to_variant());
    a.set(ArrayType::NORMAL.ord() as usize, &PackedVector3Array::from(as_v3(&m.normals)).to_variant());
    a.set(ArrayType::COLOR.ord() as usize, &PackedColorArray::from(as_col(&m.colors)).to_variant());
    if !m.uvs.is_empty() {
        a.set(ArrayType::TEX_UV.ord() as usize, &PackedVector2Array::from(as_v2(&m.uvs)).to_variant());
    }
    a.set(ArrayType::INDEX.ord() as usize, &PackedInt32Array::from(&m.indices[..]).to_variant());
    a
}

/// CPU time consumed by the calling thread (µs); 0 where unavailable. Profiling only: wall
/// time above CPU time means the main thread was preempted, not working.
fn thread_cpu_us() -> u64 {
    #[cfg(unix)]
    {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: valid out-pointer; CLOCK_THREAD_CPUTIME_ID is supported on macOS/iOS/Linux.
        if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) } == 0 {
            return ts.tv_sec as u64 * 1_000_000 + ts.tv_nsec as u64 / 1000;
        }
    }
    0
}

fn per_ring(v: [i32; RING_COUNT]) -> PackedInt32Array {
    PackedInt32Array::from(&v[..])
}

#[godot_api]
impl WorldStreamer {
    /// Set world origin and data source. Empty `tile_url` = synthetic city.
    #[func]
    fn configure(&mut self, origin_lat: f64, origin_lon: f64, tile_url: GString, cache_dir: GString) {
        // Godot keeps whatever we handed out before: unload it with the next poll_unload.
        if let Some(s) = &self.scheduler {
            self.stale_unloads.extend(s.delivered_keys());
        }
        self.scheduler = None;
        self.worker = None; // shuts the old runtime down in the background

        self.profile = std::env::var("SIMPLANE_PROFILE").is_ok_and(|v| v == "1");
        self.origin_lat = origin_lat;
        self.origin_lon = origin_lon;
        self.tile_url = tile_url;
        self.cache_dir = cache_dir;
        let frame = Frame::new(origin_lat, origin_lon);
        self.frame = Some(frame);
        self.local = LocalSpace::default();
        self.pending_shift = [0.0; 3];

        let url = self.tile_url.to_string().trim().to_string();
        let cache = self.cache_dir.to_string();
        match Worker::new(frame, &url, &cache) {
            Ok(w) => {
                self.scheduler = Some(Scheduler::new(frame, w.is_synthetic()));
                self.worker = Some(w);
            }
            Err(e) => {
                godot_error!("WorldStreamer: cannot start workers: {e}");
            }
        }
        let source = if url.is_empty() { "synthetic city".to_string() } else { url };
        godot_print!(
            "WorldStreamer configured origin=({:.6}, {:.6}) source={} cache_dir={} [{}]",
            origin_lat,
            origin_lon,
            source,
            self.cache_dir,
            version_string()
        );
    }

    /// Call every frame. Recomputes the wanted set and queues jobs.
    #[func]
    fn update_camera(&mut self, local_pos: Vector3, velocity: Vector3, alt_agl_m: f64) {
        if self.scheduler.is_none() {
            return;
        }
        let t_start = Instant::now();
        self.redelivered_this_frame = 0;
        let cpu_start = if self.profile { thread_cpu_us() } else { 0 };
        self.drain_worker();
        let t_drained = Instant::now();
        let (x, z) = (local_pos.x as f64, local_pos.z as f64);
        let cam_en = self.local.to_enu(x, z);
        let alt = if alt_agl_m.is_finite() { alt_agl_m.max(0.0) } else { 0.0 };
        let cam = [cam_en[0], cam_en[1], alt];
        // Godot velocity (x east, y up, z -north) -> (east, north, up).
        let vel = [velocity.x as f64, -(velocity.z as f64), velocity.y as f64];

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
        if self.profile {
            let us = |a: Instant, b: Instant| b.duration_since(a).as_micros();
            let end = Instant::now();
            if us(t_start, end) > 1000 {
                godot_print!(
                    "WorldStreamer profile: update_camera {}us wall, {}us thread CPU (drain {}us, scheduler {}us, submit {}us)",
                    us(t_start, end),
                    thread_cpu_us().saturating_sub(cpu_start),
                    us(t_start, t_drained),
                    us(t_drained, t_updated),
                    us(t_updated, end)
                );
            }
        }
    }

    /// Finished tiles, at most `max`.
    #[func]
    fn poll_ready(&mut self, max: i32) -> Array<VarDictionary> {
        let mut out = Array::new();
        let t_start = Instant::now();
        self.drain_worker();
        let t_drained = Instant::now();
        let mut tris = 0usize;
        let Some(sched) = self.scheduler.as_mut() else { return out };
        let mut n = 0;
        while n < max.max(0) {
            // Light-mode re-deliveries are rate-limited per frame, not per call.
            let Some(t) = sched.pop_ready(self.redelivered_this_frame < REDELIVERIES_PER_FRAME) else { break };
            n += 1;
            if t.redelivery {
                self.redelivered_this_frame += 1;
            }
            tris += t.mesh.triangle_count() + t.ground.triangle_count();
            let [ox, oz] = self.local.to_local(t.center[0], t.center[1]);
            let (lp, lc) = t.light_arrays();
            let mut d = VarDictionary::new();
            d.set("key", &GString::from(t.key.to_string().as_str()));
            d.set("ring", t.ring as i64);
            d.set("origin", Vector3::new(ox as f32, 0.0, oz as f32));
            d.set("mesh", &mesh_array(&t.mesh));
            // Always present (empty Array for R3 / no ground): Godot replaces the tile's ground.
            d.set("ground", &mesh_array(&t.ground));
            d.set("lights", &PackedVector3Array::from(as_v3(lp)));
            d.set("light_colors", &PackedColorArray::from(as_col(lc)));
            out.push(&d);
            // The tile's flat buffers (a handful of Vecs) are freed here; cheap.
        }
        if self.profile {
            let end = Instant::now();
            let total = end.duration_since(t_start).as_micros();
            if total > 1000 {
                godot_print!(
                    "WorldStreamer profile: poll_ready {}us (drain {}us, convert {} tiles / {} tris {}us)",
                    total,
                    t_drained.duration_since(t_start).as_micros(),
                    n,
                    tris,
                    end.duration_since(t_drained).as_micros()
                );
            }
        }
        out
    }

    /// Tile keys to remove.
    #[func]
    fn poll_unload(&mut self) -> PackedStringArray {
        self.unload_buf.clear();
        self.unload_buf.append(&mut self.stale_unloads);
        if let Some(s) = &mut self.scheduler {
            s.drain_unload(&mut self.unload_buf);
        }
        self.unload_buf.iter().map(|k| GString::from(k.to_string().as_str())).collect()
    }

    /// Every tile in a LOD transition: key (`z/x/y`) -> visible fraction `fade` (0..1). The
    /// incoming side ramps 0 -> 1, the outgoing side 1 -> 0, the two sum to 1. Tiles absent
    /// from the dictionary have fade 1. Finished fades report their final values once (incoming
    /// 1.0; outgoing 0.0, then unloaded by this frame's `poll_unload`).
    #[func]
    fn poll_fades(&mut self) -> VarDictionary {
        let mut d = VarDictionary::new();
        let Some(s) = &mut self.scheduler else { return d };
        self.fade_buf.clear();
        s.drain_fades(&mut self.fade_buf);
        // Finals come first; an active entry for the same key (not expected) overrides.
        for &(k, f) in &self.fade_buf {
            d.set(&GString::from(k.to_string().as_str()), f as f64);
        }
        d
    }

    /// Non-zero once after a re-center; C# subtracts it from camera and all tile roots.
    #[func]
    fn take_origin_shift(&mut self) -> Vector3 {
        let s = self.pending_shift;
        self.pending_shift = [0.0; 3];
        Vector3::new(s[0] as f32, s[1] as f32, s[2] as f32)
    }

    /// Stats for the debug overlay. Per-ring counters are PackedInt32Array of length 4.
    #[func]
    fn debug_stats(&self) -> VarDictionary {
        let st = self.scheduler.as_ref().map(|s| s.stats()).unwrap_or_default();
        let cache_mb = self.worker.as_ref().map_or(0.0, |w| w.cache_bytes() as f64 / 1_048_576.0);
        // Lat/lon of the *current* local origin (moves with re-centering).
        let (lat, lon) = match &self.frame {
            Some(f) if self.local != LocalSpace::default() => f.lat_lon(self.local.offset_e, self.local.offset_n),
            _ => (self.origin_lat, self.origin_lon),
        };
        let mut d = VarDictionary::new();
        d.set("queued", &per_ring(st.queued));
        d.set("fetching", &per_ring(st.fetching));
        d.set("meshing", &per_ring(st.meshing));
        d.set("loaded", &per_ring(st.loaded));
        d.set("cache_mb", cache_mb);
        d.set("origin_lat", lat);
        d.set("origin_lon", lon);
        d.set("version", &GString::from(version_string().as_str()));
        d
    }

    /// Terrain height (m) under a point in current local x/z meters: `geo::ground_height` at the
    /// point's lat/lon (through the current floating origin). 0 until a DEM spike; called by
    /// C# once per frame for height above ground, so it stays a few microseconds.
    #[func]
    fn ground_height_at(&self, local_x: f64, local_z: f64) -> f64 {
        let Some(f) = &self.frame else { return 0.0 };
        if !local_x.is_finite() || !local_z.is_finite() {
            return 0.0;
        }
        let [e, n] = self.local.to_enu(local_x, local_z);
        let (lat, lon) = f.lat_lon(e, n);
        crate::geo::ground_height(lat, lon)
    }

    /// One entry per tracked tile: key, ring, state, min, max (local x/z meters).
    #[func]
    fn debug_tiles(&self) -> Array<VarDictionary> {
        let mut out = Array::new();
        let Some(s) = &self.scheduler else { return out };
        let local = self.local;
        s.for_each_tile(|key, ring, state, rect| {
            let [x0, z0, x1, z1] = local.rect_to_local(rect);
            let mut d = VarDictionary::new();
            d.set("key", &GString::from(key.to_string().as_str()));
            d.set("ring", ring as i64);
            d.set("state", &GString::from(state));
            d.set("min", Vector2::new(x0 as f32, z0 as f32));
            d.set("max", Vector2::new(x1 as f32, z1 as f32));
            out.push(&d);
        });
        out
    }
}
