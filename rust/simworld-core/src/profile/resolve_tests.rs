use super::defaults_tests::CaptureLog;
use super::ranges::{pattern_matches, FREE_KEY_MAPS};
use super::resolve::join;
use super::*;
use crate::diag::NullLog;
use crate::mesher::{PROFILE_CIVIC, PROFILE_RETAIL};
use crate::mvt::{GROUND_OTHER, ROAD_OTHER, ROAD_PRIMARY};

const DEFAULT_JSON: &str = include_str!("../../../../addons/simworld/profiles/default.json");
const SCHEMA_JSON: &str = include_str!("../../../../addons/simworld/profiles/world-profile.schema.json");
/// `addons/simworld/profiles/default.json` is the compiled-in default, key for key.
#[test]
fn default_json_equals_default_profile() {
    let w: WorldProfile = serde_json::from_str(DEFAULT_JSON).expect("default.json parses");
    assert_eq!(w, WorldProfile::default());
    let p = resolve(Some(("default.json", DEFAULT_JSON)), None, &NullLog).unwrap();
    assert_eq!(p, Profile::default());
    // And it lists every key (a complete reference, not a partial profile).
    let full = serde_json::to_value(WorldProfile::default()).unwrap();
    assert_eq!(key_paths(&parse_json(DEFAULT_JSON, "d").unwrap()), key_paths(&full));
}

/// Schema properties <-> default.json keys, and schema ranges <-> `RANGES`.
#[test]
fn schema_documents_every_key_with_the_same_ranges() {
    let schema = parse_json(SCHEMA_JSON, "schema").unwrap();
    // Collect (path, node) of the schema's object properties; `additionalProperties`
    // objects (free-key maps) are walked as `*`.
    fn walk(node: &Value, path: &str, out: &mut Vec<(String, Value)>) {
        if let Some(props) = node.get("properties").and_then(Value::as_object) {
            for (k, v) in props {
                let p = join(path, k);
                out.push((p.clone(), v.clone()));
                walk(v, &p, out);
            }
        }
        if let Some(ap) = node.get("additionalProperties").filter(|v| v.is_object()) {
            let p = join(path, "*");
            out.push((p.clone(), ap.clone()));
            walk(ap, &p, out);
        }
        if let Some(items) = node.get("items").filter(|v| v.is_object()) {
            let p = format!("{path}[]");
            out.push((p.clone(), items.clone()));
            walk(items, &p, out);
        }
    }
    let mut nodes = Vec::new();
    walk(&schema, "", &mut nodes);
    // default.json keys with free-map entries generalized to `*`.
    let d = parse_json(DEFAULT_JSON, "d").unwrap();
    // A free-key map that is empty in default.json (`buildings.overrides`) documents its
    // entries (`map.*`) in the schema only.
    let empty_maps: Vec<String> = FREE_KEY_MAPS
        .iter()
        .filter(|m| d.pointer(&format!("/{}", m.replace('.', "/"))).and_then(Value::as_object).is_some_and(|o| o.is_empty()))
        .map(|m| format!("{m}.*"))
        .collect();
    let mut schema_keys: Vec<String> = nodes
        .iter()
        .map(|(p, _)| p.clone())
        // Keys inside array items (`terrain.bands[].edge_m`) are not object paths of default.json
        // (arrays are leaves there); their ranges are still checked below.
        .filter(|p| !p.contains("[]") && !empty_maps.iter().any(|m| p == m || p.starts_with(&format!("{m}."))))
        .collect();
    schema_keys.sort();
    schema_keys.dedup();
    let mut json_keys: Vec<String> = key_paths(&d)
        .into_iter()
        .map(|p| {
            let mut out = String::new();
            for seg in p.split('.') {
                let parent = out.clone();
                let s = if FREE_KEY_MAPS.contains(&parent.as_str()) { "*" } else { seg };
                out = join(&out, s);
            }
            out
        })
        .collect();
    json_keys.sort();
    json_keys.dedup();
    assert_eq!(schema_keys, json_keys, "schema properties and default.json keys differ");
    // Every schema node has a description; numeric nodes carry the same range as RANGES.
    let mut numeric = 0;
    for (p, n) in &nodes {
        if !p.ends_with("[]") {
            assert!(n.get("description").and_then(Value::as_str).is_some_and(|s| !s.is_empty()), "{p}: no description");
        }
        let ty = n.get("type").and_then(Value::as_str).unwrap_or("");
        if ty == "number" || ty == "integer" {
            numeric += 1;
            let lo = n.get("minimum").and_then(Value::as_f64);
            let hi = n.get("maximum").and_then(Value::as_f64);
            let want = range_of(p).unwrap_or_else(|| panic!("{p}: numeric key without a RANGES entry"));
            assert_eq!((lo, hi), (Some(want.0), Some(want.1)), "{p}: schema range differs from RANGES");
        }
    }
    assert!(numeric > 40, "only {numeric} numeric schema keys found");
    // Every RANGES pattern is used by some schema key.
    for (pat, _, _) in RANGES {
        assert!(nodes.iter().any(|(p, _)| pattern_matches(pat, p)), "RANGES entry {pat} matches no schema key");
    }
}

