//! Stage 3b, lots (`docs/spec-seed-city-pipeline.md` section 5.2; generator 3:
//! `docs/spec-world-seed.md` section 6.3), on the blocks of the local network.
//!
//! Each block is split along its long side (the frontage) into two rows of lots back to back,
//! `n = usable length / frontage` per row with jittered split points; a lot is kept with the
//! style's `keep` share scaled by the district density. A microdistrict block with a service loop
//! gets a civic lot (school or kindergarten) at its centre with probability 0.6. Lot positions are
//! block parameters (s, t in [0, 1] along the block's sides), so the buildings stage reproduces
//! generator 3's points exactly.

use crate::seed::city::network::local::LocalNetwork;
use crate::seed::city::{CellInput, StageHash};
use crate::seed::hash::{unit, Seed};

/// Where a lot lies in its block.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LotShape {
    /// The block centre (a school or kindergarten).
    Civic,
    /// A lot of a row: frontage interval `a` along the long side (block parameter), the row's
    /// street edge `front`, its middle `mid` (across the short side) and the direction `sign`
    /// from the front inward (+1 or -1).
    Row { a: [f64; 2], front: f64, mid: f64, sign: f64 },
}

/// A kept lot: its block (index into [`LocalNetwork::blocks`]), its seed `h` (building id and
/// attributes) and its shape.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lot {
    pub block: u32,
    pub h: u64,
    pub shape: LotShape,
}

/// Output of the lot stage: the kept lots in block order (civic lot first, then the rows).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lots {
    pub list: Vec<Lot>,
}

impl Lots {
    /// Stage hash: every lot.
    pub fn hash(&self) -> u64 {
        let mut h = StageHash::new("lots");
        for l in &self.list {
            h.u(l.block as u64).u(l.h);
            match l.shape {
                LotShape::Civic => h.u(0),
                LotShape::Row { a, front, mid, sign } => h.u(1).f(a[0]).f(a[1]).f(front).f(mid).f(sign),
            };
        }
        h.finish()
    }
}

/// The lots of one cell's blocks.
pub fn build(inp: &CellInput, net: &LocalNetwork) -> Lots {
    let (d, s, k) = (inp.d, inp.style, inp.kinds);
    let mut out = Lots::default();
    let keep_p = (s.keep * (d.density / 0.6).clamp(0.25, 1.6)).min(1.0);
    let half_street = k.street.width_m * 0.5;
    for (bi, b) in net.blocks.iter().enumerate() {
        let (i, j, block) = (b.i, b.j, bi as u32);
        if k.loop_street.is_some() && unit(crate::seed_hash!(d.id, "school", i, j)) < 0.6 {
            out.list.push(Lot { block, h: crate::seed_hash!(d.id, "school", i, j, 1), shape: LotShape::Civic });
        }
        if s.keep <= 0.0 {
            continue;
        }
        // Long side = frontage; two rows of lots back to back.
        let (lu, lv) = (b.lu, b.lv);
        let (ll_m, ls_m) = if lu >= lv { (lu, lv) } else { (lv, lu) };
        let in_l = (half_street / ll_m).min(0.2);
        let in_s = (half_street / ls_m).min(0.2);
        let f = s.frontage_m[0] + (s.frontage_m[1] - s.frontage_m[0]) * unit(crate::seed_hash!(d.id, "front", i, j));
        let usable = ll_m * (1.0 - 2.0 * in_l);
        let n = ((usable / f.max(4.0)).floor() as i64).max(1);
        for r in 0..2i64 {
            let (front, sign) = if r == 0 { (in_s, 1.0) } else { (1.0 - in_s, -1.0) };
            let mid = 0.5 - sign * 0.5 / ls_m;
            let split = |k: i64| {
                let base = in_l + (1.0 - 2.0 * in_l) * k as f64 / n as f64;
                if k == 0 || k == n {
                    base
                } else {
                    base + (unit(crate::seed_hash!(d.id, "split", i, j, r, k)) * 2.0 - 1.0) * 0.2 * (1.0 - 2.0 * in_l) / n as f64
                }
            };
            for lot in 0..n {
                let h = crate::seed_hash!(d.id, "building", i, j, r, lot);
                if unit(crate::seed_hash!(Seed(h), "keep")) >= keep_p {
                    continue;
                }
                out.list.push(Lot { block, h, shape: LotShape::Row { a: [split(lot), split(lot + 1)], front, mid, sign } });
            }
        }
    }
    out
}
