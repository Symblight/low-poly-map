//! The two data levels (docs: spec Tiles and LOD v2, sections 1, 4, 6).
//!
//! * detail z14, extent 8192: every non-landmark footprint (simplified 0.5 m, >= 20 m², all
//!   source attributes), `landuse` and `roads` of the source's z14 tiles (all classes).
//! * overview z11, extent 4096: `blocks` (buildings closer than `block_gap_m` merged, height =
//!   area-weighted mean rounded to `height_step_m`, simplified, >= `block_min_area_m2`),
//!   `lights` (one point per building), `roads` >= secondary, coarse `landuse` + water; plus the
//!   `landmarks` layer at extent 16384 (buildings with a roof >= `landmark_m`, full geometry).

use std::collections::BTreeMap;

use rayon::prelude::*;

use super::archive::{Archive, Key};
use super::geom::{self, m_per_g, tile_span_g, MPoly, Rect, Ring};
use super::input::{self, prop, Building, FGeom, Feature, Props};
use super::mvt_enc::{self, Geom, Layer, Quant, Val};

mod blocks;

pub use blocks::blocks;

#[derive(Clone, Debug)]
pub struct Params {
    pub detail_z: u8,
    pub detail_extent: u32,
    pub overview_z: u8,
    pub overview_extent: u32,
    pub landmark_extent: u32,
    /// Encoder buffer as a fraction of the tile (planetiler `buffer: 4` of 256 px).
    pub buffer_frac: f64,
    pub detail_simplify_m: f64,
    pub detail_min_area_m2: f64,
    pub landmark_m: f64,
    pub landmark_simplify_m: f64,
    pub block_gap_m: f64,
    pub block_simplify_m: f64,
    pub block_min_area_m2: f64,
    pub height_step_m: f64,
    pub overview_road_kinds: Vec<String>,
    pub overview_road_simplify_m: f64,
    pub overview_landuse_min_m2: f64,
    pub overview_water_min_m2: f64,
    pub overview_landuse_simplify_m: f64,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            detail_z: 14,
            detail_extent: 8192,
            overview_z: 11,
            overview_extent: 4096,
            landmark_extent: 16384,
            buffer_frac: 4.0 / 256.0,
            detail_simplify_m: 0.5,
            detail_min_area_m2: 20.0,
            landmark_m: 60.0,
            landmark_simplify_m: 0.1,
            block_gap_m: 4.0,
            block_simplify_m: 5.0,
            block_min_area_m2: 150.0,
            height_step_m: 5.0,
            overview_road_kinds: ["motorway", "trunk", "primary", "secondary", "motorway_link", "trunk_link", "primary_link", "secondary_link"]
                .map(String::from)
                .to_vec(),
            overview_road_simplify_m: 3.0,
            overview_landuse_min_m2: 10_000.0,
            overview_water_min_m2: 2_000.0,
            overview_landuse_simplify_m: 5.0,
        }
    }
}

impl Params {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "detail": {"zoom": self.detail_z, "extent": self.detail_extent, "simplify_m": self.detail_simplify_m, "min_area_m2": self.detail_min_area_m2},
            "overview": {"zoom": self.overview_z, "extent": self.overview_extent, "landmark_extent": self.landmark_extent,
                "block_gap_m": self.block_gap_m, "block_simplify_m": self.block_simplify_m, "block_min_area_m2": self.block_min_area_m2,
                "height_step_m": self.height_step_m, "road_kinds": self.overview_road_kinds, "road_simplify_m": self.overview_road_simplify_m,
                "landuse_min_area_m2": self.overview_landuse_min_m2, "water_min_area_m2": self.overview_water_min_m2,
                "landuse_simplify_m": self.overview_landuse_simplify_m},
            "landmark_m": self.landmark_m,
            "buffer_px_of_256": self.buffer_frac * 256.0,
        })
    }
    fn buffer_g(&self, z: u8) -> f64 {
        tile_span_g(z) * self.buffer_frac
    }
}

/// One output feature.
pub struct Feat {
    pub id: Option<u64>,
    pub props: Props,
    pub geom: Geom,
}

/// Layer name -> tile -> features.
pub type LayerTiles = BTreeMap<Key, Vec<Feat>>;

