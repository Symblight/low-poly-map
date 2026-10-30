//! Approach surfaces (`docs/spec-world-seed.md` section 5.1 step 3): from an inner edge
//! `strip_end_m` before each landing threshold (displaced thresholds applied), as wide as the
//! strip, a trapezoid widening outward at `divergence` per side over `length_m`, whose height
//! limit rises at `slope` from the height of the drawn ground at the threshold. Generated city
//! buildings inside are capped at the surface (a cap, not a removal); placement keeps tall
//! districts away from them (section 5.3 step 3).

use super::{AirportGeo, LocalFrame};
use crate::profile::{SeedApproach, SeedApproachClass};

/// Most footprint vertices [`ApproachSurface::cap_over`] clips (generated footprints have 4-6).
pub const CAP_MAX_VERTS: usize = 24;
/// Clip buffer. A half-plane adds at most one vertex to a convex ring; a concave ring can gain
/// one per pair of crossings, so a clip that would overflow gives the conservative answer.
const CAP_BUF: usize = CAP_MAX_VERTS + 4;

/// One approach surface (one runway end).
#[derive(Clone, Debug, PartialEq)]
pub struct ApproachSurface {
    frame: LocalFrame,
    /// Centre of the inner edge and the unit direction away from the runway (airport plane).
    origin: [f64; 2],
    out: [f64; 2],
    half0: f64,
    class: SeedApproachClass,
    /// Height limit at the inner edge (m above MSL).
    base_h: f64,
    /// lat/lon box [w, s, e, n].
    pub bbox: [f64; 4],
}

impl ApproachSurface {
    /// Height limit (m above MSL) at a point, `None` outside the trapezoid.
    pub fn limit(&self, lat: f64, lon: f64) -> Option<f64> {
        let p = self.frame.xy(lat, lon);
        let d = [p[0] - self.origin[0], p[1] - self.origin[1]];
        let along = d[0] * self.out[0] + d[1] * self.out[1];
        if !(0.0..=self.class.length_m).contains(&along) {
            return None;
        }
        let across = (d[0] * -self.out[1] + d[1] * self.out[0]).abs();
        (across <= self.half0 + self.class.divergence * along).then_some(self.base_h + self.class.slope * along)
    }

    /// Lowest height limit over the part of a footprint (lat/lon vertices `ring`; more than
    /// [`CAP_MAX_VERTS`], or a concave ring whose clip overflows the buffer, give the surface's
    /// lowest limit) inside the trapezoid, `None` when they do not overlap. The limit rises
    /// along the axis only, so its minimum over footprint ∩ trapezoid (a convex clip, done on the
    /// stack) lies on a vertex of the clipped polygon: exact where a footprint crosses a side or
    /// the inner edge, where the ring's own vertices would miss the lowest point.
    pub fn cap_over(&self, ring: impl Iterator<Item = (f64, f64)>) -> Option<f64> {
        let mut a = [[0.0f64; 2]; CAP_BUF];
        let mut n = 0;
        for (lat, lon) in ring {
            if n == CAP_MAX_VERTS {
                // Never for generated footprints; the lowest limit of the surface is safe.
                return Some(self.base_h);
            }
            let p = self.frame.xy(lat, lon);
            let d = [p[0] - self.origin[0], p[1] - self.origin[1]];
            a[n] = [d[0] * self.out[0] + d[1] * self.out[1], d[0] * -self.out[1] + d[1] * self.out[0]];
            n += 1;
        }
        let (len, h0, div) = (self.class.length_m, self.half0, self.class.divergence);
        // Half-planes f(p) >= 0: along >= 0, along <= len, ±across <= h0 + div * along.
        let planes: [&dyn Fn([f64; 2]) -> f64; 4] =
            [&|p| p[0], &|p| len - p[0], &|p| h0 + div * p[0] - p[1], &|p| h0 + div * p[0] + p[1]];
        let mut b = [[0.0f64; 2]; CAP_BUF];
        for f in planes {
            let mut m = 0;
            for i in 0..n {
                let (p, q) = (a[i], a[(i + 1) % n]);
                let (fp, fq) = (f(p), f(q));
                if fp >= 0.0 {
                    if m == CAP_BUF {
                        return Some(self.base_h);
                    }
                    b[m] = p;
                    m += 1;
                }
                if (fp >= 0.0) != (fq >= 0.0) {
                    if m == CAP_BUF {
                        return Some(self.base_h);
                    }
                    let t = fp / (fp - fq);
                    b[m] = [p[0] + (q[0] - p[0]) * t, p[1] + (q[1] - p[1]) * t];
                    m += 1;
                }
            }
            (a, n) = (b, m);
            if n == 0 {
                return None;
            }
        }
        let lo = a[..n].iter().map(|p| p[0].clamp(0.0, len)).fold(f64::INFINITY, f64::min);
        Some(self.base_h + self.class.slope * lo)
    }
}

/// The approach surfaces of every open runway end of a world.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ApproachIndex {
    pub surfaces: Vec<ApproachSurface>,
}

