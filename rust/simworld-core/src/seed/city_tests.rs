//! Cities on the shared two-cities world (`docs/spec-world-seed.md` section 12): plans, the
//! airport rule, seams, ownership, kinds, determinism, cost.

use std::collections::BTreeMap;

use super::city::hex::{add, edge_key, DIRS};
use super::city::{CellKind, CityPlan};
use super::world::{generate, test_world, tile_box};
use crate::geo::{lat_to_tile_y, lon_to_tile_x, TileKey};
use crate::mvt::centroid_px;

fn plans() -> Vec<std::sync::Arc<CityPlan>> {
    let w = &test_world().2;
    (0..w.cities().len()).map(|i| w.city_plan(i)).collect()
}

fn key_at(lat: f64, lon: f64, z: u8) -> TileKey {
    TileKey::new(z, lon_to_tile_x(lon, z) as u32, lat_to_tile_y(lat, z) as u32)
}

#[test]
fn both_plans_build_with_their_districts() {
    let w = &test_world().2;
    for p in plans() {
        let city = &w.cities()[p.index];
        let s = &p.stats;
        eprintln!(
            "{}: land {} water {} airport {} districts {} by type {:?} arterials {} comps {}/{} bridges {} radials {} access {} build {:.1} ms {} KB; warnings {:?}",
            p.id, s.land_cells, s.water_cells, s.airport_cells, s.districts, s.cells_by_type, s.arterial_edges, s.arterial_components_before,
            s.arterial_components, s.bridges, s.radials, s.airport_access, s.build_ms, p.bytes() / 1024, p.warnings
        );
        assert!(s.land_cells > 50, "{}", s.land_cells);
        // Each `cells` request is met; the fill takes the rest.
        for (ri, r) in city.districts.iter().enumerate() {
            let got: usize = p.districts.iter().filter(|d| d.request == ri).map(|d| d.cells.len()).sum();
            if let Some(n) = r.cells {
                assert_eq!(got, n as usize, "{} request {ri}", p.id);
            }
            assert!(got > 0, "{} request {ri} got no cell", p.id);
        }
        // The drop never splits the arterial network.
        assert_eq!(s.arterial_components, s.arterial_components_before, "{}", p.id);
        assert!(s.radials >= 3);
        assert_eq!(s.airport_access as usize, city.airports.len(), "{}: access roads", p.id);
        // No district over a cap of max_district_m2.
        let cap = w.params.city.max_district_m2;
        for d in &p.districts {
            assert!(d.cells.len() as f64 * p.grid.cell_area() <= cap.max(p.grid.cell_area()), "{}", p.id);
            assert!(d.cells.iter().all(|a| p.cells[a].kind == CellKind::Land));
        }
    }
}

