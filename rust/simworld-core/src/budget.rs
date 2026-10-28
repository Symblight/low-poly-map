//! Memory budget (Terrain v2 spec section 6, step M1): one byte budget, profile
//! `memory.budget_mb`, shared by the core's CPU-side pools.
//!
//! * Named pools ([`Pool`]): decoded data tiles (Tiles v2 T2), HeightTiles (Terrain v2 G1),
//!   collision patches. GPU layers are not part of this budget (the host caps them; the terrain
//!   layer later through `terrain.max_tiles`).
//! * Every item is registered by its owner with a byte size and a priority (higher = keep longer;
//!   the owner derives it from distance / SSE). When the total exceeds the budget, items are
//!   evicted lowest priority first across all pools (ties: pool, then id, so the order is
//!   deterministic). Pinned items (an in-flight job depends on them) are never evicted; when the
//!   pinned items alone exceed the budget, the overshoot is reported in [`MemoryStats`], never
//!   hidden.
//! * Evicted ids are queued per pool; the owner drains them ([`MemoryBudget::take_evicted`]) and
//!   drops its data. The budget holds sizes only, never the data.
//! * Fixed-size buffer free lists for height grids ([`BufferPool`]): a released buffer is kept
//!   for the next take (counters `allocs` / `reuses`), so steady state allocates nothing. Free
//!   buffers count against the budget and are the first thing dropped when it is exceeded.
//!
//! Pure and single-threaded (owned by `World`, used on the main thread); no clock, no global
//! state.

use std::collections::BTreeSet;

use ahash::AHashMap;

/// Pools sharing the budget. The discriminant indexes the per-pool arrays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Pool {
    /// Decoded vector data tiles (Tiles v2, step T2).
    DataTiles = 0,
    /// Terrain HeightTiles (Terrain v2, step G1).
    HeightTiles = 1,
    /// Collision patches.
    Collision = 2,
}

pub const POOL_COUNT: usize = 3;

impl Pool {
    pub const ALL: [Pool; POOL_COUNT] = [Pool::DataTiles, Pool::HeightTiles, Pool::Collision];

    /// Name in `debug_stats()["memory_mb"]`.
    pub fn name(self) -> &'static str {
        match self {
            Pool::DataTiles => "data_tiles",
            Pool::HeightTiles => "height_tiles",
            Pool::Collision => "collision",
        }
    }

    fn idx(self) -> usize {
        self as usize
    }
}

/// An item: owner-chosen id within a pool (e.g. a packed tile key).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemKey {
    pub pool: Pool,
    pub id: u64,
}

impl ItemKey {
    pub fn new(pool: Pool, id: u64) -> Self {
        Self { pool, id }
    }
}

/// Bytes per MiB.
pub const MIB: f64 = 1_048_576.0;

/// Byte count from a profile value in MiB (non-finite or negative = 0).
pub fn mb_to_bytes(mb: f64) -> u64 {
    if mb.is_finite() && mb > 0.0 {
        (mb * MIB) as u64
    } else {
        0
    }
}

/// Elements of a height grid at the default 64 quads per side (+ 3 for the border ring and
/// the morph row: 67 x 67, Terrain v2 section 6).
pub const HEIGHT_GRID_LEN: usize = 67 * 67;

/// Most free buffers kept per [`BufferPool`] (beyond this a released buffer is freed).
pub const MAX_FREE_BUFFERS: usize = 512;

/// Total order on priorities: NaN sorts lowest (evicted first), -0 equals 0.
#[derive(Clone, Copy, Debug)]
struct Prio(f64);

impl Prio {
    fn new(p: f64) -> Self {
        Prio(if p.is_nan() { f64::NEG_INFINITY } else { p + 0.0 })
    }
}
impl PartialEq for Prio {
    fn eq(&self, o: &Self) -> bool {
        self.0.total_cmp(&o.0).is_eq()
    }
}
impl Eq for Prio {}
impl PartialOrd for Prio {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Prio {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&o.0)
    }
}

#[derive(Clone, Copy, Debug)]
struct Item {
    bytes: u64,
    prio: Prio,
    pins: u32,
}

/// Fixed-size buffer free list (one element type, one length).
#[derive(Debug)]
pub struct BufferPool<T> {
    len: usize,
    free: Vec<Vec<T>>,
    allocs: u64,
    reuses: u64,
}

