use super::road::strip;
use super::*;
use crate::geo::{tile_for, Frame};
use crate::mvt::{GroundArea, RoadLine, GROUND_GRASS, GROUND_WATER, ROAD_PRIMARY, ROAD_RESIDENTIAL, ROAD_SECONDARY, ROAD_TERTIARY};

fn dp() -> Profile {
    Profile::default()
}

const VILNIUS: (f64, f64) = (54.6872, 25.2797);

fn proj() -> TileProjector {
    let key = tile_for(VILNIUS.0, VILNIUS.1, 16);
    let (lat, lon) = key.center_lat_lon();
    TileProjector::new(key, Frame::new(lat, lon), 4096.0)
}

fn sq(x0: f64, y0: f64, s: f64) -> Vec<[f64; 2]> {
    vec![[x0, y0], [x0 + s, y0], [x0 + s, y0 + s], [x0, y0 + s]]
}

fn area_m2(m: &MeshData) -> f64 {
    m.indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| {
            let (a, b, c) = (m.positions[t[0] as usize], m.positions[t[1] as usize], m.positions[t[2] as usize]);
            (((b[0] - a[0]) * (c[2] - a[2]) - (c[0] - a[0]) * (b[2] - a[2])) as f64).abs() * 0.5
        })
        .sum()
}

fn all_up(m: &MeshData) -> bool {
    m.indices.as_chunks::<3>().0.iter().all(|t| {
        let (a, b, c) = (m.positions[t[0] as usize], m.positions[t[1] as usize], m.positions[t[2] as usize]);
        (c[2] - a[2]) * (b[0] - a[0]) - (c[0] - a[0]) * (b[2] - a[2]) > 0.0
    }) && m.normals.iter().all(|n| *n == UP)
}

fn water_with_island() -> GroundFeatures {
    // Exterior clockwise in screen space, hole counter-clockwise (MVT convention).
    let mut hole = sq(1500.0, 1500.0, 1000.0);
    hole.reverse();
    GroundFeatures {
        areas: vec![GroundArea { class: GROUND_WATER, level: 7, id: 1, rings: vec![sq(1000.0, 1000.0, 2000.0), hole], tint: 0, sub: 0, cut: false }],
        roads: vec![],
        ..Default::default()
    }
}

#[test]
fn triangulation_respects_holes() {
    let p = proj();
    let mpp = p.m_per_px();
    let m = mesh_ground(&water_with_island(), &p, 0, &dp());
    assert!(!m.is_empty() && all_up(&m));
    let expect = (2000.0f64.powi(2) - 1000.0f64.powi(2)) * mpp * mpp;
    assert!((area_m2(&m) - expect).abs() / expect < 0.01, "{} vs {expect}", area_m2(&m));
    // No triangle covers the island center.
    let c = p.project([2000.0, 2000.0]);
    let hit = m.indices.as_chunks::<3>().0.iter().any(|t| {
        let v: Vec<[f32; 2]> = t.iter().map(|&i| [m.positions[i as usize][0], -m.positions[i as usize][2]]).collect();
        let s = |a: [f32; 2], b: [f32; 2]| (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0]);
        let (d1, d2, d3) = (s(v[0], v[1]), s(v[1], v[2]), s(v[2], v[0]));
        (d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0) || (d1 <= 0.0 && d2 <= 0.0 && d3 <= 0.0)
    });
    assert!(!hit);
    // Water color + level lift.
    assert!(m.colors.iter().all(|c| *c == [2.0 / 16.0, 0.0, 0.0, 1.0]));
    assert!(m.positions.iter().all(|v| (v[1] - 7.0 * dp().ground_params(0).unwrap().step_m).abs() < 1e-6));
    // R3: nothing.
    assert!(mesh_ground(&water_with_island(), &p, 3, &dp()).is_empty());
}

#[test]
fn areas_are_clipped_to_the_tile() {
    let p = proj();
    let g = GroundFeatures {
        areas: vec![GroundArea { class: GROUND_GRASS, level: 5, id: 2, rings: vec![sq(-1000.0, -1000.0, 6000.0)], tint: 0, sub: 0, cut: false }],
        roads: vec![],
        ..Default::default()
    };
    let m = mesh_ground(&g, &p, 0, &dp());
    let w = p.m_per_px() * 4096.0;
    assert!((area_m2(&m) - w * w).abs() / (w * w) < 0.01);
    let h = w / 2.0 + 0.5;
    assert!(m.positions.iter().all(|v| (v[0] as f64).abs() < h && (v[2] as f64).abs() < h));
}

