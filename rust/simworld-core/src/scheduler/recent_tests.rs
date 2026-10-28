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