impl<T: Copy + Default> BufferPool<T> {
    fn new(len: usize) -> Self {
        Self { len, free: Vec::new(), allocs: 0, reuses: 0 }
    }

    fn buffer_bytes(&self) -> u64 {
        (self.len * std::mem::size_of::<T>()) as u64
    }

    fn free_bytes(&self) -> u64 {
        self.free.len() as u64 * self.buffer_bytes()
    }

    /// A buffer of exactly `len` elements: a reused one (contents unspecified: the last user's)
    /// or a new zeroed one.
    fn take(&mut self) -> Vec<T> {
        match self.free.pop() {
            Some(b) => {
                self.reuses += 1;
                b
            }
            None => {
                self.allocs += 1;
                vec![T::default(); self.len]
            }
        }
    }

    /// Keep `buf` for reuse; a buffer of another length (or capacity) is dropped.
    fn give(&mut self, mut buf: Vec<T>) -> bool {
        if buf.capacity() != self.len || self.free.len() >= MAX_FREE_BUFFERS {
            return false;
        }
        buf.resize(self.len, T::default());
        self.free.push(buf);
        true
    }

    fn stats(&self) -> BufferStats {
        BufferStats { len: self.len, free: self.free.len(), allocs: self.allocs, reuses: self.reuses, free_bytes: self.free_bytes() }
    }
}

/// Counters of one [`BufferPool`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BufferStats {
    /// Elements per buffer.
    pub len: usize,
    /// Buffers on the free list.
    pub free: usize,
    /// Buffers allocated (a take with an empty free list).
    pub allocs: u64,
    /// Takes served from the free list.
    pub reuses: u64,
    pub free_bytes: u64,
}

/// Counters of one pool.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PoolStats {
    pub bytes: u64,
    pub items: u32,
    pub pinned_items: u32,
    pub pinned_bytes: u64,
    /// Items evicted by the budget since the world was created (owner removals do not count).
    pub evictions: u64,
}

/// Snapshot for `debug_stats()["memory_mb"]` and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryStats {
    pub budget_bytes: u64,
    /// Items of every pool plus the free buffers.
    pub total_bytes: u64,
    /// `total_bytes - budget_bytes` when only pinned items (and nothing evictable) are left
    /// above the budget; 0 otherwise.
    pub over_budget_bytes: u64,
    /// Enforcement passes that ended over the budget (pinned items alone exceeded it).
    pub overshoots: u64,
    pub pools: [PoolStats; POOL_COUNT],
    pub height_f32: BufferStats,
    pub height_f64: BufferStats,
}

impl MemoryStats {
    pub fn pool(&self, p: Pool) -> &PoolStats {
        &self.pools[p.idx()]
    }
    /// Bytes on the buffer free lists.
    pub fn free_buffer_bytes(&self) -> u64 {
        self.height_f32.free_bytes + self.height_f64.free_bytes
    }
}

/// The byte budget. See the module docs.
#[derive(Debug)]
pub struct MemoryBudget {
    budget_bytes: u64,
    items: AHashMap<ItemKey, Item>,
    /// Unpinned items, lowest priority first (the eviction order).
    evictable: BTreeSet<(Prio, ItemKey)>,
    pools: [PoolStats; POOL_COUNT],
    item_bytes: u64,
    overshoots: u64,
    evicted: [Vec<u64>; POOL_COUNT],
    height_f32: BufferPool<f32>,
    height_f64: BufferPool<f64>,
}

impl MemoryBudget {
    pub fn new(budget_bytes: u64) -> Self {
        Self {
            budget_bytes,
            items: AHashMap::new(),
            evictable: BTreeSet::new(),
            pools: [PoolStats::default(); POOL_COUNT],
            item_bytes: 0,
            overshoots: 0,
            evicted: std::array::from_fn(|_| Vec::new()),
            height_f32: BufferPool::new(HEIGHT_GRID_LEN),
            height_f64: BufferPool::new(HEIGHT_GRID_LEN),
        }
    }

    pub fn budget_bytes(&self) -> u64 {
        self.budget_bytes
    }

    /// Items plus free buffers.
    pub fn total_bytes(&self) -> u64 {
        self.item_bytes + self.height_f32.free_bytes() + self.height_f64.free_bytes()
    }

