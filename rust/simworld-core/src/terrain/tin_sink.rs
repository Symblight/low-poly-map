//! The sink pass of [`Delatin`]: per-vertex sinks that put the TIN on or under the grid
//! heights (api 1.7, contract section 9a).

use super::{orient, Delatin};

/// Bisection steps of the closing pass of [`Delatin::vertex_sinks`] (`E_t / 2^24`: below f32).
const SINK_BISECTIONS: u32 = 24;

impl Delatin<'_> {
    /// Per-vertex sink (m) that puts the TIN on or under the grid heights (api 1.7, contract
    /// section 9a). Three passes over the triangles in index order, each over the grid samples
    /// inside the triangle (edges included, raster order); `e` = the excess of the triangle's
    /// plane through its unsunk vertices over a sample:
    ///
    /// 1. **Cap:** `E_t` = the largest excess in triangle `t`; a vertex may sink at most the
    ///    largest `E_t` of its triangles (`cap`), which is at most the TIN's largest error, so a
    ///    sunk vertex lies within twice the error under the exact height.
    /// 2. **Spread:** where the sinks so far lower the plane at a sample by less than `e`, the
    ///    missing part `d` goes to the triangle's free vertices in proportion to their
    ///    barycentric weight `w` (`d x w_i / sum w^2`, the smallest change that closes it), each
    ///    clamped to its cap. A sample next to one vertex mostly sinks that vertex.
    /// 3. **Close:** a triangle with a sample still above the sunk plane (a clamp bit) raises
    ///    the sinks of its free vertices to a common floor `u`, the smallest that closes every
    ///    sample in it (bisection in [0, `E_t`]; `u = E_t` closes a triangle without a pinned
    ///    corner, and every cap is at least `E_t`).
    ///
    /// Sinks only grow, so a sample settled earlier stays settled: every triangle without a
    /// pinned vertex lies on or under all its samples. `pinned` vertices (a seed world's sea
    /// and lake vertices, exactly at the water level) never sink; their triangles (the shore)
    /// may keep up to the plain TIN error above a sample. Deterministic.
    pub fn vertex_sinks(&self, pinned: Option<&[bool]>) -> Vec<f64> {
        let n = self.coords.len();
        let free = |i: usize| !pinned.is_some_and(|p| p[i]);
        let tri_count = self.triangle_count();
        // Pass 1: largest excess per triangle, cap per vertex.
        let mut tri_excess = vec![0.0f64; tri_count];
        let mut cap = vec![0.0f64; n];
        for (t, slot) in tri_excess.iter_mut().enumerate() {
            let Some((ids, _)) = self.sink_triangle(t) else { continue };
            let mut ex = 0.0f64;
            self.for_each_sample(t, |_, e| ex = ex.max(e));
            *slot = ex;
            for &i in &ids {
                if free(i) {
                    cap[i] = cap[i].max(ex);
                }
            }
        }
        // Pass 2: spread the missing part by barycentric weight, clamped to the caps. Sinks only
        // grow, so a triangle can still be open after this pass only if a clamp (or a pinned
        // corner) bit while it was processed.
        let mut sink = vec![0.0f64; n];
        let mut clamped = vec![false; tri_count];
        for t in 0..tri_count {
            let Some((ids, _)) = self.sink_triangle(t) else { continue };
            if tri_excess[t] <= 0.0 {
                continue;
            }
            let mut bit = false;
            self.for_each_sample(t, |w, e| {
                if e <= 0.0 {
                    return;
                }
                let d = e - (w[0] * sink[ids[0]] + w[1] * sink[ids[1]] + w[2] * sink[ids[2]]);
                if d <= 0.0 {
                    return;
                }
                let wf = [0, 1, 2].map(|k| if free(ids[k]) { w[k] } else { 0.0 });
                let n2 = wf[0] * wf[0] + wf[1] * wf[1] + wf[2] * wf[2];
                if n2 <= 0.0 {
                    bit = true;
                    return;
                }
                let k = d / n2;
                for i in 0..3 {
                    if wf[i] > 0.0 {
                        let want = sink[ids[i]] + k * wf[i];
                        if want > cap[ids[i]] {
                            bit = true;
                        }
                        sink[ids[i]] = want.min(cap[ids[i]]);
                    }
                }
            });
            clamped[t] = bit;
        }
        // Pass 3: close what the clamps left open.
        for t in 0..tri_count {
            let Some((ids, _)) = self.sink_triangle(t) else { continue };
            if !clamped[t] {
                continue;
            }
            let mut open = false;
            self.for_each_sample(t, |w, e| {
                open |= e > w[0] * sink[ids[0]] + w[1] * sink[ids[1]] + w[2] * sink[ids[2]];
            });
            if !open {
                continue;
            }
            // The smallest common floor `u` of the free sinks that closes every sample
            // (bisection; `u = E_t` closes unless a corner is pinned; every cap is at least `E_t`).
            let closes = |u: f64| {
                let s = ids.map(|i| if free(i) { sink[i].max(u) } else { sink[i] });
                let mut ok = true;
                self.for_each_sample(t, |w, e| ok &= e <= w[0] * s[0] + w[1] * s[1] + w[2] * s[2]);
                ok
            };
            let (mut lo, mut hi) = (0.0f64, tri_excess[t]);
            for _ in 0..SINK_BISECTIONS {
                let mid = 0.5 * (lo + hi);
                if closes(mid) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            for &i in &ids {
                if free(i) {
                    sink[i] = sink[i].max(hi);
                }
            }
        }
        sink
    }

    /// Vertex ids and doubled area of live triangle `t` (`None` if degenerate).
    fn sink_triangle(&self, t: usize) -> Option<([usize; 3], i64)> {
        let ids = [self.triangles[t * 3] as usize, self.triangles[t * 3 + 1] as usize, self.triangles[t * 3 + 2] as usize];
        let a = orient(self.coords[ids[0]], self.coords[ids[1]], self.coords[ids[2]]);
        (a > 0).then_some((ids, a))
    }

    /// Calls `f(barycentric weights, excess of the unsunk plane over the sample)` for every grid
    /// sample inside triangle `t` (edges included), rows then columns.
    fn for_each_sample(&self, t: usize, mut f: impl FnMut([f64; 3], f64)) {
        let Some((ids, a)) = self.sink_triangle(t) else { return };
        let af = a as f64;
        let (p0, p1, p2) = (self.coords[ids[0]], self.coords[ids[1]], self.coords[ids[2]]);
        let (h0, h1, h2) = (self.height_at(p0), self.height_at(p1), self.height_at(p2));
        for y in p0[1].min(p1[1]).min(p2[1])..=p0[1].max(p1[1]).max(p2[1]) {
            let row = y as usize * self.width;
            for x in p0[0].min(p1[0]).min(p2[0])..=p0[0].max(p1[0]).max(p2[0]) {
                let q = [x, y];
                let (w0, w1, w2) = (orient(p1, p2, q), orient(p2, p0, q), orient(p0, p1, q));
                if w0 < 0 || w1 < 0 || w2 < 0 {
                    continue;
                }
                let w = [w0 as f64 / af, w1 as f64 / af, w2 as f64 / af];
                let z = h0 * w[0] + h1 * w[1] + h2 * w[2];
                f(w, z - self.h[row + x as usize] as f64);
            }
        }
    }
}
