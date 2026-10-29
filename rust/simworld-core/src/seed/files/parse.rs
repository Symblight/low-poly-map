//! Parsing: schema check of the text, then the typed form of each file kind.

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::Value;

use super::{clean_ring, q7, AirportFile, City, Manifest, RoadsFile, WaterFile, SCHEMAS};
use crate::seed::schema;

/// Which schema a file is checked against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    World,
    City,
    Airports,
    Water,
    Roads,
}

impl FileKind {
    fn schema_name(self) -> &'static str {
        match self {
            FileKind::World => "world.schema.json",
            FileKind::City => "city.schema.json",
            FileKind::Airports => "airports.schema.json",
            FileKind::Water => "water.schema.json",
            FileKind::Roads => "roads.schema.json",
        }
    }
}

/// The parsed seed schemas (once per call; they are small).
pub fn schemas() -> Vec<(&'static str, Value)> {
    SCHEMAS.iter().map(|(n, t)| (*n, serde_json::from_str(t).unwrap_or_else(|e| panic!("{n}: {e}")))).collect()
}

/// JSON integers written as `3.0` -> `3`, so typed integer fields accept what the schema accepts.
fn normalize_integers(v: &mut Value) {
    match v {
        Value::Number(n) => {
            if let Some(f) = n.as_f64().filter(|f| n.is_f64() && f.fract() == 0.0 && f.abs() < 9.0e15) {
                *v = Value::from(f as i64);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(normalize_integers),
        Value::Object(o) => o.values_mut().for_each(normalize_integers),
        _ => {}
    }
}

/// Text -> checked JSON value (syntax, schema). Errors: `<label>: <path>: <problem>`.
pub fn check_text(kind: FileKind, label: &str, text: &str) -> Result<Value, Vec<String>> {
    let mut v: Value = serde_json::from_str(text).map_err(|e| vec![format!("{label}: invalid JSON: {e}")])?;
    let schemas = schemas();
    let reg: Vec<(&str, &Value)> = schemas.iter().map(|(n, v)| (*n, v)).collect();
    let schema = reg.iter().find(|(n, _)| *n == kind.schema_name()).map(|(_, s)| *s).expect("schema");
    let mut errs = Vec::new();
    schema::validate(&reg, schema, &v, &mut errs, 25);
    if !errs.is_empty() {
        return Err(errs.into_iter().map(|e| format!("{label}: {e}")).collect());
    }
    normalize_integers(&mut v);
    Ok(v)
}

fn typed<T: for<'de> Deserialize<'de>>(label: &str, v: Value) -> Result<T, Vec<String>> {
    serde_json::from_value(v).map_err(|e| vec![format!("{label}: {e}")])
}

pub fn parse_manifest(label: &str, text: &str) -> Result<Manifest, Vec<String>> {
    let mut m: Manifest = typed(label, check_text(FileKind::World, label, text)?)?;
    for o in m.airports.overrides.values_mut() {
        o.lat = o.lat.map(q7);
        o.lon = o.lon.map(q7);
        for r in o.runways.values_mut() {
            for v in [&mut r.le_lat, &mut r.le_lon, &mut r.he_lat, &mut r.he_lon].into_iter().flatten() {
                *v = q7(*v);
            }
        }
        for r in &mut o.add {
            r.quantize();
        }
    }
    Ok(m)
}

pub fn parse_city(label: &str, text: &str) -> Result<City, Vec<String>> {
    let mut c: City = typed(label, check_text(FileKind::City, label, text)?)?;
    c.quantize();
    Ok(c)
}

pub fn parse_airports(label: &str, text: &str) -> Result<AirportFile, Vec<String>> {
    let mut f: AirportFile = typed(label, check_text(FileKind::Airports, label, text)?)?;
    let mut errs = Vec::new();
    let mut seen = BTreeSet::new();
    for a in &mut f.airports {
        a.lat = q7(a.lat);
        a.lon = q7(a.lon);
        if !seen.insert(a.ident.clone()) {
            errs.push(format!("{label}: airports: duplicate ident {}", a.ident));
        }
        let mut keys = BTreeSet::new();
        for r in &mut a.runways {
            r.quantize();
            if !keys.insert(r.key()) {
                errs.push(format!("{label}: airports.{}: duplicate runway {}", a.ident, r.key()));
            }
        }
    }
    if errs.is_empty() {
        Ok(f)
    } else {
        Err(errs)
    }
}

pub fn parse_water(label: &str, text: &str) -> Result<WaterFile, Vec<String>> {
    let mut f: WaterFile = typed(label, check_text(FileKind::Water, label, text)?)?;
    let mut errs = Vec::new();
    let [w, s, e, n] = f.bbox;
    if !(w < e && s < n) {
        errs.push(format!("{label}: bbox: {:?} is not [west, south, east, north] with west < east and south < north", f.bbox));
    }
    f.bbox = [q7(w), q7(s), q7(e), q7(n)];
    for (i, p) in f.polygons.iter_mut().enumerate() {
        let mut rings = Vec::with_capacity(p.rings.len());
        for (j, r) in p.rings.iter().enumerate() {
            match clean_ring(r) {
                Some(c) => rings.push(c),
                None if j == 0 => errs.push(format!("{label}: polygons[{i}].rings[0]: fewer than 3 distinct points after quantization")),
                None => {} // a degenerate hole is dropped
            }
        }
        p.rings = rings;
    }
    if errs.is_empty() {
        Ok(f)
    } else {
        Err(errs)
    }
}

pub fn parse_roads(label: &str, text: &str) -> Result<RoadsFile, Vec<String>> {
    let mut f: RoadsFile = typed(label, check_text(FileKind::Roads, label, text)?)?;
    let mut errs = Vec::new();
    let mut ids = BTreeSet::new();
    for (i, r) in f.roads.iter_mut().enumerate() {
        if !ids.insert(r.id.clone()) {
            errs.push(format!("{label}: roads[{i}].id: duplicate road id \"{}\"", r.id));
        }
        for p in &mut r.points {
            *p = [q7(p[0]), q7(p[1])];
        }
        r.points.dedup();
        if r.points.len() < 2 {
            errs.push(format!("{label}: roads[{i}].points: road \"{}\" has fewer than 2 distinct points", r.id));
        }
    }
    if errs.is_empty() {
        Ok(f)
    } else {
        Err(errs)
    }
}
