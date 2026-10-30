//! Local streets of one cell (district plan stage a; `docs/spec-world-seed.md` sections 6.1-6.2).
//!
//! Every style is a **street lattice** in its district's frame (origin = the district's first
//! cell centre, axis = its lattice direction): line positions `i x m` plus a hash offset within
//! the style's spacing range, each lattice point moved by a hash jitter and by smooth value-noise
//! warp. The lattice is a function of (district, i, j), so the merged cells of one district lay
//! out one continuous network; each cell keeps the parts inside its own polygon (streets end on
//! the arterial curves, water and fences). A share of the segments is dropped (merged blocks).
//! The **blocks** are the lattice quads touching the cell with both sides at least 20 m;
//! microdistrict blocks get a service loop.

use crate::mvt::splitmix64;
use crate::seed::city::lines::{bbox, bilinear, boxes_overlap, densify, len, lerp, ok_runs};
use crate::seed::city::{CellInput, District, StageHash};
use crate::seed::hash::unit;

/// A local street (plane offsets from the cell centre).
#[derive(Clone, Debug, PartialEq)]
pub struct PlanStreet {
    pub id: u64,
    pub class: u8,
    pub minor: bool,
    pub lit: bool,
    pub pts: Vec<[f32; 2]>,
}

/// A lattice quad (block) of the cell: lattice indices, plane corners (c00, c10, c11, c01) and the
/// mean lengths of its two side pairs (m).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Block {
    pub i: i64,
    pub j: i64,
    pub c: [[f64; 2]; 4],
    pub lu: f64,
    pub lv: f64,
}

/// Output of the local street stage: the cell's streets (lattice, then service loops in block
/// order) and its blocks.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LocalNetwork {
    pub streets: Vec<PlanStreet>,
    pub blocks: Vec<Block>,
}

impl LocalNetwork {
    /// Stage hash: streets and blocks.
    pub fn hash(&self) -> u64 {
        let mut h = StageHash::new("local");
        for s in &self.streets {
            h.u(s.id).u(s.class as u64).u(s.minor as u64).u(s.lit as u64).pts32(&s.pts);
        }
        for b in &self.blocks {
            h.i(b.i).i(b.j).pts(&b.c).f(b.lu).f(b.lv);
        }
        h.finish()
    }
}

/// Quintic fade.
#[inline]
fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// The street lattice of one district.
struct Lattice<'a> {
    d: &'a District,
    m: f64,
    spread: f64,
    jitter: f64,
    warp: f64,
}

impl Lattice<'_> {
    fn line(&self, axis: u32, i: i64) -> f64 {
        i as f64 * self.m + (unit(crate::seed_hash!(self.d.id, "line", axis, i)) - 0.5) * self.spread * 0.9
    }

    /// Smooth warp (local metres) at a local point: value noise with period 2 m, quintic.
    fn warp_at(&self, p: [f64; 2]) -> [f64; 2] {
        if self.warp <= 0.0 {
            return [0.0; 2];
        }
        let per = 2.0 * self.m;
        let (fx, fy) = (p[0] / per, p[1] / per);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fade(fx - x0), fade(fy - y0));
        let (ix, iy) = (x0 as i64, y0 as i64);
        let mut out = [0.0; 2];
        for (c, o) in out.iter_mut().enumerate() {
            let v = |x: i64, y: i64| unit(crate::seed_hash!(self.d.id, "warp", c, x, y)) * 2.0 - 1.0;
            let a = v(ix, iy) + (v(ix + 1, iy) - v(ix, iy)) * tx;
            let b = v(ix, iy + 1) + (v(ix + 1, iy + 1) - v(ix, iy + 1)) * tx;
            *o = (a + (b - a) * ty) * self.warp;
        }
        out
    }

    fn base(&self, i: i64, j: i64) -> [f64; 2] {
        [self.line(0, i), self.line(1, j)]
    }

    /// Local position of lattice point (i, j): base + jitter + warp.
    fn point(&self, i: i64, j: i64) -> [f64; 2] {
        let b = self.base(i, j);
        let h = crate::seed_hash!(self.d.id, "jit", i, j);
        let jx = (unit(h) * 2.0 - 1.0) * self.jitter * self.m;
        let jy = (unit(splitmix64(h)) * 2.0 - 1.0) * self.jitter * self.m;
        let w = self.warp_at(b);
        [b[0] + jx + w[0], b[1] + jy + w[1]]
    }

    fn to_plane(&self, l: [f64; 2]) -> [f64; 2] {
        let (u, v) = (self.d.dir, [-self.d.dir[1], self.d.dir[0]]);
        [self.d.origin[0] + u[0] * l[0] + v[0] * l[1], self.d.origin[1] + u[1] * l[0] + v[1] * l[1]]
    }

    fn to_local(&self, p: [f64; 2]) -> [f64; 2] {
        let (u, v) = (self.d.dir, [-self.d.dir[1], self.d.dir[0]]);
        let d = [p[0] - self.d.origin[0], p[1] - self.d.origin[1]];
        [d[0] * u[0] + d[1] * u[1], d[0] * v[0] + d[1] * v[1]]
    }

    /// The street from lattice point (i, j) to its neighbour along `axis`, in `n` pieces: straight
    /// between the two points plus the warp's curvature between them.
    fn segment(&self, i: i64, j: i64, axis: u32, n: usize) -> Vec<[f64; 2]> {
        let (i1, j1) = if axis == 0 { (i + 1, j) } else { (i, j + 1) };
        let (p0, p1) = (self.point(i, j), self.point(i1, j1));
        let (b0, b1) = (self.base(i, j), self.base(i1, j1));
        let (w0, w1) = (self.warp_at(b0), self.warp_at(b1));
        (0..=n)
            .map(|k| {
                let t = k as f64 / n as f64;
                let p = lerp(p0, p1, t);
                let wt = self.warp_at(lerp(b0, b1, t));
                let wl = lerp(w0, w1, t);
                self.to_plane([p[0] + wt[0] - wl[0], p[1] + wt[1] - wl[1]])
            })
            .collect()
    }
}