fn road(class: u8, link: bool, lit: bool, line: Vec<[f64; 2]>) -> RoadLine {
    RoadLine { class, link, minor: false, lit, id: 7, lines: vec![line] }
}

#[test]
fn strip_widths_by_class() {
    let p = proj();
    let mpp = p.m_per_px();
    for (class, link, w) in [
        (ROAD_MOTORWAY, false, 14.0),
        (ROAD_PRIMARY, false, 10.0),
        (ROAD_SECONDARY, false, 8.0),
        (ROAD_TERTIARY, false, 6.0),
        (ROAD_RESIDENTIAL, false, 6.0),
        (ROAD_OTHER, false, 3.0),
        (ROAD_PRIMARY, true, 6.0),
        (ROAD_RESIDENTIAL, true, 3.6),
    ] {
        assert_eq!(dp().road_width_m(class, link), w);
        // Straight east-west road through the whole tile (clipped at both edges).
        let g = GroundFeatures { areas: vec![], roads: vec![road(class, link, false, vec![[-200.0, 2048.0], [4300.0, 2048.0]])], ..Default::default() };
        let m = mesh_ground(&g, &p, 0, &dp());
        assert!(all_up(&m));
        let zs: Vec<f32> = m.positions.iter().map(|v| v[2]).collect();
        let span = zs.iter().cloned().fold(f32::MIN, f32::max) - zs.iter().cloned().fold(f32::MAX, f32::min);
        assert!((span as f64 - w).abs() < 0.05, "class {class}: {span}");
        let len = 4096.0 * mpp;
        assert!((area_m2(&m) - len * w).abs() / (len * w) < 0.01);
    }
}

#[test]
fn joins_miter_and_bevel_cover_the_corner() {
    let mut m = MeshData::default();
    // 90° turn: miter.
    strip(&mut m, &[[0.0, 0.0], [100.0, 0.0], [100.0, 100.0]], &[0.0; 3], 3.0, road_color(ROAD_TERTIARY, true));
    assert_eq!(m.vertex_count(), 6);
    assert!((area_m2(&m) - 2.0 * 100.0 * 6.0).abs() < 1.0, "{}", area_m2(&m));
    // Hairpin (170°): bevel join with a fill triangle, no spike.
    let mut b = MeshData::default();
    let back = [100.0 - 100.0 * 170f64.to_radians().cos().abs(), 100.0 * 170f64.to_radians().sin()];
    strip(&mut b, &[[0.0, 0.0], [100.0, 0.0], back], &[0.0; 3], 3.0, road_color(ROAD_TERTIARY, false));
    assert_eq!(b.triangle_count(), 5);
    assert!(b.positions.iter().all(|v| v[0] < 104.0));
    assert!(all_up(&m) && all_up(&b));
}

