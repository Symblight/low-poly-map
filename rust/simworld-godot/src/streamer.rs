//! `WorldStreamer` node: the contract between Rust (world) and Godot/C# (display).
//!
//! A type-conversion shell over `simworld_core::World`: Godot types in, Packed arrays and
//! Dictionaries out. All logic (floating origin, pacing, warning limits, profiling) lives in the
//! core. Main thread only; never blocks.
//!
//! Per-frame call order from C#: `update_camera` -> `take_origin_shift` -> `poll_ready(n)` ->
//! `poll_fades` -> `poll_unload`. Fades advance and complete inside `update_camera`; a fade
//! started by a `poll_ready` delivery is already in that frame's `poll_fades`. Re-centering happens inside `update_camera`; every position handed out
//! afterwards (`poll_ready` origins, `debug_tiles` rects) is in the post-shift local frame.
//!
//! Layers (api 1.5): a host that requested the layer `terrain` (and whose profile enables it)
//! also gets terrain tiles, keys `"t/z/x/y"`, through the same `poll_ready` / `poll_fades` /
//! `poll_unload`; every ready tile carries `layer` (`"world"` / `"terrain"`).
//!
//! Signals (api 0.7) are emitted on the main thread from inside the calls that cause them:
//! `origin_shifted` from `update_camera` / `update_camera_geodetic`, `status_changed` from those
//! and `poll_ready`. Emission goes through `base_mut()` (gdext typed signals), so handlers may
//! call back into this node.

use std::sync::Arc;

use godot::classes::INode;
use godot::prelude::*;

use simworld_core::budget::{Pool, MIB};
use simworld_core::latency::{Windows, STAGES};
use simworld_core::rings::{MAX_RINGS, RING_COUNT};
use simworld_core::{ConfigureResult, Log, Status, World, API_MAJOR, API_MINOR};

use crate::convert::{json_to_variant, lod_state_dict, options_from_dict, vector_lod_into};
use crate::ready::{key_string, ready_dict, v3};
use crate::seed::{airport_dict, seed_info_dict};
use crate::host::{diagnostics_from_env, GodotLog};
use crate::version_string;

#[derive(GodotClass)]
#[class(base = Node)]
pub struct WorldStreamer {
    world: World,
    base: Base<Node>,
}

#[godot_api]
impl INode for WorldStreamer {
    fn init(base: Base<Node>) -> Self {
        Self { world: World::new(Arc::new(GodotLog), diagnostics_from_env(), version_string()), base }
    }
}

fn per_ring(v: [i32; RING_COUNT]) -> PackedInt32Array {
    PackedInt32Array::from(&v[..])
}

fn per_terrain_ring(v: &[i32; MAX_RINGS], rings: usize) -> PackedInt32Array {
    PackedInt32Array::from(&v[..rings.min(MAX_RINGS)])
}

/// `debug_stats()["latency"]` (api 1.3): stage name -> { p50, p95, max (ms), n } per band.
fn latency_dict(l: &Windows) -> VarDictionary {
    let mut out = VarDictionary::new();
    for st in STAGES {
        let s: [_; RING_COUNT] = std::array::from_fn(|r| l.summary(st, r));
        let mut d = VarDictionary::new();
        d.set("p50", &PackedFloat32Array::from(&s.map(|x| x.p50)[..]));
        d.set("p95", &PackedFloat32Array::from(&s.map(|x| x.p95)[..]));
        d.set("max", &PackedFloat32Array::from(&s.map(|x| x.max)[..]));
        d.set("n", &PackedInt32Array::from(&s.map(|x| x.n as i32)[..]));
        out.set(st.name(), &d);
    }
    out
}



fn status_dict(s: &Status) -> VarDictionary {
    let mut d = VarDictionary::new();
    d.set("state", &GString::from(s.state.as_str()));
    d.set("message", &GString::from(s.message.as_str()));
    d.set("failed_fetches", s.failed_fetches.min(i64::MAX as u64) as i64);
    d.set("last_error", &GString::from(s.last_error.as_str()));
    d.set("terrain_active", s.terrain_active);
    d
}

