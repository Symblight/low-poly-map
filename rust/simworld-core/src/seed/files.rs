//! Seed world files (`docs/spec-world-seed.md` section 4): typed forms of `world.json`,
//! `cities/<id>.json`, `airports.json`, `water.json` and `roads.json`, their loaders, and the
//! load-time validation of section 4.6 (the same checks `simworld-cli seed validate` runs).
//!
//! Every file is checked against its JSON Schema in `addons/simworld/seed/` first (errors name the
//! file, the key path and the value), then parsed into the typed form and quantized (coordinates
//! to 1e-7 degrees). The core never touches a file system here: the host hands it text through a
//! reader callback, so the Godot binding (res://, user://) and the CLI share one parser.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::schema;

/// File format version every seed file declares (`"format": 1`).
pub const SEED_FORMAT: u64 = 1;

pub const WORLD_SCHEMA: &str = include_str!("../../../../addons/simworld/seed/world.schema.json");
pub const CITY_SCHEMA: &str = include_str!("../../../../addons/simworld/seed/city.schema.json");
pub const AIRPORTS_SCHEMA: &str = include_str!("../../../../addons/simworld/seed/airports.schema.json");
pub const WATER_SCHEMA: &str = include_str!("../../../../addons/simworld/seed/water.schema.json");
pub const ROADS_SCHEMA: &str = include_str!("../../../../addons/simworld/seed/roads.schema.json");

/// (file name, text) of every seed schema, for cross-file `$ref`s.
pub const SCHEMAS: [(&str, &str); 5] = [
    ("world.schema.json", WORLD_SCHEMA),
    ("city.schema.json", CITY_SCHEMA),
    ("airports.schema.json", AIRPORTS_SCHEMA),
    ("water.schema.json", WATER_SCHEMA),
    ("roads.schema.json", ROADS_SCHEMA),
];

/// Meters per degree of latitude (mean Earth radius); for distances in validation messages only.
const M_PER_DEG: f64 = 6_371_008.8 * std::f64::consts::PI / 180.0;

/// Quantize degrees to 1e-7 (section 7: inputs are quantized on load, so every decision sees the
/// same bits on every platform).
#[inline]
pub fn q7(deg: f64) -> f64 {
    (deg * 1e7).round() / 1e7
}

/// Quantize meters to 1 cm.
#[inline]
pub fn q_cm(m: f64) -> f64 {
    (m * 100.0).round() / 100.0
}

// ---------------------------------------------------------------------------------------------
// Manifest

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: u32,
    pub world_version: i64,
    pub world_seed: String,
    pub water: String,
    pub airports: AirportSelection,
    pub cities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roads: Option<String>,
}

