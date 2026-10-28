//! Tests of the seed files: the two-cities fixture loads; each error of section 4.6 that S0 can
//! check has a failing fixture; the schemas use only what the checker supports.

use std::collections::BTreeMap;

use serde_json::Value;

use super::*;

/// The shared two-cities world (spec 11a.1): the bench scene and `cargo test` read one copy.
const DIR: &str = "low-poly-map/world/two-cities/";

fn fixture() -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    for (k, v) in [
        ("world.json", include_str!("../../../../low-poly-map/world/two-cities/world.json")),
        ("airports.json", include_str!("../../../../low-poly-map/world/two-cities/airports.json")),
        ("water.json", include_str!("../../../../low-poly-map/world/two-cities/water.json")),
        ("roads.json", include_str!("../../../../low-poly-map/world/two-cities/roads.json")),
        ("cities/vilnius.json", include_str!("../../../../low-poly-map/world/two-cities/cities/vilnius.json")),
        ("cities/kaunas.json", include_str!("../../../../low-poly-map/world/two-cities/cities/kaunas.json")),
    ] {
        m.insert(k.to_string(), v.to_string());
    }
    m
}

fn load(files: &BTreeMap<String, String>) -> Result<SeedFiles, LoadError> {
    let mut read = |p: &str| files.get(p).map(|t| (format!("{DIR}{p}"), t.clone())).ok_or_else(|| "no such file".to_string());
    load_world(("world.json", &files["world.json"]), &mut read)
}

/// Edit one file's JSON in place.
fn edit(files: &mut BTreeMap<String, String>, name: &str, f: impl FnOnce(&mut Value)) {
    let mut v: Value = serde_json::from_str(&files[name]).unwrap();
    f(&mut v);
    files.insert(name.to_string(), serde_json::to_string(&v).unwrap());
}

fn errors_of(files: &BTreeMap<String, String>) -> String {
    match load(files) {
        Ok(_) => panic!("expected the load to fail"),
        Err(e) => e.errors.join("\n"),
    }
}

