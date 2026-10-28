//! Offline fake city used when `tile_url` is empty. Goes through the same path as real data
//! (pixel-space `RawBuilding` -> `TileProjector` -> `Building`).
//!
//! The city lives on a global Web Mercator lot grid (z19 cells, ~44 m at Vilnius), so the same
//! lot yields the same building at every zoom and on every run. Lots are always fully inside
//! one tile at z13..z16, so every synthetic building is owned by exactly one tile per zoom.
//! Coarser zooms are thinned (z14 keeps 1/2, z13 1/4) like a real low-zoom profile would.
//!
//! Three landmark lots (`special_lot`) inside the z16 tile that holds the frame origin carry a
//! castle (`yes` + `historic=castle`), a slender tower (`yes` + `man_made=tower`, tagged height)
//! and a ruin (`house` + `historic=ruins`) at every zoom (never thinned), so the light-profile
//! rules (landmark / dark, docs/spec-night-windows.md) are exercised by the synthetic city.
//!
//! Ground (`generate_ground`): every district (16 x 16 lots = one z15 tile) is one landuse area
//! (park, industrial, commercial or residential like its buildings; some districts are a lake
//! with a built-up island, i.e. a polygon with a hole), and streets run along every 4th lot
//! line (secondary on district edges, residential otherwise, lit by a hash of the line).

use crate::geo::{lat_to_tile_y, lon_to_tile_x, tile_for, tile_width_m, Frame, TileKey};
use crate::mvt::{
    splitmix64, Building, BuildingTags, GroundArea, GroundFeatures, Landuse, OsmType, RawBuilding, RoadLine, TileProjector, GROUND_COMMERCIAL,
    GROUND_GRASS, GROUND_INDUSTRIAL, GROUND_RESIDENTIAL, GROUND_WATER, ROAD_RESIDENTIAL, ROAD_SECONDARY,
};

const LOT_ZOOM: u8 = 19;
const EXTENT: f64 = 4096.0;

const DISTRICT_LOTS: u64 = 16;
const STREET_EVERY: u64 = 4;

/// The landmark lots of the synthetic city.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Special {
    Castle,
    Tower,
    Ruin,
}

/// First lot (z19) of the z16 tile holding the frame origin.
fn special_base(frame: Frame) -> (u64, u64) {
    let t = tile_for(frame.lat0, frame.lon0, 16);
    let per = 1u64 << (LOT_ZOOM - 16);
    (t.x as u64 * per, t.y as u64 * per)
}

/// Lots (z19, 8 x 8 per z16 tile) of the z16 tile holding the frame origin (`base`): castle at
/// local (2, 2), tower at (5, 2), ruin at (2, 5).
fn special_lot(base: (u64, u64), lx: u64, ly: u64) -> Option<Special> {
    let (bx, by) = base;
    match (lx.checked_sub(bx)?, ly.checked_sub(by)?) {
        (2, 2) => Some(Special::Castle),
        (5, 2) => Some(Special::Tower),
        (2, 5) => Some(Special::Ruin),
        _ => None,
    }
}

fn unit(h: u64) -> f64 {
    (h >> 11) as f64 / (1u64 << 53) as f64
}

fn district_hash(dx: u64, dy: u64) -> u64 {
    splitmix64((dx << 32) ^ dy ^ 0xD157)
}

fn is_park(d: u64) -> bool {
    d.is_multiple_of(10)
}

fn is_lake(d: u64) -> bool {
    !is_park(d) && d % 23 == 5
}

/// Island lots inside a lake district (district-local lots 5..11).
fn on_island(lx: u64, ly: u64) -> bool {
    let (i, j) = (lx % DISTRICT_LOTS, ly % DISTRICT_LOTS);
    (5..11).contains(&i) && (5..11).contains(&j)
}

