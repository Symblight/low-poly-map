//! Terrain v2 G1: HeightTile lattice, parent copy, borders, quantization, buffers, error proxy.

use std::collections::BTreeSet;

use super::*;
use crate::budget::MemoryBudget;
use crate::geo::{tile_for, TileKey};
use crate::profile::TerrainProfile;
use crate::seed::world::test_world;
use crate::terrain::sample::lattice_lat_lon;

const VILNIUS: (f64, f64) = (54.6872, 25.2797);
const N: i64 = GRID_QUADS as i64;

/// The bench's seed spots (see `seed::drape_tests`).
const SPOTS: [(&str, f64, f64); 4] =
    [("coast", 55.85, 21.06), ("lake", 54.855, 24.17), ("eyvi", 54.6341, 25.2858), ("eyka", 54.9639, 24.0848)];

/// Default terrain with its flat zone at Vilnius.
fn plain() -> Terrain {
    Terrain::new(&TerrainProfile { enabled: true, ..TerrainProfile::default() }, VILNIUS.0, VILNIUS.1)
}

/// Default terrain without a flat zone.
fn no_flat() -> Terrain {
    Terrain::new(
        &TerrainProfile { enabled: true, flat_radius_m: 0.0, flat_blend_m: 0.0, ..TerrainProfile::default() },
        VILNIUS.0,
        VILNIUS.1,
    )
}

fn seed_terrain() -> &'static Terrain {
    &test_world().1
}

fn build(t: &Terrain, key: TileKey, parent: Option<&HeightTile>) -> HeightTile {
    HeightTile::build(t, key, HeightQuant::for_terrain(t), parent, HeightBuffers::new())
}

/// Tiles of `(lat, lon)` at zooms `z0..=z1`, plus the east and south neighbours.
fn tiles_at(lat: f64, lon: f64, z0: u8, z1: u8) -> Vec<TileKey> {
    let mut v = Vec::new();
    for z in z0..=z1 {
        let k = tile_for(lat, lon, z);
        v.push(k);
        v.push(TileKey::new(z, k.x + 1, k.y));
        v.push(TileKey::new(z, k.x, k.y + 1));
    }
    v
}

/// Every sample equals `terrain.height` at its lattice point, bit for bit.
fn assert_exact(t: &Terrain, tile: &HeightTile, what: &str) {
    let key = tile.key();
    for j in -1..=N + 1 {
        for i in -1..=N + 1 {
            let (lat, lon) = lattice_lat_lon(key, GRID_QUADS as u32, i, j);
            let want = t.height(lat, lon);
            assert_eq!(tile.get(i, j).to_bits(), want.to_bits(), "{what} {key} sample ({i}, {j}): {} != {want}", tile.get(i, j));
        }
    }
}

/// (2) The row / column trig precompute and the parent copy are bit-identical to the height
/// function, for the plain terrain (with and without flat zone) and the seed world (coast, lake,
/// both runways), zooms 10..16.
#[test]
fn every_sample_equals_the_height_function() {
    let plain = plain();
    let no_flat = no_flat();
    let seed = seed_terrain();
    let mut cases: Vec<(&str, &Terrain, TileKey)> = Vec::new();
    for k in tiles_at(VILNIUS.0 + 0.02, VILNIUS.1 + 0.03, 10, 16) {
        cases.push(("plain", &plain, k));
    }
    for k in tiles_at(54.9, 23.9, 10, 16) {
        cases.push(("no_flat", &no_flat, k));
    }
    for (name, lat, lon) in SPOTS {
        for z in 10..=16u8 {
            cases.push((name, seed, tile_for(lat, lon, z)));
        }
    }
    let mut copied = 0usize;
    for (what, t, key) in cases {
        let alone = build(t, key, None);
        assert_eq!(alone.copied_samples(), 0);
        assert_exact(t, &alone, what);
        let parent = build(t, parent_key(key), None);
        let child = build(t, key, Some(&parent));
        assert_eq!(child.copied_samples() as usize, SHARED_WITH_PARENT, "{what} {key}");
        let bits = |t: &HeightTile| t.heights().iter().map(|h| h.to_bits()).collect::<Vec<u64>>();
        assert_eq!(bits(&child), bits(&alone), "{what} {key}: the parent copy changes nothing");
        assert_eq!((child.r16(), child.range()), (alone.r16(), alone.range()), "{what} {key}");
        copied += 1;
        // A tile that is not the parent is ignored.
        let other = build(t, TileKey::new(key.z - 1, (key.x >> 1) + 1, key.y >> 1), None);
        assert_eq!(build(t, key, Some(&other)).copied_samples(), 0);
    }
    assert!(copied >= 60, "{copied}");
}

