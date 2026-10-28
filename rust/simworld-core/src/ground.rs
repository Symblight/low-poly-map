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

use crate::mesher::{clip_ring, simplify_ring, ClipRect, MeshData};
use ahash::AHashMap;

use crate::mvt::{ring_area2, GroundFeatures, RoadLine, TileProjector, GROUND_ROAD, GROUND_WATER, ROAD_MOTORWAY, ROAD_OTHER};
use crate::profile::Profile;
use crate::rings::ring_for_zoom;

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

/// Terrain height at a tile pixel plus, with a lift field, the weighted band margin.
#[inline]
fn draped_h(proj: &TileProjector, px: [f64; 2], h: f64, field: Option<f64>) -> f64 {
    match field {
        Some(l) => {
            let (lat, lon) = proj.lat_lon(px);
            h + proj.terrain().lift_weight(lat, lon) * l
        }
        None => h,
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_area(
    m: &mut MeshData,
    s: &mut Scratch,
    rings: &[Vec<[f64; 2]>],
    ctx: &AreaCtx<'_>,
    tol_px: f64,
    lift: f32,
    color: [f32; 4],
    water: bool,
    cut: bool,
) {
    let (proj, rect, field) = (ctx.proj, ctx.rect, ctx.field);
    let bb = bbox(&rings[0]);
    if bb[2] <= rect.x0 || bb[0] >= rect.x1 || bb[3] <= rect.y0 || bb[1] >= rect.y1 {
        return;
    }
    s.flat.clear();
    s.hs.clear();
    s.pxs.clear();
    s.holes.clear();
    for (ri, ring) in rings.iter().enumerate() {
        let rb = bbox(ring);
        if ri > 0 && (rb[2] <= rect.x0 || rb[0] >= rect.x1 || rb[3] <= rect.y0 || rb[1] >= rect.y1) {
            continue; // hole entirely outside the tile
        }
        let needs_clip = rb[0] < rect.x0 || rb[2] > rect.x1 || rb[1] < rect.y0 || rb[3] > rect.y1;
        if needs_clip {
            clip_ring(ring, rect, &mut s.clip_a, &mut s.clip_b);
        } else {
            s.clip_a.clear();
            s.clip_a.extend_from_slice(ring);
        }
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
                return;
            }
            continue;
        }
        s.locked.clear();
        s.locked.extend(s.dedup.iter().map(|&p| needs_clip && rect.flags(p) != 0));
        simplify_ring(&s.dedup, &s.locked, tol_px, &mut s.keep, &mut s.kept);
        if s.kept.len() < 3 {
            if ri == 0 {
                return;
            }
            continue;
        }
        let start = s.flat.len();
        for &i in &s.kept {
            let px = s.dedup[i];
            let (en, h) = if water { proj.project_water(px) } else { proj.project_h(px) };
            s.flat.push(en[0]);
            s.flat.push(en[1]);
            s.hs.push(draped_h(proj, px, h, field));
            s.pxs.push(px);
        }
        let (ring_en, _) = s.flat[start..].as_chunks::<2>();
        if ring_area2(ring_en).abs() < 1e-3 {
            // Degenerate (e.g. a sliver along a clip line): drop it.
            s.flat.truncate(start);
            s.hs.truncate(start / 2);
            s.pxs.truncate(start / 2);
            if ri == 0 {
                return;
            }
            continue;
        }
        if ri > 0 {
            s.holes.push(start / 2);
        }
    }
    let Ok(mut tris) = earcutr::earcut(&s.flat, &s.holes, 2) else { return };
    if cut && !ctx.cutouts.is_empty() {
        tris = cut_triangles(s, &tris, ctx, water);
    }
    if tris.is_empty() {
        return;
    }
    // Water drapes like every other area unless `terrain.water_snap_m` asks for the DEM-era snap
    // (then its corners are snapped to the lake level and it is not subdivided).
    if proj.terrain().is_active() && (!water || proj.terrain().water_snap_m() <= 0.0) {
        // Drape: split long triangle edges where the terrain is not flat along them.
        let mut d = Drape::new(proj, &s.pxs, &s.flat, &s.hs, field, lift as f64, ctx.step);
        let mut out = Vec::with_capacity(tris.len());
        for t in tris.chunks_exact(3) {
            d.triangle([t[0] as u32, t[1] as u32, t[2] as u32], 0, &mut out);
        }
        let base = m.positions.len() as i32;
        for k in 0..d.en.len() {
            push_v(m, d.en[k], d.h[k] + lift as f64, color);
        }
        for t in out {
            tri_up(m, base + t[0] as i32, base + t[1] as i32, base + t[2] as i32);
        }
        return;
    }
    let base = m.positions.len() as i32;
    for k in 0..s.flat.len() / 2 {
        push_v(m, [s.flat[2 * k], s.flat[2 * k + 1]], s.hs[k] + lift as f64, color);
    }
    for t in tris.chunks_exact(3) {
        tri_up(m, base + t[0] as i32, base + t[1] as i32, base + t[2] as i32);
    }
}

