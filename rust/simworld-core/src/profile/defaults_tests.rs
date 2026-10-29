use super::*;
use crate::diag::{Log, NullLog};
use crate::mesher::{
    PROFILE_CIVIC, PROFILE_DARK, PROFILE_INDUSTRIAL, PROFILE_LANDMARK, PROFILE_OFFICE, PROFILE_RESIDENTIAL, PROFILE_RETAIL,
};
use crate::mvt::{ROAD_OTHER, ROAD_RESIDENTIAL, ROAD_SECONDARY};
use std::sync::Mutex;

#[derive(Default)]
pub(super) struct CaptureLog(pub(super) Mutex<Vec<String>>);
impl Log for CaptureLog {
    fn info(&self, _: &str) {}
    fn warn(&self, m: &str) {
        self.0.lock().unwrap().push(m.to_string());
    }
    fn error(&self, _: &str) {}
}

/// The compiled-in defaults are exactly the spike's constants (0.5.0).
#[test]
fn defaults_equal_the_spike_constants() {
    let p = Profile::default();
    let b = p.bands();
    // api 1.10: the SSE edges at the default view (1080 px, 60 deg, tau 4) are the spike's
    // fixed edges, bit for bit.
    assert_eq!(b.edge_m, [600.0, 2500.0, 7000.0, 25_000.0]);
    assert_eq!(b.edge_m[3], 25_000.0);
    assert_eq!((b.hysteresis, b.look_ahead_s, b.look_ahead_clamp, b.default_tile_height_m), (1.15, 20.0, 0.5, 60.0));
    assert_eq!(p.lod(0), Some(LodParams { dp_m: 0.5, min_area_m2: 0.0, min_height_m: 0.0, windows: true }));
    assert_eq!(p.lod(1), Some(LodParams { dp_m: 2.0, min_area_m2: 20.0, min_height_m: 0.0, windows: true }));
    assert_eq!(p.lod(2), Some(LodParams { dp_m: 4.0, min_area_m2: 50.0, min_height_m: 0.0, windows: false }));
    assert_eq!(p.lod(3), None);
    let gp = |a, m, r, c, minor, s| Some(GroundParams { area_dp_m: a, min_area_m2: m, road_dp_m: r, max_road_class: c, minor_paths: minor, step_m: s });
    assert_eq!(p.ground_params(0), gp(0.5, 0.0, 0.5, ROAD_OTHER, true, 0.02f32));
    assert_eq!(p.ground_params(1), gp(2.0, 200.0, 2.0, ROAD_OTHER, false, 0.06f32));
    assert_eq!(p.ground_params(2), gp(6.0, 2500.0, 5.0, ROAD_RESIDENTIAL, false, 0.15f32));
    assert_eq!(p.ground_params(3), gp(20.0, 40000.0, 15.0, ROAD_SECONDARY, false, 0.5f32));
    assert_eq!(p.profile_color(PROFILE_RESIDENTIAL), [1.0, 0.655, 0.341, 1.0]);
    assert_eq!(p.profile_color(PROFILE_OFFICE), [0.86, 0.9, 1.0, 1.0]);
    assert_eq!(p.profile_color(PROFILE_RETAIL), [1.0, 0.98, 0.95, 1.0]);
    assert_eq!(p.profile_color(PROFILE_INDUSTRIAL), [1.0, 0.55, 0.15, 1.0]);
    assert_eq!(p.profile_color(PROFILE_CIVIC), [1.0, 0.706, 0.42, 1.0]);
    assert_eq!(p.profile_color(crate::mesher::PROFILE_UNKNOWN), [1.0, 0.655, 0.341, 1.0]);
    assert_eq!(p.cluster_cell_m(), 100.0f32);
    let w = p.world();
    assert_eq!((w.lights.cluster_on_agl_m, w.lights.cluster_off_agl_m), (3300.0, 2700.0));
    assert_eq!((w.streaming.max_fetch_per_band, w.streaming.max_mesh_per_band, w.streaming.redeliveries_per_frame), (4, 8, 1));
    assert_eq!(w.fades.cap_s, 0.75);
    assert_eq!(w.origin.recenter_distance_m, 5000.0);
    assert_eq!((w.cache.cap_mb * 1024.0 * 1024.0) as u64, 300 * 1024 * 1024);
    assert_eq!(p.backoff(1), std::time::Duration::from_millis(500));
    assert_eq!(p.backoff(3), std::time::Duration::from_millis(2000));
    assert_eq!(p.backoff(20), std::time::Duration::from_secs(30));
    // Landuse fallback (design doc table).
    assert_eq!(p.landuse_profile(Some(Landuse::Commercial)), PROFILE_OFFICE);
    assert_eq!(p.landuse_profile(Some(Landuse::Retail)), PROFILE_RETAIL);
    assert_eq!(p.landuse_profile(Some(Landuse::Industrial)), PROFILE_INDUSTRIAL);
    assert_eq!(p.landuse_profile(Some(Landuse::Civic)), PROFILE_CIVIC);
    for l in [Some(Landuse::Residential), Some(Landuse::Other), None] {
        assert_eq!(p.landuse_profile(l), PROFILE_RESIDENTIAL);
    }
    assert_eq!(p.profile_for_kind("hotel"), Some(PROFILE_RESIDENTIAL));
    assert_eq!(p.profile_for_kind("bank"), Some(PROFILE_OFFICE));
    assert_eq!(p.profile_for_kind("embassy"), Some(PROFILE_CIVIC));
    assert_eq!(p.profile_for_kind("yes"), None);
}