/// (1) Child even interior samples coincide with the parent's samples bit for bit (all four
/// quadrants), computed independently.
#[test]
fn shared_samples_equal_the_parent() {
    for (what, t) in [("no_flat", &no_flat()), ("seed", seed_terrain())] {
        for (lat, lon) in [(54.9, 23.9), (SPOTS[2].1, SPOTS[2].2), (SPOTS[0].1, SPOTS[0].2)] {
            for z in [11u8, 13, 15] {
                let p = tile_for(lat, lon, z);
                let parent = build(t, p, None);
                for (dx, dy) in [(0u32, 0u32), (1, 0), (0, 1), (1, 1)] {
                    let key = TileKey::new(z + 1, 2 * p.x + dx, 2 * p.y + dy);
                    let child = build(t, key, None);
                    let (ox, oy) = (dx as i64 * N / 2, dy as i64 * N / 2);
                    for b in 0..=N / 2 {
                        for a in 0..=N / 2 {
                            assert_eq!(
                                child.get(2 * a, 2 * b).to_bits(),
                                parent.get(ox + a, oy + b).to_bits(),
                                "{what} {key} ({a}, {b})"
                            );
                        }
                    }
                }
            }
        }
    }
}

/// (1) Border samples are the neighbours' first interior samples, and the edges are shared.
#[test]
fn borders_equal_the_neighbours() {
    for (what, t) in [("no_flat", &no_flat()), ("seed", seed_terrain())] {
        for z in [10u8, 13, 16] {
            let k = tile_for(SPOTS[3].1, SPOTS[3].2, z);
            let a = build(t, k, None);
            let east = build(t, TileKey::new(z, k.x + 1, k.y), None);
            let south = build(t, TileKey::new(z, k.x, k.y + 1), None);
            for m in -1..=N + 1 {
                assert_eq!(a.get(N + 1, m).to_bits(), east.get(1, m).to_bits(), "{what} {k} east border {m}");
                assert_eq!(a.get(N, m).to_bits(), east.get(0, m).to_bits(), "{what} {k} east edge {m}");
                assert_eq!(east.get(-1, m).to_bits(), a.get(N - 1, m).to_bits(), "{what} {k} west border {m}");
                assert_eq!(a.get(m, N + 1).to_bits(), south.get(m, 1).to_bits(), "{what} {k} south border {m}");
                assert_eq!(a.get(m, N).to_bits(), south.get(m, 0).to_bits(), "{what} {k} south edge {m}");
                assert_eq!(south.get(m, -1).to_bits(), a.get(m, N - 1).to_bits(), "{what} {k} north border {m}");
            }
        }
    }
}

/// (1) R16 with the world-wide quantum round-trips within quantum / 2; the range is the interior.
#[test]
fn quantization_round_trips_within_half_a_quantum() {
    for (what, t) in [("plain", &plain()), ("seed", seed_terrain())] {
        let q = HeightQuant::for_terrain(t);
        let [lo, hi] = t.bounds();
        assert_eq!(q.offset_m, lo);
        assert!((q.decode(65535) - hi).abs() < 1e-9, "{what}");
        assert!(q.quantum_m < 0.02, "{what}: quantum {}", q.quantum_m);
        for (name, lat, lon) in SPOTS {
            for z in [10u8, 14, 16] {
                let tile = build(t, tile_for(lat, lon, z), None);
                let (mut rlo, mut rhi) = (f64::INFINITY, f64::NEG_INFINITY);
                for (k, (&h, &v)) in tile.heights().iter().zip(tile.r16()).enumerate() {
                    assert!((q.decode(v) - h).abs() <= q.quantum_m * 0.500_001, "{what} {name} z{z} sample {k}: {h} -> {v}");
                    let (i, j) = ((k % GRID_SIDE) as i64 - 1, (k / GRID_SIDE) as i64 - 1);
                    if (0..=N).contains(&i) && (0..=N).contains(&j) {
                        rlo = rlo.min(h);
                        rhi = rhi.max(h);
                    }
                }
                assert_eq!(tile.range(), [rlo, rhi]);
                assert_eq!(tile.quant(), q);
            }
        }
    }
    // Flat terrain: every height and code is 0.
    let flat = Terrain::flat();
    let tile = build(&flat, TileKey::new(14, 9345, 5216), None);
    assert!(tile.heights().iter().all(|h| h.to_bits() == 0) && tile.r16().iter().all(|&v| v == 0));
}

