//! Tile decode: layers -> buildings, landuse areas and road lines in one pixel space.

use std::collections::HashMap;
use std::io::Read;

use super::attrs::{building_tags, num, text};
use super::geometry::{feature_lines, feature_polys};
use super::{
    centroid_px, feeds_building_fallback, stack_raised_parts, Building, GroundArea, GroundFeatures, Landuse, LanduseIndex, OsmType,
    RawBuilding, RoadLine, TileProjector, BLOCKS_LAYER, BUILDINGS_LAYER, LANDMARKS_LAYER, LANDUSE_LAYER, LIGHTS_LAYER, ROADS_LAYER,
};
use crate::geo::{Frame, TileKey};
use crate::profile::Profile;
use crate::terrain::Terrain;

/// Inflate gzip if the payload starts with the gzip magic, else return as is.
pub fn maybe_gunzip(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
        let mut out = Vec::with_capacity(bytes.len() * 3);
        flate2::read::MultiGzDecoder::new(bytes)
            .read_to_end(&mut out)
            .map_err(|e| format!("gunzip: {e}"))?;
        Ok(out)
    } else {
        Ok(bytes.to_vec())
    }
}

pub(super) fn layer_extent(l: &mvt_reader::layer::Layer) -> f64 {
    if l.extent == 0 {
        4096.0
    } else {
        l.extent as f64
    }
}

/// Scale every point by `k` (a layer whose extent differs from the tile's common extent; the
/// common extent is the largest, so `k` is a power of two for the usual extents and exact).
fn scale_rings(rings: &mut [Vec<[f64; 2]>], k: f64) {
    if k != 1.0 {
        for p in rings.iter_mut().flatten() {
            p[0] *= k;
            p[1] *= k;
        }
    }
}

/// The common pixel extent of a tile: the largest extent of the layers the decoder reads. Each
/// layer may have its own extent (the Tiles v2 overview level mixes 4096 and 16384); every layer
/// is scaled to the common one, so all geometry of a tile shares one pixel space.
pub(super) fn common_extent(layers: &[mvt_reader::layer::Layer]) -> f64 {
    layers
        .iter()
        .filter(|l| {
            [BUILDINGS_LAYER, LANDUSE_LAYER, ROADS_LAYER, BLOCKS_LAYER, LANDMARKS_LAYER, LIGHTS_LAYER].contains(&l.name.as_str())
        })
        .map(layer_extent)
        .fold(None, |a: Option<f64>, e| Some(a.map_or(e, |a| a.max(e))))
        .unwrap_or(4096.0)
}

/// `landuse` layer -> building fallback index, plus ground areas when `ground` is set.
pub(super) fn decode_landuse(
    reader: &mvt_reader::Reader,
    layers: &[mvt_reader::layer::Layer],
    ground: Option<&mut Vec<GroundArea>>,
    profile: &Profile,
    extent: f64,
) -> Result<LanduseIndex, String> {
    let Some(layer) = layers.iter().find(|l| l.name == LANDUSE_LAYER) else {
        return Ok(LanduseIndex::empty());
    };
    let k = extent / layer_extent(layer);
    let features = reader
        .get_features(layer.layer_index)
        .map_err(|e| format!("mvt landuse: {e:?}"))?;
    let empty = HashMap::new();
    let mut polys = Vec::new();
    let mut areas = ground;
    for f in features {
        let Some(pp) = feature_polys(&f.geometry) else { continue };
        let props = f.properties.as_ref().unwrap_or(&empty);
        let kind = props.get("kind").and_then(text).unwrap_or_default();
        let id = props.get("id").and_then(num).filter(|v| *v >= 0.0).map(|v| v as u64).or(f.id).unwrap_or(0);
        let fallback = feeds_building_fallback(&kind);
        let class = Landuse::parse(&kind);
        let (gclass, level) = profile.ground_area_class(&kind);
        for mut rings in pp {
            if rings.is_empty() || rings[0].len() < 3 {
                continue;
            }
            scale_rings(&mut rings, k);
            match areas.as_deref_mut() {
                Some(a) => {
                    if fallback {
                        polys.push((class, id, rings.clone()));
                    }
                    a.push(GroundArea { class: gclass, level, id, rings, tint: 0, sub: 0, cut: false });
                }
                None if fallback => polys.push((class, id, rings)),
                None => {}
            }
        }
    }
    Ok(LanduseIndex::new(polys, extent))
}

/// `roads` layer -> road polylines.
pub(super) fn decode_roads(
    reader: &mvt_reader::Reader,
    layers: &[mvt_reader::layer::Layer],
    out: &mut Vec<RoadLine>,
    profile: &Profile,
    extent: f64,
) -> Result<(), String> {
    let Some(layer) = layers.iter().find(|l| l.name == ROADS_LAYER) else {
        return Ok(());
    };
    let k = extent / layer_extent(layer);
    let features = reader
        .get_features(layer.layer_index)
        .map_err(|e| format!("mvt roads: {e:?}"))?;
    let empty = HashMap::new();
    for f in features {
        let Some(mut lines) = feature_lines(&f.geometry) else { continue };
        lines.retain(|l| l.len() >= 2);
        if lines.is_empty() {
            continue;
        }
        scale_rings(&mut lines, k);
        let props = f.properties.as_ref().unwrap_or(&empty);
        let kind = props.get("kind").and_then(text).unwrap_or_default();
        let (class, link, minor) = profile.road_class(&kind);
        let lit = props.get("lit").and_then(text).is_some_and(|v| profile.is_lit(&v));
        let id = props.get("id").and_then(num).filter(|v| *v >= 0.0).map(|v| v as u64).or(f.id).unwrap_or(0);
        out.push(RoadLine { class, link, minor, lit, id, lines });
    }
    Ok(())
}

