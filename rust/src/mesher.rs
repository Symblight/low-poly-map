//! Buildings -> flat, merged vertex buffers (rayon worker; pure Rust, no Godot types).
//!
//! Per building: resolve profile / heights (`resolve`), clip the footprint to the target
//! rectangle in exact tile pixel space, Douglas–Peucker simplify (0.5 m R0, 2 m R1; vertices
//! on a clip line are locked so seams stay exact), project the survivors to ENU meters,
//! orient rings (exterior CCW, holes CW seen from above), then extrude from `min_height` to
//! `height`: one flat-shaded quad per edge + an earcut roof (with holes). Everything of one
//! target rectangle is merged into one `MeshData` (one draw call).
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

use crate::mvt::{ring_area2, Building, Landuse, TileProjector};

/// Merged mesh in `Mesh.ARRAY_MAX` order (VERTEX, NORMAL, COLOR, TEX_UV, INDEX), positions
/// relative to the tile origin (tile center, y = 0).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub colors: Vec<[f32; 4]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<i32>,
}

impl MeshData {
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
    /// Approximate heap bytes of the buffers (for budget reporting).
    pub fn bytes(&self) -> usize {
        self.positions.len() * (12 + 12 + 16 + 8) + self.indices.len() * 4
    }
    fn shrink(&mut self) {
        self.positions.shrink_to_fit();
        self.normals.shrink_to_fit();
        self.colors.shrink_to_fit();
        self.uvs.shrink_to_fit();
        self.indices.shrink_to_fit();
    }
}

/// Per-ring meshing parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LodParams {
    /// Douglas–Peucker tolerance (m).
    pub dp_m: f64,
    /// Drop buildings whose (unclipped) footprint is smaller than this (m²).
    pub min_area_m2: f64,
}

pub const R0_PARAMS: LodParams = LodParams { dp_m: 0.5, min_area_m2: 0.0 };
pub const R1_PARAMS: LodParams = LodParams { dp_m: 2.0, min_area_m2: 20.0 };
/// R2 (z14, 2.5–7 km): merged blocks seen from far away; small sheds vanish.
pub const R2_PARAMS: LodParams = LodParams { dp_m: 4.0, min_area_m2: 50.0 };

// ---------------------------------------------------------------------------------------------
// Attributes: profile + heights

pub const PROFILE_RESIDENTIAL: u8 = 0;
pub const PROFILE_OFFICE: u8 = 1;
pub const PROFILE_RETAIL: u8 = 2;
pub const PROFILE_INDUSTRIAL: u8 = 3;
pub const PROFILE_CIVIC: u8 = 4;
pub const PROFILE_UNKNOWN: u8 = 5;

pub const FLOOR_H_DEFAULT: f32 = 3.0;
pub const MAX_HEIGHT_M: f32 = 400.0;

/// Profile from the OSM `building` value (`None` = unknown, resolve via landuse).
pub fn profile_for_kind(kind: &str) -> Option<u8> {
    Some(match kind {
        "residential" | "apartments" | "house" | "detached" | "semidetached_house" | "terrace"
        | "dormitory" | "bungalow" | "cabin" | "farm" | "hut" | "static_caravan" | "houseboat"
        | "flats" | "hotel" => PROFILE_RESIDENTIAL,
        "office" | "commercial" | "bank" => PROFILE_OFFICE,
        "retail" | "supermarket" | "kiosk" | "shop" | "mall" | "store" | "department_store"
        | "restaurant" => PROFILE_RETAIL,
        "industrial" | "warehouse" | "factory" | "manufacture" | "hangar" | "storage_tank"
        | "depot" | "garage" | "garages" | "shed" | "barn" | "farm_auxiliary" | "service"
        | "transformer_tower" | "boathouse" | "greenhouse" | "parking" => PROFILE_INDUSTRIAL,
        "church" | "cathedral" | "chapel" | "mosque" | "synagogue" | "temple" | "religious"
        | "monastery" | "shrine" | "government" | "university" | "college" | "school"
        | "kindergarten" | "hospital" | "public" | "civic" | "townhall" | "museum"
        | "train_station" | "transportation" | "stadium" | "sports_hall" | "fire_station"
        | "library" | "theatre" | "courthouse" | "embassy" => PROFILE_CIVIC,
        _ => return None,
    })
}

pub fn landuse_profile(l: Option<Landuse>) -> u8 {
    match l {
        Some(Landuse::Commercial) => PROFILE_OFFICE,
        Some(Landuse::Retail) => PROFILE_RETAIL,
        Some(Landuse::Industrial) => PROFILE_INDUSTRIAL,
        Some(Landuse::Civic) => PROFILE_CIVIC,
        // Unknown with no (or an unmapped) landuse -> residential, per the design doc table.
        Some(Landuse::Residential) | Some(Landuse::Other) | None => PROFILE_RESIDENTIAL,
    }
}

