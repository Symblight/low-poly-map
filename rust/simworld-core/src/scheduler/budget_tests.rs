//! Terrain cover under the tile budget `terrain.max_tiles` (review M1), on the scheduler test
//! harness of [`super::tests`].

use std::collections::HashSet;

use super::tests::{terrain_harness, terrain_setup};
use crate::geo::TileKey;

/// Review M1, terrain: `terrain.max_tiles` bounds the SSE cover (leaves). Without the cap the
/// plan is unchanged (same held set as an uncapped run at the default relief); a steep relief
/// at a narrow fov stays near the budget, the guard splits are kept and the cover is valid.
#[test]
fn terrain_cover_is_capped_by_the_tile_budget() {
    let run = |amp: f64, fov: f64, detail: f64, max_tiles: u32| {
        let (p, t) = terrain_setup(|tp| {
            tp.amplitude_m = amp;
            tp.max_tiles = max_tiles;
        });
        let mut h = terrain_harness(p, t);
        h.s.set_lod_view(&crate::rings::LodView::new(fov, 1080.0, [0.0, 0.0, -1.0]).unwrap(), 1.0);
        h.s.set_detail(detail);
        h.settle([0.0, 0.0, 300.0], 40_000);
        let held: HashSet<TileKey> = h.s.delivered_keys().into_iter().collect();
        (held, h.s.budget_skipped())
    };
    let verbose = std::env::var("SIMWORLD_M1_TERRAIN").is_ok();
    if verbose {
        for (amp, fov, detail) in [(450.0, 60.0, 1.0), (450.0, 30.0, 1.0), (450.0, 15.0, 1.0), (450.0, 60.0, 0.25), (1200.0, 60.0, 1.0), (1200.0, 30.0, 1.0)] {
            let (a, _) = run(amp, fov, detail, 0);
            let (b, sk) = run(amp, fov, detail, crate::profile::WorldProfile::default().terrain.max_tiles);
            eprintln!("terrain amp {amp} fov {fov} detail {detail}: uncapped {} tiles, default cap {} (skipped {sk})", a.len(), b.len());
        }
    }
    // Default relief and view: the default budget does not bind (same cover as no cap).
    let (free, _) = run(450.0, 60.0, 1.0, 0);
    let (dflt, sk) = run(450.0, 60.0, 1.0, crate::profile::WorldProfile::default().terrain.max_tiles);
    assert_eq!((free.clone(), sk), (dflt, 0));
    // Steep relief, fov 30: capped to 600 leaves (+ the 2:1 balance).
    let (big, _) = run(1200.0, 30.0, 1.0, 0);
    let (capped, skipped) = run(1200.0, 30.0, 1.0, 600);
    eprintln!("amp 1200 fov 30: {} uncapped, {} with max_tiles 600 ({skipped} splits skipped)", big.len(), capped.len());
    assert!(big.len() > 900 && skipped > 0, "{} {skipped}", big.len());
    assert!(capped.len() <= 700, "{}", capped.len());
}
