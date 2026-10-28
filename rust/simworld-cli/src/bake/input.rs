//! Reading the source archive (today's z13-z16 `vilnius.pmtiles`): whole building footprints
//! stitched from the z16 tiles, the landuse fallback index, and the ground layers.

use std::collections::HashMap;

use rayon::prelude::*;
use simworld_core::mvt::{feeds_building_fallback, Landuse};

use super::archive::{Archive, Key};
use super::geom::{self, mpoly_area, tile_span_g, MPoly, Pt, Rect, Ring};
use super::mvt_enc::Val;

pub type Props = Vec<(String, Val)>;

pub fn prop<'a>(p: &'a Props, k: &str) -> Option<&'a Val> {
    p.iter().find(|(n, _)| n == k).map(|(_, v)| v)
}

pub enum FGeom {
    Polys(MPoly),
    Lines(Vec<Ring>),
    Points(Vec<Pt>),
}

/// One decoded feature in global units.
pub struct Feature {
    pub fid: Option<u64>,
    pub props: Props,
    pub geom: FGeom,
}

/// Decoded layer: (extent, features). Geometry in global units.
pub fn decode_layer(bytes: &[u8], key: Key, name: &str) -> Result<Option<(u32, Vec<Feature>)>, String> {
    let r = mvt_reader::Reader::new(bytes.to_vec()).map_err(|e| format!("{key:?}: mvt: {e:?}"))?;
    let layers = r.get_layer_metadata().map_err(|e| format!("{key:?}: mvt layers: {e:?}"))?;
    let Some(l) = layers.iter().find(|l| l.name == name) else { return Ok(None) };
    let extent = if l.extent == 0 { 4096 } else { l.extent };
    let span = tile_span_g(key.0);
    let (ox, oy, s) = (key.1 as f64 * span, key.2 as f64 * span, span / extent as f64);
    let g = |c: &geo::Coord<f32>| [ox + c.x as f64 * s, oy + c.y as f64 * s];
    let ring = |ls: &geo::LineString<f32>| {
        let mut r: Ring = ls.0.iter().map(g).collect();
        if r.len() > 1 && r.first() == r.last() {
            r.pop();
        }
        r
    };
    let poly = |p: &geo::Polygon<f32>| {
        let mut q = vec![ring(p.exterior())];
        q.extend(p.interiors().iter().map(ring));
        q
    };
    let feats = r.get_features(l.layer_index).map_err(|e| format!("{key:?}: mvt {name}: {e:?}"))?;
    let mut out = Vec::with_capacity(feats.len());
    for f in feats {
        let geom = match &f.geometry {
            geo::Geometry::Polygon(p) => FGeom::Polys(vec![poly(p)]),
            geo::Geometry::MultiPolygon(m) => FGeom::Polys(m.0.iter().map(poly).collect()),
            geo::Geometry::LineString(ls) => FGeom::Lines(vec![ls.0.iter().map(g).collect()]),
            geo::Geometry::MultiLineString(m) => FGeom::Lines(m.0.iter().map(|ls| ls.0.iter().map(g).collect()).collect()),
            geo::Geometry::Point(p) => FGeom::Points(vec![g(&p.0)]),
            geo::Geometry::MultiPoint(m) => FGeom::Points(m.0.iter().map(|p| g(&p.0)).collect()),
            _ => continue,
        };
        let mut props: Props = f
            .properties
            .as_ref()
            .map(|m| m.iter().filter_map(|(k, v)| Val::from_reader(v).map(|v| (k.clone(), v))).collect())
            .unwrap_or_default();
        props.sort_by(|a, b| a.0.cmp(&b.0));
        out.push(Feature { fid: f.id, props, geom });
    }
    Ok(Some((extent, out)))
}

/// OSM type code: 1 way, 2 relation, 0 other; from `osm_type`, else the feature id's last digit.
pub fn osm_type_code(p: &Props, fid: Option<u64>) -> u8 {
    match prop(p, "osm_type").and_then(Val::as_str) {
        Some("way") => 1,
        Some("relation") => 2,
        Some(_) => 0,
        None => match fid.map(|f| f % 10) {
            Some(2) => 1,
            Some(3) => 2,
            _ => 0,
        },
    }
}

pub fn osm_id(p: &Props, fid: Option<u64>) -> u64 {
    prop(p, "id").and_then(Val::as_f64).filter(|v| *v >= 0.0).map(|v| v as u64).or(fid.map(|f| f / 10)).unwrap_or(0)
}

