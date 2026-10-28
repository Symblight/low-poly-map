//! The bucketed stacking equals the plain scan it replaced, on generated overlaps and on the
//! densest real data tile (ignored, timing).

use std::sync::Arc;
use std::time::Instant;

use super::stack_raised_parts;
use crate::geo::{tile_for, Frame, TileKey};
use crate::mvt::{point_in_rings, Building, BuildingTags, OsmType, RawBuilding, TileProjector};

const VILNIUS: (f64, f64) = (54.6872, 25.2797);

/// The plain scan before the grid (reference): every raised part against every grounded
/// footprint.
fn stack_reference(buildings: &mut [Building]) {
    let raised: Vec<usize> = (0..buildings.len()).filter(|&i| buildings[i].min_height > 0.25).collect();
    for i in raised {
        let c = buildings[i].centroid_px;
        let mut best: Option<f32> = None;
        for (j, o) in buildings.iter().enumerate() {
            if j == i || o.min_height > 0.25 {
                continue;
            }
            if o.polygons.iter().any(|p| point_in_rings(c, p)) {
                best = Some(best.map_or(o.base_m, |b| b.max(o.base_m)));
            }
        }
        if let Some(b) = best {
            buildings[i].base_m = b;
        }
    }
}

fn bases(b: &[Building]) -> Vec<u32> {
    b.iter().map(|b| b.base_m.to_bits()).collect()
}

#[test]
fn bucketed_stacking_equals_the_plain_scan() {
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let key = tile_for(VILNIUS.0, VILNIUS.1, 14);
    let proj = TileProjector::new(key, frame, 4096.0);
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (1u64 << 53) as f64
    };
    for round in 0..20 {
        let mut bs = Vec::new();
        for id in 0..300u64 {
            // Squares and L shapes with holes, some sharing edges and corners, some on the
            // tile buffer; a third raised.
            let (x, y) = ((rnd() * 4400.0 - 150.0).round(), (rnd() * 4400.0 - 150.0).round());
            let s = (10.0 + rnd() * 300.0).round();
            let mut rings = vec![vec![[x, y], [x + s, y], [x + s, y + s / 2.0], [x + s / 2.0, y + s / 2.0], [x + s / 2.0, y + s], [x, y + s]]];
            if id % 5 == 0 {
                rings.push(vec![[x + 2.0, y + 2.0], [x + 4.0, y + 2.0], [x + 4.0, y + 4.0], [x + 2.0, y + 4.0]]);
            }
            let raw = RawBuilding {
                osm_type: OsmType::Way,
                id,
                kind: "yes".into(),
                height: Some(20.0),
                levels: None,
                min_height: if id % 3 == 0 { 8.0 } else if id % 7 == 0 { 0.25 } else { 0.0 },
                landuse: None,
                tags: BuildingTags::default(),
                polygons: vec![rings],
            };
            if let Some(mut b) = proj.finish(raw) {
                b.base_m = (rnd() * 50.0) as f32;
                if round == 0 && id == 4 {
                    // A part centred exactly on another footprint's corner / edge.
                    b.centroid_px = [x, y];
                }
                bs.push(b);
            }
        }
        // Centroids on grounded corners and edges.
        let n = bs.len();
        for i in (0..n).step_by(9) {
            let corner = bs[(i + 1) % n].polygons[0][0][0];
            if bs[i].min_height > 0.25 {
                bs[i].centroid_px = corner;
            }
        }
        let mut a = bs.clone();
        let mut b = bs.clone();
        stack_reference(&mut a);
        stack_raised_parts(&mut b);
        assert_eq!(bases(&a), bases(&b), "round {round}");
        let moved = bases(&a).iter().zip(bases(&bs)).filter(|(x, y)| *x != y).count();
        assert!(moved > 10, "round {round}: only {moved} parts stacked");
        // The direct scan (a few raised parts): keep the first 3 raised parts only.
        let mut few = bs.clone();
        let mut kept = 0;
        for b in few.iter_mut().filter(|b| b.min_height > 0.25) {
            kept += 1;
            if kept > 3 {
                b.min_height = 0.0;
            }
        }
        let (mut a, mut b) = (few.clone(), few);
        stack_reference(&mut a);
        stack_raised_parts(&mut b);
        assert_eq!(bases(&a), bases(&b), "round {round}, 3 raised parts");
    }
}

