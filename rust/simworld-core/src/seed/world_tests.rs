//! Seed world S1 tests (`docs/spec-world-seed.md` sections 11a.3 "S1" and 12) on the shared
//! two-cities world (`low-poly-map/world/two-cities/`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::*;
use crate::geo::{tile_for, Frame};
use crate::mvt::{GROUND_FOREST, GROUND_GRASS};
use crate::seed::airport::SUB_RUNWAY_PAVED;
use crate::seed::files::{q_cm, WaterKind};
use crate::seed::water::WaterHit;
use crate::world::{World, WorldOptions};
use crate::worker::build_seed_tile_on;

#[path = "world_stream_tests.rs"]
mod stream;

const VILNIUS: (f64, f64) = (54.6872, 25.2797);

/// Points in the Baltic west of the Lithuanian coast (inside the water file's bbox).
const OFFSHORE: [(f64, f64); 10] = [
    (55.40, 20.90),
    (55.50, 20.70),
    (55.60, 20.95),
    (55.70, 20.60),
    (55.80, 20.90),
    (55.90, 20.80),
    (56.00, 20.95),
    (55.45, 20.40),
    (55.65, 19.90),
    (56.20, 20.50),
];

fn seed_options(lat: f64, lon: f64) -> WorldOptions {
    WorldOptions {
        origin_lat: lat,
        origin_lon: lon,
        profile_overrides: Some(test_profile_json()),
        layers: Some(vec!["world".into(), "terrain".into()]),
        seed_world: Some(TEST_WORLD.into()),
        ..WorldOptions::default()
    }
}

fn new_world() -> World {
    World::new(Arc::new(crate::NullLog), crate::Diagnostics::default(), "test")
}

fn is_kind(w: &SeedWorld, b: u32, k: WaterKind) -> bool {
    w.water.bodies()[b as usize].kind == k
}

#[test]
fn loads_the_two_cities_world() {
    let (_, _, w) = test_world();
    let s = w.stats;
    eprintln!(
        "two-cities: {} airports, {} water polygons, index {:.2} MiB, rasters {:.2} MiB ({} mixed cells, {} rasters), strips {:.1} KiB, airports {:.1} KiB, load {:.0} ms",
        w.airports.len(),
        w.files.water.polygons.len(),
        s.water_index_bytes as f64 / 1_048_576.0,
        s.water_raster_bytes as f64 / 1_048_576.0,
        s.water_mixed_cells,
        s.water_raster_cells,
        s.strip_index_bytes as f64 / 1024.0,
        s.airport_bytes as f64 / 1024.0,
        s.build_ms
    );
    for ident in ["EYVI", "EYKA", "EYPA"] {
        let a = w.airport(ident).unwrap_or_else(|| panic!("{ident}"));
        assert!(a.runways.iter().all(|r| !r.approximate), "{ident}: both ends in the file");
        assert!(a.buildings.iter().any(|b| b.kind == "transportation") && a.areas.iter().any(|x| x.sub == SUB_RUNWAY_PAVED));
        assert!(!a.lights.is_empty(), "{ident}: lighted runway");
    }
    // Approximate runways (Pociunai: three runways without ends) are placed at the reference point.
    let eypr = w.airport("EYPR").unwrap();
    assert_eq!(eypr.runways.len(), 3);
    assert!(eypr.runways.iter().all(|r| r.approximate));
    assert_eq!(w.attributions, ["\u{a9} OpenStreetMap contributors", "Airport data: OurAirports"]);
}

