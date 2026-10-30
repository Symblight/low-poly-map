//! Allowed ranges of the numeric keys and the range check.

use serde_json::Value;

// ---------------------------------------------------------------------------------------------
// Ranges (single source for validation; the JSON Schema must agree, see tests)

/// Allowed range per numeric key. `*` matches any one key (free-key maps and the r0..r2
/// objects); `[]` is every element of an array.
pub const RANGES: &[(&str, f64, f64)] = &[
    ("profile_version", 1.0, 1_000_000.0),
    ("bands.max_distance_m", 50.0, 100_000.0),
    ("bands.hysteresis", 1.01, 2.0),
    ("bands.look_ahead_s", 0.0, 120.0),
    ("bands.look_ahead_clamp", 0.0, 1.0),
    ("bands.default_tile_height_m", 0.0, 400.0),
    ("lod.tau_px", 0.5, 64.0),
    ("lod.max_tiles", 0.0, 1_000_000.0),
    ("lod.error_m.*", 0.01, 1000.0),
    ("lod.rear_tau_scale", 1.0, 16.0),
    ("lod.adaptive.frame_budget_ms", 1.0, 1000.0),
    ("lod.adaptive.min_scale", 0.05, 1.0),
    ("lod.adaptive.max_scale", 1.0, 16.0),
    ("lod.adaptive.smoothing_s", 0.0, 60.0),
    ("buildings.*.dp_m", 0.0, 50.0),
    ("buildings.*.min_area_m2", 0.0, 100_000.0),
    ("buildings.*.min_height_m", 0.0, 500.0),
    ("buildings.slender_tower.ratio", 0.5, 50.0),
    ("buildings.slender_tower.max_area_m2", 0.0, 100_000.0),
    ("buildings.min_window_wall_m", 0.0, 100.0),
    ("ground.r0.area_dp_m", 0.0, 50.0),
    ("ground.r1.area_dp_m", 0.0, 50.0),
    ("ground.r2.area_dp_m", 0.0, 50.0),
    ("ground.r3.area_dp_m", 0.0, 50.0),
    ("ground.r0.min_area_m2", 0.0, 1_000_000.0),
    ("ground.r1.min_area_m2", 0.0, 1_000_000.0),
    ("ground.r2.min_area_m2", 0.0, 1_000_000.0),
    ("ground.r3.min_area_m2", 0.0, 1_000_000.0),
    ("ground.r0.road_dp_m", 0.0, 50.0),
    ("ground.r1.road_dp_m", 0.0, 50.0),
    ("ground.r2.road_dp_m", 0.0, 50.0),
    ("ground.r3.road_dp_m", 0.0, 50.0),
    ("ground.r0.lift_step_m", 0.0, 0.5),
    ("ground.r1.lift_step_m", 0.0, 0.5),
    ("ground.r2.lift_step_m", 0.0, 0.5),
    ("ground.r3.lift_step_m", 0.0, 0.5),
    ("ground.roads.*.width_m", 0.5, 100.0),
    ("ground.roads.other_width_m", 0.5, 100.0),
    ("ground.roads.link_width_factor", 0.1, 1.0),
    ("ground.roads.link_min_width_m", 0.0, 100.0),
    ("ground.areas.*.level", 0.0, 7.0),
    ("ground.other_area_level", 0.0, 7.0),
    ("lights.cluster_on_agl_m", 0.0, 100_000.0),
    ("lights.cluster_off_agl_m", 0.0, 100_000.0),
    ("lights.cluster_cell_m", 1.0, 10_000.0),
    ("lights.min_ring", 0.0, 3.0),
    ("lights.street.min_ring", 0.0, 4.0),
    ("lights.street.color[]", 0.0, 1.0),
    ("lights.street.alt_color[]", 0.0, 1.0),
    ("lights.street.alt_share", 0.0, 1.0),
    ("lights.street.spacing_m", 5.0, 1000.0),
    ("lights.street.spacing_scale[]", 1.0, 16.0),
    ("lights.street.height_m", 0.0, 100.0),
    ("lights.street.untagged_max_class", 0.0, 6.0),
    ("lights.beacons.min_ring", 0.0, 4.0),
    ("lights.beacons.min_height_m", 1.0, 1000.0),
    ("lights.beacons.level_spacing_m", 0.0, 500.0),
    ("lights.beacons.corner_min_size_m", 0.0, 500.0),
    ("lights.beacons.color[]", 0.0, 1.0),
    ("lights.beacons.alt_color[]", 0.0, 1.0),
    ("lights.beacons.alt_share", 0.0, 1.0),
    ("lights.profiles.*.color[]", 0.0, 1.0),
    ("lights.profiles.*.windows.width_m", 0.5, 50.0),
    ("lights.profiles.*.windows.lit_share[]", 0.0, 1.0),
    ("lights.profiles.*.floodlight.strength", 0.0, 100.0),
    ("lights.profiles.*.floodlight.fade_height_m", 0.5, 1000.0),
    ("streaming.max_fetch_per_band", 1.0, 64.0),
    ("streaming.max_mesh_per_band", 1.0, 64.0),
    ("streaming.redeliveries_per_frame", 1.0, 64.0),
    ("streaming.worker_threads", 0.0, 64.0),
    ("streaming.retry_base_s", 0.05, 60.0),
    ("streaming.retry_max_s", 0.05, 3600.0),
    ("fades.cap_s", 0.1, 60.0),
    ("fades.max_active", 0.0, 256.0),
    ("origin.recenter_distance_m", 500.0, 50_000.0),
    ("cache.cap_mb", 1.0, 1_000_000.0),
    ("cache.connect_timeout_s", 0.1, 600.0),
    ("cache.request_timeout_s", 0.1, 3600.0),
    ("cache.open_retry_s", 0.0, 3600.0),
    ("cache.validate_retry_s", 1.0, 86_400.0),
    ("memory.budget_mb", 32.0, 65_536.0),
    ("memory.data_idle_s", 0.0, 3600.0),
    ("memory.recent_tiles_mb", 0.0, 65_536.0),
    ("memory.recent_tiles_s", 0.0, 600.0),
    ("terrain.seed", 0.0, 4_294_967_295.0),
    ("terrain.amplitude_m", 0.0, 3000.0),
    ("terrain.feature_size_m", 200.0, 200_000.0),
    ("terrain.octaves", 1.0, 10.0),
    ("terrain.ridge_share", 0.0, 1.0),
    ("terrain.valley_share", 0.0, 1.0),
    ("terrain.plateau_share", 0.0, 1.0),
    ("terrain.sea_level_m", -1000.0, 3000.0),
    ("terrain.flat_radius_m", 0.0, 100_000.0),
    ("terrain.flat_blend_m", 0.0, 100_000.0),
    ("terrain.exaggeration", 0.1, 5.0),
    ("terrain.height_offset_m", -500.0, 500.0),
    ("terrain.water_snap_m", 0.0, 200.0),
    ("terrain.drape_step_m", 5.0, 200.0),
    ("terrain.detail_scale_max", 1.0, 8.0),
    ("terrain.tau_px", 0.5, 64.0),
    ("terrain.max_tiles", 0.0, 1_000_000.0),
    ("terrain.skirt_m", 0.0, 200.0),
    ("terrain.horizon_margin_m", 0.0, 50_000.0),
    ("terrain.max_fetch", 1.0, 16.0),
    ("terrain.max_mesh", 1.0, 16.0),
    ("terrain.near_zoom", 8.0, 16.0),
    ("terrain.far_zoom", 8.0, 16.0),
    ("terrain.bands[].edge_m", 100.0, 500_000.0),
    ("terrain.bands[].max_error_m", 0.05, 200.0),
    ("terrain.bands[].max_edge_m", 0.0, 50_000.0),
    ("terrain.bands[].max_triangles", 2.0, 65_000.0),
    ("terrain.bands[].patch_cells", 0.0, 512.0),
    ("seed.aeroway_class", 6.0, 7.0),
    ("seed.water.blend_m", 50.0, 5000.0),
    ("seed.water.raster_n", 8.0, 256.0),
    ("seed.airport.shoulder_m", 0.0, 2000.0),
    ("seed.airport.strip_end_m", 0.0, 500.0),
    ("seed.airport.strip_half_m.*", 10.0, 500.0),
    ("seed.airport.fence_margin_m", 0.0, 2000.0),
    ("seed.airport.taxi_offset_m.*", 0.0, 1000.0),
    ("seed.airport.taxiway_width_m.*", 3.0, 100.0),
    ("seed.airport.runway_width_m.*", 5.0, 150.0),
    ("seed.airport.default_length_m", 100.0, 6000.0),
    ("seed.airport.max_slope", 0.0, 0.1),
    ("seed.airport.lights.height_m", 0.0, 10.0),
    ("seed.airport.lights.edge_spacing_m", 10.0, 500.0),
    ("seed.airport.lights.approach_length_m", 0.0, 3000.0),
    ("seed.airport.lights.approach_spacing_m", 5.0, 500.0),
    ("seed.airport.lights.approach_min_runway_m", 0.0, 6000.0),
    ("seed.airport.lights.taxiway_spacing_m", 10.0, 500.0),
    ("seed.airport.lights.intensity", 0.0, 1.0),
    ("seed.airport.lights.edge_color[]", 0.0, 1.0),
    ("seed.airport.lights.threshold_color[]", 0.0, 1.0),
    ("seed.airport.lights.end_color[]", 0.0, 1.0),
    ("seed.airport.lights.approach_color[]", 0.0, 1.0),
    ("seed.airport.lights.taxiway_color[]", 0.0, 1.0),
    ("seed.countryside.forest_share", 0.0, 1.0),
    ("seed.countryside.water_forest_share", 0.0, 1.0),
    ("seed.countryside.water_distance_m", 0.0, 5000.0),
    ("seed.countryside.meadow_share", 0.0, 1.0),
    ("seed.countryside.noise_cells", 1.0, 64.0),
    ("seed.countryside.tint_levels", 1.0, 8.0),
    ("seed.countryside.edge_warp", 0.0, 0.4),
    ("seed.airport.approach.*.divergence", 0.0, 1.0),
    ("seed.airport.approach.*.length_m", 0.0, 15000.0),
    ("seed.airport.approach.*.slope", 0.001, 1.0),
    ("seed.airport.approach.long_min_runway_m", 0.0, 6000.0),
    ("seed.airport.approach.min_building_m", 0.0, 50.0),
    ("seed.city.arterial_drop", 0.0, 0.9),
    ("seed.city.edge_warp", 0.0, 0.3),
    ("seed.city.max_district_m2", 50000.0, 20000000.0),
    ("seed.city.bridge_spacing_km", 0.1, 50.0),
    ("seed.city.bridge_max_m", 0.0, 5000.0),
    ("seed.city.approach_penalty", 0.0, 100.0),
    ("seed.city.approach_tall_m", 0.0, 1000.0),
    ("seed.city.radial_step_m", 20.0, 500.0),
    ("seed.city.radial_turn_deg", 0.0, 45.0),
    ("seed.city.access_max_km", 0.0, 200.0),
    ("seed.city.clearance_m", 0.0, 50.0),
    ("seed.city.floor_m", 2.0, 6.0),
    ("seed.city.landmark_height_m[]", 5.0, 300.0),
    ("seed.city.plan_cache", 16.0, 65536.0),
    ("seed.styles.*.spacing_m[]", 20.0, 2000.0),
    ("seed.styles.*.jitter", 0.0, 0.45),
    ("seed.styles.*.warp_m", 0.0, 200.0),
    ("seed.styles.*.street_lit", 0.0, 1.0),
    ("seed.styles.*.street_drop", 0.0, 0.9),
    ("seed.styles.*.frontage_m[]", 4.0, 500.0),
    ("seed.styles.*.depth_m[]", 3.0, 500.0),
    ("seed.styles.*.setback_m", 0.0, 100.0),
    ("seed.styles.*.side_gap_m", 0.0, 100.0),
    ("seed.styles.*.floors[]", 1.0, 150.0),
    ("seed.styles.*.height_exp", 0.1, 10.0),
    ("seed.styles.*.keep", 0.0, 1.0),
    ("seed.styles.*.l_share", 0.0, 1.0),
    ("seed.styles.*.max_height_m", 3.0, 1000.0),
    ("seed.styles.*.density", 0.0, 1.0),
];