#[test]
fn two_cities_fixture_loads() {
    let w = load(&fixture()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(w.manifest.world_seed, "bench-two-cities");
    assert_eq!(w.cities.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["vilnius", "kaunas"]);
    let idents: Vec<&str> = w.airports.iter().map(|a| a.ident.as_str()).collect();
    for want in ["EYKA", "EYVI", "EYPA", "EYPR", "EYSA"] {
        assert!(idents.contains(&want), "{want} selected: {idents:?}");
    }
    assert!(!idents.contains(&"LT-0001") && !idents.contains(&"EYNE"), "closed and heliport types filtered");
    assert!(!idents.contains(&"EYAL"), "an airport without runways is not selected");
    assert!(idents.windows(2).all(|p| p[0] < p[1]), "sorted by ident");
    // The EYPA override of spec 4.1 (OurAirports ends 180 m off), quantized.
    let eypa = w.airports.iter().find(|a| a.ident == "EYPA").unwrap();
    let r = &eypa.runways[0];
    assert_eq!((r.le_lat, r.le_lon, r.he_lat, r.he_lon), (Some(55.9628424), Some(21.0896732), Some(55.9833478), Some(21.0979302)));
    assert_eq!(r.length_ft, Some(7679.0));
    // Closed runways are dropped from the selection, kept in all_airports.
    let eytl = w.airports.iter().find(|a| a.ident == "EYTL").unwrap();
    assert_eq!(eytl.runways.iter().map(Runway::key).collect::<Vec<_>>(), ["05R/23L", "11/29"]);
    assert!(w.all_airports.iter().any(|a| a.ident == "EYTL" && a.runways.iter().any(|r| r.key() == "05L/23R" && r.closed)));
    // Approximate runways (an end missing) are warnings, not errors.
    assert!(w.warnings.iter().any(|x| x.contains("EYPR") && x.contains("04/22")), "{:?}", w.warnings);
    // Water: the Lithuanian sea (two polygons first) plus lakes and rivers; closing duplicates dropped.
    assert!(w.water.polygons.len() > 4000, "{}", w.water.polygons.len());
    assert_eq!(w.water.polygons.iter().filter(|p| p.kind == WaterKind::Sea).count(), 2);
    assert_eq!(w.water.polygons[0].kind, WaterKind::Sea);
    for p in &w.water.polygons {
        for r in &p.rings {
            assert_ne!(r.first(), r.last(), "closing duplicate dropped");
        }
    }
    assert_eq!(w.roads.as_ref().unwrap().roads.len(), 2);
    assert_eq!(w.cities[1].landmarks[0].kind, "cathedral");
    // Ring geometry of the fixture cities.
    assert_eq!(w.cities[0].ring_count(), 14);
    assert!(w.cities[0].cell_count(1.0) > 300, "{}", w.cities[0].cell_count(1.0));
}

#[test]
fn load_is_deterministic_and_quantized() {
    let a = load(&fixture()).unwrap();
    let b = load(&fixture()).unwrap();
    assert_eq!(a.airports, b.airports);
    assert_eq!(a.water, b.water);
    let mut f = fixture();
    edit(&mut f, "cities/kaunas.json", |v| v["center"]["lat"] = Value::from(54.898_500_04));
    let c = load(&f).unwrap();
    assert_eq!(c.cities[1].center.lat, 54.8985);
    for p in a.water.polygons.iter().flat_map(|p| p.rings.iter().flatten()) {
        assert_eq!(*p, [q7(p[0]), q7(p[1])]);
    }
}

#[test]
fn schemas_are_supported_and_documented() {
    for (name, v) in schemas() {
        schema::check_schema_keywords(&v).unwrap_or_else(|e| panic!("{name}: {e}"));
        // Every property node carries a description (or is a $ref to a documented def).
        fn walk(n: &Value, path: &str, name: &str) {
            if let Some(props) = n.get("properties").and_then(Value::as_object) {
                for (k, p) in props {
                    let documented = p.get("description").and_then(Value::as_str).is_some_and(|s| !s.is_empty()) || p.get("$ref").is_some();
                    assert!(documented, "{name}: {path}.{k} has no description");
                    walk(p, &format!("{path}.{k}"), name);
                }
            }
            for key in ["items", "additionalProperties"] {
                if let Some(x) = n.get(key).filter(|x| x.is_object()) {
                    walk(x, &format!("{path}.{key}"), name);
                }
            }
            if let Some(defs) = n.get("$defs").and_then(Value::as_object) {
                for (k, d) in defs {
                    assert!(d.get("description").is_some(), "{name}: $defs.{k} has no description");
                    walk(d, &format!("$defs.{k}"), name);
                }
            }
        }
        walk(&v, "", name);
    }
}

#[test]
fn schema_errors_name_file_key_and_value() {
    let mut f = fixture();
    edit(&mut f, "cities/kaunas.json", |v| {
        v["radius_km"] = Value::from(45.0);
        v["layout"]["cell_sise_m"] = Value::from(800);
        v["districts"][1]["near"] = Value::from("lake");
    });
    let e = errors_of(&f);
    assert!(e.contains("cities/kaunas.json: radius_km: 45.0 is above the maximum 30"), "{e}");
    assert!(e.contains("cities/kaunas.json: layout: unknown key `cell_sise_m`"), "{e}");
    assert!(e.contains("districts[1].near: \"lake\" is not one of"), "{e}");
    let mut f = fixture();
    edit(&mut f, "world.json", |v| v["format"] = Value::from(2));
    let e = errors_of(&f);
    assert!(e.contains("world.json: format: 2 must be 1"), "{e}");
    let mut f = fixture();
    edit(&mut f, "water.json", |v| {
        v["polygons"][0]["rings"][0][2] = serde_json::json!([24.24, 95.0]);
        v["outside"] = Value::Null;
    });
    let e = errors_of(&f);
    assert!(e.contains("water.json: polygons[0].rings[0][2][1]: 95.0 is above the maximum 85"), "{e}");
    assert!(e.contains("water.json: outside: null is not of type string"), "{e}");
    let mut f = fixture();
    f.insert("roads.json".into(), "{ \"format\": 1, \"roads\": [ }".into());
    assert!(errors_of(&f).contains("roads.json: invalid JSON"), "{}", errors_of(&f));
    let mut f = fixture();
    f.remove("water.json");
    assert!(errors_of(&f).contains("cannot read water.json"));
}

#[test]
fn section_4_6_errors() {
    type Case = (&'static str, &'static str, fn(&mut Value), &'static str);
    let cases: &[Case] = &[
        ("duplicate city id", "cities/kaunas.json", |v| v["id"] = Value::from("vilnius"), "duplicate city id \"vilnius\""),
        (
            "footprints overlap",
            "cities/kaunas.json",
            |v| v["center"] = serde_json::json!({ "lat": 54.75, "lon": 25.10 }),
            "footprints overlap",
        ),
        ("unknown airport", "cities/kaunas.json", |v| v["airports"] = serde_json::json!(["EVRA"]), "\"EVRA\" is not in the airport file"),
        ("cells and share", "cities/kaunas.json", |v| v["districts"][0]["share"] = Value::from(0.1), "has both `cells` and `share`"),
        (
            "neither cells nor share",
            "cities/kaunas.json",
            |v| {
                v["districts"][0].as_object_mut().unwrap().remove("cells");
            },
            "has neither `cells` nor `share`",
        ),
        ("rings outside", "cities/kaunas.json", |v| v["districts"][2]["rings"] = serde_json::json!([1, 40]), "rings: [1, 40]: the city has rings 0..9"),
        ("rings reversed", "cities/kaunas.json", |v| v["districts"][2]["rings"] = serde_json::json!([3, 1]), "min above max"),
        ("cells exceed", "cities/kaunas.json", |v| v["districts"][0]["cells"] = Value::from(5000), "requests take 5001 cells"),
        ("bad style", "cities/kaunas.json", |v| v["districts"][0]["style"] = Value::from("houses"), "is not a style of old_town"),
        (
            "real url and path",
            "cities/vilnius.json",
            |v| v["real"]["path"] = Value::from("/tmp/v.pmtiles"),
            "has both `url` and `path`",
        ),
        ("real scheme", "cities/vilnius.json", |v| v["real"]["url"] = Value::from("ftp://x/v.pmtiles"), "must start with http:// or https://"),
        (
            "real res path",
            "cities/vilnius.json",
            |v| v["real"] = serde_json::json!({ "path": "res://world/v.pmtiles" }),
            "is under res://",
        ),
        ("duplicate road", "roads.json", |v| v["roads"][1]["id"] = Value::from("a1-vilnius-kaunas"), "duplicate road id"),
        ("one point road", "roads.json", |v| v["roads"][1]["points"] = serde_json::json!([[24.0, 54.9]]), "1 items, at least 2 required"),
        ("unknown road end", "roads.json", |v| v["roads"][0]["to"] = Value::from("klaipeda"), "\"klaipeda\" is neither a city id"),
        ("unknown road airport", "roads.json", |v| v["roads"][1]["to"] = Value::from("airport:EVRA"), "\"airport:EVRA\" names no selected airport"),
        (
            "duplicate landmark",
            "cities/kaunas.json",
            |v| {
                let l = v["landmarks"][0].clone();
                v["landmarks"].as_array_mut().unwrap().push(l);
            },
            "duplicate landmark id \"cathedral\"",
        ),
        ("landmark model", "cities/kaunas.json", |v| v["landmarks"][0]["model"] = Value::from("user://x.obj"), "must be a res:// .glb"),
        (
            "landmark footprint forms",
            "cities/kaunas.json",
            |v| v["landmarks"][0]["footprint"] = serde_json::json!([[23.88, 54.89], [23.89, 54.89], [23.89, 54.9]]),
            "has both `footprint_m` and `footprint`",
        ),
        (
            "override unknown ident",
            "world.json",
            |v| v["airports"]["overrides"]["EVRA"] = serde_json::json!({ "lat": 56.9 }),
            "airports.overrides.EVRA: unknown airport ident",
        ),
        (
            "override unknown runway",
            "world.json",
            |v| v["airports"]["overrides"]["EYKA"]["runways"]["09/27"] = serde_json::json!({ "width_ft": 150 }),
            "airports.overrides.EYKA.runways.09/27: unknown runway \"09/27\" (the file has 08/26)",
        ),
        (
            "override add existing",
            "world.json",
            |v| {
                v["airports"]["overrides"]["EYKS"] = serde_json::json!({ "add": [
                    { "le": "09R", "he": "27L", "length_ft": 2000, "width_ft": 100, "surface": "ASP", "lighted": false, "closed": false }
                ] });
            },
            "already exists",
        ),
        (
            "duplicate airport ident",
            "airports.json",
            |v| v["airports"][1]["ident"] = Value::from("EYKA"),
            "duplicate ident EYKA",
        ),
    ];
    for (name, file, f, want) in cases {
        let mut files = fixture();
        edit(&mut files, file, f);
        let e = match load(&files) {
            Ok(_) => panic!("{name}: expected the load to fail"),
            Err(e) => e.errors.join("\n"),
        };
        assert!(e.contains(want), "{name}: want `{want}` in:\n{e}");
    }
}

#[test]
fn section_4_6_warnings() {
    let mut f = fixture();
    edit(&mut f, "cities/kaunas.json", |v| {
        v["districts"][2]["share"] = Value::from(0.95);
        v["landmarks"][0]["lat"] = Value::from(55.2);
        v["airports"] = serde_json::json!(["EYKA", "LT-0001"]);
    });
    edit(&mut f, "roads.json", |v| v["roads"][1]["points"][0] = serde_json::json!([23.5, 55.15]));
    let w = load(&f).unwrap_or_else(|e| panic!("{e}"));
    let all = w.warnings.join("\n");
    assert!(all.contains("cells + share x land cells"), "{all}");
    assert!(all.contains("outside the city footprint"), "{all}");
    assert!(all.contains("\"LT-0001\" is not selected"), "{all}");
    assert!(all.contains("km outside city kaunas"), "{all}");
}

#[test]
fn override_remove_and_fields() {
    let mut f = fixture();
    edit(&mut f, "world.json", |v| {
        v["airports"]["overrides"]["EYVI"] = serde_json::json!({
            "lat": 54.63411111, "elevation_ft": 650, "type": "medium_airport",
            "runways": { "01/19": { "width_ft": 150, "lighted": false, "le_displaced_threshold_ft": 300 } }
        });
        v["airports"]["overrides"]["EYDR"] = serde_json::json!({ "runways": { "03/21": { "remove": true } } });
        v["airports"]["overrides"]["EYKS"] = serde_json::json!({ "add": [
            { "le": "04", "he": "22", "length_ft": 2000, "width_ft": 100, "surface": "GRS", "lighted": false, "closed": false,
              "le_lat": 54.9640001, "le_lon": 24.0001, "he_lat": 54.9680, "he_lon": 24.0080 } ] });
    });
    let w = load(&f).unwrap_or_else(|e| panic!("{e}"));
    let eyvi = w.airports.iter().find(|a| a.ident == "EYVI").unwrap();
    assert_eq!((eyvi.lat, eyvi.elevation_ft, eyvi.kind.as_str()), (54.6341111, Some(650.0), "medium_airport"));
    let r = &eyvi.runways[0];
    assert_eq!((r.width_ft, r.lighted, r.le_displaced_threshold_ft), (Some(150.0), false, Some(300.0)));
    // EYDR lost its only runway: not selected any more (no open runway).
    assert!(!w.airports.iter().any(|a| a.ident == "EYDR"));
    assert!(!w.warnings.iter().any(|x| x.contains("EYDR")));
    // An added runway joins the airport (sorted by key, quantized).
    let eyks: Vec<String> = w.airports.iter().find(|a| a.ident == "EYKS").unwrap().runways.iter().map(Runway::key).collect();
    assert_eq!(eyks, ["04/22", "09L/27R", "09R/27L"]);
}
