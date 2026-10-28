//! The recent-tile cache's policy (`recent`), and the scheduler serving a coarsening parent
//! from it on a flight (host rule checked every frame, outputs equal to a rebuild).

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{Kept, RecentTiles};
use crate::geo::TileKey;
use crate::mesher::MeshData;

fn kept(verts: usize) -> Kept {
    Kept {
        mesh: Arc::new(MeshData { positions: vec![[0.0; 3]; verts], indices: vec![0; 3], ..MeshData::default() }),
        ground: Arc::new(MeshData::default()),
        lights: Arc::default(),
        terrain: None,
        max_height: 10.0,
        building_count: 1,
    }
}

#[test]
fn held_entries_go_first_then_the_oldest_departures() {
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let size = kept(1000).bytes();
    // Room for exactly three entries.
    let mut c = RecentTiles::new(3.0 * size as f64 / 1_048_576.0, 10.0);
    let k = |i: u32| TileKey::new(15, i, 0);
    c.keep(k(1), kept(1000), at(0));
    c.keep(k(2), kept(1000), at(1));
    c.keep(k(3), kept(1000), at(2));
    c.left(k(1), at(3)); // 1 departed, 2 and 3 held
    c.keep(k(4), kept(1000), at(4)); // over the cap: the oldest held entry (2) goes
    assert!(c.get(k(2), at(4)).is_none());
    assert!(c.get(k(1), at(4)).is_some() && c.get(k(3), at(4)).is_some() && c.get(k(4), at(4)).is_some());
    // A departed entry lives `max_age` after it left; held ones do not age.
    assert!(c.get(k(1), at(13)).is_some());
    assert!(c.get(k(1), at(14)).is_none());
    c.expire(at(14));
    assert_eq!(c.stats().entries, 2);
    assert!(c.get(k(3), at(100)).is_some(), "held entries never expire");
    assert_eq!(c.stats().hits, 5);
}

/// The age-ordered cache (perf review 3 item 2) keeps exactly what the former full-scan policy
/// kept: eviction by `(gone, at, key)`, expiry of departures past the age limit. Random keeps,
/// departures, lookups and expiries against a plain model of that policy.
#[test]
fn ordered_eviction_matches_the_scan_policy() {
    use std::collections::HashMap;
    let t0 = Instant::now();
    let size = kept(100).bytes();
    let mut c = RecentTiles::new(8.0 * size as f64 / 1_048_576.0, 5.0);
    // Model: key -> (gone, at); every entry has the same size, so the cap is 8 entries.
    let mut m: HashMap<TileKey, (bool, Instant)> = HashMap::new();
    let mut rng = 0x2545_f491_4f6c_dd1du64;
    let mut next = |n: u64| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng % n
    };
    for step in 0..20_000u64 {
        let now = t0 + Duration::from_millis(step * 37);
        let k = TileKey::new(15, next(24) as u32, 0);
        match next(4) {
            0 | 1 => {
                c.keep(k, kept(100), now);
                m.insert(k, (false, now));
                while m.len() > 8 {
                    let (&first, _) = m.iter().min_by_key(|(key, (g, at))| (*g, *at, **key)).unwrap();
                    m.remove(&first);
                }
            }
            2 => {
                c.left(k, now);
                if let Some(e) = m.get_mut(&k) {
                    *e = (true, now);
                }
            }
            _ => {
                c.expire(now);
                m.retain(|_, (g, at)| !(*g && now.saturating_duration_since(*at) > Duration::from_secs(5)));
            }
        }
        let live = |(g, at): (bool, Instant)| !g || now.saturating_duration_since(at) <= Duration::from_secs(5);
        for i in 0..24 {
            let key = TileKey::new(15, i, 0);
            assert_eq!(c.get(key, now).is_some(), m.get(&key).is_some_and(|&e| live(e)), "step {step} key {key}");
        }
        assert_eq!(c.stats().entries, m.len(), "step {step}");
        assert_eq!(c.held.len() + c.gone.len(), m.len(), "step {step}: order sets out of step");
    }
}

#[test]
fn zero_megabytes_turns_the_cache_off() {
    let mut c = RecentTiles::new(0.0, 10.0);
    let now = Instant::now();
    c.keep(TileKey::new(14, 1, 1), kept(10), now);
    assert!(c.get(TileKey::new(14, 1, 1), now).is_none());
    assert_eq!(c.stats().entries, 0);
}

