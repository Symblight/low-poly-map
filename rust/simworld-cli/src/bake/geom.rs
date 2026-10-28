//! Plane geometry of the bake in global units.
//!
//! A global unit ("g") is one pixel of a z16 tile at extent 4096, i.e. 2^28 g around the world in
//! Web Mercator, y down. The input archive's z16 vertices are exact integers in it, and every
//! output level is a power-of-two subdivision: a tile at zoom `z` spans `4096 * 2^(16-z)` g.

use geo::{Coord, LineString, MultiPolygon, Polygon};

pub type Pt = [f64; 2];
pub type Ring = Vec<Pt>;
/// Exterior ring first, then holes. Rings are open (no repeated closing point).
pub type Poly = Vec<Ring>;
pub type MPoly = Vec<Poly>;

pub const G_WORLD: f64 = 268_435_456.0; // 2^28
const EARTH_CIRCUMFERENCE_M: f64 = 40_075_016.685_578_49;

/// Tile span in g at zoom `z` (z <= 16).
pub fn tile_span_g(z: u8) -> f64 {
    4096.0 * (1u64 << (16 - z as u32)) as f64
}

/// Latitude (deg) of a global y.
pub fn lat_of(gy: f64) -> f64 {
    let n = std::f64::consts::PI * (1.0 - 2.0 * gy / G_WORLD);
    n.sinh().atan().to_degrees()
}

/// Ground metres per g at global y (Web Mercator scale factor).
pub fn m_per_g(gy: f64) -> f64 {
    EARTH_CIRCUMFERENCE_M * lat_of(gy).to_radians().cos() / G_WORLD
}

/// Signed shoelace area (positive = clockwise on screen, y down; the MVT exterior orientation).
pub fn ring_area(r: &[Pt]) -> f64 {
    let n = r.len();
    let mut a = 0.0;
    for i in 0..n {
        let (p, q) = (r[i], r[(i + 1) % n]);
        a += p[0] * q[1] - q[0] * p[1];
    }
    a * 0.5
}

/// Exterior minus holes (absolute).
pub fn poly_area(p: &Poly) -> f64 {
    match p.split_first() {
        Some((ext, holes)) => ring_area(ext).abs() - holes.iter().map(|h| ring_area(h).abs()).sum::<f64>(),
        None => 0.0,
    }
}

pub fn mpoly_area(m: &MPoly) -> f64 {
    m.iter().map(poly_area).sum()
}