/// Synthetic ground features (pixel space) for a tile; deterministic and seamless across
/// zooms (global lot grid, like the buildings).
pub fn generate_ground(key: TileKey, frame: Frame, profile: &crate::profile::Profile) -> GroundFeatures {
    let mut g = GroundFeatures::default();
    if key.z > LOT_ZOOM {
        return g;
    }
    let per = 1u64 << (LOT_ZOOM - key.z);
    let ppl = EXTENT / per as f64;
    let (lx0, ly0) = (key.x as u64 * per, key.y as u64 * per);
    let px = |l: u64, l0: u64| (l as f64 - l0 as f64) * ppl;
    let (clat, _) = key.center_lat_lon();
    let lot_m = tile_width_m(clat, LOT_ZOOM);
    let ox = lon_to_tile_x(frame.lon0, LOT_ZOOM);
    let oy = lat_to_tile_y(frame.lat0, LOT_ZOOM);

    // Districts overlapping the tile (a z16 tile is a quarter district).
    let (d0x, d0y) = (lx0 / DISTRICT_LOTS, ly0 / DISTRICT_LOTS);
    let nd = per.div_ceil(DISTRICT_LOTS);
    for dy in d0y..d0y + nd {
        for dx in d0x..d0x + nd {
            let d = district_hash(dx, dy);
            let (x0, y0) = (dx * DISTRICT_LOTS, dy * DISTRICT_LOTS);
            let (x1, y1) = (x0 + DISTRICT_LOTS, y0 + DISTRICT_LOTS);
            // Exterior clockwise in screen space (MVT convention), hole counter-clockwise.
            let rect = |a: u64, b: u64, c: u64, e: u64| vec![[px(a, lx0), px(b, ly0)], [px(c, lx0), px(b, ly0)], [px(c, lx0), px(e, ly0)], [px(a, lx0), px(e, ly0)]];
            let dist_m = crate::math::hypot(x0 as f64 + 8.0 - ox, y0 as f64 + 8.0 - oy) * lot_m;
            let class = if is_park(d) {
                GROUND_GRASS
            } else if is_lake(d) {
                GROUND_WATER
            } else if d % 7 == 3 {
                GROUND_INDUSTRIAL
            } else if dist_m < 1500.0 {
                GROUND_COMMERCIAL
            } else {
                GROUND_RESIDENTIAL
            };
            let level = profile.ground_area_class(match class {
                GROUND_GRASS => "park",
                GROUND_WATER => "water",
                GROUND_INDUSTRIAL => "industrial",
                GROUND_COMMERCIAL => "commercial",
                _ => "residential",
            })
            .1;
            let mut rings = vec![rect(x0, y0, x1, y1)];
            if class == GROUND_WATER {
                let mut hole = rect(x0 + 5, y0 + 5, x0 + 11, y0 + 11);
                hole.reverse();
                rings.push(hole);
            }
            g.areas.push(GroundArea { class, level, id: d & 0xFFFF_FFFF, rings, tint: 0, sub: 0, cut: false });
        }
    }
    // Streets on every 4th lot line inside [l0, l0 + per) (the line on the far edge belongs to
    // the neighbour), running across the whole tile.
    let first = |l0: u64| l0.div_ceil(STREET_EVERY) * STREET_EVERY;
    for axis in 0..2 {
        let l0 = if axis == 0 { lx0 } else { ly0 };
        let mut l = first(l0);
        while l < l0 + per {
            let h = splitmix64(l ^ ((axis as u64) << 40) ^ 0x57EE7);
            let class = if l.is_multiple_of(DISTRICT_LOTS) { ROAD_SECONDARY } else { ROAD_RESIDENTIAL };
            let c = px(l, l0);
            let line = if axis == 0 { vec![[c, 0.0], [c, EXTENT]] } else { vec![[0.0, c], [EXTENT, c]] };
            g.roads.push(RoadLine { class, link: false, minor: false, lit: !h.is_multiple_of(3), id: h & 0xFFFF_FFFF, lines: vec![line] });
            l += STREET_EVERY;
        }
    }
    g.sort();
    g
}

pub fn generate(key: TileKey, frame: Frame) -> Vec<Building> {
    generate_on(key, frame, &crate::terrain::Terrain::flat())
}

/// [`generate`] on `terrain` (building bases stand on it).
pub fn generate_on(key: TileKey, frame: Frame, terrain: &crate::terrain::Terrain) -> Vec<Building> {
    generate_lots(key, frame, terrain, true)
}

/// Every lot of `key` (`thin` = the per-zoom thinning of the per-tile path: z14 keeps 1/2, z13
/// 1/4; the data levels keep every building).
fn generate_lots(key: TileKey, frame: Frame, terrain: &crate::terrain::Terrain, thin: bool) -> Vec<Building> {
    let proj = TileProjector::new(key, frame, EXTENT).with_terrain(terrain.clone());
    generate_raw(key, frame, thin).into_iter().filter_map(|r| proj.finish(r)).collect()
}

