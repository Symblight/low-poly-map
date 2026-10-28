//! Engine surfaces (`surface`): the bytes decode back to the arrays they were built from
//! (positions, UVs and indices exact, colors to 8 bits, normals within octahedral precision,
//! tangents orthogonal to the normal), on real tile meshes of every kind.

use super::*;
use crate::data::{build_chunk_with, synthetic_data_tile, DataLevel, DataLevels, DataTile};
use crate::geo::{tile_for, Frame, TileKey};
use crate::profile::{Profile, TerrainShading};
use crate::terrain::Terrain;
use std::sync::Arc;

const VILNIUS: (f64, f64) = (54.6872, 25.2797);

/// The decoded arrays of a surface (the inverse of `EngineSurface::encode`, as Godot's
/// `_get_array_from_surface` reads them): positions, normals, tangents (xyz, sign), colors
/// (bytes), uvs, indices.
#[derive(Default)]
struct Decoded {
    positions: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    tangents: Vec<([f32; 3], f32)>,
    colors: Vec<[u8; 4]>,
    uvs: Vec<[f32; 2]>,
    indices: Vec<i32>,
}

fn f32_at(b: &[u8], at: usize) -> f32 {
    f32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}

fn decode(s: &EngineSurface) -> Decoded {
    let n = s.vertex_count as usize;
    let f = s.format;
    let mut d = Decoded::default();
    assert_eq!(f & ARRAY_FLAG_FORMAT_VERSION_2, ARRAY_FLAG_FORMAT_VERSION_2);
    let normals = f & ARRAY_FORMAT_NORMAL != 0;
    assert_eq!(normals, f & ARRAY_FORMAT_TANGENT != 0, "NORMAL always comes with TANGENT");
    assert_eq!(s.vertex_data.len(), n * if normals { 20 } else { 12 }, "vertex stream size");
    for i in 0..n {
        d.positions.push([0, 1, 2].map(|c| f32_at(&s.vertex_data, i * 12 + c * 4)));
    }
    if normals {
        for i in 0..n {
            let at = n * 12 + i * 8;
            let q = |k: usize| u16_at(&s.vertex_data, at + 2 * k) as f32 / 65535.0;
            d.normals.push(oct_decode([q(0), q(1)]));
            // Godot's octahedron_tangent_decode.
            let y = q(3) * 2.0 - 1.0;
            d.tangents.push((oct_decode([q(2), y.abs()]), if y >= 0.0 { 1.0 } else { -1.0 }));
        }
    }
    let (colors, uvs) = (f & ARRAY_FORMAT_COLOR != 0, f & ARRAY_FORMAT_TEX_UV != 0);
    let stride = if colors { 4 } else { 0 } + if uvs { 8 } else { 0 };
    assert_eq!(s.attribute_data.len(), n * stride, "attribute stream size");
    for i in 0..n {
        let at = i * stride;
        if colors {
            d.colors.push(s.attribute_data[at..at + 4].try_into().unwrap());
        }
        if uvs {
            let u = at + if colors { 4 } else { 0 };
            d.uvs.push([f32_at(&s.attribute_data, u), f32_at(&s.attribute_data, u + 4)]);
        }
    }
    let wide = n > 65536;
    assert_eq!(s.index_data.len(), s.index_count as usize * if wide { 4 } else { 2 }, "index stream size");
    for i in 0..s.index_count as usize {
        d.indices.push(if wide {
            u32::from_le_bytes(s.index_data[i * 4..i * 4 + 4].try_into().unwrap()) as i32
        } else {
            u16_at(&s.index_data, i * 2) as i32
        });
    }
    d
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// `m` packed decodes back to `m` (see the module docs).
fn check_roundtrip(label: &str, m: &MeshData) {
    let s = EngineSurface::encode(m);
    let d = decode(&s);
    assert_eq!(d.positions, m.positions, "{label}: positions");
    assert_eq!(d.uvs, m.uvs, "{label}: uvs");
    assert_eq!(d.indices, m.indices, "{label}: indices");
    let bytes: Vec<[u8; 4]> = m.colors.iter().map(|c| c.map(|v| (v as f64 * 255.0).clamp(0.0, 255.0) as u8)).collect();
    assert_eq!(d.colors, bytes, "{label}: colors");
    assert_eq!(d.normals.len(), m.normals.len(), "{label}: normal count");
    for (i, (got, want)) in d.normals.iter().zip(&m.normals).enumerate() {
        let w = normalized(*want);
        assert!(dot(*got, w) > 0.999_99, "{label}: normal {i} {got:?} vs {w:?}");
        let (t, sign) = d.tangents[i];
        assert!(dot(t, w).abs() < 1e-3 && (dot(t, t) - 1.0).abs() < 1e-3 && sign == 1.0, "{label}: tangent {i} {t:?} for {w:?}");
    }
    // AABB: the positions' bounds.
    for (a, (lo, size)) in s.aabb_position.iter().zip(&s.aabb_size).enumerate() {
        let (mn, mx) = m.positions.iter().fold((f32::MAX, f32::MIN), |(l, h), p| (l.min(p[a]), h.max(p[a])));
        assert_eq!(*lo, mn, "{label}: aabb min");
        assert_eq!(*size, (mx - mn).max(SMALL_EXTENT), "{label}: aabb size");
    }
    assert_eq!(s.bytes(), s.vertex_data.len() + s.attribute_data.len() + s.index_data.len());
    // Packing in place frees the arrays and keeps the counts.
    let mut packed = m.clone();
    packed.pack_engine_surface();
    assert!(packed.positions.is_empty() && packed.indices.is_empty());
    assert_eq!((packed.vertex_count(), packed.triangle_count()), (m.vertex_count(), m.triangle_count()), "{label}: counts");
    assert_eq!(packed.surface.as_deref(), Some(&s), "{label}: deterministic");
}

/// A synthetic city chunk (buildings with UVs, ground without).
fn chunk(key: TileKey, ring: usize, profile: &Profile) -> crate::worker::TileData {
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let levels = DataLevels::SYNTHETIC;
    let dk = levels.data_of(key).unwrap();
    let terrain = Terrain::flat();
    let dt = synthetic_data_tile(dk, levels.level_of(dk), frame, &terrain, profile);
    let lk = levels.landmarks_of(key).unwrap();
    let lt = (lk != dk).then(|| synthetic_data_tile(lk, DataLevel::Overview, frame, &terrain, profile));
    let lm: &DataTile = lt.as_ref().unwrap_or(&dt);
    build_chunk_with(&dt, Some(lm), key, ring, profile, &|| false).unwrap().unwrap()
}

#[test]
fn building_and_ground_surfaces_decode_to_the_arrays() {
    let p = crate::profile::test_default();
    let mut seen = 0;
    for ring in 0..4usize {
        let k = tile_for(VILNIUS.0, VILNIUS.1, 16 - ring as u8);
        let d = chunk(k, ring, p);
        for (what, m) in [("mesh", &d.mesh), ("ground", &d.ground)] {
            if !m.is_empty() {
                check_roundtrip(&format!("r{ring} {what}"), m);
                seen += 1;
            }
        }
    }
    assert!(seen >= 5, "too few meshes checked: {seen}");
}

#[test]
fn terrain_surfaces_decode_to_the_arrays() {
    for shading in [TerrainShading::Flat, TerrainShading::Smooth] {
        let mut w = crate::profile::WorldProfile::default();
        w.terrain.enabled = true;
        w.terrain.shading = shading;
        let p = Profile::new(w).unwrap();
        let frame = Frame::new(VILNIUS.0, VILNIUS.1);
        let t = Terrain::from_profile(&p, VILNIUS.0, VILNIUS.1);
        let k = tile_for(VILNIUS.0 + 0.1, VILNIUS.1, 13);
        let tile = crate::terrain::tin::build_terrain_tile(k, 1, frame, &t, &p, 1.0).unwrap();
        assert_eq!(tile.mesh.normals.is_empty(), shading == TerrainShading::Flat);
        check_roundtrip(&format!("terrain {shading:?}"), &tile.mesh);
    }
}

/// Above 65536 vertices the index stream is 32-bit; empty meshes stay unpacked.
#[test]
fn wide_indices_and_empty_meshes() {
    let n = 70_000usize;
    let mut m = MeshData::default();
    for i in 0..n {
        m.positions.push([i as f32 * 0.01, (i % 7) as f32, -(i as f32) * 0.02]);
        m.normals.push([[0.0, 1.0, 0.0], [0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.3, 0.2, -0.9]][i % 4]);
        m.colors.push([(i % 8) as f32 / 8.0, 0.5, 1.0, 1.0]);
    }
    m.indices = (0..n as i32 - 2).flat_map(|i| [i, i + 1, i + 2]).collect();
    check_roundtrip("wide", &m);
    let mut e = MeshData::default();
    e.pack_engine_surface();
    assert!(e.surface.is_none() && e.is_empty());
}

/// The format bits Godot gives the same arrays (`mesh_create_surface_data_from_arrays`).
#[test]
fn format_bits_match_godot() {
    let p = crate::profile::test_default();
    let d = chunk(tile_for(VILNIUS.0, VILNIUS.1, 16), 0, p);
    // VERTEX | NORMAL | TANGENT | COLOR | TEX_UV | INDEX | version 2.
    assert_eq!(EngineSurface::encode(&d.mesh).format, 0b1_0000_0001_1111 | (1 << 35));
    // Ground: no TEX_UV.
    assert_eq!(EngineSurface::encode(&d.ground).format, 0b1_0000_0000_1111 | (1 << 35));
    assert_eq!(PRIMITIVE_TRIANGLES, 3);
}

/// `configure_world` option `engine_surfaces`: every delivered mesh (buildings, ground, terrain)
/// comes packed (arrays empty), and equals the packing of what a world without the option
/// delivers for the same key.
#[test]
fn a_world_with_engine_surfaces_delivers_packed_meshes() {
    use crate::rings::LayerKey;
    use crate::world::{World, WorldOptions};
    use std::collections::HashMap;
    type Meshes = [MeshData; 3];
    let run = |surfaces: bool| -> HashMap<LayerKey, Meshes> {
        let mut w = World::new(Arc::new(crate::NullLog), crate::Diagnostics::default(), "test");
        let o = WorldOptions {
            origin_lat: VILNIUS.0,
            origin_lon: VILNIUS.1,
            profile_overrides: Some(serde_json::json!({ "terrain": { "enabled": true } })),
            layers: Some(vec!["world".into(), "terrain".into()]),
            engine_surfaces: surfaces,
            ..WorldOptions::default()
        };
        assert!(w.configure_world(o).ok);
        let mut got = HashMap::new();
        let t0 = std::time::Instant::now();
        while got.len() < 60 && t0.elapsed() < std::time::Duration::from_secs(60) {
            w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
            w.pop_ready(16, |v| {
                got.insert(v.layer_key(), [v.mesh.clone(), v.ground.clone(), v.terrain.clone()]);
            });
            let _ = w.drain_fades();
            let _ = w.drain_unload();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(w.shutdown());
        got
    };
    let packed = run(true);
    let arrays = run(false);
    let mut compared = 0;
    for (k, p) in &packed {
        for m in p {
            assert!(m.positions.is_empty() && m.indices.is_empty(), "{k}: arrays delivered with engine_surfaces");
        }
        let Some(a) = arrays.get(k) else { continue };
        for (pm, am) in p.iter().zip(a) {
            assert_eq!(pm.surface.as_deref(), (!am.is_empty()).then(|| EngineSurface::encode(am)).as_ref(), "{k}");
            compared += !am.is_empty() as usize;
        }
    }
    assert!(compared > 20, "only {compared} meshes compared");
}
