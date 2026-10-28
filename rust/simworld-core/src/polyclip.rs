//! Clip of a polygon with holes against an axis-aligned rectangle (Weiler–Atherton for the rect
//! case). Plain per-ring Sutherland–Hodgman clipping is exact for every ring on its own, but a
//! hole that crosses the rect boundary then becomes a ring lying on the outer ring's boundary,
//! which earcut does not read as a notch (the hole got covered). Here every ring is clipped with
//! [`clip_ring`] (orientation fixed first: outer positive, holes negative), the edges lying on
//! the rect boundary are dropped, the remaining chains (each running from boundary to boundary
//! with the polygon interior on its left) are stitched along the rect boundary in the positive
//! direction, and the holes that stay closed are assigned to the piece that contains them.
//!
//! Pure f64, no hashing or time: the output is a function of the input only. Intersection points
//! carry the clip coordinate exactly (as `clip_ring`), so seams of neighbouring rects match.

use crate::mesher::{clip_ring, ClipRect};

type Ring = Vec<[f64; 2]>;

fn bbox(r: &[[f64; 2]]) -> [f64; 4] {
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for p in r {
        b[0] = b[0].min(p[0]);
        b[1] = b[1].min(p[1]);
        b[2] = b[2].max(p[0]);
        b[3] = b[3].max(p[1]);
    }
    b
}

fn area2(r: &[[f64; 2]]) -> f64 {
    let n = r.len();
    (0..n).map(|i| r[i][0] * r[(i + 1) % n][1] - r[(i + 1) % n][0] * r[i][1]).sum()
}

/// True when some hole of `poly` (rings after the first) reaches the boundary of `r` from
/// inside or crosses it (by bounding box): only then does per-ring clipping go wrong, so the
/// callers keep their per-ring path (and its exact output) for every other polygon.
pub(crate) fn hole_crosses(poly: &[Ring], r: &ClipRect) -> bool {
    poly.iter().skip(1).any(|h| {
        if h.is_empty() {
            return false;
        }
        let b = bbox(h);
        let outside = b[2] <= r.x0 || b[0] >= r.x1 || b[3] <= r.y0 || b[1] >= r.y1;
        let inside = b[0] > r.x0 && b[2] < r.x1 && b[1] > r.y0 && b[3] < r.y1;
        !outside && !inside
    })
}

/// Winding number of `r` around `p` (p not on `r`).
fn winding(r: &[[f64; 2]], p: [f64; 2]) -> i32 {
    let n = r.len();
    let mut w = 0;
    for i in 0..n {
        let (a, b) = (r[i], r[(i + 1) % n]);
        let side = (b[0] - a[0]) * (p[1] - a[1]) - (p[0] - a[0]) * (b[1] - a[1]);
        if a[1] <= p[1] {
            if b[1] > p[1] && side > 0.0 {
                w += 1;
            }
        } else if b[1] <= p[1] && side < 0.0 {
            w -= 1;
        }
    }
    w
}

/// Even-odd point in ring.
fn inside(r: &[[f64; 2]], p: [f64; 2]) -> bool {
    let n = r.len();
    let mut c = false;
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (r[i], r[j]);
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
            c = !c;
        }
        j = i;
    }
    c
}

/// Position of a boundary point along the rect perimeter in the positive direction
/// (x0,y0) -> (x1,y0) -> (x1,y1) -> (x0,y1).
fn perim(r: &ClipRect, p: [f64; 2]) -> f64 {
    let (w, h) = (r.x1 - r.x0, r.y1 - r.y0);
    if p[1] == r.y0 {
        p[0] - r.x0
    } else if p[0] == r.x1 {
        w + (p[1] - r.y0)
    } else if p[1] == r.y1 {
        w + h + (r.x1 - p[0])
    } else {
        2.0 * w + h + (r.y1 - p[1])
    }
}