/// profile_version 4 (docs/spec-night-windows.md section 4): seven profiles, windows on
/// residential / office / retail / civic, floodlight on landmark, kind moves, resolution keys.
#[test]
fn night_window_profiles_load_with_their_defaults() {
    let p = Profile::default();
    let lp = &p.lights.profiles;
    // Seven named profiles, ids 0-4, 6, 7.
    let ids: Vec<u8> = [
        LightProfileName::Residential,
        LightProfileName::Office,
        LightProfileName::Retail,
        LightProfileName::Industrial,
        LightProfileName::Civic,
        LightProfileName::Landmark,
        LightProfileName::Dark,
    ]
    .map(LightProfileName::id)
    .to_vec();
    assert_eq!(ids, vec![0, 1, 2, 3, 4, 6, 7]);
    assert_eq!(p.profile_color(PROFILE_LANDMARK), [1.0, 0.75, 0.45, 1.0]);
    assert_eq!(p.profile_color(PROFILE_DARK), [0.0, 0.0, 0.0, 1.0]);
    // Windows: widths, 24 hourly shares, none on industrial / landmark / dark.
    let widths: Vec<Option<f32>> = (0..PROFILE_COUNT as u8).map(|i| p.window_width_m(i)).collect();
    assert_eq!(widths, vec![Some(3.0), Some(2.0), Some(4.0), None, Some(3.5), None, None, None]);
    for w in [&lp.residential, &lp.office, &lp.retail, &lp.civic] {
        let w = w.windows.as_ref().unwrap();
        assert_eq!(w.lit_share.len(), 24);
        assert!(w.lit_share.iter().all(|x| (0.0..=1.0).contains(x)));
    }
    assert_eq!(lp.residential.windows.as_ref().unwrap().lit_share[19], 0.45);
    let office = &lp.office.windows.as_ref().unwrap().lit_share;
    assert_eq!((office[0], office[7], office[12], office[21], office[23]), (0.05, 0.375, 0.70, 0.15, 0.10));
    let retail = &lp.retail.windows.as_ref().unwrap().lit_share;
    assert_eq!((retail[3], retail[8], retail[15], retail[21], retail[22]), (0.10, 0.5, 0.90, 0.5, 0.10));
    let civic = &lp.civic.windows.as_ref().unwrap().lit_share;
    assert_eq!((civic[0], civic[7], civic[10], civic[20], civic[23]), (0.03, 0.315, 0.60, 0.10, 0.065));
    assert_eq!(lp.landmark.floodlight, Some(Floodlight { strength: 1.2, fade_height_m: 25.0 }));
    assert!(p.warnings().is_empty(), "{:?}", p.warnings());
    // Kind moves (not copies) out of civic and industrial.
    for k in ["church", "cathedral", "chapel", "mosque", "synagogue", "temple", "monastery", "shrine", "castle", "tower"] {
        assert_eq!(p.profile_for_kind(k), Some(PROFILE_LANDMARK), "{k}");
    }
    for k in ["garage", "garages", "shed", "barn", "service", "transformer_tower", "greenhouse", "storage_tank", "ruins", "chimney"] {
        assert_eq!(p.profile_for_kind(k), Some(PROFILE_DARK), "{k}");
    }
    assert_eq!(p.profile_for_kind("hangar"), Some(PROFILE_INDUSTRIAL));
    assert_eq!(p.profile_for_kind("religious"), Some(PROFILE_CIVIC));
    // Resolution keys.
    assert_eq!(p.min_window_wall_m(), 2.0);
    assert!(p.is_slender_tower(42.0, 49.0) && !p.is_slender_tower(20.0, 49.0) && !p.is_slender_tower(400.0, 150.0));
    assert!(!p.is_slender_tower(42.0, 0.0));
    let tags = |f: fn(&mut BuildingTags)| {
        let mut t = BuildingTags::default();
        f(&mut t);
        t
    };
    assert!(p.is_dark_state(&tags(|t| t.historic = Some("ruins".into()))));
    assert!(!p.is_dark_state(&tags(|t| t.historic = Some("castle".into()))));
    assert!(p.is_dark_state(&tags(|t| t.ruins = true)));
    assert!(p.is_dark_state(&tags(|t| t.disused = true)));
    assert!(p.is_dark_state(&tags(|t| t.abandoned = true)));
    assert!(!p.is_dark_state(&BuildingTags::default()));
    assert_eq!(p.override_profile(OsmType::Way, 1), None);
    // Overrides by OSM id, through JSON (Godot passes them in profile_overrides).
    let o = serde_json::json!({ "buildings": { "overrides": { "w123456": "landmark", "r7": "dark", "n5": "office" },
                                               "dark_states": ["man_made=chimney", "historic"] } });
    let p = resolve(None, Some(&o), &NullLog).unwrap();
    assert_eq!(p.override_profile(OsmType::Way, 123_456), Some(PROFILE_LANDMARK));
    assert_eq!(p.override_profile(OsmType::Relation, 7), Some(PROFILE_DARK));
    assert_eq!(p.override_profile(OsmType::Node, 5), Some(PROFILE_OFFICE));
    assert_eq!(p.override_profile(OsmType::Relation, 123_456), None, "type is part of the id");
    assert!(p.is_dark_state(&tags(|t| t.man_made = Some("chimney".into()))));
    assert!(!p.is_dark_state(&tags(|t| t.man_made = Some("tower".into()))));
    assert!(p.is_dark_state(&tags(|t| t.historic = Some("castle".into()))), "bare `historic` = any value");
    assert!(!p.is_dark_state(&tags(|t| t.ruins = true)), "the list replaces the defaults");
    // A version 3 file (no new keys) loads with the new defaults.
    let v3 = r#"{ "profile_version": 3, "lights": { "min_ring": 1 } }"#;
    let p = resolve(Some(("v3", v3)), None, &NullLog).unwrap();
    assert_eq!(p.lights.profiles.landmark, Profile::default().lights.profiles.landmark);
    assert_eq!(p.buildings.slender_tower, SlenderTower { ratio: 3.0, max_area_m2: 150.0 });
    // Partial windows override merges deeply (lit_share kept).
    let p = resolve(None, Some(&serde_json::json!({"lights": {"profiles": {"office": {"windows": {"width_m": 2.5}}}}})), &NullLog)
        .unwrap();
    assert_eq!(p.window_width_m(PROFILE_OFFICE), Some(2.5));
    assert_eq!(p.lights.profiles.office.windows.as_ref().unwrap().lit_share.len(), 24);
}

