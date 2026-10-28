//! Tests of the water index and the terrain-water rule (section 5.2a) on a synthetic coast.

use super::*;
use crate::seed::files::{q7, WaterFile, WaterPolygon};
use crate::seed::geom::{point_in_ring, simplify_ring};
use crate::seed::hash::{unit, Seed};

const COAST_LON: f64 = 20.8;

/// The fixture coast: waves of ~7.8 km (1.9 km amplitude), ~1.7 km (510 m) and ~370 m (130 m)
/// wavelength; the last ones are about the raster spacing, the hard case for the distance raster.
fn coast_lon(lat: f64) -> f64 {
    coast_lon_smooth(lat) + 0.008 * crate::math::sin(lat * 410.0) + 0.002 * crate::math::sin(lat * 1900.0)
}

fn coast_lon_smooth(lat: f64) -> f64 {
    COAST_LON + 0.03 * crate::math::sin(lat * 90.0)
}

fn ellipse(c: [f64; 2], rx_m: f64, ry_m: f64, n: usize, ccw: bool) -> Vec<[f64; 2]> {
    let kx = m_per_deg_lon(c[1]);
    let mut v: Vec<[f64; 2]> = (0..n)
        .map(|i| {
            let a = i as f64 / n as f64 * std::f64::consts::TAU;
            [c[0] + rx_m * crate::math::cos(a) / kx, c[1] + ry_m * crate::math::sin(a) / M_PER_DEG]
        })
        .collect();
    if !ccw {
        v.reverse();
    }
    v.into_iter().map(|p| [q7(p[0]), q7(p[1])]).collect()
}


/// bbox [20, 54, 22, 55]: sea west of a wavy coast at ~20.8 E, a lake with an island at
/// (21.4, 54.5), a short river strip, `outside` as given.
pub(super) fn coast_file(outside: Outside) -> WaterFile {
    coast_file_with(outside, coast_lon)
}

fn coast_file_with(outside: Outside, coast: fn(f64) -> f64) -> WaterFile {
    let mut sea = vec![[20.0, 54.0]];
    let steps = 2000;
    for i in 0..=steps {
        let lat = 54.0 + i as f64 / steps as f64;
        sea.push([q7(coast(lat)), q7(lat)]);
    }
    sea.push([20.0, 55.0]); // east along the south edge, north along the coast, back west: counter-clockwise
    let lake = ellipse([21.4, 54.5], 3000.0, 2000.0, 240, true);
    let island = ellipse([21.4, 54.5], 400.0, 400.0, 40, false);
    let river = vec![[21.0, 54.700], [21.3, 54.700], [21.3, 54.702], [21.0, 54.702]];
    WaterFile {
        format: 1,
        source: "synthetic".into(),
        source_date: None,
        bbox: [20.0, 54.0, 22.0, 55.0],
        outside,
        tolerance_m: 0.0,
        min_area_m2: None,
        polygons: vec![
            WaterPolygon { kind: WaterKind::Sea, rings: vec![sea] },
            WaterPolygon { kind: WaterKind::Lake, rings: vec![lake, island] },
            WaterPolygon { kind: WaterKind::River, rings: vec![river] },
        ],
    }
}

/// A 1 % slope rising east from the coast, plus small waves; always above sea level.
fn field(lat: f64, lon: f64) -> f64 {
    30.0 + (lon - COAST_LON) * 640.0 + 3.0 * crate::math::sin(lat * 200.0)
}

fn brute_water(f: &WaterFile, lat: f64, lon: f64) -> WaterHit {
    let [w, s, e, n] = f.bbox;
    if !(lon >= w && lon <= e && lat >= s && lat <= n) {
        return if f.outside == Outside::Sea { WaterHit::OutsideSea } else { WaterHit::Land };
    }
    for (i, p) in f.polygons.iter().enumerate() {
        let inside = p.rings.iter().filter(|r| point_in_ring([lon, lat], r)).count() % 2 == 1;
        if inside {
            return WaterHit::Body(i as u32);
        }
    }
    WaterHit::Land
}