/// Spec 11a.3 S1: along the runway centreline the height is linear between the threshold
/// heights within 1 cm and equal across the strip; a runway whose strip overlaps another strip
/// of the airport (parallel or crossing) takes the profile of its group's longest runway, so the
/// height never steps on a runway (S1 fix).
fn check_runway(t: &Terrain, g: &AirportGeo, ri: usize) {
    let r = &g.runways[ri];
    let pl = &g.runways[crate::seed::airport::runway_planes(g)[ri]];
    let lin = |q: [f64; 2]| {
        let d = [q[0] - pl.a[0], q[1] - pl.a[1]];
        pl.height_at(d[0] * pl.dir[0] + d[1] * pl.dir[1])
    };
    for i in 0..20 {
        let s = r.le_thr_s + (r.he_thr_s - r.le_thr_s) * (i as f64 / 19.0);
        for tt in [0.0, -r.width_m * 0.5, r.width_m * 0.5, r.strip_half_m * 0.9, -r.strip_half_m * 0.9] {
            let q = r.at(s, tt);
            let (lat, lon) = g.frame.ll(q);
            let h = t.height(lat, lon);
            assert!((h - lin(q)).abs() <= 0.01, "{} {}: s {s:.0} t {tt:.0}: {h} vs linear {}", g.ident, r.key, lin(q));
        }
    }
    // Threshold heights: the terrain before the strips (1 cm), the slope capped at max_slope.
    let before = |s: f64| {
        let (lat, lon) = g.frame.ll(r.at(s, 0.0));
        q_cm(t.seed().unwrap().height_before_strips(lat, lon, || t.field_only_height(lat, lon)))
    };
    let span = r.he_thr_s - r.le_thr_s;
    assert!((r.he_thr_h - r.le_thr_h).abs() <= 0.01 * span + 0.011, "{} {}: slope", g.ident, r.key);
    if (before(r.he_thr_s) - before(r.le_thr_s)).abs() <= 0.01 * span {
        assert_eq!((r.le_thr_h, r.he_thr_h), (before(r.le_thr_s), before(r.he_thr_s)));
    } else {
        assert!(((r.le_thr_h + r.he_thr_h) - (before(r.le_thr_s) + before(r.he_thr_s))).abs() < 0.02, "capped around the mean");
    }
}

#[test]
fn runways_are_level_in_the_height_function() {
    let (_, t, w) = test_world();
    for ident in ["EYVI", "EYKA", "EYPA", "EYSA"] {
        let g = w.airport(ident).unwrap();
        for ri in 0..g.runways.len() {
            check_runway(t, g, ri);
        }
        // Inside the fence the ground is level with the nearest strip: within the strips' range.
        let (lo, hi) = g.runways.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), r| {
            let (a, b) = (r.height_at(0.0), r.height_at(r.length_m));
            (lo.min(a.min(b)), hi.max(a.max(b)))
        });
        for q in g.fence.ring() {
            let c = [g.fence.c[0] + (q[0] - g.fence.c[0]) * 0.98, g.fence.c[1] + (q[1] - g.fence.c[1]) * 0.98];
            let (lat, lon) = g.frame.ll(c);
            let h = t.height(lat, lon);
            assert!(h >= lo - 0.01 && h <= hi + 0.01, "{ident}: fence corner {h} outside [{lo}, {hi}]");
        }
    }
    // Crossing approximate runways (EYPR, three runways through the reference point): one shared
    // plane, linear and continuous on every runway.
    let g = w.airport("EYPR").unwrap();
    let planes = crate::seed::airport::runway_planes(g);
    assert!(planes.iter().all(|&p| p == planes[0]), "EYPR: one group");
    for ri in 0..g.runways.len() {
        check_runway(t, g, ri);
    }
    // The same heights through the World API.
    let mut world = new_world();
    let res = world.configure_world(seed_options(VILNIUS.0, VILNIUS.1));
    assert!(res.ok, "{}", res.error);
    let g = world.seed_world().unwrap().airport("EYVI").unwrap().clone();
    let r = &g.runways[0];
    for i in 0..20 {
        let (lat, lon) = g.frame.ll(r.at(r.le_thr_s + (r.he_thr_s - r.le_thr_s) * (i as f64 / 19.0), 0.0));
        let p = world.geodetic_to_local(lat, lon, 0.0).unwrap();
        let h = world.ground_height_at(p[0], p[2]);
        assert!((h - t.height(lat, lon)).abs() < 0.02, "ground_height_at {h} vs {}", t.height(lat, lon));
    }
}