/// (4) Buffers come from the budget's free list and go back to it; a reused buffer gives the
/// same tile.
#[test]
fn buffers_come_from_the_budget_and_are_reused() {
    let t = no_flat();
    let mut budget = MemoryBudget::new(64 << 20);
    let key = tile_for(54.9, 23.9, 14);
    let a = HeightTile::build(&t, key, HeightQuant::for_terrain(&t), None, HeightBuffers::from_budget(&mut budget, None));
    let r16 = a.clone().release(&mut budget);
    assert_eq!(budget.stats().height_f64.free, 1);
    // A worker fills buffers it was handed (here on another thread).
    let bufs = HeightBuffers::from_budget(&mut budget, Some(r16));
    let b = std::thread::scope(|s| s.spawn(|| HeightTile::build(&t, key, HeightQuant::for_terrain(&t), None, bufs)).join().unwrap());
    assert_eq!(a, b);
    let st = budget.stats().height_f64;
    assert_eq!((st.allocs, st.reuses), (1, 1));
}

// ----------------------------------------------------------------------------------------------
// Error proxy
// ----------------------------------------------------------------------------------------------

/// Brute-force error of drawing `tile` (bilinear over its grid) against the height function
/// sampled `density` x denser (grid points excluded: they are exact).
fn measured_error(t: &Terrain, tile: &HeightTile, density: i64) -> f64 {
    let key = tile.key();
    let cells = N * density;
    let mut worst = 0.0f64;
    for b in 0..=cells {
        for a in 0..=cells {
            if a % density == 0 && b % density == 0 {
                continue;
            }
            let (lat, lon) = lattice_lat_lon(key, cells as u32, a, b);
            let g = tile.sample_frac(a as f64 / cells as f64, b as f64 / cells as f64);
            worst = worst.max((t.height(lat, lon) - g).abs());
        }
    }
    worst
}

/// Measured error of many tiles on all cores.
fn measure_all(t: &Terrain, keys: &[TileKey], density: i64) -> Vec<f64> {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let chunk = keys.len().div_ceil(threads).max(1);
    std::thread::scope(|s| {
        let hs: Vec<_> = keys
            .chunks(chunk)
            .map(|c| s.spawn(move || c.iter().map(|&k| measured_error(t, &build(t, k, None), density)).collect::<Vec<f64>>()))
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
    })
}

fn proxy_for(t: &Terrain) -> ErrorProxy {
    let p = &test_world().0;
    ErrorProxy::from_profile(t, p)
}