/// Remove the tile's cutouts (convex quads, a seed world's airport fences) from an area's
/// triangles (`tris` index `s.flat` / `s.hs` / `s.pxs`; new vertices are appended there). Each
/// triangle minus a convex quad is the union of convex pieces (outside edge 0; inside 0 and
/// outside 1; ...), fan-triangulated. New vertices lie on a quad edge; they are computed from the
/// triangle edge's endpoints in a canonical order, snapped to 2^-20 px and shared through a map,
/// so two triangles that share an edge cut it at the same vertex (no T-junction for the drape).
fn cut_triangles(s: &mut Scratch, tris: &[usize], ctx: &AreaCtx<'_>, water: bool) -> Vec<usize> {
    const Q: f64 = (1u64 << 20) as f64;
    let quads: Vec<([[f64; 2]; 4], [f64; 4], f64)> = ctx
        .cutouts
        .iter()
        .map(|q| {
            let area2: f64 = (0..4).map(|i| q[i][0] * q[(i + 1) % 4][1] - q[(i + 1) % 4][0] * q[i][1]).sum();
            (*q, bbox(q), area2.signum())
        })
        .collect();
    let mut index: AHashMap<(u64, u64), usize> = AHashMap::new();
    let mut out = Vec::with_capacity(tris.len());
    // Polygon vertex: (tile pixel, index into the vertex arrays or usize::MAX = new).
    type V = ([f64; 2], usize);
    let mut work: Vec<Vec<V>> = Vec::new();
    let mut next: Vec<Vec<V>> = Vec::new();
    for t in tris.chunks_exact(3) {
        let tri: Vec<V> = t.iter().map(|&i| (s.pxs[i], i)).collect();
        let tb = bbox(&[tri[0].0, tri[1].0, tri[2].0]);
        work.clear();
        work.push(tri);
        for (q, qb, sign) in &quads {
            if tb[2] <= qb[0] || tb[0] >= qb[2] || tb[3] <= qb[1] || tb[1] >= qb[3] {
                continue;
            }
            next.clear();
            for poly in work.drain(..) {
                // Signed side of edge i: > 0 inside the quad.
                let mut rest = poly;
                for i in 0..4 {
                    let (a, b) = (q[i], q[(i + 1) % 4]);
                    let f = |p: [f64; 2]| sign * ((b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]));
                    let outside = clip_half(&rest, |p| -f(p));
                    if outside.len() >= 3 {
                        next.push(outside);
                    }
                    rest = clip_half(&rest, f);
                    if rest.len() < 3 {
                        break;
                    }
                }
            }
            std::mem::swap(&mut work, &mut next);
        }
        for poly in &work {
            let ids: Vec<usize> = poly
                .iter()
                .map(|&(p, i)| {
                    if i != usize::MAX {
                        return i;
                    }
                    let p = [(p[0] * Q).round() / Q, (p[1] * Q).round() / Q];
                    *index.entry((p[0].to_bits(), p[1].to_bits())).or_insert_with(|| {
                        let (en, h) = if water { ctx.proj.project_water(p) } else { ctx.proj.project_h(p) };
                        s.flat.push(en[0]);
                        s.flat.push(en[1]);
                        s.hs.push(draped_h(ctx.proj, p, h, ctx.field));
                        s.pxs.push(p);
                        s.hs.len() - 1
                    })
                })
                .collect();
            for k in 1..ids.len() - 1 {
                out.extend_from_slice(&[ids[0], ids[k], ids[k + 1]]);
            }
        }
    }
    out
}

/// Sutherland-Hodgman against one half-plane: keeps the part where `f >= 0`. Crossing points are
/// computed from the edge's endpoints in lexicographic order, so both triangles of a shared
/// edge get the same bits.
fn clip_half(poly: &[([f64; 2], usize)], f: impl Fn([f64; 2]) -> f64) -> Vec<([f64; 2], usize)> {
    let n = poly.len();
    let mut out = Vec::with_capacity(n + 2);
    for k in 0..n {
        let (p, q) = (poly[k], poly[(k + 1) % n]);
        let (fp, fq) = (f(p.0), f(q.0));
        if fp >= 0.0 {
            out.push(p);
        }
        if (fp > 0.0 && fq < 0.0) || (fp < 0.0 && fq > 0.0) {
            let (a, b, fa, fb) = if (p.0[0], p.0[1]) <= (q.0[0], q.0[1]) { (p.0, q.0, fp, fq) } else { (q.0, p.0, fq, fp) };
            let t = fa / (fa - fb);
            out.push(([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t], usize::MAX));
        }
    }
    out
}

/// Smallest drape tolerance (m): a draped edge on gentle relief is never split for less.
pub const DRAPE_FLAT_TOLERANCE_M: f64 = 0.1;
/// Safety cap of the triangle subdivision depth.
const DRAPE_MAX_DEPTH: u32 = 20;

/// Whether the ground edge a-b (tile pixels, ENU meters, terrain heights) must be split: longer
/// than `terrain.drape_step_m` and, at 1/4, 1/2 or 3/4 of it, the terrain rises more than the
/// band's drape margin above the straight line between the end heights (the margin the ground
/// vertices carry would no longer keep the edge on or above the exact height), or falls more
/// than twice the margin below it (the ground would float). Callers pass the ends in a
/// canonical order so both triangles of a shared edge decide the same.
pub fn drape_split(proj: &TileProjector, pa: [f64; 2], pb: [f64; 2], ea: [f64; 2], eb: [f64; 2], ha: f64, hb: f64) -> bool {
    drape_split_on(proj, pa, pb, ea, eb, ha, hb, None)
}