/// Spec 5.1 step 3 with contract 7 "[1.7] Per-vertex walls": the roof the mesher draws (its own
/// base rule, `TileProjector::finish`, in every R2-R0 tile the footprint touches) + the
/// building's height stays at or below the approach surface (+ 1 cm) at every footprint point
/// inside a trapezoid (vertices and points every 0.5 m along the edges).
#[test]
fn mesh_roofs_stay_under_the_approach_surfaces() {
    use crate::mvt::{BuildingTags, OsmType, RawBuilding, TileProjector};
    use crate::rings::{LEAF_ZOOM, ROOT_ZOOM};
    use crate::seed::world::{px_of, EXTENT};
    let (_, terrain, w) = test_world();
    let frame = crate::geo::Frame::new(54.6872, 25.2797);
    let (mut checked, mut capped, mut worst) = (0usize, 0usize, f64::NEG_INFINITY);
    let mut bad = Vec::new();
    for p in plans() {
        for c in p.cells.values().filter(|c| c.district.is_some()) {
            let cp = w.cell_plan(&p, c.ax);
            for b in &cp.buildings {
                let ll: Vec<(f64, f64)> = b.ring.iter().map(|q| p.frame.ll(cp.abs(*q))).collect();
                let bb = ll.iter().fold([f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY], |m, &(la, lo)| {
                    [m[0].min(lo), m[1].min(la), m[2].max(lo), m[3].max(la)]
                });
                if w.approach.within(bb).is_empty() {
                    continue;
                }
                // Footprint points: vertices and every 0.5 m along each edge (plan metres).
                let mut pts = Vec::new();
                for i in 0..b.ring.len() {
                    let (a, e) = (cp.abs(b.ring[i]), cp.abs(b.ring[(i + 1) % b.ring.len()]));
                    let n = ((e[0] - a[0]).hypot(e[1] - a[1]) / 0.5).ceil().max(1.0) as usize;
                    for k in 0..n {
                        let t = k as f64 / n as f64;
                        pts.push(p.frame.ll([a[0] + (e[0] - a[0]) * t, a[1] + (e[1] - a[1]) * t]));
                    }
                }
                let cap = pts.iter().filter_map(|&(la, lo)| w.approach.limit(la, lo)).fold(f64::INFINITY, f64::min);
                if !cap.is_finite() {
                    continue;
                }
                checked += 1;
                capped += b.capped as usize;
                // The mesher's base in every tile of every building band the footprint touches.
                for z in ROOT_ZOOM + 1..=LEAF_ZOOM {
                    let (x0, x1) = (lon_to_tile_x(bb[0], z) as u32, lon_to_tile_x(bb[2], z) as u32);
                    let (y0, y1) = (lat_to_tile_y(bb[3], z) as u32, lat_to_tile_y(bb[1], z) as u32);
                    for ty in y0..=y1 {
                        for tx in x0..=x1 {
                            let key = TileKey::new(z, tx, ty);
                            let proj = TileProjector::new(key, frame, EXTENT).with_terrain(terrain.clone());
                            let raw = RawBuilding {
                                osm_type: OsmType::Way,
                                id: b.id,
                                kind: b.kind.clone(),
                                height: Some(b.height_m),
                                levels: Some(b.levels),
                                min_height: 0.0,
                                landuse: None,
                                tags: BuildingTags::default(),
                                polygons: vec![vec![ll.iter().map(|&(la, lo)| px_of(key, la, lo)).collect()]],
                            };
                            let mb = proj.finish(raw).expect("a footprint");
                            let over = (mb.base_m + b.height_m) as f64 - cap;
                            worst = worst.max(over);
                            if over > 0.01 {
                                bad.push(format!("{} building {} z{z}: roof {:.2} is {over:.2} m over {cap:.2}", p.id, b.id, mb.base_m + b.height_m));
                            }
                        }
                    }
                }
            }
        }
    }
    eprintln!("approach: {checked} buildings under a surface, {capped} capped, worst {worst:.3} m, {} over", bad.len());
    assert!(checked > 50 && capped > 0, "{checked} {capped}");
    assert!(bad.is_empty(), "{} roofs over the approach surface, e.g. {:?}", bad.len(), &bad[..bad.len().min(5)]);
}

/// Review M1: a footprint may overhang its cell box into a same-district neighbour; the cell's
/// approach query box (grown by `APPROACH_MARGIN_M`) holds every building of both cities, so the
/// per-footprint fallback query never runs with the default styles.
#[test]
fn every_building_lies_inside_its_cells_approach_box() {
    use super::city::buildings::{approach_box, APPROACH_MARGIN_M};
    let w = &test_world().2;
    let (mut n, mut worst) = (0usize, 0.0f64);
    for p in plans() {
        for c in p.cells.values().filter(|c| c.district.is_some()) {
            let cp = w.cell_plan(&p, c.ax);
            let ab = approach_box(&p.frame, c.bbox);
            for b in &cp.buildings {
                n += 1;
                for q in &b.ring {
                    let a = cp.abs(*q);
                    worst = worst.max(c.bbox[0] - a[0]).max(c.bbox[1] - a[1]).max(a[0] - c.bbox[2]).max(a[1] - c.bbox[3]);
                    let (la, lo) = p.frame.ll(a);
                    assert!(ab[0] <= lo && lo <= ab[2] && ab[1] <= la && la <= ab[3], "{} building {} outside its approach box", p.id, b.id);
                }
            }
        }
    }
    eprintln!("approach box: {n} buildings, largest overhang past the cell box {worst:.2} m (margin {APPROACH_MARGIN_M} m)");
    assert!(n > 1000 && worst < APPROACH_MARGIN_M, "{n} {worst}");
}