/// Tiles of the seed world touched by an edit at zooms `z0..=z1`: along every runway of EYVI and
/// EYKA (centreline and strip edges) and across their shoulders, over Kauno marios and the
/// Baltic coast north of Klaipeda.
fn edit_tiles(z0: u8, z1: u8, per_zoom: usize) -> Vec<TileKey> {
    let (_, t, w) = test_world();
    let mut pts: Vec<(f64, f64)> = Vec::new();
    for g in w.airports.iter().filter(|g| g.ident == "EYVI" || g.ident == "EYKA") {
        for r in &g.runways {
            for k in 0..=12 {
                let s = r.length_m * k as f64 / 12.0;
                for tt in [0.0, r.strip_half_m, -r.strip_half_m, r.strip_half_m + 75.0, -r.strip_half_m - 75.0] {
                    pts.push(g.frame.ll(r.at(s, tt)));
                }
            }
        }
    }
    for (_, lat, lon) in SPOTS[..2].iter() {
        for j in -3..=3 {
            for i in -3..=3 {
                pts.push((lat + j as f64 * 0.01, lon + i as f64 * 0.016));
            }
        }
    }
    let proxy = proxy_for(t);
    let mut out = Vec::new();
    for z in z0..=z1 {
        let set: BTreeSet<(u32, u32)> = pts.iter().map(|&(a, b)| tile_for(a, b, z)).map(|k| (k.x, k.y)).collect();
        let keys: Vec<TileKey> = set.into_iter().map(|(x, y)| TileKey::new(z, x, y)).filter(|&k| proxy.edit_touch(k).any()).collect();
        // Spread the sample over the set.
        let step = (keys.len() / per_zoom.max(1)).max(1);
        out.extend(keys.into_iter().step_by(step).take(per_zoom));
    }
    out
}

/// (3) Proxy properties: pure (two proxies agree, memo = raw clamp), zero without terrain,
/// monotonic child <= parent over whole subtrees, edited tiles below the minimum zoom get their
/// whole range, no edits outside a seed world, flat open sea is (almost) free.
#[test]
fn error_proxy_properties() {
    let t = no_flat();
    let mut a = ErrorProxy::new(&t, 10);
    let mut b = ErrorProxy::new(&t, 10);
    let root = tile_for(54.9, 23.9, 10);
    // Walk four levels below the root (1 + 4 + 16 + 64 + 256 keys) in two orders.
    let mut keys = vec![root];
    let mut level = vec![root];
    for _ in 0..4 {
        level = level.iter().flat_map(|k| (0..4).map(move |q| TileKey::new(k.z + 1, 2 * k.x + (q & 1), 2 * k.y + (q >> 1)))).collect();
        keys.extend(level.iter().copied());
    }
    let fwd: Vec<u64> = keys.iter().map(|&k| a.error_m(k).to_bits()).collect();
    let back: Vec<u64> = keys.iter().rev().map(|&k| b.error_m(k).to_bits()).collect::<Vec<_>>().into_iter().rev().collect();
    assert_eq!(fwd, back, "memoized proxy depends on the query order");
    let mut violations_raw = 0;
    for &k in &keys[1..] {
        let (e, ep) = (a.error_m(k), a.error_m(parent_key(k)));
        assert!(e <= ep, "{k}: {e} > parent {ep}");
        assert!(e > 0.0 && e.is_finite());
        violations_raw += (a.raw_error_m(k) > a.raw_error_m(parent_key(k))) as usize;
        assert_eq!(a.edit_touch(k), EditTouch::default());
    }
    eprintln!("raw proxy above its parent's raw for {violations_raw} of {} keys", keys.len() - 1);
    // Off = 0.
    let mut off = ErrorProxy::new(&Terrain::flat(), 10);
    assert_eq!(off.error_m(root), 0.0);
    // Seed world: edited tiles below EDIT_MIN_ZOOM carry their whole range.
    let seed = seed_terrain();
    let proxy = proxy_for(seed);
    for (name, lat, lon) in SPOTS {
        for z in 10..EDIT_MIN_ZOOM {
            let k = tile_for(lat, lon, z);
            assert!(proxy.edit_touch(k).any(), "{name} {k}");
            let pr = proxy.probe(k);
            assert_eq!(proxy.raw_error_m(k), pr.range(), "{name} {k}");
        }
    }
    let eyvi = tile_for(SPOTS[2].1, SPOTS[2].2, 14);
    assert!(proxy.edit_touch(eyvi).strips);
    assert!(proxy.edit_touch(tile_for(SPOTS[1].1, SPOTS[1].2, 14)).water);
    // Open sea far off the coast: the probe is flat and the proxy is 0.
    let sea = tile_for(55.6, 20.2, 13);
    assert_eq!(proxy.probe(sea).step, 0.0);
    assert_eq!(proxy.raw_error_m(sea), 0.0);
    // Flat zone at Vilnius: much smaller than the same zoom in the hills.
    let pl = ErrorProxy::new(&plain(), 10);
    let centre = pl.raw_error_m(tile_for(VILNIUS.0, VILNIUS.1, 14));
    let hills = ErrorProxy::new(&no_flat(), 10).raw_error_m(tile_for(VILNIUS.0, VILNIUS.1, 14));
    assert!(centre < 0.2 * hills, "flat zone {centre} vs hills {hills}");
}