/// Tolerance of a draped edge inside an airport fence (m), at least: there the height is planar
/// per strip group (kinks only at the strip ends), so the ground follows it to 1 cm in R0 and to
/// half a stacking step in the coarser bands (R1 3 cm, R2 7.5 cm, R3 25 cm at the defaults).
pub const DRAPE_FIELD_TOLERANCE_M: f64 = 0.01;

/// The margin field of one draped feature: the band margin, the feature's stacking offset
/// (slack outside the fence) and the tolerance inside the fence.
#[derive(Clone, Copy)]
struct FieldLift {
    lift: f64,
    slack: f64,
    inside: f64,
}

impl FieldLift {
    fn new(field: Option<f64>, slack: f64, step: f64) -> Option<Self> {
        field.map(|lift| Self { lift, slack, inside: (0.5 * step).max(DRAPE_FIELD_TOLERANCE_M) })
    }
}

/// [`drape_split`] with an optional margin field (`Some((band margin, stacking offset))`,
/// contract section 9 "Airports"): heights are then terrain + weight x margin. Inside a fence
/// (weight 0 at a test point) the tolerance is [`DRAPE_FIELD_TOLERANCE_M`] / half a step both
/// ways; outside it the smallest weighted margin plus half the feature's stacking offset
/// upwards (twice the margin downwards), so the edge stays on or above the exact height.
#[allow(clippy::too_many_arguments)]
fn drape_split_on(proj: &TileProjector, pa: [f64; 2], pb: [f64; 2], ea: [f64; 2], eb: [f64; 2], ha: f64, hb: f64, field: Option<FieldLift>) -> bool {
    let step = proj.terrain().drape_step_m();
    if crate::math::hypot(eb[0] - ea[0], eb[1] - ea[1]) <= step {
        return false;
    }
    if let Some(FieldLift { lift: l, slack, inside }) = field {
        let mut test = [(0.0, 0.0); 3];
        let mut w_min = 1.0f64;
        for (k, t) in [0.25, 0.5, 0.75].into_iter().enumerate() {
            let px = [pa[0] + (pb[0] - pa[0]) * t, pa[1] + (pb[1] - pa[1]) * t];
            let (lat, lon) = proj.lat_lon(px);
            let w = proj.terrain().lift_weight(lat, lon);
            w_min = w_min.min(w);
            test[k] = (t, proj.terrain().height(lat, lon) + w * l);
        }
        // Outside the fence half the feature's stacking offset is slack too; inside it (w = 0)
        // 1 cm (R0) or half a step.
        let (rise, fall) = if w_min > 0.0 {
            ((l * w_min + 0.5 * slack).max(inside), (2.0 * l * w_min + 0.5 * slack).max(inside))
        } else {
            (inside, inside)
        };
        return test.iter().any(|&(t, h)| {
            let d = h - (ha + (hb - ha) * t);
            d > rise || -d > fall
        });
    }
    let margin = ring_for_zoom(proj.key().z)
        .map_or(DRAPE_FLAT_TOLERANCE_M, |r| (proj.terrain().drape_margin_m(r) as f64).max(DRAPE_FLAT_TOLERANCE_M));
    [0.25, 0.5, 0.75].into_iter().any(|t| {
        let px = [pa[0] + (pb[0] - pa[0]) * t, pa[1] + (pb[1] - pa[1]) * t];
        let d = proj.ground_at(px) - (ha + (hb - ha) * t);
        d > margin || -d > 2.0 * margin
    })
}

/// Triangle subdivision of one area polygon by edge midpoints (computed in tile pixels and
/// projected), shared through a per-polygon edge map, so neighbouring triangles always split a
/// shared edge the same way (no T-junction cracks).
struct Drape<'a> {
    proj: &'a TileProjector,
    px: Vec<[f64; 2]>,
    en: Vec<[f64; 2]>,
    h: Vec<f64>,
    /// (lower index, higher index) -> midpoint index, or `u32::MAX` = not split.
    mids: AHashMap<(u32, u32), u32>,
    /// Lift field (see [`AreaCtx::field`]) and the stacking offset as drape slack.
    field: Option<f64>,
    slack: f64,
    step: f64,
}

impl<'a> Drape<'a> {
    fn new(proj: &'a TileProjector, px: &[[f64; 2]], flat: &[f64], h: &[f64], field: Option<f64>, slack: f64, step: f64) -> Self {
        let en = flat.chunks_exact(2).map(|c| [c[0], c[1]]).collect();
        Self { proj, px: px.to_vec(), en, h: h.to_vec(), mids: AHashMap::new(), field, slack, step }
    }

