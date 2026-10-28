//! Engine-layout mesh surfaces (perf review 3 item 1, api 1.11, `docs/contract.md` section 4a).
//!
//! The host's upload of a tile mesh was dominated by Godot packing the arrays into its vertex
//! format on the main thread (`ArrayMesh.AddSurfaceFromArrays`: about 1.3 ms for a
//! 20 000-vertex building tile, 0.9 ms of it the normal / tangent encoding). This module builds
//! exactly those bytes on the job pool, so the host only hands them to the renderer
//! (`RenderingServer.mesh_add_surface` / `ArrayMesh._surfaces`).
//!
//! The layout is Godot 4's **uncompressed** surface format, version 2 (the one
//! `mesh_create_surface_data_from_arrays` produces without `ARRAY_FLAG_COMPRESS_ATTRIBUTES`;
//! `servers/rendering/rendering_server.cpp`, 4.7.2-stable: `_surface_set_data` lines 369-984,
//! `mesh_surface_make_offsets_from_format` 1055-1177, `mesh_create_surface_data_from_arrays`
//! 1179-1393; flags in `rendering_server_enums.h` 127-202). Every byte below is what Godot writes
//! for the same arrays, within the rounding of its float maths:
//!
//! - **vertex stream**: `vertex_count` positions (3 x f32), then, with NORMAL, `vertex_count`
//!   normal / tangent pairs (2 x u16 octahedral normal, 2 x u16 octahedral tangent). The tangent
//!   is the arbitrary one Godot generates when NORMAL has no TANGENT array
//!   (`Vector3(n.z, -n.x, n.y).cross(n.normalized()).normalized()`, sign +1); the format then
//!   carries `ARRAY_FORMAT_TANGENT` too ("we must use tangents if using normals").
//! - **attribute stream**: per vertex COLOR as RGBA8 (`trunc(clamp(c * 255.0, 0, 255))`, the
//!   multiply in f64), then TEX_UV as 2 x f32 (stride 4 + 8 when both are present).
//! - **index stream**: u16 when `vertex_count <= 65536`, else u32.
//! - **aabb**: min corner and size of the positions, the size at least 1e-5 per axis.
//!
//! All little-endian. Deterministic: a pure function of the arrays.

use crate::mesh_data::MeshData;

/// `RenderingServer.ARRAY_FORMAT_*` bits used here (`1 << ARRAY_*`).
pub const ARRAY_FORMAT_VERTEX: u64 = 1 << 0;
pub const ARRAY_FORMAT_NORMAL: u64 = 1 << 1;
pub const ARRAY_FORMAT_TANGENT: u64 = 1 << 2;
pub const ARRAY_FORMAT_COLOR: u64 = 1 << 3;
pub const ARRAY_FORMAT_TEX_UV: u64 = 1 << 4;
pub const ARRAY_FORMAT_INDEX: u64 = 1 << 12;
/// `ARRAY_FLAG_FORMAT_VERSION_2` (= `ARRAY_FLAG_FORMAT_CURRENT_VERSION`, `1 << 35`).
pub const ARRAY_FLAG_FORMAT_VERSION_2: u64 = 1 << 35;
/// `RenderingServer.PRIMITIVE_TRIANGLES`.
pub const PRIMITIVE_TRIANGLES: i64 = 3;
/// Godot's `SMALL_VEC3` (`CMP_EPSILON`): the smallest AABB extent per axis.
const SMALL_EXTENT: f32 = 0.00001;

/// One mesh surface in the renderer's layout (see the module docs).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EngineSurface {
    /// `ARRAY_FORMAT_*` bits | `ARRAY_FLAG_FORMAT_VERSION_2`.
    pub format: u64,
    pub vertex_count: u32,
    pub index_count: u32,
    pub vertex_data: Vec<u8>,
    pub attribute_data: Vec<u8>,
    pub index_data: Vec<u8>,
    /// AABB position (min corner) and size, tile-local metres.
    pub aabb_position: [f32; 3],
    pub aabb_size: [f32; 3],
}