/// The densest detail tile of the real two-level archive (`SIMWORLD_V2_ARCHIVE`), decoded with
/// the terrain on: the grid gives the plain scan's bases bit for bit; prints both timings.
#[test]
#[ignore]
fn real_archive_densest_tile_stacking_timing() {
    let Ok(path) = std::env::var("SIMWORLD_V2_ARCHIVE") else {
        eprintln!("SIMWORLD_V2_ARCHIVE not set; skipped");
        return;
    };
    let (url, _) = crate::data_world_tests::serve(std::fs::read(&path).expect("archive"));
    let dir = std::env::temp_dir().join(format!("simworld-stack-{}", std::process::id()));
    let fetcher = crate::fetch::Fetcher::new(
        &url,
        &dir.to_string_lossy(),
        Arc::new(std::sync::atomic::AtomicU64::new(0)),
        crate::fetch::FetchParams::default(),
        Box::new(|_| {}),
    )
    .unwrap();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut w = crate::profile::WorldProfile::default();
    w.terrain.enabled = true;
    let profile = crate::profile::Profile::new(w).unwrap();
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let terrain = crate::terrain::Terrain::from_profile(&profile, VILNIUS.0, VILNIUS.1);
    let decode = |key: TileKey| {
        let (bytes, _) = rt.block_on(fetcher.fetch(key)).ok()?;
        crate::mvt::decode_tile_on(&bytes, key, frame, &terrain, true, true, &profile).ok()
    };
    // The densest tile named by the review, and the tile with the largest raised parts x
    // grounded footprints product around Vilnius (the plain scan's worst case).
    let mut worst = (0usize, TileKey::new(14, 9342, 5207));
    for x in 9325..9360 {
        for y in 5190..5225 {
            if let Some(d) = decode(TileKey::new(14, x, y)) {
                let r = d.buildings.iter().filter(|b| b.min_height > 0.25).count();
                worst = worst.max((r * (d.buildings.len() - r), TileKey::new(14, x, y)));
            }
        }
    }
    for key in [TileKey::new(14, 9342, 5207), worst.1] {
        stacking_timing(&decode(key).expect("tile").buildings, key, "");
    }
    // Every building of the densest tile with a raised part on it (an old town mapped with
    // `building:part`): the plain scan's quadratic case.
    let mut dense = decode(TileKey::new(14, 9342, 5207)).expect("tile").buildings;
    let parts: Vec<Building> = dense.iter().map(|b| Building { min_height: 6.0, base_m: 0.0, ..b.clone() }).collect();
    dense.extend(parts);
    stacking_timing(&dense, TileKey::new(14, 9342, 5207), " with a raised part on every building");
    let _ = std::fs::remove_dir_all(&dir);
}

fn stacking_timing(buildings: &[Building], key: TileKey, what: &str) {
    let raised = buildings.iter().filter(|b| b.min_height > 0.25).count();
    let time = |f: fn(&mut [Building])| {
        let mut best = f64::INFINITY;
        let mut out = Vec::new();
        for _ in 0..5 {
            let mut bs = buildings.to_vec();
            let t = Instant::now();
            f(&mut bs);
            best = best.min(t.elapsed().as_secs_f64() * 1000.0);
            out = bs;
        }
        (best, out)
    };
    let (t_ref, a) = time(stack_reference);
    let (t_grid, b) = time(stack_raised_parts);
    eprintln!("tile {key}{what}: {} buildings, {raised} raised parts; plain scan {t_ref:.3} ms, grid {t_grid:.3} ms", buildings.len());
    assert_eq!(bases(&a), bases(&b));
}
