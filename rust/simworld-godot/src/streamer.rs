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

use std::sync::Arc;

use godot::classes::mesh::ArrayType;
use godot::classes::INode;
use godot::prelude::*;

use simworld_core::geo::TileKey;
use simworld_core::mesher::MeshData;
use simworld_core::rings::RING_COUNT;
use simworld_core::{World, API_MAJOR, API_MINOR};

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

fn per_ring(v: [i32; RING_COUNT]) -> PackedInt32Array {
    PackedInt32Array::from(&v[..])
}

fn key_string(k: TileKey) -> GString {
    GString::from(k.to_string().as_str())
}

#[godot_api]
impl WorldStreamer {
    /// Set world origin and data source. Empty `tile_url` = synthetic city.
    #[func]
    fn configure(&mut self, origin_lat: f64, origin_lon: f64, tile_url: GString, cache_dir: GString) {
        self.world.configure(origin_lat, origin_lon, &tile_url.to_string(), &cache_dir.to_string());
    }

    /// Call every frame. Recomputes the wanted set and queues jobs.
    #[func]
    fn update_camera(&mut self, local_pos: Vector3, velocity: Vector3, alt_agl_m: f64) {
        self.world.update_camera(
            [local_pos.x as f64, local_pos.y as f64, local_pos.z as f64],
            [velocity.x as f64, velocity.y as f64, velocity.z as f64],
            alt_agl_m,
        );
    }

    /// Finished tiles, at most `max`.
    #[func]
    fn poll_ready(&mut self, max: i32) -> Array<VarDictionary> {
        let mut out = Array::new();
        self.world.pop_ready(max.max(0) as usize, |t| {
            let mut d = VarDictionary::new();
            d.set("key", &key_string(t.key));
            d.set("ring", t.ring as i64);
            d.set("origin", Vector3::new(t.origin[0] as f32, t.origin[1] as f32, t.origin[2] as f32));
            d.set("mesh", &mesh_array(t.mesh));
            // Always present (empty Array for R3 / no ground): Godot replaces the tile's ground.
            d.set("ground", &mesh_array(t.ground));
            d.set("lights", &PackedVector3Array::from(as_v3(t.lights)));
            d.set("light_colors", &PackedColorArray::from(as_col(t.light_colors)));
            out.push(&d);
        });
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
        d
    }

    /// Contract version (`docs/contract.md`): `Vector2i(major, minor)`. A minor bump is
    /// additive; a major bump is breaking. Constant for the lifetime of the library; callable
    /// before `configure`.
    #[func]
    fn api_version(&self) -> Vector2i {
        Vector2i::new(API_MAJOR as i32, API_MINOR as i32)
    }

    /// Terrain height (m) under a point in current local x/z meters: `geo::ground_height` at the
    /// point's lat/lon (through the current floating origin). 0 until a DEM spike; called by
    /// C# once per frame for height above ground, so it stays a few microseconds.
    #[func]
    fn ground_height_at(&self, local_x: f64, local_z: f64) -> f64 {
        self.world.ground_height_at(local_x, local_z)
    }

    /// One entry per tracked tile: key, ring, state, min, max (local x/z meters).
    #[func]
    fn debug_tiles(&self) -> Array<VarDictionary> {
        let mut out = Array::new();
        self.world.debug_tiles(|key, ring, state, [x0, z0, x1, z1]| {
            let mut d = VarDictionary::new();
            d.set("key", &key_string(key));
            d.set("ring", ring as i64);
            d.set("state", &GString::from(state));
            d.set("min", Vector2::new(x0 as f32, z0 as f32));
            d.set("max", Vector2::new(x1 as f32, z1 as f32));
            out.push(&d);
        });
        out
    }
}