#[derive(Default, Debug)]
pub struct Counts {
    pub buildings: usize,
    pub detail_buildings: usize,
    pub landmarks: usize,
    pub dropped_small: usize,
    pub block_candidates: usize,
    pub blocks: usize,
    pub buildings_in_blocks: usize,
    /// Footprint m² of all block candidates / of those inside a kept block.
    pub candidate_m2: f64,
    pub in_blocks_m2: f64,
    pub clusters: usize,
    pub lights: usize,
}

pub fn is_landmark(b: &Building, p: &Params) -> bool {
    b.tagged_roof_m.is_some_and(|h| h as f64 >= p.landmark_m)
}

fn fid(b: &Building) -> Option<u64> {
    // planetiler's feature id: osm_id * 10 + (2 way, 3 relation); the runtime falls back to it.
    let t = match b.key.0 {
        1 => 2,
        2 => 3,
        _ => 0,
    };
    b.key.1.checked_mul(10).map(|v| v + t)
}

/// Clips `m` into every tile of zoom `z` it meets (tile + buffer) and quantises it.
pub(crate) fn polys_into_tiles(m: &MPoly, z: u8, extent: u32, buffer: f64, mut f: impl FnMut(Key, Geom)) {
    let span = tile_span_g(z);
    for (x, y) in Rect::of_mpoly(m).tiles(z, buffer) {
        let c = geom::clip_mpoly(m, &Rect::tile(z, x, y, buffer));
        if c.is_empty() {
            continue;
        }
        let q = Quant { ox: x as f64 * span, oy: y as f64 * span, g_per_unit: span / extent as f64 };
        f((z, x, y), Geom::Polys(q.mpoly(&c)));
    }
}

pub(crate) fn lines_into_tiles(l: &[Ring], z: u8, extent: u32, buffer: f64, mut f: impl FnMut(Key, Geom)) {
    let span = tile_span_g(z);
    for (x, y) in Rect::of_lines(l).tiles(z, buffer) {
        let r = Rect::tile(z, x, y, buffer);
        let c: Vec<Ring> = l.iter().flat_map(|s| geom::clip_line(s, &r)).collect();
        if c.is_empty() {
            continue;
        }
        let q = Quant { ox: x as f64 * span, oy: y as f64 * span, g_per_unit: span / extent as f64 };
        f((z, x, y), Geom::Lines(q.lines(&c)));
    }
}

fn round_step(v: f64, step: f64) -> f64 {
    ((v / step).round() * step).max(step)
}

// ---------------------------------------------------------------------------------------------
// Detail

/// The detail footprint of a building: simplified by `detail_simplify_m` (the source footprint
/// if simplification collapses it), `None` for landmarks and footprints under
/// `detail_min_area_m2` after simplification.
pub fn detail_footprint(b: &Building, p: &Params) -> Option<MPoly> {
    if is_landmark(b, p) {
        return None;
    }
    let mpg = m_per_g(b.centroid[1]);
    let mut m = geom::simplify_mpoly(&b.polys, p.detail_simplify_m / mpg);
    if m.is_empty() {
        m = b.polys.clone();
    }
    (geom::mpoly_area(&m) * mpg * mpg >= p.detail_min_area_m2).then_some(m)
}

pub fn detail_buildings(bs: &[Building], p: &Params, counts: &mut Counts) -> LayerTiles {
    let (z, ext, buf) = (p.detail_z, p.detail_extent, p.buffer_g(p.detail_z));
    let per: Vec<Option<Vec<(Key, Feat)>>> = bs
        .par_iter()
        .map(|b| {
            let m = detail_footprint(b, p)?;
            let mut out = Vec::new();
            polys_into_tiles(&m, z, ext, buf, |k, g| out.push((k, Feat { id: fid(b), props: b.props.clone(), geom: g })));
            Some(out)
        })
        .collect();
    counts.detail_buildings = per.iter().filter(|v| v.is_some()).count();
    counts.dropped_small = bs.iter().filter(|b| !is_landmark(b, p)).count() - counts.detail_buildings;
    let mut t = LayerTiles::new();
    for (k, f) in per.into_iter().flatten().flatten() {
        t.entry(k).or_default().push(f);
    }
    t
}