#[test]
fn airports_api_lists_eyvi_and_eyka_with_both_ends() {
    let mut world = new_world();
    assert!(world.airports().is_empty() && world.seed_world_info().is_none(), "no seed world yet");
    assert!(world.configure_world(seed_options(VILNIUS.0, VILNIUS.1)).ok);
    let list = world.airports();
    for ident in ["EYVI", "EYKA"] {
        let a = list.iter().find(|a| a.ident == ident).unwrap_or_else(|| panic!("{ident} missing"));
        let r = &a.runways[0];
        assert!(!r.approximate && r.lighted && r.paved && r.length_m > 2000.0, "{ident}: {r:?}");
        assert!(r.le.lat != r.he.lat && r.le.lon != r.he.lon);
        assert!(!r.lights.is_empty() && r.lights.len() == r.light_colors.len());
        // Positions are local and on the ground.
        let h = world.ground_height_at(r.le.threshold_position[0], r.le.threshold_position[2]);
        assert!((h - r.le.threshold_position[1]).abs() < 0.05, "{ident}: threshold y {} vs ground {h}", r.le.threshold_position[1]);
        assert!(((r.le.heading_deg - r.he.heading_deg).abs() - 180.0).abs() < 1e-6);
    }
    let eyvi = list.iter().find(|a| a.ident == "EYVI").unwrap();
    // EYVI runway 01/19: about 2.5 km long, about 20 degrees true.
    assert!((eyvi.runways[0].length_m - 2515.0).abs() < 60.0, "{}", eyvi.runways[0].length_m);
    assert!((eyvi.runways[0].le.heading_deg - 20.0).abs() < 4.0, "{}", eyvi.runways[0].le.heading_deg);
    let info = world.seed_world_info().unwrap();
    assert_eq!(info.phases, ["S0", "S1"]);
    assert_eq!(info.generator_version, SEED_GEN_VERSION);
    assert_eq!((info.world_seed.as_str(), info.world_version), ("bench-two-cities", 1));
    assert_eq!(info.cities.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["vilnius", "kaunas"]);
    assert!(info.cities.iter().all(|c| c.source == "synthetic"));
    assert_eq!(world.attributions(), ["\u{a9} OpenStreetMap contributors", "Airport data: OurAirports"]);
}

#[test]
fn the_sea_is_at_sea_level_offshore_also_in_the_terrain_mesh() {
    let (p, t, w) = test_world();
    let sea = p.terrain.sea_level_m;
    for (lat, lon) in OFFSHORE {
        assert_ne!(w.water.water_at(lat, lon), WaterHit::Land, "({lat}, {lon}) is in the sea");
        assert_eq!(t.height(lat, lon), sea, "({lat}, {lon})");
    }
    // Terrain tiles over the coast: surface vertices in the sea lie at sea level.
    let frame = Frame::new(55.6, 21.0);
    for z in [14u8, 12, 10] {
        let ring = (p.terrain.near_zoom as u8 - z) as usize;
        let key = tile_for(55.6, 21.06, z);
        let tile = crate::terrain::tin::build_terrain_tile(key, ring, frame, t, p, 1.0).unwrap();
        let center = frame.tile_center(key);
        let surface = tile.mesh.indices[..tile.surface_triangles as usize * 3].iter().copied().max().unwrap() as usize + 1;
        let (mut wet, mut checked) = (0, 0);
        for v in &tile.mesh.positions[..surface] {
            let (lat, lon) = frame.lat_lon(v[0] as f64 + center[0], -(v[2] as f64) + center[1]);
            if let WaterHit::Body(b) = w.water.water_at(lat, lon) {
                if is_kind(w, b, WaterKind::Sea) {
                    checked += 1;
                    if (v[1] as f64 - sea).abs() < 1e-3 {
                        wet += 1;
                    }
                }
            }
        }
        assert!(wet > 0 && wet == checked, "z{z}: {wet} of {checked} sea vertices at sea level");
    }
}