impl WorldStreamer {
    /// Emit `origin_shifted` for a re-center done by this call, then `status_changed` if the
    /// status changed. Main thread, no world borrow held while handlers run.
    fn emit_after_update(&mut self, shift: Option<[f64; 3]>) {
        if let Some(s) = shift {
            self.signals().origin_shifted().emit(v3(s));
        }
        self.emit_status_change();
    }

    fn emit_status_change(&mut self) {
        if let Some(s) = self.world.take_status_change() {
            let d = status_dict(&s);
            self.signals().status_changed().emit(&d);
        }
    }
}

#[godot_api]
impl WorldStreamer {
    /// Emitted during `update_camera` / `update_camera_geodetic` when the floating origin is
    /// re-centered: `shift` (local meters, y = 0) is what to subtract from every node in local
    /// space. Same vector as the next `take_origin_shift()`, which this does not consume.
    #[signal]
    fn origin_shifted(shift: Vector3);

    /// Emitted (from `update_camera` / `update_camera_geodetic` / `poll_ready`) when the
    /// `status()` state or message changes. Same Dictionary shape as `status()`.
    #[signal]
    fn status_changed(status: VarDictionary);

    /// Set world origin and data source with the default world profile. Empty `tile_url` =
    /// synthetic city. A wrapper over `configure_world` (errors are logged).
    #[func]
    fn configure(&mut self, origin_lat: f64, origin_lon: f64, tile_url: GString, cache_dir: GString) {
        self.world.configure(origin_lat, origin_lon, &tile_url.to_string(), &cache_dir.to_string());
    }

    /// Set origin, data source, world profile (`profile_path`, `profile_overrides`) and
    /// diagnostics from one options Dictionary. Returns `{ ok, error, api_version,
    /// attributions }`; on an error nothing changes.
    #[func]
    fn configure_world(&mut self, options: VarDictionary) -> VarDictionary {
        let r = match options_from_dict(&options, self.world.diagnostics()) {
            Ok((o, warns)) => {
                for w in warns {
                    GodotLog.warn(&format!("WorldStreamer: {w}"));
                }
                self.world.configure_world(o)
            }
            Err(e) => {
                GodotLog.error(&format!("WorldStreamer: configure_world failed: {e}"));
                ConfigureResult { ok: false, error: e, attributions: self.world.attributions().to_vec() }
            }
        };
        let mut d = VarDictionary::new();
        d.set("ok", r.ok);
        d.set("error", &GString::from(r.error.as_str()));
        d.set("api_version", self.api_version());
        let attributions: PackedStringArray = r.attributions.iter().map(|s| GString::from(s.as_str())).collect();
        d.set("attributions", &attributions);
        d
    }

    /// The resolved world profile (defaults until a `configure_world` with a profile) as a
    /// Dictionary with the keys of `addons/simworld/profiles/default.json`.
    #[func]
    fn world_profile(&self) -> VarDictionary {
        json_to_variant(&self.world.profile().to_json()).try_to::<VarDictionary>().unwrap_or_default()
    }

    /// Call every frame. Recomputes the wanted set and queues jobs.
    #[func]
    fn update_camera(&mut self, local_pos: Vector3, velocity: Vector3, alt_agl_m: f64) {
        let shift = self.world.update_camera(
            [local_pos.x as f64, local_pos.y as f64, local_pos.z as f64],
            [velocity.x as f64, velocity.y as f64, velocity.z as f64],
            alt_agl_m,
        );
        self.emit_after_update(shift);
    }

    /// Camera from geodetic input instead of `update_camera` (call one of the two per frame):
    /// WGS84 degrees, meters above MSL, velocity NED (north, east, down) m/s. Invalid input is
    /// logged and ignored.
    #[func]
    fn update_camera_geodetic(&mut self, lat: f64, lon: f64, alt_msl_m: f64, vel_ned: Vector3) {
        let v = [vel_ned.x as f64, vel_ned.y as f64, vel_ned.z as f64];
        let shift = self.world.update_camera_geodetic(lat, lon, alt_msl_m, v).ok().flatten();
        self.emit_after_update(shift);
    }

