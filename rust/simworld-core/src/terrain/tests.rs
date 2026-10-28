//! Terrain phase 1 tests (docs/spec-terrain.md section 11, adapted to the procedural field).

use super::*;
use crate::geo::{tile_for, Frame, TileKey};
use crate::profile::{Profile, TerrainProfile};

const VILNIUS: (f64, f64) = (54.6872, 25.2797);

fn active(t: TerrainProfile) -> Terrain {
    Terrain::new(&t, VILNIUS.0, VILNIUS.1)
}

fn default_active() -> Terrain {
    active(TerrainProfile { enabled: true, ..TerrainProfile::default() })
}

/// No flat zone: the raw field everywhere.
fn no_flat() -> Terrain {
    active(TerrainProfile { enabled: true, flat_radius_m: 0.0, flat_blend_m: 0.0, ..TerrainProfile::default() })
}

fn grid_points(n: usize, span_deg: f64) -> Vec<(f64, f64)> {
    (0..n * n)
        .map(|i| {
            let (a, b) = ((i % n) as f64 / n as f64, (i / n) as f64 / n as f64);
            (VILNIUS.0 - span_deg / 2.0 + b * span_deg, VILNIUS.1 - span_deg + a * 2.0 * span_deg)
        })
        .collect()
}

#[test]
fn flat_terrain_is_exactly_zero_and_samples_nothing() {
    let t = Terrain::flat();
    assert!(!t.is_active());
    for (lat, lon) in grid_points(5, 1.0) {
        assert_eq!(t.height(lat, lon).to_bits(), 0.0f64.to_bits());
        assert_eq!(t.water_height(lat, lon).to_bits(), 0.0f64.to_bits());
    }
    assert_eq!(t.tile_range(tile_for(VILNIUS.0, VILNIUS.1, 13)), [0.0, 0.0]);
    assert_eq!((t.drape_margin_m(0), t.foundation_m(0), t.max_height()), (0.0, 0.0, 0.0));
    // The profile's terrain is off by default.
    assert!(!Profile::default().terrain.enabled);
}

