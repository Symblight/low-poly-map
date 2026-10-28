//! `seed water`: build the low-poly water file from GeoJSON polygons (docs/spec-world-seed.md
//! sections 3.2, 4.4): clip to the bbox, Douglas-Peucker at the tolerance, drop slivers, fix the
//! ring orientation, quantize to 1e-7 degrees, sort.
//!
//! Inputs are GeoJSON (Polygon / MultiPolygon features), so the tool needs no GIS library:
//! * `--sea F`: water polygons that are sea (osmdata.openstreetmap.de `water-polygons-split-4326`,
//!   converted with `ogr2ogr -f GeoJSON -spat W S E N sea.geojson water_polygons.shp`).
//! * `--land F`: land polygons instead (`land-polygons-complete-4326`): the sea is the bbox minus
//!   them, one polygon without the split seams of the water polygons.
//! * `--lakes F`: inland water from the Geofabrik extract (`ogr2ogr -f GeoJSON lakes.geojson
//!   extract.osm.pbf multipolygons -where "natural='water' OR waterway='riverbank' OR landuse='reservoir'"`);
//!   features tagged as a river (`water=river|canal|stream|oxbow`, `waterway=riverbank|river|canal`,
//!   in properties or in GDAL's `other_tags`) become `river`, the rest `lake`.
//! * `--rivers F`: everything in the file is `river`.

use simworld_core::seed::files::{clean_ring, Outside, WaterFile, WaterKind, WaterPolygon};
use simworld_core::seed::geom::{clip_ring, signed_area, signed_area_m2, simplify_ring};

use serde_json::Value;

/// Options of one build.
#[derive(Clone, Debug)]
pub struct WaterOptions {
    pub bbox: [f64; 4],
    pub tolerance_m: f64,
    pub min_area_m2: f64,
    pub outside: Outside,
    pub source_date: Option<String>,
}

/// One input file and how to read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    Sea,
    Land,
    Lakes,
    Rivers,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WaterReport {
    pub features_in: usize,
    pub polygons_out: [usize; 3],
    pub points_out: usize,
    pub dropped_small: usize,
    pub dropped_outside: usize,
    pub holes_dropped: usize,
}

type Poly = Vec<Vec<[f64; 2]>>;

/// Polygons (rings of [lon, lat]) of a GeoJSON text, with each feature's properties.
pub fn read_geojson(text: &str, label: &str) -> Result<Vec<(Poly, Value)>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("{label}: invalid JSON: {e}"))?;
    let mut out = Vec::new();
    fn ring(v: &Value) -> Option<Vec<[f64; 2]>> {
        v.as_array()?.iter().map(|p| Some([p.get(0)?.as_f64()?, p.get(1)?.as_f64()?])).collect()
    }
    fn poly(v: &Value) -> Option<Poly> {
        v.as_array()?.iter().map(ring).collect()
    }
    fn geom(g: &Value, props: &Value, out: &mut Vec<(Poly, Value)>, label: &str) -> Result<(), String> {
        let coords = g.get("coordinates").unwrap_or(&Value::Null);
        match g.get("type").and_then(Value::as_str) {
            Some("Polygon") => out.push((poly(coords).ok_or(format!("{label}: bad Polygon coordinates"))?, props.clone())),
            Some("MultiPolygon") => {
                for p in coords.as_array().ok_or(format!("{label}: bad MultiPolygon"))? {
                    out.push((poly(p).ok_or(format!("{label}: bad MultiPolygon coordinates"))?, props.clone()));
                }
            }
            Some("GeometryCollection") => {
                for x in g.get("geometries").and_then(Value::as_array).into_iter().flatten() {
                    geom(x, props, out, label)?;
                }
            }
            _ => {} // points and lines are not water areas
        }
        Ok(())
    }
    match v.get("type").and_then(Value::as_str) {
        Some("FeatureCollection") => {
            for f in v.get("features").and_then(Value::as_array).into_iter().flatten() {
                if let Some(g) = f.get("geometry").filter(|g| !g.is_null()) {
                    geom(g, f.get("properties").unwrap_or(&Value::Null), &mut out, label)?;
                }
            }
        }
        Some("Feature") => {
            if let Some(g) = v.get("geometry") {
                geom(g, v.get("properties").unwrap_or(&Value::Null), &mut out, label)?;
            }
        }
        Some(_) => geom(&v, &Value::Null, &mut out, label)?,
        None => return Err(format!("{label}: not GeoJSON (no `type`)")),
    }
    Ok(out)
}

