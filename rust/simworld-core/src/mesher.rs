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

use crate::mvt::{ring_area2, Building, Landuse, TileProjector};
use crate::profile::Profile;

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
    /// Append `other` (same origin; indices offset). Both must have UVs or neither (building
    /// meshes always have them).
    pub fn append(&mut self, other: MeshData) {
        if other.positions.is_empty() {
            return;
        }
        if self.positions.is_empty() {
            *self = other;
            return;
        }
        let base = self.positions.len() as i32;
        self.positions.extend(other.positions);
        self.normals.extend(other.normals);
        self.colors.extend(other.colors);
        self.uvs.extend(other.uvs);
        self.indices.extend(other.indices.into_iter().map(|i| i + base));
    }
    fn shrink(&mut self) {
        self.positions.shrink_to_fit();
        self.normals.shrink_to_fit();
        self.colors.shrink_to_fit();
        self.uvs.shrink_to_fit();
        self.indices.shrink_to_fit();
    }
}

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

// ---------------------------------------------------------------------------------------------
// Attributes: profile + heights

pub const PROFILE_RESIDENTIAL: u8 = 0;
pub const PROFILE_OFFICE: u8 = 1;
pub const PROFILE_RETAIL: u8 = 2;
pub const PROFILE_INDUSTRIAL: u8 = 3;
pub const PROFILE_CIVIC: u8 = 4;
pub const PROFILE_UNKNOWN: u8 = 5;
/// Castles, towers, churches: no windows, floodlit facade (docs/spec-night-windows.md).
pub const PROFILE_LANDMARK: u8 = 6;
/// Ruins, garages, chimneys: no windows, no light point.
pub const PROFILE_DARK: u8 = 7;
/// Number of profile ids (0..=7; COLOR `r` = id / 8).
pub const PROFILE_COUNT: usize = 8;

pub const FLOOR_H_DEFAULT: f32 = 3.0;
pub const MAX_HEIGHT_M: f32 = 400.0;

// Kind -> profile and the landuse fallback come from the world profile (`lights.profiles.*.kinds`,
// `lights.landuse_fallback`; `Profile::profile_for_kind` / `Profile::landuse_profile`).

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
    /// Light profile id: 0 residential .. 4 civic, 6 landmark, 7 dark (never 5).
    pub profile: u8,
    /// The building's walls may carry windows: its profile has a `windows` block and it is not
    /// tagged `windows=no` (a `windows=yes` building on a windowless profile switches to a
    /// windowed one, see [`resolve`]). Per-wall rules (wall height, R2) come on top.
    pub windows: bool,
    /// Roof height above ground (m).
    pub height: f32,
    /// Bottom of the extrusion (m).
    pub min_height: f32,
    /// Floor-to-floor height (m).
    pub floor_h: f32,
    pub seed: f32,
}

/// Heights, then the light profile (docs/spec-night-windows.md section 1), first match wins:
///
/// 1. `buildings.overrides` by OSM id;
/// 2. a state tag in `buildings.dark_states` (ruins, disused, abandoned, ...) -> dark;
/// 3. the kind table (`lights.profiles.*.kinds`) for `kind`, then, if `kind` is in no
///    profile (`yes`, ...), for `historic`, then `man_made`;
/// 4. generic kinds only, with a **tagged** `height` or `levels` and a footprint the encoder
///    did not clip: `buildings.slender_tower` -> landmark;
/// 5. the landuse fallback.
///
/// Then the `windows` tag: `no` keeps the profile without windows; `yes` on a windowless
/// profile (industrial, landmark, dark) switches to the landuse fallback profile if that one has
/// windows, else to residential if it has, else changes nothing. The switch also gives the
/// building that profile's light color (COLOR `r` is the one profile the shader sees).
pub fn resolve(b: &Building, p: &Profile) -> Resolved {
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
    let height = height.min(MAX_HEIGHT_M);
    let tagged_height = h.is_some() || l.is_some();
    let profile = resolve_profile(b, p, height, tagged_height);
    let (profile, windows) = match b.tags.windows {
        Some(false) => (profile, false),
        Some(true) if p.window_width_m(profile).is_none() => {
            let fallback = p.landuse_profile(b.landuse);
            if p.window_width_m(fallback).is_some() {
                (fallback, true)
            } else if p.window_width_m(PROFILE_RESIDENTIAL).is_some() {
                (PROFILE_RESIDENTIAL, true)
            } else {
                (profile, false)
            }
        }
        _ => (profile, p.window_width_m(profile).is_some()),
    };
    Resolved { profile, windows, height, min_height: min_h, floor_h, seed: b.seed01() }
}

/// Steps 1-5 of [`resolve`] (the light profile before the `windows` tag).
fn resolve_profile(b: &Building, p: &Profile, height: f32, tagged_height: bool) -> u8 {
    if let Some(id) = p.override_profile(b.osm_type, b.id) {
        return id;
    }
    if p.is_dark_state(&b.tags) {
        return PROFILE_DARK;
    }
    let by_table = p
        .profile_for_kind(&b.kind)
        .or_else(|| b.tags.historic.as_deref().and_then(|v| p.profile_for_kind(v)))
        .or_else(|| b.tags.man_made.as_deref().and_then(|v| p.profile_for_kind(v)));
    if let Some(id) = by_table {
        return id;
    }
    // Fallback heights are not evidence of a tower (a 5 x 5 m `yes` in residential landuse gets
    // 15 m), and a clipped footprint's area differs per tile and zoom.
    if tagged_height && !b.clipped && p.is_slender_tower(height, b.footprint_m2) {
        return PROFILE_LANDMARK;
    }
    p.landuse_profile(b.landuse)
}

/// Vertex COLOR per contract: r = profile id / 8 (0-4, 6, 7), g = seed, b = floor height / 10 m,
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

#[inline]
fn gv(p: [f64; 2], y: f32) -> [f32; 3] {
    [p[0] as f32, y, -(p[1] as f32)]
}

/// Per-building wall parameters of [`emit_polygon`].
#[derive(Clone, Copy, Debug)]
struct WallLayout {
    /// Window column width (m) when the building's walls may carry windows in this band
    /// (profile has `windows`, not tagged `windows=no`, band is not R2); `None` = windowless.
    window_w: Option<f64>,
    /// `buildings.min_window_wall_m`: lower wall parts are windowless.
    min_window_wall_m: f32,
    /// Foundation depth below the exact ground at each wall vertex (m; terrain on:
    /// `Terrain::foundation_m`), 0 = none.
    foundation_m: f32,
}

/// Terrain under the walls (api 1.7; `None` = flat world, walls from the base as before).
#[derive(Clone, Copy)]
struct GroundWalls<'a> {
    proj: &'a TileProjector,
    /// A wall edge longer than `step_m` is subdivided while the ground along it falls more than
    /// this below the straight line between its end heights (the band's drape margin, at least
    /// 0.1 m), so the wall bottom follows a valley instead of bridging it.
    tol_m: f64,
    step_m: f64,
}

