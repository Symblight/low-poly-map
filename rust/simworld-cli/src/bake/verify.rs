//! `bake --verify`: structural checks of a two-level archive.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rayon::prelude::*;

use super::archive::{Archive, Key};
use super::geom::{self, m_per_g, Rect};
use super::input::{self, prop, Building, FGeom};
use super::levels::Params;
use super::mvt_enc::Val;

const DETAIL_LAYERS: [&str; 3] = ["buildings", "landuse", "roads"];
const OVERVIEW_LAYERS: [&str; 5] = ["blocks", "landmarks", "lights", "roads", "landuse"];

#[derive(Default)]
struct TileFacts {
    errors: Vec<String>,
    /// (key, area m², quantisation tolerance m²)
    detail: Vec<((u8, u64), f64, f64)>,
    landmarks: Vec<(u8, u64)>,
    blocks: Vec<(u64, f64, f64)>,
    lights: Vec<(u8, u64)>,
}

fn num(p: &input::Props, k: &str) -> Option<f64> {
    prop(p, k).and_then(Val::as_f64)
}

fn is_step(v: f64, step: f64) -> bool {
    v >= step - 1e-6 && ((v / step) - (v / step).round()).abs() < 1e-6
}

fn check_tile(a: &Archive, k: Key, p: &Params) -> TileFacts {
    let mut f = TileFacts::default();
    let bytes = match a.tile(k) {
        Ok(Some(b)) => b,
        Ok(None) => return f,
        Err(e) => {
            f.errors.push(format!("{k:?}: {e}"));
            return f;
        }
    };
    let layers = match mvt_reader::Reader::new(bytes.clone()).and_then(|r| r.get_layer_metadata()) {
        Ok(l) => l,
        Err(e) => {
            f.errors.push(format!("{k:?}: mvt: {e:?}"));
            return f;
        }
    };
    let detail = k.0 == p.detail_z;
    let allowed: &[&str] = if detail { &DETAIL_LAYERS } else { &OVERVIEW_LAYERS };
    for l in &layers {
        if !allowed.contains(&l.name.as_str()) {
            f.errors.push(format!("{k:?}: unexpected layer `{}`", l.name));
        }
        let want = match (detail, l.name.as_str()) {
            (true, _) => p.detail_extent,
            (false, "landmarks") => p.landmark_extent,
            (false, _) => p.overview_extent,
        };
        if l.extent != want {
            f.errors.push(format!("{k:?}: layer `{}` extent {} (want {want})", l.name, l.extent));
        }
    }
    let exact = Rect::tile(k.0, k.1, k.2, 0.0);
    // Area inside the tile proper, and how far rounding to the tile grid can have moved it:
    // every vertex moves by at most half a unit per axis, so the area by at most
    // perimeter x unit / sqrt(2).
    let area_m2 = |m: &geom::MPoly, extent: u32| {
        let c = geom::clip_mpoly(m, &exact);
        let s = m_per_g((exact.y0 + exact.y1) / 2.0);
        let unit_m = (exact.x1 - exact.x0) / extent as f64 * s;
        let perim: f64 = c.iter().flatten().map(|r| (0..r.len()).map(|i| {
            let (a, b) = (r[i], r[(i + 1) % r.len()]);
            ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
        }).sum::<f64>()).sum::<f64>() * s;
        (geom::mpoly_area(&c) * s * s, perim * unit_m * std::f64::consts::FRAC_1_SQRT_2)
    };
    for name in allowed {
        let feats = match input::decode_layer(&bytes, k, name) {
            Ok(Some((_, v))) => v,
            Ok(None) => continue,
            Err(e) => {
                f.errors.push(e);
                continue;
            }
        };
        for ft in feats {
            let key = (input::osm_type_code(&ft.props, ft.fid), input::osm_id(&ft.props, ft.fid));
            let kind = prop(&ft.props, "kind").and_then(Val::as_str).unwrap_or("yes").to_string();
            match (detail, *name, &ft.geom) {
                (true, "buildings", FGeom::Polys(m)) => {
                    if let (_, Some(h)) = input::resolve_height(&ft.props, &kind, None) {
                        if h as f64 >= p.landmark_m {
                            f.errors.push(format!("{k:?}: detail building {key:?} has a {h} m roof (landmark)"));
                        }
                    }
                    let (ar, tol) = area_m2(m, p.detail_extent);
                    f.detail.push((key, ar, tol));
                }
                (false, "landmarks", FGeom::Polys(_)) => {
                    match input::resolve_height(&ft.props, &kind, None).1 {
                        Some(h) if h as f64 >= p.landmark_m => {}
                        h => f.errors.push(format!("{k:?}: landmark {key:?} roof {h:?} below {} m", p.landmark_m)),
                    }
                    f.landmarks.push(key);
                }
                (false, "blocks", FGeom::Polys(m)) => {
                    match num(&ft.props, "height") {
                        Some(h) if is_step(h, p.height_step_m) => {}
                        h => f.errors.push(format!("{k:?}: block {} height {h:?} not a {} m step", key.1, p.height_step_m)),
                    }
                    let (ar, tol) = area_m2(m, p.overview_extent);
                    f.blocks.push((key.1, ar, tol));
                }
                (false, "lights", FGeom::Points(pts)) => {
                    if pts.len() != 1 {
                        f.errors.push(format!("{k:?}: light {key:?} has {} points", pts.len()));
                    }
                    if !pts.iter().all(|q| exact.contains(*q)) {
                        f.errors.push(format!("{k:?}: light {key:?} outside its tile"));
                    }
                    if let Some(h) = num(&ft.props, "height") {
                        if !is_step(h, p.height_step_m) {
                            f.errors.push(format!("{k:?}: light {key:?} height {h} not a {} m step", p.height_step_m));
                        }
                    }
                    f.lights.push(key);
                }
                (_, "roads", FGeom::Lines(_)) | (_, "landuse", FGeom::Polys(_)) => {}
                (_, n, _) => f.errors.push(format!("{k:?}: layer `{n}` has an unexpected geometry type")),
            }
        }
    }
    f
}