/// River or lake from OSM tags (plain properties or GDAL's `other_tags` hstore string).
pub fn is_river(props: &Value) -> bool {
    let tag = |k: &str| -> Option<String> {
        if let Some(s) = props.get(k).and_then(Value::as_str) {
            return Some(s.to_string());
        }
        let other = props.get("other_tags").and_then(Value::as_str)?;
        let needle = format!("\"{k}\"=>\"");
        let i = other.find(&needle)? + needle.len();
        Some(other[i..].split('"').next()?.to_string())
    };
    if tag("kind").as_deref() == Some("river") {
        return true;
    }
    matches!(tag("water").as_deref(), Some("river" | "canal" | "stream" | "oxbow" | "riverbank" | "ditch"))
        || matches!(tag("waterway").as_deref(), Some("riverbank" | "river" | "canal" | "dock"))
}

/// Build the water file. `inputs` = (kind, GeoJSON text, label).
pub fn build(inputs: &[(InputKind, String, String)], o: &WaterOptions) -> Result<(WaterFile, WaterReport), String> {
    let [w, s, e, n] = o.bbox;
    if !(w < e && s < n) {
        return Err(format!("--bbox {:?}: need west < east and south < north", o.bbox));
    }
    let bbox_ring = vec![[w, s], [e, s], [e, n], [w, n]];
    let rect = [w, s, e, n];
    let mut rep = WaterReport::default();
    let mut raw: Vec<(WaterKind, Poly)> = Vec::new();
    let mut land_holes: Vec<Vec<[f64; 2]>> = Vec::new();
    for (kind, text, label) in inputs {
        for (poly, props) in read_geojson(text, label)? {
            rep.features_in += 1;
            match kind {
                InputKind::Land => {
                    // The land's outer ring is a hole of the sea; its holes are sea again.
                    let mut it = poly.into_iter();
                    if let Some(outer) = it.next() {
                        land_holes.push(outer);
                    }
                    for h in it {
                        raw.push((WaterKind::Sea, vec![h]));
                    }
                }
                InputKind::Sea => raw.push((WaterKind::Sea, poly)),
                InputKind::Rivers => raw.push((WaterKind::River, poly)),
                InputKind::Lakes => raw.push((if is_river(&props) { WaterKind::River } else { WaterKind::Lake }, poly)),
            }
        }
    }
    if inputs.iter().any(|(k, _, _)| *k == InputKind::Land) {
        let mut sea = vec![bbox_ring.clone()];
        sea.extend(land_holes);
        raw.push((WaterKind::Sea, sea));
    }
    let mut polys: Vec<WaterPolygon> = Vec::new();
    for (kind, poly) in raw {
        let mut rings: Vec<Vec<[f64; 2]>> = Vec::new();
        for (j, r) in poly.iter().enumerate() {
            let outer = j == 0;
            let mut r = r.clone();
            if r.len() > 1 && r.first() == r.last() {
                r.pop();
            }
            let c = clip_ring(&r, rect);
            if c.is_empty() {
                if outer {
                    rep.dropped_outside += 1;
                    break;
                }
                continue;
            }
            let simp = simplify_ring(&c, o.tolerance_m);
            let q = clean_ring(&simp).filter(|q| signed_area_m2(q).abs() >= o.min_area_m2);
            let Some(mut q) = q else {
                if outer {
                    rep.dropped_small += 1;
                    break;
                }
                rep.holes_dropped += 1;
                continue;
            };
            // Outer counter-clockwise, holes clockwise.
            if (signed_area(&q) > 0.0) != outer {
                q.reverse();
            }
            rings.push(q);
        }
        if rings.is_empty() {
            continue;
        }
        polys.push(WaterPolygon { kind, rings });
    }
    // Deterministic order: kind, then the outer ring's lowest point, then size.
    polys.sort_by(|a, b| {
        let key = |p: &WaterPolygon| {
            let lo = p.rings[0].iter().fold([f64::INFINITY; 2], |m, v| [m[0].min(v[0]), m[1].min(v[1])]);
            (p.kind, lo, p.rings[0].len())
        };
        let (ka, kb) = (key(a), key(b));
        ka.0.cmp(&kb.0).then(ka.1[1].total_cmp(&kb.1[1])).then(ka.1[0].total_cmp(&kb.1[0])).then(ka.2.cmp(&kb.2))
    });
    for p in &polys {
        rep.polygons_out[p.kind as usize] += 1;
        rep.points_out += p.rings.iter().map(Vec::len).sum::<usize>();
    }
    let file = WaterFile {
        format: 1,
        source: "OpenStreetMap".into(),
        source_date: o.source_date.clone(),
        bbox: o.bbox,
        outside: o.outside,
        tolerance_m: o.tolerance_m,
        min_area_m2: Some(o.min_area_m2),
        polygons: polys,
    };
    Ok((file, rep))
}

