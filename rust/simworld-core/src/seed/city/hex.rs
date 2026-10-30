//! The city's hex grid (`docs/spec-world-seed.md` section 5.3 step 1): pointy-top cells, size =
//! centre-to-centre distance, rotated about the city centre, in the city plane (metres, x east,
//! y north). Shared geometry is computed from canonical keys so both neighbours get the same
//! bits: a corner from the sorted triple of its cells, an edge curve from the sorted pair.

use crate::seed::hash::{edge_seed, unit, Seed};

/// Axial cell coordinates (q, r).
pub type Ax = (i32, i32);

/// sqrt(3) / 2.
pub const S3H: f64 = 0.866_025_403_784_438_6;

/// Neighbour directions in order of angle (0, 60, ..., 300 degrees, unrotated).
pub const DIRS: [Ax; 6] = [(1, 0), (0, 1), (-1, 1), (-1, 0), (0, -1), (1, -1)];

#[inline]
pub fn add(a: Ax, d: Ax) -> Ax {
    (a.0 + d.0, a.1 + d.1)
}

/// Ring (hex distance from the centre cell).
#[inline]
pub fn ring(a: Ax) -> u32 {
    ((a.0.abs() + a.1.abs() + (a.0 + a.1).abs()) / 2) as u32
}

/// Hex distance between two cells.
#[inline]
pub fn dist(a: Ax, b: Ax) -> u32 {
    ring((a.0 - b.0, a.1 - b.1))
}

/// Sorted pair: the canonical key of the edge between two neighbours.
#[inline]
pub fn edge_key(a: Ax, b: Ax) -> (Ax, Ax) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Canonical key of corner `k` of cell `a` (between neighbours `k` and `k + 1`): its three cells, sorted.
pub fn corner_key(a: Ax, k: usize) -> [Ax; 3] {
    let mut c = [a, add(a, DIRS[k % 6]), add(a, DIRS[(k + 1) % 6])];
    c.sort();
    c
}

/// The rotated hex grid of one city.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HexGrid {
    pub cell_m: f64,
    cos: f64,
    sin: f64,
}

impl HexGrid {
    pub fn new(cell_m: f64, rotation_rad: f64) -> Self {
        let (sin, cos) = crate::math::sin_cos(rotation_rad);
        Self { cell_m, cos, sin }
    }

    /// Area of one cell (m²).
    pub fn cell_area(&self) -> f64 {
        self.cell_m * self.cell_m * S3H
    }

    #[inline]
    fn rot(&self, p: [f64; 2]) -> [f64; 2] {
        [p[0] * self.cos - p[1] * self.sin, p[0] * self.sin + p[1] * self.cos]
    }

    #[inline]
    fn unrot(&self, p: [f64; 2]) -> [f64; 2] {
        [p[0] * self.cos + p[1] * self.sin, -p[0] * self.sin + p[1] * self.cos]
    }

    #[inline]
    fn raw_center(&self, a: Ax) -> [f64; 2] {
        [self.cell_m * (a.0 as f64 + a.1 as f64 * 0.5), self.cell_m * S3H * a.1 as f64]
    }

    /// Centre of a cell in the city plane.
    pub fn center(&self, a: Ax) -> [f64; 2] {
        self.rot(self.raw_center(a))
    }

    /// A point given relative to a cell centre in the unrotated hex frame -> city plane.
    pub fn offset(&self, a: Ax, d: [f64; 2]) -> [f64; 2] {
        let c = self.raw_center(a);
        self.rot([c[0] + d[0], c[1] + d[1]])
    }

    /// Corner position: the mean of its three cell centres (summed in key order, so every cell
    /// sharing it gets the same bits).
    pub fn corner(&self, key: [Ax; 3]) -> [f64; 2] {
        let [a, b, c] = key.map(|k| self.raw_center(k));
        self.rot([(a[0] + b[0] + c[0]) / 3.0, (a[1] + b[1] + c[1]) / 3.0])
    }

