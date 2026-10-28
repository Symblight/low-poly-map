//! `simworld-cli seed ...`: the seed world's offline tools (docs/spec-world-seed.md section 3.2).
//! S0: `airports`, `water`, `validate`. Later phases add `preview`, `roads`, `export`,
//! `import-edits`.

pub mod airports;
pub mod csv;
pub mod validate;
pub mod water;

use std::path::PathBuf;

use simworld_core::seed::files::Outside;
use simworld_core::seed::water::WaterParams;

pub const USAGE: &str = "\
  simworld-cli seed airports (--bbox W,S,E,N | --countries LT,LV,EE) [--types large_airport,...]
                             [--source-date YYYY-MM-DD] <airports.csv> <runways.csv> -o airports.json
      cut OurAirports CSVs (https://ourairports.com/data/) to a region; reports runways with an end missing
  simworld-cli seed water --bbox W,S,E,N --outside sea|land [--tolerance-m 50] [--min-area-m2 20000]
                          [--sea F.geojson]... [--land F.geojson]... [--lakes F.geojson]... [--rivers F.geojson]...
                          [--source-date YYYY-MM-DD] -o water.json
      build the low-poly water file from GeoJSON polygons (see `seed water --help`)
  simworld-cli seed validate <world.json> [--res-root DIR] [--raster-n 64] [--blend-m 400]
      load a seed world like the library does: errors (exit 1), warnings, summary, water index size";

const WATER_HELP: &str = "\
seed water inputs are GeoJSON Polygon / MultiPolygon features (no GIS library in the CLI). Typical
conversions with GDAL (e.g. the ghcr.io/osgeo/gdal Docker image):
  sea   (osmdata.openstreetmap.de water-polygons-split-4326.zip):
        ogr2ogr -f GeoJSON -spat W S E N sea.geojson water_polygons.shp            -> --sea sea.geojson
  land  (land-polygons-complete-4326.zip; one sea polygon, no split seams):
        ogr2ogr -f GeoJSON -spat W S E N land.geojson land_polygons.shp            -> --land land.geojson
  lakes (the Geofabrik extract the tile build uses):
        ogr2ogr -f GeoJSON -spat W S E N lakes.geojson extract.osm.pbf multipolygons \\
          -where \"natural='water' OR waterway='riverbank' OR landuse='reservoir'\"  -> --lakes lakes.geojson
        (features tagged water=river/canal/... or waterway=riverbank become `river`, the rest `lake`)";

fn arg_value<'a>(args: &'a [String], i: &mut usize, name: &str) -> Result<&'a str, String> {
    *i += 1;
    args.get(*i).map(String::as_str).ok_or_else(|| format!("{name} needs a value"))
}

fn parse_bbox(s: &str) -> Result<[f64; 4], String> {
    let v: Vec<f64> = s.split(',').map(|x| x.trim().parse::<f64>()).collect::<Result<_, _>>().map_err(|_| format!("--bbox {s}: four numbers W,S,E,N"))?;
    match v.as_slice() {
        [w, s2, e, n] if w < e && s2 < n => Ok([*w, *s2, *e, *n]),
        _ => Err(format!("--bbox {s}: four numbers W,S,E,N with W < E and S < N")),
    }
}

fn check_date(s: &str) -> Result<String, String> {
    let ok = s.len() == 10 && s.chars().enumerate().all(|(i, c)| if i == 4 || i == 7 { c == '-' } else { c.is_ascii_digit() });
    ok.then(|| s.to_string()).ok_or_else(|| format!("--source-date {s}: YYYY-MM-DD"))
}

/// UTC date of a file's modification time (YYYY-MM-DD), the default `source_date`.
fn file_date(path: &str) -> String {
    let secs = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn write_out(path: &str, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("{path}: {e}"))
}