    /// WGS84 (degrees, meters above MSL) -> current local frame (x east, y = altitude, z
    /// -north). `Vector3.ZERO` before configure / after `shutdown` / for invalid input.
    #[func]
    fn geodetic_to_local(&self, lat: f64, lon: f64, alt_msl: f64) -> Vector3 {
        self.world.geodetic_to_local(lat, lon, alt_msl).map_or(Vector3::ZERO, v3)
    }

    /// Current local frame -> `Vector3(lat, lon, alt_msl)`. f32 output: lat/lon resolution is
    /// ~4e-6 degrees (decimeters). `Vector3.ZERO` before configure / after `shutdown`.
    #[func]
    fn local_to_geodetic(&self, local: Vector3) -> Vector3 {
        self.world.local_to_geodetic([local.x as f64, local.y as f64, local.z as f64]).map_or(Vector3::ZERO, v3)
    }

    /// Attribution strings the host must show while map data is visible (empty for the
    /// synthetic city, before configure and after `shutdown`).
    #[func]
    fn attributions(&self) -> PackedStringArray {
        self.world.attributions().iter().map(|s| GString::from(s.as_str())).collect()
    }

    /// Data-source health: `{ state, message, failed_fetches, last_error }`.
    #[func]
    fn status(&self) -> VarDictionary {
        status_dict(&self.world.status())
    }

    /// Stop the workers, wait (at most 1 s) for in-flight cache I/O, release the cache folder
    /// lock. The node is inert until the next `configure` / `configure_world`.
    #[func]
    fn shutdown(&mut self) {
        self.world.shutdown();
    }

    /// Finished tiles, at most `max`.
    #[func]
    fn poll_ready(&mut self, max: i32) -> Array<VarDictionary> {
        let mut out = Array::new();
        self.world.pop_ready(max.max(0) as usize, |t| out.push(&ready_dict(&t)));
        self.emit_status_change();
        out
    }

    /// Tile keys to remove.
    #[func]
    fn poll_unload(&mut self) -> PackedStringArray {
        self.world.drain_unload().iter().map(|&k| key_string(k)).collect()
    }

    /// Every tile in a LOD transition: key (`z/x/y`) -> visible fraction `fade` (0..1). The
    /// incoming side ramps 0 -> 1, the outgoing side 1 -> 0, the two sum to 1. Tiles absent
    /// from the dictionary have fade 1. Finished fades report their final values once (incoming
    /// 1.0; outgoing 0.0, then unloaded by this frame's `poll_unload`).
    #[func]
    fn poll_fades(&mut self) -> VarDictionary {
        let mut d = VarDictionary::new();
        // Finals come first; an active entry for the same key (not expected) overrides.
        for &(k, f) in self.world.drain_fades() {
            d.set(&key_string(k), f as f64);
        }
        d
    }

    /// Non-zero once after a re-center; C# subtracts it from camera and all tile roots.
    #[func]
    fn take_origin_shift(&mut self) -> Vector3 {
        let s = self.world.take_origin_shift();
        Vector3::new(s[0] as f32, s[1] as f32, s[2] as f32)
    }