/// Summary numbers of a verified archive.
#[derive(Debug, Default)]
pub struct Summary {
    pub detail_ids: usize,
    pub landmark_ids: usize,
    pub blocks: usize,
    pub lights: usize,
}

/// Checks the archive; with `source` (the source buildings) also that every source building is in
/// exactly one level (or dropped from detail by the minimum area) and has exactly one light.
pub fn verify(a: &Archive, source: Option<&[Building]>) -> Result<Summary, Vec<String>> {
    let mut errors = Vec::new();
    let m = &a.metadata;
    let p = match params_from_metadata(m) {
        Ok(p) => p,
        Err(e) => return Err(vec![e]),
    };
    if m.get("simworld_levels") != Some(&serde_json::json!({"detail": p.detail_z, "overview": p.overview_z})) {
        errors.push(format!("metadata simworld_levels is {:?}", m.get("simworld_levels")));
    }
    if !m.get("attribution").and_then(|v| v.as_str()).is_some_and(|s| s.contains("© OpenStreetMap contributors")) {
        errors.push("metadata attribution lacks \"© OpenStreetMap contributors\"".into());
    }
    let keys: Vec<Key> = a.keys().collect();
    for k in &keys {
        if k.0 != p.detail_z && k.0 != p.overview_z {
            errors.push(format!("tile {k:?} outside the two levels"));
        }
    }
    let facts: Vec<TileFacts> = keys.par_iter().filter(|k| k.0 == p.detail_z || k.0 == p.overview_z).map(|&k| check_tile(a, k, &p)).collect();
    let mut detail: HashMap<(u8, u64), (f64, f64)> = HashMap::new();
    let mut landmarks: BTreeSet<(u8, u64)> = BTreeSet::new();
    let mut blocks: HashMap<u64, (f64, f64)> = HashMap::new();
    let mut lights: HashMap<(u8, u64), usize> = HashMap::new();
    for f in facts {
        errors.extend(f.errors);
        for (k, ar, tol) in f.detail {
            let e = detail.entry(k).or_default();
            e.0 += ar;
            e.1 += tol;
        }
        landmarks.extend(f.landmarks);
        for (k, ar, tol) in f.blocks {
            let e = blocks.entry(k).or_default();
            e.0 += ar;
            e.1 += tol;
        }
        for k in f.lights {
            *lights.entry(k).or_default() += 1;
        }
    }
    let both: Vec<_> = landmarks.iter().filter(|k| detail.contains_key(k)).collect();
    if !both.is_empty() {
        errors.push(format!("{} buildings in both detail and landmarks, e.g. {:?}", both.len(), &both[..both.len().min(5)]));
    }
    let small: BTreeMap<_, _> = detail.iter().filter(|(_, a)| a.0 + a.1 < p.detail_min_area_m2).collect();
    if !small.is_empty() {
        errors.push(format!("{} detail footprints below {} m², e.g. {:?}", small.len(), p.detail_min_area_m2, small.iter().take(5).collect::<Vec<_>>()));
    }
    let small: BTreeMap<_, _> = blocks.iter().filter(|(_, a)| a.0 + a.1 < p.block_min_area_m2).collect();
    if !small.is_empty() {
        errors.push(format!("{} blocks below {} m², e.g. {:?}", small.len(), p.block_min_area_m2, small.iter().take(5).collect::<Vec<_>>()));
    }
    let multi: Vec<_> = lights.iter().filter(|(_, n)| **n != 1).collect();
    if !multi.is_empty() {
        errors.push(format!("{} buildings with more than one light point, e.g. {:?}", multi.len(), &multi[..multi.len().min(5)]));
    }
    if let Some(src) = source {
        let mut missing = Vec::new();
        let mut wrong = Vec::new();
        for b in src {
            let lm = super::levels::is_landmark(b, &p);
            let (in_d, in_l) = (detail.contains_key(&b.key), landmarks.contains(&b.key));
            let ok = match (lm, in_d, in_l) {
                (true, false, true) => true,
                (false, true, false) => super::levels::detail_footprint(b, &p).is_some(),
                (false, false, false) => super::levels::detail_footprint(b, &p).is_none(),
                _ => false,
            };
            if !ok {
                wrong.push((b.key, lm, in_d, in_l, b.area_m2));
            }
            if lights.get(&b.key) != Some(&1) {
                missing.push(b.key);
            }
        }
        let src_keys: BTreeSet<(u8, u64)> = src.iter().map(|b| b.key).collect();
        let extra = detail.keys().chain(landmarks.iter()).chain(lights.keys()).filter(|k| !src_keys.contains(k)).count();
        if !wrong.is_empty() {
            errors.push(format!("{} source buildings not in exactly one level, e.g. (key, landmark, in detail, in landmarks, m²) {:?}", wrong.len(), &wrong[..wrong.len().min(5)]));
        }
        if !missing.is_empty() {
            errors.push(format!("{} source buildings without exactly one light point, e.g. {:?}", missing.len(), &missing[..missing.len().min(5)]));
        }
        if extra > 0 {
            errors.push(format!("{extra} building ids in the output that are not in the source"));
        }
    }
    if errors.is_empty() {
        Ok(Summary { detail_ids: detail.len(), landmark_ids: landmarks.len(), blocks: blocks.len(), lights: lights.len() })
    } else {
        Err(errors)
    }
}

/// The bake parameters recorded in the metadata (`simworld_bake`).
pub fn params_from_metadata(m: &serde_json::Value) -> Result<Params, String> {
    let b = m.get("simworld_bake").ok_or("metadata has no simworld_bake")?;
    let f = |path: &[&str]| -> Result<f64, String> {
        let mut v = b;
        for k in path {
            v = v.get(k).ok_or_else(|| format!("simworld_bake.{} missing", path.join(".")))?;
        }
        v.as_f64().ok_or_else(|| format!("simworld_bake.{} not a number", path.join(".")))
    };
    Ok(Params {
        detail_z: f(&["detail", "zoom"])? as u8,
        detail_extent: f(&["detail", "extent"])? as u32,
        detail_min_area_m2: f(&["detail", "min_area_m2"])?,
        overview_z: f(&["overview", "zoom"])? as u8,
        overview_extent: f(&["overview", "extent"])? as u32,
        landmark_extent: f(&["overview", "landmark_extent"])? as u32,
        block_min_area_m2: f(&["overview", "block_min_area_m2"])?,
        height_step_m: f(&["overview", "height_step_m"])?,
        landmark_m: f(&["landmark_m"])?,
        ..Params::default()
    })
}
