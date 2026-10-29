//! Data levels (Tiles and LOD v2, step T2c): the synthetic city as a two-level source, built the
//! way `simworld-cli bake` builds an archive, so the streaming path is the same offline.

use super::{generate_ground, generate_lots, generate_raw, EXTENT, LOT_ZOOM, STREET_EVERY};
use crate::geo::{Frame, TileKey};
use crate::mvt::{Building, BuildingTags, GroundFeatures, Landuse, RawBuilding, TileProjector, ROAD_SECONDARY};

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