    /// Stats for the debug overlay. Per-ring counters are PackedInt32Array of length 4.
    #[func]
    fn debug_stats(&self) -> VarDictionary {
        let st = self.world.stats();
        let mut d = VarDictionary::new();
        d.set("queued", &per_ring(st.queued));
        d.set("fetching", &per_ring(st.fetching));
        d.set("meshing", &per_ring(st.meshing));
        d.set("loaded", &per_ring(st.loaded));
        d.set("cache_mb", st.cache_mb);
        d.set("origin_lat", st.origin_lat);
        d.set("origin_lon", st.origin_lon);
        d.set("version", &GString::from(self.world.version()));
        d.set("api_major", API_MAJOR as i64);
        d.set("api_minor", API_MINOR as i64);
        // Shared CPU job pool (both layers; Tiles v2 + Terrain v2 step J1).
        d.set("jobs_queued", st.jobs.queued as i64);
        d.set("jobs_running", st.jobs.running as i64);
        d.set("jobs_cancelled", st.jobs.cancelled.min(i64::MAX as u64) as i64);
        // Memory budget of the core's CPU-side pools (Terrain v2 step M1), MiB.
        let mem = &st.memory;
        let mut m = VarDictionary::new();
        for p in Pool::ALL {
            m.set(p.name(), mem.pool(p).bytes as f64 / MIB);
        }
        m.set("free_buffers", mem.free_buffer_bytes() as f64 / MIB);
        m.set("total", mem.total_bytes as f64 / MIB);
        m.set("budget", mem.budget_bytes as f64 / MIB);
        m.set("over_budget", mem.over_budget_bytes as f64 / MIB);
        d.set("memory_mb", &m);
        // api 1.11 (perf review 2): tiles delivered again from memory instead of rebuilt.
        let mut rt = VarDictionary::new();
        rt.set("hits", st.recent.hits.min(i64::MAX as u64) as i64);
        rt.set("entries", st.recent.entries as i64);
        rt.set("mb", st.recent.bytes as f64 / MIB);
        d.set("recent_tiles", &rt);
        // Tiles v2 data tiles (step T2a): resident now / decoded since configure.
        d.set("data_tiles_loaded", st.data_tiles_loaded as i64);
        d.set("data_tiles_decoded", st.data_tiles_decoded.min(i64::MAX as u64) as i64);
        // api 1.11 (review M3): data tile evictions and re-decodes (thrash shows as a rising
        // `data_tiles_redecoded`).
        d.set("data_tiles_evicted_idle", st.data_tiles_evicted_idle.min(i64::MAX as u64) as i64);
        d.set("data_tiles_evicted_budget", st.data_tiles_evicted_budget.min(i64::MAX as u64) as i64);
        d.set("data_tiles_redecoded", st.data_tiles_redecoded.min(i64::MAX as u64) as i64);
        // api 1.11: the source's data levels (once known) and the landmark layer.
        if let Some(l) = st.data_levels {
            let mut dl = VarDictionary::new();
            dl.set("detail", l.detail as i64);
            dl.set("overview", l.overview as i64);
            dl.set("two_level", l.two_level);
            dl.set("landmarks", l.landmarks);
            d.set("data_levels", &dl);
        }
        if self.world.landmark_layer_active() {
            d.set("landmarks_loaded", st.landmarks.loaded as i64);
            d.set("landmarks_pending", st.landmarks.pending as i64);
            d.set("landmarks_empty", st.landmarks.empty as i64);
        }
        if let Some(l) = self.world.latency() {
            d.set("latency", &latency_dict(l.all()));
            // api 1.11 (review M2): first deliveries only (new area), without the coarsening
            // re-deliveries that dominate `latency` during flight.
            d.set("latency_first", &latency_dict(l.first()));
        }
        // api 1.10 / 1.11: screen-space-error LOD (the vector keys of `lod_state()`).
        vector_lod_into(&mut d, &self.world.lod_state());
        // api 1.5: terrain-mesh layer counters, only while the layer runs.
        if self.world.terrain_layer_active() {
            let n = st.terrain_rings;
            d.set("terrain_queued", &per_terrain_ring(&st.terrain_queued, n));
            d.set("terrain_fetching", &per_terrain_ring(&st.terrain_fetching, n));
            d.set("terrain_meshing", &per_terrain_ring(&st.terrain_meshing, n));
            d.set("terrain_loaded", &per_terrain_ring(&st.terrain_loaded, n));
            d.set("terrain_tris", st.terrain_tris.min(i64::MAX as u64) as i64);
            d.set("terrain_detail", self.world.terrain_detail());
            // api 1.11 (review M1): SSE splits left out by `terrain.max_tiles`.
            d.set("terrain_budget_skipped", st.terrain_budget_skipped as i64);
        }
        d
    }

