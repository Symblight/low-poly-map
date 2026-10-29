//! Buildings -> flat, merged vertex buffers (job pool worker; pure Rust, no Godot types).
//!
//! Per building: resolve profile / heights (`resolve`), clip the footprint to the target
//! rectangle in exact tile pixel space, Douglas–Peucker simplify (0.5 m R0, 2 m R1; vertices
//! on a clip line are locked so seams stay exact), project the survivors to ENU meters,
//! orient rings (exterior CCW, holes CW seen from above), then extrude from `min_height` to
//! `height`: one flat-shaded quad per edge + an earcut roof (with holes). Everything of one
//! target rectangle is merged into one `MeshData` (one draw call).
//!
//! Wall UV (api 1.0, docs/spec-night-windows.md section 2): windowed walls carry `u` in window
//! columns (whole columns per run between corners) and `v` in floors; windowless walls (and
//! every R2 wall) carry `u = -1` and `v` in meters. Roofs and bottom caps: tile-local meters.
//!
//! Tile partitioning: every building fragment inside the tile rectangle is meshed, clipped to
//! it, instead of whole buildings by centroid. A child tile's rectangle is exactly one quarter
//! of its parent's, so the 4 children of a tile cover the same geometry as the parent (no
//! double buildings, no holes when the sibling group swaps with its parent), even for
//! buildings that cross tile edges and come clipped (+4 px buffer) out of the encoder. Wall edges lying on a clip line are interior and
//! skipped. For buildings that do not cross an edge (the vast majority) this is identical to
//! assigning the building to the child that contains its centroid.
//!
//! Godot conventions: x = east, y = up, z = -north; triangles are clockwise when seen from
//! the front (Godot's front-face winding), i.e. `(c - a) x (b - a)` points along the normal.

use crate::mvt::{ring_area2, Building, TileProjector};
use crate::profile::Profile;

pub use crate::mesh_data::MeshData;

mod attrs;
mod clip;
mod emit;
mod walls;

pub use attrs::{
    fallback_levels, resolve, vertex_color, Resolved, FLOOR_H_DEFAULT, MAX_HEIGHT_M, PROFILE_CIVIC, PROFILE_COUNT, PROFILE_DARK,
    PROFILE_INDUSTRIAL, PROFILE_LANDMARK, PROFILE_OFFICE, PROFILE_RESIDENTIAL, PROFILE_RETAIL, PROFILE_UNKNOWN,
};
pub(crate) use clip::clip_ring;
pub use clip::{quadrant_of, simplify_ring, ClipRect};

use clip::bbox;
use emit::emit_polygon;
use walls::{GroundWalls, Run, WallLayout};

/// Per-ring meshing parameters (world profile `buildings.r0..r2`; `Profile::lod`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LodParams {
    /// Douglas–Peucker tolerance (m).
    pub dp_m: f64,
    /// Drop buildings whose (unclipped) footprint is smaller than this (m²).
    pub min_area_m2: f64,
    /// Drop buildings whose resolved roof height ([`Resolved::height`]: tagged `height`, else
    /// levels x floor height, else the kind/landuse fallback; above ground, including any
    /// `min_height`) is below this (m). Per feature, like `min_area_m2`. 0 = keep all. Mesh
    /// only: light points are generated from all buildings.
    pub min_height_m: f64,
    /// Walls of this band may carry windows (false at R2: R2 has light sprites, and coarse
    /// merged blocks must not carry a window grid; docs/spec-night-windows.md section 1). A
    /// structural rule of the band, set by `Profile::lod`, not a profile key.
    pub windows: bool,
}