/// (3) The proxy never under-estimates an edited tile (runway strips, lakes, coast) against the
/// brute-force error at 4 x density, at zooms EDIT_MIN_ZOOM..16.
#[test]
fn error_proxy_covers_edited_tiles() {
    let t = seed_terrain();
    let mut proxy = proxy_for(t);
    let keys = edit_tiles(EDIT_MIN_ZOOM, 16, 6);
    assert!(keys.len() >= 25, "{}", keys.len());
    let measured = measure_all(t, &keys, 4);
    let mut worst: (f64, TileKey) = (f64::INFINITY, keys[0]);
    for (&k, &m) in keys.iter().zip(&measured) {
        let e = proxy.error_m(k);
        if e - m < worst.0 {
            worst = (e - m, k);
        }
        assert!(e >= m, "{k} ({:?}): proxy {e:.4} < measured {m:.4}", proxy.edit_touch(k));
    }
    eprintln!("edited tiles: {} checked, smallest proxy - measured {:.4} m at {}", keys.len(), worst.0, worst.1);
}

/// Measurement (not a check): the proxy against the brute-force error at 4 x density on a
/// sample of tiles (plain terrain without flat zone, the default terrain with its flat zone at
/// Vilnius, the seed world incl. edited tiles), z10..16: Pearson correlation of ln(error), ratio
/// quantiles, worst under-estimate.
/// `cargo test -p simworld-core --release error_proxy_vs_measured -- --ignored --nocapture`
#[test]
#[ignore]
fn error_proxy_vs_measured() {
    let plain = plain();
    let no_flat = no_flat();
    let seed = seed_terrain();
    let mut all: Vec<(String, f64, f64, bool)> = Vec::new();
    for (what, t) in [("no_flat", &no_flat), ("plain", &plain), ("seed", seed)] {
        let mut proxy = proxy_for(t);
        let mut keys = Vec::new();
        for z in 10..=16u8 {
            // A 10 x 10 grid of points per zoom (the plain world: 6 x 6 around the flat zone).
            let n = if what == "plain" { 6 } else { 10 };
            for q in 0..n * n {
                let (u, v) = ((q % n) as f64 / (n - 1) as f64, (q / n) as f64 / (n - 1) as f64);
                let (lat, lon) = match what {
                    "plain" => (VILNIUS.0 - 0.15 + 0.3 * v, VILNIUS.1 - 0.25 + 0.5 * u),
                    "no_flat" => (54.0 + 1.5 * v, 22.6 + 2.5 * u),
                    _ => (54.2 + 1.6 * v, 21.0 + 4.5 * u),
                };
                keys.push(tile_for(lat, lon, z));
            }
        }
        if what == "seed" {
            keys.extend(edit_tiles(10, 16, 30));
        }
        keys.sort_by_key(|k| (k.z, k.x, k.y));
        keys.dedup();
        let t0 = std::time::Instant::now();
        let measured = measure_all(t, &keys, 4);
        eprintln!("{what}: {} tiles measured in {:.1} s", keys.len(), t0.elapsed().as_secs_f64());
        for (&k, &m) in keys.iter().zip(&measured) {
            all.push((format!("{what} {k}"), proxy.error_m(k), m, proxy.edit_touch(k).any()));
        }
    }
    let stats = |name: &str, v: &[&(String, f64, f64, bool)]| {
        let pts: Vec<(f64, f64)> = v.iter().filter(|x| x.2 > 1e-4 && x.1 > 1e-4).map(|x| (x.1.ln(), x.2.ln())).collect();
        let n = pts.len() as f64;
        let (mx, my) = (pts.iter().map(|p| p.0).sum::<f64>() / n, pts.iter().map(|p| p.1).sum::<f64>() / n);
        let cov: f64 = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
        let (vx, vy): (f64, f64) = (pts.iter().map(|p| (p.0 - mx).powi(2)).sum(), pts.iter().map(|p| (p.1 - my).powi(2)).sum());
        let mut ratios: Vec<f64> = v.iter().filter(|x| x.2 > 1e-3).map(|x| x.1 / x.2).collect();
        ratios.sort_by(f64::total_cmp);
        let q = |f: f64| ratios.get(((ratios.len() as f64 - 1.0) * f) as usize).copied().unwrap_or(f64::NAN);
        let under = v.iter().filter(|x| x.1 < x.2).count();
        let worst = v.iter().map(|x| (x.2 - x.1, &x.0, x.1, x.2)).max_by(|a, b| a.0.total_cmp(&b.0));
        eprintln!(
            "{name}: n {} | corr(ln) {:.3} | proxy/measured p05 {:.2} p50 {:.2} p95 {:.2} | under-estimated {under} | worst {:?}",
            v.len(),
            cov / (vx * vy).sqrt(),
            q(0.05),
            q(0.5),
            q(0.95),
            worst.map(|w| format!("{} proxy {:.3} measured {:.3} (short by {:.3} m)", w.1, w.2, w.3, w.0))
        );
    };
    let refs: Vec<&(String, f64, f64, bool)> = all.iter().collect();
    stats("all", &refs);
    for what in ["no_flat", "plain", "seed"] {
        let v: Vec<_> = all.iter().filter(|x| x.0.starts_with(what)).collect();
        stats(what, &v);
    }
    let edited: Vec<_> = all.iter().filter(|x| x.3).collect();
    stats("edited", &edited);
    for z in 10..=16 {
        let v: Vec<_> = all.iter().filter(|x| x.0.contains(&format!(" {z}/"))).collect();
        stats(&format!("z{z}"), &v);
    }
}