impl EngineSurface {
    /// The engine form of `m` (array form, at least one triangle).
    pub fn encode(m: &MeshData) -> Self {
        let n = m.positions.len();
        let has_normals = !m.normals.is_empty();
        let (has_colors, has_uvs) = (!m.colors.is_empty(), !m.uvs.is_empty());
        debug_assert!((!has_normals || m.normals.len() == n) && (!has_colors || m.colors.len() == n) && (!has_uvs || m.uvs.len() == n));
        let mut format = ARRAY_FORMAT_VERTEX | ARRAY_FLAG_FORMAT_VERSION_2;
        if has_normals {
            format |= ARRAY_FORMAT_NORMAL | ARRAY_FORMAT_TANGENT;
        }
        if has_colors {
            format |= ARRAY_FORMAT_COLOR;
        }
        if has_uvs {
            format |= ARRAY_FORMAT_TEX_UV;
        }
        if !m.indices.is_empty() {
            format |= ARRAY_FORMAT_INDEX;
        }

        let mut vertex_data = Vec::with_capacity(n * if has_normals { 20 } else { 12 });
        for p in &m.positions {
            for c in p {
                vertex_data.extend_from_slice(&c.to_le_bytes());
            }
        }
        if has_normals {
            for nv in &m.normals {
                let [ox, oy] = quantize2(oct_encode(*nv));
                let [tx, ty] = tangent_bits(*nv);
                for v in [ox, oy, tx, ty] {
                    vertex_data.extend_from_slice(&v.to_le_bytes());
                }
            }
        }

        let stride = if has_colors { 4 } else { 0 } + if has_uvs { 8 } else { 0 };
        let mut attribute_data = Vec::with_capacity(n * stride);
        if stride > 0 {
            for i in 0..n {
                if has_colors {
                    attribute_data.extend_from_slice(&color8(m.colors[i]));
                }
                if has_uvs {
                    attribute_data.extend_from_slice(&m.uvs[i][0].to_le_bytes());
                    attribute_data.extend_from_slice(&m.uvs[i][1].to_le_bytes());
                }
            }
        }

        let wide = n > 65536;
        let mut index_data = Vec::with_capacity(m.indices.len() * if wide { 4 } else { 2 });
        for &i in &m.indices {
            if wide {
                index_data.extend_from_slice(&(i as u32).to_le_bytes());
            } else {
                index_data.extend_from_slice(&(i as u16).to_le_bytes());
            }
        }

        let (aabb_position, aabb_size) = aabb(&m.positions);
        Self {
            format,
            vertex_count: n as u32,
            index_count: m.indices.len() as u32,
            vertex_data,
            attribute_data,
            index_data,
            aabb_position,
            aabb_size,
        }
    }

    /// Heap bytes of the three streams.
    pub fn bytes(&self) -> usize {
        self.vertex_data.len() + self.attribute_data.len() + self.index_data.len()
    }
}

/// Godot's `Vector3::octahedron_encode` (f32): `n` need not be unit length.
pub fn oct_encode(n: [f32; 3]) -> [f32; 2] {
    let s = n[0].abs() + n[1].abs() + n[2].abs();
    let (x, y, z) = (n[0] / s, n[1] / s, n[2] / s);
    let (ox, oy) = if z >= 0.0 {
        (x, y)
    } else {
        ((1.0 - y.abs()) * if x >= 0.0 { 1.0 } else { -1.0 }, (1.0 - x.abs()) * if y >= 0.0 { 1.0 } else { -1.0 })
    };
    [ox * 0.5 + 0.5, oy * 0.5 + 0.5]
}

/// Godot's `Vector3::octahedron_decode` (f32).
pub fn oct_decode(o: [f32; 2]) -> [f32; 3] {
    let (fx, fy) = (o[0] * 2.0 - 1.0, o[1] * 2.0 - 1.0);
    let mut n = [fx, fy, 1.0 - fx.abs() - fy.abs()];
    let t = (-n[2]).clamp(0.0, 1.0);
    n[0] += if n[0] >= 0.0 { -t } else { t };
    n[1] += if n[1] >= 0.0 { -t } else { t };
    normalized(n)
}

/// `(uint16_t)CLAMP(v * 65535, 0, 65535)` per component.
fn quantize2(o: [f32; 2]) -> [u16; 2] {
    o.map(|v| (v * 65535.0).clamp(0.0, 65535.0) as u16)
}

/// The tangent Godot generates for a NORMAL without a TANGENT array, octahedral with sign +1
/// (`Vector3::octahedron_tangent_encode`), quantized and sanitized as in `_surface_set_data`.
fn tangent_bits(n: [f32; 3]) -> [u16; 2] {
    let t = normalized(cross([n[2], -n[0], n[1]], normalized(n)));
    let mut o = oct_encode(t);
    o[1] = o[1].max(1.0 / 32767.0) * 0.5 + 0.5;
    let mut q = quantize2(o);
    if q == [0, 65535] {
        q[0] = 65535; // (0, 1) and (1, 1) decode alike; Godot keeps (1, 1)
    }
    q
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// Godot's `Vector3::normalized` (zero stays zero).
fn normalized(v: [f32; 3]) -> [f32; 3] {
    let l2 = v[0] * v[0] + v[1] * v[1] + v[2] * v[2];
    if l2 == 0.0 {
        return [0.0; 3];
    }
    let l = l2.sqrt();
    [v[0] / l, v[1] / l, v[2] / l]
}

/// Godot's COLOR packing: `uint8_t(CLAMP(c * 255.0, 0.0, 255.0))` with a double multiply.
fn color8(c: [f32; 4]) -> [u8; 4] {
    c.map(|v| (v as f64 * 255.0).clamp(0.0, 255.0) as u8)
}

/// Min corner and size (at least [`SMALL_EXTENT`] per axis) of `p` (non-empty).
fn aabb(p: &[[f32; 3]]) -> ([f32; 3], [f32; 3]) {
    let Some(first) = p.first() else { return ([0.0; 3], [0.0; 3]) };
    let (mut lo, mut hi) = (*first, *first);
    for v in p {
        for a in 0..3 {
            lo[a] = lo[a].min(v[a]);
            hi[a] = hi[a].max(v[a]);
        }
    }
    (lo, [0, 1, 2].map(|a| (hi[a] - lo[a]).max(SMALL_EXTENT)))
}

#[cfg(test)]
#[path = "surface_tests.rs"]
mod tests;