#[test]
fn colour_encoding_matches_contract() {
    assert_eq!(area_color(GROUND_GRASS), [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(area_color(GROUND_WATER), [0.125, 0.0, 0.0, 1.0]);
    assert_eq!(area_color(crate::mvt::GROUND_OTHER), [6.0 / 16.0, 0.0, 0.0, 1.0]);
    assert_eq!(road_color(ROAD_MOTORWAY, true), [0.5, 1.0, 0.125, 1.0]);
    assert_eq!(road_color(ROAD_OTHER, false), [0.5, 0.0, 0.75, 1.0]);
    // Exact multiples (Godot decodes round(r * 16), round(b * 8)).
    for c in 0..=8u8 {
        assert_eq!(area_color(c)[0] * 16.0, c as f32);
    }
    for rc in 1..=6u8 {
        assert_eq!(road_color(rc, false)[2] * 8.0, rc as f32);
    }
    // Classification tables.
    let d = dp();
    let (ground_area_class, road_class, is_lit) =
        (|k: &str| d.ground_area_class(k), |h: &str| d.road_class(h), |v: &str| d.is_lit(v));
    assert_eq!(ground_area_class("park"), (GROUND_GRASS, 5));
    assert_eq!(ground_area_class("riverbank"), (GROUND_WATER, 7));
    assert_eq!(ground_area_class("wood").0, crate::mvt::GROUND_FOREST);
    assert_eq!(ground_area_class("retail").0, crate::mvt::GROUND_COMMERCIAL);
    assert_eq!(ground_area_class("education").0, crate::mvt::GROUND_OTHER);
    assert_eq!(road_class("motorway_link"), (ROAD_MOTORWAY, true, false));
    assert_eq!(road_class("trunk"), (ROAD_PRIMARY, false, false));
    assert_eq!(road_class("living_street"), (ROAD_RESIDENTIAL, false, false));
    assert_eq!(road_class("service"), (ROAD_OTHER, false, false));
    assert_eq!(road_class("footway"), (ROAD_OTHER, false, true));
    assert!(is_lit("yes") && is_lit("24/7") && !is_lit("no") && !is_lit(""));
    // Roads stack above all areas, major on top.
    assert!(road_level(ROAD_OTHER) > 7 && road_level(ROAD_MOTORWAY) > road_level(ROAD_PRIMARY));
}

#[test]
fn coarser_bands_drop_minor_roads_and_small_areas() {
    let p = proj();
    let mut minor = road(ROAD_OTHER, false, false, vec![[100.0, 100.0], [3000.0, 100.0]]);
    minor.minor = true;
    let g = GroundFeatures {
        areas: vec![GroundArea { class: GROUND_GRASS, level: 5, id: 3, rings: vec![sq(10.0, 10.0, 50.0)], tint: 0, sub: 0, cut: false }],
        roads: vec![minor, road(ROAD_OTHER, false, false, vec![[100.0, 900.0], [3000.0, 900.0]])],
        ..Default::default()
    };
    let t = |r| mesh_ground(&g, &p, r, &dp()).triangle_count();
    assert!(t(0) > t(1) && t(1) > t(2));
    assert_eq!(t(2), 0);
}

/// Water on a slope drapes like every other area: no vertex under the terrain surface, none
/// above it by more than the drape margin and the level step (the default `water_snap_m` is 0).
#[test]
fn water_drapes_on_a_slope() {
    use crate::geo::Frame;
    use crate::profile::TerrainProfile;
    use crate::terrain::Terrain;
    let tp = TerrainProfile { enabled: true, flat_radius_m: 0.0, flat_blend_m: 0.0, ..TerrainProfile::default() };
    assert_eq!(tp.water_snap_m, 0.0);
    let (lat0, lon0) = (VILNIUS.0 + 0.05, VILNIUS.1 + 0.06);
    let key = tile_for(lat0, lon0, 16);
    let (lat, lon) = key.center_lat_lon();
    let frame = Frame::new(lat, lon);
    let terrain = Terrain::new(&tp, lat, lon);
    let p = TileProjector::new(key, frame, 4096.0).with_terrain(terrain.clone());
    let g = GroundFeatures {
        areas: vec![GroundArea { class: GROUND_WATER, level: 7, id: 1, rings: vec![sq(500.0, 500.0, 3000.0)], tint: 0, sub: 0, cut: false }],
        roads: vec![],
        ..Default::default()
    };
    let prof = dp();
    let ring = 0;
    let m = mesh_ground(&g, &p, ring, &prof);
    assert!(!m.is_empty());
    let lift = terrain.drape_margin_m(ring) as f64;
    let step = prof.ground_params(ring).unwrap().step_m as f64;
    let (mut lo, mut hi, mut slope) = (f64::MAX, f64::MIN, 0.0f64);
    for v in &m.positions {
        let (la, lo_) = frame.lat_lon(v[0] as f64, -v[2] as f64);
        let h = terrain.height(la, lo_);
        let above = v[1] as f64 - h;
        lo = lo.min(above);
        hi = hi.max(above);
        slope = slope.max(h);
    }
    assert!(lo >= lift - 0.01, "a water vertex is {lo} m above the terrain, margin {lift}");
    assert!(hi <= lift + 7.0 * step + 0.01, "a water vertex is {hi} m above the terrain");
    let (a, b) = (terrain.height(lat, lon), slope);
    assert!(b > a + 1.0 || a > 1.0, "the tile has no relief to test against");
}

/// Drape cost on the default relief (amplitude 450, 8 km from the flat zone): a forest
/// polygon over (almost) a whole tile stays within a per-band triangle budget, so the
/// drape tolerance really follows the band (a fixed 0.1 m made R3 tiles ~25 000 triangles).
#[test]
fn drape_triangles_per_band_on_the_default_relief() {
    use crate::geo::Frame;
    use crate::profile::TerrainProfile;
    use crate::terrain::Terrain;
    let prof = dp();
    let tp = TerrainProfile { enabled: true, ..TerrainProfile::default() };
    let origin = (VILNIUS.0, VILNIUS.1);
    let terrain = Terrain::new(&tp, origin.0, origin.1);
    // Same spot (~8 km north-east of the origin) at the zoom of each band.
    let (lat0, lon0) = (origin.0 + 0.05, origin.1 + 0.1);
    let mut counts = Vec::new();
    for ring in 0..4usize {
        let key = tile_for(lat0, lon0, 16 - ring as u8);
        let p = TileProjector::new(key, Frame::new(origin.0, origin.1), 4096.0).with_terrain(terrain.clone());
        let g = GroundFeatures {
            areas: vec![GroundArea { class: crate::mvt::GROUND_FOREST, level: 6, id: 1, rings: vec![sq(100.0, 100.0, 3896.0)], tint: 0, sub: 0, cut: false }],
            roads: vec![],
            ..Default::default()
        };
        let m = mesh_ground(&g, &p, ring, &prof);
        counts.push(m.triangle_count());
    }
    eprintln!("drape triangles per band R0..R3: {counts:?}");
    // Measured 59 / 82 / 104 / 162; a fixed 0.1 m tolerance gave 155 / 605 / 2539 / 11175.
    const CAP: [usize; 4] = [120, 250, 400, 700];
    for (r, (&c, &cap)) in counts.iter().zip(CAP.iter()).enumerate() {
        assert!(c <= cap, "R{r}: {c} triangles");
    }
}

#[test]
fn deterministic() {
    let p = proj();
    let mut g = water_with_island();
    g.roads.push(road(ROAD_SECONDARY, false, true, vec![[0.0, 0.0], [1200.0, 3000.0], [4096.0, 3500.0]]));
    assert_eq!(mesh_ground(&g, &p, 0, &dp()), mesh_ground(&g, &p, 0, &dp()));
    assert_eq!(mesh_ground(&g, &p, 1, &dp()), mesh_ground(&g, &p, 1, &dp()));
}

/// Holes reaching the clip rect (a pond in landuse, an inner ring cut by a tile or chunk
/// edge): area = outer ∩ rect − holes ∩ rect, and the four z16 chunks of a z15 tile add up.
#[test]
fn holes_crossing_the_clip_rect_stay_open() {
    use crate::polyclip::fixtures;
    let key = tile_for(VILNIUS.0, VILNIUS.1, 15);
    let (lat, lon) = key.center_lat_lon();
    let parent = TileProjector::new(key, Frame::new(lat, lon), 4096.0);
    for (name, outer, holes) in fixtures::cases() {
        let g = GroundFeatures {
            areas: vec![GroundArea { class: GROUND_GRASS, level: 0, id: 7, rings: fixtures::rings(outer, &holes), tint: 0, sub: 0, cut: false }],
            roads: vec![],
            ..Default::default()
        };
        let whole = mesh_ground(&g, &parent, 1, &dp());
        let a = area_m2(&whole);
        let expect = fixtures::expected_m2(&parent, [0.0, 0.0, 4096.0, 4096.0], outer, &holes);
        assert!((a - expect).abs() / expect < 1e-6, "{name}: tile {a} vs {expect}");
        assert!(all_up(&whole), "{name}");
        let mut sum = 0.0;
        for q in 0..4u32 {
            let child = crate::geo::TileKey::new(key.z + 1, key.x * 2 + (q & 1), key.y * 2 + (q >> 1));
            let p = parent.cut(child).unwrap();
            let r = p.rect();
            let m = mesh_ground(&g, &p, 0, &dp());
            let am = area_m2(&m);
            let e = fixtures::expected_m2(&p, [r.x0, r.y0, r.x1, r.y1], outer, &holes);
            assert!((am - e).abs() <= 1e-6 * e.max(1.0), "{name} child {q}: {am} vs {e}");
            assert!(all_up(&m), "{name} child {q}");
            assert_eq!(m, mesh_ground(&g, &p, 0, &dp()), "{name}: deterministic");
            sum += am;
        }
        // Exact in tile pixels (`expected_m2` checks it); in ENU the tile lacks the chunk seam
        // vertices and a straight pixel edge bends slightly (~2e-6 of the area at z15): 1e-5.
        assert!((sum - a).abs() / a < 1e-5, "{name}: chunks {sum} vs tile {a}");
    }
}