#[test]
fn lakes_are_flat_and_rivers_are_not_flattened() {
    let (_, t, w) = test_world();
    // Around Kaunas: the reservoir (Kauno marios) and the Nemunas / Neris.
    let mut by_body: std::collections::BTreeMap<u32, Vec<(f64, f64)>> = Default::default();
    for i in 0..80 {
        for j in 0..40 {
            let (lat, lon) = (54.84 + j as f64 * 0.002, 23.85 + i as f64 * 0.004);
            if let WaterHit::Body(b) = w.water.water_at(lat, lon) {
                by_body.entry(b).or_default().push((lat, lon));
            }
        }
    }
    let lakes: Vec<_> = by_body.iter().filter(|(b, _)| is_kind(w, **b, WaterKind::Lake)).collect();
    let (b, pts) = lakes.iter().max_by_key(|(_, v)| v.len()).expect("a lake near Kaunas");
    assert!(pts.len() > 50, "the reservoir is large: {}", pts.len());
    let level = w.water.bodies()[**b as usize].level;
    for &(lat, lon) in pts.iter() {
        assert_eq!(t.height(lat, lon), level, "lake {b} at ({lat}, {lon})");
    }
    // Rivers (S1 decision): not part of the terrain rule, so a river follows the relief.
    let rivers: Vec<_> = by_body.iter().filter(|(b, _)| is_kind(w, **b, WaterKind::River)).collect();
    let river_pts: Vec<(f64, f64)> = rivers.iter().flat_map(|(_, v)| v.iter().copied()).collect();
    assert!(!river_pts.is_empty(), "the Nemunas / Neris near Kaunas");
    let flat = river_pts
        .iter()
        .filter(|&&(lat, lon)| rivers.iter().any(|(b, _)| t.height(lat, lon) == w.water.bodies()[**b as usize].level))
        .count();
    assert!(flat * 10 < river_pts.len(), "{flat} of {} river points sit at a river level", river_pts.len());
}

#[test]
fn generated_tiles_are_deterministic_and_seamless_across_zooms() {
    let (_, _, w) = test_world();
    let k16 = tile_for(55.0, 24.5, 16);
    let a = format!("{:?}", generate(k16, w));
    assert_eq!(a, format!("{:?}", generate(k16, w)));
    // A countryside patch has the same outline at every zoom (power-of-two scale, exact).
    let k13 = TileKey::new(13, k16.x >> 3, k16.y >> 3);
    let (g16, g13) = (generate(k16, w).ground, generate(k13, w).ground);
    let mut shared = 0;
    for a16 in &g16.areas {
        if let Some(a13) = g13.areas.iter().find(|a| a.id == a16.id && a.level == a16.level && a.class == a16.class) {
            for (p16, p13) in a16.rings[0].iter().zip(&a13.rings[0]) {
                let back = [p13[0] * 8.0 - (k16.x - k13.x * 8) as f64 * EXTENT, p13[1] * 8.0 - (k16.y - k13.y * 8) as f64 * EXTENT];
                assert_eq!(*p16, back, "patch {} vertex", a16.id);
            }
            shared += 1;
        }
    }
    assert!(shared > 0, "patches shared between z16 and z13");
}

