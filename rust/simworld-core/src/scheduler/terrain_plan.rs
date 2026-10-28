//! The terrain layer's split plan (Terrain v2 G2): SSE splits under the tile budget
//! `terrain.max_tiles` (review M1), then the 2:1 balance. Pure in camera, view, scale, profile
//! and the hysteresis state; reused across frames while provably unchanged (`plan_memo`).

use super::plan_memo::PlanInputs;
use super::Scheduler;
use crate::geo::TileKey;
use crate::rings::{above_horizon, children, horizon_line_m, ViewPoint, MAX_RINGS};

/// A terrain split candidate of the plan (`plan_terrain_splits`): max-heap order = guard splits
/// first, then the largest `refine / distance`, then the key (deterministic).
#[derive(Clone, Copy, Debug)]
pub(super) struct SplitCandidate {
    prio: f64,
    guarded: bool,
    key: TileKey,
}

impl SplitCandidate {
    fn order_key(&self) -> (bool, u8, u32, u32) {
        (self.guarded, self.key.z, self.key.x, self.key.y)
    }
}

impl PartialEq for SplitCandidate {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == std::cmp::Ordering::Equal
    }
}
impl Eq for SplitCandidate {}
impl PartialOrd for SplitCandidate {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for SplitCandidate {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        self.guarded.cmp(&o.guarded).then(self.prio.total_cmp(&o.prio)).then_with(|| o.order_key().cmp(&self.order_key()))
    }
}

impl Scheduler {
    /// Terrain layer: the target split set of this frame (`t_split`), before the stateful pass.
    ///
    /// 1. SSE plan from the roots in the cover (`self.stack`): a node of ring > 0 splits when
    ///    its slant distance from the finer ring's look-ahead point is inside its refine distance
    ///    ([`Scheduler::terrain_refine_m`], with the hysteresis of its current state) and one of
    ///    its children is above the horizon (whole sibling groups). Candidates split in order of
    ///    `refine / distance` (largest screen error first; ties by key), the foundation guard's
    ///    first, while the leaves (roots + 3 per split) stay within `terrain.max_tiles` (0 = no
    ///    cap; review M1): over the budget the remaining SSE splits are skipped, guard splits
    ///    never. Without the budget hitting, the plan is the same set as an unordered walk.
    ///    When the balance (step 2) takes a capped plan over the budget, the plan runs once more
    ///    with the budget lowered by that overshoot.
    /// 2. 2:1 balance: wherever a split node's edge neighbour (same zoom) is not in the tree,
    ///    the neighbour's parent is split too, until nothing changes (the least balanced
    ///    superset: unique, independent of the order). So two terrain leaves that share an edge
    ///    differ by at most one zoom, as with the 1.7 distance bands: the skirts (sized for the
    ///    next coarser band, contract 9a) close every seam. Neighbours whose root is outside the
    ///    cover are skipped.
    ///
    /// A frame whose inputs provably give the last plan again reuses it (`plan_memo`).
    pub(super) fn plan_terrain_splits(&mut self, views: &[ViewPoint; MAX_RINGS], grounds: &[f64; MAX_RINGS]) {
        let inputs = PlanInputs { roots: self.stack.iter().map(|s| s.0).collect(), sse_m_per_m: self.sse_m_per_m, guard_m: self.guard_m };
        let entries = &self.entries;
        let split = |k: TileKey| entries.get(&k).is_some_and(|e| e.split);
        let probe_gen = self.probe_generation();
        if self.plan_memo.still_valid(&inputs, views, grounds, self.layer.hysteresis, probe_gen, split) {
            self.plan_memo.reused += 1;
            (self.balance_forced, self.budget_skipped) = self.plan_memo.counters;
            #[cfg(test)]
            if self.verify_plan_reuse {
                let kept = self.t_split.clone();
                self.plan_fresh(inputs, views, grounds);
                assert_eq!(kept, self.t_split, "a reused terrain plan differs from a fresh one");
            }
            return;
        }
        self.plan_fresh(inputs, views, grounds);
    }

