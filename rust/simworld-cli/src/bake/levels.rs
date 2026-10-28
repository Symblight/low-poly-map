//! The two data levels (docs: spec Tiles and LOD v2, sections 1, 4, 6).
//!
//! * detail z14, extent 8192: every non-landmark footprint (simplified 0.5 m, >= 20 m², all
//!   source attributes), `landuse` and `roads` of the source's z14 tiles (all classes).
//! * overview z11, extent 4096: `blocks` (buildings closer than `block_gap_m` merged, height =
//!   area-weighted mean rounded to `height_step_m`, simplified, >= `block_min_area_m2`),
//!   `lights` (one point per building), `roads` >= secondary, coarse `landuse` + water; plus the
//!   `landmarks` layer at extent 16384 (buildings with a roof >= `landmark_m`, full geometry).

use std::collections::{BTreeMap, HashMap};

use geo::algorithm::buffer::{Buffer, BufferStyle, LineJoin};
use rayon::prelude::*;

use super::archive::{Archive, Key};
use super::geom::{self, m_per_g, tile_span_g, MPoly, Rect, Ring};
use super::input::{self, prop, Building, FGeom, Feature, Props};
use super::mvt_enc::{self, Geom, Layer, Quant, Val};

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

/// A merged block in global units.
pub struct Block {
    pub polys: MPoly,
    pub height_m: f64,
    pub kind: String,
    pub id: u64,
    pub members: usize,
}

struct Dsu(Vec<u32>);
impl Dsu {
    fn find(&mut self, mut i: u32) -> u32 {
        while self.0[i as usize] != i {
            self.0[i as usize] = self.0[self.0[i as usize] as usize];
            i = self.0[i as usize];
        }
        i
    }
    fn join(&mut self, a: u32, b: u32) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[a.max(b) as usize] = a.min(b);
        }
    }
}

/// Clusters of buildings whose footprints are within `block_gap_m` of each other.
pub fn clusters(cand: &[&Building], gap_m: f64) -> Vec<Vec<usize>> {
    const CELL: f64 = 2048.0;
    let gap_g: Vec<f64> = cand.iter().map(|b| gap_m / m_per_g(b.centroid[1])).collect();
    let rects: Vec<Rect> = cand.iter().zip(&gap_g).map(|(b, g)| Rect::of_mpoly(&b.polys).grow(g / 2.0)).collect();
    let mut cells: HashMap<(i64, i64), Vec<u32>> = HashMap::new();
    for (i, r) in rects.iter().enumerate() {
        for cy in (r.y0 / CELL).floor() as i64..=(r.y1 / CELL).floor() as i64 {
            for cx in (r.x0 / CELL).floor() as i64..=(r.x1 / CELL).floor() as i64 {
                cells.entry((cx, cy)).or_default().push(i as u32);
            }
        }
    }
    // Candidate pairs (bbox overlap), distance tested in parallel, joined in a fixed order.
    let mut keys: Vec<&(i64, i64)> = cells.keys().collect();
    keys.sort();
    let pairs: Vec<(u32, u32)> = keys
        .par_iter()
        .flat_map_iter(|k| {
            let v = &cells[*k];
            let mut out = Vec::new();
            for (ai, &a) in v.iter().enumerate() {
                for &b in &v[ai + 1..] {
                    if !rects[a as usize].intersects(&rects[b as usize]) {
                        continue;
                    }
                    // Test each pair once: in the cell holding the overlap's top-left corner.
                    let (ra, rb) = (rects[a as usize], rects[b as usize]);
                    let (ox, oy) = (ra.x0.max(rb.x0), ra.y0.max(rb.y0));
                    if ((ox / CELL).floor() as i64, (oy / CELL).floor() as i64) != **k {
                        continue;
                    }
                    let g = (gap_g[a as usize] + gap_g[b as usize]) / 2.0;
                    if geom::mpoly_distance(&cand[a as usize].polys, &cand[b as usize].polys) <= g {
                        out.push((a, b));
                    }
                }
            }
            out
        })
        .collect();
    let mut dsu = Dsu((0..cand.len() as u32).collect());
    for (a, b) in pairs {
        dsu.join(a, b);
    }
    let mut groups: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for i in 0..cand.len() {
        let r = dsu.find(i as u32);
        groups.entry(r).or_default().push(i);
    }
    groups.into_values().collect()
}