    fn mid(&mut self, a: u32, b: u32) -> Option<u32> {
        let k = (a.min(b), a.max(b));
        if let Some(&m) = self.mids.get(&k) {
            return (m != u32::MAX).then_some(m);
        }
        let (i, j) = (k.0 as usize, k.1 as usize);
        let split = drape_split_on(self.proj, self.px[i], self.px[j], self.en[i], self.en[j], self.h[i], self.h[j], FieldLift::new(self.field, self.slack, self.step));
        let m = if split {
            let p = [(self.px[i][0] + self.px[j][0]) * 0.5, (self.px[i][1] + self.px[j][1]) * 0.5];
            let (en, h) = self.proj.project_h(p);
            self.px.push(p);
            self.en.push(en);
            self.h.push(draped_h(self.proj, p, h, self.field));
            (self.px.len() - 1) as u32
        } else {
            u32::MAX
        };
        self.mids.insert(k, m);
        (m != u32::MAX).then_some(m)
    }

    fn triangle(&mut self, t: [u32; 3], depth: u32, out: &mut Vec<[u32; 3]>) {
        if depth >= DRAPE_MAX_DEPTH {
            out.push(t);
            return;
        }
        let [a, b, c] = t;
        let (ab, bc, ca) = (self.mid(a, b), self.mid(b, c), self.mid(c, a));
        let d = depth + 1;
        match (ab, bc, ca) {
            (None, None, None) => out.push(t),
            (Some(ab), Some(bc), Some(ca)) => {
                self.triangle([a, ab, ca], d, out);
                self.triangle([ab, b, bc], d, out);
                self.triangle([ca, bc, c], d, out);
                self.triangle([ab, bc, ca], d, out);
            }
            (Some(ab), None, None) => {
                self.triangle([a, ab, c], d, out);
                self.triangle([ab, b, c], d, out);
            }
            (None, Some(bc), None) => {
                self.triangle([b, bc, a], d, out);
                self.triangle([bc, c, a], d, out);
            }
            (None, None, Some(ca)) => {
                self.triangle([c, ca, b], d, out);
                self.triangle([ca, a, b], d, out);
            }
            (Some(ab), Some(bc), None) => {
                self.triangle([ab, b, bc], d, out);
                self.triangle([a, ab, bc], d, out);
                self.triangle([a, bc, c], d, out);
            }
            (None, Some(bc), Some(ca)) => {
                self.triangle([bc, c, ca], d, out);
                self.triangle([b, bc, ca], d, out);
                self.triangle([b, ca, a], d, out);
            }
            (Some(ab), None, Some(ca)) => {
                self.triangle([ca, a, ab], d, out);
                self.triangle([c, ca, ab], d, out);
                self.triangle([c, ab, b], d, out);
            }
        }
    }
}

/// Densify a road run (tile pixels) for the drape: recursively insert midpoints into segments
/// that [`drape_split`] says must be split. `out` receives the run with the inserted points.
fn densify_run(proj: &TileProjector, run: &[[f64; 2]], field: Option<f64>, slack: f64, step: f64, out: &mut Vec<([f64; 2], [f64; 2], f64)>) {
    type Pt = ([f64; 2], [f64; 2], f64);
    #[allow(clippy::too_many_arguments)]
    fn seg(proj: &TileProjector, a: Pt, b: Pt, depth: u32, field: Option<f64>, slack: f64, step: f64, out: &mut Vec<Pt>) {
        if depth < DRAPE_MAX_DEPTH && drape_split_on(proj, a.0, b.0, a.1, b.1, a.2, b.2, FieldLift::new(field, slack, step)) {
            let p = [(a.0[0] + b.0[0]) * 0.5, (a.0[1] + b.0[1]) * 0.5];
            let (en, h) = proj.project_h(p);
            let m = (p, en, draped_h(proj, p, h, field));
            seg(proj, a, m, depth + 1, field, slack, step, out);
            seg(proj, m, b, depth + 1, field, slack, step, out);
        } else {
            out.push(b);
        }
    }
    out.clear();
    let mut prev: Option<Pt> = None;
    for &p in run {
        let (en, h) = proj.project_h(p);
        let cur = (p, en, draped_h(proj, p, h, field));
        match prev {
            None => out.push(cur),
            Some(a) => seg(proj, a, cur, 0, field, slack, step, out),
        }
        prev = Some(cur);
    }
}

/// Liang–Barsky: the part of segment a-b inside `r`, as parameters (t0, t1).
fn clip_segment(a: [f64; 2], b: [f64; 2], r: &ClipRect) -> Option<(f64, f64)> {
    let d = [b[0] - a[0], b[1] - a[1]];
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [(-d[0], a[0] - r.x0), (d[0], r.x1 - a[0]), (-d[1], a[1] - r.y0), (d[1], r.y1 - a[1])] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                if t > t1 {
                    return None;
                }
                t0 = t0.max(t);
            } else {
                if t < t0 {
                    return None;
                }
                t1 = t1.min(t);
            }
        }
    }
    (t1 > t0).then_some((t0, t1))
}