    /// The cell whose plain hexagon holds a plane point (cube rounding).
    pub fn cell_at(&self, p: [f64; 2]) -> Ax {
        let u = self.unrot(p);
        let r = u[1] / (self.cell_m * S3H);
        let q = u[0] / self.cell_m - r * 0.5;
        let s = -q - r;
        let (mut rq, mut rr, rs) = (q.round(), r.round(), s.round());
        let (dq, dr, ds) = ((rq - q).abs(), (rr - r).abs(), (rs - s).abs());
        if dq > dr && dq > ds {
            rq = -rr - rs;
        } else if dr > ds {
            rr = -rq - rs;
        }
        (rq as i32, rr as i32)
    }

    /// The curve of the edge between neighbours `a` and `b`, from its lower corner key to its
    /// higher one: the midpoint moved across the edge by `warp x cell x (2u - 1)` with
    /// `u = unit(edge seed)`, then two Chaikin passes with fixed ends (12 points).
    pub fn edge_curve(&self, city: Seed, a: Ax, b: Ax, warp: f64) -> Vec<[f64; 2]> {
        let (a, b) = edge_key(a, b);
        let k = DIRS.iter().position(|d| add(a, *d) == b).expect("neighbours");
        let (mut k0, mut k1) = (corner_key(a, k + 5), corner_key(a, k));
        if k1 < k0 {
            std::mem::swap(&mut k0, &mut k1);
        }
        let (p0, p1) = (self.corner(k0), self.corner(k1));
        let d = [p1[0] - p0[0], p1[1] - p0[1]];
        let len = (d[0] * d[0] + d[1] * d[1]).sqrt();
        let off = warp * self.cell_m * (unit(edge_seed(city, a, b).0) * 2.0 - 1.0);
        let n = [-d[1] / len, d[0] / len];
        let mid = [(p0[0] + p1[0]) * 0.5 + n[0] * off, (p0[1] + p1[1]) * 0.5 + n[1] * off];
        chaikin(&chaikin(&[p0, mid, p1]))
    }

    /// Ring of cell `a` (counter-clockwise): its six edge curves, each from corner `k - 1` to
    /// corner `k`, last point of each dropped. `curve` returns the canonical curve of an edge.
    pub fn cell_ring(&self, a: Ax, curve: &mut dyn FnMut(Ax, Ax) -> Vec<[f64; 2]>) -> Vec<[f64; 2]> {
        let mut out = Vec::with_capacity(66);
        for (k, d) in DIRS.iter().enumerate() {
            let b = add(a, *d);
            let (from, to) = (corner_key(a, k + 5), corner_key(a, k));
            let mut c = curve(a, b);
            if from > to {
                c.reverse();
            }
            out.extend_from_slice(&c[..c.len() - 1]);
        }
        out
    }
}

/// One Chaikin pass with fixed end points.
pub fn chaikin(p: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut out = Vec::with_capacity(p.len() * 2);
    out.push(p[0]);
    for w in p.windows(2) {
        let (a, b) = (w[0], w[1]);
        out.push([0.75 * a[0] + 0.25 * b[0], 0.75 * a[1] + 0.25 * b[1]]);
        out.push([0.25 * a[0] + 0.75 * b[0], 0.25 * a[1] + 0.75 * b[1]]);
    }
    out.push(p[p.len() - 1]);
    out
}

/// Union-find with a size per root (district merge and the arterial spanning tree).
pub(super) struct Dsu {
    parent: Vec<usize>,
    pub(super) size: Vec<usize>,
}

impl Dsu {
    pub(super) fn new(n: usize) -> Self {
        Self { parent: (0..n).collect(), size: vec![1; n] }
    }
    pub(super) fn find(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]];
            x = self.parent[x];
        }
        x
    }
    /// Join the sets of `a` and `b` (the lower root wins); false when already joined.
    pub(super) fn union(&mut self, a: usize, b: usize) -> bool {
        let (a, b) = (self.find(a), self.find(b));
        if a == b {
            return false;
        }
        let (lo, hi) = if a < b { (a, b) } else { (b, a) };
        self.parent[hi] = lo;
        self.size[lo] += self.size[hi];
        true
    }
}