/// A whole building (stitched across the z16 tiles it touches).
pub struct Building {
    pub key: (u8, u64),
    pub props: Props,
    pub polys: MPoly,
    pub area_m2: f64,
    pub centroid: Pt,
    /// Roof height (m above ground) from tagged `height` / `levels`, as the runtime resolves it.
    pub tagged_roof_m: Option<f32>,
    /// Resolved roof height incl. the kind / landuse fallback (`mesher::resolve`).
    pub roof_m: f32,
    pub kind: String,
    /// Kind of the smallest fallback landuse polygon at the centroid.
    pub landuse: Option<String>,
}

struct Piece {
    key: (u8, u64),
    props: Props,
    polys: MPoly,
    clipped: bool,
}

/// Landuse fallback index (only the kinds `mvt::feeds_building_fallback` accepts), global units.
pub struct LanduseIndex {
    polys: Vec<(Rect, f64, u64, String, Vec<Ring>)>,
    cells: HashMap<(i64, i64), Vec<u32>>,
}

const LU_CELL: f64 = 4096.0;

impl LanduseIndex {
    pub fn build(src: &Archive, z: u8) -> Result<Self, String> {
        let per_tile: Vec<Vec<(Rect, f64, u64, String, Vec<Ring>)>> = src
            .keys_at(z)
            .par_iter()
            .map(|&k| {
                let bytes = src.tile(k)?.unwrap_or_default();
                let mut out = Vec::new();
                if let Some((_, feats)) = decode_layer(&bytes, k, "landuse")? {
                    for f in feats {
                        let kind = prop(&f.props, "kind").and_then(Val::as_str).unwrap_or("").to_string();
                        if !feeds_building_fallback(&kind) {
                            continue;
                        }
                        let id = osm_id(&f.props, f.fid);
                        if let FGeom::Polys(m) = f.geom {
                            for p in m {
                                let area = geom::poly_area(&p);
                                out.push((Rect::of_mpoly(&vec![p.clone()]), area, id, kind.clone(), p));
                            }
                        }
                    }
                }
                Ok(out)
            })
            .collect::<Result<_, String>>()?;
        let polys: Vec<_> = per_tile.into_iter().flatten().collect();
        let mut cells: HashMap<(i64, i64), Vec<u32>> = HashMap::new();
        for (i, p) in polys.iter().enumerate() {
            let r = p.0;
            for cy in (r.y0 / LU_CELL).floor() as i64..=(r.y1 / LU_CELL).floor() as i64 {
                for cx in (r.x0 / LU_CELL).floor() as i64..=(r.x1 / LU_CELL).floor() as i64 {
                    cells.entry((cx, cy)).or_default().push(i as u32);
                }
            }
        }
        Ok(Self { polys, cells })
    }

    /// Smallest containing polygon (most specific), ties by id, as the runtime's index.
    pub fn lookup(&self, p: Pt) -> Option<&str> {
        let cell = ((p[0] / LU_CELL).floor() as i64, (p[1] / LU_CELL).floor() as i64);
        let mut best: Option<usize> = None;
        for &i in self.cells.get(&cell)? {
            let lp = &self.polys[i as usize];
            if !lp.0.contains(p) {
                continue;
            }
            if let Some(b) = best {
                let cur = &self.polys[b];
                if (lp.1, lp.2) >= (cur.1, cur.2) {
                    continue;
                }
            }
            if geom::point_in_poly(p, &lp.4) {
                best = Some(i as usize);
            }
        }
        best.map(|i| self.polys[i].3.as_str())
    }
}

/// `mesher::resolve`'s height part: (resolved roof, roof from tagged values only).
pub fn resolve_height(props: &Props, kind: &str, landuse: Option<&str>) -> (f32, Option<f32>) {
    const FLOOR_H: f32 = simworld_core::mesher::FLOOR_H_DEFAULT;
    let num = |k| prop(props, k).and_then(Val::as_f64).map(|v| v as f32);
    let min_h = num("min_height").filter(|v| v.is_finite()).unwrap_or(0.0).clamp(0.0, simworld_core::mesher::MAX_HEIGHT_M);
    let h = num("height").filter(|h| h.is_finite() && *h > 0.0);
    let l = num("levels").filter(|l| l.is_finite() && *l > 0.0);
    let mut height = match (h, l) {
        (Some(h), _) => h,
        (None, Some(l)) => l * FLOOR_H,
        (None, None) => simworld_core::mesher::fallback_levels(kind, landuse.map(Landuse::parse)) * FLOOR_H,
    };
    if h.is_none() && height <= min_h {
        height += min_h;
    }
    let height = height.min(simworld_core::mesher::MAX_HEIGHT_M);
    (height, (h.is_some() || l.is_some()).then_some(height))
}