/// Area-weighted centroid of all polygons (holes subtract); vertex mean for degenerate input.
pub fn centroid(m: &MPoly) -> Option<Pt> {
    let (mut a, mut cx, mut cy, mut n, mut sx, mut sy) = (0.0, 0.0, 0.0, 0usize, 0.0, 0.0);
    // Relative to the first vertex: global coordinates are ~1e8, products would lose bits.
    let o = *m.first()?.first()?.first()?;
    for p in m {
        for (k, r) in p.iter().enumerate() {
            let sign = if k == 0 { 1.0 } else { -1.0 };
            let ra = ring_area(r);
            let s = if ra == 0.0 { 0.0 } else { sign * ra.signum() };
            for i in 0..r.len() {
                let (p0, q0) = (r[i], r[(i + 1) % r.len()]);
                let (p, q) = ([p0[0] - o[0], p0[1] - o[1]], [q0[0] - o[0], q0[1] - o[1]]);
                let cr = (p[0] * q[1] - q[0] * p[1]) * s;
                a += cr;
                cx += (p[0] + q[0]) * cr;
                cy += (p[1] + q[1]) * cr;
                n += 1;
                sx += p[0];
                sy += p[1];
            }
        }
    }
    if n == 0 {
        return None;
    }
    Some(if a.abs() > 1e-9 { [o[0] + cx / (3.0 * a), o[1] + cy / (3.0 * a)] } else { [o[0] + sx / n as f64, o[1] + sy / n as f64] })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    pub const EMPTY: Rect = Rect { x0: f64::INFINITY, y0: f64::INFINITY, x1: f64::NEG_INFINITY, y1: f64::NEG_INFINITY };
    /// Tile `z/x/y` grown by `buffer` g on every side.
    pub fn tile(z: u8, x: u32, y: u32, buffer: f64) -> Rect {
        let s = tile_span_g(z);
        Rect { x0: x as f64 * s - buffer, y0: y as f64 * s - buffer, x1: (x + 1) as f64 * s + buffer, y1: (y + 1) as f64 * s + buffer }
    }
    pub fn add(&mut self, p: Pt) {
        self.x0 = self.x0.min(p[0]);
        self.y0 = self.y0.min(p[1]);
        self.x1 = self.x1.max(p[0]);
        self.y1 = self.y1.max(p[1]);
    }
    pub fn of_mpoly(m: &MPoly) -> Rect {
        let mut r = Rect::EMPTY;
        m.iter().flatten().flatten().for_each(|p| r.add(*p));
        r
    }
    pub fn of_lines(l: &[Ring]) -> Rect {
        let mut r = Rect::EMPTY;
        l.iter().flatten().for_each(|p| r.add(*p));
        r
    }
    pub fn grow(&self, d: f64) -> Rect {
        Rect { x0: self.x0 - d, y0: self.y0 - d, x1: self.x1 + d, y1: self.y1 + d }
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x0 <= o.x1 && o.x0 <= self.x1 && self.y0 <= o.y1 && o.y0 <= self.y1
    }
    pub fn contains(&self, p: Pt) -> bool {
        p[0] >= self.x0 && p[0] <= self.x1 && p[1] >= self.y0 && p[1] <= self.y1
    }
    /// Tiles of zoom `z` whose rectangle grown by `buffer` meets this one.
    pub fn tiles(&self, z: u8, buffer: f64) -> impl Iterator<Item = (u32, u32)> {
        let s = tile_span_g(z);
        let n = (1u64 << z) as f64;
        let lo = |v: f64| ((v - buffer) / s).floor().clamp(0.0, n - 1.0) as u32;
        let hi = |v: f64| ((v + buffer) / s).floor().clamp(0.0, n - 1.0) as u32;
        let (x0, x1, y0, y1) = (lo(self.x0), hi(self.x1), lo(self.y0), hi(self.y1));
        (y0..=y1).flat_map(move |y| (x0..=x1).map(move |x| (x, y)))
    }
}

/// Sutherland–Hodgman clip of one ring to a rectangle (may leave zero-width bridges along the
/// rectangle's edge for concave rings, like other tile encoders; they lie on the buffer line).
pub fn clip_ring(r: &[Pt], c: &Rect) -> Ring {
    let mut out: Ring = r.to_vec();
    for edge in 0..4 {
        if out.is_empty() {
            break;
        }
        let inside = |p: Pt| match edge {
            0 => p[0] >= c.x0,
            1 => p[0] <= c.x1,
            2 => p[1] >= c.y0,
            _ => p[1] <= c.y1,
        };
        let cut = |a: Pt, b: Pt| -> Pt {
            let t = match edge {
                0 => (c.x0 - a[0]) / (b[0] - a[0]),
                1 => (c.x1 - a[0]) / (b[0] - a[0]),
                2 => (c.y0 - a[1]) / (b[1] - a[1]),
                _ => (c.y1 - a[1]) / (b[1] - a[1]),
            };
            let mut p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
            match edge {
                0 => p[0] = c.x0,
                1 => p[0] = c.x1,
                2 => p[1] = c.y0,
                _ => p[1] = c.y1,
            }
            p
        };
        let input = std::mem::take(&mut out);
        let n = input.len();
        for i in 0..n {
            let (a, b) = (input[(i + n - 1) % n], input[i]);
            match (inside(a), inside(b)) {
                (true, true) => out.push(b),
                (true, false) => out.push(cut(a, b)),
                (false, true) => {
                    out.push(cut(a, b));
                    out.push(b);
                }
                (false, false) => {}
            }
        }
    }
    out
}