#[test]
fn partial_profiles_and_overrides_merge_deeply() {
    let file = r#"{ "profile_version": 7, "lod": { "error_m": { "r1": 2.0 } }, "lights": { "profiles": { "office": { "color": [0.5, 0.6, 0.7] } } } }"#;
    let ov = serde_json::json!({ "bands": { "hysteresis": 1.2 }, "streaming": { "max_fetch_per_band": 6.0 },
        "ground": { "areas": { "beach": { "class": "other", "level": 1, "kinds": ["beach"] } } } });
    let p = resolve(Some(("f.json", file)), Some(&ov), &NullLog).unwrap();
    let d = WorldProfile::default();
    assert_eq!(p.lod.error_m.r1, 2.0);
    assert_eq!(p.lod.error_m.r2, d.lod.error_m.r2);
    assert_eq!(p.bands.hysteresis, 1.2);
    assert_eq!(p.streaming.max_fetch_per_band, 6, "integral floats (Godot) are accepted as integers");
    assert_eq!(p.lights.profiles.office.color, [0.5, 0.6, 0.7]);
    assert_eq!(p.lights.profiles.office.kinds, d.lights.profiles.office.kinds, "kinds kept");
    assert_eq!(p.ground_area_class("beach"), (GROUND_OTHER, 1));
    assert_eq!(p.ground.areas.len(), d.ground.areas.len() + 1);
    // Overrides win over the file.
    let p = resolve(Some(("f.json", file)), Some(&serde_json::json!({"lod": {"error_m": {"r1": 1.5}}})), &NullLog).unwrap();
    assert_eq!(p.lod.error_m.r1, 1.5);
    // Empty sources = defaults.
    assert_eq!(resolve(Some(("e", "{}")), Some(&empty_object()), &NullLog).unwrap(), Profile::default());
}