/// `poly` (outer ring + holes, tile pixels, any orientation) intersected with `r`, as polygons
/// (outer ring first, positive area; holes negative). Every vertex lies in the closed rect;
/// edges on the rect boundary are the clip seams.
pub(crate) fn clip_polygon(poly: &[Ring], r: &ClipRect) -> Vec<Vec<Ring>> {
    let (mut a, mut tmp) = (Vec::new(), Vec::new());
    let mut oriented: Ring = Vec::new();
    let mut chains: Vec<Ring> = Vec::new();
    let mut outers: Vec<Ring> = Vec::new();
    let mut holes: Vec<Ring> = Vec::new();
    let center = [(r.x0 + r.x1) * 0.5, (r.y0 + r.y1) * 0.5];
    // Winding at the rect centre of the rings lying entirely on the rect boundary.
    let mut base_w = 0;
    for (ri, ring) in poly.iter().enumerate() {
        if ring.len() < 3 {
            continue;
        }
        let a0 = area2(ring);
        if a0 == 0.0 {
            continue;
        }
        oriented.clear();
        oriented.extend_from_slice(ring);
        if (a0 > 0.0) != (ri == 0) {
            oriented.reverse();
        }
        clip_ring(&oriented, r, &mut a, &mut tmp);
        a.dedup();
        while a.len() > 1 && a.first() == a.last() {
            a.pop();
        }
        if a.len() < 3 || area2(&a) == 0.0 {
            continue;
        }
        let n = a.len();
        let bnd: Vec<bool> = (0..n).map(|i| r.flags(a[i]) & r.flags(a[(i + 1) % n]) != 0).collect();
        if !bnd.iter().any(|&x| x) {
            if ri == 0 {
                outers.push(a.clone());
            } else {
                holes.push(a.clone());
            }
            continue;
        }
        if bnd.iter().all(|&x| x) {
            base_w += winding(&a, center);
            continue;
        }
        // Chains of non-boundary edges, each starting right after a boundary edge.
        for i in 0..n {
            if bnd[(i + n - 1) % n] && !bnd[i] {
                let mut c = vec![a[i]];
                let mut j = i;
                while !bnd[j] {
                    j = (j + 1) % n;
                    c.push(a[j]);
                }
                chains.push(c);
            }
        }
    }
    if chains.is_empty() {
        if base_w > 0 {
            outers.push(vec![[r.x0, r.y0], [r.x1, r.y0], [r.x1, r.y1], [r.x0, r.y1]]);
        }
    } else {
        let (w, h) = (r.x1 - r.x0, r.y1 - r.y0);
        let per = 2.0 * (w + h);
        let corners = [(0.0, [r.x0, r.y0]), (w, [r.x1, r.y0]), (w + h, [r.x1, r.y1]), (2.0 * w + h, [r.x0, r.y1])];
        let starts: Vec<f64> = chains.iter().map(|c| perim(r, c[0])).collect();
        let ends: Vec<f64> = chains.iter().map(|c| perim(r, *c.last().unwrap())).collect();
        let m = chains.len();
        let mut used = vec![false; m];
        for first in 0..m {
            if used[first] {
                continue;
            }
            let mut out: Ring = Vec::new();
            let mut c = first;
            for _ in 0..=m {
                used[c] = true;
                for &p in &chains[c] {
                    if out.last() != Some(&p) {
                        out.push(p);
                    }
                }
                let e = ends[c];
                // Next chain start in the positive direction; on a tie close the ring first.
                let mut best: Option<(f64, usize, usize)> = None;
                for j in 0..m {
                    if used[j] && j != first {
                        continue;
                    }
                    let d = (starts[j] - e).rem_euclid(per);
                    let key = (d, (j != first) as usize, j);
                    if best.is_none_or(|b| (key.0, key.1, key.2) < b) {
                        best = Some(key);
                    }
                }
                let Some((d, _, j)) = best else { break };
                let mut cs: Vec<(f64, [f64; 2])> = corners
                    .iter()
                    .map(|&(cp, pt)| ((cp - e).rem_euclid(per), pt))
                    .filter(|&(dc, _)| dc > 0.0 && dc < d)
                    .collect();
                cs.sort_by(|x, y| x.0.total_cmp(&y.0));
                for (_, pt) in cs {
                    if out.last() != Some(&pt) {
                        out.push(pt);
                    }
                }
                if j == first {
                    break;
                }
                c = j;
            }
            while out.len() > 1 && out.first() == out.last() {
                out.pop();
            }
            if out.len() >= 3 && area2(&out) > 0.0 {
                outers.push(out);
            }
        }
    }
    let mut polys: Vec<Vec<Ring>> = outers.into_iter().map(|o| vec![o]).collect();
    for hole in holes {
        // The piece holding most of the hole's vertices (pieces only touch, so a strictly
        // interior vertex decides; ties go to the first piece).
        let mut best = (0usize, usize::MAX);
        for (k, p) in polys.iter().enumerate() {
            let votes = hole.iter().filter(|&&v| inside(&p[0], v)).count();
            if votes > best.0 {
                best = (votes, k);
            }
        }
        if best.1 != usize::MAX {
            polys[best.1].push(hole);
        }
    }
    polys
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sq(x: f64, y: f64, s: f64) -> Ring {
        vec![[x, y], [x + s, y], [x + s, y + s], [x, y + s]]
    }
    fn area(polys: &[Vec<Ring>]) -> f64 {
        polys.iter().flat_map(|p| p.iter()).map(|r| area2(r) * 0.5).sum()
    }
    fn rect() -> ClipRect {
        ClipRect { x0: 0.0, y0: 0.0, x1: 100.0, y1: 100.0 }
    }

    #[test]
    fn hole_on_edge_notches_the_outer() {
        let poly = vec![sq(-50.0, -50.0, 200.0), sq(90.0, 40.0, 20.0)];
        let out = clip_polygon(&poly, &rect());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), 1, "the hole became a notch of the outer ring");
        assert!((area(&out) - (10000.0 - 200.0)).abs() < 1e-9);
    }

    #[test]
    fn hole_spanning_the_rect_splits_it() {
        let poly = vec![sq(-50.0, -50.0, 200.0), vec![[-10.0, 40.0], [110.0, 40.0], [110.0, 60.0], [-10.0, 60.0]]];
        let out = clip_polygon(&poly, &rect());
        assert_eq!(out.len(), 2);
        assert!((area(&out) - 8000.0).abs() < 1e-9);
    }

    #[test]
    fn hole_covering_the_rect_leaves_nothing_and_inner_hole_stays() {
        let poly = vec![sq(-50.0, -50.0, 200.0), sq(-10.0, -10.0, 120.0)];
        assert!(clip_polygon(&poly, &rect()).is_empty());
        let poly = vec![sq(-50.0, -50.0, 200.0), sq(40.0, 40.0, 10.0)];
        let out = clip_polygon(&poly, &rect());
        assert_eq!((out.len(), out[0].len()), (1, 2));
        assert!((area(&out) - 9900.0).abs() < 1e-9);
    }

    #[test]
    fn deterministic() {
        let poly = vec![sq(-50.0, -50.0, 200.0), sq(90.0, 90.0, 20.0), sq(-5.0, 30.0, 10.0)];
        assert_eq!(clip_polygon(&poly, &rect()), clip_polygon(&poly, &rect()));
    }
}