/// Run `seed <args>`; Ok(true) = success.
pub fn run(args: &[String]) -> Result<bool, String> {
    match args.first().map(String::as_str) {
        Some("airports") => cmd_airports(&args[1..]),
        Some("water") => cmd_water(&args[1..]),
        Some("validate") => cmd_validate(&args[1..]),
        _ => Err(format!("usage:\n{USAGE}")),
    }
}

fn cmd_airports(args: &[String]) -> Result<bool, String> {
    let (mut region, mut types, mut date, mut out, mut files) = (None, Vec::new(), None, None, Vec::new());
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--bbox" => region = Some(airports::Region::Bbox(parse_bbox(arg_value(args, &mut i, "--bbox")?)?)),
            "--countries" => {
                let v = arg_value(args, &mut i, "--countries")?;
                region = Some(airports::Region::Countries(v.split(',').map(|s| s.trim().to_uppercase()).filter(|s| !s.is_empty()).collect()));
            }
            "--types" => types = arg_value(args, &mut i, "--types")?.split(',').map(|s| s.trim().to_string()).collect(),
            "--source-date" => date = Some(check_date(arg_value(args, &mut i, "--source-date")?)?),
            "-o" => out = Some(arg_value(args, &mut i, "-o")?.to_string()),
            a if a.starts_with('-') => return Err(format!("seed airports: unknown option {a}\n{USAGE}")),
            a => files.push(a.to_string()),
        }
        i += 1;
    }
    let (Some(region), Some(out), [ap, rw]) = (region, out, files.as_slice()) else {
        return Err(format!("seed airports: need --bbox or --countries, -o and two CSV files\n{USAGE}"));
    };
    let read = |p: &str| std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"));
    let date = date.unwrap_or_else(|| file_date(ap));
    let (file, rep) = airports::cut(&read(ap)?, &read(rw)?, &region, &types, &date)?;
    let json = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())? + "\n";
    simworld_core::seed::files::parse_airports(&out, &json).map_err(|e| format!("internal: output fails the schema: {}", e.join("; ")))?;
    write_out(&out, &json)?;
    for n in &rep.notes {
        eprintln!("note: {n}");
    }
    for l in &rep.incomplete {
        eprintln!("incomplete: {l}");
    }
    eprintln!(
        "{out}: {} airports ({}), {} runways, {} with an end missing; source_date {date}",
        rep.airports,
        rep.by_type.iter().map(|(k, v)| format!("{v} {k}")).collect::<Vec<_>>().join(", "),
        rep.runways,
        rep.incomplete.len()
    );
    Ok(true)
}

