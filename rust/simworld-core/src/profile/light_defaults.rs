//! Compiled-in defaults of the `lights` section: the light profiles, their hourly lit shares
//! and the landuse fallback.

use super::defaults::strings;
use super::*;

impl Default for Lights {
    fn default() -> Self {
        Self {
            cluster_on_agl_m: 3300.0,
            cluster_off_agl_m: 2700.0,
            cluster_cell_m: 100.0,
            min_ring: 1,
            street: StreetLights::default(),
            beacons: Beacons::default(),
            profiles: LightProfiles::default(),
            landuse_fallback: LanduseFallback::default(),
        }
    }
}

/// 24 hourly values from (first hour, last hour, value) levels; the hours in between ramp
/// linearly (one-hour ramps between the start-value levels of docs/spec-night-windows.md).
fn hourly(levels: &[(usize, usize, f64)]) -> Vec<f64> {
    let mut v = vec![f64::NAN; 24];
    for &(a, b, x) in levels {
        v[a..=b].fill(x);
    }
    for h in 0..24 {
        if v[h].is_nan() {
            let prev = (1..24).map(|d| v[(h + 24 - d) % 24]).find(|x| !x.is_nan()).unwrap_or(0.0);
            let next = (1..24).map(|d| v[(h + d) % 24]).find(|x| !x.is_nan()).unwrap_or(0.0);
            v[h] = ((prev + next) * 0.5 * 1000.0).round() / 1000.0;
        }
    }
    v
}

impl Default for LightProfiles {
    fn default() -> Self {
        let p = |color, kinds: &[&str]| LightProfile { color, kinds: strings(kinds), windows: None, floodlight: None };
        let w = |width_m, lit_share: Vec<f64>| Some(WindowSpec { width_m, lit_share });
        Self {
            // Warm 2700 K.
            residential: LightProfile {
                windows: w(
                    3.0,
                    vec![
                        0.08, 0.06, 0.05, 0.05, 0.05, 0.08, 0.20, 0.30, 0.20, 0.10, 0.08, 0.08, 0.08, 0.08, 0.08, 0.10, 0.15,
                        0.30, 0.45, 0.45, 0.45, 0.40, 0.25, 0.12,
                    ],
                ),
                ..p(
                    [1.0, 0.655, 0.341],
                    &[
                        "residential",
                        "apartments",
                        "house",
                        "detached",
                        "semidetached_house",
                        "terrace",
                        "dormitory",
                        "bungalow",
                        "cabin",
                        "farm",
                        "hut",
                        "static_caravan",
                        "houseboat",
                        "flats",
                        "hotel",
                    ],
                )
            },
            // Cool 4500 K.
            office: LightProfile {
                windows: w(2.0, hourly(&[(0, 6, 0.05), (8, 19, 0.70), (20, 22, 0.15)])),
                ..p([0.86, 0.9, 1.0], &["office", "commercial", "bank"])
            },
            // Bright white.
            retail: LightProfile {
                windows: w(4.0, hourly(&[(0, 7, 0.10), (9, 20, 0.90), (22, 23, 0.10)])),
                ..p(
                    [1.0, 0.98, 0.95],
                    &["retail", "supermarket", "kiosk", "shop", "mall", "store", "department_store", "restaurant"],
                )
            },
            // Sodium orange. No windows (wall lamps later).
            industrial: p(
                [1.0, 0.55, 0.15],
                &[
                    "industrial",
                    "warehouse",
                    "factory",
                    "manufacture",
                    "hangar",
                    "depot",
                    "farm_auxiliary",
                    "boathouse",
                    "parking",
                ],
            ),
            // Warm ~3000 K.
            civic: LightProfile {
                windows: w(3.5, hourly(&[(0, 6, 0.03), (8, 17, 0.60), (18, 22, 0.10)])),
                ..p(
                    [1.0, 0.706, 0.42],
                    &[
                        "religious",
                        "government",
                        "university",
                        "college",
                        "school",
                        "kindergarten",
                        "hospital",
                        "public",
                        "civic",
                        "townhall",
                        "museum",
                        "train_station",
                        "transportation",
                        "stadium",
                        "sports_hall",
                        "fire_station",
                        "library",
                        "theatre",
                        "courthouse",
                        "embassy",
                    ],
                )
            },
            // Floodlit facade, warm ~3000 K; no windows.
            landmark: LightProfile {
                floodlight: Some(Floodlight { strength: 1.2, fade_height_m: 25.0 }),
                ..p(
                    [1.0, 0.75, 0.45],
                    &[
                        "castle",
                        "fort",
                        "tower",
                        "bell_tower",
                        "church",
                        "cathedral",
                        "chapel",
                        "mosque",
                        "synagogue",
                        "temple",
                        "monastery",
                        "shrine",
                        "gatehouse",
                        "city_gate",
                        "lighthouse",
                        "monument",
                        "memorial",
                    ],
                )
            },
            // No windows, no light point.
            dark: p(
                [0.0, 0.0, 0.0],
                &[
                    "ruins",
                    "roof",
                    "carport",
                    "garage",
                    "garages",
                    "shed",
                    "barn",
                    "bunker",
                    "silo",
                    "storage_tank",
                    "slurry_tank",
                    "digester",
                    "chimney",
                    "water_tower",
                    "container",
                    "greenhouse",
                    "ship",
                    "service",
                    "transformer_tower",
                    "bridge",
                ],
            ),
        }
    }
}

impl Default for LanduseFallback {
    fn default() -> Self {
        use LightProfileName as P;
        // Unknown kinds with no (or an unmapped) landuse -> residential (design doc table).
        Self {
            residential: P::Residential,
            commercial: P::Office,
            retail: P::Retail,
            industrial: P::Industrial,
            civic: P::Civic,
            other: P::Residential,
            none: P::Residential,
        }
    }
}
