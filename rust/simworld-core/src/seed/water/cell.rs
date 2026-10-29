//! Mixed cells: clipped rings with per-latitude-band edge references for the point test.

use super::{ClippedRing, MixedCell, BANDS};
use crate::seed::geom::Rect;

impl MixedCell {
    pub(super) fn new(rect: Rect, polys: Vec<u32>, rings: Vec<ClippedRing>) -> Self {
        let h = (rect[3] - rect[1]) / BANDS as f64;
        let band = |lat: f64| (((lat - rect[1]) / h).floor().max(0.0) as usize).min(BANDS - 1);
        let mut per: Vec<Vec<(u32, u32)>> = vec![Vec::new(); BANDS];
        for (ri, r) in rings.iter().enumerate() {
            let n = r.pts.len();
            for i in 0..n {
                let (a, b) = (r.pts[i], r.pts[(i + 1) % n]);
                if a[1] == b[1] {
                    continue; // horizontal: never crossed by a horizontal ray (half-open rule)
                }
                for v in per.iter_mut().take(band(a[1].max(b[1])) + 1).skip(band(a[1].min(b[1]))) {
                    v.push((ri as u32, i as u32));
                }
            }
        }
        let mut band_start = [0u32; BANDS + 1];
        let mut band_edges = Vec::new();
        for (k, v) in per.into_iter().enumerate() {
            band_edges.extend(v);
            band_start[k + 1] = band_edges.len() as u32;
        }
        Self { rect, polys, rings, band_start, band_edges }
    }

    /// Even-odd test per polygon against the edges of the point's band: the lowest polygon id
    /// containing the point.
    pub(super) fn test(&self, p: [f64; 2]) -> Option<u32> {
        self.test_where(p, |_| true)
    }

    /// [`MixedCell::test`] among the polygons `keep` accepts. A point on (or within 1e-9 cell
    /// sizes of) the cell's border is tested just inside it: the clipped rings run along the
    /// border, where the half-open ray test would count a water cell's edge as outside (a point
    /// exactly on a cell row's boundary, e.g. a terrain lattice sample, read as land).
    pub(super) fn test_where(&self, p: [f64; 2], keep: impl Fn(u32) -> bool) -> Option<u32> {
        let r = self.rect;
        let e = [(r[2] - r[0]) * 1e-9, (r[3] - r[1]) * 1e-9];
        let p = [p[0].clamp(r[0] + e[0], r[2] - e[0]), p[1].clamp(r[1] + e[1], r[3] - e[1])];
        let h = (self.rect[3] - self.rect[1]) / BANDS as f64;
        let b = (((p[1] - self.rect[1]) / h).floor().max(0.0) as usize).min(BANDS - 1);
        let mut parity = [0u64; 4];
        let mut overflow: Vec<bool> = Vec::new();
        if self.polys.len() > 256 {
            overflow = vec![false; self.polys.len()];
        }
        for &(ri, i) in &self.band_edges[self.band_start[b] as usize..self.band_start[b + 1] as usize] {
            let r = &self.rings[ri as usize];
            let n = r.pts.len();
            let (a, c) = (r.pts[i as usize], r.pts[(i as usize + 1) % n]);
            if (a[1] > p[1]) != (c[1] > p[1]) {
                let x = a[0] + (p[1] - a[1]) / (c[1] - a[1]) * (c[0] - a[0]);
                if x > p[0] {
                    let l = r.local as usize;
                    if overflow.is_empty() {
                        parity[l / 64] ^= 1 << (l % 64);
                    } else {
                        overflow[l] = !overflow[l];
                    }
                }
            }
        }
        let first = if overflow.is_empty() {
            (0..self.polys.len()).find(|&l| parity[l / 64] >> (l % 64) & 1 == 1 && keep(self.polys[l]))
        } else {
            (0..self.polys.len()).find(|&l| overflow[l] && keep(self.polys[l]))
        };
        first.map(|l| self.polys[l])
    }
}
