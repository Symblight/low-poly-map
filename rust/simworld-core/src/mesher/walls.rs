//! Walls: wall runs between corners, window-column UV, flat and terrain-following quads.

use super::{MeshData, Scratch};
use crate::mvt::TileProjector;

#[inline]
pub(super) fn gv(p: [f64; 2], y: f32) -> [f32; 3] {
    [p[0] as f32, y, -(p[1] as f32)]
}

/// Per-building wall parameters of [`emit_polygon`].
#[derive(Clone, Copy, Debug)]
pub(super) struct WallLayout {
    /// Window column width (m) when the building's walls may carry windows in this band
    /// (profile has `windows`, not tagged `windows=no`, band is not R2); `None` = windowless.
    pub(super) window_w: Option<f64>,
    /// `buildings.min_window_wall_m`: lower wall parts are windowless.
    pub(super) min_window_wall_m: f32,
    /// Foundation depth below the exact ground at each wall vertex (m; terrain on:
    /// `Terrain::foundation_m`), 0 = none.
    pub(super) foundation_m: f32,
}

/// Terrain under the walls (api 1.7; `None` = flat world, walls from the base as before).
#[derive(Clone, Copy)]
pub(super) struct GroundWalls<'a> {
    pub(super) proj: &'a TileProjector,
    /// A wall edge longer than `step_m` is subdivided while the ground along it falls more than
    /// this below the straight line between its end heights (the band's drape margin, at least
    /// 0.1 m), so the wall bottom follows a valley instead of bridging it.
    pub(super) tol_m: f64,
    pub(super) step_m: f64,
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
pub(super) fn emit_walls_flat(m: &mut MeshData, s: &mut Scratch, color: [f32; 4], y0: f32, y1: f32, wall_h: f32, floors: f32, window_w: Option<f64>) {
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
pub(super) fn emit_walls_on(
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
        for (i, &eu) in edge_u.iter().enumerate() {
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
                match eu {
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