/// The local streets and blocks of one cell.
pub fn build(inp: &CellInput) -> LocalNetwork {
    let (d, s, k) = (inp.d, inp.style, inp.kinds);
    let cell = inp.cell;
    let mut out = LocalNetwork::default();
    let (smin, smax) = (s.spacing_m[0].min(s.spacing_m[1]), s.spacing_m[0].max(s.spacing_m[1]));
    let lat = Lattice { d, m: (smin + smax) * 0.5, spread: smax - smin, jitter: s.jitter, warp: s.warp_m };
    let ok = |p: [f64; 2]| inp.ok(p);
    // Lattice range covering the cell (+2 lines for jitter and warp).
    let local: Vec<[f64; 2]> = cell.poly.iter().map(|p| lat.to_local(*p)).collect();
    let lb = bbox(&local);
    let (i0, i1) = ((lb[0] / lat.m).floor() as i64 - 2, (lb[2] / lat.m).ceil() as i64 + 2);
    let (j0, j1) = ((lb[1] / lat.m).floor() as i64 - 2, (lb[3] / lat.m).ceil() as i64 + 2);

    // Streets.
    for i in i0..=i1 {
        for j in j0..=j1 {
            for axis in 0..2u32 {
                if unit(crate::seed_hash!(d.id, "sdrop", axis, i, j)) < s.street_drop {
                    continue;
                }
                let seg = lat.segment(i, j, axis, 6);
                if !boxes_overlap(bbox(&seg), cell.bbox) {
                    continue;
                }
                let id = crate::seed_hash!(d.id, "street", axis, i, j) & 0xFFFF_FFFF;
                let lit = unit(crate::seed_hash!(d.id, "lit", axis, i, j)) < d.lit;
                for run in ok_runs(&seg, &ok, 15.0) {
                    out.streets.push(PlanStreet { id, class: k.street.class, minor: k.street.minor, lit, pts: run.iter().map(|p| inp.rel(*p)).collect() });
                }
            }
        }
    }

    // Blocks and their service loops.
    for i in i0..i1 {
        for j in j0..j1 {
            let c = [lat.point(i, j), lat.point(i + 1, j), lat.point(i + 1, j + 1), lat.point(i, j + 1)].map(|l| lat.to_plane(l));
            if !boxes_overlap(bbox(&c), cell.bbox) {
                continue;
            }
            let lu = (len(c[0], c[1]) + len(c[3], c[2])) * 0.5;
            let lv = (len(c[0], c[3]) + len(c[1], c[2])) * 0.5;
            if lu < 20.0 || lv < 20.0 {
                continue;
            }
            if let Some(lk) = k.loop_street {
                let q = |s: f64, t: f64| bilinear(&c, s, t);
                let ring = vec![q(0.28, 0.28), q(0.72, 0.28), q(0.72, 0.72), q(0.28, 0.72), q(0.28, 0.28)];
                let id = crate::seed_hash!(d.id, "loop", i, j) & 0xFFFF_FFFF;
                for run in ok_runs(&densify(&ring, 25.0), &ok, 15.0) {
                    out.streets.push(PlanStreet { id, class: lk.class, minor: lk.minor, lit: true, pts: run.iter().map(|p| inp.rel(*p)).collect() });
                }
            }
            out.blocks.push(Block { i, j, c, lu, lv });
        }
    }
    out
}