#[test]
fn airport_ground_class_7_sub_kinds_lights_and_kinds() {
    let (p, t, w) = test_world();
    let g = w.airport("EYVI").unwrap();
    let (lat, lon) = g.frame.ll(g.runways[0].at(g.runways[0].length_m * 0.5, 0.0));
    let frame = Frame::new(lat, lon);
    for ring in 0..4usize {
        let key = tile_for(lat, lon, 16 - ring as u8);
        let d = build_seed_tile_on(key, ring, frame, t, w, p).unwrap();
        let classes: Vec<(u8, u8)> = d.ground.colors.iter().map(|c| ((c[0] * 16.0).round() as u8, (c[2] * 8.0).round() as u8)).collect();
        assert!(classes.contains(&(7, SUB_RUNWAY_PAVED)), "ring {ring}: a paved runway (class 7, b = 1/8)");
        assert!(classes.contains(&(7, 0)), "ring {ring}: the airfield");
        assert!(!d.lights.airport_positions.is_empty(), "ring {ring}: airport lights");
    }
    // Buildings resolve to the intended light profiles (section 5.1 step 5).
    let key = tile_for(lat, lon, 14);
    let dec = generate(key, w).buildings;
    assert!(!dec.is_empty());
    for b in &dec {
        let built = crate::mvt::TileProjector::new(key, frame, EXTENT).finish(b.clone()).unwrap();
        let want = match b.kind.as_str() {
            "transportation" => crate::mesher::PROFILE_CIVIC,
            "hangar" => crate::mesher::PROFILE_INDUSTRIAL,
            "office" => crate::mesher::PROFILE_OFFICE,
            k => panic!("unexpected kind {k}"),
        };
        assert_eq!(crate::mesher::resolve(&built, p).profile, want, "{}", b.kind);
    }
    assert!(dec.iter().any(|b| b.kind == "office" && b.height == Some(45.0)), "EYVI tower");
    // seed.aeroway_class = 6 for a host without the class 7 branch.
    let o = serde_json::json!({ "terrain": { "enabled": true, "flat_radius_m": 0.0, "flat_blend_m": 0.0 }, "seed": { "aeroway_class": 6 } });
    let p6 = crate::profile::resolve(None, Some(&o), &crate::NullLog).unwrap();
    let field = Terrain::from_profile(&p6, lat, lon);
    let read = |path: &str| std::fs::read_to_string(path).map_err(|e| e.to_string());
    let w6 = SeedWorld::load(TEST_WORLD, &read, &p6, &field, None).unwrap();
    let d = build_seed_tile_on(tile_for(lat, lon, 16), 0, frame, &field.with_seed(w6.terrain.clone()), &w6, &p6).unwrap();
    let classes: Vec<(u8, u8)> = d.ground.colors.iter().map(|c| ((c[0] * 16.0).round() as u8, (c[2] * 8.0).round() as u8)).collect();
    assert!(classes.contains(&(6, SUB_RUNWAY_PAVED)) && !classes.iter().any(|c| c.0 == 7));
}

#[test]
fn countryside_has_forest_fields_and_tints_in_every_band() {
    let (p, t, w) = test_world();
    let (lat, lon) = (55.25, 24.3); // between the cities
    let frame = Frame::new(lat, lon);
    for ring in 0..4usize {
        let d = build_seed_tile_on(tile_for(lat, lon, 16 - ring as u8), ring, frame, t, w, p).unwrap();
        assert!(!d.ground.is_empty(), "ring {ring}: countryside ground");
    }
    let d = build_seed_tile_on(tile_for(lat, lon, 13), 3, frame, t, w, p).unwrap();
    let classes: std::collections::BTreeSet<u8> = d.ground.colors.iter().map(|c| (c[0] * 16.0).round() as u8).collect();
    let tints: std::collections::BTreeSet<u8> = d.ground.colors.iter().map(|c| (c[1] * 8.0).round() as u8).collect();
    assert!(classes.contains(&GROUND_FOREST) && classes.contains(&GROUND_GRASS), "{classes:?}");
    assert!(tints.len() >= 4, "tints {tints:?}");
    // The terrain colour class: water, airfield, countryside.
    let (class, tint) = w.class_at(lat, lon);
    assert!((class == GROUND_FOREST || class == GROUND_GRASS) && tint < 8);
    assert_eq!(w.class_at(55.5, 20.9), (crate::mvt::GROUND_WATER, 0), "sea");
    let g = w.airport("EYKA").unwrap();
    let (alat, alon) = g.frame.ll(g.fence.c);
    assert_eq!(w.class_at(alat, alon), (7, 0), "inside a fence");
}

