//! Overview data tiles (Tiles and LOD v2): blocks, landmarks and one light point per building.

use std::collections::HashMap;

use super::attrs::{building_tags, num, text};
use super::decode::{common_extent, decode_building_layer, decode_landuse, decode_roads, layer_extent, sort_buildings};
use super::{
    feeds_building_fallback, maybe_gunzip, stack_raised_parts, Building, BuildingTags, GroundFeatures, Landuse, OsmType, TileProjector,
    BLOCKS_LAYER, LANDMARKS_LAYER, LIGHTS_LAYER,
};
use crate::geo::{Frame, TileKey};
use crate::profile::Profile;
use crate::terrain::Terrain;

/// OSM type from a planetiler feature id (`osm_id * 10 + 2` way, `+ 3` relation).
fn osm_type_of_fid(fid: u64) -> OsmType {
    match fid % 10 {
        2 => OsmType::Way,
        3 => OsmType::Relation,
        _ => OsmType::Unknown,
    }
}

/// Attributes of one light point (see [`light_point`]).
#[derive(Clone, Debug, Default)]
pub struct LightAttrs {
    pub osm_type: Option<OsmType>,
    pub id: u64,
    pub kind: String,
    pub height: Option<f32>,
    pub levels: Option<f32>,
    pub area_m2: Option<f32>,
    pub landuse: Option<Landuse>,
    pub tags: BuildingTags,
}

/// A light point of the overview `lights` layer as a building without geometry: `polygons` is
/// empty, `centroid_px` is the point, `base_m` the terrain there, `footprint_m2` the `area`
/// attribute (tagged heights only; without it the building counts as `clipped`, so the
/// slender-tower rule needs the area) and `height` the rounded tagged roof height. Light
/// points, light profiles (kind, state tags, overrides by OSM id, slender towers, landuse
/// fallback) and heights resolve like a footprint's ([`crate::mesher::resolve`]).
pub fn light_point(proj: &TileProjector, px: [f64; 2], a: LightAttrs) -> Building {
    let ext = proj.extent();
    Building {
        osm_type: a.osm_type.unwrap_or(OsmType::Unknown),
        id: a.id,
        kind: if a.kind.is_empty() { "yes".into() } else { a.kind },
        height: a.height,
        levels: a.levels,
        min_height: 0.0,
        landuse: a.landuse,
        tags: a.tags,
        polygons: Vec::new(),
        clipped: a.area_m2.is_none(),
        footprint_m2: a.area_m2.unwrap_or(0.0),
        centroid_px: px,
        centroid: proj.project(px),
        base_m: proj.ground_at(px) as f32,
        owned: px[0] >= 0.0 && px[0] < ext && px[1] >= 0.0 && px[1] < ext,
    }
}

/// The `lights` layer (points) -> light points ([`light_point`]), sorted like buildings.
fn decode_lights_layer(
    reader: &mvt_reader::Reader,
    layers: &[mvt_reader::layer::Layer],
    extent: f64,
    proj: &TileProjector,
) -> Result<Vec<Building>, String> {
    let Some(layer) = layers.iter().find(|l| l.name == LIGHTS_LAYER) else {
        return Ok(Vec::new());
    };
    let k = extent / layer_extent(layer);
    let features = reader
        .get_features(layer.layer_index)
        .map_err(|e| format!("mvt lights: {e:?}"))?;
    let empty = HashMap::new();
    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let p = match &f.geometry {
            geo_types::Geometry::Point(p) => p.0,
            geo_types::Geometry::MultiPoint(mp) => match mp.0.first() {
                Some(p) => p.0,
                None => continue,
            },
            _ => continue,
        };
        let props = f.properties.as_ref().unwrap_or(&empty);
        let fid = f.id.unwrap_or(0);
        let id = props.get("id").and_then(num).filter(|v| *v >= 0.0).map(|v| v as u64).unwrap_or(fid / 10);
        let osm_type = props.get("osm_type").and_then(text).map(|s| OsmType::parse(&s)).unwrap_or_else(|| osm_type_of_fid(fid));
        let landuse = props.get("landuse").and_then(text).filter(|k| feeds_building_fallback(k)).map(|k| Landuse::parse(&k));
        let attrs = LightAttrs {
            osm_type: Some(osm_type),
            id,
            kind: props.get("kind").and_then(text).unwrap_or_default(),
            height: props.get("height").and_then(num).map(|v| v as f32),
            levels: props.get("levels").and_then(num).map(|v| v as f32),
            area_m2: props.get("area").and_then(num).map(|v| v as f32),
            landuse,
            tags: building_tags(props),
        };
        out.push(light_point(proj, [p.x as f64 * k, p.y as f64 * k], attrs));
    }
    sort_buildings(&mut out);
    Ok(out)
}