/// Fallback level count when neither `height` nor `levels` is tagged.
pub fn fallback_levels(kind: &str, l: Option<Landuse>) -> f32 {
    // Small single-purpose kinds never get landuse-sized heights (a garage in a residential
    // area is not a 5-storey block).
    match kind {
        "garage" | "garages" | "shed" | "kiosk" | "carport" | "roof" | "toilets" | "hut"
        | "service" | "transformer_tower" | "greenhouse" | "cabin" => return 1.0,
        "house" | "detached" | "semidetached_house" | "bungalow" | "farm" => return 2.0,
        _ => {}
    }
    match l {
        Some(Landuse::Residential) => 5.0,
        Some(Landuse::Commercial) => 4.0,
        Some(Landuse::Industrial) => 1.0,
        _ => 2.0,
    }
}

/// Resolved per-building attributes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Resolved {
    /// 0 residential .. 4 civic (never 5 after resolution).
    pub profile: u8,
    /// Roof height above ground (m).
    pub height: f32,
    /// Bottom of the extrusion (m).
    pub min_height: f32,
    /// Floor-to-floor height (m).
    pub floor_h: f32,
    pub seed: f32,
}

pub fn resolve(b: &Building) -> Resolved {
    let profile = profile_for_kind(&b.kind).unwrap_or_else(|| landuse_profile(b.landuse));
    let min_h = if b.min_height.is_finite() { b.min_height.clamp(0.0, MAX_HEIGHT_M) } else { 0.0 };
    let h = b.height.filter(|h| h.is_finite() && *h > 0.0);
    let l = b.levels.filter(|l| l.is_finite() && *l > 0.0);
    let (mut height, floor_h) = match (h, l) {
        (Some(h), Some(l)) => (h, (h / l).clamp(2.5, 5.0)),
        (Some(h), None) => (h, FLOOR_H_DEFAULT),
        (None, Some(l)) => (l * FLOOR_H_DEFAULT, FLOOR_H_DEFAULT),
        (None, None) => (fallback_levels(&b.kind, b.landuse) * FLOOR_H_DEFAULT, FLOOR_H_DEFAULT),
    };
    if h.is_none() && height <= min_h {
        // building:levels on a raised part counted from its bottom.
        height += min_h;
    }
    Resolved { profile, height: height.min(MAX_HEIGHT_M), min_height: min_h, floor_h, seed: b.seed01() }
}

/// Vertex COLOR per contract: r = profile / 8, g = seed, b = floor height / 10 m,
/// a = building height / 400 m.
pub fn vertex_color(r: &Resolved) -> [f32; 4] {
    [
        r.profile as f32 / 8.0,
        r.seed,
        (r.floor_h / 10.0).clamp(0.0, 1.0),
        (r.height / MAX_HEIGHT_M).clamp(0.0, 1.0),
    ]
}

// ---------------------------------------------------------------------------------------------
// Geometry helpers (tile pixel space, y down)

/// Axis-aligned clip rectangle in tile pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipRect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl ClipRect {
    pub fn tile(extent: f64) -> Self {
        Self { x0: 0.0, y0: 0.0, x1: extent, y1: extent }
    }
    /// Child quadrant in contract order: 0 NW, 1 NE, 2 SW, 3 SE (= child keys
    /// `2x/2y`, `2x+1/2y`, `2x/2y+1`, `2x+1/2y+1`).
    pub fn quadrant(extent: f64, q: usize) -> Self {
        let h = extent / 2.0;
        let (cx, cy) = ((q & 1) as f64, (q >> 1) as f64);
        Self { x0: cx * h, y0: cy * h, x1: (cx + 1.0) * h, y1: (cy + 1.0) * h }
    }
    pub(crate) fn flags(&self, p: [f64; 2]) -> u8 {
        (p[0] == self.x0) as u8 | ((p[0] == self.x1) as u8) << 1 | ((p[1] == self.y0) as u8) << 2 | ((p[1] == self.y1) as u8) << 3
    }
}

/// Which child quadrant (0 NW, 1 NE, 2 SW, 3 SE) contains a pixel position.
pub fn quadrant_of(extent: f64, p: [f64; 2]) -> usize {
    let h = extent / 2.0;
    (p[0] >= h) as usize | ((p[1] >= h) as usize) << 1
}

fn bbox(polys: &[Vec<Vec<[f64; 2]>>]) -> [f64; 4] {
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for poly in polys {
        if let Some(ext) = poly.first() {
            for p in ext {
                b[0] = b[0].min(p[0]);
                b[1] = b[1].min(p[1]);
                b[2] = b[2].max(p[0]);
                b[3] = b[3].max(p[1]);
            }
        }
    }
    b
}

