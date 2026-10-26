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
//! * Heights: every vertex sits at `geo::ground_height` (area vertices at their own point, strip
//!   vertices at their centerline point) plus a small per-level lift against z-fighting:
//!   `level * step(ring)`. Levels bottom to top: 0 nature_reserve, 1 other landuse,
//!   2 residential, 3 industrial, 4 commercial, 5 grass/park, 6 forest, 7 water, 8..13 roads
//!   (other .. motorway). The step grows with the band (seen from farther away).
//! * COLOR (contract): `r` = class / 16, `g` = lit (roads with lit=yes), `b` = road class / 8,
//!   `a` = 1. NORMAL = +Y. No UV. Triangles are clockwise seen from above (Godot front face).
//!
//! Coarser bands are simplified more (`ground_params`): bigger DP tolerances, small areas
//! dropped, minor paths dropped at R1, all "other" roads dropped at R2. R3 has no ground.

use crate::mesher::{clip_ring, simplify_ring, ClipRect, MeshData};
use crate::mvt::{
    ring_area2, GroundFeatures, RoadLine, TileProjector, GROUND_ROAD, ROAD_MOTORWAY, ROAD_OTHER, ROAD_PRIMARY,
    ROAD_RESIDENTIAL, ROAD_SECONDARY, ROAD_TERTIARY,
};

/// Per-band ground meshing parameters.
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

pub const R0_GROUND: GroundParams =
    GroundParams { area_dp_m: 0.5, min_area_m2: 0.0, road_dp_m: 0.5, max_road_class: ROAD_OTHER, minor_paths: true, step_m: 0.02 };
pub const R1_GROUND: GroundParams = GroundParams {
    area_dp_m: 2.0,
    min_area_m2: 200.0,
    road_dp_m: 2.0,
    max_road_class: ROAD_OTHER,
    minor_paths: false,
    step_m: 0.06,
};
pub const R2_GROUND: GroundParams = GroundParams {
    area_dp_m: 6.0,
    min_area_m2: 2500.0,
    road_dp_m: 5.0,
    max_road_class: ROAD_RESIDENTIAL,
    minor_paths: false,
    step_m: 0.15,
};

/// Ground parameters per ring; `None` for R3 (no ground).
pub fn ground_params(ring: usize) -> Option<GroundParams> {
    match ring {
        0 => Some(R0_GROUND),
        1 => Some(R1_GROUND),
        2 => Some(R2_GROUND),
        _ => None,
    }
}

/// Full strip width (m) per road class; `_link` roads get 60 % of it (0.1 m steps), at least 3 m.
pub fn road_width_m(class: u8, link: bool) -> f64 {
    let w = match class {
        ROAD_MOTORWAY => 14.0,
        ROAD_PRIMARY => 10.0,
        ROAD_SECONDARY => 8.0,
        ROAD_TERTIARY | ROAD_RESIDENTIAL => 6.0,
        _ => 3.0,
    };
    if link {
        ((w * 6.0f64).round() / 10.0).max(3.0)
    } else {
        w
    }
}

/// Stacking level of a road strip: above every area level (0..7), major roads on top.
pub fn road_level(class: u8) -> u8 {
    8 + (ROAD_OTHER - class.clamp(ROAD_MOTORWAY, ROAD_OTHER))
}

/// Contract COLOR of an area vertex.
pub fn area_color(class: u8) -> [f32; 4] {
    [class as f32 / 16.0, 0.0, 0.0, 1.0]
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
    holes: Vec<usize>,
    run: Vec<[f64; 2]>,
    pts: Vec<[f64; 2]>,
    ph: Vec<f64>,
}

