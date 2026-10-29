//! Compiled-in defaults of the profile data tree (all sections but lights, terrain bands and
//! seed).

use std::collections::BTreeMap;

use super::*;

// ---------------------------------------------------------------------------------------------
// Defaults: exactly the constants of the spike (0.5.0). Pinned by `golden_tests` and by
// `profile::defaults_tests::defaults_equal_the_spike_constants`.

pub(super) fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

impl Default for WorldProfile {
    fn default() -> Self {
        Self {
            profile_version: PROFILE_VERSION,
            bands: Bands::default(),
            lod: Lod::default(),
            buildings: Buildings::default(),
            ground: Ground::default(),
            lights: Lights::default(),
            streaming: Streaming::default(),
            fades: Fades::default(),
            origin: Origin::default(),
            cache: Cache::default(),
            memory: Memory::default(),
            terrain: TerrainProfile::default(),
            seed: SeedProfile::default(),
        }
    }
}

impl Default for Bands {
    fn default() -> Self {
        Self {
            max_distance_m: 25_000.0,
            hysteresis: 1.15,
            look_ahead_s: 20.0,
            look_ahead_clamp: 0.5,
            default_tile_height_m: 60.0,
        }
    }
}

impl Default for Lod {
    /// tau 4 px; errors = the old edges 600 / 2500 / 7000 m x `2 tan 30 deg x 4 / 1080` in full
    /// precision, picked to the ulp so `rings::sse_refine_distance` gives exactly 600.0 / 2500.0 /
    /// 7000.0 at the default view (1080 px, 60 deg): the default cover is the old one bit for bit
    /// (2.566 / 10.69 / 29.94 rounded would move the R2 edge by 0.78 m and change the golden cover).
    fn default() -> Self {
        Self { tau_px: 4.0, max_tiles: 1000, error_m: LodErrors::default(), rear_tau_scale: 2.0, adaptive: LodAdaptive::default() }
    }
}

impl Default for LodErrors {
    fn default() -> Self {
        Self { r1: 2.5660011963983367, r2: 10.691671651659735, r3: 29.93668062464726 }
    }
}

impl Default for LodAdaptive {
    fn default() -> Self {
        Self { enabled: true, frame_budget_ms: 33.3, min_scale: 0.5, max_scale: 3.0, smoothing_s: 2.0 }
    }
}

impl Default for Buildings {
    fn default() -> Self {
        Self {
            r0: BuildingLod { dp_m: 0.5, min_area_m2: 0.0, min_height_m: 0.0 },
            r1: BuildingLod { dp_m: 2.0, min_area_m2: 20.0, min_height_m: 0.0 },
            // R2 (z14, 2.5-7 km): merged blocks seen from far away; small sheds vanish.
            r2: BuildingLod { dp_m: 4.0, min_area_m2: 50.0, min_height_m: 0.0 },
            overrides: BTreeMap::new(),
            dark_states: strings(&["historic=ruins", "ruins", "disused", "abandoned"]),
            slender_tower: SlenderTower { ratio: 3.0, max_area_m2: 150.0 },
            min_window_wall_m: 2.0,
        }
    }
}

impl Default for Ground {
    fn default() -> Self {
        let group = |class, level, kinds: &[&str]| AreaGroup { class, level, kinds: strings(kinds) };
        let mut areas = BTreeMap::new();
        // A protection boundary, often spanning forest, water and houses: bottom level.
        areas.insert("nature_reserve".into(), group(AreaClass::Grass, 0, &["nature_reserve"]));
        areas.insert(
            "grass".into(),
            group(
                AreaClass::Grass,
                5,
                &[
                    "park",
                    "garden",
                    "playground",
                    "pitch",
                    "golf_course",
                    "recreation_ground",
                    "grass",
                    "meadow",
                    "grassland",
                    "scrub",
                    "heath",
                    "wetland",
                    "village_green",
                    "cemetery",
                    "allotments",
                    "orchard",
                    "vineyard",
                    "farmland",
                    "plant_nursery",
                    "flowerbed",
                ],
            ),
        );
        areas.insert("forest".into(), group(AreaClass::Forest, 6, &["forest", "wood"]));
        areas.insert(
            "water".into(),
            group(AreaClass::Water, 7, &["water", "riverbank", "reservoir", "basin", "salt_pond"]),
        );
        areas.insert("residential".into(), group(AreaClass::Residential, 2, &["residential"]));
        areas.insert("commercial".into(), group(AreaClass::Commercial, 4, &["commercial", "retail"]));
        areas.insert(
            "industrial".into(),
            group(
                AreaClass::Industrial,
                3,
                &["industrial", "railway", "port", "depot", "garages", "quarry", "landfill"],
            ),
        );
        Self {
            r0: GroundLod {
                area_dp_m: 0.5,
                min_area_m2: 0.0,
                road_dp_m: 0.5,
                max_road_class: RoadClass::Other,
                minor_paths: true,
                lift_step_m: 0.02,
            },
            r1: GroundLod {
                area_dp_m: 2.0,
                min_area_m2: 200.0,
                road_dp_m: 2.0,
                max_road_class: RoadClass::Other,
                minor_paths: false,
                lift_step_m: 0.06,
            },
            r2: GroundLod {
                area_dp_m: 6.0,
                min_area_m2: 2500.0,
                road_dp_m: 5.0,
                max_road_class: RoadClass::Residential,
                minor_paths: false,
                lift_step_m: 0.15,
            },
            r3: GroundLod {
                area_dp_m: 20.0,
                min_area_m2: 40000.0,
                road_dp_m: 15.0,
                max_road_class: RoadClass::Secondary,
                minor_paths: false,
                lift_step_m: 0.5,
            },
            roads: Roads::default(),
            areas,
            other_area_level: 1,
        }
    }
}

