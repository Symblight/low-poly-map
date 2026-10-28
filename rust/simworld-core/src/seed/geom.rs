//! Small planar geometry helpers for the seed files and the water index: rectangle clipping,
//! ring areas, Douglas-Peucker on rings, point-segment distance. Only `+ - * / sqrt` (and
//! `crate::math::cos` for the meters-per-degree scale), so results are bit-identical everywhere.

/// Meters per degree of latitude on the mean Earth sphere.
pub const M_PER_DEG: f64 = 6_371_008.8 * std::f64::consts::PI / 180.0;

/// Meters per degree of longitude at `lat`.
#[inline]
pub fn m_per_deg_lon(lat: f64) -> f64 {
    M_PER_DEG * crate::math::cos(lat.to_radians())
}

/// Axis-aligned rectangle [min_x, min_y, max_x, max_y] (lon/lat degrees or meters).
pub type Rect = [f64; 4];

/// Sutherland-Hodgman clip of a closed ring (open form: no repeated first point) against a
/// rectangle. Points created on a side get that side's coordinate exactly, so an edge along the
/// rectangle is recognisable by [`on_same_side`]. Concave rings may produce zero-area overlapping
/// edges along the rectangle; they do not change even-odd point tests or areas. Returns an empty
/// ring when nothing is inside.
pub fn clip_ring(ring: &[[f64; 2]], r: Rect) -> Vec<[f64; 2]> {
    let mut cur: Vec<[f64; 2]> = ring.to_vec();
    // (axis, bound, keep >= bound)
    for (axis, bound, ge) in [(0usize, r[0], true), (0, r[2], false), (1, r[1], true), (1, r[3], false)] {
        if cur.is_empty() {
            break;
        }
        let inside = |p: &[f64; 2]| if ge { p[axis] >= bound } else { p[axis] <= bound };
        let mut out = Vec::with_capacity(cur.len() + 4);
        let n = cur.len();
        for i in 0..n {
            let a = cur[(i + n - 1) % n];
            let b = cur[i];
            let (ia, ib) = (inside(&a), inside(&b));
            if ia != ib {
                let t = (bound - a[axis]) / (b[axis] - a[axis]);
                let o = 1 - axis;
                let mut p = [0.0; 2];
                p[axis] = bound;
                p[o] = a[o] + t * (b[o] - a[o]);
                out.push(p);
            }
            if ib {
                out.push(b);
            }
        }
        out.dedup();
        while out.len() > 1 && out.first() == out.last() {
            out.pop();
        }
        cur = out;
    }
    if cur.len() < 3 {
        cur.clear();
    }
    cur
}

/// Both points lie exactly on the same side of `r` (an edge created by clipping, not a real
/// polygon edge).
#[inline]
pub fn on_same_side(a: [f64; 2], b: [f64; 2], r: Rect) -> bool {
    (a[0] == r[0] && b[0] == r[0]) || (a[0] == r[2] && b[0] == r[2]) || (a[1] == r[1] && b[1] == r[1]) || (a[1] == r[3] && b[1] == r[3])
}

/// Signed shoelace area in the ring's own units (positive = counter-clockwise with x east, y north).
pub fn signed_area(ring: &[[f64; 2]]) -> f64 {
    let n = ring.len();
    let mut s = 0.0;
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        s += a[0] * b[1] - b[0] * a[1];
    }
    0.5 * s
}

/// Signed area (m²) of a lon/lat ring (equirectangular at the ring's mean latitude; good to
/// well under 1 % for rings up to a few hundred km).
pub fn signed_area_m2(ring: &[[f64; 2]]) -> f64 {
    if ring.is_empty() {
        return 0.0;
    }
    let lat_mid = ring.iter().map(|p| p[1]).sum::<f64>() / ring.len() as f64;
    signed_area(ring) * M_PER_DEG * m_per_deg_lon(lat_mid)
}

/// Distance from `p` to the segment a-b (same units).
#[inline]
pub fn point_segment_dist(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    let (ex, ey) = (a[0] + t * dx - p[0], a[1] + t * dy - p[1]);
    (ex * ex + ey * ey).sqrt()
}

/// Douglas-Peucker of an open polyline in meters (first and last points kept).
fn dp(pts: &[[f64; 2]], tol: f64, keep: &mut [bool]) {
    let mut stack = vec![(0usize, pts.len() - 1)];
    while let Some((i, j)) = stack.pop() {
        if j <= i + 1 {
            continue;
        }
        let (mut best, mut at) = (-1.0, i);
        for (k, p) in pts.iter().enumerate().take(j).skip(i + 1) {
            let d = point_segment_dist(*p, pts[i], pts[j]);
            if d > best {
                best = d;
                at = k;
            }
        }
        if best > tol {
            keep[at] = true;
            stack.push((i, at));
            stack.push((at, j));
        }
    }
}

