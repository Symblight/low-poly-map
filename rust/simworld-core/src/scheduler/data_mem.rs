//! The data store's side of the shared memory budget (pool `DataTiles`): the queued
//! [`MemOp`]s and their application ([`DataStore::sync`]), eviction back into the store and the
//! release on shutdown.

use crate::budget::{ItemKey, MemoryBudget, Pool};
use crate::geo::TileKey;

use super::data::{DataState, DataStore};

/// A queued change to the memory budget (applied by [`DataStore::sync`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum MemOp {
    Insert { id: u64, bytes: u64, prio: f64, pins: u32 },
    Pin { id: u64 },
    Unpin { id: u64 },
    Priority { id: u64, prio: f64 },
    Remove { id: u64 },
}

/// Packed key of a data tile in the budget pool (`(z << 58) | (x << 29) | y`).
pub(super) fn item_id(k: TileKey) -> u64 {
    crate::fetch::pack_key(k) as u64
}

pub(super) fn unpack(id: u64) -> TileKey {
    TileKey::new((id >> 58) as u8, ((id >> 29) & ((1 << 29) - 1)) as u32, (id & ((1 << 29) - 1)) as u32)
}

impl DataStore {
    /// Apply the queued budget changes, then drop the tiles the budget evicted.
    pub fn sync(&mut self, mem: &mut MemoryBudget) {
        for op in self.ops.drain(..) {
            match op {
                MemOp::Insert { id, bytes, prio, pins } => {
                    mem.insert_pinned(ItemKey::new(Pool::DataTiles, id), bytes, prio, pins);
                }
                MemOp::Pin { id } => {
                    mem.pin(ItemKey::new(Pool::DataTiles, id));
                }
                MemOp::Unpin { id } => {
                    mem.unpin(ItemKey::new(Pool::DataTiles, id));
                }
                MemOp::Priority { id, prio } => {
                    mem.set_priority(ItemKey::new(Pool::DataTiles, id), prio);
                }
                MemOp::Remove { id } => {
                    mem.remove(ItemKey::new(Pool::DataTiles, id));
                }
            }
        }
        self.evicted.clear();
        mem.take_evicted(Pool::DataTiles, &mut self.evicted);
        let mut evicted = std::mem::take(&mut self.evicted);
        for &id in &evicted {
            let k = unpack(id);
            if let Some(e) = self.tiles.get_mut(&k) {
                if matches!(e.state, DataState::Resident(_)) {
                    e.state = DataState::Absent;
                    e.idle_since = None;
                    self.evicted_budget += 1;
                    self.remember_evicted(k);
                }
            }
        }
        evicted.clear();
        self.evicted = evicted;
    }

    /// Unregister every resident tile from the budget (the scheduler is being replaced or shut
    /// down); the store is empty afterwards.
    pub fn release(&mut self, mem: &mut MemoryBudget) {
        self.ops.clear();
        for (k, e) in self.tiles.drain() {
            if matches!(e.state, DataState::Resident(_)) {
                mem.remove(ItemKey::new(Pool::DataTiles, item_id(k)));
            }
        }
        // Anything the budget evicted from this pool meanwhile is moot now.
        self.evicted.clear();
        mem.take_evicted(Pool::DataTiles, &mut self.evicted);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_ids_round_trip() {
        for k in [TileKey::new(14, 9341, 5206), TileKey::new(0, 0, 0), TileKey::new(20, (1 << 20) - 1, 12345)] {
            assert_eq!(unpack(item_id(k)), k);
        }
    }
}
