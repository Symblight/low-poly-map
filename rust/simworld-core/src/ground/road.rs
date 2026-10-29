//! Roads: clipped runs buffered into flat strips (miter / bevel joins, butt caps).

use super::drape::densify_run;
use super::{push_v, road_color, tri_up, Scratch};
use crate::mesher::{simplify_ring, ClipRect, MeshData};
use crate::mvt::{RoadLine, TileProjector};

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
pub(super) fn emit_road(m: &mut MeshData, s: &mut Scratch, r: &RoadLine, proj: &TileProjector, rect: &ClipRect, tol_px: f64, lift: f32, hw: f64, field: Option<f64>, step: f64) {
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