/// Shared fixtures of the mesher / ground tests: axis-aligned outer + holes (tile pixels,
/// extent 4096) whose holes reach the z16 child seams (x, y = 2048) or the tile edge.
#[cfg(test)]
pub(crate) mod fixtures {
    use crate::mvt::TileProjector;

    /// (name, outer [x0, y0, x1, y1], holes).
    pub(crate) type Case = (&'static str, [f64; 4], Vec<[f64; 4]>);

    pub(crate) fn cases() -> Vec<Case> {
        let outer = [1000.0, 1000.0, 3000.0, 3000.0];
        let big = [-64.0, -64.0, 4160.0, 4160.0];
        vec![
            ("one edge", outer, vec![[1900.0, 1300.0, 2200.0, 1500.0]]),
            ("corner", outer, vec![[1900.0, 1900.0, 2200.0, 2200.0]]),
            ("spans the rect width", big, vec![[-32.0, 1800.0, 4128.0, 1900.0]]),
            ("crosses the tile corner", big, vec![[-32.0, -32.0, 300.0, 300.0]]),
            ("touches the edge", outer, vec![[1700.0, 1300.0, 2048.0, 1500.0], [2048.0, 2500.0, 2300.0, 2700.0]]),
            (
                "all at once",
                big,
                vec![[-32.0, 1800.0, 4128.0, 1900.0], [1900.0, 2300.0, 2200.0, 2600.0], [1700.0, 1300.0, 2048.0, 1500.0], [500.0, 500.0, 700.0, 700.0]],
            ),
        ]
    }

    /// Rings in the MVT convention: exterior one way, holes the other.
    pub(crate) fn rings(outer: [f64; 4], holes: &[[f64; 4]]) -> Vec<Vec<[f64; 2]>> {
        let r = |b: [f64; 4]| vec![[b[0], b[1]], [b[2], b[1]], [b[2], b[3]], [b[0], b[3]]];
        let mut v = vec![r(outer)];
        for &h in holes {
            let mut ring = r(h);
            ring.reverse();
            v.push(ring);
        }
        v
    }

    /// Expected area (m2) of `outer` minus `holes`, both cut to `clip`. First checks in tile
    /// pixels that [`super::clip_polygon`] yields exactly outer ∩ clip − holes ∩ clip (1e-9
    /// relative), then returns the area of those pieces through the projector (the same vertex
    /// set as the mesh: a straight pixel edge is not exactly straight in ENU, so projecting the
    /// rectangles instead would differ by ~1e-6 where a seam or notch adds a vertex).
    pub(crate) fn expected_m2(p: &TileProjector, clip: [f64; 4], outer: [f64; 4], holes: &[[f64; 4]]) -> f64 {
        let part = |b: [f64; 4]| -> f64 {
            let i = [b[0].max(clip[0]), b[1].max(clip[1]), b[2].min(clip[2]), b[3].min(clip[3])];
            if i[2] <= i[0] || i[3] <= i[1] { 0.0 } else { (i[2] - i[0]) * (i[3] - i[1]) }
        };
        let px = part(outer) - holes.iter().map(|&h| part(h)).sum::<f64>();
        let rect = crate::mesher::ClipRect { x0: clip[0], y0: clip[1], x1: clip[2], y1: clip[3] };
        let pieces = super::clip_polygon(&rings(outer, holes), &rect);
        let got: f64 = pieces.iter().flatten().map(|r| super::area2(r) * 0.5).sum();
        assert!((got - px).abs() <= 1e-9 * px.max(1.0), "pixel area {got} vs {px}");
        pieces
            .iter()
            .flatten()
            .map(|r| {
                let q: Vec<[f64; 2]> = r.iter().map(|&v| p.project64(v)).collect();
                super::area2(&q) * 0.5
            })
            .sum::<f64>()
            .abs()
    }
}