fn rnd(i: u64, k: &str) -> f64 {
    unit(crate::seed_hash!(Seed(7), k, i))
}

#[test]
fn water_test_matches_brute_force() {
    let f = coast_file(Outside::Sea);
    let idx = WaterIndex::build(&f, WaterParams::default(), &field);
    let mut counts = [0usize; 3];
    for i in 0..20_000u64 {
        let lat = 53.9 + 1.2 * rnd(i, "lat");
        let lon = 19.9 + 2.2 * rnd(i, "lon");
        let want = brute_water(&f, lat, lon);
        assert_eq!(idx.water_at(lat, lon), want, "at {lat}, {lon}");
        counts[match want {
            WaterHit::Land => 0,
            WaterHit::OutsideSea => 1,
            WaterHit::Body(_) => 2,
        }] += 1;
    }
    assert!(counts.iter().all(|c| *c > 500), "{counts:?}");
    // Points hugging the coast line (inside mixed cells, both sides).
    for i in 0..5_000u64 {
        let lat = 54.0 + rnd(i, "clat");
        let lon = coast_lon(lat) + (rnd(i, "dx") - 0.5) * 0.002;
        assert_eq!(idx.water_at(lat, lon), brute_water(&f, lat, lon), "coast {lat}, {lon}");
    }
}

#[test]
fn sea_lake_and_land_heights() {
    let f = coast_file(Outside::Sea);
    let p = WaterParams { sea_level_m: 0.0, ..Default::default() };
    let idx = WaterIndex::build(&f, p, &field);
    let lake_level = q_cm(f.polygons[1].rings.iter().flatten().map(|v| field(v[1], v[0])).fold(f64::INFINITY, f64::min));
    assert_eq!(idx.bodies()[1].level, lake_level);
    assert_eq!(idx.bodies()[0].level, 0.0);
    for i in 0..2000u64 {
        // Open sea, outside the bbox, the lake (not the island), far inland.
        let lat = 54.05 + 0.9 * rnd(i, "a");
        let lon = 20.05 + 0.6 * rnd(i, "b");
        assert_eq!(idx.apply(field(lat, lon), lat, lon), 0.0);
        assert_eq!(idx.apply(field(lat, 19.0), lat, 19.0), 0.0);
        let ang = rnd(i, "c") * std::f64::consts::TAU;
        let r = 0.2 + 0.7 * rnd(i, "d"); // between the island and the shore
        let (la, lo) = (54.5 + r * 2000.0 / M_PER_DEG * crate::math::sin(ang), 21.4 + r * 3000.0 / m_per_deg_lon(54.5) * crate::math::cos(ang));
        if idx.water_at(la, lo) == WaterHit::Body(1) {
            assert_eq!(idx.apply(field(la, lo), la, lo), lake_level);
        }
        let (fl, fo) = (54.1 + 0.05 * rnd(i, "e"), 21.85 + 0.1 * rnd(i, "f"));
        assert_eq!(idx.apply(field(fl, fo), fl, fo), field(fl, fo), "far inland is untouched");
    }
    // The island is land, between the lake level and the field.
    let h = idx.apply(field(54.5, 21.4), 54.5, 21.4);
    assert!(h > lake_level && h <= field(54.5, 21.4), "{h} {lake_level}");
    // Outside land: the field.
    let idx_land = WaterIndex::build(&coast_file(Outside::Land), p, &field);
    assert_eq!(idx_land.apply(12.5, 54.5, 19.0), 12.5);
}

