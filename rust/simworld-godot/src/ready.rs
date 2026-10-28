//! The ready-tile dictionary (contract section 4): one `Dictionary` per delivered tile, built on
//! the main thread inside `poll_ready`.
//!
//! The workers already lay every array out as Godot does (`[f32; 3]` = `Vector3`, `[f32; 4]` =
//! `Color`, `[f32; 2]` = `Vector2`, `i32` indices; asserted below), so each array is one bulk
//! copy from the tile's buffer into its Packed array: nothing is converted per element here
//! (perf review 2 item 4: 0.02-0.05 ms per tile p50 in release, `diagnostics.profile`).

use godot::classes::mesh::ArrayType;
use godot::prelude::*;

use simworld_core::mesher::MeshData;
use simworld_core::rings::{LayerKey, LayerKind};
use simworld_core::surface::{EngineSurface, PRIMITIVE_TRIANGLES};
use simworld_core::ReadyTileView;

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

pub(crate) fn as_col(s: &[[f32; 4]]) -> &[Color] {
    // SAFETY: Color is repr(C) { r, g, b, a: f32 } (size/align asserted above).
    unsafe { std::slice::from_raw_parts(s.as_ptr().cast::<Color>(), s.len()) }
}

/// An f64 point as a Godot `Vector3` (local metres).
pub(crate) fn v3(p: [f64; 3]) -> Vector3 {
    Vector3::new(p[0] as f32, p[1] as f32, p[2] as f32)
}

/// `"z/x/y"` (world), `"t/z/x/y"` (terrain) or `"l/z/x/y"` (landmarks).
pub(crate) fn key_string(k: LayerKey) -> GString {
    GString::from(&k.to_string())
}

/// `Mesh.ARRAY_MAX`-length Array (VERTEX, NORMAL, COLOR, TEX_UV, INDEX; other slots nil), or
/// an empty Array when there is no geometry. TEX_UV stays nil when the mesh has no UVs (ground).
fn mesh_array(m: &MeshData) -> VarArray {
    let mut a = VarArray::new();
    if m.is_empty() || m.surface.is_some() {
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

/// Terrain mesh (api 1.5): `Mesh.ARRAY_MAX`-length Array with VERTEX and INDEX, plus NORMAL
/// when the mesh has normals (`terrain.shading = "smooth"`) and COLOR in a seed world (api 1.7:
/// r = ground class / 8, g = tint / 8); every other slot nil. Empty Array when there is no
/// terrain mesh (vector tiles).
fn terrain_array(m: &MeshData) -> VarArray {
    let mut a = VarArray::new();
    if m.is_empty() || m.surface.is_some() {
        return a;
    }
    a.resize(ArrayType::MAX.ord() as usize, &Variant::nil());
    a.set(ArrayType::VERTEX.ord() as usize, &PackedVector3Array::from(as_v3(&m.positions)).to_variant());
    if !m.normals.is_empty() {
        a.set(ArrayType::NORMAL.ord() as usize, &PackedVector3Array::from(as_v3(&m.normals)).to_variant());
    }
    if !m.colors.is_empty() {
        a.set(ArrayType::COLOR.ord() as usize, &PackedColorArray::from(as_col(&m.colors)).to_variant());
    }
    a.set(ArrayType::INDEX.ord() as usize, &PackedInt32Array::from(&m.indices[..]).to_variant());
    a
}

/// The contract's ready-tile dictionary of `t`.
pub(crate) fn ready_dict(t: &ReadyTileView<'_>) -> VarDictionary {
    let mut d = VarDictionary::new();
    d.set("key", &key_string(t.layer_key()));
    d.set("layer", &GString::from(t.layer.name()));
    d.set("ring", t.ring as i64);
    d.set("origin", Vector3::new(t.origin[0] as f32, t.origin[1] as f32, t.origin[2] as f32));
    d.set("mesh", &mesh_array(t.mesh));
    // Always present (empty Array for R3 / no ground): Godot replaces the tile's ground.
    d.set("ground", &mesh_array(t.ground));
    d.set("lights", &PackedVector3Array::from(as_v3(t.lights)));
    d.set("light_colors", &PackedColorArray::from(as_col(t.light_colors)));
    d.set("street_lights", &PackedVector3Array::from(as_v3(t.street_lights)));
    d.set("street_light_colors", &PackedColorArray::from(as_col(t.street_light_colors)));
    d.set("beacon_lights", &PackedVector3Array::from(as_v3(t.beacon_lights)));
    d.set("beacon_light_colors", &PackedColorArray::from(as_col(t.beacon_light_colors)));
    // api 1.7: airport lights of a seed world (empty elsewhere).
    d.set("airport_lights", &PackedVector3Array::from(as_v3(t.airport_lights)));
    d.set("airport_light_colors", &PackedColorArray::from(as_col(t.airport_light_colors)));
    // api 1.5: the terrain mesh (empty Array on vector tiles), its height range.
    d.set("terrain", &terrain_array(t.terrain));
    if t.layer == LayerKind::Terrain {
        d.set("h_min", t.h_min);
        d.set("h_max", t.h_max);
    }
    // api 1.11 (`engine_surfaces`, contract section 4a): packed meshes as surface Dictionaries.
    for (name, m) in [("mesh_surface", t.mesh), ("ground_surface", t.ground), ("terrain_surface", t.terrain)] {
        if let Some(s) = &m.surface {
            d.set(name, &surface_dict(s));
        }
    }
    d
}

/// An engine surface as the Dictionary `RenderingServer.mesh_add_surface` and
/// `ArrayMesh._surfaces` take (contract section 4a): one bulk copy per stream.
fn surface_dict(s: &EngineSurface) -> VarDictionary {
    let mut d = VarDictionary::new();
    d.set("primitive", PRIMITIVE_TRIANGLES);
    d.set("format", s.format as i64);
    d.set("vertex_count", s.vertex_count as i64);
    d.set("vertex_data", &PackedByteArray::from(&s.vertex_data[..]));
    d.set("attribute_data", &PackedByteArray::from(&s.attribute_data[..]));
    d.set("index_count", s.index_count as i64);
    d.set("index_data", &PackedByteArray::from(&s.index_data[..]));
    let [px, py, pz] = s.aabb_position;
    let [sx, sy, sz] = s.aabb_size;
    d.set("aabb", Aabb::new(Vector3::new(px, py, pz), Vector3::new(sx, sy, sz)));
    d
}
