//! Ground features -> one flat merged ground mesh per tile (M5 "Ground"; rayon worker, pure
//! Rust, no Godot types).
//!
//! * Areas (`landuse` layer: landuse, parks, forest, water) are clipped exactly to the tile
//!   rectangle in tile pixel space (holes included), Douglas–Peucker simplified with vertices on
//!   the clip lines locked (seams stay exact), projected to ENU and triangulated with `earcutr`.
//! * Roads (`roads` layer) are clipped to the tile rectangle, simplified, projected and buffered
//!   into flat strips: miter joins up to 120° turns (miter <= 2 half-widths), bevel joins
//!   (plus a fill triangle on the outer side) beyond that, butt caps. A road leaving the tile
//!   ends with a butt cap on the tile edge, where the neighbour continues it with the same cap.
//! * Heights (terrain, api 1.4): every vertex sits on the projector's terrain (area vertices at
//!   their own point, strip vertices at their centerline point; water likewise unless
//!   `terrain.water_snap_m` > 0 snaps its corners to the minimum within that radius,
//!   `Terrain::water_height`) plus the band's drape margin (`Terrain::drape_margin_m`, a
//!   quarter of the terrain error at that distance; 0 with the terrain off) plus a small
//!   per-level lift against z-fighting: `level * step(ring)`. Since api 1.7 there is no drape
//!   lift over the terrain mesh: the terrain mesh is sunk under the height function instead
//!   (contract section 9a), and the margin only covers the drape's own chord error (an edge is
//!   subdivided once the height rises more than the margin above it), so the drawn ground is
//!   not under the exact height at the tested edge points (a triangle whose edges are all
//!   shorter than `drape_step_m` is not split and can sag a little under a concave height,
//!   contract section 9 Drape). Levels bottom to top: 0 nature_reserve, 1 other landuse,
//!   2 residential, 3 industrial, 4 commercial, 5 grass/park, 6 forest, 7 water, 8..13 roads
//!   (other .. motorway). The step grows with the band (seen from farther away).
//! * Airports (seed world, contract section 9 "Airports"): on a tile near an airport the drape
//!   margin is per vertex, `margin(band) x Terrain::lift_weight` (0 inside a fence, smoothstep
//!   over the shoulder), and the fence quads (`GroundFeatures::cutouts`) are cut out of every `cut`
//!   area, so the airport's own levels 1..3 lie directly on the levelled strip height.
//! * COLOR (contract): `r` = class / 16, `g` = lit (roads with lit=yes), `b` = road class / 8,
//!   `a` = 1. NORMAL = +Y. No UV. Triangles are clockwise seen from above (Godot front face).
//!
//! Coarser bands are simplified more (world profile `ground.r0..r3`, `Profile::ground_params`):
//! bigger DP tolerances, small areas dropped, minor paths dropped at R1, all "other" roads
//! dropped at R2, only secondary and more important roads at R3 (api 1.3: R3 had no ground, so a
//! far tile was see-through while its R2 children loaded). Road widths and the kind -> class tables also come from the
//! profile (`ground.roads`, `ground.areas`).

use crate::mesher::{ClipRect, MeshData};

use crate::mvt::{ring_area2, GroundFeatures, TileProjector, GROUND_ROAD, GROUND_WATER, ROAD_MOTORWAY, ROAD_OTHER};
use crate::profile::Profile;

mod area;
mod drape;
mod road;

pub use drape::{drape_split, DRAPE_FIELD_TOLERANCE_M, DRAPE_FLAT_TOLERANCE_M};

use area::emit_area;
use road::emit_road;

/// Per-band ground meshing parameters (world profile `ground.r0..r3`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroundParams {
    /// DP tolerance for area outlines (m).
    pub area_dp_m: f64,
    /// Drop area polygons (unclipped, holes subtracted) smaller than this (m²).
    pub min_area_m2: f64,
    /// DP tolerance for road centerlines (m).
    pub road_dp_m: f64,
    /// Keep roads with class <= this.
    pub max_road_class: u8,
    /// Keep minor paths (footway, path, cycleway, steps, track, ...).
    pub minor_paths: bool,
    /// Vertical lift per stacking level (m).
    pub step_m: f32,
}

/// Stacking level of a road strip: above every area level (0..7), major roads on top.
pub fn road_level(class: u8) -> u8 {
    8 + (ROAD_OTHER - class.clamp(ROAD_MOTORWAY, ROAD_OTHER))
}

/// Contract COLOR of an area vertex.
pub fn area_color(class: u8) -> [f32; 4] {
    [class as f32 / 16.0, 0.0, 0.0, 1.0]
}

/// Contract COLOR of an area vertex with a tint (`g` = tint / 8) and a sub-kind (`b` = sub / 8;
/// api 1.7, class 7 `aeroway`). Both 0 = [`area_color`].
pub fn area_color_ext(class: u8, tint: u8, sub: u8) -> [f32; 4] {
    [class as f32 / 16.0, tint as f32 / 8.0, sub as f32 / 8.0, 1.0]
}

/// Contract COLOR of a road vertex.
pub fn road_color(class: u8, lit: bool) -> [f32; 4] {
    [GROUND_ROAD as f32 / 16.0, if lit { 1.0 } else { 0.0 }, class as f32 / 8.0, 1.0]
}

