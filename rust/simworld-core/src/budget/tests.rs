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