/// Largest height change between 2 m steps on land, and across the shore line, over 40
/// west-east transects of a coast.
fn transect_steps(idx: &WaterIndex, coast: fn(f64) -> f64) -> (f64, f64) {
    let mut worst_step = 0.0f64;
    let mut worst_shore = 0.0f64;
    for k in 0..40 {
        let lat = 54.01 + k as f64 * 0.0245;
        let c = coast(lat);
        let step = 2.0 / m_per_deg_lon(lat); // 2 m
        let mut lon = c - 0.012;
        let mut prev: Option<(f64, bool)> = None;
        while lon < c + 0.02 {
            let h = idx.apply(field(lat, lon), lat, lon);
            assert!(h >= 0.0, "below sea level at {lat}, {lon}: {h}");
            let water = idx.water_at(lat, lon) != WaterHit::Land;
            if let Some((ph, pw)) = prev {
                let d = (h - ph).abs();
                if pw != water {
                    worst_shore = worst_shore.max(d);
                } else {
                    worst_step = worst_step.max(d);
                }
            }
            prev = Some((h, water));
            lon += step;
        }
    }
    (worst_step, worst_shore)
}

#[test]
fn coast_is_continuous_and_never_below_the_water() {
    // Smooth coast (one 7.8 km wave): no visible step anywhere.
    let smooth = WaterIndex::build(&coast_file_with(Outside::Sea, coast_lon_smooth), WaterParams::default(), &field);
    let (step, shore) = transect_steps(&smooth, coast_lon_smooth);
    // Slope of the blend: (field - w) * max smoothstep' * |grad d| = ~52 m * 1.5 / 400 m = 0.2 m
    // per m here, plus the field's own 1 %.
    eprintln!("smooth coast: largest 2 m step on land {step:.3} m, across the shore line {shore:.3} m");
    assert!(step < 0.45, "smooth coast: largest 2 m step on land {step}");
    assert!(shore < 0.45, "smooth coast: step across the shore line {shore} (one 2 m step of the slope)");
    // Coast wiggling at the raster spacing: the bilinear distance is off by up to ~80 m there,
    // so the land next to the water starts up to (field - w) * smoothstep(80 / 400) above it.
    // Measured: ~0.46 m per 2 m on land, ~2.3 m across the shore line (section 5.2a open issue).
    let rough = WaterIndex::build(&coast_file(Outside::Sea), WaterParams::default(), &field);
    let (step, shore) = transect_steps(&rough, coast_lon);
    eprintln!("rough coast: largest 2 m step on land {step:.3} m, across the shore line {shore:.3} m");
    assert!(step < 0.6, "rough coast: largest 2 m step on land {step}");
    assert!(shore < 3.0, "rough coast: step across the shore line {shore}");
}

#[test]
fn raster_distance_is_close_to_exact() {
    let f = coast_file(Outside::Sea);
    let p = WaterParams::default();
    let idx = WaterIndex::build(&f, p, &field);
    let mut worst = 0.0f64;
    let mut n = 0;
    for i in 0..4000u64 {
        let lat = 54.02 + 0.96 * rnd(i, "rl");
        let lon = coast_lon(lat) + 0.006 * rnd(i, "ro"); // up to ~390 m inland
        if idx.water_at(lat, lon) != WaterHit::Land {
            continue;
        }
        let exact = idx.exact_distance_m(lat, lon);
        if exact >= p.blend_m {
            continue;
        }
        n += 1;
        worst = worst.max((idx.raster_distance_m(lat, lon) - exact).abs());
    }
    assert!(n > 2000, "{n}");
    // Sample spacing of a z11 cell at 54.5 N over 63 intervals: ~180 m. The fixture coast wiggles
    // at about twice that wavelength, where bilinear interpolation of the distance is off by up
    // to about half the spacing (measured ~80 m); on a smooth coast the error is a few meters.
    assert!(worst < 100.0, "raster vs exact distance: worst {worst:.1} m");
}