/// JSON text of a water file: header keys, then one polygon per line (diffable, compact).
pub fn to_json(f: &WaterFile) -> String {
    let mut head = serde_json::to_value(f).unwrap_or(Value::Null);
    if let Value::Object(m) = &mut head {
        m.remove("polygons");
    }
    let mut s = serde_json::to_string(&head).unwrap_or_default();
    s.pop(); // '}'
    s.push_str(",\n\"polygons\": [\n");
    for (i, p) in f.polygons.iter().enumerate() {
        s.push_str(&serde_json::to_string(p).unwrap_or_default());
        s.push_str(if i + 1 < f.polygons.len() { ",\n" } else { "\n" });
    }
    s.push_str("]}\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEA: &str = include_str!("../../testdata/water/sea.geojson");
    const LAND: &str = include_str!("../../testdata/water/land.geojson");
    const LAKES: &str = include_str!("../../testdata/water/lakes.geojson");

    fn opts() -> WaterOptions {
        WaterOptions { bbox: [20.0, 54.0, 22.0, 55.0], tolerance_m: 50.0, min_area_m2: 20_000.0, outside: Outside::Sea, source_date: None }
    }

    #[test]
    fn sea_and_lakes_to_water_file() {
        let inputs = vec![(InputKind::Sea, SEA.to_string(), "sea".to_string()), (InputKind::Lakes, LAKES.to_string(), "lakes".to_string())];
        let (f, rep) = build(&inputs, &opts()).unwrap();
        // sea, big lake (with island hole), river (from other_tags), the pond is a sliver.
        assert_eq!(rep.polygons_out, [1, 1, 1], "{rep:?}");
        assert_eq!(rep.dropped_small, 1);
        assert_eq!(rep.dropped_outside, 1);
        let lake = f.polygons.iter().find(|p| p.kind == WaterKind::Lake).unwrap();
        assert_eq!(lake.rings.len(), 2);
        assert!(signed_area(&lake.rings[0]) > 0.0 && signed_area(&lake.rings[1]) < 0.0, "orientation");
        // The sea was clipped to the bbox.
        for p in f.polygons.iter().flat_map(|p| p.rings.iter().flatten()) {
            assert!(p[0] >= 20.0 && p[0] <= 22.0 && p[1] >= 54.0 && p[1] <= 55.0, "{p:?}");
        }
        // DP 50 m dropped the coast's small (26 m) wiggles: fewer points than the 200 + 3 input.
        let sea = &f.polygons[0];
        assert!(sea.rings[0].len() < 150, "{}", sea.rings[0].len());
        // Text round trip through the core's loader; deterministic.
        let json = to_json(&f);
        let back = simworld_core::seed::files::parse_water("water.json", &json).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(back.polygons.len(), 3);
        assert_eq!(to_json(&build(&inputs, &opts()).unwrap().0), json);
    }

    #[test]
    fn land_polygons_make_one_sea() {
        let inputs = vec![(InputKind::Land, LAND.to_string(), "land".to_string())];
        let (f, rep) = build(&inputs, &opts()).unwrap();
        assert_eq!(rep.polygons_out, [1, 0, 0], "{rep:?}");
        let sea = &f.polygons[0];
        assert_eq!(sea.rings.len(), 3, "bbox + mainland hole + island hole");
        let idx = simworld_core::seed::water::WaterIndex::build(&f, Default::default(), &|_, _| 0.0);
        use simworld_core::seed::water::WaterHit;
        assert_eq!(idx.water_at(54.5, 20.2), WaterHit::Body(0), "open sea");
        assert_eq!(idx.water_at(54.5, 21.5), WaterHit::Land, "mainland");
        assert_eq!(idx.water_at(54.505, 20.5), WaterHit::Land, "island");
    }

    #[test]
    fn river_tags() {
        assert!(is_river(&serde_json::json!({ "other_tags": "\"name\"=>\"Nemunas\",\"water\"=>\"river\"" })));
        assert!(is_river(&serde_json::json!({ "waterway": "riverbank" })));
        assert!(!is_river(&serde_json::json!({ "natural": "water", "other_tags": "\"water\"=>\"lake\"" })));
    }
}