/// The lots of [`generate_lots`] before projection (pixel space, no terrain sampled).
fn generate_raw(key: TileKey, frame: Frame, thin: bool) -> Vec<RawBuilding> {
    if key.z > LOT_ZOOM {
        return Vec::new();
    }
    let per = 1u64 << (LOT_ZOOM - key.z);
    let (clat, _) = key.center_lat_lon();
    let lot_m = tile_width_m(clat, LOT_ZOOM);
    let px_per_m = EXTENT / tile_width_m(clat, key.z);
    let px_per_lot = EXTENT / per as f64;
    let ox = lon_to_tile_x(frame.lon0, LOT_ZOOM);
    let oy = lat_to_tile_y(frame.lat0, LOT_ZOOM);
    let base = special_base(frame);

    let mut out = Vec::new();
    for j in 0..per {
        for i in 0..per {
            let lx = key.x as u64 * per + i;
            let ly = key.y as u64 * per + j;
            let h = splitmix64((lx << 32) ^ ly ^ 0x5157_4E54);
            let district = district_hash(lx / DISTRICT_LOTS, ly / DISTRICT_LOTS);
            if let Some(sp) = special_lot(base, lx, ly) {
                // Landmark lots: always built, at every zoom.
                let (cx, cy) = ((i as f64 + 0.5) * px_per_lot, (j as f64 + 0.5) * px_per_lot);
                let (hw, hd, kind, height, levels, tags) = match sp {
                    Special::Castle => (
                        17.0,
                        14.0,
                        "yes",
                        Some(18.0),
                        None,
                        BuildingTags { historic: Some("castle".into()), ..Default::default() },
                    ),
                    Special::Tower => (
                        3.5,
                        3.5,
                        "yes",
                        Some(42.0),
                        None,
                        BuildingTags { man_made: Some("tower".into()), ..Default::default() },
                    ),
                    Special::Ruin => (
                        7.0,
                        4.5,
                        "house",
                        None,
                        Some(1.0),
                        BuildingTags { historic: Some("ruins".into()), ..Default::default() },
                    ),
                };
                let (w, d) = (hw * px_per_m, hd * px_per_m);
                let raw = RawBuilding {
                    osm_type: OsmType::Synthetic,
                    id: h & 0x0000_FFFF_FFFF_FFFF,
                    kind: kind.to_string(),
                    height,
                    levels,
                    min_height: 0.0,
                    landuse: Some(Landuse::Commercial),
                    tags,
                    polygons: vec![vec![vec![[cx - w, cy - d], [cx + w, cy - d], [cx + w, cy + d], [cx - w, cy + d]]]],
                };
                out.push(raw);
                continue;
            }
            if is_park(district) || (is_lake(district) && !on_island(lx, ly)) {
                continue; // park, or open water around a lake island
            }
            let dist_m = crate::math::hypot(lx as f64 + 0.5 - ox, ly as f64 + 0.5 - oy) * lot_m;
            let p_occ = 0.15 + 0.7 * crate::math::exp(-dist_m / 6000.0);
            if unit(h) > p_occ {
                continue;
            }
            let h2 = splitmix64(h);
            let keep = !thin || match key.z {
                z if z >= 15 => true,
                14 => h2.is_multiple_of(2),
                _ => h2.is_multiple_of(4),
            };
            if !keep {
                continue;
            }
            let h3 = splitmix64(h2);
            let industrial = district % 7 == 3;
            let central = dist_m < 1500.0;
            let r = unit(h3);
            let (kind, height, levels): (&str, Option<f32>, Option<f32>) = if industrial {
                ("industrial", Some(6.0 + (r * 6.0) as f32), Some(1.0))
            } else if central {
                match h3 % 10 {
                    0..=3 => ("office", Some(20.0 + (r * 60.0) as f32), None),
                    4..=5 => ("retail", None, Some(2.0 + (r * 2.0).floor() as f32)),
                    6 => ("church", Some(25.0 + (r * 20.0) as f32), None),
                    _ => ("apartments", None, Some(4.0 + (r * 6.0).floor() as f32)),
                }
            } else {
                match h3 % 10 {
                    0..=4 => ("house", None, Some(1.0 + (r * 2.0).floor() as f32)),
                    5..=8 => ("apartments", None, Some(3.0 + (r * 6.0).floor() as f32)),
                    _ => ("yes", None, None),
                }
            };
            let h4 = splitmix64(h3);
            let fill = if industrial { 0.85 } else { 0.4 + 0.4 * unit(h4) };
            let w = lot_m * fill * px_per_m * 0.5;
            let d = lot_m * (0.4 + 0.4 * unit(splitmix64(h4))) * px_per_m * 0.5;
            let cx = (i as f64 + 0.5) * px_per_lot;
            let cy = (j as f64 + 0.5) * px_per_lot;
            let ring = vec![[cx - w, cy - d], [cx + w, cy - d], [cx + w, cy + d], [cx - w, cy + d]];
            let raw = RawBuilding {
                osm_type: OsmType::Synthetic,
                id: h & 0x0000_FFFF_FFFF_FFFF,
                kind: kind.to_string(),
                height,
                levels,
                min_height: 0.0,
                landuse: Some(if industrial {
                    Landuse::Industrial
                } else if central {
                    Landuse::Commercial
                } else {
                    Landuse::Residential
                }),
                tags: BuildingTags::default(),
                polygons: vec![vec![ring]],
            };
            out.push(raw);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Data levels (Tiles and LOD v2, step T2c): the synthetic city as a two-level source, built the
// way `simworld-cli bake` builds an archive, so the streaming path is the same offline.

/// The bake's landmark rule (`landmark_m`): a tagged roof (height, else levels x 3 m) of at
/// least this many metres.
pub const LANDMARK_M: f32 = 60.0;
/// The bake's height quantisation of blocks and light points (`height_step_m`).
const HEIGHT_STEP_M: f32 = 5.0;
/// The bake's smallest block (`block_min_area_m2`).
const BLOCK_MIN_AREA_M2: f64 = 150.0;

fn tagged_roof(b: &Building) -> Option<f32> {
    b.height.or(b.levels.map(|l| l * 3.0))
}

fn round_step(v: f32, step: f32) -> f32 {
    ((v / step).round() * step).max(step)
}

/// Landmark (bake rule, [`LANDMARK_M`]).
pub fn is_landmark(b: &Building) -> bool {
    tagged_roof(b).is_some_and(|h| h >= LANDMARK_M)
}

/// The detail data tile `key` (z14): every lot's building except the landmarks, all ground.
pub fn detail_level(key: TileKey, frame: Frame, terrain: &crate::terrain::Terrain, profile: &crate::profile::Profile) -> (Vec<Building>, GroundFeatures, TileProjector) {
    let mut b = generate_lots(key, frame, terrain, false);
    b.retain(|b| !is_landmark(b));
    (b, generate_ground(key, frame, profile), TileProjector::new(key, frame, EXTENT).with_terrain(terrain.clone()))
}

/// The overview data tile of the synthetic city (see [`overview_level`]).
#[derive(Debug)]
pub struct OverviewLevel {
    pub blocks: Vec<Building>,
    pub landmarks: Vec<Building>,
    pub lights: Vec<Building>,
    pub light_landmark: Vec<bool>,
    pub ground: GroundFeatures,
    pub proj: TileProjector,
}

/// The overview data tile `key` (z11): one block per street block (4 x 4 lots between the
/// streets) from its non-landmark buildings (bounding box, area-weighted mean height rounded to
/// 5 m, kind by area, smallest member id; blocks under 150 m² dropped; base left at 0 for the
/// chunks to resolve, `data::LazyBases`), the landmarks with full
/// geometry, one light point per building (landmarks too; tagged roofs rounded to 5 m, area for
/// tagged heights), districts and the secondary streets.
pub fn overview_level(key: TileKey, frame: Frame, terrain: &crate::terrain::Terrain, profile: &crate::profile::Profile) -> OverviewLevel {
    use crate::mvt::{centroid_px, light_point, ring_area2, LightAttrs, OsmType};
    use std::collections::BTreeMap;
    let proj = TileProjector::new(key, frame, EXTENT).with_terrain(terrain.clone());
    let per = 1u64 << (LOT_ZOOM.saturating_sub(key.z));
    let px_per_lot = EXTENT / per as f64;
    let mpp = proj.m_per_px();
    // Height of a lot's building as the runtime resolves it (block weights), without projecting.
    let resolved = |r: &RawBuilding| {
        let b = Building {
            osm_type: r.osm_type,
            id: r.id,
            kind: r.kind.clone(),
            height: r.height,
            levels: r.levels,
            min_height: r.min_height,
            landuse: r.landuse,
            tags: r.tags.clone(),
            polygons: Vec::new(),
            clipped: true,
            footprint_m2: 0.0,
            centroid_px: [0.0; 2],
            centroid: [0.0; 2],
            base_m: 0.0,
            owned: true,
        };
        crate::mesher::resolve(&b, profile).height
    };
    struct Member {
        bb: [f64; 4],
        area: f64,
        h: f32,
        id: u64,
        kind: String,
        landuse: Option<Landuse>,
    }
    let mut landmarks = Vec::new();
    let mut lights: Vec<(Building, bool)> = Vec::new();
    let mut groups: BTreeMap<(u64, u64), Vec<Member>> = BTreeMap::new();
    for r in generate_raw(key, frame, false) {
        let Some(c) = centroid_px(&r.polygons) else { continue };
        let ring = &r.polygons[0][0];
        let area = ring_area2(ring).abs() * 0.5 * mpp * mpp;
        let roof = r.height.or(r.levels.map(|l| l * 3.0));
        let lmk = roof.is_some_and(|h| h >= LANDMARK_M);
        let roof = roof.map(|h| round_step(h, HEIGHT_STEP_M));
        let attrs = LightAttrs {
            osm_type: Some(r.osm_type),
            id: r.id,
            kind: r.kind.clone(),
            height: roof,
            levels: None,
            area_m2: roof.map(|_| area.round() as f32),
            landuse: r.landuse,
            tags: r.tags.clone(),
        };
        lights.push((light_point(&proj, c, attrs), lmk));
        if lmk {
            landmarks.extend(proj.finish(r));
            continue;
        }
        let mut bb = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for p in ring {
            bb = [bb[0].min(p[0]), bb[1].min(p[1]), bb[2].max(p[0]), bb[3].max(p[1])];
        }
        let (lx, ly) = ((c[0] / px_per_lot) as u64, (c[1] / px_per_lot) as u64);
        let h = resolved(&r);
        groups.entry((lx / STREET_EVERY, ly / STREET_EVERY)).or_default().push(Member { bb, area, h, id: r.id, kind: r.kind, landuse: r.landuse });
    }
    let mut blocks = Vec::new();
    let flat = TileProjector::new(key, frame, EXTENT);
    for members in groups.values() {
        let mut bb = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for m in members {
            bb = [bb[0].min(m.bb[0]), bb[1].min(m.bb[1]), bb[2].max(m.bb[2]), bb[3].max(m.bb[3])];
        }
        if (bb[2] - bb[0]) * (bb[3] - bb[1]) * mpp * mpp < BLOCK_MIN_AREA_M2 {
            continue;
        }
        let (mut sw, mut sh) = (0.0f64, 0.0f64);
        let mut kinds: BTreeMap<&str, f64> = BTreeMap::new();
        for m in members {
            sw += m.area;
            sh += m.area * m.h as f64;
            *kinds.entry(m.kind.as_str()).or_default() += m.area;
        }
        let h = if sw > 0.0 { (sh / sw) as f32 } else { 0.0 };
        let kind = kinds.iter().max_by(|a, b| a.1.total_cmp(b.1).then(b.0.cmp(a.0))).map_or("yes", |k| k.0).to_string();
        let raw = RawBuilding {
            osm_type: OsmType::Unknown,
            id: members.iter().map(|m| m.id).min().unwrap_or(0),
            kind,
            height: Some(round_step(h, HEIGHT_STEP_M)),
            levels: None,
            min_height: 0.0,
            landuse: members[0].landuse,
            tags: BuildingTags::default(),
            polygons: vec![vec![vec![[bb[0], bb[1]], [bb[2], bb[1]], [bb[2], bb[3]], [bb[0], bb[3]]]]],
        };
        // Base left open: resolved by the chunks that mesh the block (`data::LazyBases`).
        blocks.extend(flat.finish(raw));
    }
    let order = |a: &Building, b: &Building| {
        (a.osm_type.tag(), a.id)
            .cmp(&(b.osm_type.tag(), b.id))
            .then(a.centroid_px[0].total_cmp(&b.centroid_px[0]))
            .then(a.centroid_px[1].total_cmp(&b.centroid_px[1]))
    };
    blocks.sort_by(order);
    landmarks.sort_by(order);
    lights.sort_by(|a, b| order(&a.0, &b.0));
    let (lights, light_landmark) = lights.into_iter().unzip();
    let mut ground = generate_ground(key, frame, profile);
    ground.roads.retain(|r| r.class <= ROAD_SECONDARY);
    OverviewLevel { blocks, landmarks, lights, light_landmark, ground, proj }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::tile_for;

    const VILNIUS: (f64, f64) = (54.6872, 25.2797);

    #[test]
    fn deterministic_and_owned() {
        let f = Frame::new(VILNIUS.0, VILNIUS.1);
        let k = tile_for(VILNIUS.0, VILNIUS.1, 16);
        let a = generate(k, f);
        let b = generate(k, f);
        assert_eq!(a, b);
        assert!(a.len() > 10, "dense center: {}", a.len());
        assert!(a.iter().all(|b| b.owned));
        // Footprints stay inside the tile (lots partition it).
        for b in &a {
            for p in &b.polygons[0][0] {
                assert!((0.0..=EXTENT).contains(&p[0]) && (0.0..=EXTENT).contains(&p[1]));
            }
        }
    }

    #[test]
    fn same_lot_same_building_across_zooms() {
        let f = Frame::new(VILNIUS.0, VILNIUS.1);
        let k16 = tile_for(VILNIUS.0, VILNIUS.1, 16);
        let k13 = tile_for(VILNIUS.0, VILNIUS.1, 13);
        let z16 = generate(k16, f);
        let z13 = generate(k13, f);
        // Every z13 building inside the z16 tile also exists at z16 with the same id/height.
        let c16 = f.tile_center(k16);
        let c13 = f.tile_center(k13);
        let rect = f.tile_rect(k16);
        let dp = crate::profile::Profile::default();
        let mut matched = 0;
        for b in &z13 {
            let e = b.centroid[0] as f64 + c13[0];
            let n = b.centroid[1] as f64 + c13[1];
            if e > rect.min_e + 1.0 && e < rect.max_e - 1.0 && n > rect.min_n + 1.0 && n < rect.max_n - 1.0 {
                let twin = z16.iter().find(|x| x.id == b.id).expect("twin at z16");
                assert_eq!(crate::mesher::resolve(twin, &dp), crate::mesher::resolve(b, &dp));
                assert!((twin.centroid[0] as f64 + c16[0] - e).abs() < 0.5);
                matched += 1;
            }
        }
        assert!(matched > 0);
        assert!(z13.len() < 2000, "z13 thinned: {}", z13.len());
    }

    #[test]
    fn ground_is_deterministic_with_streets_districts_and_lakes() {
        let f = Frame::new(VILNIUS.0, VILNIUS.1);
        let k16 = tile_for(VILNIUS.0, VILNIUS.1, 16);
        let g = generate_ground(k16, f, &crate::profile::Profile::default());
        assert_eq!(g, generate_ground(k16, f, &crate::profile::Profile::default()));
        assert_eq!(g.areas.len(), 1, "a z16 tile is a quarter district");
        assert_eq!(g.roads.len(), 4, "streets every 4th of 8 lot lines, two axes");
        let k13 = tile_for(VILNIUS.0, VILNIUS.1, 13);
        let g13 = generate_ground(k13, f, &crate::profile::Profile::default());
        assert_eq!(g13.areas.len(), 16);
        assert_eq!(g13.roads.len(), 32);
        // Somewhere near Vilnius there is a lake with an island hole.
        let lake = (0..64u32)
            .flat_map(|i| (0..64u32).map(move |j| TileKey::new(13, k13.x - 32 + i, k13.y - 32 + j)))
            .flat_map(|k| generate_ground(k, f, &crate::profile::Profile::default()).areas)
            .find(|a| a.class == GROUND_WATER)
            .expect("a lake");
        assert_eq!(lake.rings.len(), 2);
    }
}