/// Blocks of one cluster: buffer by gap/2, union, assign members, shrink by gap/2, simplify,
/// keep >= min area. Returns the blocks and how many members ended in a kept block.
fn cluster_blocks(members: &[&Building], p: &Params) -> (Vec<Block>, usize, f64) {
    let o = members[0].polys[0][0][0];
    let scale = m_per_g(members[0].centroid[1]); // g -> local metres
    let r = p.block_gap_m / 2.0;
    let style = |d: f64| BufferStyle::new(d).line_join(LineJoin::Miter(0.4));
    // Components in local metres with their member indices.
    let comps: Vec<(MPoly, Vec<usize>)> = if members.len() == 1 {
        vec![(geom::from_geo(&geom::to_geo(&members[0].polys, o, scale), [0.0, 0.0], 1.0), vec![0])]
    } else {
        let buffered: Vec<geo::Polygon<f64>> = members.iter().flat_map(|b| geom::to_geo(&b.polys, o, scale).buffer_with_style(style(r)).0).collect();
        let u = geom::from_geo(&geo::unary_union(&buffered), [0.0, 0.0], 1.0);
        let mut owner: Vec<Vec<usize>> = vec![Vec::new(); u.len()];
        for (i, b) in members.iter().enumerate() {
            let v = b.polys[0][0][0];
            let pt = [(v[0] - o[0]) * scale, (v[1] - o[1]) * scale];
            let c = u.iter().position(|poly| geom::point_in_poly(pt, poly)).unwrap_or_else(|| {
                // On a boundary by rounding: the nearest component.
                (0..u.len())
                    .min_by(|&a, &b| {
                        let d = |k: usize| geom::mpoly_distance(&vec![u[k].clone()], &vec![vec![vec![pt, [pt[0] + 1e-6, pt[1]], [pt[0], pt[1] + 1e-6]]]]);
                        d(a).total_cmp(&d(b))
                    })
                    .unwrap_or(0)
            });
            owner[c].push(i);
        }
        u.into_iter()
            .zip(owner)
            .filter(|(_, m)| !m.is_empty())
            .map(|(poly, m)| {
                let shrunk = geom::to_geo(&vec![poly], [0.0, 0.0], 1.0).buffer_with_style(style(-r));
                (geom::from_geo(&shrunk, [0.0, 0.0], 1.0), m)
            })
            .collect()
    };
    let mut out = Vec::new();
    let mut kept = 0;
    let mut kept_m2 = 0.0;
    for (m, idx) in comps {
        let s = geom::simplify_mpoly(&m, p.block_simplify_m);
        let area = geom::mpoly_area(&s);
        if area < p.block_min_area_m2 {
            continue;
        }
        let (mut sw, mut sh) = (0.0, 0.0);
        let mut kinds: BTreeMap<&str, f64> = BTreeMap::new();
        for &i in &idx {
            let b = members[i];
            sw += b.area_m2;
            sh += b.area_m2 * b.roof_m as f64;
            *kinds.entry(b.kind.as_str()).or_default() += b.area_m2;
        }
        let h = if sw > 0.0 { sh / sw } else { 0.0 };
        let kind = kinds.iter().max_by(|a, b| a.1.total_cmp(b.1).then(b.0.cmp(a.0))).map(|k| k.0.to_string()).unwrap_or_else(|| "yes".into());
        let id = idx.iter().map(|&i| members[i].key.1).min().unwrap_or(0);
        kept += idx.len();
        kept_m2 += sw;
        let polys = geom::from_geo(&geom::to_geo(&s, [0.0, 0.0], 1.0), o, scale);
        out.push(Block { polys, height_m: round_step(h, p.height_step_m), kind, id, members: idx.len() });
    }
    (out, kept, kept_m2)
}

pub fn blocks(bs: &[Building], p: &Params, counts: &mut Counts) -> (LayerTiles, Vec<Block>) {
    let cand: Vec<&Building> = bs.iter().filter(|b| !is_landmark(b, p)).collect();
    let cl = clusters(&cand, p.block_gap_m);
    counts.block_candidates = cand.len();
    counts.clusters = cl.len();
    counts.candidate_m2 = cand.iter().map(|b| b.area_m2).sum();
    let res: Vec<(Vec<Block>, usize, f64)> = cl
        .par_iter()
        .map(|idx| {
            let members: Vec<&Building> = idx.iter().map(|&i| cand[i]).collect();
            cluster_blocks(&members, p)
        })
        .collect();
    let mut all = Vec::new();
    for (b, k, m2) in res {
        counts.buildings_in_blocks += k;
        counts.in_blocks_m2 += m2;
        all.extend(b);
    }
    counts.blocks = all.len();
    let (z, ext, buf) = (p.overview_z, p.overview_extent, p.buffer_g(p.overview_z));
    let mut t = LayerTiles::new();
    for b in &all {
        let props: Props = vec![
            ("id".into(), Val::F64(b.id as f64)),
            ("kind".into(), Val::Str(b.kind.clone())),
            ("height".into(), Val::F64(b.height_m)),
            ("count".into(), Val::I64(b.members as i64)),
        ];
        polys_into_tiles(&b.polys, z, ext, buf, |k, g| t.entry(k).or_default().push(Feat { id: None, props: props.clone(), geom: g }));
    }
    (t, all)
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
    let groups: Vec<((u64, String), (Props, Vec<MPoly>))> = by_id.into_iter().collect();
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