#[test]
fn neighbouring_cells_share_their_edges_exactly() {
    for p in plans() {
        for c in p.cells.values() {
            for d in DIRS {
                let Some(n) = p.cells.get(&add(c.ax, d)) else { continue };
                let (lo, _) = edge_key(c.ax, n.ax);
                if lo != c.ax {
                    continue;
                }
                let shared = c.poly.iter().filter(|q| n.poly.contains(q)).count();
                assert!(shared >= 12, "{} cells {:?} {:?} share {shared} points", p.id, c.ax, n.ax);
            }
        }
    }
}

#[test]
fn nothing_generated_inside_airport_fences() {
    let w = &test_world().2;
    let mut buildings = 0;
    for p in plans() {
        for c in p.cells.values().filter(|c| c.district.is_some()) {
            let cp = w.cell_plan(&p, c.ax);
            for b in &cp.buildings {
                buildings += 1;
                for q in &b.ring {
                    let (lat, lon) = p.frame.ll(cp.abs(*q));
                    assert!(!w.terrain.strips.in_fence(lat, lon), "{} building {} in a fence", p.id, b.id);
                }
            }
            for s in &cp.streets {
                for q in &s.pts {
                    let (lat, lon) = p.frame.ll(cp.abs(*q));
                    assert!(!w.terrain.strips.in_fence(lat, lon), "{} street {} in a fence", p.id, s.id);
                }
            }
        }
        for r in p.roads.iter().filter(|r| r.kind == super::city::RoadKind::Arterial) {
            for q in &r.pts {
                let (lat, lon) = p.frame.ll(*q);
                assert!(!w.terrain.strips.in_fence(lat, lon), "{} arterial {} in a fence", p.id, r.id);
            }
        }
    }
    assert!(buildings > 10_000, "{buildings}");
}

#[test]
fn kinds_resolve_to_their_light_profiles_and_areas() {
    let (p, _, w) = test_world();
    use crate::mesher::{PROFILE_CIVIC, PROFILE_INDUSTRIAL, PROFILE_LANDMARK, PROFILE_OFFICE, PROFILE_RESIDENTIAL};
    let want = |kinds: &[String], profile: u8| {
        for k in kinds {
            assert_eq!(p.profile_for_kind(k), Some(profile), "kind {k}");
        }
    };
    let s = &w.params.styles;
    want(&s.business.kinds, PROFILE_OFFICE);
    want(&s.microdistrict.kinds, PROFILE_RESIDENTIAL);
    want(&s.houses.kinds, PROFILE_RESIDENTIAL);
    want(&s.industrial.kinds, PROFILE_INDUSTRIAL);
    want(&s.port.kinds, PROFILE_INDUSTRIAL);
    want(&["church".into(), "tower".into()], PROFILE_LANDMARK);
    want(&["school".into(), "kindergarten".into()], PROFILE_CIVIC);
    assert!(!s.business.kinds.iter().any(|k| k == "hotel"));
    use crate::mvt::{GROUND_COMMERCIAL, GROUND_GRASS, GROUND_INDUSTRIAL, GROUND_RESIDENTIAL};
    let classes: Vec<u8> = w.city_kinds.styles.iter().map(|k| k.area.0).collect();
    assert_eq!(classes, [GROUND_RESIDENTIAL, GROUND_COMMERCIAL, GROUND_RESIDENTIAL, GROUND_RESIDENTIAL, GROUND_INDUSTRIAL, GROUND_INDUSTRIAL, GROUND_GRASS]);
}

#[test]
fn city_tiles_are_deterministic_and_each_building_has_one_owner_per_zoom() {
    let w = &test_world().2;
    let k15 = key_at(54.6872, 25.2797, 15);
    let a = format!("{:?}", generate(k15, w));
    assert_eq!(a, format!("{:?}", generate(k15, w)));
    let parent = generate(k15, w);
    assert!(parent.buildings.len() > 50 && parent.ground.roads.len() > 20, "{} buildings", parent.buildings.len());
    // The four z16 children: every building of the parent with its centroid inside is owned by
    // exactly one child.
    let mut owners: BTreeMap<u64, u32> = BTreeMap::new();
    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        let ck = TileKey::new(16, k15.x * 2 + dx, k15.y * 2 + dy);
        for b in generate(ck, w).buildings {
            let c = centroid_px(&b.polygons).unwrap();
            if (0.0..4096.0).contains(&c[0]) && (0.0..4096.0).contains(&c[1]) {
                *owners.entry(b.id).or_default() += 1;
            }
        }
    }
    for b in &parent.buildings {
        let c = centroid_px(&b.polygons).unwrap();
        if (0.0..4096.0).contains(&c[0]) && (0.0..4096.0).contains(&c[1]) {
            assert_eq!(owners.get(&b.id), Some(&1), "building {} owners", b.id);
        }
    }
    // R3 carries only owned buildings (lights).
    let k13 = key_at(54.6872, 25.2797, 13);
    let g13 = generate(k13, w);
    assert!(g13.buildings.iter().all(|b| centroid_px(&b.polygons).is_some_and(|c| (0.0..4096.0).contains(&c[0]) && (0.0..4096.0).contains(&c[1]))));
    let _ = tile_box(k13);
}

