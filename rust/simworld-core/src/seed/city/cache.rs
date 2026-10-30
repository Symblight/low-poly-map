//! The district plan cache (`docs/spec-world-seed.md` section 6.1): cell plans keyed by
//! (city, cell), each behind an `Arc<OnceLock>` so the first worker that needs a plan builds it
//! and the others wait for that one build (the build is pure computation and never waits on
//! another job, so blocking a pool thread briefly cannot deadlock). At most `cap` plans are kept;
//! the oldest is forgotten first and rebuilt on demand with the same output. A job holding an
//! evicted plan's `Arc` finishes with it unchanged.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use super::CellPlan;
use super::hex::Ax;

type Key = (u32, Ax);
type Slot = Arc<OnceLock<Arc<CellPlan>>>;

#[derive(Debug, Default)]
struct Inner {
    map: HashMap<Key, Slot>,
    order: VecDeque<Key>,
}

/// Bounded cache of cell plans.
#[derive(Debug)]
pub struct PlanCache {
    cap: usize,
    inner: Mutex<Inner>,
    builds: AtomicU64,
    build_us: AtomicU64,
}

impl PlanCache {
    pub fn new(cap: usize) -> Self {
        Self { cap: cap.max(1), inner: Mutex::new(Inner::default()), builds: AtomicU64::new(0), build_us: AtomicU64::new(0) }
    }

    /// The plan of `(city, cell)`, built by `build` on a miss.
    pub fn get(&self, city: u32, a: Ax, build: impl FnOnce() -> CellPlan) -> Arc<CellPlan> {
        let slot = {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let key = (city, a);
            if let Some(s) = g.map.get(&key) {
                s.clone()
            } else {
                let s: Slot = Arc::new(OnceLock::new());
                g.map.insert(key, s.clone());
                g.order.push_back(key);
                while g.order.len() > self.cap {
                    if let Some(old) = g.order.pop_front() {
                        g.map.remove(&old);
                    }
                }
                s
            }
        };
        slot.get_or_init(|| {
            let t = std::time::Instant::now();
            let p = Arc::new(build());
            self.builds.fetch_add(1, Ordering::Relaxed);
            self.build_us.fetch_add(t.elapsed().as_micros() as u64, Ordering::Relaxed);
            p
        })
        .clone()
    }

    /// (plans held, bytes of the built ones, plans built so far, total build ms).
    pub fn stats(&self) -> (usize, usize, u64, f64) {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let bytes = g.map.values().filter_map(|s| s.get()).map(|p| p.bytes()).sum();
        (g.map.len(), bytes, self.builds.load(Ordering::Relaxed), self.build_us.load(Ordering::Relaxed) as f64 / 1000.0)
    }
}