/// Douglas-Peucker of a closed lon/lat ring (open form) at `tol_m` meters, in an equirectangular
/// frame at the ring's mean latitude. The ring is split at its first vertex and the vertex
/// farthest from it, so the result does not depend on a start point inside a straight run.
/// Returns fewer than 3 points when the ring collapses.
pub fn simplify_ring(ring: &[[f64; 2]], tol_m: f64) -> Vec<[f64; 2]> {
    let n = ring.len();
    if n < 4 || tol_m <= 0.0 {
        return ring.to_vec();
    }
    let lat_mid = ring.iter().map(|p| p[1]).sum::<f64>() / n as f64;
    let (kx, ky) = (m_per_deg_lon(lat_mid), M_PER_DEG);
    let m: Vec<[f64; 2]> = ring.iter().map(|p| [p[0] * kx, p[1] * ky]).collect();
    let far = (1..n)
        .max_by(|&a, &b| {
            let d2 = |k: usize| {
                let (x, y) = (m[k][0] - m[0][0], m[k][1] - m[0][1]);
                x * x + y * y
            };
            let (da, db) = (d2(a), d2(b));
            da.total_cmp(&db).then(b.cmp(&a))
        })
        .unwrap_or(n / 2);
    let mut keep = vec![false; n + 1];
    keep[0] = true;
    keep[far] = true;
    keep[n] = true;
    let mut closed = m.clone();
    closed.push(m[0]);
    let mut k1 = vec![false; far + 1];
    dp(&closed[..=far], tol_m, &mut k1);
    let mut k2 = vec![false; n - far + 1];
    dp(&closed[far..], tol_m, &mut k2);
    for (i, k) in k1.iter().enumerate() {
        keep[i] |= *k;
    }
    for (i, k) in k2.iter().enumerate() {
        keep[far + i] |= *k;
    }
    (0..n).filter(|&i| keep[i]).map(|i| ring[i]).collect()
}

/// Even-odd point-in-ring test (half-open in y).
pub fn point_in_ring(p: [f64; 2], ring: &[[f64; 2]]) -> bool {
    let n = ring.len();
    let mut inside = false;
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        if (a[1] > p[1]) != (b[1] > p[1]) {
            let x = a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
            if x > p[0] {
                inside = !inside;
            }
        }
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_square_and_concave() {
        let sq = vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        let c = clip_ring(&sq, [5.0, -1.0, 20.0, 5.0]);
        assert!((signed_area(&c) - 25.0).abs() < 1e-12, "{c:?}");
        assert!(clip_ring(&sq, [20.0, 20.0, 30.0, 30.0]).is_empty());
        let full = clip_ring(&sq, [2.0, 2.0, 4.0, 4.0]);
        assert!((signed_area(&full) - 4.0).abs() < 1e-12);
        assert!((0..full.len()).all(|i| on_same_side(full[i], full[(i + 1) % full.len()], [2.0, 2.0, 4.0, 4.0])));
        // U shape clipped across both arms: area kept, point test still right.
        let u = vec![[0.0, 0.0], [9.0, 0.0], [9.0, 9.0], [6.0, 9.0], [6.0, 3.0], [3.0, 3.0], [3.0, 9.0], [0.0, 9.0]];
        let r = [-1.0, 5.0, 10.0, 10.0];
        let c = clip_ring(&u, r);
        assert!((signed_area(&c).abs() - 24.0).abs() < 1e-9, "{}", signed_area(&c));
        assert!(point_in_ring([1.0, 7.0], &c) && point_in_ring([8.0, 7.0], &c) && !point_in_ring([4.5, 7.0], &c));
    }

    #[test]
    fn simplify_keeps_shape_within_tolerance() {
        // A circle of radius ~1 km sampled every degree.
        let (lat0, lon0) = (55.0f64, 24.0f64);
        let r_deg = 1000.0 / M_PER_DEG;
        let ring: Vec<[f64; 2]> = (0..360)
            .map(|i| {
                let a = (i as f64).to_radians();
                [lon0 + r_deg * crate::math::cos(a) / crate::math::cos(lat0.to_radians()), lat0 + r_deg * crate::math::sin(a)]
            })
            .collect();
        let s = simplify_ring(&ring, 50.0);
        assert!(s.len() >= 8 && s.len() < 40, "{}", s.len());
        let a0 = signed_area_m2(&ring);
        let a1 = signed_area_m2(&s);
        assert!((a0 - std::f64::consts::PI * 1e6).abs() / a0 < 0.01, "{a0}");
        assert!((a1 - a0).abs() / a0 < 0.08, "{a1} vs {a0}");
        assert_eq!(simplify_ring(&ring, 50.0), s, "deterministic");
    }
}