/// Clip every polygon to `c`; drops rings that vanish (an exterior that vanishes drops its holes).
pub fn clip_mpoly(m: &MPoly, c: &Rect) -> MPoly {
    let mut out = Vec::new();
    for p in m {
        let Some(ext) = p.first() else { continue };
        let e = clip_ring(ext, c);
        if e.len() < 3 || ring_area(&e).abs() <= 1e-9 {
            continue;
        }
        let mut q = vec![e];
        for h in &p[1..] {
            let h = clip_ring(h, c);
            if h.len() >= 3 && ring_area(&h).abs() > 1e-9 {
                q.push(h);
            }
        }
        out.push(q);
    }
    out
}

/// Liang–Barsky clip of a polyline to `c`: the pieces inside.
pub fn clip_line(l: &[Pt], c: &Rect) -> Vec<Ring> {
    let mut out: Vec<Ring> = Vec::new();
    let mut cur: Ring = Vec::new();
    for w in l.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let (mut t0, mut t1) = (0.0f64, 1.0f64);
        let mut ok = true;
        for (p, q) in [(-dx, a[0] - c.x0), (dx, c.x1 - a[0]), (-dy, a[1] - c.y0), (dy, c.y1 - a[1])] {
            if p == 0.0 {
                if q < 0.0 {
                    ok = false;
                    break;
                }
            } else {
                let r = q / p;
                if p < 0.0 {
                    t0 = t0.max(r);
                } else {
                    t1 = t1.min(r);
                }
            }
        }
        if !ok || t0 > t1 {
            if cur.len() >= 2 {
                out.push(std::mem::take(&mut cur));
            }
            cur.clear();
            continue;
        }
        let p0 = [a[0] + dx * t0, a[1] + dy * t0];
        let p1 = [a[0] + dx * t1, a[1] + dy * t1];
        if cur.last() != Some(&p0) {
            if cur.len() >= 2 {
                out.push(std::mem::take(&mut cur));
            }
            cur.clear();
            cur.push(p0);
        }
        cur.push(p1);
        if t1 < 1.0 {
            out.push(std::mem::take(&mut cur));
        }
    }
    if cur.len() >= 2 {
        out.push(cur);
    }
    out
}

/// Douglas–Peucker on a closed ring (open representation in, open out). Rings that collapse
/// below three points come back empty.
pub fn simplify_ring(r: &[Pt], tol: f64) -> Ring {
    if tol <= 0.0 || r.len() <= 4 {
        return r.to_vec();
    }
    // Split at the vertex farthest from vertex 0 so both halves are open chains.
    let far = (1..r.len()).max_by(|&a, &b| d2(r[0], r[a]).total_cmp(&d2(r[0], r[b]))).unwrap_or(0);
    let mut a: Ring = r[..=far].to_vec();
    let mut b: Ring = r[far..].to_vec();
    b.push(r[0]);
    a = dp(&a, tol);
    b = dp(&b, tol);
    a.pop();
    b.pop();
    a.extend(b);
    if a.len() < 3 {
        Vec::new()
    } else {
        a
    }
}

pub fn simplify_line(l: &[Pt], tol: f64) -> Ring {
    if tol <= 0.0 || l.len() <= 2 {
        l.to_vec()
    } else {
        dp(l, tol)
    }
}

fn d2(a: Pt, b: Pt) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)
}

fn seg_d2(p: Pt, a: Pt, b: Pt) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let l2 = dx * dx + dy * dy;
    if l2 == 0.0 {
        return d2(p, a);
    }
    let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2).clamp(0.0, 1.0);
    d2(p, [a[0] + dx * t, a[1] + dy * t])
}

fn dp(l: &[Pt], tol: f64) -> Ring {
    let n = l.len();
    let mut keep = vec![false; n];
    keep[0] = true;
    keep[n - 1] = true;
    let mut stack = vec![(0usize, n - 1)];
    let t2 = tol * tol;
    while let Some((s, e)) = stack.pop() {
        let (mut best, mut bi) = (0.0, 0);
        for i in s + 1..e {
            let d = seg_d2(l[i], l[s], l[e]);
            if d > best {
                best = d;
                bi = i;
            }
        }
        if best > t2 {
            keep[bi] = true;
            stack.push((s, bi));
            stack.push((bi, e));
        }
    }
    l.iter().zip(keep).filter(|(_, k)| *k).map(|(p, _)| *p).collect()
}