/// The source's own z14 `landuse` / `roads` tiles re-encoded at the detail extent (same features,
/// same clipping: the detail level is the source's z14 grid).
pub fn detail_ground(src: &Archive, p: &Params, layer: &str) -> Result<LayerTiles, String> {
    let z = p.detail_z;
    let span = tile_span_g(z);
    let per: Vec<(Key, Vec<Feat>)> = src
        .keys_at(z)
        .par_iter()
        .map(|&k| {
            let bytes = src.tile(k)?.unwrap_or_default();
            let Some((_, feats)) = input::decode_layer(&bytes, k, layer)? else { return Ok((k, Vec::new())) };
            let q = Quant { ox: k.1 as f64 * span, oy: k.2 as f64 * span, g_per_unit: span / p.detail_extent as f64 };
            let out = feats
                .into_iter()
                .filter_map(|f| {
                    let geom = match &f.geom {
                        FGeom::Polys(m) => Geom::Polys(q.mpoly(m)),
                        FGeom::Lines(l) => Geom::Lines(q.lines(l)),
                        FGeom::Points(_) => return None,
                    };
                    Some(Feat { id: f.fid, props: f.props, geom })
                })
                .collect();
            Ok((k, out))
        })
        .collect::<Result<_, String>>()?;
    Ok(per.into_iter().filter(|(_, v)| !v.is_empty()).collect())
}

// ---------------------------------------------------------------------------------------------
// Overview

pub fn landmarks(bs: &[Building], p: &Params, counts: &mut Counts) -> LayerTiles {
    let (z, ext, buf) = (p.overview_z, p.landmark_extent, p.buffer_g(p.overview_z));
    let mut t = LayerTiles::new();
    for b in bs.iter().filter(|b| is_landmark(b, p)) {
        counts.landmarks += 1;
        let mut m = geom::simplify_mpoly(&b.polys, p.landmark_simplify_m / m_per_g(b.centroid[1]));
        if m.is_empty() {
            m = b.polys.clone();
        }
        polys_into_tiles(&m, z, ext, buf, |k, g| t.entry(k).or_default().push(Feat { id: fid(b), props: b.props.clone(), geom: g }));
    }
    t
}

const LIGHT_TAGS: [&str; 6] = ["historic", "man_made", "windows", "ruins", "disused", "abandoned"];

/// One point per building (landmarks included) at its footprint centroid, in the single overview
/// tile that contains it. The OSM id and type are only in the feature id (`id * 10 + 2` way,
/// `+ 3` relation, planetiler's encoding; a 247k-entry `id` value table would double the layer).
/// Attributes: kind, the state tags; `height` (tagged roof rounded to
/// `height_step_m`) and `area` (m², for the slender-tower rule) only for tagged heights; `landuse`
/// = the fallback landuse kind at the centroid (the runtime's kind / height fallback).
pub fn lights(bs: &[Building], p: &Params, counts: &mut Counts) -> LayerTiles {
    let (z, ext) = (p.overview_z, p.overview_extent);
    let span = tile_span_g(z);
    let mut t = LayerTiles::new();
    for b in bs {
        let (x, y) = ((b.centroid[0] / span).floor() as u32, (b.centroid[1] / span).floor() as u32);
        let q = Quant { ox: x as f64 * span, oy: y as f64 * span, g_per_unit: span / ext as f64 };
        let mut props: Props = vec![("kind".into(), Val::Str(b.kind.clone()))];
        if let Some(h) = b.tagged_roof_m {
            props.push(("height".into(), Val::F64(round_step(h as f64, p.height_step_m))));
            props.push(("area".into(), Val::F64(b.area_m2.round())));
        }
        if let Some(l) = &b.landuse {
            props.push(("landuse".into(), Val::Str(l.clone())));
        }
        for k in LIGHT_TAGS {
            if let Some(v) = prop(&b.props, k) {
                props.push((k.into(), v.clone()));
            }
        }
        let pt = q.pt(b.centroid);
        let pt = [pt[0].clamp(0, ext as i32 - 1), pt[1].clamp(0, ext as i32 - 1)];
        t.entry((z, x, y)).or_default().push(Feat { id: fid(b), props, geom: Geom::Points(vec![pt]) });
        counts.lights += 1;
    }
    t
}