/// One road feature as strips of half-width `hw` (m).
#[allow(clippy::too_many_arguments)]
fn emit_road(m: &mut MeshData, s: &mut Scratch, r: &RoadLine, proj: &TileProjector, rect: &ClipRect, tol_px: f64, lift: f32, hw: f64, field: Option<f64>, step: f64) {
    let color = road_color(r.class, r.lit);
    for line in &r.lines {
        // Split into runs inside the tile.
        s.run.clear();
        for w in line.windows(2) {
            let (a, b) = (w[0], w[1]);
            let Some((t0, t1)) = clip_segment(a, b, rect) else {
                flush_run(m, s, proj, tol_px, hw, lift, color, field, step);
                continue;
            };
            let lerp = |t: f64| if t == 0.0 { a } else if t == 1.0 { b } else { [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t] };
            let (p, q) = (lerp(t0), lerp(t1));
            if s.run.last() != Some(&p) {
                flush_run(m, s, proj, tol_px, hw, lift, color, field, step);
                s.run.push(p);
            }
            s.run.push(q);
            if t1 < 1.0 {
                flush_run(m, s, proj, tol_px, hw, lift, color, field, step);
            }
        }
        flush_run(m, s, proj, tol_px, hw, lift, color, field, step);
    }
}

/// Simplify, project and buffer the current run, then clear it.
#[allow(clippy::too_many_arguments)]
fn flush_run(m: &mut MeshData, s: &mut Scratch, proj: &TileProjector, tol_px: f64, hw: f64, lift: f32, color: [f32; 4], field: Option<f64>, step: f64) {
    if s.run.len() < 2 {
        s.run.clear();
        return;
    }
    // Open polyline DP via the closed-ring routine with both endpoints locked (the closing
    // segment between them then has no interior points).
    let n = s.run.len();
    s.locked.clear();
    s.locked.resize(n, false);
    s.locked[0] = true;
    s.locked[n - 1] = true;
    simplify_ring(&s.run, &s.locked, tol_px, &mut s.keep, &mut s.kept);
    s.pts.clear();
    s.ph.clear();
    if proj.terrain().is_active() {
        // Drape: the strip follows the terrain along its centerline.
        let kept: Vec<[f64; 2]> = s.kept.iter().map(|&i| s.run[i]).collect();
        let mut dense = Vec::with_capacity(kept.len());
        densify_run(proj, &kept, field, lift as f64, step, &mut dense);
        for (_, en, h) in dense {
            if s.pts.last().is_some_and(|l: &[f64; 2]| crate::math::hypot(l[0] - en[0], l[1] - en[1]) < 0.01) {
                continue;
            }
            s.pts.push(en);
            s.ph.push(h + lift as f64);
        }
        s.run.clear();
        strip(m, &s.pts, &s.ph, hw, color);
        return;
    }
    for &i in &s.kept {
        let (en, h) = proj.project_h(s.run[i]);
        if s.pts.last().is_some_and(|l: &[f64; 2]| crate::math::hypot(l[0] - en[0], l[1] - en[1]) < 0.01) {
            continue;
        }
        s.pts.push(en);
        s.ph.push(h + lift as f64);
    }
    s.run.clear();
    strip(m, &s.pts, &s.ph, hw, color);
}