/// Simplify every ring; drops rings that collapse (an exterior that collapses drops its
/// polygon) and holes smaller than `tol` squared.
pub fn simplify_mpoly(m: &MPoly, tol: f64) -> MPoly {
    let mut out = Vec::new();
    for p in m {
        let Some(ext) = p.first() else { continue };
        let e = simplify_ring(ext, tol);
        if e.len() < 3 || ring_area(&e) == 0.0 {
            continue;
        }
        let mut q = vec![e];
        for h in &p[1..] {
            let h = simplify_ring(h, tol);
            if h.len() >= 3 && ring_area(&h).abs() > tol * tol {
                q.push(h);
            }
        }
        if poly_area(&q) > 0.0 {
            out.push(q);
        }
    }
    out
}

/// Even-odd point in rings.
pub fn point_in_poly(p: Pt, rings: &[Ring]) -> bool {
    let mut inside = false;
    for r in rings {
        let n = r.len();
        if n < 3 {
            continue;
        }
        let mut j = n - 1;
        for i in 0..n {
            let (a, b) = (r[i], r[j]);
            if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
                inside = !inside;
            }
            j = i;
        }
    }
    inside
}

/// Minimum distance between two polygon sets (0 when they overlap).
pub fn mpoly_distance(a: &MPoly, b: &MPoly) -> f64 {
    let first = |m: &MPoly| m.first().and_then(|p| p.first()).and_then(|r| r.first()).copied();
    if let (Some(pa), Some(pb)) = (first(a), first(b)) {
        if b.iter().any(|p| point_in_poly(pa, p)) || a.iter().any(|p| point_in_poly(pb, p)) {
            return 0.0;
        }
    }
    let mut best = f64::INFINITY;
    for ra in a.iter().flatten() {
        for rb in b.iter().flatten() {
            for i in 0..ra.len() {
                let (p, q) = (ra[i], ra[(i + 1) % ra.len()]);
                for j in 0..rb.len() {
                    let (r, s) = (rb[j], rb[(j + 1) % rb.len()]);
                    if segments_cross(p, q, r, s) {
                        return 0.0;
                    }
                    best = best.min(seg_d2(p, r, s)).min(seg_d2(q, r, s)).min(seg_d2(r, p, q)).min(seg_d2(s, p, q));
                }
            }
        }
    }
    best.sqrt()
}

fn orient(a: Pt, b: Pt, c: Pt) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

fn segments_cross(p: Pt, q: Pt, r: Pt, s: Pt) -> bool {
    let (d1, d2_, d3, d4) = (orient(r, s, p), orient(r, s, q), orient(p, q, r), orient(p, q, s));
    ((d1 > 0.0 && d2_ < 0.0) || (d1 < 0.0 && d2_ > 0.0)) && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
}

// --- geo interop (local frame: subtract an origin so i_overlay keeps full precision) ----------

pub fn to_geo(m: &MPoly, o: Pt, scale: f64) -> MultiPolygon<f64> {
    let ring = |r: &Ring| {
        let mut cs: Vec<Coord<f64>> = r.iter().map(|p| Coord { x: (p[0] - o[0]) * scale, y: (p[1] - o[1]) * scale }).collect();
        if let Some(f) = cs.first().copied() {
            cs.push(f);
        }
        LineString::new(cs)
    };
    MultiPolygon::new(
        m.iter()
            .filter(|p| !p.is_empty())
            .map(|p| Polygon::new(ring(&p[0]), p[1..].iter().map(ring).collect()))
            .collect(),
    )
}