impl Default for Roads {
    fn default() -> Self {
        let g = |width_m, h: &[&str]| RoadGroup { width_m, highways: strings(h) };
        Self {
            motorway: g(14.0, &["motorway"]),
            primary: g(10.0, &["trunk", "primary"]),
            secondary: g(8.0, &["secondary"]),
            tertiary: g(6.0, &["tertiary"]),
            residential: g(6.0, &["residential", "unclassified", "living_street"]),
            other_width_m: 3.0,
            link_width_factor: 0.6,
            link_min_width_m: 3.0,
            minor_paths: strings(&[
                "footway",
                "path",
                "cycleway",
                "steps",
                "bridleway",
                "track",
                "corridor",
                "elevator",
                "via_ferrata",
                "escalator",
            ]),
            lit_values: strings(&[
                "yes",
                "24/7",
                "automatic",
                "sunset-sunrise",
                "dusk-dawn",
                "limited",
                "interval",
                "night",
            ]),
        }
    }
}

impl Default for Streaming {
    fn default() -> Self {
        Self {
            max_fetch_per_band: 4,
            max_mesh_per_band: 8,
            redeliveries_per_frame: 1,
            worker_threads: 0,
            retry_base_s: 0.5,
            retry_max_s: 30.0,
        }
    }
}

impl Default for Fades {
    fn default() -> Self {
        Self { cap_s: 0.75, max_active: 4 }
    }
}

impl Default for Origin {
    fn default() -> Self {
        Self { recenter_distance_m: 5000.0 }
    }
}

impl Default for Cache {
    fn default() -> Self {
        Self { cap_mb: 300.0, connect_timeout_s: 5.0, request_timeout_s: 20.0, open_retry_s: 1.5, validate_retry_s: 30.0 }
    }
}

impl Default for Memory {
    fn default() -> Self {
        Self { budget_mb: 512.0, data_idle_s: 20.0, recent_tiles_mb: 32.0, recent_tiles_s: 15.0 }
    }
}

impl Default for TerrainProfile {
    fn default() -> Self {
        let band = |edge_m, max_error_m, max_edge_m, max_triangles, patch_cells| TerrainBand { edge_m, max_error_m, max_edge_m, max_triangles, patch_cells };
        Self {
            enabled: false,
            seed: 1,
            amplitude_m: 450.0,
            feature_size_m: 9000.0,
            octaves: 6,
            ridge_share: 0.5,
            valley_share: 0.6,
            plateau_share: 0.25,
            sea_level_m: 0.0,
            flat_radius_m: 1500.0,
            flat_blend_m: 2000.0,
            exaggeration: 1.0,
            height_offset_m: 0.0,
            water_snap_m: 0.0,
            drape_step_m: 25.0,
            detail_scale_max: 2.0,
            tau_px: TERRAIN_TAU_PX_DEFAULT,
            max_tiles: 1500,
            skirt_m: 0.0,
            shading: TerrainShading::Flat,
            horizon_cull: true,
            horizon_margin_m: 2000.0,
            max_fetch: 4,
            max_mesh: 2,
            near_zoom: 14,
            far_zoom: 10,
            // docs/spec-terrain.md 5.2: T0 z14 .. T4 z10.
            bands: vec![
                band(2500.0, 0.5, 0.0, 4096, 0),
                band(7000.0, 1.5, 0.0, 4096, 0),
                band(25_000.0, 4.0, 2000.0, 8192, 128),
                band(60_000.0, 10.0, 3000.0, 8192, 128),
                band(130_000.0, 25.0, 5000.0, 8192, 128),
            ],
        }
    }
}