#[test]
fn terrain_colour_is_the_district_class_in_a_city() {
    let w = &test_world().2;
    let p = w.city_plan(0);
    let dry = |c: &super::city::Cell| {
        let (lat, lon) = p.frame.ll(p.grid.center(c.ax));
        w.water.water_at(lat, lon) == super::water::WaterHit::Land && !w.terrain.strips.in_fence(lat, lon)
    };
    let c = p.cells.values().find(|c| c.district.is_some() && c.ring == 2 && dry(c)).unwrap();
    let (lat, lon) = p.frame.ll(p.grid.center(c.ax));
    let d = &p.districts[c.district.unwrap() as usize];
    assert_eq!(w.class_at(lat, lon).0, w.city_kinds.styles[d.style_index].area.0);
}

/// A city tile goes through the normal pipeline: R0 / R1 building meshes, street lamps, light
/// points from R1 on (default `lights.min_ring`), ground in every band.
#[test]
fn city_tiles_mesh_and_light_in_every_band() {
    let (p, t, w) = test_world();
    let frame = crate::geo::Frame::new(54.6872, 25.2797);
    for ring in 0..4usize {
        let key = key_at(54.6872, 25.2797, 16 - ring as u8);
        let d = crate::worker::build_seed_tile_on(key, ring, frame, t, w, p).unwrap();
        assert!(!d.ground.indices.is_empty(), "R{ring}: ground");
        assert!(d.building_count > 0, "R{ring}: buildings");
        if ring < 3 {
            assert!(!d.mesh.indices.is_empty(), "R{ring}: building mesh");
            assert!(d.max_height > 3.0, "R{ring}: {}", d.max_height);
        }
        if ring >= p.lights.min_ring as usize {
            assert!(!d.lights.positions.is_empty() || !d.lights.clustered_positions.is_empty(), "R{ring}: light points");
        }
        if ring >= p.lights.street.min_ring as usize {
            assert!(!d.lights.street_positions.is_empty() || !d.lights.clustered_street_positions.is_empty(), "R{ring}: street lamps");
        }
    }
}

#[test]
#[ignore = "measurement: cargo test -p simworld-core --release city_cost -- --ignored --nocapture"]
fn city_cost() {
    let w = &test_world().2;
    let mut ms = Vec::new();
    for p in plans() {
        for c in p.cells.values().filter(|c| c.district.is_some()) {
            let t = std::time::Instant::now();
            let cp = super::city::CellPlan::build(&p, c.ax, w);
            ms.push((t.elapsed().as_secs_f64() * 1000.0, cp.buildings.len(), cp.bytes()));
        }
    }
    ms.sort_by(|a, b| a.0.total_cmp(&b.0));
    let n = ms.len();
    eprintln!(
        "district plans: {n}, ms p50 {:.2} p95 {:.2} max {:.2}; buildings max {}; KB p50 {} max {}",
        ms[n / 2].0,
        ms[n * 95 / 100].0,
        ms[n - 1].0,
        ms.iter().map(|m| m.1).max().unwrap(),
        { let mut b: Vec<usize> = ms.iter().map(|m| m.2).collect(); b.sort(); b[n / 2] / 1024 },
        ms.iter().map(|m| m.2).max().unwrap() / 1024
    );
    for z in [16u8, 15, 14, 13] {
        let k = key_at(54.6872, 25.2797, z);
        let t = std::time::Instant::now();
        let g = generate(k, w);
        eprintln!("tile z{z}: {:.2} ms, {} buildings, {} roads, {} areas", t.elapsed().as_secs_f64() * 1000.0, g.buildings.len(), g.ground.roads.len(), g.ground.areas.len());
    }
}