/// Every building of the archive's `zoom` tiles, stitched into whole footprints: a copy the encoder
/// did not cut is taken as is, otherwise the pieces of all tiles are unioned (they overlap in the
/// tile buffers and share the same z16 integer grid). Sorted by (osm_type, id).
pub fn buildings(src: &Archive, zoom: u8, landuse: &LanduseIndex) -> Result<Vec<Building>, String> {
    let pieces: Vec<Vec<Piece>> = src
        .keys_at(zoom)
        .par_iter()
        .map(|&k| {
            let bytes = src.tile(k)?.unwrap_or_default();
            let Some((extent, feats)) = decode_layer(&bytes, k, "buildings")? else { return Ok(Vec::new()) };
            let span = tile_span_g(k.0);
            let buf = span * 4.0 / 256.0; // planetiler `buffer: 4` of 256
            let s = span / extent as f64;
            let inner = Rect::tile(k.0, k.1, k.2, buf - s);
            let mut out = Vec::with_capacity(feats.len());
            for f in feats {
                let FGeom::Polys(polys) = f.geom else { continue };
                let polys: MPoly = polys.into_iter().filter(|p| !p.is_empty() && p[0].len() >= 3).collect();
                if polys.is_empty() {
                    continue;
                }
                let clipped = polys.iter().flatten().flatten().any(|p| !(p[0] > inner.x0 && p[0] < inner.x1 && p[1] > inner.y0 && p[1] < inner.y1));
                out.push(Piece { key: (osm_type_code(&f.props, f.fid), osm_id(&f.props, f.fid)), props: f.props, polys, clipped });
            }
            Ok(out)
        })
        .collect::<Result<_, String>>()?;
    let mut groups: HashMap<(u8, u64), Vec<Piece>> = HashMap::new();
    for p in pieces.into_iter().flatten() {
        groups.entry(p.key).or_default().push(p);
    }
    let mut groups: Vec<((u8, u64), Vec<Piece>)> = groups.into_iter().collect();
    groups.sort_by_key(|g| g.0);
    let out: Vec<Building> = groups
        .into_par_iter()
        .filter_map(|(key, mut ps)| {
            let polys = match ps.iter().position(|p| !p.clipped) {
                Some(i) => std::mem::take(&mut ps[i].polys),
                None if ps.len() == 1 => std::mem::take(&mut ps[0].polys),
                None => geom::union(&ps.iter().map(|p| p.polys.clone()).collect::<Vec<_>>()),
            };
            let c = geom::centroid(&polys)?;
            let mpg = geom::m_per_g(c[1]);
            let area_m2 = mpoly_area(&polys) * mpg * mpg;
            let props = std::mem::take(&mut ps[0].props);
            let kind = prop(&props, "kind").and_then(Val::as_str).unwrap_or("yes").to_string();
            let lu = landuse.lookup(c).map(str::to_string);
            let (roof_m, tagged_roof_m) = resolve_height(&props, &kind, lu.as_deref());
            Some(Building { key, props, polys, area_m2, centroid: c, tagged_roof_m, roof_m, kind, landuse: lu })
        })
        .collect();
    Ok(out)
}

/// Every feature of `layer` in the archive's `zoom` tiles, each clipped to its own tile rectangle
/// without buffer, so pieces of one feature in neighbouring tiles meet edge to edge.
pub fn exact_pieces(src: &Archive, zoom: u8, layer: &str) -> Result<Vec<Feature>, String> {
    let per: Vec<Vec<Feature>> = src
        .keys_at(zoom)
        .par_iter()
        .map(|&k| {
            let bytes = src.tile(k)?.unwrap_or_default();
            let Some((_, feats)) = decode_layer(&bytes, k, layer)? else { return Ok(Vec::new()) };
            let r = Rect::tile(k.0, k.1, k.2, 0.0);
            Ok(feats
                .into_iter()
                .filter_map(|f| {
                    let geom = match f.geom {
                        FGeom::Polys(m) => {
                            let c = geom::clip_mpoly(&m, &r);
                            (!c.is_empty()).then_some(FGeom::Polys(c))?
                        }
                        FGeom::Lines(ls) => {
                            let c: Vec<Ring> = ls.iter().flat_map(|l| geom::clip_line(l, &r)).collect();
                            (!c.is_empty()).then_some(FGeom::Lines(c))?
                        }
                        FGeom::Points(ps) => FGeom::Points(ps.into_iter().filter(|p| p[0] >= r.x0 && p[0] < r.x1 && p[1] >= r.y0 && p[1] < r.y1).collect()),
                    };
                    Some(Feature { fid: f.fid, props: f.props, geom })
                })
                .collect())
        })
        .collect::<Result<_, String>>()?;
    Ok(per.into_iter().flatten().collect())
}