impl ApproachIndex {
    /// Build from the airports; `height(lat, lon)` gives the base of each surface (the terrain at
    /// the threshold, runway strips included).
    pub fn new(airports: &[AirportGeo], p: &SeedApproach, strip_end_m: f64, height: &dyn Fn(f64, f64) -> f64) -> Self {
        let mut surfaces = Vec::new();
        for g in airports {
            for r in &g.runways {
                let class = if r.paved && r.length_m >= p.long_min_runway_m { p.long.clone() } else { p.short.clone() };
                for (s, sign) in [(r.le_thr_s, -1.0), (r.he_thr_s, 1.0)] {
                    let out = [r.dir[0] * sign, r.dir[1] * sign];
                    let thr = r.at(s, 0.0);
                    let origin = [thr[0] + out[0] * strip_end_m, thr[1] + out[1] * strip_end_m];
                    let (lat, lon) = g.frame.ll(thr);
                    let base_h = (height(lat, lon) * 100.0).round() / 100.0;
                    let half0 = r.strip_half_m;
                    let far = [origin[0] + out[0] * class.length_m, origin[1] + out[1] * class.length_m];
                    let half1 = half0 + class.divergence * class.length_m;
                    let n = [-out[1], out[0]];
                    let corners = [
                        [origin[0] + n[0] * half0, origin[1] + n[1] * half0],
                        [origin[0] - n[0] * half0, origin[1] - n[1] * half0],
                        [far[0] + n[0] * half1, far[1] + n[1] * half1],
                        [far[0] - n[0] * half1, far[1] - n[1] * half1],
                    ];
                    let mut bbox = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
                    for c in corners {
                        let (la, lo) = g.frame.ll(c);
                        bbox = [bbox[0].min(lo), bbox[1].min(la), bbox[2].max(lo), bbox[3].max(la)];
                    }
                    surfaces.push(ApproachSurface { frame: g.frame, origin, out, half0, class: class.clone(), base_h, bbox });
                }
            }
        }
        Self { surfaces }
    }

    /// Surfaces whose box overlaps a lat/lon box [w, s, e, n].
    pub fn within(&self, b: [f64; 4]) -> Vec<&ApproachSurface> {
        self.surfaces.iter().filter(|s| s.bbox[0] <= b[2] && b[0] <= s.bbox[2] && s.bbox[1] <= b[3] && b[1] <= s.bbox[3]).collect()
    }

    /// Lowest height limit at a point over all surfaces, `None` outside every surface.
    pub fn limit(&self, lat: f64, lon: f64) -> Option<f64> {
        limit_of(self.surfaces.iter(), lat, lon)
    }
}

/// Lowest limit of some surfaces at a point.
pub fn limit_of<'a>(surfaces: impl Iterator<Item = &'a ApproachSurface>, lat: f64, lon: f64) -> Option<f64> {
    surfaces
        .filter(|s| s.bbox[0] <= lon && lon <= s.bbox[2] && s.bbox[1] <= lat && lat <= s.bbox[3])
        .filter_map(|s| s.limit(lat, lon))
        .fold(None, |m: Option<f64>, v| Some(m.map_or(v, |m| m.min(v))))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A surface pointing east from the plane origin: half width 50 m at the inner edge, 0.15
    /// divergence, 3 km long, 2 % slope, base 100 m.
    fn surface() -> ApproachSurface {
        let class = SeedApproachClass { divergence: 0.15, length_m: 3000.0, slope: 0.02 };
        let frame = LocalFrame::new(55.0, 25.0);
        ApproachSurface { frame, origin: [0.0, 0.0], out: [1.0, 0.0], half0: 50.0, class, base_h: 100.0, bbox: [0.0; 4] }
    }

    fn cap(s: &ApproachSurface, ring: &[[f64; 2]]) -> Option<f64> {
        s.cap_over(ring.iter().map(|p| s.frame.ll(*p)))
    }

    #[test]
    fn square_straddling_the_inner_edge_gets_the_base() {
        let s = surface();
        let got = cap(&s, &[[-10.0, -5.0], [10.0, -5.0], [10.0, 5.0], [-10.0, 5.0]]).unwrap();
        assert!((got - 100.0).abs() < 1e-6, "{got}");
        assert_eq!(cap(&s, &[[-30.0, -5.0], [-10.0, -5.0], [-10.0, 5.0], [-30.0, 5.0]]), None);
    }

    #[test]
    fn square_crossing_a_side_gets_the_limit_at_the_crossing() {
        // Only (200, 70) is inside; the side y = 50 + 0.15 x meets y = 70 at x = 133.33.
        let s = surface();
        let got = cap(&s, &[[100.0, 70.0], [200.0, 70.0], [200.0, 120.0], [100.0, 120.0]]).unwrap();
        let want = 100.0 + 0.02 * (20.0 / 0.15);
        assert!((got - want).abs() < 1e-6, "{got} vs {want}");
    }

    #[test]
    fn l_shaped_ring_gets_its_lowest_inside_point() {
        // The L's corner (200, 40) is inside (half width 80 there), its arm crosses the side.
        let s = surface();
        let ring = [[200.0, 40.0], [300.0, 40.0], [300.0, 60.0], [240.0, 60.0], [240.0, 120.0], [200.0, 120.0]];
        let got = cap(&s, &ring).unwrap();
        assert!((got - 104.0).abs() < 1e-6, "{got}");
    }

    #[test]
    fn concave_ring_overflowing_the_clip_buffer_is_conservative() {
        // A zigzag across the inner edge: 22 teeth vertices + 2 closing ones = CAP_MAX_VERTS,
        // 21 crossings of along = 0 overflow the buffer.
        let s = surface();
        let mut ring: Vec<[f64; 2]> = (0..22).map(|k| [if k % 2 == 0 { 10.0 } else { -10.0 }, 2.0 * k as f64]).collect();
        ring.extend([[30.0, 42.0], [30.0, 0.0]]);
        assert_eq!(ring.len(), CAP_MAX_VERTS);
        assert_eq!(cap(&s, &ring), Some(100.0));
    }
}