#[test]
fn index_is_deterministic_and_order_independent_in_heights() {
    let f = coast_file(Outside::Sea);
    let a = WaterIndex::build(&f, WaterParams::default(), &field);
    let b = WaterIndex::build(&f, WaterParams::default(), &field);
    assert_eq!(a.stats(), b.stats());
    let mut g = f.clone();
    g.polygons.reverse();
    let c = WaterIndex::build(&g, WaterParams::default(), &field);
    for i in 0..3000u64 {
        let lat = 54.0 + rnd(i, "x");
        let lon = 20.0 + 2.0 * rnd(i, "y");
        let h = a.apply(field(lat, lon), lat, lon);
        assert_eq!(h.to_bits(), b.apply(field(lat, lon), lat, lon).to_bits());
        assert_eq!(h.to_bits(), c.apply(field(lat, lon), lat, lon).to_bits(), "polygon order changed a height at {lat}, {lon}");
    }
    let s = a.stats();
    // Cells along the bbox edges of the sea polygon are mixed (point test) but get no raster (no coast).
    assert!(s.mixed_cells > 10 && s.raster_cells > 10 && s.water_cells > 10, "{s:?}");
    assert_eq!(s.raster_bytes / s.raster_cells.max(1) / 1000, 12, "about 12 KB per raster cell: {s:?}");
}

/// A fractal coast line (midpoint displacement, hash driven) from `a` to `b`, `levels` deep,
/// displacement `rough` x segment length.
fn fractal(a: [f64; 2], b: [f64; 2], levels: u32, rough: f64, seed: u64) -> Vec<[f64; 2]> {
    let mut pts = vec![a, b];
    for l in 0..levels {
        let mut next = Vec::with_capacity(pts.len() * 2);
        for i in 0..pts.len() - 1 {
            let (p, q) = (pts[i], pts[i + 1]);
            let (dx, dy) = (q[0] - p[0], q[1] - p[1]);
            let off = (unit(crate::seed_hash!(Seed(seed), l, i as u64)) - 0.5) * 2.0 * rough;
            next.push(p);
            next.push([(p[0] + q[0]) * 0.5 - dy * off, (p[1] + q[1]) * 0.5 + dx * off]);
        }
        next.push(*pts.last().unwrap());
        pts = next;
    }
    pts
}

