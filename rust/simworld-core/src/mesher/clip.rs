//! Geometry helpers (tile pixel space, y down): clip rectangle, ring clipping, Douglas–Peucker.

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

pub(super) fn bbox(polys: &[Vec<Vec<[f64; 2]>>]) -> [f64; 4] {
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