impl GroundWalls<'_> {
    /// Edge `a`..`b` (ENU, tile pixels) as points with ground heights and their share of the
    /// edge, subdivided at midpoints where needed (both ends included).
    fn edge(&self, ea: [f64; 2], eb: [f64; 2], pa: [f64; 2], pb: [f64; 2], out: &mut Vec<([f64; 2], [f64; 2], f64, f64)>) {
        out.clear();
        let ha = self.proj.ground_at(pa);
        let hb = self.proj.ground_at(pb);
        out.push((ea, pa, ha, 0.0));
        self.split(ea, eb, pa, pb, ha, hb, 0.0, 1.0, 0, out);
    }

    #[allow(clippy::too_many_arguments)]
    fn split(&self, ea: [f64; 2], eb: [f64; 2], pa: [f64; 2], pb: [f64; 2], ha: f64, hb: f64, ta: f64, tb: f64, depth: u32, out: &mut Vec<([f64; 2], [f64; 2], f64, f64)>) {
        let long = crate::math::hypot(eb[0] - ea[0], eb[1] - ea[1]) > self.step_m;
        if depth < WALL_SPLIT_MAX_DEPTH && long {
            let dips = [0.25, 0.5, 0.75].into_iter().any(|t| {
                let px = [pa[0] + (pb[0] - pa[0]) * t, pa[1] + (pb[1] - pa[1]) * t];
                ha + (hb - ha) * t - self.proj.ground_at(px) > self.tol_m
            });
            if dips {
                let pm = [(pa[0] + pb[0]) * 0.5, (pa[1] + pb[1]) * 0.5];
                let em = [(ea[0] + eb[0]) * 0.5, (ea[1] + eb[1]) * 0.5];
                let hm = self.proj.ground_at(pm);
                let tm = (ta + tb) * 0.5;
                self.split(ea, em, pa, pm, ha, hm, ta, tm, depth + 1, out);
                self.split(em, eb, pm, pb, hm, hb, tm, tb, depth + 1, out);
                return;
            }
        }
        out.push((eb, pb, hb, tb));
    }
}

/// Safety cap of the wall subdivision depth.
const WALL_SPLIT_MAX_DEPTH: u32 = 8;

/// One wall run: consecutive wall edges of a ring between two corners (or clip seams).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Run {
    /// Index of the run's first edge in the ring (edge i = vertex i -> i + 1, mod n).
    pub first: usize,
    /// Number of edges (degenerate ones included; never a clip seam).
    pub edges: usize,
    /// Length (m).
    pub len: f64,
}

/// cos 35 deg and cos 20 deg: turn thresholds of the corner rule.
const COS_CORNER: f64 = 0.819_152_044_288_991_8;
const COS_CORNER_LONG: f64 = 0.939_692_620_785_908_4;

/// Corner rule (docs/spec-night-windows.md section 2): the outline turns at `v` by more than
/// 35 deg, or by more than 20 deg with both adjacent edges at least `2 * width` long.
pub(crate) fn is_corner(prev: [f64; 2], v: [f64; 2], next: [f64; 2], width: f64) -> bool {
    let a = [v[0] - prev[0], v[1] - prev[1]];
    let b = [next[0] - v[0], next[1] - v[1]];
    let (la, lb) = (crate::math::hypot(a[0], a[1]), crate::math::hypot(b[0], b[1]));
    if la < 1e-4 || lb < 1e-4 {
        return false;
    }
    let cos = (a[0] * b[0] + a[1] * b[1]) / (la * lb);
    cos < COS_CORNER || (cos < COS_CORNER_LONG && la >= 2.0 * width && lb >= 2.0 * width)
}

/// Splits a closed ring (ENU meters) into wall runs. `seam[i]` marks edge i as an interior
/// clip seam (no wall; it ends a run). Iteration starts at the first run boundary (a corner,
/// or a vertex next to a seam), so vertex 0 lying mid-wall does not split that wall; a ring
/// without boundaries (a circle) is one run starting at vertex 0.
pub(crate) fn ring_runs(ring: &[[f64; 2]], seam: &[bool], width: f64, out: &mut Vec<Run>) {
    out.clear();
    let n = ring.len();
    if n < 2 {
        return;
    }
    let boundary = |i: usize| {
        seam[i] || seam[(i + n - 1) % n] || is_corner(ring[(i + n - 1) % n], ring[i], ring[(i + 1) % n], width)
    };
    let start = (0..n).find(|&i| boundary(i)).unwrap_or(0);
    let edge_len = |i: usize| {
        let (p, q) = (ring[i], ring[(i + 1) % n]);
        crate::math::hypot(q[0] - p[0], q[1] - p[1])
    };
    let mut k = 0;
    while k < n {
        let i = (start + k) % n;
        if seam[i] {
            k += 1;
            continue;
        }
        let mut run = Run { first: i, edges: 0, len: 0.0 };
        loop {
            run.len += edge_len((start + k) % n);
            run.edges += 1;
            k += 1;
            if k >= n || boundary((start + k) % n) {
                break;
            }
        }
        out.push(run);
    }
}

/// Pushes one wall quad (p..q, y0..y1) with UVs `u0..u1`, `0..v_top`.
#[allow(clippy::too_many_arguments)]
fn push_wall(m: &mut MeshData, p: [f64; 2], q: [f64; 2], len: f64, y0: f32, y1: f32, color: [f32; 4], u0: f32, u1: f32, v_top: f32) {
    let (de, dn) = (q[0] - p[0], q[1] - p[1]);
    // Outward normal of a CCW exterior / CW hole edge: (dn, -de) in EN -> Godot (x, z).
    let nrm = [(dn / len) as f32, 0.0, (de / len) as f32];
    let base = m.positions.len() as i32;
    m.positions.extend_from_slice(&[gv(p, y0), gv(q, y0), gv(q, y1), gv(p, y1)]);
    m.normals.extend_from_slice(&[nrm; 4]);
    m.colors.extend_from_slice(&[color; 4]);
    m.uvs.extend_from_slice(&[[u0, 0.0], [u1, 0.0], [u1, v_top], [u0, v_top]]);
    // pb, pt, qt / pb, qt, qb: clockwise seen from outside.
    m.indices.extend_from_slice(&[base, base + 3, base + 2, base, base + 2, base + 1]);
}

/// One wall quad p..q over the terrain (api 1.7): bottom heights `bot` and top heights `top` at
/// (p, q), UV `u0..u1`, `v` per corner (`vb` at the bottoms, `vt` at the tops).
#[allow(clippy::too_many_arguments)]
fn push_wall_on(m: &mut MeshData, p: [f64; 2], q: [f64; 2], len: f64, bot: [f32; 2], top: [f32; 2], color: [f32; 4], u0: f32, u1: f32, vb: [f32; 2], vt: [f32; 2]) {
    let (de, dn) = (q[0] - p[0], q[1] - p[1]);
    let nrm = [(dn / len) as f32, 0.0, (de / len) as f32];
    let base = m.positions.len() as i32;
    m.positions.extend_from_slice(&[gv(p, bot[0]), gv(q, bot[1]), gv(q, top[1]), gv(p, top[0])]);
    m.normals.extend_from_slice(&[nrm; 4]);
    m.colors.extend_from_slice(&[color; 4]);
    m.uvs.extend_from_slice(&[[u0, vb[0]], [u1, vb[1]], [u1, vt[1]], [u0, vt[0]]]);
    m.indices.extend_from_slice(&[base, base + 3, base + 2, base, base + 2, base + 1]);
}