/// Sutherland–Hodgman clip of a closed ring against an axis-aligned rectangle. Intersection
/// points get the clip coordinate exactly, so seams of neighbouring tiles match bit for bit.
pub(crate) fn clip_ring(ring: &[[f64; 2]], r: &ClipRect, out: &mut Vec<[f64; 2]>, tmp: &mut Vec<[f64; 2]>) {
    out.clear();
    out.extend_from_slice(ring);
    // (axis, value, keep_greater_or_equal)
    for (axis, v, ge) in [(0usize, r.x0, true), (0, r.x1, false), (1, r.y0, true), (1, r.y1, false)] {
        if out.is_empty() {
            return;
        }
        std::mem::swap(out, tmp);
        out.clear();
        let inside = |p: &[f64; 2]| if ge { p[axis] >= v } else { p[axis] <= v };
        let n = tmp.len();
        for i in 0..n {
            let cur = tmp[i];
            let prev = tmp[(i + n - 1) % n];
            let (ci, pi) = (inside(&cur), inside(&prev));
            if ci != pi {
                let t = (v - prev[axis]) / (cur[axis] - prev[axis]);
                let o = 1 - axis;
                let mut x = [0.0; 2];
                x[axis] = v;
                x[o] = prev[o] + t * (cur[o] - prev[o]);
                out.push(x);
            }
            if ci {
                out.push(cur);
            }
        }
    }
}

fn seg_dist2(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    let (ex, ey) = (a[0] + t * dx - p[0], a[1] + t * dy - p[1]);
    ex * ex + ey * ey
}

/// Douglas–Peucker on a closed ring; `locked` vertices are always kept. Returns kept indices
/// in ring order. Deterministic (anchors chosen by index / first-max).
pub fn simplify_ring(pts: &[[f64; 2]], locked: &[bool], tol: f64, keep: &mut Vec<bool>, out: &mut Vec<usize>) {
    let n = pts.len();
    out.clear();
    if n <= 3 || tol <= 0.0 {
        out.extend(0..n);
        return;
    }
    keep.clear();
    keep.resize(n, false);
    let mut anchors: Vec<usize> = (0..n).filter(|&i| locked[i]).collect();
    if anchors.len() < 2 {
        let a = anchors.first().copied().unwrap_or(0);
        let mut far = (a + 1) % n;
        let mut best = -1.0;
        for i in 0..n {
            let d = (pts[i][0] - pts[a][0]).powi(2) + (pts[i][1] - pts[a][1]).powi(2);
            if d > best {
                best = d;
                far = i;
            }
        }
        anchors = vec![a.min(far), a.max(far)];
        if anchors[0] == anchors[1] {
            out.extend(0..n);
            return;
        }
    }
    for &a in &anchors {
        keep[a] = true;
    }
    let tol2 = tol * tol;
    let mut stack: Vec<(usize, usize)> = Vec::with_capacity(16);
    for k in 0..anchors.len() {
        let s = anchors[k];
        let e = if k + 1 < anchors.len() { anchors[k + 1] } else { anchors[0] + n };
        stack.push((s, e));
        while let Some((s, e)) = stack.pop() {
            if e <= s + 1 {
                continue;
            }
            let (a, b) = (pts[s % n], pts[e % n]);
            let mut best = (0usize, -1.0f64);
            for i in s + 1..e {
                let d = seg_dist2(pts[i % n], a, b);
                if d > best.1 {
                    best = (i, d);
                }
            }
            if best.1 > tol2 {
                keep[best.0 % n] = true;
                stack.push((best.0, e));
                stack.push((s, best.0));
            }
        }
    }
    out.extend((0..n).filter(|&i| keep[i]));
}

// ---------------------------------------------------------------------------------------------
// Mesher

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
    flat: Vec<f64>,
    holes: Vec<usize>,
}