    /// Change the budget (a reconfigure); evicts at once if the new budget is smaller.
    pub fn set_budget_bytes(&mut self, bytes: u64) {
        self.budget_bytes = bytes;
        self.enforce();
    }

    /// Register `key` (or update its size and priority; pins are kept), then enforce the
    /// budget. Returns whether `key` is still resident: false = it was itself the lowest
    /// priority and got evicted (it is then also in the pool's evicted queue).
    pub fn insert(&mut self, key: ItemKey, bytes: u64, priority: f64) -> bool {
        self.insert_pinned(key, bytes, priority, 0)
    }

    /// [`MemoryBudget::insert`] with `pins` pins added before the budget is enforced, so an item
    /// that running jobs already depend on is never evicted by its own insertion.
    pub fn insert_pinned(&mut self, key: ItemKey, bytes: u64, priority: f64, pins: u32) -> bool {
        let prio = Prio::new(priority);
        let old_pins = match self.items.get(&key).copied() {
            Some(old) => {
                self.detach(key, old);
                old.pins
            }
            None => 0,
        };
        self.attach(key, Item { bytes, prio, pins: old_pins + pins });
        self.enforce();
        self.items.contains_key(&key)
    }

    /// New priority for a registered item (no eviction: the total is unchanged). False if
    /// `key` is not registered.
    pub fn set_priority(&mut self, key: ItemKey, priority: f64) -> bool {
        let Some(it) = self.items.get_mut(&key) else { return false };
        let new = Prio::new(priority);
        if it.pins == 0 {
            self.evictable.remove(&(it.prio, key));
            self.evictable.insert((new, key));
        }
        it.prio = new;
        true
    }

    /// The owner dropped `key` (pinned or not); not counted as an eviction.
    pub fn remove(&mut self, key: ItemKey) -> bool {
        match self.items.get(&key).copied() {
            Some(it) => {
                self.detach(key, it);
                true
            }
            None => false,
        }
    }

    /// Protect `key` from eviction (counted: n pins need n unpins). False if not registered.
    pub fn pin(&mut self, key: ItemKey) -> bool {
        let Some(it) = self.items.get_mut(&key) else { return false };
        if it.pins == 0 {
            self.evictable.remove(&(it.prio, key));
            let p = &mut self.pools[key.pool.idx()];
            p.pinned_items += 1;
            p.pinned_bytes += it.bytes;
        }
        it.pins += 1;
        true
    }

    /// Release one pin; the last one makes the item evictable again and enforces the budget.
    /// False if not registered or not pinned.
    pub fn unpin(&mut self, key: ItemKey) -> bool {
        let Some(it) = self.items.get_mut(&key) else { return false };
        if it.pins == 0 {
            return false;
        }
        it.pins -= 1;
        if it.pins == 0 {
            let (prio, bytes) = (it.prio, it.bytes);
            self.evictable.insert((prio, key));
            let p = &mut self.pools[key.pool.idx()];
            p.pinned_items -= 1;
            p.pinned_bytes -= bytes;
            self.enforce();
        }
        true
    }

    pub fn contains(&self, key: ItemKey) -> bool {
        self.items.contains_key(&key)
    }

    pub fn is_pinned(&self, key: ItemKey) -> bool {
        self.items.get(&key).is_some_and(|i| i.pins > 0)
    }

    /// Move the ids the budget evicted from `pool` (in eviction order) into `out`; the owner
    /// drops their data. `out` is appended to, so a reused buffer allocates nothing.
    pub fn take_evicted(&mut self, pool: Pool, out: &mut Vec<u64>) {
        out.append(&mut self.evicted[pool.idx()]);
    }

    /// A height grid buffer of [`HEIGHT_GRID_LEN`] f32 (reused from the free list when possible).
    pub fn take_height_f32(&mut self) -> Vec<f32> {
        self.height_f32.take()
    }
    pub fn take_height_f64(&mut self) -> Vec<f64> {
        self.height_f64.take()
    }
    /// Return a buffer for reuse. Kept only if it has the pool's size and fits in the budget;
    /// returns whether it was kept.
    pub fn give_height_f32(&mut self, buf: Vec<f32>) -> bool {
        let fits = self.total_bytes() + self.height_f32.buffer_bytes() <= self.budget_bytes;
        fits && self.height_f32.give(buf)
    }
    pub fn give_height_f64(&mut self, buf: Vec<f64>) -> bool {
        let fits = self.total_bytes() + self.height_f64.buffer_bytes() <= self.budget_bytes;
        fits && self.height_f64.give(buf)
    }