/// profile_version 2 added `buildings.r0..r2.min_height_m` (default 0 = keep all) and
/// version 3 added `lights.min_ring` (default 2, version 4: 1); a version 1 file without them still loads
/// strictly and keeps the defaults.
#[test]
fn version_1_files_load_with_the_version_2_defaults() {
    assert_eq!(PROFILE_VERSION, 7);
    let v1 = r#"{ "profile_version": 1, "buildings": { "r1": { "dp_m": 3.0, "min_area_m2": 25.0 } } }"#;
    let p = resolve(Some(("v1", v1)), None, &NullLog).unwrap();
    assert_eq!(p.profile_version, PROFILE_VERSION);
    assert_eq!(p.lod(1), Some(LodParams { dp_m: 3.0, min_area_m2: 25.0, min_height_m: 0.0, windows: true }));
    for r in 0..3 {
        assert_eq!(p.lod(r).unwrap().min_height_m, 0.0);
    }
    // A version 1 file is still strict.
    assert!(resolve(Some(("v1", r#"{ "profile_version": 1, "buildings": { "r1": { "min_heigth_m": 6 } } }"#)), None, &NullLog).is_err());
    // The new key per band, and its range.
    let o = serde_json::json!({ "buildings": { "r1": { "min_height_m": 6.0 }, "r2": { "min_height_m": 12.0 } } });
    let p = resolve(None, Some(&o), &NullLog).unwrap();
    assert_eq!([0, 1, 2].map(|r| p.lod(r).unwrap().min_height_m), [0.0, 6.0, 12.0]);
    assert_eq!(range_of("buildings.r0.min_height_m"), Some((0.0, 500.0)));
    let bad = serde_json::json!({ "buildings": { "r0": { "min_height_m": 500.5 } } });
    let e = resolve(None, Some(&bad), &NullLog).unwrap_err();
    assert!(e.contains("buildings.r0.min_height_m"), "{e}");
    // Fade safety cap default (profile_version 2 era).
    assert_eq!(Profile::default().fades.cap_s, 0.75);
}

/// profile_version 7 adds `memory.budget_mb` (default 512 MiB, one number for every
/// platform); a version 6 file still loads strictly with the default.
/// profile_version 7 (api 1.10, Tiles and LOD v2 T3a): `lod.*` loads, the default view
/// reproduces the old edges, ranges and cross-key rules hold.
#[test]
fn version_7_lod_keys_load_and_validate() {
    let p = Profile::default();
    assert_eq!(p.lod.tau_px, 4.0);
    assert_eq!((p.lod.error_m.r1, p.lod.error_m.r2, p.lod.error_m.r3), (2.5660011963983367, 10.691671651659735, 29.93668062464726));
    assert_eq!(p.lod.rear_tau_scale, 2.0);
    assert_eq!(p.lod.adaptive, LodAdaptive { enabled: true, frame_budget_ms: 33.3, min_scale: 0.5, max_scale: 3.0, smoothing_s: 2.0 });
    assert_eq!(p.bands.max_distance_m, 25_000.0);
    let ov = serde_json::json!({ "lod": { "tau_px": 8.0, "error_m": { "r1": 3.0, "r2": 12.0, "r3": 40.0 }, "rear_tau_scale": 3.0,
        "adaptive": { "enabled": false, "frame_budget_ms": 16.7, "min_scale": 0.8, "max_scale": 2.0, "smoothing_s": 1.0 } },
        "bands": { "max_distance_m": 20000.0 } });
    let q = resolve(None, Some(&ov), &NullLog).unwrap();
    assert_eq!(q.lod.tau_px, 8.0);
    assert!(!q.lod.adaptive.enabled);
    assert_eq!(q.bands().error_m, [3.0, 12.0, 40.0]);
    assert_eq!(q.bands().edge_m[3], 20000.0);
    let bad = |v: serde_json::Value| resolve(None, Some(&v), &NullLog).unwrap_err();
    assert!(bad(serde_json::json!({"lod": {"tau_px": 0.1}})).contains("lod.tau_px = 0.1 is out of range [0.5, 64]"));
    assert!(bad(serde_json::json!({"lod": {"error_m": {"r3": 10.0}}})).contains("lod.error_m.r3 = 10 must be greater than lod.error_m.r2"));
    let e = resolve(None, Some(&serde_json::json!({"lod": {"adaptive": {"max_scale": 1.0}}})), &NullLog);
    assert!(e.is_ok(), "max_scale 1 is allowed (min <= 1 <= max)");
    assert!(bad(serde_json::json!({"lod": {"adaptive": {"max_scale": 0.9}}})).contains("lod.adaptive.max_scale"));
    assert!(bad(serde_json::json!({"lod": {"hysteresis": 0.15}})).contains("unknown key `lod.hysteresis`"));
}

/// The fixed band edges are gone in profile_version 7: a current-version source naming them
/// is refused with a message saying what replaced them; an older file migrates
/// (`r3_edge_m` -> `max_distance_m`, r0..r2 dropped with a warning through the log).
#[test]
fn removed_band_edges_refused_in_v7_and_migrated_from_older() {
    for src in [r#"{ "profile_version": 7, "bands": { "r0_edge_m": 500 } }"#, r#"{ "bands": { "r1_edge_m": 2000 } }"#] {
        let e = resolve(Some(("p.json", src)), None, &NullLog).unwrap_err();
        assert!(e.contains("p.json") && e.contains("_edge_m` is removed in profile_version 7") && e.contains("lod.tau_px"), "{e}");
    }
    let e = resolve(Some(("p.json", r#"{ "profile_version": 7, "bands": { "r3_edge_m": 20000 } }"#)), None, &NullLog).unwrap_err();
    assert!(e.contains("`bands.r3_edge_m` is renamed to `bands.max_distance_m`"), "{e}");
    let e = resolve(None, Some(&serde_json::json!({"bands": {"r2_edge_m": 6000}})), &NullLog).unwrap_err();
    assert!(e.contains("profile_overrides") && e.contains("bands.r2_edge_m"), "{e}");
    let v6 = r#"{ "profile_version": 6, "bands": { "r0_edge_m": 450, "r1_edge_m": 2000, "r2_edge_m": 6000, "r3_edge_m": 20000, "hysteresis": 1.2 } }"#;
    let log = CaptureLog::default();
    let p = resolve(Some(("v6.json", v6)), None, &log).unwrap();
    assert_eq!(p.bands.max_distance_m, 20000.0);
    assert_eq!(p.bands.hysteresis, 1.2);
    assert_eq!(p.lod, Lod::default(), "the dropped edges do not move the SSE keys");
    let w = log.0.lock().unwrap();
    assert_eq!(w.len(), 4, "{w:?}");
    assert!(w.iter().any(|m| m.contains("bands.r3_edge_m` migrated to `bands.max_distance_m`")), "{w:?}");
    for k in ["r0_edge_m", "r1_edge_m", "r2_edge_m"] {
        assert!(w.iter().any(|m| m.contains(&format!("`bands.{k}` dropped")) && m.contains("v6.json")), "{w:?}");
    }
}

#[test]
fn version_7_adds_the_memory_budget() {
    let v6 = r#"{ "profile_version": 6, "fades": { "max_active": 2 } }"#;
    let p = resolve(Some(("v6", v6)), None, &NullLog).unwrap();
    assert_eq!(p.memory.budget_mb, 512.0);
    assert!(resolve(Some(("v6", r#"{ "profile_version": 6, "memory": { "budget_gb": 1 } }"#)), None, &NullLog).is_err());
    let p = resolve(None, Some(&serde_json::json!({ "memory": { "budget_mb": 256 } })), &NullLog).unwrap();
    assert_eq!(p.memory.budget_mb, 256.0);
    let e = resolve(None, Some(&serde_json::json!({ "memory": { "budget_mb": 8 } })), &NullLog).unwrap_err();
    assert!(e.contains("memory.budget_mb"), "{e}");
}

#[test]
fn unknown_keys_error_for_own_version_and_warn_for_newer() {
    let typo = r#"{ "profile_version": 1, "bands": { "r0_edge": 450 } }"#;
    let e = resolve(Some(("p.json", typo)), None, &NullLog).unwrap_err();
    assert!(e.contains("unknown key `bands.r0_edge`") && e.contains("p.json"), "{e}");
    // No version = the library's: strict.
    assert!(resolve(Some(("p", r#"{ "nope": 1 }"#)), None, &NullLog).unwrap_err().contains("unknown key `nope`"));
    // Overrides are strict too.
    let e = resolve(None, Some(&serde_json::json!({"cache": {"cap_gb": 1}})), &NullLog).unwrap_err();
    assert!(e.contains("profile_overrides") && e.contains("cache.cap_gb"), "{e}");
    // Newer version: warning, ignored, known keys still apply.
    let newer = r#"{ "profile_version": 99, "bands": { "max_distance_m": 20000, "r4_edge_m": 90000 }, "weather": {} }"#;
    let log = CaptureLog::default();
    let p = resolve(Some(("new.json", newer)), None, &log).unwrap();
    assert_eq!(p.bands.max_distance_m, 20000.0);
    assert_eq!(p.profile_version, PROFILE_VERSION);
    let w = log.0.lock().unwrap();
    assert_eq!(w.len(), 2, "{w:?}");
    assert!(w.iter().any(|m| m.contains("bands.r4_edge_m")) && w.iter().any(|m| m.contains("`weather`")));
    // Bad versions.
    assert!(resolve(Some(("p", r#"{ "profile_version": "1" }"#)), None, &NullLog).is_err());
    assert!(resolve(Some(("p", r#"{ "profile_version": 0 }"#)), None, &NullLog).is_err());
}

#[test]
fn out_of_range_and_wrong_types_name_key_value_and_range() {
    let e = resolve(None, Some(&serde_json::json!({"bands": {"hysteresis": 3.0}})), &NullLog).unwrap_err();
    assert!(e.contains("bands.hysteresis = 3 is out of range [1.01, 2]"), "{e}");
    let e = resolve(None, Some(&serde_json::json!({"lights": {"profiles": {"civic": {"color": [1.0, 2.0, 0.0]}}}})), &NullLog)
        .unwrap_err();
    assert!(e.contains("lights.profiles.*.color") || e.contains("color = 2"), "{e}");
    let e = resolve(None, Some(&serde_json::json!({"streaming": {"max_mesh_per_band": 0}})), &NullLog).unwrap_err();
    assert!(e.contains("streaming.max_mesh_per_band = 0 is out of range [1, 64]"), "{e}");
    let e = resolve(None, Some(&serde_json::json!({"streaming": {"max_mesh_per_band": 2.5}})), &NullLog).unwrap_err();
    assert!(e.contains("streaming.max_mesh_per_band = 2.5 must be an integer"), "{e}");
    let e = resolve(None, Some(&serde_json::json!({"bands": {"max_distance_m": "far"}})), &NullLog).unwrap_err();
    assert!(e.contains("bands.max_distance_m must be a number, got a string"), "{e}");
    let e = resolve(None, Some(&serde_json::json!({"lod": {"error_m": {"r2": 2.0}}})), &NullLog).unwrap_err();
    assert!(e.contains("lod.error_m.r2 = 2 must be greater than lod.error_m.r1 = 2.566"), "{e}");
    let e = resolve(None, Some(&serde_json::json!({"lights": {"cluster_off_agl_m": 4000}})), &NullLog).unwrap_err();
    assert!(e.contains("cluster_off_agl_m"), "{e}");
    let e = resolve(None, Some(&serde_json::json!({"ground": {"r1": {"max_road_class": "highway"}}})), &NullLog).unwrap_err();
    assert!(e.contains("highway"), "{e}");
    let e = resolve(None, Some(&serde_json::json!({"lights": {"profiles": {"office": {"kinds": ["hotel"]}}}})), &NullLog)
        .unwrap_err();
    assert!(e.contains("\"hotel\" is already in another profile"), "{e}");
    let e = resolve(Some(("bad.json", "{ \"bands\": ")), None, &NullLog).unwrap_err();
    assert!(e.contains("bad.json: invalid JSON") && e.contains("line 1"), "{e}");
    assert!(resolve(Some(("arr", "[]")), None, &NullLog).unwrap_err().contains("must be a JSON object"));
}

#[test]
fn profile_lookups_follow_overrides() {
    let p = resolve(
        None,
        Some(&serde_json::json!({
            "ground": { "roads": { "primary": { "width_m": 12.0, "highways": ["trunk", "primary", "busway"] },
                                   "lit_values": ["yes"], "minor_paths": ["footway"] },
                        "other_area_level": 2 },
            "lights": { "profiles": { "retail": { "color": [1.0, 0.0, 0.0], "kinds": ["retail", "kiosk"] } },
                        "landuse_fallback": { "none": "civic" } } })),
        &NullLog,
    )
    .unwrap();
    assert_eq!(p.road_class("busway_link"), (ROAD_PRIMARY, true, false));
    assert_eq!(p.road_width_m(ROAD_PRIMARY, false), 12.0);
    assert_eq!(p.road_width_m(ROAD_PRIMARY, true), 7.2);
    assert_eq!(p.road_class("path"), (ROAD_OTHER, false, false));
    assert!(p.is_lit(" yes ") && !p.is_lit("night"));
    assert_eq!(p.ground_area_class("education"), (GROUND_OTHER, 2));
    assert_eq!(p.profile_for_kind("shop"), None);
    assert_eq!(p.profile_color(PROFILE_RETAIL), [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(p.landuse_profile(None), PROFILE_CIVIC);
}
