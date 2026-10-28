//! Reuse of the terrain split plan across frames (perf review 2026-10-28, item 1).
//!
//! The plan ([`super::Scheduler::plan_terrain_splits`]) is a pure function of the roots in the
//! cover, the view points (and the ground under them), the SSE scale, the guard distances, the
//! tile budget and the hysteresis state (`split`) of the nodes it visits. Without the budget
//! binding, the plan is the set the unordered walk accepts, so it only changes when one of its
//! threshold decisions flips. While planning, every decision records its **slack**: how far
//! the compared value is from the threshold it was compared with (a decision inside the
//! hysteresis band records the threshold of the state it saw, and the node's state is checked
//! on reuse; any other decision is on the same side of both thresholds). A later frame reuses
//! the plan when nothing but the view points changed and a bound on how far any compared value
//! can have moved since the plan is below the smallest slack: then every decision, and so the
//! plan, is the one a fresh plan would make (`plan_reuse_equals_a_fresh_plan` checks it frame
//! by frame). The cover stays a pure function of its inputs.
//!
//! Bounds, per view point `r` (old `o`, new `n`): a horizontal distance moves by at most
//! `|w_n - w_o| m_n + h |m_n - m_o| / m_o` (Mercator units `w`, metres per unit `m` at the view's
//! latitude, `h` <= the largest compared distance), a slant distance by that plus `|alt_n -
//! alt_o|`; the horizon line `sqrt(2R max(alt - ref, 0)) + sqrt(2R max(hi - ref, 0))` (`ref` =
//! min(ground, tile min)) by `dsqrt(x_1, |d alt| + |d ground|) + dsqrt(x_2, |d ground|)` with
//! `dsqrt(x, d) = min(sqrt(2R d), sqrt(2R) d / sqrt(x))` and `x` the smallest argument seen.

use crate::geo::TileKey;
use crate::rings::{ViewPoint, HORIZON_EARTH_RADIUS_M, MAX_RINGS};

/// Absolute margin (m) added to every bound (rounding of the distance functions).
const EPS_M: f64 = 1e-3;

/// The plan's inputs other than the view points and the hysteresis state.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct PlanInputs {
    pub roots: Vec<TileKey>,
    pub sse_m_per_m: f64,
    pub guard_m: [f64; MAX_RINGS],
}

/// Slack of the decisions taken from one view point.
#[derive(Clone, Copy, Debug)]
struct ViewSlack {
    /// Smallest |distance - threshold| of the slant-distance decisions, and the largest distance.
    dist: f64,
    dist_max: f64,
    /// The same for the horizon decisions (horizontal distance against the horizon line), the
    /// largest horizontal distance, and the smallest arguments of the line's two square roots.
    horizon: f64,
    horizon_max: f64,
    x_cam: f64,
    x_tile: f64,
}

impl Default for ViewSlack {
    fn default() -> Self {
        let inf = f64::INFINITY;
        Self { dist: inf, dist_max: 0.0, horizon: inf, horizon_max: 0.0, x_cam: inf, x_tile: inf }
    }
}

/// The last plan's validity record.
#[derive(Clone, Debug, Default)]
pub(super) struct PlanMemo {
    /// Whether the record below belongs to the current plan (`t_split`).
    valid: bool,
    /// The plan waited on a probe (async boxes): valid only while no probe batch arrived, i.e.
    /// while the scheduler's probe generation is still this one (perf review 3 item 2).
    waits_on: Option<u64>,
    inputs: Option<PlanInputs>,
    views: Vec<(ViewPoint, f64)>,
    slack: [ViewSlack; MAX_RINGS],
    /// Nodes with a decision inside the hysteresis band, and the `split` state it saw.
    band: Vec<(TileKey, bool)>,
    /// The plan's counters (`balance_forced`, `budget_skipped`).
    pub counters: (usize, usize),
    /// Plans reused / computed since the scheduler started.
    pub reused: u64,
    pub planned: u64,
}

/// `|sqrt(2R a) - sqrt(2R b)|` for `|a - b| <= d` and `a >= x` (both clamped at 0).
fn dsqrt(x: f64, d: f64) -> f64 {
    let r2 = 2.0 * HORIZON_EARTH_RADIUS_M;
    if d <= 0.0 {
        return 0.0;
    }
    let holder = (r2 * d).sqrt();
    if x > 0.0 {
        holder.min(r2.sqrt() * d / x.sqrt())
    } else {
        holder
    }
}

impl PlanMemo {
    /// Start recording a fresh plan.
    pub fn begin(&mut self, inputs: PlanInputs, views: &[ViewPoint; MAX_RINGS], grounds: &[f64; MAX_RINGS]) {
        self.valid = false;
        self.inputs = Some(inputs);
        self.views.clear();
        self.views.extend(views.iter().zip(grounds).map(|(v, g)| (*v, *g)));
        self.slack = [ViewSlack::default(); MAX_RINGS];
        self.band.clear();
        self.planned += 1;
    }

    /// Finish the record: `reusable` = the budget did not bind (the plan is the unordered set);
    /// `waits_on` = the probe generation the plan saw when it waited on a probe (`None` = it
    /// did not).
    pub fn finish(&mut self, reusable: bool, waits_on: Option<u64>, counters: (usize, usize)) {
        self.valid = reusable;
        self.waits_on = waits_on;
        self.counters = counters;
    }

