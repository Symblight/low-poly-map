//! A bounded memo with gradual eviction (two generations).
//!
//! Entries go into the young generation; when it holds `cap / 2` entries the old generation is
//! dropped and the young one becomes old. A hit in the old generation moves the entry back to
//! the young one, so the entries still in use survive a rotation and only the ones not used for
//! a whole generation are dropped. Compared with clearing the whole memo when it is full, the
//! next frames do not recompute everything at once (the per-frame cost spike of the terrain
//! caches, perf review 2026-10-28).
//!
//! Only for memos of pure functions: an evicted value is recomputed to the same value, so the
//! cache never changes a result.

use ahash::AHashMap;
use std::hash::Hash;

#[derive(Debug, Clone)]
pub struct GenCache<K, V> {
    young: AHashMap<K, V>,
    old: AHashMap<K, V>,
    half: usize,
}

impl<K: Hash + Eq + Copy, V: Copy> GenCache<K, V> {
    /// A cache of at most `cap` entries (at least 2).
    pub fn new(cap: usize) -> Self {
        Self { young: AHashMap::new(), old: AHashMap::new(), half: (cap / 2).max(1) }
    }

    /// The cached value of `k` (an old-generation hit is moved to the young generation).
    pub fn get(&mut self, k: &K) -> Option<V> {
        if let Some(v) = self.young.get(k) {
            return Some(*v);
        }
        let v = self.old.remove(k)?;
        self.insert(*k, v);
        Some(v)
    }

    /// Whether `k` is cached (no promotion).
    pub fn contains(&self, k: &K) -> bool {
        self.young.contains_key(k) || self.old.contains_key(k)
    }

    pub fn insert(&mut self, k: K, v: V) {
        if self.young.len() >= self.half && !self.young.contains_key(&k) {
            // Rotate: drop the old generation, keep its allocation for the next young one.
            std::mem::swap(&mut self.young, &mut self.old);
            self.young.clear();
        }
        self.young.insert(k, v);
    }

    pub fn remove(&mut self, k: &K) {
        self.young.remove(k);
        self.old.remove(k);
    }

    pub fn clear(&mut self) {
        self.young.clear();
        self.old.clear();
    }

    /// Entries in both generations.
    pub fn len(&self) -> usize {
        self.young.len() + self.old.len()
    }

    pub fn is_empty(&self) -> bool {
        self.young.is_empty() && self.old.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::GenCache;

    #[test]
    fn bounded_and_keeps_the_entries_in_use() {
        let mut c = GenCache::new(8);
        for k in 0..4u32 {
            c.insert(k, k * 10);
        }
        // Key 0 is used in every round; the others are inserted once.
        for k in 4..100u32 {
            assert_eq!(c.get(&0), Some(0), "key 0 evicted at {k}");
            c.insert(k, k * 10);
            assert!(c.len() <= 8, "{} entries", c.len());
        }
        assert_eq!(c.get(&99), Some(990));
        assert_eq!(c.get(&5), None, "an unused entry is dropped after two rotations");
        c.remove(&0);
        assert!(!c.contains(&0));
        c.clear();
        assert!(c.is_empty());
    }
}