    /// Forget every item, queue and free buffer (shutdown). Counters are kept.
    pub fn clear(&mut self) {
        self.items.clear();
        self.evictable.clear();
        for p in &mut self.pools {
            *p = PoolStats { evictions: p.evictions, ..PoolStats::default() };
        }
        self.item_bytes = 0;
        for q in &mut self.evicted {
            q.clear();
        }
        self.height_f32.free.clear();
        self.height_f64.free.clear();
    }

    pub fn stats(&self) -> MemoryStats {
        let total = self.total_bytes();
        MemoryStats {
            budget_bytes: self.budget_bytes,
            total_bytes: total,
            over_budget_bytes: total.saturating_sub(self.budget_bytes),
            overshoots: self.overshoots,
            pools: self.pools,
            height_f32: self.height_f32.stats(),
            height_f64: self.height_f64.stats(),
        }
    }

    fn attach(&mut self, key: ItemKey, it: Item) {
        let p = &mut self.pools[key.pool.idx()];
        p.bytes += it.bytes;
        p.items += 1;
        if it.pins > 0 {
            p.pinned_items += 1;
            p.pinned_bytes += it.bytes;
        } else {
            self.evictable.insert((it.prio, key));
        }
        self.item_bytes += it.bytes;
        self.items.insert(key, it);
    }

    fn detach(&mut self, key: ItemKey, it: Item) {
        let p = &mut self.pools[key.pool.idx()];
        p.bytes -= it.bytes;
        p.items -= 1;
        if it.pins > 0 {
            p.pinned_items -= 1;
            p.pinned_bytes -= it.bytes;
        } else {
            self.evictable.remove(&(it.prio, key));
        }
        self.item_bytes -= it.bytes;
        self.items.remove(&key);
    }