#[test]
fn night_window_profile_validation() {
    let err = |o: serde_json::Value| resolve(None, Some(&o), &NullLog).unwrap_err();
    // lit_share length.
    let e = err(serde_json::json!({"lights": {"profiles": {"retail": {"windows": {"lit_share": [0.5, 0.4]}}}}}));
    assert!(e.contains("lights.profiles.retail.windows.lit_share has 2 values, needs 24"), "{e}");
    // lit_share range.
    let mut share = vec![0.1; 24];
    share[5] = 1.5;
    let e = err(serde_json::json!({"lights": {"profiles": {"retail": {"windows": {"lit_share": share}}}}}));
    assert!(e.contains("lights.profiles.retail.windows.lit_share = 1.5 is out of range [0, 1]"), "{e}");
    // A kind in two profiles (also across the new ones).
    let e = err(serde_json::json!({"lights": {"profiles": {"civic": {"kinds": ["school", "church"]}}}}));
    assert!(e.contains("\"church\" is already in another profile"), "{e}");
    let e = err(serde_json::json!({"lights": {"profiles": {"dark": {"kinds": ["castle"]}}}}));
    assert!(e.contains("\"castle\" is already in another profile"), "{e}");
    // Bad override ids.
    for bad in ["123", "x123", "w", "w12a", "W123", "w-1"] {
        let e = err(serde_json::json!({"buildings": {"overrides": {bad: "landmark"}}}));
        assert!(e.contains(&format!("buildings.overrides: \"{bad}\" is not an OSM id")), "{bad}: {e}");
    }
    let e = err(serde_json::json!({"buildings": {"overrides": {"w1": "castle"}}}));
    assert!(e.contains("castle"), "{e}");
    // dark_states over the exported state attributes only.
    for bad in ["building=ruins", "ruins=no", "historic=", "windows"] {
        let e = err(serde_json::json!({"buildings": {"dark_states": [bad]}}));
        assert!(e.contains(&format!("buildings.dark_states: \"{bad}\"")), "{bad}: {e}");
    }
    // `windows` on dark / landmark / industrial and `floodlight` elsewhere: JSON shape...
    let win = serde_json::json!({"width_m": 3.0, "lit_share": vec![0.1; 24]});
    for name in ["dark", "landmark", "industrial"] {
        let e = err(serde_json::json!({"lights": {"profiles": {name: {"windows": win.clone()}}}}));
        assert!(e.contains(&format!("unknown key `lights.profiles.{name}.windows`")) && e.contains("civic only"), "{e}");
    }
    let e = err(serde_json::json!({"lights": {"profiles": {"civic": {"floodlight": {"strength": 1.0, "fade_height_m": 9.0}}}}}));
    assert!(e.contains("unknown key `lights.profiles.civic.floodlight`"), "{e}");
    // ... and the typed rules (Rust hosts building a WorldProfile directly).
    let mut w = WorldProfile::default();
    w.lights.profiles.dark.windows = w.lights.profiles.residential.windows.clone();
    assert!(Profile::new(w).unwrap_err().contains("lights.profiles.dark.windows: the dark profile has no windows"));
    let mut w = WorldProfile::default();
    w.lights.profiles.landmark.floodlight = None;
    w.lights.profiles.office.floodlight = Some(Floodlight { strength: 1.0, fade_height_m: 5.0 });
    let e = Profile::new(w).unwrap_err();
    assert!(e.contains("lights.profiles.landmark.floodlight is required"), "{e}");
    assert!(e.contains("lights.profiles.office.floodlight: only the landmark profile has a floodlight"), "{e}");
    // Ranges of the new numbers.
    let e = err(serde_json::json!({"buildings": {"slender_tower": {"ratio": 0.1}}}));
    assert!(e.contains("buildings.slender_tower.ratio = 0.1 is out of range"), "{e}");
    let e = err(serde_json::json!({"lights": {"profiles": {"landmark": {"floodlight": {"fade_height_m": 0}}}}}));
    assert!(e.contains("lights.profiles.landmark.floodlight.fade_height_m = 0 is out of range"), "{e}");
}