/// Objects whose keys are free names (new entries allowed; entries are validated against an
/// existing entry of the defaults).
pub(super) const FREE_KEY_MAPS: &[&str] = &["ground.areas", "buildings.overrides"];

/// The shape template for [`check_shape`]: the defaults plus one example entry in every free-key
/// map that is empty by default (so a new entry has a type to check against).
pub(super) fn shape_template(defaults: &Value) -> Value {
    let mut t = defaults.clone();
    if let Some(Value::Object(o)) = t.pointer_mut("/buildings/overrides") {
        if o.is_empty() {
            o.insert("w0".into(), Value::from("residential"));
        }
    }
    t
}

pub(super) fn pattern_matches(pattern: &str, path: &str) -> bool {
    let (mut p, mut q) = (pattern.split('.'), path.split('.'));
    loop {
        match (p.next(), q.next()) {
            (None, None) => return true,
            (Some(a), Some(b)) if a == "*" || a == b => {}
            _ => return false,
        }
    }
}

/// The allowed range of a numeric key path (`None` = no range check).
pub fn range_of(path: &str) -> Option<(f64, f64)> {
    RANGES.iter().find(|(p, _, _)| pattern_matches(p, path)).map(|&(_, lo, hi)| (lo, hi))
}

pub(super) fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Range-check every numeric leaf of a profile in JSON form.
pub(super) fn check_ranges(v: &Value, path: &str, errs: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                let p = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
                check_ranges(x, &p, errs);
            }
        }
        Value::Array(a) => {
            for x in a {
                check_ranges(x, &format!("{path}[]"), errs);
            }
        }
        Value::Number(n) => {
            let x = n.as_f64().unwrap_or(f64::NAN);
            if let Some((lo, hi)) = range_of(path) {
                if !(lo..=hi).contains(&x) {
                    let key = path.trim_end_matches("[]");
                    errs.push(format!("{key} = {} is out of range [{}, {}]", fmt_num(x), fmt_num(lo), fmt_num(hi)));
                }
            }
        }
        _ => {}
    }
}