#[test]
fn terrain_colour_only_in_a_seed_world() {
    let (p, t, w) = test_world();
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let key = tile_for(VILNIUS.0, VILNIUS.1, 12);
    let plain = Terrain::from_profile(p, VILNIUS.0, VILNIUS.1);
    let a = crate::worker::build_terrain_data(key, 2, frame, &plain, p, 1.0, None).unwrap();
    assert!(a.terrain.as_ref().unwrap().mesh.colors.is_empty(), "no COLOR outside a seed world");
    let b = crate::worker::build_terrain_data(key, 2, frame, t, p, 1.0, Some(w)).unwrap();
    let m = &b.terrain.as_ref().unwrap().mesh;
    assert_eq!(m.colors.len(), m.positions.len());
    for c in &m.colors {
        let class = c[0] * 8.0;
        assert_eq!(class, class.round(), "r = class / 8");
        assert!(c[1] * 8.0 < 8.0 && c[2] == 0.0 && c[3] == 1.0);
    }
}

/// Section 10a budgets (run with `--ignored --nocapture`): height cost with the seed rules
/// against the plain field, and seed tile build times per band.
#[test]
#[ignore]
fn measure_seed_budgets() {
    let (p, t, w) = test_world();
    let plain = Terrain::from_profile(p, VILNIUS.0, VILNIUS.1);
    let mut pts = Vec::new();
    for i in 0..200_000u64 {
        let u = |k: &str| crate::seed::hash::unit(crate::seed_hash!(Seed(11), k, i));
        pts.push((53.9 + 2.5 * u("lat"), 20.9 + 5.9 * u("lon")));
    }
    let time = |f: &dyn Fn(f64, f64) -> f64, pts: &[(f64, f64)]| {
        let t0 = Instant::now();
        let mut acc = 0.0;
        for &(a, b) in pts {
            acc += f(a, b);
        }
        (t0.elapsed().as_secs_f64() / pts.len() as f64 * 1e9, acc)
    };
    let (base, _) = time(&|a, b| plain.height(a, b), &pts);
    let (with, _) = time(&|a, b| t.height(a, b), &pts);
    let land: Vec<(f64, f64)> = pts.iter().copied().filter(|&(a, b)| w.water.water_at(a, b) == WaterHit::Land).collect();
    let (land_base, _) = time(&|a, b| plain.height(a, b), &land);
    let (land_with, _) = time(&|a, b| t.height(a, b), &land);
    println!(
        "height per sample: field {base:.0} ns, seed world {with:.0} ns ({:.2}x); land only {land_base:.0} -> {land_with:.0} ns ({:.2}x)",
        with / base,
        land_with / land_base
    );
    for (name, lat, lon) in [("EYVI", 54.634, 25.2858), ("coast", 55.6, 21.06), ("countryside", 55.25, 24.3)] {
        let frame = Frame::new(lat, lon);
        for ring in 0..4usize {
            let key = tile_for(lat, lon, 16 - ring as u8);
            let n = 5;
            let t0 = Instant::now();
            let mut tris = 0;
            for _ in 0..n {
                tris = build_seed_tile_on(key, ring, frame, t, w, p).unwrap().ground.triangle_count();
            }
            println!("  {name} R{ring}: {:.2} ms per tile, ground {tris} triangles", t0.elapsed().as_secs_f64() * 1000.0 / n as f64);
        }
        for z in [14u8, 12, 10] {
            let ring = (p.terrain.near_zoom as u8 - z) as usize;
            let key = tile_for(lat, lon, z);
            let t0 = Instant::now();
            let d = crate::worker::build_terrain_data(key, ring, frame, t, p, 1.0, Some(w)).unwrap();
            let t1 = Instant::now();
            let _ = crate::worker::build_terrain_data(key, ring, frame, &plain, p, 1.0, None).unwrap();
            println!(
                "  {name} terrain z{z}: {:.1} ms with seed + colour, {:.1} ms plain, {} triangles",
                (t1 - t0).as_secs_f64() * 1000.0,
                t1.elapsed().as_secs_f64() * 1000.0,
                d.terrain.unwrap().triangle_count()
            );
        }
    }
}