/// A level flight on the scheduler harness: parents dropped behind their children come back
/// from the cache when the camera has passed (hits), with the host rule checked every frame;
/// off (0 MB), every one of them is built again.
#[test]
fn coarsening_parents_come_back_from_the_cache() {
    use super::super::tests::Harness;
    let fly = |mb: f64| {
        let mut h = Harness::new();
        h.s.recent = RecentTiles::new(mb, 15.0);
        h.settle([0.0, 0.0, 300.0], 20_000);
        let (dt, mut y) = (1.0 / 30.0, 0.0);
        for _ in 0..900 {
            y += 250.0 * dt;
            let (_, un) = h.frame([0.0, y, 300.0], [0.0, 250.0, 0.0]);
            h.check_frame(&un, "recent cache flight");
        }
        let builds: u32 = h.s.cpu_stats().n.iter().sum();
        (h.s.recent_stats(), builds as u64)
    };
    let (on, builds_on) = fly(64.0);
    let (off, builds_off) = fly(0.0);
    eprintln!("cache on: {on:?}, {builds_on} builds; off: {off:?}, {builds_off} builds");
    assert!(on.hits > 0, "{on:?}");
    assert_eq!(off.hits, 0);
    assert_eq!(builds_off - builds_on, on.hits, "every hit saves one build");
}

/// Main-thread cost of the cache per frame at the size a long flight reaches (perf review 3
/// item 2, ignored; release): 1200 entries, a delivery that evicts and a departure every frame,
/// `expire` every frame. The age-ordered cache against the former policy (full scan per evicted
/// entry, `expire` walking every entry), thread CPU per frame.
/// `cargo test --release -p simworld-core perf_recent_cache -- --ignored --nocapture`
#[test]
#[ignore]
fn perf_recent_cache_frame_cost() {
    use std::collections::HashMap;
    const N: u32 = 1200;
    const FRAMES: u32 = 20_000;
    let size = kept(10).bytes();
    let t0 = Instant::now();
    let at = |f: u32| t0 + Duration::from_millis(f as u64 * 16);
    let k = |i: u32| TileKey::new(15, i % 4096, i / 4096);
    // Ordered cache.
    let mut c = RecentTiles::new(N as f64 * size as f64 / 1_048_576.0, 30.0);
    for i in 0..N {
        c.keep(k(i), kept(10), at(0));
        if i % 2 == 0 {
            c.left(k(i), at(0));
        }
    }
    let cpu = crate::diag::thread_cpu_us();
    for f in 1..=FRAMES {
        c.keep(k(N + f), kept(10), at(f));
        c.left(k(N + f - 7), at(f));
        c.expire(at(f));
        drop(c.take_released());
    }
    let ordered = (crate::diag::thread_cpu_us() - cpu) as f64 / FRAMES as f64;
    // The former policy on a plain map: (gone, at) per key.
    let mut m: HashMap<TileKey, (bool, Instant, Kept)> = HashMap::new();
    for i in 0..N {
        m.insert(k(i), (i % 2 == 0, at(0), kept(10)));
    }
    let cpu = crate::diag::thread_cpu_us();
    for f in 1..=FRAMES {
        m.insert(k(N + f), (false, at(f), kept(10)));
        while m.len() as u32 > N {
            let first = *m.iter().min_by_key(|(key, e)| (e.0, e.1, **key)).unwrap().0;
            m.remove(&first);
        }
        if let Some(e) = m.get_mut(&k(N + f - 7)) {
            (e.0, e.1) = (true, at(f));
        }
        let now = at(f);
        let old: Vec<TileKey> = m.iter().filter(|(_, e)| e.0 && now.saturating_duration_since(e.1) > Duration::from_secs(30)).map(|(k, _)| *k).collect();
        for key in old {
            m.remove(&key);
        }
    }
    let scan = (crate::diag::thread_cpu_us() - cpu) as f64 / FRAMES as f64;
    eprintln!("recent-tile cache, {N} entries, one eviction + one departure + expire per frame: ordered {ordered:.2} us / frame, full scan {scan:.2} us / frame");
}