/// Decode the `buildings` layer of an MVT tile (gzip or raw) into buildings; each building's
/// landuse is resolved from the tile's `landuse` layer at its centroid.
pub fn decode_buildings(bytes: &[u8], key: TileKey, frame: Frame, profile: &Profile) -> Result<Vec<Building>, String> {
    decode_tile(bytes, key, frame, false, false, profile).map(|t| t.buildings)
}

/// One decoded tile: buildings, ground features (only when requested) and the projector.
#[derive(Debug)]
pub struct DecodedTile {
    pub buildings: Vec<Building>,
    pub ground: GroundFeatures,
    pub proj: TileProjector,
}

/// Decode buildings (always) and, with `want_ground`, the `landuse` areas, with `want_roads` the `roads` lines (ground
/// meshes need both; street lamps need roads only, also on R3). Flat terrain; see [`decode_tile_on`].
pub fn decode_tile(
    bytes: &[u8],
    key: TileKey,
    frame: Frame,
    want_ground: bool,
    want_roads: bool,
    profile: &Profile,
) -> Result<DecodedTile, String> {
    decode_tile_on(bytes, key, frame, &Terrain::flat(), want_ground, want_roads, profile)
}

/// [`decode_tile`] on `terrain`: building bases and every later height of the tile come from it
/// (the projector carries it).
pub fn decode_tile_on(
    bytes: &[u8],
    key: TileKey,
    frame: Frame,
    terrain: &Terrain,
    want_ground: bool,
    want_roads: bool,
    profile: &Profile,
) -> Result<DecodedTile, String> {
    let empty_tile = || DecodedTile {
        buildings: Vec::new(),
        ground: GroundFeatures::default(),
        proj: TileProjector::new(key, frame, 4096.0).with_terrain(terrain.clone()),
    };
    if bytes.is_empty() {
        return Ok(empty_tile());
    }
    let data = maybe_gunzip(bytes)?;
    let reader = mvt_reader::Reader::new(data).map_err(|e| format!("mvt: {e:?}"))?;
    let layers = reader.get_layer_metadata().map_err(|e| format!("mvt layers: {e:?}"))?;
    let mut ground = GroundFeatures::default();
    // One pixel space per tile: the largest layer extent; other layers are scaled to it.
    let extent = common_extent(&layers);
    let landuse = decode_landuse(&reader, &layers, want_ground.then_some(&mut ground.areas), profile, extent)?;
    if want_roads {
        decode_roads(&reader, &layers, &mut ground.roads, profile, extent)?;
    }
    if want_ground || want_roads {
        ground.sort();
    }
    let proj = TileProjector::new(key, frame, extent).with_terrain(terrain.clone());
    let mut out = decode_building_layer(&reader, &layers, BUILDINGS_LAYER, extent, &proj, &landuse)?;
    sort_buildings(&mut out);
    if terrain.is_active() {
        stack_raised_parts(&mut out);
    }
    Ok(DecodedTile { buildings: out, ground, proj })
}

/// Deterministic building order regardless of encoder order.
pub(super) fn sort_buildings(out: &mut [Building]) {
    out.sort_by(|a, b| {
        (a.osm_type.tag(), a.id)
            .cmp(&(b.osm_type.tag(), b.id))
            .then(a.centroid_px[0].total_cmp(&b.centroid_px[0]))
            .then(a.centroid_px[1].total_cmp(&b.centroid_px[1]))
    });
}

/// Polygon features of layer `name` as buildings (unsorted; empty when the layer is absent).
pub(super) fn decode_building_layer(
    reader: &mvt_reader::Reader,
    layers: &[mvt_reader::layer::Layer],
    name: &str,
    extent: f64,
    proj: &TileProjector,
    landuse: &LanduseIndex,
) -> Result<Vec<Building>, String> {
    let Some(layer) = layers.iter().find(|l| l.name == name) else {
        return Ok(Vec::new());
    };
    let kb = extent / layer_extent(layer);
    let features = reader
        .get_features(layer.layer_index)
        .map_err(|e| format!("mvt features: {e:?}"))?;
    let empty = HashMap::new();
    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let Some(mut polygons) = feature_polys(&f.geometry) else { continue };
        if kb != 1.0 {
            for p in polygons.iter_mut() {
                scale_rings(p, kb);
            }
        }
        let props = f.properties.as_ref().unwrap_or(&empty);
        let id = props
            .get("id")
            .and_then(num)
            .filter(|v| *v >= 0.0)
            .map(|v| v as u64)
            .or(f.id.map(|fid| fid / 10))
            .unwrap_or(0);
        let lu = centroid_px(&polygons).and_then(|c| landuse.lookup(c));
        let raw = RawBuilding {
            osm_type: props
                .get("osm_type")
                .and_then(text)
                .map(|s| OsmType::parse(&s))
                .unwrap_or(OsmType::Unknown),
            id,
            kind: props.get("kind").and_then(text).unwrap_or_else(|| "yes".into()),
            height: props.get("height").and_then(num).map(|v| v as f32),
            levels: props.get("levels").and_then(num).map(|v| v as f32),
            min_height: props.get("min_height").and_then(num).unwrap_or(0.0).max(0.0) as f32,
            landuse: lu,
            tags: building_tags(props),
            polygons,
        };
        if let Some(b) = proj.finish(raw) {
            out.push(b);
        }
    }
    Ok(out)
}