/// Mesh a tile's ground features for `ring` into one merged buffer (empty for R3).
pub fn mesh_ground(g: &GroundFeatures, proj: &TileProjector, ring: usize) -> MeshData {
    let Some(p) = ground_params(ring) else { return MeshData::default() };
    let mut m = MeshData::default();
    let mut s = Scratch::default();
    let rect = ClipRect::tile(proj.extent());
    let m_per_px = proj.m_per_px();
    let min_area_px2 = p.min_area_m2 / (m_per_px * m_per_px);
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
        let y = a.level as f32 * p.step_m;
        emit_area(&mut m, &mut s, &a.rings, proj, &rect, p.area_dp_m / m_per_px, y, area_color(a.class));
    }
    for r in &g.roads {
        if r.class > p.max_road_class || (r.minor && !p.minor_paths) {
            continue;
        }
        emit_road(&mut m, &mut s, r, proj, &rect, p.road_dp_m / m_per_px, road_level(r.class) as f32 * p.step_m);
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

#[allow(clippy::too_many_arguments)]
fn emit_area(
    m: &mut MeshData,
    s: &mut Scratch,
    rings: &[Vec<[f64; 2]>],
    proj: &TileProjector,
    rect: &ClipRect,
    tol_px: f64,
    lift: f32,
    color: [f32; 4],
) {
    let bb = bbox(&rings[0]);
    if bb[2] <= rect.x0 || bb[0] >= rect.x1 || bb[3] <= rect.y0 || bb[1] >= rect.y1 {
        return;
    }
    s.flat.clear();
    s.hs.clear();
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
            let (en, h) = proj.project_h(s.dedup[i]);
            s.flat.push(en[0]);
            s.flat.push(en[1]);
            s.hs.push(h);
        }
        let (ring_en, _) = s.flat[start..].as_chunks::<2>();
        if ring_area2(ring_en).abs() < 1e-3 {
            // Degenerate (e.g. a sliver along a clip line): drop it.
            s.flat.truncate(start);
            s.hs.truncate(start / 2);
            if ri == 0 {
                return;
            }
            continue;
        }
        if ri > 0 {
            s.holes.push(start / 2);
        }
    }
    let Ok(tris) = earcutr::earcut(&s.flat, &s.holes, 2) else { return };
    if tris.is_empty() {
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

fn emit_road(m: &mut MeshData, s: &mut Scratch, r: &RoadLine, proj: &TileProjector, rect: &ClipRect, tol_px: f64, lift: f32) {
    let hw = road_width_m(r.class, r.link) * 0.5;
    let color = road_color(r.class, r.lit);
    for line in &r.lines {
        // Split into runs inside the tile.
        s.run.clear();
        for w in line.windows(2) {
            let (a, b) = (w[0], w[1]);
            let Some((t0, t1)) = clip_segment(a, b, rect) else {
                flush_run(m, s, proj, tol_px, hw, lift, color);
                continue;
            };
            let lerp = |t: f64| if t == 0.0 { a } else if t == 1.0 { b } else { [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t] };
            let (p, q) = (lerp(t0), lerp(t1));
            if s.run.last() != Some(&p) {
                flush_run(m, s, proj, tol_px, hw, lift, color);
                s.run.push(p);
            }
            s.run.push(q);
            if t1 < 1.0 {
                flush_run(m, s, proj, tol_px, hw, lift, color);
            }
        }
        flush_run(m, s, proj, tol_px, hw, lift, color);
    }
}

/// Simplify, project and buffer the current run, then clear it.
fn flush_run(m: &mut MeshData, s: &mut Scratch, proj: &TileProjector, tol_px: f64, hw: f64, lift: f32, color: [f32; 4]) {
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
    for &i in &s.kept {
        let (en, h) = proj.project_h(s.run[i]);
        if s.pts.last().is_some_and(|l: &[f64; 2]| (l[0] - en[0]).hypot(l[1] - en[1]) < 0.01) {
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
        let l = dx.hypot(dy).max(1e-9);
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
        let mlen = mv[0].hypot(mv[1]);
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
    use crate::mvt::{GroundArea, GROUND_GRASS, GROUND_WATER};

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
            areas: vec![GroundArea { class: GROUND_WATER, level: 7, id: 1, rings: vec![sq(1000.0, 1000.0, 2000.0), hole] }],
            roads: vec![],
        }
    }

    #[test]
    fn triangulation_respects_holes() {
        let p = proj();
        let mpp = p.m_per_px();
        let m = mesh_ground(&water_with_island(), &p, 0);
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
        assert!(m.positions.iter().all(|v| (v[1] - 7.0 * R0_GROUND.step_m).abs() < 1e-6));
        // R3: nothing.
        assert!(mesh_ground(&water_with_island(), &p, 3).is_empty());
    }

    #[test]
    fn areas_are_clipped_to_the_tile() {
        let p = proj();
        let g = GroundFeatures {
            areas: vec![GroundArea { class: GROUND_GRASS, level: 5, id: 2, rings: vec![sq(-1000.0, -1000.0, 6000.0)] }],
            roads: vec![],
        };
        let m = mesh_ground(&g, &p, 0);
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
            assert_eq!(road_width_m(class, link), w);
            // Straight east-west road through the whole tile (clipped at both edges).
            let g = GroundFeatures { areas: vec![], roads: vec![road(class, link, false, vec![[-200.0, 2048.0], [4300.0, 2048.0]])] };
            let m = mesh_ground(&g, &p, 0);
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
        use crate::mvt::{ground_area_class, is_lit, road_class};
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
            areas: vec![GroundArea { class: GROUND_GRASS, level: 5, id: 3, rings: vec![sq(10.0, 10.0, 50.0)] }],
            roads: vec![minor, road(ROAD_OTHER, false, false, vec![[100.0, 900.0], [3000.0, 900.0]])],
        };
        let t = |r| mesh_ground(&g, &p, r).triangle_count();
        assert!(t(0) > t(1) && t(1) > t(2));
        assert_eq!(t(2), 0);
    }

    #[test]
    fn deterministic() {
        let p = proj();
        let mut g = water_with_island();
        g.roads.push(road(ROAD_SECONDARY, false, true, vec![[0.0, 0.0], [1200.0, 3000.0], [4096.0, 3500.0]]));
        assert_eq!(mesh_ground(&g, &p, 0), mesh_ground(&g, &p, 0));
        assert_eq!(mesh_ground(&g, &p, 1), mesh_ground(&g, &p, 1));
    }
}