/// Walls of a flat world (or a raised part): one quad per edge from `y0` to `y1` (the pre-1.7
/// layout, bit for bit).
#[allow(clippy::too_many_arguments)]
fn emit_walls_flat(m: &mut MeshData, s: &mut Scratch, color: [f32; 4], y0: f32, y1: f32, wall_h: f32, floors: f32, window_w: Option<f64>) {
    let mut column_base = 0.0f64;
    for (ring, flags) in s.rings.iter().zip(&s.ring_flags) {
        let n = ring.len();
        let Some(w) = window_w else {
            for i in 0..n {
                let (p, q) = (ring[i], ring[(i + 1) % n]);
                let len = crate::math::hypot(q[0] - p[0], q[1] - p[1]);
                if len < 1e-4 || flags[i] & flags[(i + 1) % n] != 0 {
                    continue; // degenerate, or interior seam on a clip line
                }
                push_wall(m, p, q, len, y0, y1, color, -1.0, -1.0, wall_h);
            }
            continue;
        };
        s.seam.clear();
        s.seam.extend((0..n).map(|i| flags[i] & flags[(i + 1) % n] != 0));
        ring_runs(ring, &s.seam, w, &mut s.runs);
        for run in &s.runs {
            let count = (run.len / w).round();
            let cell = if count >= 1.0 { run.len / count } else { 0.0 };
            let mut along = 0.0f64;
            for e in 0..run.edges {
                let i = (run.first + e) % n;
                let (p, q) = (ring[i], ring[(i + 1) % n]);
                let len = crate::math::hypot(q[0] - p[0], q[1] - p[1]);
                let a0 = along;
                along += len;
                if len < 1e-4 {
                    continue; // degenerate
                }
                if count < 1.0 {
                    push_wall(m, p, q, len, y0, y1, color, -1.0, -1.0, wall_h);
                    continue;
                }
                let u0 = column_base + a0 / cell;
                // The run's last edge ends exactly on the integer column count.
                let u1 = if e + 1 == run.edges { column_base + count } else { column_base + along / cell };
                push_wall(m, p, q, len, y0, y1, color, u0 as f32, u1 as f32, floors);
            }
            if count >= 1.0 {
                column_base += count;
            }
        }
    }
}

/// Walls on the terrain (api 1.7, contract section 7): per wall edge (subdivided where the
/// ground falls away between its ends), a wall quad from the exact ground at each point up to
/// the roof `y1`, and a windowless foundation quad from there `foundation_m` further down.
/// UV: windowed walls keep `v = floors` at the roof and the same floor height
/// (`wall_h / floors`) all the way down, so `v` at a bottom is `floors - (y1 - y) / floor_m`
/// (negative below the nominal bottom `y0`: more window rows downhill); windowless walls and
/// foundations carry `u = -1` and `v = y - y0` (meters, negative below `y0`).
#[allow(clippy::too_many_arguments)]
fn emit_walls_on(
    m: &mut MeshData,
    s: &mut Scratch,
    color: [f32; 4],
    y0: f32,
    y1: f32,
    wall_h: f32,
    floors: f32,
    window_w: Option<f64>,
    foundation_m: f32,
    g: GroundWalls<'_>,
) {
    let floor_m = (wall_h / floors) as f64;
    // A wall bottom never rises above the nominal bottom (a sample the base missed: the ground
    // there is above the base, so the wall is buried, never floating).
    let bottom = |h: f64| (h as f32).min(y0);
    let v_win = |y: f32| (floors as f64 - (y1 as f64 - y as f64) / floor_m) as f32;
    let v_m = |y: f32| y - y0;
    let mut column_base = 0.0f64;
    let mut pts = std::mem::take(&mut s.edge_pts);
    for ri in 0..s.rings.len() {
        let n = s.rings[ri].len();
        // Per edge: (u0, u1) in window units, or None = windowless.
        s.seam.clear();
        s.seam.extend((0..n).map(|i| s.ring_flags[ri][i] & s.ring_flags[ri][(i + 1) % n] != 0));
        let mut edge_u: Vec<Option<(f64, f64)>> = vec![None; n];
        if let Some(w) = window_w {
            ring_runs(&s.rings[ri], &s.seam, w, &mut s.runs);
            for run in &s.runs {
                let count = (run.len / w).round();
                let cell = if count >= 1.0 { run.len / count } else { 0.0 };
                let mut along = 0.0f64;
                for e in 0..run.edges {
                    let i = (run.first + e) % n;
                    let (p, q) = (s.rings[ri][i], s.rings[ri][(i + 1) % n]);
                    let len = crate::math::hypot(q[0] - p[0], q[1] - p[1]);
                    let a0 = along;
                    along += len;
                    if count >= 1.0 {
                        let u0 = column_base + a0 / cell;
                        let u1 = if e + 1 == run.edges { column_base + count } else { column_base + along / cell };
                        edge_u[i] = Some((u0, u1));
                    }
                }
                if count >= 1.0 {
                    column_base += count;
                }
            }
        }
        for i in 0..n {
            let j = (i + 1) % n;
            let (p, q) = (s.rings[ri][i], s.rings[ri][j]);
            let len = crate::math::hypot(q[0] - p[0], q[1] - p[1]);
            if len < 1e-4 || s.seam[i] {
                continue; // degenerate, or interior seam on a clip line
            }
            g.edge(p, q, s.ring_px[ri][i], s.ring_px[ri][j], &mut pts);
            for k in 1..pts.len() {
                let (a, _, ha, ta) = pts[k - 1];
                let (b, _, hb, tb) = pts[k];
                let l = crate::math::hypot(b[0] - a[0], b[1] - a[1]);
                if l < 1e-4 {
                    continue;
                }
                let (ya, yb) = (bottom(ha), bottom(hb));
                match edge_u[i] {
                    Some((u0, u1)) => {
                        let (ua, ub) = ((u0 + (u1 - u0) * ta) as f32, (u0 + (u1 - u0) * tb) as f32);
                        push_wall_on(m, a, b, l, [ya, yb], [y1; 2], color, ua, ub, [v_win(ya), v_win(yb)], [floors; 2]);
                    }
                    None => push_wall_on(m, a, b, l, [ya, yb], [y1; 2], color, -1.0, -1.0, [v_m(ya), v_m(yb)], [wall_h; 2]),
                }
                if foundation_m > 0.0 {
                    let (fa, fb) = (ya - foundation_m, yb - foundation_m);
                    push_wall_on(m, a, b, l, [fa, fb], [ya, yb], color, -1.0, -1.0, [v_m(fa), v_m(fb)], [v_m(ya), v_m(yb)]);
                }
            }
        }
    }
    s.edge_pts = pts;
}