/// Same inputs, same bits: independent of call order and thread.
#[test]
fn height_is_deterministic_across_threads_and_order() {
    let t = no_flat();
    let pts = grid_points(40, 0.6);
    let forward: Vec<u64> = pts.iter().map(|&(a, b)| t.height(a, b).to_bits()).collect();
    let backward: Vec<u64> = {
        let mut v: Vec<u64> = pts.iter().rev().map(|&(a, b)| t.height(a, b).to_bits()).collect();
        v.reverse();
        v
    };
    assert_eq!(forward, backward);
    let threaded: Vec<u64> = std::thread::scope(|s| {
        let hs: Vec<_> = pts
            .chunks(97)
            .map(|c| {
                let t = &t;
                s.spawn(move || c.iter().map(|&(a, b)| t.height(a, b).to_bits()).collect::<Vec<u64>>())
            })
            .collect();
        hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(forward, threaded);
    // The pure function agrees with the Terrain wrapper (no flat zone, exaggeration 1, offset 0).
    let (lat, lon) = pts[123];
    assert_eq!(field_height(t.field(), lat, lon).to_bits(), t.height(lat, lon).to_bits());
}

/// The defaults make real relief within the bounds, and the shapes matter.
#[test]
fn relief_is_varied_and_within_bounds() {
    let t = no_flat();
    let [lo, hi] = t.bounds();
    let hs: Vec<f64> = grid_points(60, 0.8).iter().map(|&(a, b)| t.height(a, b)).collect();
    let (min, max) = hs.iter().fold((f64::MAX, f64::MIN), |(a, b), &h| (a.min(h), b.max(h)));
    assert!(min >= lo && max <= hi, "{min}..{max} outside {lo}..{hi}");
    assert!(max - min > 100.0, "relief {min}..{max}");
    assert!(hs.iter().any(|&h| h == 0.0), "some flat sea-level plains");
    // A different seed is a different world.
    let other = active(TerrainProfile { enabled: true, seed: 2, flat_radius_m: 0.0, flat_blend_m: 0.0, ..TerrainProfile::default() });
    assert_ne!(t.height(VILNIUS.0 + 0.05, VILNIUS.1), other.height(VILNIUS.0 + 0.05, VILNIUS.1));
    // Exaggeration and offset scale every height.
    let ex = active(TerrainProfile { enabled: true, flat_radius_m: 0.0, flat_blend_m: 0.0, exaggeration: 2.0, height_offset_m: 10.0, ..TerrainProfile::default() });
    let (a, b) = (VILNIUS.0 + 0.11, VILNIUS.1 - 0.07);
    assert!((ex.height(a, b) - (2.0 * t.height(a, b) + 10.0)).abs() < 1e-9);
}

#[test]
fn flat_zone_is_level_and_blends_smoothly() {
    let t = default_active();
    let f = Frame::new(VILNIUS.0, VILNIUS.1);
    let h0 = t.height(VILNIUS.0, VILNIUS.1);
    // Inside the radius (1.5 km): exactly the center height.
    for (e, n) in [(0.0, 0.0), (1000.0, 500.0), (-900.0, 1100.0), (0.0, -1400.0)] {
        let (lat, lon) = f.lat_lon(e, n);
        assert_eq!(t.height(lat, lon), h0, "({e}, {n})");
    }
    // Beyond radius + blend: the raw field.
    let raw = no_flat();
    let (lat, lon) = f.lat_lon(6000.0, 0.0);
    assert_eq!(t.height(lat, lon), raw.height(lat, lon));
    // Smooth blend: no step between neighbouring points 10 m apart across the blend.
    let mut prev = t.height(VILNIUS.0, VILNIUS.1);
    for i in 1..700 {
        let (lat, lon) = f.lat_lon(i as f64 * 10.0, 0.0);
        let h = t.height(lat, lon);
        assert!((h - prev).abs() < 5.0, "step {} at {} m", h - prev, i * 10);
        prev = h;
    }
}

/// Two adjacent tiles' shared border samples are identical (same lattice integers).
#[test]
fn patch_seams_are_bit_identical() {
    let t = no_flat();
    for z in [16u8, 14, 12, 10] {
        let k = tile_for(VILNIUS.0 + 0.03, VILNIUS.1 + 0.02, z);
        let east = TileKey::new(z, k.x + 1, k.y);
        let south = TileKey::new(z, k.x, k.y + 1);
        let (a, b, c) = (t.patch(k, 33), t.patch(east, 33), t.patch(south, 33));
        let n = a.n() as i64 - 1;
        for j in -1..=n + 1 {
            // Shared edge column, and each tile's border = the other's first interior column.
            assert_eq!(a.get(n, j).to_bits(), b.get(0, j).to_bits(), "z{z} east edge j {j}");
            assert_eq!(a.get(n + 1, j).to_bits(), b.get(1, j).to_bits());
            assert_eq!(a.get(n - 1, j).to_bits(), b.get(-1, j).to_bits());
        }
        for i in -1..=n + 1 {
            assert_eq!(a.get(i, n).to_bits(), c.get(i, 0).to_bits(), "z{z} south edge i {i}");
        }
        // The coarse tile ranges of neighbours share their edge samples too: an edge sample
        // lies inside both boxes.
        let (ra, rb) = (t.tile_range(k), t.tile_range(east));
        let h = t.height(k.frac_to_lat_lon(1.0, 0.5).0, k.frac_to_lat_lon(1.0, 0.5).1);
        assert!(h >= ra[0].max(rb[0]) - 1e-9 && h <= ra[1].min(rb[1]) + 1e-9);
    }
}

/// Patch nodes equal the direct function; bilinear in between stays close.
#[test]
fn patch_sampling_matches_the_function() {
    let t = no_flat();
    let k = tile_for(VILNIUS.0 + 0.05, VILNIUS.1 - 0.04, 15);
    let p = t.patch(k, 65);
    for (i, j) in [(0i64, 0i64), (64, 64), (13, 50), (-1, 7), (65, 65), (32, 0)] {
        let (lat, lon) = crate::terrain::sample::lattice_lat_lon(k, 64, i, j);
        assert_eq!(p.get(i, j).to_bits(), t.height(lat, lon).to_bits());
        // sample_frac at a node is the node itself.
        assert_eq!(p.sample_frac(p.frac(i), p.frac(j)), p.get(i, j));
    }
    let mut worst = 0.0f64;
    for s in 0..400 {
        let (fx, fy) = ((s % 20) as f64 / 19.3 + 0.01, (s / 20) as f64 / 19.7 + 0.005);
        let (lat, lon) = k.frac_to_lat_lon(fx, fy);
        worst = worst.max((p.sample_frac(fx, fy) - t.height(lat, lon)).abs());
    }
    // z15 tile ~700 m, 64 cells ~11 m: bilinear error well below a meter.
    assert!(worst < 1.0, "bilinear error {worst}");
    let [lo, hi] = p.range();
    assert!(lo <= hi && lo >= t.bounds()[0] && hi <= t.bounds()[1]);
}

/// The coarse box (9 x 9 + padding) contains a much finer sampling of the tile, for many tiles
/// of every vector zoom.
#[test]
fn tile_range_contains_fine_samples() {
    let t = no_flat();
    let mut checked = 0;
    for z in 13u8..=16 {
        let c = tile_for(VILNIUS.0 + 0.02, VILNIUS.1 + 0.08, z);
        for d in 0..6u32 {
            let k = TileKey::new(z, c.x + d * 3, c.y + d);
            let [lo, hi] = t.tile_range(k);
            assert_eq!(t.tile_range(k), [lo, hi], "pure function of the key");
            let p = t.patch(k, 49);
            let [flo, fhi] = p.range();
            assert!(flo >= lo - 1e-9 && fhi <= hi + 1e-9, "{k}: fine {flo}..{fhi} outside box {lo}..{hi}");
            checked += 1;
        }
    }
    assert_eq!(checked, 24);
}

/// The drape table is frozen (Terrain v2 P0, `docs/spec-terrain.md` "Design change: drape
/// frozen"): [`DRAPE_REFERENCE`] equals what the 1.7 rules derived from the default profile (the
/// terrain error visible in each vector band, R0 1 m, R1 3 m, R2 8 m, R3 20 m at
/// detail_scale_max 2: margins a quarter of it at detail 1, foundations 2 x error + 1 m), and a
/// profile that changes those inputs no longer changes the drape or the foundations.
#[test]
fn drape_table_is_frozen_at_the_default_rules() {
    use super::reference::{band_error_rule, drape_margin_rule, foundation_rule};
    let p = Profile::default();
    assert_eq!(band_error_rule(&p.terrain.bands, p.terrain.detail_scale_max, p.bands()), [1.0, 3.0, 8.0, 20.0]);
    assert_eq!(drape_margin_rule(&p.terrain.bands, p.bands()), DRAPE_REFERENCE.margin_m);
    assert_eq!(foundation_rule(&p.terrain.bands, p.terrain.detail_scale_max, p.bands()), DRAPE_REFERENCE.foundation_m);
    assert_eq!(DRAPE_REFERENCE.margin_m, [0.125, 0.375, 1.0, 2.5]);
    assert_eq!(DRAPE_REFERENCE.foundation_m, [3.0, 7.0, 17.0, 41.0]);
    let t = default_active();
    assert_eq!([0, 1, 2, 3].map(|r| t.drape_margin_m(r)), DRAPE_REFERENCE.margin_m);
    assert_eq!([0, 1, 2, 3].map(|r| t.foundation_m(r)), DRAPE_REFERENCE.foundation_m);
    // Live inputs that the 1.7 rules read: detail_scale_max, vector band edges, terrain band
    // errors and edges. The rules would move; the terrain's drape does not.
    let p2 = crate::profile::resolve(
        None,
        Some(&serde_json::json!({
            "terrain": { "enabled": true, "detail_scale_max": 1.0, "bands": [
                { "edge_m": 2500.0, "max_error_m": 3.0, "max_edge_m": 0.0, "max_triangles": 4096 },
                { "edge_m": 7000.0, "max_error_m": 6.0, "max_edge_m": 0.0, "max_triangles": 4096 },
                { "edge_m": 25000.0, "max_error_m": 12.0, "max_edge_m": 0.0, "max_triangles": 4096 },
                { "edge_m": 60000.0, "max_error_m": 30.0, "max_edge_m": 0.0, "max_triangles": 4096 },
                { "edge_m": 130000.0, "max_error_m": 60.0, "max_edge_m": 0.0, "max_triangles": 4096 } ] },
            "bands": { "max_distance_m": 40000.0, "hysteresis": 1.3 }, "lod": { "error_m": { "r2": 8.55 } } })),
        &crate::NullLog,
    )
    .unwrap();
    assert_ne!(drape_margin_rule(&p2.terrain.bands, p2.bands()), DRAPE_REFERENCE.margin_m, "the override reaches the 1.7 rule");
    assert_ne!(foundation_rule(&p2.terrain.bands, p2.terrain.detail_scale_max, p2.bands()), DRAPE_REFERENCE.foundation_m);
    let t2 = Terrain::from_profile(&p2, VILNIUS.0, VILNIUS.1);
    assert!(t2.is_active());
    assert_eq!([0, 1, 2, 3].map(|r| t2.drape_margin_m(r)), DRAPE_REFERENCE.margin_m);
    assert_eq!([0, 1, 2, 3].map(|r| t2.foundation_m(r)), DRAPE_REFERENCE.foundation_m);
}

/// Water snap: a shore vertex takes the lake level (the minimum around it); the same vertex
/// computed from two adjacent tiles gets the same height.
#[test]
fn water_snap_takes_the_minimum_and_is_seam_consistent() {
    let t = active(TerrainProfile { enabled: true, flat_radius_m: 0.0, flat_blend_m: 0.0, water_snap_m: 60.0, ..TerrainProfile::default() });
    let f = Frame::new(VILNIUS.0, VILNIUS.1);
    let mut lower = 0;
    for i in 0..200 {
        let (lat, lon) = f.lat_lon(i as f64 * 97.0 - 9000.0, (i % 13) as f64 * 311.0 - 2000.0);
        let w = t.water_height(lat, lon);
        assert!(w <= t.height(lat, lon));
        // The window minimum: never below the 5 x 5 samples' minimum, equal to one of them.
        let step = 30.0 / M_PER_DEG;
        let dlon = 30.0 / (M_PER_DEG * lat.to_radians().cos());
        let samples: Vec<f64> =
            (-2..=2).flat_map(|j| (-2..=2).map(move |i| (j, i))).map(|(j, i)| t.height(lat + j as f64 * step, lon + i as f64 * dlon)).collect();
        assert!(samples.contains(&w));
        if w < t.height(lat, lon) - 0.5 {
            lower += 1;
        }
    }
    assert!(lower > 20, "the snap should lower sloped shore vertices ({lower})");
    // A vertex on the shared edge of two tiles, through each tile's projector.
    let k = tile_for(VILNIUS.0 + 0.04, VILNIUS.1 + 0.06, 16);
    let east = TileKey::new(16, k.x + 1, k.y);
    let pa = crate::mvt::TileProjector::new(k, f, 4096.0).with_terrain(t.clone());
    let pb = crate::mvt::TileProjector::new(east, f, 4096.0).with_terrain(t.clone());
    for y in [0.0, 1000.5, 4095.0] {
        assert_eq!(pa.project_water([4096.0, y]).1.to_bits(), pb.project_water([0.0, y]).1.to_bits());
        assert_eq!(pa.project_h([4096.0, y]).1.to_bits(), pb.project_h([0.0, y]).1.to_bits());
    }
    // Off: plain height.
    let plain = active(TerrainProfile { enabled: true, water_snap_m: 0.0, ..TerrainProfile::default() });
    assert_eq!(plain.water_height(VILNIUS.0 + 0.1, VILNIUS.1), plain.height(VILNIUS.0 + 0.1, VILNIUS.1));
}

/// Cover with a fixed camera and a synthetic peak: a high mountain far away is pulled into a
/// finer band by its box (its top is close to a high camera), and the result is deterministic.
#[test]
fn cover_follows_a_synthetic_peak_deterministically() {
    use crate::scheduler::Scheduler;
    use std::sync::Arc;
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let p = Arc::new(Profile::default());
    // Tall, rugged relief without a flat zone (peaks up to ~2 km).
    let peak = active(TerrainProfile {
        enabled: true,
        amplitude_m: 1200.0,
        ridge_share: 1.0,
        flat_radius_m: 0.0,
        flat_blend_m: 0.0,
        ..TerrainProfile::default()
    });
    let cover = |terrain: Terrain| {
        let mut s = Scheduler::new(frame, true, p.clone()).with_terrain(terrain.clone());
        let sig = crate::golden_tests::cover_signature(&mut s, [0.0, 0.0, 3000.0], [0.0; 3]);
        let boxes: Vec<(TileKey, [f64; 2])> = {
            let mut v: Vec<_> = s.entries().map(|e| (e.key, e.terrain_box)).collect();
            v.sort_by_key(|x| x.0);
            v
        };
        (sig, boxes)
    };
    let (flat_sig, _) = cover(Terrain::flat());
    let (a, boxes) = cover(peak.clone());
    let (b, _) = cover(peak.clone());
    assert_eq!(a, b, "same camera, same terrain: same cover");
    assert_ne!(a.1, flat_sig.1, "relief changes the cover");
    let top = boxes.iter().map(|(_, g)| g[1]).fold(f64::MIN, f64::max);
    assert!(top > 1000.0, "a peak above 1 km in reach: {top}");
    // Every box holds the terrain at its tile center.
    for (k, g) in &boxes {
        let (lat, lon) = k.center_lat_lon();
        let h = peak.height(lat, lon);
        assert!(h >= g[0] - 1e-9 && h <= g[1] + 1e-9, "{k}: {h} outside {g:?}");
    }
    // The camera's AGL is above the terrain: the view point height is AGL + ground.
    let mut s = Scheduler::new(frame, true, p.clone()).with_terrain(peak.clone());
    let v = s.views([0.0, 0.0, 300.0], [0.0; 3])[0];
    assert!((v.alt - (300.0 + peak.height(VILNIUS.0, VILNIUS.1))).abs() < 1e-6);
    let mut jobs = Vec::new();
    s.update([0.0, 0.0, 300.0], [0.0; 3], std::time::Instant::now(), &mut jobs);
    eprintln!("peak cover at 3000 m AGL: {:?} (flat {:?}), highest box top {top:.0} m", a.0, flat_sig.0);
}

/// The look-ahead moves the camera's MSL altitude; it does not re-add the terrain under the
/// look-ahead point to the camera's AGL (a hill under the camera, a plain ahead).
#[test]
fn view_altitude_is_camera_msl_moved_by_the_look_ahead() {
    use crate::scheduler::Scheduler;
    use std::sync::Arc;
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let p = Arc::new(Profile::default());
    let rugged = active(TerrainProfile {
        enabled: true,
        amplitude_m: 1200.0,
        ridge_share: 1.0,
        flat_radius_m: 0.0,
        flat_blend_m: 0.0,
        ..TerrainProfile::default()
    });
    let s = Scheduler::new(frame, true, p).with_terrain(rugged.clone());
    let cam_ground = rugged.height(VILNIUS.0, VILNIUS.1);
    let cam_msl = 300.0 + cam_ground;
    // Level flight in 8 directions: the view point never drops below the camera's MSL altitude
    // (AGL + the ground under the look-ahead point would, wherever the ground there is lower).
    for k in 0..8 {
        let a = k as f64 * std::f64::consts::FRAC_PI_4;
        let v = s.views([0.0, 0.0, 300.0], [100.0 * a.cos(), 100.0 * a.sin(), 0.0]);
        for (i, vp) in v.iter().enumerate().take(4) {
            assert!(vp.alt >= cam_msl - 1e-6, "dir {k} ring {i}: {} < {cam_msl}", vp.alt);
        }
    }
    let at_cam = s.views([0.0, 0.0, 300.0], [0.0; 3])[0];
    assert!((at_cam.alt - cam_msl).abs() < 1e-6);
    // A climb of 10 m/s for the look-ahead time raises the MSL altitude by that much (R1: clamp is far).
    let up = s.views([0.0, 0.0, 300.0], [0.0, 0.0, 10.0]);
    let climbed = up[1].alt - cam_msl;
    assert!(climbed > 0.0 && climbed <= 10.0 * 20.0 + 1e-6, "climb {climbed}");
}

/// Timing (informative, `cargo test --release -- --ignored terrain_cost --nocapture`).
#[test]
#[ignore]
fn terrain_cost() {
    let t = default_active();
    let pts = grid_points(300, 0.8);
    let t0 = std::time::Instant::now();
    let s: f64 = pts.iter().map(|&(a, b)| t.height(a, b)).sum();
    let per = t0.elapsed().as_nanos() as f64 / pts.len() as f64;
    let t1 = std::time::Instant::now();
    let c = tile_for(VILNIUS.0, VILNIUS.1, 13);
    for d in 0..100u32 {
        std::hint::black_box(t.tile_range(TileKey::new(13, c.x + d, c.y)));
    }
    eprintln!("height {per:.0} ns/sample (sum {s:.0}), tile_range {:.1} us", t1.elapsed().as_micros() as f64 / 100.0);
}

/// Wall quads of a building mesh as (first vertex, is_foundation): side quads (|normal.y| < 0.5)
/// come in pairs on the terrain, the wall then its foundation (u = -1, top = the wall bottom).
fn side_quads(m: &crate::mesher::MeshData) -> Vec<usize> {
    (0..m.indices.len() / 6).map(|q| m.indices[q * 6] as usize).filter(|&v0| m.normals[v0][1].abs() < 0.5).collect()
}

/// Exact terrain height under a mesh vertex of tile `key`.
fn h_at(frame: Frame, key: TileKey, t: &Terrain, v: [f32; 3]) -> f64 {
    let c = frame.tile_center(key);
    let (lat, lon) = frame.lat_lon(c[0] + v[0] as f64, c[1] - v[2] as f64);
    t.height(lat, lon)
}

/// Per-vertex walls (api 1.7, contract section 7): on a slope every wall ends at the exact ground
/// at its vertices (never above it), the foundation hangs `foundation_m` below that, the roof is at
/// the highest ground under the footprint + height (`base_m`), and windowed walls keep a constant
/// floor height: `v = floors` at the roof and `floors - (roof - y) / floor_m` at every bottom
/// (negative downhill), windowless walls and foundations `u = -1`, `v = y - y0` in meters.
#[test]
fn walls_reach_the_exact_ground_at_every_vertex() {
    use crate::mesher::{mesh_buildings, resolve, ClipRect};
    use crate::worker::{decode_payload_on, Payload};
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let t = no_flat();
    let p = Profile::default();
    let key = tile_for(VILNIUS.0 + 0.01, VILNIUS.1 + 0.01, 16);
    let dec = decode_payload_on(key, frame, &t, &Payload::Synthetic, false, false, &p).unwrap();
    let lod = p.lod(0).unwrap();
    let f = t.foundation_m(0);
    let (mut checked, mut windowed, mut negative_rows, mut max_rise) = (0, 0, 0, 0.0f64);
    for b in dec.buildings.iter().filter(|b| b.min_height == 0.0) {
        let r = resolve(b, &p);
        let m = mesh_buildings(std::slice::from_ref(b), &dec.proj, ClipRect::tile(4096.0), lod, &p);
        if m.is_empty() {
            continue;
        }
        let roof = b.base_m + r.height;
        // The base is the highest ground under the decoded outline.
        for &v in &b.polygons[0][0] {
            assert!(dec.proj.ground_at(v) <= b.base_m as f64 + 1e-3, "a footprint vertex above the base");
        }
        for (k, n) in m.normals.iter().enumerate() {
            if n[1] > 0.5 {
                assert_eq!(m.positions[k][1], roof, "roof at base + height");
            }
        }
        let quads = side_quads(&m);
        assert_eq!(quads.len() % 2, 0);
        let wall_h = r.height - r.min_height;
        let floors = (wall_h / r.floor_h.max(0.1)).round().max(1.0);
        for pair in quads.chunks_exact(2) {
            let (w, fq) = (pair[0], pair[1]);
            for k in 0..2 {
                let bot = m.positions[w + k];
                let h = h_at(frame, key, &t, bot);
                assert!(bot[1] as f64 <= h + 2e-3, "wall bottom {} above the exact ground {h}", bot[1]);
                assert!(bot[1] as f64 >= h.min(b.base_m as f64) - 2e-3, "wall bottom {} below the exact ground {h}", bot[1]);
                max_rise = max_rise.max(b.base_m as f64 - h);
                // Foundation: the wall bottom down to `f` below it, windowless.
                let (fb, ft) = (m.positions[fq + k], m.positions[fq + 3 - k]);
                assert_eq!((ft[0], ft[1], ft[2]), (bot[0], bot[1], bot[2]), "foundation top = wall bottom");
                assert!((fb[1] - (bot[1] - f)).abs() < 1e-4);
                assert!(m.uvs[fq + k][0] == -1.0 && m.uvs[fq + 3 - k][0] == -1.0);
                assert!((m.uvs[fq + k][1] - (fb[1] - b.base_m)).abs() < 2e-3, "foundation v in meters from y0");
                // Wall UV.
                let (uv_b, uv_t) = (m.uvs[w + k], m.uvs[w + 3 - k]);
                assert_eq!(m.positions[w + 3 - k][1], roof);
                if uv_b[0] >= 0.0 {
                    windowed += 1;
                    assert_eq!(uv_t[1], floors);
                    let want = floors - (roof - bot[1]) / (wall_h / floors);
                    assert!((uv_b[1] - want).abs() < 2e-3, "v {} want {want}", uv_b[1]);
                    if uv_b[1] < -0.5 {
                        negative_rows += 1;
                    }
                } else {
                    assert_eq!(uv_t[1], wall_h);
                    assert!((uv_b[1] - (bot[1] - b.base_m)).abs() < 2e-3);
                }
            }
        }
        checked += 1;
    }
    eprintln!("per-vertex walls: {checked} buildings, {windowed} windowed bottoms, {negative_rows} with whole rows below y0, largest rise {max_rise:.2} m");
    assert!(checked >= 20 && windowed > 50 && max_rise > 0.5, "the tile must test a slope");
    assert!(negative_rows > 0, "downhill rows continue below the nominal bottom");
}

/// A long wall over a dip is subdivided so its bottom follows the ground (no gap under the
/// windowed wall bigger than the band's drape margin at the tested points, and never more than
/// the foundation anywhere along it); the base also sees a hill inside a large footprint.
#[test]
fn long_walls_follow_the_ground_and_the_base_sees_the_interior() {
    use crate::mesher::{mesh_buildings, ClipRect};
    use crate::mvt::{BuildingTags, OsmType, RawBuilding, TileProjector};
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let t = active(TerrainProfile { enabled: true, flat_radius_m: 0.0, flat_blend_m: 0.0, amplitude_m: 900.0, ..TerrainProfile::default() });
    let p = Profile::default();
    let key = tile_for(VILNIUS.0 + 0.03, VILNIUS.1 - 0.05, 16);
    let proj = TileProjector::new(key, frame, 4096.0).with_terrain(t.clone());
    let lod = p.lod(0).unwrap();
    let tol = t.drape_margin_m(0) as f64;
    let mut worst_gap = 0.0f64;
    let mut interior_wins = 0;
    // A grid of 200 m x 40 m slabs across the tile (long walls on varied relief).
    for (i, (x0, y0)) in [(300.0, 300.0), (1500.0, 900.0), (2600.0, 2400.0), (600.0, 3000.0), (2000.0, 1600.0)].into_iter().enumerate() {
        let (w, h) = (2100.0, 420.0);
        let ring = vec![[x0, y0], [x0 + w, y0], [x0 + w, y0 + h], [x0, y0 + h]];
        let raw = RawBuilding {
            osm_type: OsmType::Way,
            id: 900 + i as u64,
            kind: "industrial".into(),
            height: Some(12.0),
            levels: None,
            min_height: 0.0,
            landuse: None,
            tags: BuildingTags::default(),
            polygons: vec![vec![ring.clone()]],
        };
        let b = proj.finish(raw).unwrap();
        let corner_max = ring.iter().map(|&v| proj.ground_at(v)).fold(f64::MIN, f64::max);
        if b.base_m as f64 > corner_max + 1e-3 {
            interior_wins += 1;
        }
        let m = mesh_buildings(std::slice::from_ref(&b), &proj, ClipRect::tile(4096.0), lod, &p);
        let f = t.foundation_m(0) as f64;
        for pair in side_quads(&m).chunks_exact(2) {
            let (a, c) = (m.positions[pair[0]], m.positions[pair[0] + 1]);
            let len = ((c[0] - a[0]).hypot(c[2] - a[2])) as f64;
            for k in 1..16 {
                let s = k as f64 / 16.0;
                let v = [a[0] + (c[0] - a[0]) * s as f32, a[1] + (c[1] - a[1]) * s as f32, a[2] + (c[2] - a[2]) * s as f32];
                let gap = v[1] as f64 - h_at(frame, key, &t, v);
                assert!(gap < f, "wall bottom {gap:.2} m above the ground: the foundation does not reach");
                if len > t.drape_step_m() && [4, 8, 12].contains(&k) {
                    assert!(gap <= tol + 1e-3, "long wall bottom {gap:.3} m above the ground (tolerance {tol})");
                }
                worst_gap = worst_gap.max(gap);
            }
        }
    }
    eprintln!("long walls: worst bottom gap {worst_gap:.3} m (margin {tol}), base above every corner on {interior_wins} of 5 slabs");
}

/// Clipping does not move a building (api 1.7): the base comes from the decoded outline, so the
/// four quarter rectangles of a tile (the children's pieces) put the roof of a building cut by
/// them at exactly the same height as the whole tile, and a wall bottom on a clip line sits on
/// the exact ground at that clipped point.
#[test]
fn clipped_pieces_agree_on_the_roof() {
    use crate::mesher::{mesh_buildings, ClipRect};
    use crate::worker::{decode_payload_on, Payload};
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let t = no_flat();
    let p = Profile::default();
    let key = tile_for(VILNIUS.0 + 0.01, VILNIUS.1 + 0.01, 16);
    let dec = decode_payload_on(key, frame, &t, &Payload::Synthetic, false, false, &p).unwrap();
    let lod = p.lod(0).unwrap();
    let mut cut = 0;
    for b in dec.buildings.iter().filter(|b| b.min_height == 0.0 && !b.clipped) {
        // Cut every building through the middle of its bounding box.
        let xs = b.polygons[0][0].iter().map(|v| v[0]);
        let half = (xs.clone().fold(f64::MAX, f64::min) + xs.fold(f64::MIN, f64::max)) * 0.5;
        let whole = mesh_buildings(std::slice::from_ref(b), &dec.proj, ClipRect::tile(4096.0), lod, &p);
        let roof = |m: &crate::mesher::MeshData| m.positions.iter().zip(&m.normals).filter(|(_, n)| n[1] > 0.5).map(|(v, _)| v[1]).next();
        let Some(r0) = roof(&whole) else { continue };
        for rect in [ClipRect { x0: 0.0, y0: 0.0, x1: half, y1: 4096.0 }, ClipRect { x0: half, y0: 0.0, x1: 4096.0, y1: 4096.0 }] {
            let m = mesh_buildings(std::slice::from_ref(b), &dec.proj, rect, lod, &p);
            if let Some(r) = roof(&m) {
                assert_eq!(r, r0, "a clipped piece moved the roof");
            }
            for pair in side_quads(&m).chunks_exact(2) {
                for k in 0..2 {
                    let v = m.positions[pair[0] + k];
                    let h = h_at(frame, key, &t, v);
                    assert!(v[1] as f64 <= h + 2e-3 && v[1] as f64 >= h.min(b.base_m as f64) - 2e-3, "bottom {} vs ground {h}", v[1]);
                }
            }
        }
        cut += 1;
    }
    assert!(cut > 20, "{cut} buildings cut");
}

/// A raised part stands on the building under it: it takes that building's base (its own
/// smaller footprint would give a lower one on a slope), so its bottom cap stays `min_height`
/// above the lower part's base; walls of a raised part are not stretched to the ground.
#[test]
fn raised_parts_stand_on_their_lower_part() {
    use crate::mesher::{mesh_buildings, ClipRect};
    use crate::mvt::{stack_raised_parts, BuildingTags, OsmType, RawBuilding, TileProjector};
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let t = no_flat();
    let p = Profile::default();
    let key = tile_for(VILNIUS.0 + 0.01, VILNIUS.1 + 0.01, 16);
    let proj = TileProjector::new(key, frame, 4096.0).with_terrain(t.clone());
    let sq = |x: f64, y: f64, s: f64| vec![vec![vec![[x, y], [x + s, y], [x + s, y + s], [x, y + s]]]];
    let raw = |id: u64, min_h: f32, polys| RawBuilding {
        osm_type: OsmType::Way,
        id,
        kind: "apartments".into(),
        height: Some(30.0),
        levels: None,
        min_height: min_h,
        landuse: None,
        tags: BuildingTags::default(),
        polygons: polys,
    };
    // Find a spot where the lower part's base is above the part's own (a slope under it).
    let mut found = false;
    for k in 0..40 {
        let (x, y) = (200.0 + 90.0 * k as f64, 200.0 + 85.0 * k as f64);
        let lower = proj.finish(raw(1, 0.0, sq(x, y, 400.0))).unwrap();
        let part = proj.finish(raw(2, 12.0, sq(x + 20.0, y + 20.0, 100.0))).unwrap();
        if part.base_m >= lower.base_m - 0.05 {
            continue;
        }
        let mut bs = vec![lower.clone(), part];
        stack_raised_parts(&mut bs);
        assert_eq!(bs[1].base_m, lower.base_m);
        let m = mesh_buildings(&bs[1..], &proj, ClipRect::tile(4096.0), p.lod(0).unwrap(), &p);
        let low = m.positions.iter().map(|v| v[1]).fold(f32::MAX, f32::min);
        assert_eq!(low, lower.base_m + 12.0, "the raised part's bottom cap / walls start at min_height over the lower part");
        found = true;
        break;
    }
    assert!(found, "no slope found for the raised-part test");
}

/// Terrain off: buildings are meshed exactly as before api 1.7 (walls from the base, no wall
/// below it, base 0); the golden hashes pin the bytes.
#[test]
fn flat_world_walls_are_unchanged() {
    use crate::mesher::{mesh_buildings, ClipRect};
    use crate::worker::{decode_payload_on, Payload};
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let p = Profile::default();
    let key = tile_for(VILNIUS.0 + 0.01, VILNIUS.1 + 0.01, 16);
    let flat = decode_payload_on(key, frame, &Terrain::flat(), &Payload::Synthetic, false, false, &p).unwrap();
    assert!(flat.buildings.iter().all(|b| b.base_m == 0.0));
    let m = mesh_buildings(&flat.buildings, &flat.proj, ClipRect::tile(4096.0), p.lod(0).unwrap(), &p);
    assert!(m.uvs.iter().zip(&m.normals).all(|(uv, n)| n[1].abs() > 0.5 || uv[1] >= 0.0), "no wall below its base");
    assert!(m.positions.iter().all(|v| v[1] >= 0.0));
}

/// Ground drape: area vertices sit on the terrain + band drape margin + level lift, water vertices at
/// (or below) the snapped height, roads follow the slope; long edges are subdivided on relief.
#[test]
fn ground_is_draped_and_subdivided() {
    use crate::mvt::{GROUND_ROAD, GROUND_WATER};
    use crate::worker::{build_tile_on, Payload};
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let t = no_flat();
    let p = Profile::default();
    let step = p.ground_params(1).unwrap().step_m as f64;
    let key = tile_for(VILNIUS.0 + 0.02, VILNIUS.1 - 0.03, 15);
    let draped = build_tile_on(key, 1, frame, &t, &Payload::Synthetic, &p).unwrap().ground;
    let flat = build_tile_on(key, 1, frame, &Terrain::flat(), &Payload::Synthetic, &p).unwrap().ground;
    assert!(draped.vertex_count() > flat.vertex_count(), "subdivided: {} vs {}", draped.vertex_count(), flat.vertex_count());
    let c = frame.tile_center(key);
    let lift = t.drape_margin_m(1) as f64;
    let (mut areas, mut water) = (0, 0);
    for (pos, col) in draped.positions.iter().zip(&draped.colors) {
        let class = (col[0] * 16.0).round() as u8;
        let (lat, lon) = frame.lat_lon(c[0] + pos[0] as f64, c[1] - pos[2] as f64);
        let y = pos[1] as f64;
        if class == GROUND_WATER {
            assert!(y - lift - 7.0 * step <= t.height(lat, lon) + 0.02, "water above its snapped level");
            water += 1;
        } else if class != GROUND_ROAD {
            let d = y - lift - t.height(lat, lon);
            assert!((-0.02..=7.0 * step + 0.02).contains(&d), "area vertex {d} m off the terrain + margin");
            areas += 1;
        }
    }
    assert!(areas > 50, "{areas} area vertices");
    eprintln!("draped z15 ground: {} vertices (flat {}), {water} water vertices", draped.vertex_count(), flat.vertex_count());
    // Deterministic.
    assert_eq!(draped, build_tile_on(key, 1, frame, &t, &Payload::Synthetic, &p).unwrap().ground);
}

#[derive(Default)]
struct CaptureLog(std::sync::Mutex<Vec<String>>);
impl crate::Log for CaptureLog {
    fn info(&self, _: &str) {}
    fn warn(&self, m: &str) {
        self.0.lock().unwrap().push(m.to_string());
    }
    fn error(&self, m: &str) {
        self.0.lock().unwrap().push(m.to_string());
    }
}

/// Opt-in: `terrain.enabled` without the `layers` option stays flat and warns once; with
/// `layers: ["world", "terrain"]` heights are procedural; an unknown layer changes nothing.
#[test]
fn terrain_needs_the_host_layer() {
    use crate::world::{World, WorldOptions, TERRAIN_IGNORED_WARNING};
    use std::sync::Arc;
    let log = Arc::new(CaptureLog::default());
    let mut w = World::new(log.clone(), crate::Diagnostics::default(), "test");
    let on = Some(serde_json::json!({ "terrain": { "enabled": true } }));
    let opts = |layers: Option<Vec<&str>>| WorldOptions {
        origin_lat: VILNIUS.0,
        origin_lon: VILNIUS.1,
        profile_overrides: on.clone(),
        layers: layers.map(|l| l.into_iter().map(String::from).collect()),
        ..WorldOptions::default()
    };
    let r = w.configure_world(opts(None));
    assert!(r.ok, "{}", r.error);
    for _ in 0..3 {
        w.update_camera([9000.0, 0.0, 0.0], [0.0; 3], 300.0);
    }
    assert!(!w.terrain().is_active());
    assert_eq!(w.ground_height_at(0.0, 0.0), 0.0);
    assert_eq!(w.ground_height_at(4000.0, -8000.0), 0.0);
    let warned = |log: &CaptureLog| log.0.lock().unwrap().iter().filter(|m| m.contains(TERRAIN_IGNORED_WARNING)).count();
    assert_eq!(warned(&log), 1, "warned exactly once");
    // Layer requested: procedural heights, through the floating origin.
    let r = w.configure_world(opts(Some(vec!["world", "terrain"])));
    assert!(r.ok, "{}", r.error);
    assert_eq!(warned(&log), 1);
    assert!(w.terrain().is_active());
    let expect = Terrain::from_profile(w.profile(), VILNIUS.0, VILNIUS.1);
    let f = Frame::new(VILNIUS.0, VILNIUS.1);
    let (lat, lon) = f.lat_lon(7000.0, 9000.0);
    let h = w.ground_height_at(7000.0, -9000.0);
    assert_eq!(h, expect.height(lat, lon));
    assert!(h != 0.0);
    // After a re-center the same world point keeps its height.
    let shift = w.update_camera([6000.0, 0.0, 0.0], [0.0; 3], 300.0).expect("re-center");
    assert_eq!(w.ground_height_at(7000.0 - shift[0], -9000.0 - shift[2]), h);
    // Geodetic AGL uses the terrain: MSL altitude minus ground.
    // Unknown layer: error, nothing changes.
    let r = w.configure_world(opts(Some(vec!["world", "sky"])));
    assert!(!r.ok && r.error.contains("unknown layer \"sky\""), "{}", r.error);
    assert!(w.terrain().is_active());
    // Layer requested but terrain disabled in the profile: flat, no warning.
    let r = w.configure_world(WorldOptions {
        origin_lat: VILNIUS.0,
        origin_lon: VILNIUS.1,
        layers: Some(vec!["terrain".into()]),
        ..WorldOptions::default()
    });
    assert!(r.ok && !w.terrain().is_active() && w.ground_height_at(7000.0, -9000.0) == 0.0);
    assert_eq!(warned(&log), 1);
    w.shutdown();
    assert_eq!(w.ground_height_at(0.0, 0.0), 0.0);
}

#[test]
fn terrain_profile_validation() {
    let bad = |v: serde_json::Value| crate::profile::resolve(None, Some(&v), &crate::NullLog).unwrap_err();
    let e = bad(serde_json::json!({ "terrain": { "near_zoom": 13 } }));
    assert!(e.contains("terrain.bands has 5 entries, needs 4"), "{e}");
    let e = bad(serde_json::json!({ "terrain": { "near_zoom": 9, "far_zoom": 10 } }));
    assert!(e.contains("terrain.near_zoom = 9 must not be below terrain.far_zoom = 10"), "{e}");
    let e = bad(serde_json::json!({ "terrain": { "near_zoom": 11, "far_zoom": 10, "bands": [
        { "edge_m": 5000.0, "max_error_m": 1.0, "max_edge_m": 0.0, "max_triangles": 100 },
        { "edge_m": 4000.0, "max_error_m": 2.0, "max_edge_m": 300.0, "max_triangles": 100 } ] } }));
    assert!(e.contains("terrain.bands[1].edge_m = 4000 must be greater than terrain.bands[0].edge_m = 5000"), "{e}");
    assert!(e.contains("terrain.bands[1].max_edge_m = 300 must be 0 (corners only) or at least 500"), "{e}");
    let e = bad(serde_json::json!({ "terrain": { "amplitude_m": 5000.0 } }));
    assert!(e.contains("terrain.amplitude_m = 5000 is out of range [0, 3000]"), "{e}");
    let e = bad(serde_json::json!({ "terrain": { "bands": [{ "edge_m": 1000.0, "max_error_m": 0.0, "max_edge_m": 0.0, "max_triangles": 1 }] } }));
    assert!(e.contains("terrain.bands[].max_error_m = 0 is out of range") && e.contains("max_triangles = 1"), "{e}");
    let e = bad(serde_json::json!({ "terrain": { "shading": "round" } }));
    assert!(e.contains("round"), "{e}");
    let e = bad(serde_json::json!({ "terrain": { "octaves": 2.5 } }));
    assert!(e.contains("terrain.octaves = 2.5 must be an integer"), "{e}");
    let e = bad(serde_json::json!({ "terrain": { "enable": true } }));
    assert!(e.contains("unknown key `terrain.enable`"), "{e}");
    // A shorter table with matching zooms is fine; a version 4 file loads strictly with the
    // terrain defaults (off).
    let p = crate::profile::resolve(
        None,
        Some(&serde_json::json!({ "terrain": { "enabled": true, "shading": "smooth", "near_zoom": 12, "far_zoom": 12,
            "bands": [{ "edge_m": 25000.0, "max_error_m": 3.0, "max_edge_m": 0.0, "max_triangles": 1000 }] } })),
        &crate::NullLog,
    )
    .unwrap();
    assert_eq!(p.terrain.bands.len(), 1);
    let v4 = r#"{ "profile_version": 4, "bands": { "r0_edge_m": 500 } }"#;
    let p = crate::profile::resolve(Some(("v4", v4)), None, &crate::NullLog).unwrap();
    assert_eq!(p.terrain, TerrainProfile::default());
    assert!(crate::profile::resolve(Some(("v4", r#"{ "profile_version": 4, "terrain": { "seeed": 1 } }"#)), None, &crate::NullLog).is_err());
}