fn cmd_water(args: &[String]) -> Result<bool, String> {
    let mut o = water::WaterOptions { bbox: [0.0; 4], tolerance_m: 50.0, min_area_m2: 20_000.0, outside: Outside::Sea, source_date: None };
    let (mut have_bbox, mut have_outside, mut out, mut inputs) = (false, false, None, Vec::new());
    let mut i = 0;
    while i < args.len() {
        let a = args[i].clone();
        match a.as_str() {
            "--help" | "-h" => {
                println!("{WATER_HELP}");
                return Ok(true);
            }
            "--bbox" => {
                o.bbox = parse_bbox(arg_value(args, &mut i, "--bbox")?)?;
                have_bbox = true;
            }
            "--tolerance-m" => o.tolerance_m = arg_value(args, &mut i, &a)?.parse().map_err(|_| "--tolerance-m: a number")?,
            "--min-area-m2" => o.min_area_m2 = arg_value(args, &mut i, &a)?.parse().map_err(|_| "--min-area-m2: a number")?,
            "--outside" => {
                o.outside = match arg_value(args, &mut i, &a)? {
                    "sea" => Outside::Sea,
                    "land" => Outside::Land,
                    v => return Err(format!("--outside {v}: sea or land")),
                };
                have_outside = true;
            }
            "--source-date" => o.source_date = Some(check_date(arg_value(args, &mut i, &a)?)?),
            "--sea" | "--land" | "--lakes" | "--rivers" => {
                let kind = match a.as_str() {
                    "--sea" => water::InputKind::Sea,
                    "--land" => water::InputKind::Land,
                    "--lakes" => water::InputKind::Lakes,
                    _ => water::InputKind::Rivers,
                };
                let p = arg_value(args, &mut i, &a)?.to_string();
                let text = std::fs::read_to_string(&p).map_err(|e| format!("{p}: {e}"))?;
                inputs.push((kind, text, p));
            }
            "-o" => out = Some(arg_value(args, &mut i, "-o")?.to_string()),
            a => return Err(format!("seed water: unknown argument {a}\n{USAGE}")),
        }
        i += 1;
    }
    let Some(out) = out.filter(|_| have_bbox && have_outside && !inputs.is_empty()) else {
        return Err(format!("seed water: need --bbox, --outside, -o and at least one input\n{USAGE}"));
    };
    let t = std::time::Instant::now();
    let (file, rep) = water::build(&inputs, &o)?;
    let json = water::to_json(&file);
    simworld_core::seed::files::parse_water(&out, &json).map_err(|e| format!("internal: output fails the schema: {}", e.join("; ")))?;
    write_out(&out, &json)?;
    eprintln!(
        "{out}: {} sea, {} lake, {} river polygon(s), {} points, {:.2} MB ({} features in; dropped {} small, {} outside the bbox, {} small holes) in {:.1} s",
        rep.polygons_out[0],
        rep.polygons_out[1],
        rep.polygons_out[2],
        rep.points_out,
        json.len() as f64 / 1e6,
        rep.features_in,
        rep.dropped_small,
        rep.dropped_outside,
        rep.holes_dropped,
        t.elapsed().as_secs_f64()
    );
    Ok(true)
}

fn cmd_validate(args: &[String]) -> Result<bool, String> {
    let (mut manifest, mut res_root, mut params) = (None, None, WaterParams::default());
    let mut i = 0;
    while i < args.len() {
        let a = args[i].clone();
        match a.as_str() {
            "--res-root" => res_root = Some(PathBuf::from(arg_value(args, &mut i, &a)?)),
            "--raster-n" => params.raster_n = arg_value(args, &mut i, &a)?.parse().map_err(|_| "--raster-n: an integer")?,
            "--blend-m" => params.blend_m = arg_value(args, &mut i, &a)?.parse().map_err(|_| "--blend-m: a number")?,
            a if a.starts_with('-') => return Err(format!("seed validate: unknown option {a}\n{USAGE}")),
            a => manifest = Some(PathBuf::from(a)),
        }
        i += 1;
    }
    let manifest = manifest.ok_or_else(|| format!("seed validate: need <world.json>\n{USAGE}"))?;
    let (report, ok) = validate::run(&manifest, res_root, params);
    print!("{report}");
    Ok(ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_and_dates() {
        assert_eq!(parse_bbox("20,54,22,55").unwrap(), [20.0, 54.0, 22.0, 55.0]);
        assert!(parse_bbox("22,54,20,55").is_err());
        assert!(check_date("2026-10-27").is_ok() && check_date("27.10.2026").is_err());
        let d = file_date(env!("CARGO_MANIFEST_DIR"));
        assert!(check_date(&d).is_ok() && d.as_str() > "2020-01-01", "{d}");
    }

    #[test]
    fn airports_command_writes_a_valid_file() {
        let dir = std::env::temp_dir().join(format!("simworld-seed-airports-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("airports.json");
        let t = |p: &str| format!("{}/testdata/ourairports/{p}", env!("CARGO_MANIFEST_DIR"));
        let args: Vec<String> = ["airports", "--countries", "lt", "--source-date", "2026-10-27", &t("airports.csv"), &t("runways.csv"), "-o", out.to_str().unwrap()]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(run(&args).unwrap());
        let text = std::fs::read_to_string(&out).unwrap();
        let f = simworld_core::seed::files::parse_airports("airports.json", &text).unwrap();
        assert_eq!(f.airports.len(), 4);
        assert_eq!(f.source_date, "2026-10-27");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