/// Wall UV (contract section 7, docs/spec-night-windows.md section 2):
/// - windowed walls: `u` in window columns (each run of a ring between corners holds
///   `round(len / width)` whole columns; the count continues across the rings of the polygon,
///   outer ring first, then holes), `v` in floors (`max(1, round(wall_h / floor_h))` whole
///   floors stretched to the wall height; top = the floor count exactly);
/// - windowless walls (no window width, wall lower than `min_window_wall_m`, a run too short
///   for one column): `u = -1` on all four vertices, `v` in meters up from the extrusion bottom.
fn emit_polygon(m: &mut MeshData, s: &mut Scratch, res: &Resolved, color: [f32; 4], base: f32, walls: WallLayout, ground: Option<GroundWalls<'_>>) {
    // Heights stand on the terrain under the building (`Building::base_m`, the highest ground
    // under the footprint; 0 while flat).
    let y0 = base + res.min_height;
    let y1 = base + res.height;
    // The extrusion height (= wall height, roofs are flat), independent of the terrain base.
    let wall_h = res.height - res.min_height;
    let window_w = walls.window_w.filter(|w| *w > 0.0 && wall_h >= walls.min_window_wall_m);
    let floors = (wall_h / res.floor_h.max(0.1)).round().max(1.0);
    // Terrain on and the part stands on the ground: every wall reaches down to the exact ground
    // at its vertices (per-vertex bottoms, long edges subdivided over valleys), the windows keep
    // hanging from the roof (v below the nominal bottom goes negative on the downhill side), and
    // a windowless foundation reaches `foundation_m` further down.
    if let Some(g) = ground.filter(|_| res.min_height <= 0.25) {
        emit_walls_on(m, s, color, y0, y1, wall_h, floors, window_w, walls.foundation_m, g);
    } else {
        emit_walls_flat(m, s, color, y0, y1, wall_h, floors, window_w);
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
mod tests {
    use super::*;
    use crate::geo::{tile_for, Frame};
    use crate::mvt::OsmType;

    const VILNIUS: (f64, f64) = (54.6872, 25.2797);

    fn dp() -> Profile {
        Profile::default()
    }
    fn lod0() -> LodParams {
        dp().lod(0).unwrap()
    }
    fn lod1() -> LodParams {
        dp().lod(1).unwrap()
    }

    fn bld(kind: &str, height: Option<f32>, levels: Option<f32>, polys: Vec<Vec<Vec<[f64; 2]>>>) -> Building {
        Building {
            osm_type: OsmType::Way,
            id: 4242,
            kind: kind.into(),
            height,
            levels,
            min_height: 0.0,
            landuse: None,
            tags: Default::default(),
            polygons: polys,
            clipped: false,
            footprint_m2: 0.0,
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
        let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), lod0(), &dp());
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
        // UV: window columns (office: 2 m) around the ring and floors up (30 m / 3 m = 10).
        let side = 200.0 * mpp;
        let max_u = m.uvs[..16].iter().map(|uv| uv[0]).fold(0.0f32, f32::max);
        let max_v = m.uvs[..16].iter().map(|uv| uv[1]).fold(0.0f32, f32::max);
        assert_eq!(max_v, 10.0);
        assert_eq!(max_u, 4.0 * (side / 2.0).round() as f32, "{max_u} vs side {side}");
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
        let m = mesh_buildings(&[bld("yes", Some(10.0), None, vec![vec![ring]])], &p, ClipRect::tile(4096.0), lod0(), &dp());
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
        let m = mesh_buildings(&[bld("apartments", None, Some(5.0), vec![vec![outer, hole]])], &p, ClipRect::tile(4096.0), lod0(), &dp());
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
        let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), lod0(), &dp());
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
        let r0 = mesh_buildings(std::slice::from_ref(&b), &p, ClipRect::tile(4096.0), lod0(), &dp());
        let r1 = mesh_buildings(std::slice::from_ref(&b), &p, ClipRect::tile(4096.0), lod1(), &dp());
        assert_eq!(r0.triangle_count(), 5 * 2 + 3, "0.5 m keeps the 1 m bump");
        assert_eq!(r1.triangle_count(), 4 * 2 + 2, "2 m removes it");
        // < 20 m2 dropped at R1 only.
        let side = (16.0f64).sqrt() / mpp;
        let tiny = bld("yes", Some(10.0), None, vec![vec![sq(2000.0, 2000.0, side)]]);
        assert_eq!(mesh_buildings(std::slice::from_ref(&tiny), &p, ClipRect::tile(4096.0), lod1(), &dp()).triangle_count(), 0);
        assert_eq!(mesh_buildings(&[tiny], &p, ClipRect::tile(4096.0), lod0(), &dp()).triangle_count(), 10);
    }

    #[test]
    fn clipping_partitions_buildings_across_quadrants_and_skips_seams() {
        let (p, _) = setup();
        // Straddles the vertical child seam at x = 2048.
        let b = bld("yes", Some(10.0), None, vec![vec![sq(1948.0, 1000.0, 200.0)]]);
        let whole = mesh_buildings(std::slice::from_ref(&b), &p, ClipRect::tile(4096.0), lod0(), &dp());
        let nw = mesh_buildings(std::slice::from_ref(&b), &p, ClipRect::quadrant(4096.0, 0), lod0(), &dp());
        let ne = mesh_buildings(std::slice::from_ref(&b), &p, ClipRect::quadrant(4096.0, 1), lod0(), &dp());
        let sw = mesh_buildings(std::slice::from_ref(&b), &p, ClipRect::quadrant(4096.0, 2), lod0(), &dp());
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
            resolve(&b, &dp())
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
        assert_eq!(r("cathedral", None, None, None).profile, PROFILE_LANDMARK);
        assert_eq!(r("school", None, None, None).profile, PROFILE_CIVIC);
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
        assert_eq!(resolve(&raised, &dp()).height, 15.0);
    }

    /// Resolution order of docs/spec-night-windows.md section 1 and the `windows` tag.
    #[test]
    fn resolve_light_profiles_tags_shape_and_overrides() {
        use crate::mvt::{BuildingTags, RawBuilding};
        let p = dp();
        let (proj, mpp) = setup();
        // A building through the real decode path (`TileProjector::finish` sets `clipped` and
        // `footprint_m2`): a `w_m` x `w_m` square at pixel (x0, y0).
        let mk = |kind: &str, height: Option<f32>, levels: Option<f32>, tags: BuildingTags, x0: f64, w_m: f64| {
            let s = w_m / mpp;
            proj.finish(RawBuilding {
                osm_type: OsmType::Way,
                id: 777,
                kind: kind.into(),
                height,
                levels,
                min_height: 0.0,
                landuse: Some(Landuse::Residential),
                tags,
                polygons: vec![vec![sq(x0, 1000.0, s)]],
            })
            .unwrap()
        };
        let t = |historic: Option<&str>, man_made: Option<&str>| BuildingTags {
            historic: historic.map(Into::into),
            man_made: man_made.map(Into::into),
            ..Default::default()
        };
        let prof = |b: &Building| resolve(b, &p).profile;
        // 3. Kind table, then historic, then man_made for generic kinds.
        assert_eq!(prof(&mk("castle", None, None, t(None, None), 1000.0, 30.0)), PROFILE_LANDMARK);
        assert_eq!(prof(&mk("yes", None, None, t(Some("castle"), None), 1000.0, 30.0)), PROFILE_LANDMARK);
        assert_eq!(prof(&mk("yes", None, None, t(None, Some("chimney")), 1000.0, 4.0)), PROFILE_DARK);
        assert_eq!(prof(&mk("yes", None, None, t(None, Some("tower")), 1000.0, 30.0)), PROFILE_LANDMARK);
        assert_eq!(prof(&mk("weird", None, None, t(Some("city_gate"), None), 1000.0, 30.0)), PROFILE_LANDMARK);
        // A specific kind wins over historic / man_made.
        assert_eq!(prof(&mk("apartments", None, None, t(Some("castle"), Some("tower")), 1000.0, 30.0)), PROFILE_RESIDENTIAL);
        // An unknown historic value falls through to landuse.
        assert_eq!(prof(&mk("yes", None, None, t(Some("manor"), None), 1000.0, 30.0)), PROFILE_RESIDENTIAL);
        // 2. State tags beat the kind table.
        assert_eq!(prof(&mk("house", None, None, t(Some("ruins"), None), 1000.0, 10.0)), PROFILE_DARK);
        let st = |f: fn(&mut BuildingTags)| {
            let mut b = BuildingTags::default();
            f(&mut b);
            b
        };
        assert_eq!(prof(&mk("apartments", None, Some(5.0), st(|b| b.disused = true), 1000.0, 20.0)), PROFILE_DARK);
        assert_eq!(prof(&mk("castle", None, None, st(|b| b.ruins = true), 1000.0, 20.0)), PROFILE_DARK);
        assert_eq!(prof(&mk("office", None, None, st(|b| b.abandoned = true), 1000.0, 20.0)), PROFILE_DARK);
        // 4. Slender shape: tagged height 30 m on 8 x 8 m (30 >= 3 x 8, 64 < 150 m2) -> landmark.
        let tower = mk("yes", Some(30.0), None, t(None, None), 1000.0, 8.0);
        assert!(!tower.clipped && (tower.footprint_m2 - 64.0).abs() < 1.0, "{}", tower.footprint_m2);
        assert_eq!(prof(&tower), PROFILE_LANDMARK);
        assert_eq!(prof(&mk("yes", None, Some(10.0), t(None, None), 1000.0, 8.0)), PROFILE_LANDMARK, "tagged levels count");
        assert_eq!(prof(&mk("yes", Some(20.0), None, t(None, None), 1000.0, 8.0)), PROFILE_RESIDENTIAL, "too low");
        assert_eq!(prof(&mk("yes", Some(60.0), None, t(None, None), 1000.0, 13.0)), PROFILE_RESIDENTIAL, "169 m2 too big");
        assert_eq!(prof(&mk("house", Some(30.0), None, t(None, None), 1000.0, 8.0)), PROFILE_RESIDENTIAL, "specific kind");
        // ... not with a fallback height: a 5 x 5 m `yes` in residential landuse gets 15 m.
        let small = mk("yes", None, None, t(None, None), 1000.0, 5.0);
        assert_eq!(resolve(&small, &p).height, 15.0);
        assert!(p.is_slender_tower(15.0, small.footprint_m2), "the shape alone would pass");
        assert_eq!(prof(&small), PROFILE_RESIDENTIAL);
        // ... and not when the encoder clipped the footprint (a vertex on the -64 px buffer line).
        let clipped = mk("yes", Some(30.0), None, t(None, None), -64.0, 8.0);
        assert!(clipped.clipped);
        assert_eq!(prof(&clipped), PROFILE_RESIDENTIAL);
        assert!(!mk("yes", Some(30.0), None, t(None, None), -40.0, 8.0).clipped, "inside the buffer, not on its line");
        // 1. Override by OSM id wins over kind and state.
        let po = crate::profile::resolve(
            None,
            Some(&serde_json::json!({"buildings": {"overrides": {"w777": "landmark"}}})),
            &crate::NullLog,
        )
        .unwrap();
        let ruin = mk("garage", None, None, st(|b| b.ruins = true), 1000.0, 10.0);
        assert_eq!(resolve(&ruin, &p).profile, PROFILE_DARK);
        assert_eq!(resolve(&ruin, &po).profile, PROFILE_LANDMARK);
        let mut rel = ruin.clone();
        rel.osm_type = OsmType::Relation;
        assert_eq!(resolve(&rel, &po).profile, PROFILE_DARK, "w777 is not r777");
        // Windows flag and the `windows` tag (applied after resolution).
        assert!(resolve(&mk("apartments", None, None, t(None, None), 1000.0, 20.0), &p).windows);
        assert!(!resolve(&mk("castle", None, None, t(None, None), 1000.0, 20.0), &p).windows);
        assert!(!resolve(&mk("warehouse", None, None, t(None, None), 1000.0, 20.0), &p).windows);
        let mut no = mk("office", None, None, t(None, None), 1000.0, 20.0);
        no.tags.windows = Some(false);
        let r = resolve(&no, &p);
        assert_eq!((r.profile, r.windows), (PROFILE_OFFICE, false), "windows=no keeps the profile");
        let mut castle = mk("castle", None, None, t(None, None), 1000.0, 30.0);
        castle.tags.windows = Some(true);
        castle.landuse = Some(Landuse::Commercial);
        let r = resolve(&castle, &p);
        assert_eq!((r.profile, r.windows), (PROFILE_OFFICE, true), "windows=yes borrows the landuse fallback profile");
        assert_eq!(p.window_width_m(r.profile), Some(2.0));
        castle.landuse = Some(Landuse::Industrial); // fallback industrial has no windows -> residential
        let r = resolve(&castle, &p);
        assert_eq!((r.profile, r.windows), (PROFILE_RESIDENTIAL, true));
        assert_eq!(p.window_width_m(r.profile), Some(3.0));
        let mut yes_office = mk("office", None, None, t(None, None), 1000.0, 20.0);
        yes_office.tags.windows = Some(true);
        assert_eq!(resolve(&yes_office, &p).profile, PROFILE_OFFICE, "windows=yes on a windowed profile changes nothing");
        // Ids 6 and 7 reach COLOR r.
        assert_eq!(vertex_color(&resolve(&tower, &p))[0], 6.0 / 8.0);
        assert_eq!(vertex_color(&resolve(&ruin, &p))[0], 7.0 / 8.0);
    }

    #[test]
    fn synthetic_children_match_parent_quarters_and_meshing_is_byte_identical() {
        let frame = Frame::new(VILNIUS.0, VILNIUS.1);
        let k15 = tile_for(VILNIUS.0, VILNIUS.1, 15);
        let p15 = TileProjector::new(k15, frame, 4096.0);
        let b15 = crate::synthetic::generate(k15, frame);
        let c15 = frame.tile_center(k15);
        let whole = mesh_tile(&b15, &p15, 1, &dp());
        let mut sum = 0;
        for (i, child) in crate::rings::children(k15).into_iter().enumerate() {
            // The parent's quarter i, clipped exactly, equals the child tile meshed alone.
            let q = mesh_buildings(&b15, &p15, ClipRect::quadrant(4096.0, i), lod1(), &dp());
            let p16 = TileProjector::new(child, frame, 4096.0);
            let b16 = crate::synthetic::generate(child, frame);
            let t16 = mesh_tile(&b16, &p16, 0, &dp());
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
        let again = mesh_tile(&crate::synthetic::generate(k15, frame), &p15, 1, &dp());
        assert_eq!(bits(&whole), bits(&again));
        // R2 meshes (coarser, still non-empty); R3 has no mesh.
        let k14 = tile_for(VILNIUS.0, VILNIUS.1, 14);
        let p14 = TileProjector::new(k14, frame, 4096.0);
        let b14 = crate::synthetic::generate(k14, frame);
        let r2 = mesh_tile(&b14, &p14, 2, &dp());
        assert!(!r2.is_empty());
        assert_winding(&r2);
        assert_eq!(bits(&r2), bits(&mesh_tile(&b14, &p14, 2, &dp())));
        assert!(mesh_tile(&b14, &p14, 3, &dp()).is_empty());
    }

    #[test]
    fn r2_drops_small_footprints_and_simplifies_harder() {
        let (p, m) = setup();
        // 6 x 6 m shed (36 m2): kept at R1, dropped at R2.
        let side = 6.0 / m;
        let shed = bld("yes", Some(4.0), None, vec![vec![sq(100.0, 100.0, side)]]);
        assert_eq!(mesh_tile(std::slice::from_ref(&shed), &p, 1, &dp()).triangle_count(), 10);
        assert!(mesh_tile(std::slice::from_ref(&shed), &p, 2, &dp()).is_empty());
        // A 3 m bump on a 40 m wall: kept at R1 (DP 2 m), removed at R2 (DP 4 m).
        let s = 40.0 / m;
        let bump = 3.0 / m;
        let ring = vec![[0.0, 0.0], [s * 0.5, -bump], [s, 0.0], [s, s], [0.0, s]];
        let b = bld("yes", Some(10.0), None, vec![vec![ring.into_iter().map(|q| [q[0] + 500.0, q[1] + 500.0]).collect()]]);
        let r1 = mesh_tile(std::slice::from_ref(&b), &p, 1, &dp()).triangle_count();
        let r2 = mesh_tile(std::slice::from_ref(&b), &p, 2, &dp()).triangle_count();
        assert_eq!(r1, 13); // 5 walls + 3 roof triangles
        assert_eq!(r2, 10); // box
    }

    // ---- Wall UV: corners, runs, window columns, floors (docs/spec-night-windows.md W3) ----

    /// A ring given in ENU meters (x east, y north), offset to pixel (1500, 1500).
    fn ring_px(mpp: f64, pts: &[[f64; 2]]) -> Vec<[f64; 2]> {
        pts.iter().map(|p| [1500.0 + p[0] / mpp, 1500.0 - p[1] / mpp]).collect()
    }

    /// Wall quads of a mesh: (u0, u1, v_top, length in m, quad's first vertex index).
    fn wall_quads(m: &MeshData) -> Vec<(f32, f32, f32, f32, usize)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < m.positions.len() {
            if m.normals[i][1] == 0.0 {
                let (p, q) = (m.positions[i], m.positions[i + 1]);
                let len = ((q[0] - p[0]).powi(2) + (q[2] - p[2]).powi(2)).sqrt();
                // Quad layout: [u0, 0], [u1, 0], [u1, top], [u0, top].
                assert_eq!(m.uvs[i][1], 0.0);
                assert_eq!((m.uvs[i + 3][0], m.uvs[i + 2][0]), (m.uvs[i][0], m.uvs[i + 1][0]));
                assert_eq!(m.uvs[i + 2][1], m.uvs[i + 3][1]);
                out.push((m.uvs[i][0], m.uvs[i + 1][0], m.uvs[i + 2][1], len, i));
                i += 4;
            } else {
                i += 1;
            }
        }
        out
    }

    fn is_int(x: f32) -> bool {
        x == x.round()
    }

    /// Regular n-gon of radius r (ENU meters), counter-clockwise.
    fn ngon(n: usize, r: f64) -> Vec<[f64; 2]> {
        (0..n)
            .map(|i| {
                let a = std::f64::consts::TAU * i as f64 / n as f64;
                [r * a.cos(), r * a.sin()]
            })
            .collect()
    }

    /// Polyline turn at `v`: incoming along +x with length `la`, outgoing turned by `deg`.
    fn turn(deg: f64, la: f64, lb: f64, w: f64) -> bool {
        let a = deg.to_radians();
        is_corner([-la, 0.0], [0.0, 0.0], [lb * a.cos(), lb * a.sin()], w)
    }

    #[test]
    fn corner_rule_angles_and_edge_lengths() {
        // > 35 deg is a corner on any edge length.
        assert!(turn(40.0, 4.0, 4.0, 3.0));
        assert!(turn(40.0, 1.0, 1.0, 3.0));
        assert!(turn(90.0, 4.0, 4.0, 3.0));
        assert!(turn(-40.0, 4.0, 4.0, 3.0), "either turn direction");
        // 20..35 deg: a corner only with both edges >= 2 x width.
        assert!(turn(25.0, 8.0, 8.0, 3.0), "25 deg on two 8 m edges (2 x 3 m = 6 m)");
        assert!(!turn(25.0, 8.0, 5.0, 3.0));
        assert!(!turn(25.0, 5.0, 8.0, 3.0));
        assert!(!turn(22.5, 3.9, 3.9, 3.0), "16-gon vertex on short edges");
        // <= 20 deg never.
        assert!(!turn(15.0, 50.0, 50.0, 3.0));
        assert!(!turn(0.0, 10.0, 10.0, 3.0));
        // 16-gon of radius 10 m: 22.5 deg per vertex, 3.9 m edges -> one run from vertex 0.
        let ring = ngon(16, 10.0);
        let mut runs = Vec::new();
        ring_runs(&ring, &[false; 16], 3.0, &mut runs);
        assert_eq!(runs.len(), 1);
        assert_eq!((runs[0].first, runs[0].edges), (0, 16));
        // The same 16-gon with radius 40 m (15.6 m edges >= 6 m): every vertex is a corner.
        ring_runs(&ngon(16, 40.0), &[false; 16], 3.0, &mut runs);
        assert_eq!(runs.len(), 16);
        // A 40 deg turn between short edges splits a run.
        let ring = vec![[0.0, 0.0], [4.0, 0.0], [4.0 + 4.0 * 40f64.to_radians().cos(), 4.0 * 40f64.to_radians().sin()], [0.0, 12.0]];
        ring_runs(&ring, &[false; 4], 3.0, &mut runs);
        assert_eq!(runs.len(), 4, "{runs:?}");
    }

    #[test]
    fn ring_with_vertex_0_mid_wall_is_rotated_and_seams_end_runs() {
        // A 10 x 8 m rectangle whose vertex 0 lies in the middle of the south wall.
        let ring = vec![[5.0, 0.0], [10.0, 0.0], [10.0, 8.0], [0.0, 8.0], [0.0, 0.0]];
        let mut runs = Vec::new();
        ring_runs(&ring, &[false; 5], 3.0, &mut runs);
        assert_eq!(runs.len(), 4);
        // Iteration starts at the first corner (vertex 1); the south wall is one run of two
        // edges (4 -> 0 -> 1), 10 m long.
        assert_eq!((runs[0].first, runs[0].edges), (1, 1));
        let south = runs.iter().find(|r| r.edges == 2).expect("south run");
        assert_eq!(south.first, 4);
        assert!((south.len - 10.0).abs() < 1e-9);
        // A clip seam (edge 2: (10,8) -> (0,8)) has no wall and ends the runs around it.
        let mut seam = [false; 5];
        seam[2] = true;
        ring_runs(&ring, &seam, 3.0, &mut runs);
        assert_eq!(runs.len(), 3);
        assert!(runs.iter().all(|r| r.first != 2));
        // A cornerless ring with a seam: the runs start next to the seam.
        let circle = ngon(16, 10.0);
        let mut seam = [false; 16];
        seam[5] = true;
        ring_runs(&circle, &seam, 3.0, &mut runs);
        assert_eq!(runs.len(), 1);
        assert_eq!((runs[0].first, runs[0].edges), (6, 15));

        // Through the mesher: the two halves of the south wall share one run of
        // round(10 / 3) = 3 columns, split at 1.5 (not two runs of round(5 / 3) = 2).
        let (p, mpp) = setup();
        let b = bld("apartments", Some(9.0), None, vec![vec![ring_px(mpp, &ring)]]);
        let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), lod0(), &dp());
        let q = wall_quads(&m);
        assert_eq!(q.len(), 5);
        let halves: Vec<_> = q.iter().filter(|w| (w.3 - 5.0).abs() < 0.1).collect();
        assert_eq!(halves.len(), 2);
        let (lo, hi) = (halves[0].0.min(halves[1].0), halves[0].1.max(halves[1].1));
        assert_eq!(hi - lo, 3.0);
        assert!(halves.iter().any(|w| (w.1 - (lo + 1.5)).abs() < 0.02), "{halves:?}");
    }

    #[test]
    fn wall_runs_get_round_len_over_width_columns_ending_on_integers() {
        let (p, mpp) = setup();
        // 20 x 7.4 m residential block (3 m windows): 20 / 3 -> 7, 7.4 / 3 -> 2 columns.
        let ring = ring_px(mpp, &[[0.0, 0.0], [20.0, 0.0], [20.0, 7.4], [0.0, 7.4]]);
        let b = bld("apartments", Some(15.0), None, vec![vec![ring]]);
        let res = resolve(&b, &dp());
        assert_eq!((res.profile, res.windows), (PROFILE_RESIDENTIAL, true));
        let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), lod0(), &dp());
        let mut q = wall_quads(&m);
        assert_eq!(q.len(), 4);
        q.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut next = 0.0;
        for &(u0, u1, v_top, len, _) in &q {
            assert!(is_int(u0) && is_int(u1), "{u0} {u1}");
            assert_eq!(u0, next, "columns continue around the ring");
            assert_eq!(u1 - u0, (len / 3.0).round(), "len {len}");
            assert_eq!(v_top, 5.0, "15 m / 3 m = 5 floors");
            next = u1;
        }
        assert_eq!(next, 2.0 * 7.0 + 2.0 * 2.0);
        // Office (2 m windows): 20 / 2 = 10, 7.4 / 2 -> 4.
        let b = bld("office", Some(15.0), None, vec![vec![ring_px(mpp, &[[0.0, 0.0], [20.0, 0.0], [20.0, 7.4], [0.0, 7.4]])]]);
        let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), lod0(), &dp());
        let total: f32 = wall_quads(&m).iter().map(|w| w.1 - w.0).sum();
        assert_eq!(total, 28.0);
        // A wall shorter than half a window gets no columns: u = -1, v in meters, and the
        // count of the next run is not shifted.
        let b = bld("apartments", Some(12.0), None, vec![vec![ring_px(mpp, &[[0.0, 0.0], [20.0, 0.0], [20.0, 1.2], [0.0, 1.2]])]]);
        let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), lod0(), &dp());
        let q = wall_quads(&m);
        let short: Vec<_> = q.iter().filter(|w| w.3 < 2.0).collect();
        assert_eq!(short.len(), 2);
        assert!(short.iter().all(|w| w.0 == -1.0 && w.1 == -1.0 && w.2 == 12.0), "{short:?}");
        let long: Vec<_> = q.iter().filter(|w| w.3 > 2.0).collect();
        assert!(long.iter().all(|w| w.1 - w.0 == 7.0 && w.2 == 4.0), "{long:?}");
    }

    #[test]
    fn hole_columns_continue_after_the_outer_ring() {
        let (p, mpp) = setup();
        let outer = ring_px(mpp, &[[0.0, 0.0], [30.0, 0.0], [30.0, 30.0], [0.0, 30.0]]);
        let hole = ring_px(mpp, &[[10.0, 10.0], [10.0, 20.0], [20.0, 20.0], [20.0, 10.0]]);
        let b = bld("apartments", Some(9.0), None, vec![vec![outer, hole]]);
        let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), lod0(), &dp());
        let q = wall_quads(&m);
        assert_eq!(q.len(), 8);
        // Emission order: outer ring first (4 x round(30 / 3) = 40 columns), then the hole.
        let outer_max = q[..4].iter().map(|w| w.1).fold(0.0, f32::max);
        let hole_min = q[4..].iter().map(|w| w.0).fold(f32::INFINITY, f32::min);
        let hole_max = q[4..].iter().map(|w| w.1).fold(0.0, f32::max);
        assert_eq!(outer_max, 40.0);
        assert_eq!(hole_min, 40.0, "the hole's first column continues after the outer ring's last");
        assert_eq!(hole_max, 40.0 + 4.0 * 3.0);
    }

    #[test]
    fn floors_are_stretched_to_the_wall_height() {
        let (p, mpp) = setup();
        let ring = ring_px(mpp, &[[0.0, 0.0], [12.0, 0.0], [12.0, 12.0], [0.0, 12.0]]);
        // 10.4 m, floor height 3 m: round(3.47) = 3 floors, top v exactly 3.0.
        let b = bld("apartments", Some(10.4), None, vec![vec![ring.clone()]]);
        assert_eq!(resolve(&b, &dp()).floor_h, 3.0);
        let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), lod0(), &dp());
        assert!(wall_quads(&m).iter().all(|w| w.2 == 3.0));
        // At least one floor: 2.2 m (>= min_window_wall_m 2 m) -> 1 floor.
        let b = bld("apartments", Some(2.2), None, vec![vec![ring.clone()]]);
        let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), lod0(), &dp());
        assert!(wall_quads(&m).iter().all(|w| w.2 == 1.0 && w.0 >= 0.0));
        // A raised part counts its floors from its own bottom: 20 - 8 = 12 m -> 4 floors.
        let mut b = bld("apartments", Some(20.0), None, vec![vec![ring]]);
        b.min_height = 8.0;
        let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), lod0(), &dp());
        assert!(wall_quads(&m).iter().all(|w| w.2 == 4.0));
    }

    #[test]
    fn windowless_walls_get_u_minus_1_and_meters() {
        use crate::mvt::BuildingTags;
        let (p, mpp) = setup();
        let ring = ring_px(mpp, &[[0.0, 0.0], [24.0, 0.0], [24.0, 15.0], [0.0, 15.0]]);
        let windowless = |m: &MeshData, top: f32| {
            let q = wall_quads(m);
            assert_eq!(q.len(), 4);
            for w in q {
                assert_eq!((w.0, w.1), (-1.0, -1.0), "{w:?}");
                assert!((w.2 - top).abs() < 1e-5, "v in meters: {w:?}");
            }
        };
        let mk = |kind: &str, h: f32| bld(kind, Some(h), None, vec![vec![ring.clone()]]);
        // Wall part < 2 m (a raised part 10..11.5 m).
        let mut low = mk("apartments", 11.5);
        low.min_height = 10.0;
        windowless(&mesh_buildings(&[low], &p, ClipRect::tile(4096.0), lod0(), &dp()), 1.5);
        // Dark and landmark profiles.
        let garage = mk("garage", 6.0);
        assert_eq!(resolve(&garage, &dp()).profile, PROFILE_DARK);
        windowless(&mesh_buildings(&[garage], &p, ClipRect::tile(4096.0), lod0(), &dp()), 6.0);
        let castle = mk("castle", 18.0);
        assert_eq!(resolve(&castle, &dp()).profile, PROFILE_LANDMARK);
        windowless(&mesh_buildings(&[castle], &p, ClipRect::tile(4096.0), lod0(), &dp()), 18.0);
        // Industrial has no `windows` block either.
        windowless(&mesh_buildings(&[mk("warehouse", 9.0)], &p, ClipRect::tile(4096.0), lod1(), &dp()), 9.0);
        // windows = no.
        let mut no = mk("apartments", 15.0);
        no.tags = BuildingTags { windows: Some(false), ..Default::default() };
        windowless(&mesh_buildings(&[no], &p, ClipRect::tile(4096.0), lod0(), &dp()), 15.0);
        // Every R2 wall, while the same building has windows at R0 and R1.
        let flats = mk("apartments", 15.0);
        for ring_ix in [0, 1] {
            let m = mesh_tile(std::slice::from_ref(&flats), &p, ring_ix, &dp());
            assert!(wall_quads(&m).iter().all(|w| w.0 >= 0.0 && w.2 == 5.0), "R{ring_ix}");
        }
        windowless(&mesh_tile(std::slice::from_ref(&flats), &p, 2, &dp()), 15.0);
        assert!(!dp().lod(2).unwrap().windows && dp().lod(1).unwrap().windows && dp().lod(0).unwrap().windows);
        // R2 of the whole synthetic city: no wall vertex has u >= 0.
        let frame = Frame::new(VILNIUS.0, VILNIUS.1);
        let k14 = tile_for(VILNIUS.0, VILNIUS.1, 14);
        let p14 = TileProjector::new(k14, frame, 4096.0);
        let r2 = mesh_tile(&crate::synthetic::generate(k14, frame), &p14, 2, &dp());
        let walls = wall_quads(&r2);
        assert!(!walls.is_empty());
        assert!(walls.iter().all(|w| w.0 == -1.0 && w.1 == -1.0));
    }

    #[test]
    fn roof_and_bottom_cap_uv_stay_tile_local_meters() {
        let (p, mpp) = setup();
        let ring = ring_px(mpp, &[[0.0, 0.0], [24.0, 0.0], [24.0, 15.0], [0.0, 15.0]]);
        for (kind, lod) in [("apartments", lod0()), ("castle", lod0()), ("apartments", dp().lod(2).unwrap())] {
            let mut b = bld(kind, Some(18.0), None, vec![vec![ring.clone()]]);
            b.min_height = 6.0;
            let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), lod, &dp());
            let mut caps = [0, 0];
            for i in 0..m.positions.len() {
                let n = m.normals[i][1];
                if n != 0.0 {
                    assert_eq!(m.uvs[i], [m.positions[i][0], m.positions[i][2]], "{kind}");
                    caps[(n < 0.0) as usize] += 1;
                }
            }
            assert_eq!(caps, [4, 4], "roof and bottom cap of a raised part");
        }
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

    /// Roof area (m2, flat roofs facing up) and wall area (m2) of a mesh.
    fn roof_wall_m2(m: &MeshData) -> (f64, f64) {
        let (mut roof, mut wall) = (0.0f64, 0.0f64);
        for t in m.indices.chunks_exact(3) {
            let [a, b, c] = [t[0] as usize, t[1] as usize, t[2] as usize];
            let g = cross(sub(m.positions[c], m.positions[a]), sub(m.positions[b], m.positions[a]));
            let area = (dot(g, g) as f64).sqrt() / 2.0;
            if m.normals[a][1] > 0.5 {
                roof += area;
            } else if m.normals[a][1].abs() < 0.5 {
                wall += area;
            }
        }
        (roof, wall)
    }

    /// Holes reaching the clip rect (courtyards cut by a tile or chunk edge): the roof is
    /// outer ∩ rect − holes ∩ rect, courtyard walls stay, and the four z16 chunks of a z15 tile
    /// add up to the tile (roof and walls).
    #[test]
    fn courtyards_crossing_the_clip_rect_stay_open() {
        use crate::polyclip::fixtures;
        let key = tile_for(VILNIUS.0, VILNIUS.1, 15);
        let (lat, lon) = key.center_lat_lon();
        let parent = TileProjector::new(key, Frame::new(lat, lon), 4096.0);
        for (name, outer, holes) in fixtures::cases() {
            let b = bld("yes", Some(30.0), None, vec![fixtures::rings(outer, &holes)]);
            let whole = mesh_buildings(std::slice::from_ref(&b), &parent, parent.rect(), lod1(), &dp());
            let (roof, wall) = roof_wall_m2(&whole);
            let expect = fixtures::expected_m2(&parent, [0.0, 0.0, 4096.0, 4096.0], outer, &holes);
            assert!((roof - expect).abs() / expect < 1e-6, "{name}: tile roof {roof} vs {expect}");
            assert_winding(&whole);
            let (mut roof4, mut wall4) = (0.0, 0.0);
            for q in 0..4u32 {
                let child = crate::geo::TileKey::new(key.z + 1, key.x * 2 + (q & 1), key.y * 2 + (q >> 1));
                let p = parent.cut(child).unwrap();
                let r = p.rect();
                let m = mesh_buildings(std::slice::from_ref(&b), &p, r, lod0(), &dp());
                let (rf, wl) = roof_wall_m2(&m);
                let e = fixtures::expected_m2(&p, [r.x0, r.y0, r.x1, r.y1], outer, &holes);
                assert!((rf - e).abs() <= 1e-6 * e.max(1.0), "{name} child {q}: roof {rf} vs {e}");
                assert_winding(&m);
                roof4 += rf;
                wall4 += wl;
            }
            // Exact in tile pixels (`expected_m2` checks it); in ENU the tile lacks the chunk seam
            // vertices and a straight pixel edge bends slightly (~2e-6 of the area at z15): 1e-5.
            assert!((roof4 - roof).abs() / roof < 1e-5, "{name}: chunks {roof4} vs tile {roof}");
            // Known limit (as before for outer walls): a wall lying exactly on a chunk seam is
            // treated as the seam and dropped in both chunks, so a courtyard edge on x = 2048
            // has no wall in the chunks; the walls only add up where no hole edge is on a seam.
            let edge_on_seam = holes.iter().any(|h| h.contains(&2048.0));
            if !edge_on_seam {
                assert!((wall4 - wall).abs() / wall < 1e-5, "{name}: chunk walls {wall4} vs tile {wall}");
            } else {
                assert!(wall4 < wall, "{name}");
            }
        }
        // The "one edge" courtyard (300 x 200 px) has its full walls in the tile (1 % allows
        // for the scale change across the tile); the chunk sum above equals the tile, so its
        // walls stay in both chunks where x = 2048 cuts it, and no seam gets one.
        let (outer, holes) = (fixtures::cases()[0].1, fixtures::cases()[0].2.clone());
        let b = bld("yes", Some(30.0), None, vec![fixtures::rings(outer, &holes)]);
        let solid = bld("yes", Some(30.0), None, vec![fixtures::rings(outer, &[])]);
        let ring_m = 2.0 * (300.0 + 200.0) * parent.m_per_px();
        let (_, w_hole) = roof_wall_m2(&mesh_buildings(std::slice::from_ref(&b), &parent, parent.rect(), lod1(), &dp()));
        let (_, w_solid) = roof_wall_m2(&mesh_buildings(std::slice::from_ref(&solid), &parent, parent.rect(), lod1(), &dp()));
        assert!(((w_hole - w_solid) - ring_m * 30.0).abs() / (ring_m * 30.0) < 1e-2, "{w_hole} - {w_solid}");
    }

    #[test]
    fn courtyard_clip_is_deterministic() {
        use crate::polyclip::fixtures;
        let (p, _) = setup();
        for (_, outer, holes) in fixtures::cases() {
            let b = bld("yes", Some(30.0), None, vec![fixtures::rings(outer, &holes)]);
            for q in 0..4 {
                let r = ClipRect::quadrant(4096.0, q);
                assert_eq!(mesh_buildings(std::slice::from_ref(&b), &p, r, lod0(), &dp()), mesh_buildings(std::slice::from_ref(&b), &p, r, lod0(), &dp()));
            }
        }
    }
}