/// Reusable scratch buffers (one per worker job).
#[derive(Default)]
struct Scratch {
    clip_a: Vec<[f64; 2]>,
    clip_b: Vec<[f64; 2]>,
    dedup: Vec<[f64; 2]>,
    locked: Vec<bool>,
    keep: Vec<bool>,
    kept: Vec<usize>,
    /// Current polygon: rings in ENU meters + per-vertex "on clip line" flags.
    rings: Vec<Vec<[f64; 2]>>,
    ring_flags: Vec<Vec<u8>>,
    /// Tile pixels of the ring vertices (same order as `rings`), for the per-vertex wall bottoms.
    ring_px: Vec<Vec<[f64; 2]>>,
    /// Current wall edge, subdivided for the terrain: (ENU, tile pixel, ground height, share
    /// of the edge).
    edge_pts: Vec<([f64; 2], [f64; 2], f64, f64)>,
    flat: Vec<f64>,
    holes: Vec<usize>,
    /// Wall runs of the current ring (`ring_runs`) and its clip-seam edges.
    runs: Vec<Run>,
    seam: Vec<bool>,
}

/// Mesh every building fragment inside `rect` (tile pixels) into one merged buffer.
pub fn mesh_buildings(buildings: &[Building], proj: &TileProjector, rect: ClipRect, params: LodParams, profile: &Profile) -> MeshData {
    mesh_building_iter(buildings, proj, rect, params, profile)
}

/// [`mesh_buildings`] over any sequence of buildings (a chunk's candidates from its data tile).
pub fn mesh_building_iter<'a>(
    buildings: impl IntoIterator<Item = &'a Building>,
    proj: &TileProjector,
    rect: ClipRect,
    params: LodParams,
    profile: &Profile,
) -> MeshData {
    let mut m = MeshData::default();
    let mut s = Scratch::default();
    let m_per_px = proj.m_per_px();
    let tol_px = params.dp_m / m_per_px;
    let min_area_px2 = params.min_area_m2 / (m_per_px * m_per_px);
    // Terrain on (api 1.7): walls end at the exact ground under each vertex, foundations reach
    // `foundation_m` below it; 0 = flat world, no foundations, walls from the base.
    let band = crate::rings::ring_for_zoom(proj.key().z).unwrap_or(0);
    let foundation_m = proj.terrain().foundation_m(band);
    let ground = proj.terrain().is_active().then_some(GroundWalls {
        proj,
        tol_m: (proj.terrain().drape_margin_m(band) as f64).max(crate::ground::DRAPE_FLAT_TOLERANCE_M),
        step_m: proj.terrain().drape_step_m(),
    });
    for b in buildings {
        let bb = bbox(&b.polygons);
        if bb[2] <= rect.x0 || bb[0] >= rect.x1 || bb[3] <= rect.y0 || bb[1] >= rect.y1 {
            continue;
        }
        let res = resolve(b, profile);
        if res.height < res.min_height + 0.5 {
            continue;
        }
        if params.min_height_m > 0.0 && (res.height as f64) < params.min_height_m {
            continue;
        }
        if min_area_px2 > 0.0 {
            let area: f64 = b
                .polygons
                .iter()
                .map(|p| {
                    p.first().map_or(0.0, |e| ring_area2(e).abs())
                        - p.iter().skip(1).map(|h| ring_area2(h).abs()).sum::<f64>()
                })
                .sum::<f64>()
                * 0.5;
            if area < min_area_px2 {
                continue;
            }
        }
        let needs_clip = bb[0] < rect.x0 || bb[2] > rect.x1 || bb[1] < rect.y0 || bb[3] > rect.y1;
        let color = vertex_color(&res);
        let walls = WallLayout {
            window_w: if params.windows && res.windows { profile.window_width_m(res.profile).map(f64::from) } else { None },
            min_window_wall_m: profile.min_window_wall_m(),
            foundation_m,
        };
        for poly in &b.polygons {
            if needs_clip && crate::polyclip::hole_crosses(poly, &rect) {
                // A hole reaches the clip boundary: clip the polygon as a whole (the hole notches
                // the outer ring; its walls stay, the seam edges get none).
                for piece in crate::polyclip::clip_polygon(poly, &rect) {
                    if prepare_polygon(&piece, proj, &rect, Clip::Done, tol_px, &mut s) {
                        emit_polygon(&mut m, &mut s, &res, color, b.base_m, walls, ground);
                    }
                }
            } else {
                let clip = if needs_clip { Clip::Rings } else { Clip::None };
                if prepare_polygon(poly, proj, &rect, clip, tol_px, &mut s) {
                    emit_polygon(&mut m, &mut s, &res, color, b.base_m, walls, ground);
                }
            }
        }
    }
    m.shrink();
    m
}