    /// Contract version (`docs/contract.md`): `Vector2i(major, minor)`. A minor bump is
    /// additive; a major bump is breaking. Constant for the lifetime of the library; callable
    /// before `configure`.
    #[func]
    fn api_version(&self) -> Vector2i {
        Vector2i::new(API_MAJOR as i32, API_MINOR as i32)
    }

    /// Terrain height (m) under a point in current local x/z meters: `Terrain::height` at the
    /// point's lat/lon (through the current floating origin). 0 with the terrain off; called by
    /// C# once per frame for height above ground, so it stays a few microseconds.
    #[func]
    fn ground_height_at(&self, local_x: f64, local_z: f64) -> f64 {
        self.world.ground_height_at(local_x, local_z)
    }

    /// Terrain detail (api 1.5; since 1.10 the terrain cover's tau scale, `terrain.tau_px` x
    /// `scale`), clamped to [0.25, `terrain.detail_scale_max`] (1 = the profile, 2 = coarser).
    /// The cover follows through ordinary swaps; held terrain tiles are not re-delivered.
    /// Returns the scale in effect.
    #[func]
    fn set_terrain_detail(&mut self, scale: f64) -> f64 {
        self.world.set_terrain_detail(scale)
    }

    /// The view the screen-space error is measured in (api 1.10): vertical field of view
    /// (degrees, [1, 179]), viewport height in physical pixels ([1, 65536]) and the camera's
    /// forward direction (local axes). Invalid input is ignored with a warning. Defaults 60 deg,
    /// 1080 px, -Z until called; kept across `configure_world`; the cover follows next frame.
    #[func]
    fn set_view(&mut self, fov_y_deg: f64, viewport_h_px: i64, forward: Vector3) {
        let f = [forward.x as f64, forward.y as f64, forward.z as f64];
        self.world.set_view(fov_y_deg, viewport_h_px as f64, f);
    }

    /// The LOD state only (api 1.11): the `debug_stats` LOD keys plus the terrain cover's tau,
    /// for hosts that follow the live edges several times a second.
    #[func]
    fn lod_state(&self) -> VarDictionary {
        lod_state_dict(&self.world.lod_state())
    }

    /// LOD quality (api 1.10): live multiplier of the profile's `lod.tau_px` (2 = half the
    /// refine distances), clamped to [0.25, 4], non-finite ignored. Kept across
    /// `configure_world`; the cover follows next frame. Returns the scale in effect.
    #[func]
    fn set_lod_quality(&mut self, scale: f64) -> f64 {
        self.world.set_lod_quality(scale)
    }

    /// Airports of a seed world (api 1.7): ident, name, type, reference point, runways with both
    /// ends, thresholds, touchdown points and lights, in the current local frame (call again
    /// after `origin_shifted`). Empty without a seed world.
    #[func]
    fn airports(&self) -> Array<VarDictionary> {
        let mut out = Array::new();
        for a in self.world.airports() {
            out.push(&airport_dict(&a));
        }
        out
    }

    /// The seed world (api 1.7): generator version, built phases, world version and seed,
    /// cities, counts, validation warnings and measured sizes. Empty without a seed world.
    #[func]
    fn seed_world_info(&self) -> VarDictionary {
        self.world.seed_world_info().map_or_else(VarDictionary::new, |i| seed_info_dict(&i))
    }

    /// One entry per tracked tile: key, ring, state, min, max (local x/z meters).
    #[func]
    fn debug_tiles(&self) -> Array<VarDictionary> {
        let mut out = Array::new();
        let source = |l| self.world.tile_source(l);
        self.world.debug_tiles(|key, ring, state, [x0, z0, x1, z1]| {
            let mut d = VarDictionary::new();
            d.set("key", &key_string(key));
            d.set("layer", &GString::from(key.layer.name()));
            d.set("source", &GString::from(source(key.layer)));
            d.set("ring", ring as i64);
            d.set("state", &GString::from(state));
            d.set("min", Vector2::new(x0 as f32, z0 as f32));
            d.set("max", Vector2::new(x1 as f32, z1 as f32));
            out.push(&d);
        });
        out
    }
}
