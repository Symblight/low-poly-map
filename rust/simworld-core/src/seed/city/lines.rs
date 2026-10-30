//! Polyline helpers of the city generator (plane metres; only `+ - * / sqrt`).

use crate::seed::geom::point_segment_dist;

/// Axis-aligned box [min_x, min_y, max_x, max_y] of points (empty = inverted infinite box).
pub fn bbox(pts: &[[f64; 2]]) -> [f64; 4] {
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for p in pts {
        b = [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])];
    }
    b
}

#[inline]
pub fn boxes_overlap(a: [f64; 4], b: [f64; 4]) -> bool {
    a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3]
}

#[inline]
pub fn grow(b: [f64; 4], m: f64) -> [f64; 4] {
    [b[0] - m, b[1] - m, b[2] + m, b[3] + m]
}

#[inline]
pub fn lerp(a: [f64; 2], b: [f64; 2], t: f64) -> [f64; 2] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

#[inline]
pub fn len(a: [f64; 2], b: [f64; 2]) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    (dx * dx + dy * dy).sqrt()
}

/// Distance from a point to a polyline.
pub fn dist_to_line(p: [f64; 2], line: &[[f64; 2]]) -> f64 {
    line.windows(2).map(|w| point_segment_dist(p, w[0], w[1])).fold(f64::INFINITY, f64::min)
}

/// Total length of a polyline.
pub fn line_len(line: &[[f64; 2]]) -> f64 {
    line.windows(2).map(|w| len(w[0], w[1])).sum()
}

/// The point between `a` (ok) and `b` (not ok) where `ok` changes, by bisection (16 steps).
fn boundary(a: [f64; 2], b: [f64; 2], ok: &dyn Fn([f64; 2]) -> bool) -> [f64; 2] {
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..16 {
        let m = (lo + hi) * 0.5;
        if ok(lerp(a, b, m)) {
            lo = m;
        } else {
            hi = m;
        }
    }
    lerp(a, b, lo)
}

/// The runs of a polyline where `ok` holds, each ending on the bisected change point. Runs
/// shorter than `min_len` are dropped.
pub fn ok_runs(line: &[[f64; 2]], ok: &dyn Fn([f64; 2]) -> bool, min_len: f64) -> Vec<Vec<[f64; 2]>> {
    let mut out = Vec::new();
    let mut cur: Vec<[f64; 2]> = Vec::new();
    let mut prev_ok = false;
    for (i, &p) in line.iter().enumerate() {
        let now = ok(p);
        if i > 0 && now != prev_ok {
            let q = if prev_ok { boundary(line[i - 1], p, ok) } else { boundary(p, line[i - 1], ok) };
            if prev_ok {
                cur.push(q);
                out.push(std::mem::take(&mut cur));
            } else {
                cur.push(q);
            }
        }
        if now {
            cur.push(p);
        }
        prev_ok = now;
    }
    if cur.len() >= 2 {
        out.push(cur);
    }
    out.retain(|r| r.len() >= 2 && line_len(r) >= min_len);
    out
}

/// Densify a polyline so no segment is longer than `step`.
pub fn densify(line: &[[f64; 2]], step: f64) -> Vec<[f64; 2]> {
    let mut out = Vec::with_capacity(line.len() * 2);
    for w in line.windows(2) {
        let n = (len(w[0], w[1]) / step).ceil().max(1.0) as usize;
        for k in 0..n {
            out.push(lerp(w[0], w[1], k as f64 / n as f64));
        }
    }
    if let Some(l) = line.last() {
        out.push(*l);
    }
    out
}

/// Bilinear point of a block quad (c00, c10, c11, c01).
#[inline]
pub fn bilinear(c: &[[f64; 2]; 4], s: f64, t: f64) -> [f64; 2] {
    let a = lerp(c[0], c[1], s);
    let b = lerp(c[3], c[2], s);
    lerp(a, b, t)
}