/// Measurement: HeightTile build cost per tile against today's TIN build (Delatin on a patch),
/// same keys, plain terrain and seed world, z10..14.
/// `cargo test -p simworld-core --release height_tile_build_cost -- --ignored --nocapture`
#[test]
#[ignore]
fn height_tile_build_cost() {
    use crate::geo::Frame;
    use crate::terrain::tin::build_terrain_tile;
    let p = &test_world().0;
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let no_flat = no_flat();
    for (what, t, lat, lon) in [("no_flat", &no_flat, 54.9, 23.9), ("seed eyvi", seed_terrain(), SPOTS[2].1, SPOTS[2].2), ("seed coast", seed_terrain(), SPOTS[0].1, SPOTS[0].2)] {
        for z in 10..=14u8 {
            let key = tile_for(lat, lon, z);
            let ring = (14 - z) as usize;
            let reps = 5;
            let parent = build(t, parent_key(key), None);
            let mut bufs = Some(HeightBuffers::new());
            let time = |f: &mut dyn FnMut()| {
                let mut best = f64::INFINITY;
                for _ in 0..reps {
                    let t0 = std::time::Instant::now();
                    f();
                    best = best.min(t0.elapsed().as_secs_f64() * 1e3);
                }
                best
            };
            let grid = time(&mut || {
                let tile = HeightTile::build(t, key, HeightQuant::for_terrain(t), None, bufs.take().unwrap());
                bufs = Some(tile.into_buffers());
            });
            let grid_p = time(&mut || {
                let tile = HeightTile::build(t, key, HeightQuant::for_terrain(t), Some(&parent), bufs.take().unwrap());
                bufs = Some(tile.into_buffers());
            });
            let per_point = time(&mut || {
                let mut s = 0.0;
                for j in -1..=N + 1 {
                    for i in -1..=N + 1 {
                        let (la, lo) = lattice_lat_lon(key, GRID_QUADS as u32, i, j);
                        s += t.height(la, lo);
                    }
                }
                std::hint::black_box(s);
            });
            let tin = time(&mut || {
                std::hint::black_box(build_terrain_tile(key, ring, frame, t, p, 1.0).unwrap());
            });
            eprintln!(
                "{what:10} {key:14} grid {grid:7.3} ms | with parent {grid_p:7.3} | per-point calls {per_point:7.3} | TIN {tin:8.3} ms | TIN / grid {:.1}x",
                tin / grid_p
            );
        }
    }
}