const UP: [f32; 3] = [0.0, 1.0, 0.0];

#[derive(Default)]
struct Scratch {
    clip_a: Vec<[f64; 2]>,
    clip_b: Vec<[f64; 2]>,
    dedup: Vec<[f64; 2]>,
    locked: Vec<bool>,
    keep: Vec<bool>,
    kept: Vec<usize>,
    flat: Vec<f64>,
    hs: Vec<f64>,
    /// Tile pixels of the area vertices in `flat` (drape subdivision).
    pxs: Vec<[f64; 2]>,
    holes: Vec<usize>,
    run: Vec<[f64; 2]>,
    pts: Vec<[f64; 2]>,
    ph: Vec<f64>,
}

/// Mesh a tile's ground features for `ring` into one merged buffer.
pub fn mesh_ground(g: &GroundFeatures, proj: &TileProjector, ring: usize, profile: &Profile) -> MeshData {
    let Some(p) = profile.ground_params(ring) else { return MeshData::default() };
    let mut m = MeshData::default();
    let mut s = Scratch::default();
    // The tile, or the chunk of a cut projector (Tiles v2: chunks cut from a data tile).
    let rect = proj.rect();
    let m_per_px = proj.m_per_px();
    let min_area_px2 = p.min_area_m2 / (m_per_px * m_per_px);
    // Drape margin of the band (0.0 with the terrain off: `0.0 + x` keeps the flat output bit for bit).
    let band_margin = proj.terrain().drape_margin_m(ring);
    // A seed world's airport near this tile: the margin varies per vertex (0 inside a fence,
    // smoothstep to the band margin over the shoulder; contract section 9 "Airports"). Every
    // other tile keeps the constant margin.
    let field = (band_margin > 0.0 && proj.terrain().lift_field_touches(proj.key())).then_some(band_margin as f64);
    let base_lift = if field.is_some() { 0.0 } else { band_margin };
    let ctx = AreaCtx { proj, rect: &rect, field, cutouts: &g.cutouts, step: p.step_m as f64 };
    for a in &g.areas {
        if a.rings.is_empty() || a.rings[0].len() < 3 {
            continue;
        }
        if min_area_px2 > 0.0 {
            let area = (ring_area2(&a.rings[0]).abs() - a.rings[1..].iter().map(|r| ring_area2(r).abs()).sum::<f64>()) * 0.5;
            if area < min_area_px2 {
                continue;
            }
        }
        let y = base_lift + a.level as f32 * p.step_m;
        emit_area(&mut m, &mut s, &a.rings, &ctx, p.area_dp_m / m_per_px, y, area_color_ext(a.class, a.tint, a.sub), a.class == GROUND_WATER, a.cut);
    }
    for r in &g.roads {
        if r.class > p.max_road_class || (r.minor && !p.minor_paths) {
            continue;
        }
        let hw = profile.road_width_m(r.class, r.link) * 0.5;
        emit_road(&mut m, &mut s, r, proj, &rect, p.road_dp_m / m_per_px, base_lift + road_level(r.class) as f32 * p.step_m, hw, field, p.step_m as f64);
    }
    m.positions.shrink_to_fit();
    m.normals.shrink_to_fit();
    m.colors.shrink_to_fit();
    m.indices.shrink_to_fit();
    m
}

fn bbox(ring: &[[f64; 2]]) -> [f64; 4] {
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for p in ring {
        b[0] = b[0].min(p[0]);
        b[1] = b[1].min(p[1]);
        b[2] = b[2].max(p[0]);
        b[3] = b[3].max(p[1]);
    }
    b
}

/// Push one triangle facing +Y (clockwise seen from above in Godot x/z), skipping slivers.
fn tri_up(m: &mut MeshData, a: i32, b: i32, c: i32) {
    let (pa, pb, pc) = (m.positions[a as usize], m.positions[b as usize], m.positions[c as usize]);
    // ((c - a) x (b - a)).y
    let ny = (pc[2] - pa[2]) * (pb[0] - pa[0]) - (pc[0] - pa[0]) * (pb[2] - pa[2]);
    if ny.abs() < 1e-4 {
        return;
    }
    if ny > 0.0 {
        m.indices.extend_from_slice(&[a, b, c]);
    } else {
        m.indices.extend_from_slice(&[a, c, b]);
    }
}

fn push_v(m: &mut MeshData, en: [f64; 2], y: f64, color: [f32; 4]) -> i32 {
    let i = m.positions.len() as i32;
    m.positions.push([en[0] as f32, y as f32, -(en[1] as f32)]);
    m.normals.push(UP);
    m.colors.push(color);
    i
}

/// Per-tile inputs of [`emit_area`].
struct AreaCtx<'a> {
    proj: &'a TileProjector,
    rect: &'a ClipRect,
    /// `Some(band margin)`: the drape margin varies per vertex by
    /// [`crate::terrain::Terrain::lift_weight`] and is part of the draped height; `None`: the
    /// margin is constant (in the `lift` argument).
    field: Option<f64>,
    /// Airport fences removed from areas with `cut` (tile pixels).
    cutouts: &'a [[[f64; 2]; 4]],
    /// The band's stacking step (m).
    step: f64,
}

#[cfg(test)]
mod tests;