pub fn from_geo(m: &MultiPolygon<f64>, o: Pt, scale: f64) -> MPoly {
    let ring = |l: &LineString<f64>| {
        let mut r: Ring = l.0.iter().map(|c| [c.x / scale + o[0], c.y / scale + o[1]]).collect();
        if r.len() > 1 && r.first() == r.last() {
            r.pop();
        }
        r
    };
    m.0.iter()
        .map(|p| {
            let mut q = vec![ring(p.exterior())];
            q.extend(p.interiors().iter().map(ring));
            q
        })
        .filter(|q: &Poly| q[0].len() >= 3)
        .collect()
}

/// Union of polygon sets (any orientation in; the output orientation is fixed by the encoder).
pub fn union(parts: &[MPoly]) -> MPoly {
    let Some(o) = parts.iter().flatten().flatten().flatten().next().copied() else { return Vec::new() };
    let gs: Vec<Polygon<f64>> = parts
        .iter()
        .flat_map(|m| to_geo(m, o, 1.0).0)
        .map(|p| {
            use geo::orient::{Direction, Orient};
            p.orient(Direction::Default)
        })
        .collect();
    from_geo(&geo::unary_union(&gs), o, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sq(x0: f64, y0: f64, s: f64) -> Ring {
        vec![[x0, y0], [x0 + s, y0], [x0 + s, y0 + s], [x0, y0 + s]]
    }

    #[test]
    fn clip_ring_to_rect_keeps_the_inside() {
        let r = sq(-5.0, -5.0, 10.0);
        let c = clip_ring(&r, &Rect { x0: 0.0, y0: 0.0, x1: 100.0, y1: 100.0 });
        assert!((ring_area(&c).abs() - 25.0).abs() < 1e-9);
        assert!(clip_ring(&r, &Rect { x0: 50.0, y0: 50.0, x1: 60.0, y1: 60.0 }).is_empty());
    }

    #[test]
    fn clip_line_splits_at_the_rect() {
        let l = vec![[-10.0, 5.0], [5.0, 5.0], [5.0, 20.0], [5.0, 8.0], [20.0, 8.0]];
        let c = clip_line(&l, &Rect { x0: 0.0, y0: 0.0, x1: 10.0, y1: 10.0 });
        assert_eq!(c.len(), 2);
        assert_eq!(c[0], vec![[0.0, 5.0], [5.0, 5.0], [5.0, 10.0]]);
        assert_eq!(c[1], vec![[5.0, 10.0], [5.0, 8.0], [10.0, 8.0]]);
    }

    #[test]
    fn simplify_drops_collinear_and_keeps_corners() {
        let r = vec![[0.0, 0.0], [5.0, 0.01], [10.0, 0.0], [10.0, 10.0], [5.0, 10.0], [0.0, 10.0]];
        let s = simplify_ring(&r, 0.5);
        assert_eq!(s.len(), 4);
        assert!((ring_area(&s).abs() - 100.0).abs() < 1e-9);
    }

    #[test]
    fn union_merges_overlaps_and_distance_measures_gaps() {
        let a = vec![vec![sq(0.0, 0.0, 10.0)]];
        let b = vec![vec![sq(5.0, 0.0, 10.0)]];
        let u = union(&[a.clone(), b]);
        assert_eq!(u.len(), 1);
        assert!((mpoly_area(&u) - 150.0).abs() < 1e-6);
        let c = vec![vec![sq(13.0, 0.0, 10.0)]];
        assert!((mpoly_distance(&a, &c) - 3.0).abs() < 1e-9);
        assert_eq!(mpoly_distance(&a, &vec![vec![sq(2.0, 2.0, 1.0)]]), 0.0);
    }

    #[test]
    fn mercator_scale_in_vilnius() {
        // z14 tile at Vilnius: ~1.4 km wide.
        let gy = (G_WORLD / 2.0) * (1.0 - (54.687f64.to_radians().tan() + 1.0 / 54.687f64.to_radians().cos()).ln() / std::f64::consts::PI);
        assert!((lat_of(gy) - 54.687).abs() < 1e-9);
        let w = tile_span_g(14) * m_per_g(gy);
        assert!((w - 1415.0).abs() < 5.0, "{w}");
    }
}