    /// [`Scheduler::plan_terrain_splits`] from scratch, recording the plan's slack.
    fn plan_fresh(&mut self, inputs: PlanInputs, views: &[ViewPoint; MAX_RINGS], grounds: &[f64; MAX_RINGS]) {
        self.plan_memo.begin(inputs, views, grounds);
        self.plan_incomplete = false;
        let l = self.layer;
        let mut split = std::mem::take(&mut self.t_split);
        let mut roots = std::mem::take(&mut self.t_roots);
        let mut work = std::mem::take(&mut self.t_work);
        let mut heap = std::mem::take(&mut self.t_heap);
        split.clear();
        roots.clear();
        work.clear();
        heap.clear();
        roots.extend(self.stack.iter().map(|s| s.0));
        let budget = self.profile.terrain.max_tiles as usize;
        let mut limit = budget;
        let (mut planned, mut skipped);
        loop {
            split.clear();
            heap.clear();
            work.clear();
            work.extend(self.stack.iter().map(|s| s.0));
            let mut leaves = roots.len();
            skipped = 0;
            loop {
                // Candidates of this round (the roots, then the children of each split).
                while let Some(k) = work.pop() {
                    if let Some(c) = self.split_candidate(k, views, grounds) {
                        heap.push(c);
                    }
                }
                let Some(c) = heap.pop() else { break };
                if limit > 0 && !c.guarded && leaves + 3 > limit {
                    skipped += 1;
                    continue;
                }
                split.insert(c.key);
                leaves += 3;
                work.extend(children(c.key));
            }
            // 2:1 balance (closure).
            planned = split.len();
            work.extend(split.iter().copied());
            while let Some(c) = work.pop() {
                let n = 1i64 << c.z;
                for (dx, dy) in [(-1i64, 0i64), (1, 0), (0, -1), (0, 1)] {
                    let (x, y) = (c.x as i64 + dx, c.y as i64 + dy);
                    if x < 0 || y < 0 || x >= n || y >= n {
                        continue;
                    }
                    let nb = TileKey::new(c.z, x as u32, y as u32);
                    if c.z == l.root_zoom {
                        continue; // a root neighbour is in the cover or beyond its reach
                    }
                    let p = TileKey::new(c.z - 1, nb.x / 2, nb.y / 2);
                    if split.contains(&p) || (p.z == l.root_zoom && !roots.contains(&p)) {
                        continue;
                    }
                    split.insert(p);
                    work.push(p);
                }
            }
            // Over the budget only through the balance: plan once more with the budget lowered
            // by that overshoot (so the balanced cover lands near `max_tiles`).
            let total = roots.len() + 3 * split.len();
            if skipped == 0 || limit < budget || total <= budget {
                break;
            }
            limit = budget.saturating_sub(total - budget).max(1);
        }
        self.balance_forced = split.len() - planned;
        self.budget_skipped = skipped;
        // Without the budget binding the plan is the unordered set (no decision but the
        // recorded ones changes it): reusable. A plan that met a value still on the way (async
        // boxes) is reusable until the next probe batch arrives (`PlanMemo::still_valid`).
        let reusable = budget == 0 || (limit == budget && roots.len() + 3 * planned <= budget);
        let waits_on = self.plan_incomplete.then(|| self.probe_generation());
        self.plan_memo.finish(reusable, waits_on, (self.balance_forced, skipped));
        self.t_split = split;
        self.t_roots = roots;
        self.t_work = work;
        self.t_heap = heap;
    }

    /// Terrain layer: `k` as a split candidate of the plan (`None` = it does not want to split:
    /// ring 0, outside its refine distance, or no child above the horizon). Records the slack
    /// of each decision in the plan memo.
    fn split_candidate(&mut self, k: TileKey, views: &[ViewPoint; MAX_RINGS], grounds: &[f64; MAX_RINGS]) -> Option<SplitCandidate> {
        let l = self.layer;
        let ring = l.ring_for_zoom(k.z)?;
        if ring == 0 {
            return None;
        }
        let current = self.entries.get(&k).is_some_and(|e| e.split);
        // Async boxes: a value on the way holds a new split back (a split node has its values:
        // its box and its children's in their entries; its proxy is recomputed if evicted).
        let Some(g) = self.box_known(k) else { return self.plan_waits() };
        let d = views[ring - 1].slant_box_m(k, g[0], g[1]);
        let refine = match self.error_known(k) {
            Some(e) => (e * self.sse_m_per_m).max(self.guard_m[ring]),
            None if current => self.terrain_refine_m(k, ring),
            None => return self.plan_waits(),
        };
        if !current && !self.children_boxes_known(k, self.near_split(d, refine)) && l.inside(d, refine, current) {
            return self.plan_waits();
        }
        self.plan_memo.distance(ring - 1, k, d, refine, l.hysteresis, current);
        if !l.inside(d, refine, current) {
            return None;
        }
        if let Some(margin) = l.horizon_margin_m {
            // Whole sibling groups: split while one of the 4 children is above the horizon.
            let (view, ground) = (&views[ring - 1], grounds[ring - 1]);
            let mut any = false;
            for c in children(k) {
                let Some(cg) = self.box_known(c) else { return self.plan_waits() };
                let hi = cg[1] + l.default_tile_height_m;
                let line = horizon_line_m(view.alt, ground, cg[0], hi, margin);
                let h = view.horizontal_m(c);
                let h_ref = ground.min(cg[0]);
                self.plan_memo.horizon(ring - 1, k, h, line, l.hysteresis, current, (view.alt - h_ref).max(0.0), (hi - h_ref).max(0.0));
                if above_horizon(h, line, l.hysteresis, current) {
                    any = true;
                    break;
                }
            }
            if !any {
                return None;
            }
        }
        let guarded = l.inside(d, self.guard_m[ring], current);
        Some(SplitCandidate { prio: refine / d.max(1e-3), guarded, key: k })
    }

    /// A plan decision waits for a probe (async boxes): no split now, plan not reusable.
    fn plan_waits(&mut self) -> Option<SplitCandidate> {
        self.plan_incomplete = true;
        None
    }

    /// Terrain layer: plans reused and plans computed since the scheduler started.
    pub fn plan_reuse(&self) -> (u64, u64) {
        (self.plan_memo.reused, self.plan_memo.planned)
    }

    /// Terrain layer: SSE splits left out of the last plan by `terrain.max_tiles`.
    pub fn budget_skipped(&self) -> usize {
        self.budget_skipped
    }

    /// Terrain layer: splits the 2:1 balance added to the SSE plan in the last `update`.
    pub fn balance_forced(&self) -> usize {
        self.balance_forced
    }

}