    /// A slant-distance decision `inside(d, thr, current)` from view `r` (thresholds `thr` and
    /// `thr * hyst` for a node in the cover).
    pub fn distance(&mut self, r: usize, k: TileKey, d: f64, thr: f64, hyst: f64, current: bool) {
        let s = &mut self.slack[r];
        s.dist_max = s.dist_max.max(d);
        s.dist = s.dist.min(Self::side(&mut self.band, k, d, thr, hyst, current));
    }

    /// A horizon decision `above_horizon(h, line, hyst, current)` from view `r`; `x_cam`, `x_tile`
    /// = the arguments of the line's square roots.
    #[allow(clippy::too_many_arguments)]
    pub fn horizon(&mut self, r: usize, k: TileKey, h: f64, line: f64, hyst: f64, current: bool, x_cam: f64, x_tile: f64) {
        let s = &mut self.slack[r];
        s.horizon_max = s.horizon_max.max(h);
        s.x_cam = s.x_cam.min(x_cam);
        s.x_tile = s.x_tile.min(x_tile);
        s.horizon = s.horizon.min(Self::side(&mut self.band, k, h, line, hyst, current));
    }

    /// Slack of `v` against `thr` / `thr * hyst`; a value between the two records the node.
    fn side(band: &mut Vec<(TileKey, bool)>, k: TileKey, v: f64, thr: f64, hyst: f64, current: bool) -> f64 {
        let hi = thr * hyst;
        if !(v.is_finite() && thr.is_finite()) {
            return 0.0;
        }
        if v < thr {
            hi.min(thr) - v
        } else if v > hi.max(thr) {
            v - hi.max(thr)
        } else {
            band.push((k, current));
            if current {
                hi - v
            } else {
                v - thr
            }
        }
    }

    /// Whether the plan of the record is the plan a fresh run would make now. `split(k)` = the
    /// node's hysteresis state now; `probe_gen` = the scheduler's probe generation now.
    ///
    /// A plan that waited on a probe made every other decision with the slack recorded, and
    /// each wait depends only on which boxes and error proxies are known: until a probe batch
    /// arrives the waits are the same (a wait is a node that does not split, and the value it
    /// waits for is still on the way), so the plan is reused like a complete one.
    #[allow(clippy::too_many_arguments)]
    pub fn still_valid(
        &self,
        inputs: &PlanInputs,
        views: &[ViewPoint; MAX_RINGS],
        grounds: &[f64; MAX_RINGS],
        hyst: f64,
        probe_gen: u64,
        split: impl Fn(TileKey) -> bool,
    ) -> bool {
        if !self.valid || self.inputs.as_ref() != Some(inputs) || self.waits_on.is_some_and(|g| g != probe_gen) {
            return false;
        }
        for (r, ((o, og), (n, ng))) in self.views.iter().zip(views.iter().zip(grounds)).enumerate() {
            let s = &self.slack[r];
            if s.dist.is_infinite() && s.horizon.is_infinite() {
                continue; // no decision from this view point
            }
            let du = crate::math::hypot(n.wx - o.wx, n.wy - o.wy) * n.m_per_unit.max(o.m_per_unit);
            let rho = (n.m_per_unit - o.m_per_unit).abs() / o.m_per_unit;
            let (dalt, dg) = ((n.alt - o.alt).abs(), (ng - og).abs());
            if !(du.is_finite() && rho.is_finite() && dalt.is_finite() && dg.is_finite()) {
                return false;
            }
            if du + s.dist_max * rho + dalt + EPS_M >= s.dist {
                return false;
            }
            let dline = dsqrt(s.x_cam, dalt + dg) + dsqrt(s.x_tile, dg);
            if s.horizon.is_finite() && du + s.horizon_max * rho + hyst.max(1.0) * dline + EPS_M >= s.horizon {
                return false;
            }
        }
        self.band.iter().all(|&(k, cur)| split(k) == cur)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqrt_bound_holds() {
        let r2 = 2.0 * HORIZON_EARTH_RADIUS_M;
        for x in [0.0, 0.5, 3.0, 300.0, 5000.0] {
            for d in [0.0, 1e-4, 0.01, 1.0, 40.0] {
                let real = (r2 * (x + d)).sqrt() - (r2 * x).sqrt();
                assert!(real <= dsqrt(x, d) + 1e-9, "x {x} d {d}: {real} > {}", dsqrt(x, d));
                if x >= d {
                    let down = (r2 * x).sqrt() - (r2 * (x - d)).sqrt();
                    assert!(down <= dsqrt(x - d, d) + 1e-9);
                }
            }
        }
    }

    #[test]
    fn decisions_in_the_band_record_their_state() {
        let mut m = PlanMemo::default();
        let k = TileKey::new(12, 1, 1);
        m.distance(0, k, 50.0, 100.0, 1.15, false); // inside both: slack 50
        m.distance(0, k, 200.0, 100.0, 1.15, true); // outside both: slack 85
        assert!(m.band.is_empty());
        assert_eq!(m.slack[0].dist, 50.0);
        m.distance(0, k, 110.0, 100.0, 1.15, true); // band, kept by the hysteresis
        assert_eq!(m.band, vec![(k, true)]);
        assert!((m.slack[0].dist - 5.0).abs() < 1e-9);
    }
}