/// A `lit_share` that rises between 18 h and 3 h loads, with one warning naming the hours.
#[test]
fn lit_share_rising_in_the_evening_warns() {
    let mut share = Profile::default().lights.profiles.office.windows.as_ref().unwrap().lit_share.clone();
    share[1] = 0.3; // 0 h 0.05 -> 1 h 0.3
    let log = CaptureLog::default();
    let p = resolve(None, Some(&serde_json::json!({"lights": {"profiles": {"office": {"windows": {"lit_share": share}}}}})), &log)
        .unwrap();
    assert_eq!(p.warnings().len(), 1);
    let w = log.0.lock().unwrap();
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("lights.profiles.office.windows.lit_share rises from 0 h (0.05) to 1 h (0.3)"), "{}", w[0]);
    // Rising in the morning (3 h .. 18 h) is fine; so is the 23 h -> 0 h wrap when it falls.
    let log = CaptureLog::default();
    resolve(None, Some(&serde_json::json!({"lights": {"profiles": {"office": {"windows": {"width_m": 2.0}}}}})), &log).unwrap();
    assert!(log.0.lock().unwrap().is_empty());
    let mut share = vec![0.1; 24];
    share[23] = 0.05;
    share[0] = 0.2; // 23 h -> 0 h rises
    let p = resolve(None, Some(&serde_json::json!({"lights": {"profiles": {"civic": {"windows": {"lit_share": share}}}}})), &NullLog)
        .unwrap();
    assert!(p.warnings()[0].contains("rises from 23 h"), "{:?}", p.warnings());
}