    /// Free buffers first, then unpinned items lowest priority first, until the total fits.
    fn enforce(&mut self) {
        while self.total_bytes() > self.budget_bytes {
            if self.height_f64.free.pop().is_some() || self.height_f32.free.pop().is_some() {
                continue;
            }
            let Some((prio, key)) = self.evictable.pop_first() else {
                self.overshoots += 1;
                return;
            };
            debug_assert!(self.items[&key].prio == prio);
            let it = self.items[&key];
            self.detach(key, it);
            self.pools[key.pool.idx()].evictions += 1;
            self.evicted[key.pool.idx()].push(key.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: Pool = Pool::DataTiles;
    const H: Pool = Pool::HeightTiles;
    const C: Pool = Pool::Collision;

    fn k(pool: Pool, id: u64) -> ItemKey {
        ItemKey::new(pool, id)
    }

    fn evicted(b: &mut MemoryBudget, pool: Pool) -> Vec<u64> {
        let mut v = Vec::new();
        b.take_evicted(pool, &mut v);
        v
    }

    /// The invariant: within the budget, or nothing evictable is left.
    fn check(b: &MemoryBudget) {
        let s = b.stats();
        let sum: u64 = s.pools.iter().map(|p| p.bytes).sum();
        assert_eq!(s.total_bytes, sum + s.free_buffer_bytes());
        if s.total_bytes > s.budget_bytes {
            assert!(b.evictable.is_empty(), "over budget with evictable items: {s:?}");
            assert_eq!(s.free_buffer_bytes(), 0);
            let pinned: u64 = s.pools.iter().map(|p| p.pinned_bytes).sum();
            assert_eq!(pinned, sum, "only pinned items may exceed the budget");
        }
    }

    #[test]
    fn evicts_lowest_priority_first_across_pools() {
        let mut b = MemoryBudget::new(100);
        assert!(b.insert(k(D, 1), 30, 5.0));
        assert!(b.insert(k(H, 2), 30, 1.0));
        assert!(b.insert(k(C, 3), 30, 3.0));
        check(&b);
        // 120 > 100: the lowest (H 2, priority 1) goes.
        assert!(b.insert(k(D, 4), 30, 4.0));
        assert_eq!(evicted(&mut b, H), vec![2]);
        assert!(evicted(&mut b, D).is_empty());
        // A big item pushes out C 3 (3.0) then D 4 (4.0).
        assert!(b.insert(k(H, 5), 60, 10.0));
        assert_eq!(evicted(&mut b, C), vec![3]);
        assert_eq!(evicted(&mut b, D), vec![4]);
        assert!(b.contains(k(D, 1)) && b.contains(k(H, 5)));
        check(&b);
        // An insert that is itself the lowest is evicted at once.
        assert!(!b.insert(k(C, 6), 20, 0.5));
        assert_eq!(evicted(&mut b, C), vec![6]);
        let s = b.stats();
        assert_eq!((s.pool(D).evictions, s.pool(H).evictions, s.pool(C).evictions), (1, 1, 2));
        assert_eq!(s.total_bytes, 90);
    }

    #[test]
    fn ties_break_by_pool_then_id_and_nan_goes_first() {
        let mut b = MemoryBudget::new(1000);
        for id in [7, 3, 5] {
            b.insert(k(H, id), 100, 2.0);
            b.insert(k(D, id), 100, 2.0);
        }
        b.insert(k(C, 9), 100, f64::NAN);
        b.insert(k(C, 10), 100, -0.0);
        b.set_budget_bytes(300);
        assert_eq!(evicted(&mut b, C), vec![9, 10]);
        assert_eq!(evicted(&mut b, D), vec![3, 5, 7]);
        assert!(evicted(&mut b, H).is_empty());
        assert_eq!(b.total_bytes(), 300);
        check(&b);
    }

    #[test]
    fn pinned_items_are_never_evicted_and_overshoot_is_reported() {
        let mut b = MemoryBudget::new(100);
        b.insert(k(D, 1), 60, 0.0);
        assert!(b.pin(k(D, 1)));
        assert!(b.pin(k(D, 1)));
        b.insert(k(H, 2), 30, 9.0);
        // The pinned lowest-priority item stays; the new item is now the lowest and goes.
        assert!(!b.insert(k(C, 3), 30, 5.0));
        assert_eq!(evicted(&mut b, C), vec![3]);
        assert!(b.contains(k(D, 1)));
        // A higher one pushes out H 2.
        assert!(b.insert(k(C, 4), 30, 10.0));
        assert_eq!(evicted(&mut b, H), vec![2]);
        check(&b);
        // Pinned items alone over the budget: nothing to evict, the overshoot is reported.
        assert!(b.pin(k(C, 4)));
        b.set_budget_bytes(50);
        let s = b.stats();
        assert_eq!((s.total_bytes, s.over_budget_bytes, s.overshoots), (90, 40, 1));
        assert_eq!((s.pool(D).pinned_items, s.pool(D).pinned_bytes), (1, 60));
        check(&b);
        // An item inserted with pins is protected from its own insertion.
        assert!(b.insert_pinned(k(H, 7), 500, -100.0, 1));
        assert!(b.is_pinned(k(H, 7)) && b.contains(k(H, 7)));
        assert!(b.unpin(k(H, 7)));
        assert_eq!(evicted(&mut b, H), vec![7]);
        // Updating a pinned item keeps its pins.
        b.insert(k(D, 1), 70, 0.0);
        assert!(b.is_pinned(k(D, 1)));
        assert_eq!(b.stats().over_budget_bytes, 50);
        // Two pins on D 1: one unpin keeps it protected.
        assert!(b.unpin(k(D, 1)));
        assert!(b.contains(k(D, 1)) && b.is_pinned(k(D, 1)));
        // Last unpin: D 1 is evicted at once, C 4 is still pinned and within the budget.
        assert!(b.unpin(k(D, 1)));
        assert_eq!(evicted(&mut b, D), vec![1]);
        assert!(!b.unpin(k(D, 1)));
        assert_eq!(b.stats().over_budget_bytes, 0);
        assert!(b.unpin(k(C, 4)));
        assert!(!b.unpin(k(C, 4)));
        assert!(b.contains(k(C, 4)));
        check(&b);
    }

    #[test]
    fn remove_and_priority_updates() {
        let mut b = MemoryBudget::new(100);
        b.insert(k(D, 1), 40, 1.0);
        b.insert(k(D, 2), 40, 2.0);
        assert!(b.set_priority(k(D, 1), 3.0));
        assert!(!b.set_priority(k(D, 9), 3.0));
        b.insert(k(D, 3), 40, 2.5);
        assert_eq!(evicted(&mut b, D), vec![2]);
        assert!(b.remove(k(D, 1)));
        assert!(!b.remove(k(D, 1)));
        b.pin(k(D, 3));
        assert!(b.remove(k(D, 3)), "pinned items can be removed by their owner");
        let s = b.stats();
        assert_eq!((s.total_bytes, s.pool(D).items, s.pool(D).pinned_items, s.pool(D).evictions), (0, 0, 0, 1));
    }

    #[test]
    fn height_buffers_are_reused_and_count_against_the_budget() {
        let buf = (HEIGHT_GRID_LEN * 4) as u64;
        let mut b = MemoryBudget::new(64 * 1024 * 1024);
        // Steady state: take / give cycles allocate only the peak working set.
        for _ in 0..100 {
            let a = b.take_height_f32();
            let c = b.take_height_f32();
            assert_eq!(a.len(), HEIGHT_GRID_LEN);
            assert!(b.give_height_f32(a));
            assert!(b.give_height_f32(c));
        }
        let s = b.stats().height_f32;
        assert_eq!((s.allocs, s.reuses, s.free), (2, 198, 2));
        assert_eq!(b.total_bytes(), 2 * buf);
        let d = b.take_height_f64();
        assert!(b.give_height_f64(d));
        assert!(!b.give_height_f64(vec![0.0; 10]), "wrong size is dropped");
        assert_eq!(b.stats().free_buffer_bytes(), 2 * buf + 2 * buf);
        // Over budget: free buffers go before any item.
        b.insert(k(H, 1), 64 * 1024 * 1024 - 2 * buf, 0.0);
        assert!(b.contains(k(H, 1)));
        assert_eq!(b.stats().free_buffer_bytes(), 2 * buf);
        check(&b);
        // A give that would not fit is dropped.
        b.insert(k(H, 1), 64 * 1024 * 1024 - 2 * buf, 0.0);
        let x = vec![0.0f32; HEIGHT_GRID_LEN];
        assert!(!b.give_height_f32(x));
        check(&b);
        b.clear();
        let s = b.stats();
        assert_eq!((s.total_bytes, s.free_buffer_bytes(), s.pool(H).items), (0, 0, 0));
    }

    /// The default budget holds the Tiles v2 / Terrain v2 working set with room to spare:
    /// 28 overview tiles of 1.1 MB, 18 detail tiles of 0.2 MB and 400 HeightTiles (f32 + f64).
    #[test]
    fn default_budget_holds_the_planned_working_set() {
        let mut b = MemoryBudget::new(mb_to_bytes(crate::profile::WorldProfile::default().memory.budget_mb));
        let mut id = 0;
        for (n, bytes) in [(28u64, 1_100_000u64), (18, 200_000)] {
            for _ in 0..n {
                id += 1;
                assert!(b.insert(k(D, id), bytes, id as f64));
            }
        }
        for i in 0..400 {
            assert!(b.insert(k(H, i), (HEIGHT_GRID_LEN * 12) as u64, i as f64));
        }
        let s = b.stats();
        assert_eq!(s.pool(D).evictions + s.pool(H).evictions, 0);
        assert!(s.total_bytes * 4 < s.budget_bytes, "{} of {}", s.total_bytes, s.budget_bytes);
    }

    /// Random operations (fixed seed): the invariant holds after every step and the result is
    /// the same on every run.
    #[test]
    fn random_operations_keep_the_invariant_deterministically() {
        fn run() -> (MemoryStats, Vec<u64>) {
            let mut b = MemoryBudget::new(10_000);
            let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
            let mut next = || {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                s
            };
            let mut out = Vec::new();
            for _ in 0..5000 {
                let r = next();
                let key = k(Pool::ALL[(r % 3) as usize], (r >> 8) % 64);
                match (r >> 20) % 8 {
                    0..=3 => {
                        b.insert(key, (r >> 32) % 1500, ((r >> 40) % 100) as f64);
                    }
                    4 => {
                        b.pin(key);
                    }
                    5 => {
                        b.unpin(key);
                    }
                    6 => {
                        b.set_priority(key, ((r >> 40) % 100) as f64);
                    }
                    _ => {
                        b.remove(key);
                    }
                }
                check(&b);
                for p in Pool::ALL {
                    b.take_evicted(p, &mut out);
                }
            }
            (b.stats(), out)
        }
        let a = run();
        assert!(a.1.len() > 100, "the run evicts");
        assert_eq!(a, run());
    }
}