/// Mesh every building fragment inside `rect` (tile pixels) into one merged buffer.
pub fn mesh_buildings(buildings: &[Building], proj: &TileProjector, rect: ClipRect, params: LodParams) -> MeshData {
    let mut m = MeshData::default();
    let mut s = Scratch::default();
    let m_per_px = proj.m_per_px();
    let tol_px = params.dp_m / m_per_px;
    let min_area_px2 = params.min_area_m2 / (m_per_px * m_per_px);
    for b in buildings {
        let bb = bbox(&b.polygons);
        if bb[2] <= rect.x0 || bb[0] >= rect.x1 || bb[3] <= rect.y0 || bb[1] >= rect.y1 {
            continue;
        }
        let res = resolve(b);
        if res.height < res.min_height + 0.5 {
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
        for poly in &b.polygons {
            if prepare_polygon(poly, proj, &rect, needs_clip, tol_px, &mut s) {
                emit_polygon(&mut m, &mut s, &res, color, b.base_m);
            }
        }
    }
    m.shrink();
    m
}

/// Clip + simplify + project + orient one polygon into `s.rings` / `s.ring_flags`.
/// Returns false if the exterior vanished.
fn prepare_polygon(
    poly: &[Vec<[f64; 2]>],
    proj: &TileProjector,
    rect: &ClipRect,
    needs_clip: bool,
    tol_px: f64,
    s: &mut Scratch,
) -> bool {
    s.rings.clear();
    s.ring_flags.clear();
    for (ri, ring) in poly.iter().enumerate() {
        if needs_clip {
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
        }
        s.rings.push(pts);
        s.ring_flags.push(flags);
    }
    !s.rings.is_empty()
}

#[inline]
fn gv(p: [f64; 2], y: f32) -> [f32; 3] {
    [p[0] as f32, y, -(p[1] as f32)]
}

fn emit_polygon(m: &mut MeshData, s: &mut Scratch, res: &Resolved, color: [f32; 4], base: f32) {
    // Heights stand on the terrain under the building (`Building::base_m`, 0 while flat).
    let y0 = base + res.min_height;
    let y1 = base + res.height;
    let wall_h = y1 - y0;
    // Walls.
    for (ring, flags) in s.rings.iter().zip(&s.ring_flags) {
        let n = ring.len();
        let mut u = 0.0f64;
        for i in 0..n {
            let (p, q) = (ring[i], ring[(i + 1) % n]);
            let (de, dn) = (q[0] - p[0], q[1] - p[1]);
            let len = de.hypot(dn);
            let u0 = u;
            u += len;
            if len < 1e-4 || flags[i] & flags[(i + 1) % n] != 0 {
                continue; // degenerate, or interior seam on a clip line
            }
            // Outward normal of a CCW exterior / CW hole edge: (dn, -de) in EN -> Godot (x, z).
            let nrm = [(dn / len) as f32, 0.0, (de / len) as f32];
            let base = m.positions.len() as i32;
            m.positions.extend_from_slice(&[gv(p, y0), gv(q, y0), gv(q, y1), gv(p, y1)]);
            m.normals.extend_from_slice(&[nrm; 4]);
            m.colors.extend_from_slice(&[color; 4]);
            let (u0, u1) = (u0 as f32, u as f32);
            m.uvs.extend_from_slice(&[[u0, 0.0], [u1, 0.0], [u1, wall_h], [u0, wall_h]]);
            // pb, pt, qt / pb, qt, qb: clockwise seen from outside.
            m.indices.extend_from_slice(&[base, base + 3, base + 2, base, base + 2, base + 1]);
        }
    }
    // Roof (and bottom for raised parts) via earcut with holes.
    s.flat.clear();
    s.holes.clear();
    for (i, ring) in s.rings.iter().enumerate() {
        if i > 0 {
            s.holes.push(s.flat.len() / 2);
        }
        for p in ring {
            s.flat.push(p[0]);
            s.flat.push(p[1]);
        }
    }
    let Ok(tris) = earcutr::earcut(&s.flat, &s.holes, 2) else { return };
    if tris.is_empty() {
        return;
    }
    let caps: &[(f32, f32)] = if res.min_height > 0.25 { &[(1.0, y1), (-1.0, y0)] } else { &[(1.0, y1)] };
    for &(dir, y) in caps {
        let base = m.positions.len() as i32;
        for k in 0..s.flat.len() / 2 {
            let p = [s.flat[2 * k], s.flat[2 * k + 1]];
            let v = gv(p, y);
            m.positions.push(v);
            m.normals.push([0.0, dir, 0.0]);
            m.colors.push(color);
            m.uvs.push([v[0], v[2]]);
        }
        for t in tris.chunks_exact(3) {
            let (a, b, c) = (t[0], t[1], t[2]);
            // ((c - a) x (b - a)).y == cross_EN(c - a, b - a), so a front face toward +y needs
            // (a, b, c) clockwise in the EN plane; toward -y counter-clockwise.
            let pa = [s.flat[2 * a], s.flat[2 * a + 1]];
            let pb = [s.flat[2 * b], s.flat[2 * b + 1]];
            let pc = [s.flat[2 * c], s.flat[2 * c + 1]];
            let cr = (pb[0] - pa[0]) * (pc[1] - pa[1]) - (pb[1] - pa[1]) * (pc[0] - pa[0]);
            if cr.abs() < 2e-3 {
                continue; // < 0.001 m2 sliver (collinear clip-line vertices); f32 can't orient it
            }
            let ccw = cr > 0.0;
            let (b, c) = if ccw != (dir > 0.0) { (b, c) } else { (c, b) };
            m.indices.extend_from_slice(&[base + a as i32, base + b as i32, base + c as i32]);
        }
    }
}

/// Meshing parameters per ring; `None` for R3 (light points only).
pub fn lod_params(ring: usize) -> Option<LodParams> {
    match ring {
        0 => Some(R0_PARAMS),
        1 => Some(R1_PARAMS),
        2 => Some(R2_PARAMS),
        _ => None,
    }
}

/// Ring-dependent meshing: one merged mesh per tile, clipped exactly to the tile rectangle
/// (R0 full detail, R1 boxes, R2 coarse merged blocks); R3 has no mesh.
pub fn mesh_tile(buildings: &[Building], proj: &TileProjector, ring: usize) -> MeshData {
    match lod_params(ring) {
        Some(p) => mesh_buildings(buildings, proj, ClipRect::tile(proj.extent()), p),
        None => MeshData::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::{tile_for, Frame};
    use crate::mvt::OsmType;

    const VILNIUS: (f64, f64) = (54.6872, 25.2797);

    fn bld(kind: &str, height: Option<f32>, levels: Option<f32>, polys: Vec<Vec<Vec<[f64; 2]>>>) -> Building {
        Building {
            osm_type: OsmType::Way,
            id: 4242,
            kind: kind.into(),
            height,
            levels,
            min_height: 0.0,
            landuse: None,
            polygons: polys,
            centroid_px: [0.0; 2],
            centroid: [0.0; 2],
            base_m: 0.0,
            owned: true,
        }
    }

    /// Square ring in pixels, clockwise in screen space (y down) like MVT exteriors.
    fn sq(x0: f64, y0: f64, s: f64) -> Vec<[f64; 2]> {
        vec![[x0, y0], [x0 + s, y0], [x0 + s, y0 + s], [x0, y0 + s]]
    }

    fn setup() -> (TileProjector, f64) {
        let key = tile_for(VILNIUS.0, VILNIUS.1, 16);
        let (lat, lon) = key.center_lat_lon();
        let p = TileProjector::new(key, Frame::new(lat, lon), 4096.0);
        let m = p.m_per_px();
        (p, m)
    }

    fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
    }
    fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }
    fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }

    /// Every triangle is clockwise seen from the side its normal points to (Godot front face),
    /// and its geometric normal agrees with the vertex normal.
    fn assert_winding(m: &MeshData) {
        for t in m.indices.chunks_exact(3) {
            let [a, b, c] = [t[0] as usize, t[1] as usize, t[2] as usize];
            let g = cross(sub(m.positions[c], m.positions[a]), sub(m.positions[b], m.positions[a]));
            let l = dot(g, g).sqrt();
            assert!(l > 0.0);
            assert!(dot(g, m.normals[a]) / l > 0.99, "triangle {t:?} faces away from its normal");
        }
    }

    fn mesh_centroid(m: &MeshData) -> [f32; 3] {
        let n = m.positions.len() as f32;
        let mut c = [0.0; 3];
        for p in &m.positions {
            for k in 0..3 {
                c[k] += p[k] / n;
            }
        }
        c
    }

    #[test]
    fn box_triangle_count_normals_outward_and_uvs() {
        let (p, mpp) = setup();
        let b = bld("office", Some(30.0), None, vec![vec![sq(1000.0, 1000.0, 200.0)]]);
        let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), R0_PARAMS);
        // 4 walls x 2 + roof 2 = 10 triangles; 4 x 4 + 4 vertices.
        assert_eq!(m.triangle_count(), 10);
        assert_eq!(m.vertex_count(), 20);
        assert_winding(&m);
        // Wall normals point away from the footprint center.
        let c = mesh_centroid(&m);
        for (i, n) in m.normals.iter().enumerate() {
            if n[1] == 0.0 {
                let d = sub(m.positions[i], c);
                assert!(d[0] * n[0] + d[2] * n[2] > 0.0, "wall normal points inward");
                assert!((dot(*n, *n) - 1.0).abs() < 1e-5);
            } else {
                assert_eq!(*n, [0.0, 1.0, 0.0]);
            }
        }
        // UV: perimeter meters and meters up.
        let side = 200.0 * mpp;
        let max_u = m.uvs[..16].iter().map(|uv| uv[0]).fold(0.0f32, f32::max);
        let max_v = m.uvs[..16].iter().map(|uv| uv[1]).fold(0.0f32, f32::max);
        assert!((max_v - 30.0).abs() < 1e-4);
        // Mercator scale varies across the tile; m_per_px is the center value.
        assert!((max_u as f64 - 4.0 * side).abs() / (4.0 * side) < 0.005, "{max_u} vs {}", 4.0 * side);
        // Roof at 30 m with x/z meter UVs.
        for i in 16..20 {
            assert_eq!(m.positions[i][1], 30.0);
            assert_eq!(m.uvs[i], [m.positions[i][0], m.positions[i][2]]);
        }
        // Vertex color encoding.
        let col = m.colors[0];
        assert_eq!(col[0], 1.0 / 8.0);
        assert!((col[2] - 0.3).abs() < 1e-6);
        assert!((col[3] - 30.0 / 400.0).abs() < 1e-6);
    }

    #[test]
    fn both_windings_give_outward_normals() {
        let (p, _) = setup();
        let mut ring = sq(1000.0, 1000.0, 200.0);
        ring.reverse();
        let m = mesh_buildings(&[bld("yes", Some(10.0), None, vec![vec![ring]])], &p, ClipRect::tile(4096.0), R0_PARAMS);
        assert_eq!(m.triangle_count(), 10);
        assert_winding(&m);
        let c = mesh_centroid(&m);
        for (i, n) in m.normals.iter().enumerate().filter(|(_, n)| n[1] == 0.0) {
            let d = sub(m.positions[i], c);
            assert!(d[0] * n[0] + d[2] * n[2] > 0.0);
        }
    }

    #[test]
    fn earcut_with_hole_and_courtyard_walls_face_inward_to_the_yard() {
        let (p, _) = setup();
        let outer = sq(1000.0, 1000.0, 300.0);
        let mut hole = sq(1100.0, 1100.0, 100.0);
        hole.reverse();
        let m = mesh_buildings(&[bld("apartments", None, Some(5.0), vec![vec![outer, hole]])], &p, ClipRect::tile(4096.0), R0_PARAMS);
        // Walls: 8 edges x 2; roof of a square with a square hole: 8 vertices -> 8 triangles.
        assert_eq!(m.triangle_count(), 16 + 8);
        assert_winding(&m);
        // Roof area = outer - hole.
        let mut area = 0.0f32;
        for t in m.indices.chunks_exact(3) {
            let [a, b, c] = [t[0] as usize, t[1] as usize, t[2] as usize];
            if m.normals[a][1] > 0.5 {
                let g = cross(sub(m.positions[c], m.positions[a]), sub(m.positions[b], m.positions[a]));
                area += dot(g, g).sqrt() / 2.0;
            }
        }
        let (_, mpp) = setup();
        let expect = ((300.0f64 * 300.0 - 100.0 * 100.0) * mpp * mpp) as f32;
        assert!((area - expect).abs() / expect < 0.005, "{area} vs {expect}");
        // Courtyard walls: normals point toward the yard center (away from building material).
        let yard = p.project([1150.0, 1150.0]);
        let yard = [yard[0], 0.0, -yard[1]];
        let mut yard_walls = 0;
        for (i, n) in m.normals.iter().enumerate().filter(|(_, n)| n[1] == 0.0) {
            let d = sub(m.positions[i], yard);
            if d[0].abs() < 8.0 && d[2].abs() < 8.0 {
                assert!(d[0] * n[0] + d[2] * n[2] < 0.0, "yard wall faces into the building");
                yard_walls += 1;
            }
        }
        assert_eq!(yard_walls, 16);
    }

    #[test]
    fn multipolygon_parts_and_raised_part_gets_bottom() {
        let (p, _) = setup();
        let mut b = bld("yes", Some(20.0), None, vec![vec![sq(100.0, 100.0, 100.0)], vec![sq(400.0, 100.0, 100.0)]]);
        b.min_height = 8.0;
        let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), R0_PARAMS);
        // Two boxes, each: 8 wall + 2 roof + 2 bottom.
        assert_eq!(m.triangle_count(), 24);
        assert_winding(&m);
        assert!(m.positions.iter().all(|v| v[1] >= 8.0));
        assert!(m.normals.iter().any(|n| n[1] < -0.5));
        // Wall v starts at 0 at min_height.
        assert!(m.uvs.iter().all(|uv| uv[1] <= 12.0 + 1e-4));
    }

    #[test]
    fn douglas_peucker_tolerances_and_small_drop() {
        let (p, mpp) = setup();
        // A square with a 1 m bump in the middle of one side (in pixels).
        let bump = 1.0 / mpp;
        let ring = vec![[1000.0, 1000.0], [1100.0, 1000.0 - bump], [1200.0, 1000.0], [1200.0, 1200.0], [1000.0, 1200.0]];
        let b = bld("yes", Some(10.0), None, vec![vec![ring]]);
        let r0 = mesh_buildings(std::slice::from_ref(&b), &p, ClipRect::tile(4096.0), R0_PARAMS);
        let r1 = mesh_buildings(std::slice::from_ref(&b), &p, ClipRect::tile(4096.0), R1_PARAMS);
        assert_eq!(r0.triangle_count(), 5 * 2 + 3, "0.5 m keeps the 1 m bump");
        assert_eq!(r1.triangle_count(), 4 * 2 + 2, "2 m removes it");
        // < 20 m2 dropped at R1 only.
        let side = (16.0f64).sqrt() / mpp;
        let tiny = bld("yes", Some(10.0), None, vec![vec![sq(2000.0, 2000.0, side)]]);
        assert_eq!(mesh_buildings(std::slice::from_ref(&tiny), &p, ClipRect::tile(4096.0), R1_PARAMS).triangle_count(), 0);
        assert_eq!(mesh_buildings(&[tiny], &p, ClipRect::tile(4096.0), R0_PARAMS).triangle_count(), 10);
    }

    #[test]
    fn clipping_partitions_buildings_across_quadrants_and_skips_seams() {
        let (p, _) = setup();
        // Straddles the vertical child seam at x = 2048.
        let b = bld("yes", Some(10.0), None, vec![vec![sq(1948.0, 1000.0, 200.0)]]);
        let whole = mesh_buildings(std::slice::from_ref(&b), &p, ClipRect::tile(4096.0), R0_PARAMS);
        let nw = mesh_buildings(std::slice::from_ref(&b), &p, ClipRect::quadrant(4096.0, 0), R0_PARAMS);
        let ne = mesh_buildings(std::slice::from_ref(&b), &p, ClipRect::quadrant(4096.0, 1), R0_PARAMS);
        let sw = mesh_buildings(std::slice::from_ref(&b), &p, ClipRect::quadrant(4096.0, 2), R0_PARAMS);
        assert!(sw.is_empty());
        // Each half: 3 outer walls (seam wall skipped) + 2 roof triangles.
        assert_eq!(nw.triangle_count(), 8);
        assert_eq!(ne.triangle_count(), 8);
        assert_winding(&nw);
        assert_winding(&ne);
        let roof_area = |m: &MeshData| -> f32 {
            m.indices
                .chunks_exact(3)
                .filter(|t| m.normals[t[0] as usize][1] > 0.5)
                .map(|t| {
                    let [a, b, c] = [t[0] as usize, t[1] as usize, t[2] as usize];
                    let g = cross(sub(m.positions[c], m.positions[a]), sub(m.positions[b], m.positions[a]));
                    dot(g, g).sqrt() / 2.0
                })
                .sum()
        };
        let total = roof_area(&whole);
        assert!((roof_area(&nw) + roof_area(&ne) - total).abs() / total < 1e-4);
    }

    #[test]
    fn heights_and_profiles_fallbacks() {
        let r = |kind: &str, h, l, lu| {
            let mut b = bld(kind, h, l, vec![]);
            b.landuse = lu;
            resolve(&b)
        };
        // height wins; floor = height / levels clamped.
        let a = r("office", Some(40.0), Some(10.0), None);
        assert_eq!((a.profile, a.height, a.floor_h), (PROFILE_OFFICE, 40.0, 4.0));
        assert_eq!(r("yes", Some(40.0), Some(2.0), None).floor_h, 5.0);
        assert_eq!(r("yes", Some(10.0), Some(10.0), None).floor_h, 2.5);
        assert_eq!(r("yes", Some(12.0), None, None).floor_h, 3.0);
        // levels x 3.
        let b = r("apartments", None, Some(9.0), None);
        assert_eq!((b.profile, b.height, b.floor_h), (PROFILE_RESIDENTIAL, 27.0, 3.0));
        // landuse fallback levels.
        assert_eq!(r("yes", None, None, Some(Landuse::Residential)).height, 15.0);
        assert_eq!(r("yes", None, None, Some(Landuse::Commercial)).height, 12.0);
        assert_eq!(r("yes", None, None, Some(Landuse::Industrial)).height, 3.0);
        assert_eq!(r("yes", None, None, Some(Landuse::Other)).height, 6.0);
        assert_eq!(r("yes", None, None, None).height, 6.0);
        assert_eq!(r("garage", None, None, Some(Landuse::Residential)).height, 3.0);
        // Profiles.
        assert_eq!(r("house", None, None, Some(Landuse::Industrial)).profile, PROFILE_RESIDENTIAL);
        assert_eq!(r("supermarket", None, None, None).profile, PROFILE_RETAIL);
        assert_eq!(r("warehouse", None, None, None).profile, PROFILE_INDUSTRIAL);
        assert_eq!(r("cathedral", None, None, None).profile, PROFILE_CIVIC);
        assert_eq!(r("yes", None, None, Some(Landuse::Industrial)).profile, PROFILE_INDUSTRIAL);
        assert_eq!(r("yes", None, None, Some(Landuse::Commercial)).profile, PROFILE_OFFICE);
        assert_eq!(r("yes", None, None, Some(Landuse::Retail)).profile, PROFILE_RETAIL);
        assert_eq!(r("yes", None, None, Some(Landuse::Civic)).profile, PROFILE_CIVIC);
        assert_eq!(r("yes", None, None, Some(Landuse::Other)).profile, PROFILE_RESIDENTIAL);
        assert_eq!(r("yes", None, None, None).profile, PROFILE_RESIDENTIAL);
        assert_eq!(r("weird_value", None, None, None).profile, PROFILE_RESIDENTIAL);
        // Raised part with levels only.
        let mut raised = bld("yes", None, Some(2.0), vec![]);
        raised.min_height = 9.0;
        assert_eq!(resolve(&raised).height, 15.0);
    }

    #[test]
    fn synthetic_children_match_parent_quarters_and_meshing_is_byte_identical() {
        let frame = Frame::new(VILNIUS.0, VILNIUS.1);
        let k15 = tile_for(VILNIUS.0, VILNIUS.1, 15);
        let p15 = TileProjector::new(k15, frame, 4096.0);
        let b15 = crate::synthetic::generate(k15, frame);
        let c15 = frame.tile_center(k15);
        let whole = mesh_tile(&b15, &p15, 1);
        let mut sum = 0;
        for (i, child) in crate::rings::children(k15).into_iter().enumerate() {
            // The parent's quarter i, clipped exactly, equals the child tile meshed alone.
            let q = mesh_buildings(&b15, &p15, ClipRect::quadrant(4096.0, i), R1_PARAMS);
            let p16 = TileProjector::new(child, frame, 4096.0);
            let b16 = crate::synthetic::generate(child, frame);
            let t16 = mesh_tile(&b16, &p16, 0);
            // Same buildings, same triangles (synthetic footprints are rectangles: DP 0.5 and
            // 2 m agree, nothing < 20 m2).
            assert_eq!(q.triangle_count(), t16.triangle_count(), "quarter {i} vs {child}");
            assert!(!q.is_empty());
            sum += q.triangle_count();
            // ... at the same world positions (tile-root offsets differ).
            let c16 = frame.tile_center(child);
            let (dx, dz) = ((c16[0] - c15[0]) as f32, -(c16[1] - c15[1]) as f32);
            let a = mesh_centroid(&q);
            let b = mesh_centroid(&t16);
            assert!((a[0] - (b[0] + dx)).abs() < 0.05 && (a[2] - (b[2] + dz)).abs() < 0.05);
            // Centroid ownership: every z16-owned building lies in quarter i of the parent.
            for bb in b15.iter().filter(|bb| bb.owned) {
                let qi = quadrant_of(4096.0, bb.centroid_px);
                let in_child = b16.iter().any(|x| x.id == bb.id && x.owned);
                assert_eq!(qi == i, in_child, "building {} quadrant {qi}", bb.id);
            }
        }
        assert_eq!(sum, whole.triangle_count());
        // Determinism: byte-identical buffers.
        let bits = |m: &MeshData| -> Vec<u32> {
            m.positions
                .iter()
                .flatten()
                .chain(m.normals.iter().flatten())
                .chain(m.colors.iter().flatten())
                .chain(m.uvs.iter().flatten())
                .map(|f| f.to_bits())
                .chain(m.indices.iter().map(|&i| i as u32))
                .collect()
        };
        let again = mesh_tile(&crate::synthetic::generate(k15, frame), &p15, 1);
        assert_eq!(bits(&whole), bits(&again));
        // R2 meshes (coarser, still non-empty); R3 has no mesh.
        let k14 = tile_for(VILNIUS.0, VILNIUS.1, 14);
        let p14 = TileProjector::new(k14, frame, 4096.0);
        let b14 = crate::synthetic::generate(k14, frame);
        let r2 = mesh_tile(&b14, &p14, 2);
        assert!(!r2.is_empty());
        assert_winding(&r2);
        assert_eq!(bits(&r2), bits(&mesh_tile(&b14, &p14, 2)));
        assert!(mesh_tile(&b14, &p14, 3).is_empty());
    }

    #[test]
    fn r2_drops_small_footprints_and_simplifies_harder() {
        let (p, m) = setup();
        // 6 x 6 m shed (36 m2): kept at R1, dropped at R2.
        let side = 6.0 / m;
        let shed = bld("yes", Some(4.0), None, vec![vec![sq(100.0, 100.0, side)]]);
        assert_eq!(mesh_tile(std::slice::from_ref(&shed), &p, 1).triangle_count(), 10);
        assert!(mesh_tile(std::slice::from_ref(&shed), &p, 2).is_empty());
        // A 3 m bump on a 40 m wall: kept at R1 (DP 2 m), removed at R2 (DP 4 m).
        let s = 40.0 / m;
        let bump = 3.0 / m;
        let ring = vec![[0.0, 0.0], [s * 0.5, -bump], [s, 0.0], [s, s], [0.0, s]];
        let b = bld("yes", Some(10.0), None, vec![vec![ring.into_iter().map(|q| [q[0] + 500.0, q[1] + 500.0]).collect()]]);
        let r1 = mesh_tile(std::slice::from_ref(&b), &p, 1).triangle_count();
        let r2 = mesh_tile(std::slice::from_ref(&b), &p, 2).triangle_count();
        assert_eq!(r1, 13); // 5 walls + 3 roof triangles
        assert_eq!(r2, 10); // box
    }

    #[test]
    fn simplify_keeps_locked_vertices() {
        let pts = vec![[0.0, 0.0], [5.0, 0.01], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        let (mut keep, mut out) = (Vec::new(), Vec::new());
        simplify_ring(&pts, &[false; 5], 1.0, &mut keep, &mut out);
        assert_eq!(out, vec![0, 2, 3, 4]);
        simplify_ring(&pts, &[false, true, false, false, false], 1.0, &mut keep, &mut out);
        assert!(out.contains(&1));
    }
}
