//! The `seed` section of the profile data tree (profile_version 6) with its defaults.

use serde::{Deserialize, Serialize};

/// World seed (profile_version 6, `docs/spec-world-seed.md` section 9.2). Only the keys phase S1
/// uses; later phases add `city`, `styles`, `roads`, `landmarks`, `plan_cache`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SeedProfile {
    /// Ground class written for airport pavements and fences: 7 (`aeroway`, api 1.7) or 6
    /// (`other`, for a host without the class 7 shader branch). The sub-kind goes into COLOR `b`
    /// either way.
    pub aeroway_class: u32,
    pub water: SeedWater,
    pub airport: SeedAirport,
    pub countryside: SeedCountryside,
}

/// The terrain-water rule (section 5.2a).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SeedWater {
    /// Land within this distance of the sea or a lake slopes down to the water level (m).
    pub blend_m: f64,
    /// Distance raster samples per side of a z11 cell (64 desktop; 32 halves the memory, iOS).
    pub raster_n: u32,
}

/// A value per airport type (OurAirports `large_airport`, `medium_airport`, `small_airport`;
/// any other type uses `small`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ByAirportType {
    pub large: f64,
    pub medium: f64,
    pub small: f64,
}

impl Default for ByAirportType {
    fn default() -> Self {
        Self { large: 0.0, medium: 0.0, small: 0.0 }
    }
}

impl ByAirportType {
    pub fn get(&self, airport_type: &str) -> f64 {
        match airport_type {
            "large_airport" => self.large,
            "medium_airport" => self.medium,
            _ => self.small,
        }
    }
}

/// Generated airports (section 5.1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SeedAirport {
    /// The runway strips' flat ground blends into the relief over this distance outside the
    /// fence (m).
    pub shoulder_m: f64,
    /// The strip extends this far past each physical runway end (m).
    pub strip_end_m: f64,
    /// Half-width of the runway strip (no buildings or streets; flat in the terrain), m.
    pub strip_half_m: ByAirportType,
    /// The fence is the rectangle around all strips, taxiways, apron and buildings plus this (m).
    pub fence_margin_m: f64,
    /// Parallel taxiway: centreline distance from the main runway's centreline (m; 0 = none).
    pub taxi_offset_m: ByAirportType,
    /// Taxiway width (m).
    pub taxiway_width_m: ByAirportType,
    /// Runway width when the airport file has none (m).
    pub runway_width_m: ByAirportType,
    /// Length of a runway whose file entry has neither ends nor `length_ft` (m).
    pub default_length_m: f64,
    /// Steepest runway in the terrain (rise / run): threshold heights further apart are moved
    /// toward their mean (the procedural relief knows nothing about airports; ICAO allows 1 %).
    pub max_slope: f64,
    pub lights: SeedAirportLights,
}

/// Airport lights (ready-tile keys `airport_lights` / `airport_light_colors`, api 1.7), for
/// runways with `lighted` in the airport file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SeedAirportLights {
    /// Height of a light above the drawn pavement (m).
    pub height_m: f64,
    /// Runway edge lights every this many meters on both edges (white).
    pub edge_spacing_m: f64,
    /// Approach light row length before each threshold (m; 0 = none).
    pub approach_length_m: f64,
    /// Approach light spacing along the row (m).
    pub approach_spacing_m: f64,
    /// Only paved runways at least this long get approach lights (m).
    pub approach_min_runway_m: f64,
    /// Taxiway edge lights every this many meters (medium and large airports; blue).
    pub taxiway_spacing_m: f64,
    /// COLOR `a` of every airport light (intensity weight, like the other light keys).
    pub intensity: f64,
    pub edge_color: [f64; 3],
    pub threshold_color: [f64; 3],
    pub end_color: [f64; 3],
    pub approach_color: [f64; 3],
    pub taxiway_color: [f64; 3],
}

/// Countryside ground (section 5.2): forest / farmland / meadow patches on the z15 grid.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SeedCountryside {
    /// Share of the land that is forest.
    pub forest_share: f64,
    /// Forest share for patches within `water_distance_m` of water.
    pub water_forest_share: f64,
    /// See `water_forest_share` (m).
    pub water_distance_m: f64,
    /// Share of the non-forest patches that are meadow (the rest is farmland).
    pub meadow_share: f64,
    /// Size of the low-frequency noise that groups forests, in z15 cells (about 700 m each).
    pub noise_cells: f64,
    /// Number of tint values (ground COLOR `g` = tint / 8 on countryside areas, 0..levels-1).
    pub tint_levels: u32,
    /// Patch corners and edge points move up to this share of a z15 cell (fields are not squares).
    pub edge_warp: f64,
}

impl Default for SeedProfile {
    fn default() -> Self {
        Self {
            aeroway_class: 7,
            water: SeedWater { blend_m: 400.0, raster_n: 64 },
            airport: SeedAirport::default(),
            countryside: SeedCountryside {
                forest_share: 0.32,
                water_forest_share: 0.12,
                water_distance_m: 500.0,
                meadow_share: 0.4,
                noise_cells: 3.0,
                tint_levels: 8,
                edge_warp: 0.22,
            },
        }
    }
}

impl Default for SeedWater {
    fn default() -> Self {
        SeedProfile::default().water
    }
}

impl Default for SeedCountryside {
    fn default() -> Self {
        SeedProfile::default().countryside
    }
}

impl Default for SeedAirport {
    fn default() -> Self {
        let t = |large, medium, small| ByAirportType { large, medium, small };
        Self {
            shoulder_m: 150.0,
            strip_end_m: 60.0,
            strip_half_m: t(150.0, 150.0, 40.0),
            fence_margin_m: 100.0,
            taxi_offset_m: t(180.0, 120.0, 0.0),
            taxiway_width_m: t(23.0, 18.0, 10.0),
            runway_width_m: t(45.0, 30.0, 18.0),
            default_length_m: 800.0,
            max_slope: 0.01,
            lights: SeedAirportLights::default(),
        }
    }
}

impl Default for SeedAirportLights {
    fn default() -> Self {
        Self {
            height_m: 0.5,
            edge_spacing_m: 60.0,
            approach_length_m: 900.0,
            approach_spacing_m: 30.0,
            approach_min_runway_m: 1200.0,
            taxiway_spacing_m: 60.0,
            intensity: 0.35,
            edge_color: [1.0, 0.92, 0.78],
            threshold_color: [0.25, 1.0, 0.45],
            end_color: [1.0, 0.12, 0.08],
            approach_color: [1.0, 0.95, 0.85],
            taxiway_color: [0.2, 0.35, 1.0],
        }
    }
}
