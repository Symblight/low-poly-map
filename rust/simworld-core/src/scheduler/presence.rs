//! The contract's "present" / "effective" / "deliverable" rules, memoized (perf review 2,
//! item 5).
//!
//! `present(k)` = held, or all 4 children present: for a tile whose subtree is split it visits
//! the subtree down to the held tiles, and `effective` asks it for the tile and its 3 siblings at
//! every level up to the root. `best_ready` asked that for every ready tile, so one call walked
//! the same subtrees again and again (4^depth per sibling group). Presence only changes when a
//! tile's `held` flag does, so a memo is exact for as long as no tile is taken, dropped or
//! unloaded: [`Presence`] lives for one pass (one `best_ready` call, the cover walk of one
//! `update`) and is cleared before the next.

use ahash::AHashMap;

use super::Scheduler;
use crate::geo::TileKey;
use crate::rings::children;

/// Presence of the tiles one pass has asked about (not held ones: those are a lookup).
pub(super) type Presence = AHashMap<TileKey, bool>;

impl Scheduler {
    /// Contract "present": uploaded, or all 4 children present.
    pub fn present(&self, k: TileKey) -> bool {
        self.present_in(k, &mut Presence::new())
    }

    /// [`Scheduler::present`] through `memo` (valid while no tile's `held` flag changes).
    pub(super) fn present_in(&self, k: TileKey, memo: &mut Presence) -> bool {
        if self.held(k) {
            return true;
        }
        if k.z >= self.layer.leaf_zoom {
            return false;
        }
        if let Some(&v) = memo.get(&k) {
            return v;
        }
        let v = children(k).iter().all(|&c| self.present_in(c, memo));
        memo.insert(k, v);
        v
    }

    /// Present, and its sibling group present all the way up to the root: children delivered
    /// under it can never overlap a coarser visible tile.
    fn effective_in(&self, k: TileKey, memo: &mut Presence) -> bool {
        let mut k = k;
        loop {
            if !self.present_in(k, memo) {
                return false;
            }
            let Some(p) = self.layer.parent(k) else { return true };
            if !self.layer.siblings(k).iter().all(|&s| self.present_in(s, memo)) {
                return false;
            }
            k = p;
        }
    }

    /// Root, or the parent is effective and not part of a fade (no tile in two fades).
    #[cfg(test)]
    pub(super) fn deliverable(&self, k: TileKey) -> bool {
        self.deliverable_in(k, &mut Presence::new())
    }

    /// [`Scheduler::deliverable`] through `memo`.
    pub(super) fn deliverable_in(&self, k: TileKey, memo: &mut Presence) -> bool {
        k.z == self.layer.root_zoom || self.layer.parent(k).is_some_and(|p| self.effective_in(p, memo) && !self.in_fade(p))
    }
}

#[cfg(test)]
#[path = "presence_tests.rs"]
mod tests;