/// One decoded overview data tile (Tiles and LOD v2): merged `blocks` (R2 meshes), `landmarks`
/// (full geometry), `lights` (one point per building, [`light_point`]) with a flag per light
/// whose building is one of this tile's landmarks, and the ground (`landuse`, `roads`).
#[derive(Debug)]
pub struct DecodedOverview {
    pub blocks: Vec<Building>,
    pub landmarks: Vec<Building>,
    pub lights: Vec<Building>,
    pub light_landmark: Vec<bool>,
    pub ground: GroundFeatures,
    pub proj: TileProjector,
}

/// Flag the lights whose OSM id belongs to one of `landmarks` (a landmark's light lies in the
/// overview tile holding its centroid, where the landmark is too).
pub fn landmark_flags(lights: &[Building], landmarks: &[Building]) -> Vec<bool> {
    let mut ids: Vec<u64> = landmarks.iter().map(|b| b.id).collect();
    ids.sort_unstable();
    ids.dedup();
    lights.iter().map(|l| ids.binary_search(&l.id).is_ok()).collect()
}

/// Decode an overview data tile (gzip or raw MVT; empty bytes = an empty tile) on `terrain`.
/// Every layer shares one pixel space (the largest layer extent, the landmarks' 16384).
pub fn decode_overview_on(bytes: &[u8], key: TileKey, frame: Frame, terrain: &Terrain, profile: &Profile) -> Result<DecodedOverview, String> {
    if bytes.is_empty() {
        return Ok(DecodedOverview {
            blocks: Vec::new(),
            landmarks: Vec::new(),
            lights: Vec::new(),
            light_landmark: Vec::new(),
            ground: GroundFeatures::default(),
            proj: TileProjector::new(key, frame, 4096.0).with_terrain(terrain.clone()),
        });
    }
    let data = maybe_gunzip(bytes)?;
    let reader = mvt_reader::Reader::new(data).map_err(|e| format!("mvt: {e:?}"))?;
    let layers = reader.get_layer_metadata().map_err(|e| format!("mvt layers: {e:?}"))?;
    let extent = common_extent(&layers);
    let mut ground = GroundFeatures::default();
    let landuse = decode_landuse(&reader, &layers, Some(&mut ground.areas), profile, extent)?;
    decode_roads(&reader, &layers, &mut ground.roads, profile, extent)?;
    ground.sort();
    let proj = TileProjector::new(key, frame, extent).with_terrain(terrain.clone());
    // Block bases are left open (placeholder from the flat projector): the R2 chunks that mesh a
    // block resolve it with `proj` (`data::LazyBases`, perf review 2 item 7: thousands of blocks
    // sampled the terrain along every edge before any chunk of the tile could start).
    let flat = TileProjector::new(key, frame, extent);
    let mut blocks = decode_building_layer(&reader, &layers, BLOCKS_LAYER, extent, &flat, &landuse)?;
    sort_buildings(&mut blocks);
    let mut landmarks = decode_building_layer(&reader, &layers, LANDMARKS_LAYER, extent, &proj, &landuse)?;
    sort_buildings(&mut landmarks);
    if terrain.is_active() {
        stack_raised_parts(&mut landmarks);
    }
    let lights = decode_lights_layer(&reader, &layers, extent, &proj)?;
    let light_landmark = landmark_flags(&lights, &landmarks);
    Ok(DecodedOverview { blocks, landmarks, lights, light_landmark, ground, proj })
}