/// Measurement for section 10a (run with `--ignored --nocapture`): a fractal coast of about
/// `COAST_KM` km and `LAKES` lakes, simplified at 50 m like the CLI does, then the index, the
/// rasters at 64 and 32, the build time and the per-sample cost of the rule against the field.
#[test]
#[ignore]
fn measure_water_index() {
    use std::time::Instant;
    let bbox = [21.0, 54.0, 28.0, 60.0];
    let mut polys = Vec::new();
    // Coast: from the south-west to the north-east corner, rough, simplified at 50 m.
    let line = fractal([23.0, 54.0], [24.0, 60.0], 16, 0.32, 1);
    let mut sea = vec![[21.0, 54.0]];
    sea.extend(line.iter().map(|p| [p[0].clamp(21.0, 28.0), p[1].clamp(54.0, 60.0)]));
    sea.push([21.0, 60.0]);
    let sea = simplify_ring(&crate::seed::files::clean_ring(&sea).unwrap(), 50.0);
    let mut coast_km = 0.0;
    for w in sea.windows(2) {
        coast_km += crate::math::hypot((w[1][0] - w[0][0]) * m_per_deg_lon(w[0][1]), (w[1][1] - w[0][1]) * M_PER_DEG) / 1000.0;
    }
    polys.push(WaterPolygon { kind: WaterKind::Sea, rings: vec![sea.clone()] });
    let lakes = 800u64;
    let mut lake_pts = 0;
    for i in 0..lakes {
        let c = [24.5 + 3.3 * rnd(i, "lx"), 54.2 + 5.6 * rnd(i, "ly")];
        let r = 300.0 + 2500.0 * rnd(i, "lr") * rnd(i, "lr2");
        let mut ring = ellipse(c, r, r * (0.4 + 0.6 * rnd(i, "le")), 64, true);
        // Wobble the outline.
        for (k, p) in ring.iter_mut().enumerate() {
            let s = 1.0 + 0.25 * (rnd(i * 1000 + k as u64, "lw") - 0.5);
            *p = [c[0] + (p[0] - c[0]) * s, c[1] + (p[1] - c[1]) * s];
        }
        let ring = simplify_ring(&ring, 50.0);
        lake_pts += ring.len();
        polys.push(WaterPolygon { kind: WaterKind::Lake, rings: vec![ring] });
    }
    let file = WaterFile {
        format: 1,
        source: "synthetic".into(),
        source_date: None,
        bbox,
        outside: Outside::Sea,
        tolerance_m: 50.0,
        min_area_m2: None,
        polygons: polys,
    };
    let json = serde_json::to_string(&file).unwrap();
    println!(
        "synthetic water file: coast {:.0} km ({} points after DP 50 m), {lakes} lakes ({lake_pts} points), JSON {:.2} MB",
        coast_km,
        sea.len(),
        json.len() as f64 / 1e6
    );
    let terrain_profile = crate::profile::TerrainProfile { enabled: true, flat_radius_m: 0.0, flat_blend_m: 0.0, ..Default::default() };
    let terrain = crate::terrain::Terrain::new(&terrain_profile, crate::profile::Profile::default().bands(), 56.0, 24.0);
    let fieldf = |lat: f64, lon: f64| terrain.height(lat, lon);
    for n in [64usize, 32] {
        let t = Instant::now();
        let idx = WaterIndex::build(&file, WaterParams { raster_n: n, ..Default::default() }, &fieldf);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let s = idx.stats();
        println!(
            "raster {n}x{n}: build {ms:.0} ms; grid {} cells, water {}, mixed {}, rasters {}; ring points {}, band refs {}; index {:.2} MB, rasters {:.2} MB ({:.1} KB each)",
            s.grid_cells,
            s.water_cells,
            s.mixed_cells,
            s.raster_cells,
            s.ring_points,
            s.band_refs,
            s.index_bytes as f64 / 1048576.0,
            s.raster_bytes as f64 / 1048576.0,
            s.raster_bytes as f64 / s.raster_cells.max(1) as f64 / 1024.0
        );
        // Per-sample cost: random points over the bbox, field alone vs field + rule.
        let pts: Vec<(f64, f64)> = (0..200_000u64).map(|i| (54.0 + 6.0 * rnd(i, "py"), 21.0 + 7.0 * rnd(i, "px"))).collect();
        let t = Instant::now();
        let mut acc = 0.0;
        for &(la, lo) in &pts {
            acc += fieldf(la, lo);
        }
        let base = t.elapsed().as_secs_f64() / pts.len() as f64 * 1e9;
        let t = Instant::now();
        for &(la, lo) in &pts {
            acc += idx.apply(fieldf(la, lo), la, lo);
        }
        let with = t.elapsed().as_secs_f64() / pts.len() as f64 * 1e9;
        // Only the coast band (mixed / raster cells).
        let near: Vec<(f64, f64)> = pts.iter().copied().filter(|&(la, lo)| idx.raster_distance_m(la, lo) < 400.0).take(20_000).collect();
        let t = Instant::now();
        for &(la, lo) in &near {
            acc += idx.apply(fieldf(la, lo), la, lo);
        }
        let near_ns = t.elapsed().as_secs_f64() / near.len().max(1) as f64 * 1e9;
        println!("  per sample: field {base:.0} ns, field + water rule {with:.0} ns ({:.2}x), near water {near_ns:.0} ns [{acc:.0}]", with / base);
    }
}

#[test]
fn city_water_warnings() {
    let f = coast_file(Outside::Sea);
    let idx = WaterIndex::build(&f, WaterParams::default(), &field);
    let mut c: crate::seed::files::City = serde_json::from_str(include_str!("../../../../low-poly-map/world/two-cities/cities/kaunas.json")).unwrap();
    c.center = crate::seed::files::LatLon { lat: 54.5, lon: 20.5 };
    let w = idx.city_warnings(std::slice::from_ref(&c));
    assert!(w.iter().any(|x| x.contains("centre") && x.contains("in water")), "{w:?}");
    assert!(w.iter().any(|x| x.contains("% water")), "{w:?}");
    c.center = crate::seed::files::LatLon { lat: 54.2, lon: 21.7 };
    assert!(idx.city_warnings(&[c]).is_empty());
}