/// Overview roads: the source's z13 roads of `overview_road_kinds`, cut at the z13 tile edges,
/// grouped by id per overview tile, simplified.
pub fn overview_roads(pieces: &[Feature], p: &Params) -> LayerTiles {
    let (z, ext, buf) = (p.overview_z, p.overview_extent, p.buffer_g(p.overview_z));
    let mut by_id: BTreeMap<(u64, String), (Props, Vec<Ring>)> = BTreeMap::new();
    for f in pieces {
        let kind = prop(&f.props, "kind").and_then(Val::as_str).unwrap_or("");
        if !p.overview_road_kinds.iter().any(|k| k == kind) {
            continue;
        }
        let FGeom::Lines(ls) = &f.geom else { continue };
        let e = by_id.entry((input::osm_id(&f.props, f.fid), kind.to_string())).or_insert_with(|| (f.props.clone(), Vec::new()));
        e.1.extend(ls.iter().cloned());
    }
    let mut t = LayerTiles::new();
    for ((_, _), (props, lines)) in by_id {
        let lines: Vec<Ring> = lines
            .iter()
            .map(|l| geom::simplify_line(l, p.overview_road_simplify_m / m_per_g(l[0][1])))
            .filter(|l| l.len() >= 2)
            .collect();
        lines_into_tiles(&lines, z, ext, buf, |k, g| t.entry(k).or_default().push(Feat { id: None, props: props.clone(), geom: g }));
    }
    t
}

fn is_water(kind: &str) -> bool {
    matches!(kind, "water" | "riverbank" | "reservoir" | "basin")
}

/// Overview landuse: the source's z13 polygons, pieces of one id unioned, water >=
/// `overview_water_min_m2`, the rest >= `overview_landuse_min_m2`, simplified.
pub fn overview_landuse(pieces: &[Feature], p: &Params) -> LayerTiles {
    let mut by_id: BTreeMap<(u64, String), (Props, Vec<MPoly>)> = BTreeMap::new();
    for f in pieces {
        let FGeom::Polys(m) = &f.geom else { continue };
        let kind = prop(&f.props, "kind").and_then(Val::as_str).unwrap_or("").to_string();
        let e = by_id.entry((input::osm_id(&f.props, f.fid), kind)).or_insert_with(|| (f.props.clone(), Vec::new()));
        e.1.push(m.clone());
    }
    let groups: Vec<_> = by_id.into_iter().collect();
    let done: Vec<Option<(Props, MPoly)>> = groups
        .into_par_iter()
        .map(|((_, kind), (props, parts))| {
            let m = if parts.len() == 1 { parts.into_iter().next().unwrap() } else { geom::union(&parts) };
            let c = geom::centroid(&m)?;
            let mpg = m_per_g(c[1]);
            let min = if is_water(&kind) { p.overview_water_min_m2 } else { p.overview_landuse_min_m2 };
            if geom::mpoly_area(&m) * mpg * mpg < min {
                return None;
            }
            let s = geom::simplify_mpoly(&m, p.overview_landuse_simplify_m / mpg);
            (!s.is_empty()).then_some((props, s))
        })
        .collect();
    let (z, ext, buf) = (p.overview_z, p.overview_extent, p.buffer_g(p.overview_z));
    let mut t = LayerTiles::new();
    for (props, m) in done.into_iter().flatten() {
        polys_into_tiles(&m, z, ext, buf, |k, g| t.entry(k).or_default().push(Feat { id: None, props: props.clone(), geom: g }));
    }
    t
}

/// Encodes the given layers per tile: (tile -> MVT bytes, tile -> per-layer raw bytes).
pub fn encode(layers: Vec<(&str, u32, LayerTiles)>) -> BTreeMap<Key, Vec<(String, Vec<u8>)>> {
    let mut keys: Vec<Key> = layers.iter().flat_map(|l| l.2.keys().copied()).collect();
    keys.sort();
    keys.dedup();
    keys.par_iter()
        .map(|k| {
            let mut out = Vec::new();
            for (name, extent, tiles) in &layers {
                let Some(feats) = tiles.get(k) else { continue };
                let mut l = Layer::new(name, *extent);
                for f in feats {
                    let pr: Vec<(&str, Val)> = f.props.iter().map(|(a, v)| (a.as_str(), v.clone())).collect();
                    l.add(f.id, &pr, &f.geom);
                }
                let b = l.encode();
                if !b.is_empty() {
                    out.push((name.to_string(), b));
                }
            }
            (*k, out)
        })
        .filter(|(_, v)| !v.is_empty())
        .collect::<Vec<_>>()
        .into_iter()
        .collect()
}

pub fn tile_bytes(layers: &[(String, Vec<u8>)]) -> Vec<u8> {
    mvt_enc::tile(&layers.iter().map(|l| l.1.clone()).collect::<Vec<_>>())
}