/// How [`prepare_polygon`] treats the clip rect.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Clip {
    /// Polygon inside the rect: no clip, no seams.
    None,
    /// Clip every ring on its own ([`clip_ring`]; right when no hole reaches the boundary).
    Rings,
    /// Already clipped as a whole ([`crate::polyclip::clip_polygon`]): seams and locks only.
    Done,
}

/// Clip + simplify + project + orient one polygon into `s.rings` / `s.ring_flags`.
/// Returns false if the exterior vanished.
fn prepare_polygon(
    poly: &[Vec<[f64; 2]>],
    proj: &TileProjector,
    rect: &ClipRect,
    clip: Clip,
    tol_px: f64,
    s: &mut Scratch,
) -> bool {
    let needs_clip = clip != Clip::None;
    s.rings.clear();
    s.ring_flags.clear();
    s.ring_px.clear();
    for (ri, ring) in poly.iter().enumerate() {
        if clip == Clip::Rings {
            clip_ring(ring, rect, &mut s.clip_a, &mut s.clip_b);
        } else {
            s.clip_a.clear();
            s.clip_a.extend_from_slice(ring);
        }
        // Drop consecutive duplicates (incl. wrap-around).
        s.dedup.clear();
        for &p in &s.clip_a {
            if s.dedup.last() != Some(&p) {
                s.dedup.push(p);
            }
        }
        while s.dedup.len() > 1 && s.dedup.first() == s.dedup.last() {
            s.dedup.pop();
        }
        if s.dedup.len() < 3 {
            if ri == 0 {
                return false;
            }
            continue;
        }
        s.locked.clear();
        s.locked.extend(s.dedup.iter().map(|&p| needs_clip && rect.flags(p) != 0));
        simplify_ring(&s.dedup, &s.locked, tol_px, &mut s.keep, &mut s.kept);
        if s.kept.len() < 3 {
            if ri == 0 {
                return false;
            }
            continue;
        }
        let mut pts: Vec<[f64; 2]> = s.kept.iter().map(|&i| proj.project64(s.dedup[i])).collect();
        let mut pxs: Vec<[f64; 2]> = s.kept.iter().map(|&i| s.dedup[i]).collect();
        let mut flags: Vec<u8> = s
            .kept
            .iter()
            .map(|&i| if needs_clip { rect.flags(s.dedup[i]) } else { 0 })
            .collect();
        let a = ring_area2(&pts);
        if a.abs() < 1e-6 {
            if ri == 0 {
                return false;
            }
            continue;
        }
        // ENU is y-up: exterior CCW (a > 0), holes CW.
        if (ri == 0) != (a > 0.0) {
            pts.reverse();
            flags.reverse();
            pxs.reverse();
        }
        s.rings.push(pts);
        s.ring_flags.push(flags);
        s.ring_px.push(pxs);
    }
    !s.rings.is_empty()
}

/// Ring-dependent meshing: one merged mesh per tile, clipped exactly to the tile rectangle
/// (R0 full detail, R1 boxes, R2 coarse merged blocks); R3 has no mesh. A cut projector
/// ([`TileProjector::cut`]) meshes its chunk's rectangle of the data tile.
pub fn mesh_tile(buildings: &[Building], proj: &TileProjector, ring: usize, profile: &Profile) -> MeshData {
    match profile.lod(ring) {
        Some(p) => mesh_buildings(buildings, proj, proj.rect(), p, profile),
        None => MeshData::default(),
    }
}

#[cfg(test)]
mod tests;
