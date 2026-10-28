//! `seed airports`: cut OurAirports `airports.csv` + `runways.csv` to a region and write the
//! seed airport file (docs/spec-world-seed.md sections 3.2, 4.3).

use std::collections::BTreeMap;

use simworld_core::seed::files::{q7, Airport, AirportFile, Runway};

use super::csv::Table;

/// Region filter.
#[derive(Clone, Debug, PartialEq)]
pub enum Region {
    /// [west, south, east, north].
    Bbox([f64; 4]),
    /// ISO country codes (OurAirports `iso_country`).
    Countries(Vec<String>),
}

/// What the cut found, for the report.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CutReport {
    pub airports: usize,
    pub runways: usize,
    pub by_type: BTreeMap<String, usize>,
    /// "IDENT runway le/he: missing ..." lines.
    pub incomplete: Vec<String>,
    pub notes: Vec<String>,
}

fn num(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        t.parse::<f64>().ok().filter(|v| v.is_finite())
    }
}

fn flag(s: &str) -> bool {
    matches!(s.trim(), "1" | "true" | "TRUE" | "yes")
}

/// Cut the CSVs. `types`: keep only these OurAirports types (empty = all).
pub fn cut(
    airports_csv: &str,
    runways_csv: &str,
    region: &Region,
    types: &[String],
    source_date: &str,
) -> Result<(AirportFile, CutReport), String> {
    let at = Table::parse(airports_csv, "airports.csv")?;
    let rt = Table::parse(runways_csv, "runways.csv")?;
    let a = |n: &str| at.col(n, "airports.csv");
    let (c_ident, c_type, c_name, c_lat, c_lon, c_elev, c_country) =
        (a("ident")?, a("type")?, a("name")?, a("latitude_deg")?, a("longitude_deg")?, a("elevation_ft")?, a("iso_country")?);
    let r = |n: &str| rt.col(n, "runways.csv");
    let rc = [
        r("airport_ident")?,
        r("length_ft")?,
        r("width_ft")?,
        r("surface")?,
        r("lighted")?,
        r("closed")?,
        r("le_ident")?,
        r("le_latitude_deg")?,
        r("le_longitude_deg")?,
        r("le_heading_degT")?,
        r("le_displaced_threshold_ft")?,
        r("he_ident")?,
        r("he_latitude_deg")?,
        r("he_longitude_deg")?,
        r("he_displaced_threshold_ft")?,
    ];
    let mut report = CutReport::default();
    let mut out: BTreeMap<String, Airport> = BTreeMap::new();
    for (i, row) in at.rows.iter().enumerate() {
        let get = |c: usize| row.get(c).map(String::as_str).unwrap_or("");
        let (Some(lat), Some(lon)) = (num(get(c_lat)), num(get(c_lon))) else {
            report.notes.push(format!("airports.csv row {}: {} has no position: skipped", i + 2, get(c_ident)));
            continue;
        };
        let keep = match region {
            Region::Bbox([w, s, e, n]) => lon >= *w && lon <= *e && lat >= *s && lat <= *n,
            Region::Countries(cs) => cs.iter().any(|c| c == get(c_country)),
        };
        let kind = get(c_type).to_string();
        if !keep || (!types.is_empty() && !types.contains(&kind)) {
            continue;
        }
        let ident = get(c_ident).trim().to_string();
        if ident.is_empty() || !ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            report.notes.push(format!("airports.csv row {}: ident {ident:?} is not [A-Za-z0-9_-]+: skipped", i + 2));
            continue;
        }
        if !(-85.0..=85.0).contains(&lat) {
            report.notes.push(format!("{ident}: latitude {lat} outside +-85: skipped"));
            continue;
        }
        if out.contains_key(&ident) {
            report.notes.push(format!("{ident}: duplicate ident in airports.csv: first row kept"));
            continue;
        }
        out.insert(
            ident.clone(),
            Airport {
                ident,
                kind,
                name: get(c_name).to_string(),
                lat: q7(lat),
                lon: q7(lon),
                elevation_ft: num(get(c_elev)),
                country: get(c_country).to_string(),
                runways: Vec::new(),
            },
        );
    }
    for row in &rt.rows {
        let get = |k: usize| row.get(rc[k]).map(String::as_str).unwrap_or("");
        let Some(ap) = out.get_mut(get(0).trim()) else { continue };
        let ident_part = |s: &str| {
            let t = s.trim();
            if t.contains('/') {
                t.replace('/', "-")
            } else {
                t.to_string()
            }
        };
        let ll = |s: &str| num(s).map(q7);
        let rw = Runway {
            le: ident_part(get(6)),
            he: ident_part(get(11)),
            length_ft: num(get(1)).filter(|v| *v >= 0.0),
            width_ft: num(get(2)).filter(|v| *v >= 0.0),
            surface: get(3).trim().to_string(),
            lighted: flag(get(4)),
            closed: flag(get(5)),
            le_lat: ll(get(7)),
            le_lon: ll(get(8)),
            he_lat: ll(get(12)),
            he_lon: ll(get(13)),
            le_heading_deg_t: num(get(9)).filter(|v| (0.0..=360.0).contains(v)),
            le_displaced_threshold_ft: num(get(10)).filter(|v| *v >= 0.0),
            he_displaced_threshold_ft: num(get(14)).filter(|v| *v >= 0.0),
        };
        if get(6).contains('/') || get(11).contains('/') {
            report.notes.push(format!("{}: runway designator with '/' ({} / {}) written with '-'", ap.ident, get(6), get(11)));
        }
        if ap.runways.iter().any(|x| x.key() == rw.key()) {
            report.notes.push(format!("{}: duplicate runway {}: first kept", ap.ident, rw.key()));
            continue;
        }
        ap.runways.push(rw);
    }
    let mut airports: Vec<Airport> = out.into_values().collect();
    for a in &mut airports {
        a.runways.sort_by_key(Runway::key);
        *report.by_type.entry(a.kind.clone()).or_default() += 1;
        report.runways += a.runways.len();
        for r in &a.runways {
            if !r.has_both_ends() && !r.closed {
                let mut miss = Vec::new();
                if r.le_lat.is_none() || r.le_lon.is_none() {
                    miss.push("le end");
                }
                if r.he_lat.is_none() || r.he_lon.is_none() {
                    miss.push("he end");
                }
                report.incomplete.push(format!("{} ({}) runway {}: missing {}", a.ident, a.kind, r.key(), miss.join(" and ")));
            }
        }
    }
    report.airports = airports.len();
    Ok((AirportFile { format: 1, source: "OurAirports".into(), source_date: source_date.into(), airports }, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    const AIRPORTS: &str = include_str!("../../testdata/ourairports/airports.csv");
    const RUNWAYS: &str = include_str!("../../testdata/ourairports/runways.csv");

    #[test]
    fn cut_by_country_and_bbox() {
        let (f, rep) = cut(AIRPORTS, RUNWAYS, &Region::Countries(vec!["LT".into(), "LV".into()]), &[], "2026-10-27").unwrap();
        let idents: Vec<&str> = f.airports.iter().map(|a| a.ident.as_str()).collect();
        assert_eq!(idents, ["EVRA", "EYKA", "EYPA", "EYVI", "LT-0003"], "sorted, EE dropped");
        let eyka = &f.airports[1];
        assert_eq!(eyka.name, "Kaunas International Airport, \"Karmėlava\"");
        assert_eq!(eyka.runways[0].key(), "08/26");
        assert_eq!(eyka.runways[0].le_lat, Some(54.9627));
        assert!(eyka.runways[0].lighted && !eyka.runways[0].closed);
        assert!(rep.incomplete.iter().any(|l| l.starts_with("LT-0003") && l.contains("missing le end and he end")), "{:?}", rep.incomplete);
        assert_eq!(rep.by_type["large_airport"], 2);
        // The output passes the core's loader (schema + parse).
        let json = serde_json::to_string_pretty(&f).unwrap();
        let back = simworld_core::seed::files::parse_airports("out", &json).unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(back, f);
        // bbox around Kaunas only.
        let (g, _) = cut(AIRPORTS, RUNWAYS, &Region::Bbox([23.5, 54.8, 24.5, 55.1]), &[], "2026-10-27").unwrap();
        assert_eq!(g.airports.iter().map(|a| a.ident.as_str()).collect::<Vec<_>>(), ["EYKA"]);
        // Type filter.
        let (h, _) = cut(AIRPORTS, RUNWAYS, &Region::Countries(vec!["LT".into()]), &["small_airport".into()], "2026-10-27").unwrap();
        assert_eq!(h.airports.iter().map(|a| a.ident.as_str()).collect::<Vec<_>>(), ["LT-0003"]);
    }

    #[test]
    fn missing_column_is_named() {
        let e = cut("ident,type\nX,small_airport\n", RUNWAYS, &Region::Countries(vec![]), &[], "2026-10-27").unwrap_err();
        assert!(e.contains("no column `name`"), "{e}");
    }
}