/// Buffer a polyline (ENU meters, per-point heights) into a flat strip of half-width `hw`.
pub(crate) fn strip(m: &mut MeshData, pts: &[[f64; 2]], hs: &[f64], hw: f64, color: [f32; 4]) {
    let n = pts.len();
    if n < 2 {
        return;
    }
    let dir = |i: usize| {
        let (dx, dy) = (pts[i + 1][0] - pts[i][0], pts[i + 1][1] - pts[i][1]);
        let l = crate::math::hypot(dx, dy).max(1e-9);
        [dx / l, dy / l]
    };
    // Left normal of a direction (EN plane).
    let left = |d: [f64; 2]| [-d[1], d[0]];
    let off = |p: [f64; 2], v: [f64; 2], k: f64| [p[0] + v[0] * k, p[1] + v[1] * k];
    let d0 = dir(0);
    let l0 = left(d0);
    let mut pl = push_v(m, off(pts[0], l0, hw), hs[0], color);
    let mut pr = push_v(m, off(pts[0], l0, -hw), hs[0], color);
    let mut dprev = d0;
    for i in 1..n {
        let p = pts[i];
        let a = left(dprev);
        if i == n - 1 {
            let l = push_v(m, off(p, a, hw), hs[i], color);
            let r = push_v(m, off(p, a, -hw), hs[i], color);
            tri_up(m, pl, pr, r);
            tri_up(m, pl, r, l);
            break;
        }
        let d = dir(i);
        let b = left(d);
        let mv = [a[0] + b[0], a[1] + b[1]];
        let mlen = crate::math::hypot(mv[0], mv[1]);
        if mlen >= 1.0 {
            // Miter (turn <= 120°): miter length hw / cos(θ/2) <= 2 hw.
            let k = hw / (mlen * 0.5) / mlen;
            let l = push_v(m, off(p, mv, k), hs[i], color);
            let r = push_v(m, off(p, mv, -k), hs[i], color);
            tri_up(m, pl, pr, r);
            tri_up(m, pl, r, l);
            pl = l;
            pr = r;
        } else {
            // Bevel: close the incoming segment, fill the outer wedge, start the next one.
            let le = push_v(m, off(p, a, hw), hs[i], color);
            let re = push_v(m, off(p, a, -hw), hs[i], color);
            tri_up(m, pl, pr, re);
            tri_up(m, pl, re, le);
            let ls = push_v(m, off(p, b, hw), hs[i], color);
            let rs = push_v(m, off(p, b, -hw), hs[i], color);
            let c = push_v(m, p, hs[i], color);
            let turn_left = dprev[0] * d[1] - dprev[1] * d[0] > 0.0;
            if turn_left {
                tri_up(m, c, re, rs);
            } else {
                tri_up(m, c, le, ls);
            }
            pl = ls;
            pr = rs;
        }
        dprev = d;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::{tile_for, Frame};
    use crate::mvt::{GroundArea, GROUND_GRASS, GROUND_WATER, ROAD_PRIMARY, ROAD_RESIDENTIAL, ROAD_SECONDARY, ROAD_TERTIARY};

    fn dp() -> Profile {
        Profile::default()
    }

    const VILNIUS: (f64, f64) = (54.6872, 25.2797);

    fn proj() -> TileProjector {
        let key = tile_for(VILNIUS.0, VILNIUS.1, 16);
        let (lat, lon) = key.center_lat_lon();
        TileProjector::new(key, Frame::new(lat, lon), 4096.0)
    }

    fn sq(x0: f64, y0: f64, s: f64) -> Vec<[f64; 2]> {
        vec![[x0, y0], [x0 + s, y0], [x0 + s, y0 + s], [x0, y0 + s]]
    }

    fn area_m2(m: &MeshData) -> f64 {
        m.indices
            .chunks_exact(3)
            .map(|t| {
                let (a, b, c) = (m.positions[t[0] as usize], m.positions[t[1] as usize], m.positions[t[2] as usize]);
                (((b[0] - a[0]) * (c[2] - a[2]) - (c[0] - a[0]) * (b[2] - a[2])) as f64).abs() * 0.5
            })
            .sum()
    }

    fn all_up(m: &MeshData) -> bool {
        m.indices.chunks_exact(3).all(|t| {
            let (a, b, c) = (m.positions[t[0] as usize], m.positions[t[1] as usize], m.positions[t[2] as usize]);
            (c[2] - a[2]) * (b[0] - a[0]) - (c[0] - a[0]) * (b[2] - a[2]) > 0.0
        }) && m.normals.iter().all(|n| *n == UP)
    }

    fn water_with_island() -> GroundFeatures {
        // Exterior clockwise in screen space, hole counter-clockwise (MVT convention).
        let mut hole = sq(1500.0, 1500.0, 1000.0);
        hole.reverse();
        GroundFeatures {
            areas: vec![GroundArea { class: GROUND_WATER, level: 7, id: 1, rings: vec![sq(1000.0, 1000.0, 2000.0), hole], tint: 0, sub: 0, cut: false }],
            roads: vec![],
            ..Default::default()
        }
    }

    #[test]
    fn triangulation_respects_holes() {
        let p = proj();
        let mpp = p.m_per_px();
        let m = mesh_ground(&water_with_island(), &p, 0, &dp());
        assert!(!m.is_empty() && all_up(&m));
        let expect = (2000.0f64.powi(2) - 1000.0f64.powi(2)) * mpp * mpp;
        assert!((area_m2(&m) - expect).abs() / expect < 0.01, "{} vs {expect}", area_m2(&m));
        // No triangle covers the island center.
        let c = p.project([2000.0, 2000.0]);
        let hit = m.indices.chunks_exact(3).any(|t| {
            let v: Vec<[f32; 2]> = t.iter().map(|&i| [m.positions[i as usize][0], -m.positions[i as usize][2]]).collect();
            let s = |a: [f32; 2], b: [f32; 2]| (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
            let (d1, d2, d3) = (s(v[0], v[1]), s(v[1], v[2]), s(v[2], v[0]));
            (d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0) || (d1 <= 0.0 && d2 <= 0.0 && d3 <= 0.0)
        });
        assert!(!hit);
        // Water color + level lift.
        assert!(m.colors.iter().all(|c| *c == [2.0 / 16.0, 0.0, 0.0, 1.0]));
        assert!(m.positions.iter().all(|v| (v[1] - 7.0 * dp().ground_params(0).unwrap().step_m).abs() < 1e-6));
        // R3: nothing.
        assert!(mesh_ground(&water_with_island(), &p, 3, &dp()).is_empty());
    }

    #[test]
    fn areas_are_clipped_to_the_tile() {
        let p = proj();
        let g = GroundFeatures {
            areas: vec![GroundArea { class: GROUND_GRASS, level: 5, id: 2, rings: vec![sq(-1000.0, -1000.0, 6000.0)], tint: 0, sub: 0, cut: false }],
            roads: vec![],
            ..Default::default()
        };
        let m = mesh_ground(&g, &p, 0, &dp());
        let w = p.m_per_px() * 4096.0;
        assert!((area_m2(&m) - w * w).abs() / (w * w) < 0.01);
        let h = w / 2.0 + 0.5;
        assert!(m.positions.iter().all(|v| (v[0] as f64).abs() < h && (v[2] as f64).abs() < h));
    }

    fn road(class: u8, link: bool, lit: bool, line: Vec<[f64; 2]>) -> RoadLine {
        RoadLine { class, link, minor: false, lit, id: 7, lines: vec![line] }
    }

    #[test]
    fn strip_widths_by_class() {
        let p = proj();
        let mpp = p.m_per_px();
        for (class, link, w) in [
            (ROAD_MOTORWAY, false, 14.0),
            (ROAD_PRIMARY, false, 10.0),
            (ROAD_SECONDARY, false, 8.0),
            (ROAD_TERTIARY, false, 6.0),
            (ROAD_RESIDENTIAL, false, 6.0),
            (ROAD_OTHER, false, 3.0),
            (ROAD_PRIMARY, true, 6.0),
            (ROAD_RESIDENTIAL, true, 3.6),
        ] {
            assert_eq!(dp().road_width_m(class, link), w);
            // Straight east-west road through the whole tile (clipped at both edges).
            let g = GroundFeatures { areas: vec![], roads: vec![road(class, link, false, vec![[-200.0, 2048.0], [4300.0, 2048.0]])], ..Default::default() };
            let m = mesh_ground(&g, &p, 0, &dp());
            assert!(all_up(&m));
            let zs: Vec<f32> = m.positions.iter().map(|v| v[2]).collect();
            let span = zs.iter().cloned().fold(f32::MIN, f32::max) - zs.iter().cloned().fold(f32::MAX, f32::min);
            assert!((span as f64 - w).abs() < 0.05, "class {class}: {span}");
            let len = 4096.0 * mpp;
            assert!((area_m2(&m) - len * w).abs() / (len * w) < 0.01);
        }
    }

    #[test]
    fn joins_miter_and_bevel_cover_the_corner() {
        let mut m = MeshData::default();
        // 90° turn: miter.
        strip(&mut m, &[[0.0, 0.0], [100.0, 0.0], [100.0, 100.0]], &[0.0; 3], 3.0, road_color(ROAD_TERTIARY, true));
        assert_eq!(m.vertex_count(), 6);
        assert!((area_m2(&m) - 2.0 * 100.0 * 6.0).abs() < 1.0, "{}", area_m2(&m));
        // Hairpin (170°): bevel join with a fill triangle, no spike.
        let mut b = MeshData::default();
        let back = [100.0 - 100.0 * 170f64.to_radians().cos().abs(), 100.0 * 170f64.to_radians().sin()];
        strip(&mut b, &[[0.0, 0.0], [100.0, 0.0], back], &[0.0; 3], 3.0, road_color(ROAD_TERTIARY, false));
        assert_eq!(b.triangle_count(), 5);
        assert!(b.positions.iter().all(|v| v[0] < 104.0));
        assert!(all_up(&m) && all_up(&b));
    }

    #[test]
    fn colour_encoding_matches_contract() {
        assert_eq!(area_color(GROUND_GRASS), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(area_color(GROUND_WATER), [0.125, 0.0, 0.0, 1.0]);
        assert_eq!(area_color(crate::mvt::GROUND_OTHER), [6.0 / 16.0, 0.0, 0.0, 1.0]);
        assert_eq!(road_color(ROAD_MOTORWAY, true), [0.5, 1.0, 0.125, 1.0]);
        assert_eq!(road_color(ROAD_OTHER, false), [0.5, 0.0, 0.75, 1.0]);
        // Exact multiples (Godot decodes round(r * 16), round(b * 8)).
        for c in 0..=8u8 {
            assert_eq!(area_color(c)[0] * 16.0, c as f32);
        }
        for rc in 1..=6u8 {
            assert_eq!(road_color(rc, false)[2] * 8.0, rc as f32);
        }
        // Classification tables.
        let d = dp();
        let (ground_area_class, road_class, is_lit) =
            (|k: &str| d.ground_area_class(k), |h: &str| d.road_class(h), |v: &str| d.is_lit(v));
        assert_eq!(ground_area_class("park"), (GROUND_GRASS, 5));
        assert_eq!(ground_area_class("riverbank"), (GROUND_WATER, 7));
        assert_eq!(ground_area_class("wood").0, crate::mvt::GROUND_FOREST);
        assert_eq!(ground_area_class("retail").0, crate::mvt::GROUND_COMMERCIAL);
        assert_eq!(ground_area_class("education").0, crate::mvt::GROUND_OTHER);
        assert_eq!(road_class("motorway_link"), (ROAD_MOTORWAY, true, false));
        assert_eq!(road_class("trunk"), (ROAD_PRIMARY, false, false));
        assert_eq!(road_class("living_street"), (ROAD_RESIDENTIAL, false, false));
        assert_eq!(road_class("service"), (ROAD_OTHER, false, false));
        assert_eq!(road_class("footway"), (ROAD_OTHER, false, true));
        assert!(is_lit("yes") && is_lit("24/7") && !is_lit("no") && !is_lit(""));
        // Roads stack above all areas, major on top.
        assert!(road_level(ROAD_OTHER) > 7 && road_level(ROAD_MOTORWAY) > road_level(ROAD_PRIMARY));
    }

    #[test]
    fn coarser_bands_drop_minor_roads_and_small_areas() {
        let p = proj();
        let mut minor = road(ROAD_OTHER, false, false, vec![[100.0, 100.0], [3000.0, 100.0]]);
        minor.minor = true;
        let g = GroundFeatures {
            areas: vec![GroundArea { class: GROUND_GRASS, level: 5, id: 3, rings: vec![sq(10.0, 10.0, 50.0)], tint: 0, sub: 0, cut: false }],
            roads: vec![minor, road(ROAD_OTHER, false, false, vec![[100.0, 900.0], [3000.0, 900.0]])],
            ..Default::default()
        };
        let t = |r| mesh_ground(&g, &p, r, &dp()).triangle_count();
        assert!(t(0) > t(1) && t(1) > t(2));
        assert_eq!(t(2), 0);
    }

    /// Water on a slope drapes like every other area: no vertex under the terrain surface, none
    /// above it by more than the drape margin and the level step (the default `water_snap_m` is 0).
    #[test]
    fn water_drapes_on_a_slope() {
        use crate::geo::Frame;
        use crate::profile::TerrainProfile;
        use crate::terrain::Terrain;
        let tp = TerrainProfile { enabled: true, flat_radius_m: 0.0, flat_blend_m: 0.0, ..TerrainProfile::default() };
        assert_eq!(tp.water_snap_m, 0.0);
        let (lat0, lon0) = (VILNIUS.0 + 0.05, VILNIUS.1 + 0.06);
        let key = tile_for(lat0, lon0, 16);
        let (lat, lon) = key.center_lat_lon();
        let frame = Frame::new(lat, lon);
        let terrain = Terrain::new(&tp, lat, lon);
        let p = TileProjector::new(key, frame, 4096.0).with_terrain(terrain.clone());
        let g = GroundFeatures {
            areas: vec![GroundArea { class: GROUND_WATER, level: 7, id: 1, rings: vec![sq(500.0, 500.0, 3000.0)], tint: 0, sub: 0, cut: false }],
            roads: vec![],
            ..Default::default()
        };
        let prof = dp();
        let ring = 0;
        let m = mesh_ground(&g, &p, ring, &prof);
        assert!(!m.is_empty());
        let lift = terrain.drape_margin_m(ring) as f64;
        let step = prof.ground_params(ring).unwrap().step_m as f64;
        let (mut lo, mut hi, mut slope) = (f64::MAX, f64::MIN, 0.0f64);
        for v in &m.positions {
            let (la, lo_) = frame.lat_lon(v[0] as f64, -v[2] as f64);
            let h = terrain.height(la, lo_);
            let above = v[1] as f64 - h;
            lo = lo.min(above);
            hi = hi.max(above);
            slope = slope.max(h);
        }
        assert!(lo >= lift - 0.01, "a water vertex is {lo} m above the terrain, margin {lift}");
        assert!(hi <= lift + 7.0 * step + 0.01, "a water vertex is {hi} m above the terrain");
        let (a, b) = (terrain.height(lat, lon), slope);
        assert!(b > a + 1.0 || a > 1.0, "the tile has no relief to test against");
    }

    /// Drape cost on the default relief (amplitude 450, 8 km from the flat zone): a forest
    /// polygon over (almost) a whole tile stays within a per-band triangle budget, so the
    /// drape tolerance really follows the band (a fixed 0.1 m made R3 tiles ~25 000 triangles).
    #[test]
    fn drape_triangles_per_band_on_the_default_relief() {
        use crate::geo::Frame;
        use crate::profile::TerrainProfile;
        use crate::terrain::Terrain;
        let prof = dp();
        let tp = TerrainProfile { enabled: true, ..TerrainProfile::default() };
        let origin = (VILNIUS.0, VILNIUS.1);
        let terrain = Terrain::new(&tp, origin.0, origin.1);
        // Same spot (~8 km north-east of the origin) at the zoom of each band.
        let (lat0, lon0) = (origin.0 + 0.05, origin.1 + 0.1);
        let mut counts = Vec::new();
        for ring in 0..4usize {
            let key = tile_for(lat0, lon0, 16 - ring as u8);
            let p = TileProjector::new(key, Frame::new(origin.0, origin.1), 4096.0).with_terrain(terrain.clone());
            let g = GroundFeatures {
                areas: vec![GroundArea { class: crate::mvt::GROUND_FOREST, level: 6, id: 1, rings: vec![sq(100.0, 100.0, 3896.0)], tint: 0, sub: 0, cut: false }],
                roads: vec![],
                ..Default::default()
            };
            let m = mesh_ground(&g, &p, ring, &prof);
            counts.push(m.triangle_count());
        }
        eprintln!("drape triangles per band R0..R3: {counts:?}");
        // Measured 59 / 82 / 104 / 162; a fixed 0.1 m tolerance gave 155 / 605 / 2539 / 11175.
        const CAP: [usize; 4] = [120, 250, 400, 700];
        for (r, (&c, &cap)) in counts.iter().zip(CAP.iter()).enumerate() {
            assert!(c <= cap, "R{r}: {c} triangles");
        }
    }

    #[test]
    fn deterministic() {
        let p = proj();
        let mut g = water_with_island();
        g.roads.push(road(ROAD_SECONDARY, false, true, vec![[0.0, 0.0], [1200.0, 3000.0], [4096.0, 3500.0]]));
        assert_eq!(mesh_ground(&g, &p, 0, &dp()), mesh_ground(&g, &p, 0, &dp()));
        assert_eq!(mesh_ground(&g, &p, 1, &dp()), mesh_ground(&g, &p, 1, &dp()));
    }
}
