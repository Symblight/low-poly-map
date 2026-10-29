//! The whole world (section 4.6): load every file, apply overrides, select airports, validate.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    parse_airports, parse_city, parse_manifest, parse_roads, parse_water, Airport, AirportOverride, AirportSelection, City, LatLon,
    Manifest, RoadsFile, Runway, WaterFile, FOOTPRINT_NOISE,
};

/// Meters per degree of latitude (mean Earth radius); for distances in validation messages only.
const M_PER_DEG: f64 = 6_371_008.8 * std::f64::consts::PI / 180.0;

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