fn default_airport_types() -> Vec<String> {
    ["large_airport", "medium_airport", "small_airport"].iter().map(|s| s.to_string()).collect()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AirportSelection {
    pub file: String,
    #[serde(default = "default_airport_types")]
    pub types: Vec<String>,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub overrides: BTreeMap<String, AirportOverride>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AirportOverride {
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub elevation_ft: Option<f64>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub runways: BTreeMap<String, RunwayOverride>,
    #[serde(default)]
    pub add: Vec<Runway>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunwayOverride {
    pub le_lat: Option<f64>,
    pub le_lon: Option<f64>,
    pub he_lat: Option<f64>,
    pub he_lon: Option<f64>,
    pub width_ft: Option<f64>,
    pub length_ft: Option<f64>,
    pub surface: Option<String>,
    pub lighted: Option<bool>,
    pub closed: Option<bool>,
    #[serde(rename = "le_heading_degT")]
    pub le_heading_deg_t: Option<f64>,
    pub le_displaced_threshold_ft: Option<f64>,
    pub he_displaced_threshold_ft: Option<f64>,
    #[serde(default)]
    pub remove: bool,
}

// ---------------------------------------------------------------------------------------------
// Airports

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AirportFile {
    pub format: u32,
    pub source: String,
    pub source_date: String,
    pub airports: Vec<Airport>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Airport {
    pub ident: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub elevation_ft: Option<f64>,
    #[serde(default)]
    pub country: String,
    pub runways: Vec<Runway>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Runway {
    pub le: String,
    pub he: String,
    #[serde(default)]
    pub length_ft: Option<f64>,
    #[serde(default)]
    pub width_ft: Option<f64>,
    #[serde(default)]
    pub surface: String,
    #[serde(default)]
    pub lighted: bool,
    #[serde(default)]
    pub closed: bool,
    #[serde(default)]
    pub le_lat: Option<f64>,
    #[serde(default)]
    pub le_lon: Option<f64>,
    #[serde(default)]
    pub he_lat: Option<f64>,
    #[serde(default)]
    pub he_lon: Option<f64>,
    #[serde(default, rename = "le_heading_degT")]
    pub le_heading_deg_t: Option<f64>,
    #[serde(default)]
    pub le_displaced_threshold_ft: Option<f64>,
    #[serde(default)]
    pub he_displaced_threshold_ft: Option<f64>,
}

impl Runway {
    /// Key in `airports.overrides.<ident>.runways`: `"<le>/<he>"`.
    pub fn key(&self) -> String {
        format!("{}/{}", self.le, self.he)
    }

    /// Both ends known (else the generator places it approximately, section 5.1 step 1).
    pub fn has_both_ends(&self) -> bool {
        self.le_lat.is_some() && self.le_lon.is_some() && self.he_lat.is_some() && self.he_lon.is_some()
    }

    fn quantize(&mut self) {
        for v in [&mut self.le_lat, &mut self.le_lon, &mut self.he_lat, &mut self.he_lon].into_iter().flatten() {
            *v = q7(*v);
        }
    }

    fn apply(&mut self, o: &RunwayOverride) {
        macro_rules! set {
            ($($f:ident),*) => {$( if o.$f.is_some() { self.$f = o.$f.clone(); } )*};
        }
        set!(le_lat, le_lon, he_lat, he_lon, width_ft, length_ft, le_heading_deg_t, le_displaced_threshold_ft, he_displaced_threshold_ft);
        if let Some(s) = &o.surface {
            self.surface = s.clone();
        }
        if let Some(b) = o.lighted {
            self.lighted = b;
        }
        if let Some(b) = o.closed {
            self.closed = b;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Cities

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutType {
    OldCenterHex,
    Grid,
    Radial,
    Linear,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layout {
    #[serde(rename = "type")]
    pub kind: LayoutType,
    pub cell_size_m: f64,
    #[serde(default)]
    pub rotation_deg: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistrictType {
    OldTown,
    Business,
    Residential,
    Industrial,
    Port,
    Park,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Style {
    Organic,
    Grid,
    Microdistrict,
    Houses,
    Quay,
    Paths,
}

impl DistrictType {
    pub fn name(self) -> &'static str {
        match self {
            DistrictType::OldTown => "old_town",
            DistrictType::Business => "business",
            DistrictType::Residential => "residential",
            DistrictType::Industrial => "industrial",
            DistrictType::Port => "port",
            DistrictType::Park => "park",
        }
    }

    /// Default style of the type (section 6.2).
    pub fn default_style(self) -> Style {
        match self {
            DistrictType::OldTown => Style::Organic,
            DistrictType::Business | DistrictType::Industrial => Style::Grid,
            DistrictType::Residential => Style::Microdistrict,
            DistrictType::Port => Style::Quay,
            DistrictType::Park => Style::Paths,
        }
    }

    /// Styles the type has (section 6.2).
    pub fn styles(self) -> &'static [Style] {
        match self {
            DistrictType::OldTown => &[Style::Organic],
            DistrictType::Business | DistrictType::Industrial => &[Style::Grid],
            DistrictType::Residential => &[Style::Microdistrict, Style::Houses],
            DistrictType::Port => &[Style::Quay],
            DistrictType::Park => &[Style::Paths],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NearFeature {
    Water,
    Coast,
    River,
    Airport,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Near {
    One(NearFeature),
    Any(Vec<NearFeature>),
}

impl Near {
    pub fn features(&self) -> Vec<NearFeature> {
        match self {
            Near::One(f) => vec![*f],
            Near::Any(v) => v.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistrictRequest {
    #[serde(rename = "type")]
    pub kind: DistrictType,
    #[serde(default)]
    pub style: Option<Style>,
    #[serde(default)]
    pub cells: Option<u32>,
    #[serde(default)]
    pub share: Option<f64>,
    #[serde(default)]
    pub rings: Option<[u32; 2]>,
    #[serde(default)]
    pub near: Option<Near>,
    #[serde(default)]
    pub max_height_m: Option<f64>,
    #[serde(default)]
    pub density: Option<f64>,
    #[serde(default)]
    pub landmarks: Option<u32>,
    #[serde(default)]
    pub lit: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fill {
    #[serde(rename = "type")]
    pub kind: DistrictType,
    #[serde(default)]
    pub style: Option<Style>,
    #[serde(default)]
    pub max_height_m: Option<f64>,
    #[serde(default)]
    pub density: Option<f64>,
    #[serde(default)]
    pub lit: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealSource {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandmarkGround {
    Park,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InReal {
    Skip,
    Replace,
}

fn default_landmark_kind() -> String {
    "tower".into()
}
fn default_ground() -> LandmarkGround {
    LandmarkGround::Park
}
fn default_in_real() -> InReal {
    InReal::Skip
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Landmark {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub model: String,
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub heading_deg: f64,
    #[serde(default)]
    pub footprint_m: Option<[f64; 2]>,
    #[serde(default)]
    pub footprint: Option<Vec<[f64; 2]>>,
    pub height_m: f64,
    #[serde(default)]
    pub clear_m: f64,
    #[serde(default = "default_ground")]
    pub ground: LandmarkGround,
    #[serde(default = "default_landmark_kind")]
    pub kind: String,
    #[serde(default = "default_in_real")]
    pub in_real: InReal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct City {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: i64,
    #[serde(default)]
    pub variant: i64,
    pub center: LatLon,
    pub radius_km: f64,
    pub layout: Layout,
    #[serde(default)]
    pub max_height_m: Option<f64>,
    #[serde(default)]
    pub density: Option<f64>,
    #[serde(default)]
    pub airports: Vec<String>,
    pub districts: Vec<DistrictRequest>,
    pub fill: Fill,
    #[serde(default)]
    pub real: Option<RealSource>,
    #[serde(default)]
    pub edits: Option<String>,
    #[serde(default)]
    pub landmarks: Vec<Landmark>,
}

/// Upper bound of the footprint radius: the outer-ring edge noise keeps a cell while
/// `distance < radius * (1 + 0.15 * noise)` (section 5.3 step 1), noise in [-1, 1].
pub const FOOTPRINT_NOISE: f64 = 0.15;

/// Pointy-top axial hex (q, r) -> centre offset (east, north) in meters for a centre-to-centre
/// distance `cell_m`, unrotated.
pub fn hex_center(q: i32, r: i32, cell_m: f64) -> [f64; 2] {
    let s3h = 0.866_025_403_784_438_6; // sqrt(3) / 2
    [cell_m * (q as f64 + r as f64 * 0.5), cell_m * s3h * r as f64]
}

/// Hex distance from the centre cell (= ring).
pub fn hex_ring(q: i32, r: i32) -> u32 {
    ((q.abs() + r.abs() + (q + r).abs()) / 2) as u32
}

impl City {
    /// Rings a cell of this city can be in: 0..ring_count. The innermost centre of ring `r` lies
    /// `r * cell * sqrt(3)/2` from the centre, and a cell is kept up to `radius * (1 + 0.15)`.
    pub fn ring_count(&self) -> u32 {
        let reach = self.radius_km * 1000.0 * (1.0 + FOOTPRINT_NOISE);
        (reach / (self.layout.cell_size_m * 0.866_025_403_784_438_6)).floor() as u32 + 1
    }

    /// Cells whose centre is within `radius * factor` (no edge noise): `factor` 1 = the nominal
    /// footprint, `1 + FOOTPRINT_NOISE` = the most cells the footprint can ever have.
    pub fn cell_count(&self, factor: f64) -> u32 {
        let rad = self.radius_km * 1000.0 * factor;
        let n = self.ring_count() as i32;
        let mut count = 0;
        for q in -n..=n {
            for r in -n..=n {
                let [x, y] = hex_center(q, r, self.layout.cell_size_m);
                if (x * x + y * y).sqrt() < rad {
                    count += 1;
                }
            }
        }
        count
    }

    fn quantize(&mut self) {
        self.center.lat = q7(self.center.lat);
        self.center.lon = q7(self.center.lon);
        for l in &mut self.landmarks {
            l.lat = q7(l.lat);
            l.lon = q7(l.lon);
            for p in l.footprint.iter_mut().flatten() {
                *p = [q7(p[0]), q7(p[1])];
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Water

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaterKind {
    Sea,
    Lake,
    River,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outside {
    Sea,
    Land,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaterPolygon {
    pub kind: WaterKind,
    /// Outer ring first, then holes; [lon, lat]; open (the loader drops a closing duplicate).
    pub rings: Vec<Vec<[f64; 2]>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaterFile {
    pub format: u32,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_date: Option<String>,
    /// [west, south, east, north].
    pub bbox: [f64; 4],
    pub outside: Outside,
    pub tolerance_m: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_area_m2: Option<f64>,
    pub polygons: Vec<WaterPolygon>,
}

/// Quantize, drop consecutive duplicates and the closing duplicate. `None` when fewer than 3
/// distinct points remain.
pub fn clean_ring(ring: &[[f64; 2]]) -> Option<Vec<[f64; 2]>> {
    let mut out: Vec<[f64; 2]> = Vec::with_capacity(ring.len());
    for p in ring {
        let q = [q7(p[0]), q7(p[1])];
        if out.last() != Some(&q) {
            out.push(q);
        }
    }
    while out.len() > 1 && out.first() == out.last() {
        out.pop();
    }
    (out.len() >= 3).then_some(out)
}

// ---------------------------------------------------------------------------------------------
// Roads

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoadClassName {
    Motorway,
    Trunk,
    Primary,
    Secondary,
    Tertiary,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Road {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub class: RoadClassName,
    #[serde(default)]
    pub lit: f64,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    pub points: Vec<[f64; 2]>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoadsFile {
    pub format: u32,
    pub roads: Vec<Road>,
}

// ---------------------------------------------------------------------------------------------
// Parsing

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

// ---------------------------------------------------------------------------------------------
// The whole world (section 4.6)

/// Every file of a seed world, parsed, corrected and checked.
#[derive(Clone, Debug)]
pub struct SeedFiles {
    pub manifest: Manifest,
    pub airport_source: (String, String),
    /// Every airport of the file after `airports.overrides` (sorted by ident).
    pub all_airports: Vec<Airport>,
    /// The selected airports (types, include, exclude, at least one open runway), sorted by
    /// ident, with closed and removed runways dropped.
    pub airports: Vec<Airport>,
    pub cities: Vec<City>,
    pub water: WaterFile,
    pub roads: Option<RoadsFile>,
    pub warnings: Vec<String>,
}

/// A failed load: every error found (and the warnings so far).
#[derive(Clone, Debug, Default)]
pub struct LoadError {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.errors.join("\n"))
    }
}

/// Reader callback: path as written in the manifest -> (label for messages, text). The host
/// resolves `res://`, `user://` and relative paths.
pub type Reader<'a> = dyn FnMut(&str) -> Result<(String, String), String> + 'a;

/// Apply `airports.overrides` (section 4.1): an unknown ident or runway is an error.
pub fn apply_overrides(airports: &mut [Airport], overrides: &BTreeMap<String, AirportOverride>, label: &str, errs: &mut Vec<String>) {
    for (ident, o) in overrides {
        let Some(a) = airports.iter_mut().find(|a| &a.ident == ident) else {
            errs.push(format!("{label}: airports.overrides.{ident}: unknown airport ident \"{ident}\""));
            continue;
        };
        if let Some(v) = o.lat {
            a.lat = v;
        }
        if let Some(v) = o.lon {
            a.lon = v;
        }
        if o.elevation_ft.is_some() {
            a.elevation_ft = o.elevation_ft;
        }
        if let Some(k) = &o.kind {
            a.kind = k.clone();
        }
        for (key, ro) in &o.runways {
            let Some(i) = a.runways.iter().position(|r| &r.key() == key) else {
                let known: Vec<String> = a.runways.iter().map(Runway::key).collect();
                errs.push(format!(
                    "{label}: airports.overrides.{ident}.runways.{key}: unknown runway \"{key}\" (the file has {})",
                    if known.is_empty() { "none".to_string() } else { known.join(", ") }
                ));
                continue;
            };
            if ro.remove {
                a.runways.remove(i);
            } else {
                a.runways[i].apply(ro);
            }
        }
        for r in &o.add {
            if a.runways.iter().any(|x| x.key() == r.key()) {
                errs.push(format!("{label}: airports.overrides.{ident}.add: runway \"{}\" already exists (use runways.\"{}\")", r.key(), r.key()));
            } else {
                a.runways.push(r.clone());
            }
        }
        a.runways.sort_by_key(Runway::key);
    }
}

/// Equirectangular distance (m) between two points; validation messages and coarse checks only.
pub fn approx_distance_m(a: LatLon, b: LatLon) -> f64 {
    let k = crate::math::cos(((a.lat + b.lat) * 0.5).to_radians());
    let dx = (b.lon - a.lon) * M_PER_DEG * k;
    let dy = (b.lat - a.lat) * M_PER_DEG;
    (dx * dx + dy * dy).sqrt()
}

/// Load and check a whole seed world. `manifest` = (label, text); `read` resolves every other
/// path. Collects all errors it can find (a broken city does not hide a broken road file).
pub fn load_world(manifest: (&str, &str), read: &mut Reader<'_>) -> Result<SeedFiles, LoadError> {
    let (mlabel, mtext) = manifest;
    let mut errs = Vec::new();
    let mut warns = Vec::new();
    let m = parse_manifest(mlabel, mtext).map_err(|e| LoadError { errors: e, warnings: vec![] })?;

    let mut load = |path: &str, errs: &mut Vec<String>| -> Option<(String, String)> {
        match read(path) {
            Ok(x) => Some(x),
            Err(e) => {
                errs.push(format!("{mlabel}: cannot read {path}: {e}"));
                None
            }
        }
    };

    // Airports, with overrides, then the selection.
    let mut airport_file = None;
    if let Some((l, t)) = load(&m.airports.file, &mut errs) {
        match parse_airports(&l, &t) {
            Ok(f) => airport_file = Some(f),
            Err(e) => errs.extend(e),
        }
    }
    let water = load(&m.water, &mut errs).and_then(|(l, t)| parse_water(&l, &t).map_err(|e| errs.extend(e)).ok());
    let roads = match &m.roads {
        Some(p) => load(p, &mut errs).and_then(|(l, t)| parse_roads(&l, &t).map_err(|e| errs.extend(e)).ok()),
        None => None,
    };
    let mut cities: Vec<City> = Vec::new();
    for p in &m.cities {
        if let Some((l, t)) = load(p, &mut errs) {
            match parse_city(&l, &t) {
                Ok(c) => {
                    check_city(&c, &l, &mut errs, &mut warns);
                    cities.push(c);
                }
                Err(e) => errs.extend(e),
            }
        }
    }

    let airport_ok = airport_file.is_some();
    let (mut all_airports, source) = match airport_file {
        Some(f) => (f.airports, (f.source, f.source_date)),
        None => (Vec::new(), (String::new(), String::new())),
    };
    if airport_ok {
        apply_overrides(&mut all_airports, &m.airports.overrides, mlabel, &mut errs);
    }
    for x in m.airports.include.iter().chain(&m.airports.exclude) {
        if airport_ok && !all_airports.iter().any(|a| &a.ident == x) {
            warns.push(format!("{mlabel}: airports: include/exclude names \"{x}\", which is not in the airport file"));
        }
    }
    let airports = select_airports(&all_airports, &m.airports, &mut warns);

    // Cities against each other and the airports.
    let mut ids = BTreeSet::new();
    for c in &cities {
        if !ids.insert(c.id.clone()) {
            errs.push(format!("{mlabel}: cities: duplicate city id \"{}\"", c.id));
        }
        for ident in c.airports.iter().filter(|_| airport_ok) {
            if !all_airports.iter().any(|a| &a.ident == ident) {
                errs.push(format!("city {}: airports: \"{ident}\" is not in the airport file", c.id));
            } else if !airports.iter().any(|a| &a.ident == ident) {
                warns.push(format!("city {}: airports: \"{ident}\" is not selected (type filter, exclude or no open runway)", c.id));
            }
        }
    }
    for (i, a) in cities.iter().enumerate() {
        for b in &cities[i + 1..] {
            let d = approx_distance_m(a.center, b.center);
            let reach = (a.radius_km + b.radius_km) * 1000.0 * (1.0 + FOOTPRINT_NOISE);
            if d < reach {
                errs.push(format!(
                    "cities {} and {}: footprints overlap (centres {:.1} km apart, radii {} + {} km, x {:.2} for the footprint edge noise = {:.1} km)",
                    a.id,
                    b.id,
                    d / 1000.0,
                    a.radius_km,
                    b.radius_km,
                    1.0 + FOOTPRINT_NOISE,
                    reach / 1000.0
                ));
            }
        }
    }

    // Roads against cities and airports.
    if let Some(r) = &roads {
        for (i, road) in r.roads.iter().enumerate() {
            for (key, end, pt) in [("from", &road.from, road.points.first()), ("to", &road.to, road.points.last())] {
                let Some(end) = end else { continue };
                if let Some(ident) = end.strip_prefix("airport:") {
                    if !airports.iter().any(|a| a.ident == ident) {
                        errs.push(format!("roads.json: roads[{i}].{key}: \"{end}\" names no selected airport"));
                    }
                } else if let Some(c) = cities.iter().find(|c| &c.id == end) {
                    if let Some(p) = pt {
                        let d = approx_distance_m(c.center, LatLon { lat: p[1], lon: p[0] }) - c.radius_km * 1000.0 * (1.0 + FOOTPRINT_NOISE);
                        if d > 2000.0 {
                            warns.push(format!("roads.json: roads[{i}].{key}: road \"{}\" ends {:.1} km outside city {}", road.id, d / 1000.0, c.id));
                        }
                    }
                } else {
                    errs.push(format!("roads.json: roads[{i}].{key}: \"{end}\" is neither a city id nor airport:<ident>"));
                }
            }
        }
    }

    // Runways with an end missing (approximate placement, section 5.1).
    for a in &airports {
        for r in a.runways.iter().filter(|r| !r.has_both_ends()) {
            warns.push(format!("airport {}: runway {} has an end missing: placed approximately", a.ident, r.key()));
        }
    }

    match (errs.is_empty(), water) {
        (true, Some(water)) => Ok(SeedFiles {
            manifest: m,
            airport_source: source,
            all_airports,
            airports,
            cities,
            water,
            roads,
            warnings: warns,
        }),
        _ => Err(LoadError { errors: errs, warnings: warns }),
    }
}

/// Selection of section 4.1: type in `types`, plus `include`, minus `exclude`, with at least one
/// open runway. Closed runways are dropped from the result.
pub fn select_airports(all: &[Airport], sel: &AirportSelection, warns: &mut Vec<String>) -> Vec<Airport> {
    let mut out = Vec::new();
    for a in all {
        let wanted = (sel.types.contains(&a.kind) || sel.include.contains(&a.ident)) && !sel.exclude.contains(&a.ident);
        if !wanted {
            continue;
        }
        let mut a = a.clone();
        a.runways.retain(|r| !r.closed);
        if a.runways.is_empty() {
            if sel.include.contains(&a.ident) {
                warns.push(format!("airport {}: included but has no open runway: skipped", a.ident));
            }
            continue;
        }
        out.push(a);
    }
    out.sort_by(|a, b| a.ident.cmp(&b.ident));
    out
}

/// Per-city checks that need only the city file.
fn check_city(c: &City, label: &str, errs: &mut Vec<String>, warns: &mut Vec<String>) {
    let rings = c.ring_count();
    let max_cells = c.cell_count(1.0 + FOOTPRINT_NOISE);
    let nominal = c.cell_count(1.0).max(1);
    let (mut cells_sum, mut share_sum) = (0u64, 0.0f64);
    for (i, d) in c.districts.iter().enumerate() {
        let at = format!("{label}: districts[{i}]");
        match (d.cells, d.share) {
            (Some(_), Some(_)) => errs.push(format!("{at}: has both `cells` and `share` (exactly one)")),
            (None, None) => errs.push(format!("{at}: has neither `cells` nor `share` (exactly one)")),
            (Some(n), None) => cells_sum += n as u64,
            (None, Some(s)) => share_sum += s,
        }
        if let Some([lo, hi]) = d.rings {
            if lo > hi {
                errs.push(format!("{at}.rings: [{lo}, {hi}]: min above max"));
            } else if hi >= rings {
                errs.push(format!("{at}.rings: [{lo}, {hi}]: the city has rings 0..{} (radius {} km, cell {} m)", rings - 1, c.radius_km, c.layout.cell_size_m));
            }
        }
        if let Some(s) = d.style {
            if !d.kind.styles().contains(&s) {
                errs.push(format!("{at}.style: {s:?} is not a style of {} (styles: {:?})", d.kind.name(), d.kind.styles()));
            }
        }
    }
    if let Some(s) = c.fill.style {
        if !c.fill.kind.styles().contains(&s) {
            errs.push(format!("{label}: fill.style: {s:?} is not a style of {}", c.fill.kind.name()));
        }
    }
    if cells_sum > max_cells as u64 {
        errs.push(format!("{label}: districts: requests take {cells_sum} cells, the city has at most {max_cells}"));
    } else {
        let want = cells_sum as f64 + share_sum * nominal as f64;
        if want > nominal as f64 {
            warns.push(format!("{label}: districts: cells + share x land cells = {want:.0} exceed the about {nominal} cells of the city"));
        }
    }
    if let Some(r) = &c.real {
        match (&r.url, &r.path) {
            (Some(_), Some(_)) => errs.push(format!("{label}: real: has both `url` and `path` (exactly one)")),
            (None, None) => errs.push(format!("{label}: real: has neither `url` nor `path`")),
            (Some(u), None) => {
                if !(u.starts_with("http://") || u.starts_with("https://")) {
                    errs.push(format!("{label}: real.url: \"{u}\" must start with http:// or https://"));
                }
            }
            (None, Some(p)) => {
                if p.starts_with("res://") {
                    errs.push(format!("{label}: real.path: \"{p}\" is under res:// (not a file in an exported game; copy it to the cache folder)"));
                }
            }
        }
    }
    let mut lm_ids = BTreeSet::new();
    for (i, l) in c.landmarks.iter().enumerate() {
        let at = format!("{label}: landmarks[{i}]");
        if !lm_ids.insert(&l.id) {
            errs.push(format!("{at}.id: duplicate landmark id \"{}\"", l.id));
        }
        let lower = l.model.to_ascii_lowercase();
        if !l.model.starts_with("res://") || ![".glb", ".gltf", ".tscn", ".scn"].iter().any(|e| lower.ends_with(e)) {
            errs.push(format!("{at}.model: \"{}\" must be a res:// .glb, .gltf, .tscn or .scn", l.model));
        }
        match (&l.footprint_m, &l.footprint) {
            (Some(_), Some(_)) => errs.push(format!("{at}: has both `footprint_m` and `footprint` (exactly one)")),
            (None, None) => errs.push(format!("{at}: has neither `footprint_m` nor `footprint`")),
            _ => {}
        }
        let d = approx_distance_m(c.center, LatLon { lat: l.lat, lon: l.lon });
        if d > c.radius_km * 1000.0 * (1.0 + FOOTPRINT_NOISE) {
            warns.push(format!("{at}: \"{}\" lies {:.1} km from the centre, outside the city footprint", l.id, d / 1000.0));
        }
    }
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
