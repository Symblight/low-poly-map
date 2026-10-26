//! World profile: every tuning value of the world (spec-library-packaging.md section 3, audit A6)
//! as data, with compiled-in defaults equal to the values the spike shipped with.
//!
//! * [`WorldProfile`] is the serde data tree; its JSON form is
//!   `addons/simworld/profiles/default.json`, documented by `world-profile.schema.json`.
//! * [`Profile`] is a validated `WorldProfile` plus lookup tables, shared as `Arc<Profile>` by
//!   the scheduler, the workers (decode, mesher, ground, lights) and the fetcher.
//! * [`resolve`] builds a `Profile` from an optional JSON file and optional overrides (both
//!   partial: omitted keys keep their defaults; objects merge deeply, everything else replaces).
//!
//! Unknown keys: a file whose `profile_version` is the library's ([`PROFILE_VERSION`]) or older
//! is strict (an unknown key is a typo and an error); a newer file loads with a warning per
//! unknown key, which is ignored. Out-of-range values fail with the key, the value and the
//! allowed range.
//!
//! Structural values (ring count, zooms 13..16, layer names) are not tuning and are not here
//! (audit A18): they are part of the tileset contract and are checked against the archive
//! header when it is opened (`fetch`).

use std::collections::BTreeMap;
use std::ops::Deref;

use ahash::{AHashMap, AHashSet};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::diag::Log;
use crate::ground::GroundParams;
use crate::mesher::{LodParams, PROFILE_CIVIC, PROFILE_INDUSTRIAL, PROFILE_OFFICE, PROFILE_RESIDENTIAL, PROFILE_RETAIL};
use crate::mvt::{
    Landuse, GROUND_COMMERCIAL, GROUND_FOREST, GROUND_GRASS, GROUND_INDUSTRIAL, GROUND_OTHER, GROUND_RESIDENTIAL,
    GROUND_WATER, ROAD_MOTORWAY, ROAD_OTHER, ROAD_PRIMARY, ROAD_RESIDENTIAL, ROAD_SECONDARY, ROAD_TERTIARY,
};
use crate::rings::{BandParams, RING_COUNT};

/// Profile schema version this library reads. Bump when keys are added (and update
/// `default.json`, the JSON Schema and `CHANGELOG.md`).
pub const PROFILE_VERSION: u32 = 1;

// ---------------------------------------------------------------------------------------------
// Data tree

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WorldProfile {
    /// Schema version of the file (see module docs). The resolved profile always carries
    /// [`PROFILE_VERSION`].
    pub profile_version: u32,
    pub bands: Bands,
    pub buildings: Buildings,
    pub ground: Ground,
    pub lights: Lights,
    pub streaming: Streaming,
    pub fades: Fades,
    pub origin: Origin,
    pub cache: Cache,
}

/// LOD bands: slant-distance edges per ring (R0 = z16 .. R3 = z13), hysteresis, look-ahead.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Bands {
    pub r0_edge_m: f64,
    pub r1_edge_m: f64,
    pub r2_edge_m: f64,
    /// Outer edge of the whole cover: nothing is requested beyond it.
    pub r3_edge_m: f64,
    /// A tile changes band only once its slant distance is this factor past the edge.
    pub hysteresis: f64,
    /// Look-ahead horizon (s): camera + velocity x this (3D).
    pub look_ahead_s: f64,
    /// Look-ahead offset clamp as a fraction of the band's edge.
    pub look_ahead_clamp: f64,
    /// Box height (m) of a tile whose own and ancestors' heights are still unknown.
    pub default_tile_height_m: f64,
}

/// Building meshing per band (R3 has no buildings).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Buildings {
    pub r0: BuildingLod,
    pub r1: BuildingLod,
    pub r2: BuildingLod,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildingLod {
    /// Douglas-Peucker tolerance (m).
    pub dp_m: f64,
    /// Drop buildings with a smaller footprint (m^2).
    pub min_area_m2: f64,
}

/// Ground meshing: per-band parameters, road classes and widths, area classes and levels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Ground {
    pub r0: GroundLod,
    pub r1: GroundLod,
    pub r2: GroundLod,
    pub roads: Roads,
    /// Area groups by name (free keys): `landuse` kinds -> ground class and stacking level.
    pub areas: BTreeMap<String, AreaGroup>,
    /// Stacking level of landuse kinds not listed in any group (class `other`).
    pub other_area_level: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroundLod {
    pub area_dp_m: f64,
    pub min_area_m2: f64,
    pub road_dp_m: f64,
    /// Keep roads of this class and every more important one.
    pub max_road_class: RoadClass,
    /// Keep minor paths (`ground.roads.minor_paths`).
    pub minor_paths: bool,
    /// Vertical lift per stacking level (m).
    pub lift_step_m: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Roads {
    pub motorway: RoadGroup,
    pub primary: RoadGroup,
    pub secondary: RoadGroup,
    pub tertiary: RoadGroup,
    pub residential: RoadGroup,
    /// Full strip width (m) of every other `highway` value (class 6).
    pub other_width_m: f64,
    /// `_link` roads keep their class at this fraction of its width (0.1 m steps) ...
    pub link_width_factor: f64,
    /// ... and at least this width (m).
    pub link_min_width_m: f64,
    /// Class-6 `highway` values dropped from R1 on (and with class 6 at R2).
    pub minor_paths: Vec<String>,
    /// OSM `lit` values that count as lit (ground COLOR `g` = 1).
    pub lit_values: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoadGroup {
    /// Full strip width (m).
    pub width_m: f64,
    /// `highway` values of this class (`_link` variants included automatically).
    pub highways: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AreaGroup {
    pub class: AreaClass,
    /// Stacking level 0..7 (higher draws on top; roads use 8..13).
    pub level: u8,
    pub kinds: Vec<String>,
}

/// Ground area class (contract ground COLOR `r` = id / 16).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AreaClass {
    Grass,
    Forest,
    Water,
    Residential,
    Commercial,
    Industrial,
    Other,
}

impl AreaClass {
    pub fn id(self) -> u8 {
        match self {
            Self::Grass => GROUND_GRASS,
            Self::Forest => GROUND_FOREST,
            Self::Water => GROUND_WATER,
            Self::Residential => GROUND_RESIDENTIAL,
            Self::Commercial => GROUND_COMMERCIAL,
            Self::Industrial => GROUND_INDUSTRIAL,
            Self::Other => GROUND_OTHER,
        }
    }
}

/// Road class (contract ground COLOR `b` = id / 8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoadClass {
    Motorway,
    Primary,
    Secondary,
    Tertiary,
    Residential,
    Other,
}

impl RoadClass {
    pub fn id(self) -> u8 {
        match self {
            Self::Motorway => ROAD_MOTORWAY,
            Self::Primary => ROAD_PRIMARY,
            Self::Secondary => ROAD_SECONDARY,
            Self::Tertiary => ROAD_TERTIARY,
            Self::Residential => ROAD_RESIDENTIAL,
            Self::Other => ROAD_OTHER,
        }
    }
}

/// Light profiles (contract vertex COLOR `r` = id / 8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LightProfileName {
    Residential,
    Office,
    Retail,
    Industrial,
    Civic,
}

impl LightProfileName {
    pub fn id(self) -> u8 {
        match self {
            Self::Residential => PROFILE_RESIDENTIAL,
            Self::Office => PROFILE_OFFICE,
            Self::Retail => PROFILE_RETAIL,
            Self::Industrial => PROFILE_INDUSTRIAL,
            Self::Civic => PROFILE_CIVIC,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Lights {
    /// Light clustering (100 m light carpet) switches on above this AGL (m) ...
    pub cluster_on_agl_m: f64,
    /// ... and off below this one.
    pub cluster_off_agl_m: f64,
    /// Cell size (m) of the clustered light carpet.
    pub cluster_cell_m: f64,
    pub profiles: LightProfiles,
    /// Profile of buildings whose kind is in no profile, by the landuse around them.
    pub landuse_fallback: LanduseFallback,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LightProfiles {
    pub residential: LightProfile,
    pub office: LightProfile,
    pub retail: LightProfile,
    pub industrial: LightProfile,
    pub civic: LightProfile,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightProfile {
    /// Light sprite color (linear rgb 0..1).
    pub color: [f64; 3],
    /// OSM `building` values of this profile.
    pub kinds: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LanduseFallback {
    pub residential: LightProfileName,
    pub commercial: LightProfileName,
    pub retail: LightProfileName,
    pub industrial: LightProfileName,
    pub civic: LightProfileName,
    pub other: LightProfileName,
    /// No landuse polygon under the building.
    pub none: LightProfileName,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Streaming {
    /// Fetches in flight per band.
    pub max_fetch_per_band: u32,
    /// Mesh (decode) jobs in flight per band.
    pub max_mesh_per_band: u32,
    /// Light-mode re-deliveries per frame.
    pub redeliveries_per_frame: u32,
    /// Retry backoff after a failed fetch/decode: base (s), doubled per attempt ...
    pub retry_base_s: f64,
    /// ... up to this (s).
    pub retry_max_s: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Fades {
    /// Safety cap: a LOD fade is force-completed this long after it started (s).
    pub cap_s: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Origin {
    /// Re-center the floating origin beyond this horizontal distance from it (m).
    pub recenter_distance_m: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Cache {
    /// Disk cache LRU cap (MiB).
    pub cap_mb: f64,
    pub connect_timeout_s: f64,
    pub request_timeout_s: f64,
    /// Minimum wait before re-opening an archive that failed to open (s).
    pub open_retry_s: f64,
    /// While the archive is unreachable, re-validate the disk cache this often (s).
    pub validate_retry_s: f64,
}

// ---------------------------------------------------------------------------------------------
// Defaults: exactly the constants of the spike (0.5.0). Pinned by `golden_tests` and by
// `profile::tests::defaults_equal_the_spike_constants`.

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

impl Default for WorldProfile {
    fn default() -> Self {
        Self {
            profile_version: PROFILE_VERSION,
            bands: Bands::default(),
            buildings: Buildings::default(),
            ground: Ground::default(),
            lights: Lights::default(),
            streaming: Streaming::default(),
            fades: Fades::default(),
            origin: Origin::default(),
            cache: Cache::default(),
        }
    }
}

impl Default for Bands {
    fn default() -> Self {
        Self {
            r0_edge_m: 600.0,
            r1_edge_m: 2500.0,
            r2_edge_m: 7000.0,
            r3_edge_m: 25_000.0,
            hysteresis: 1.15,
            look_ahead_s: 20.0,
            look_ahead_clamp: 0.5,
            default_tile_height_m: 60.0,
        }
    }
}

impl Default for Buildings {
    fn default() -> Self {
        Self {
            r0: BuildingLod { dp_m: 0.5, min_area_m2: 0.0 },
            r1: BuildingLod { dp_m: 2.0, min_area_m2: 20.0 },
            // R2 (z14, 2.5-7 km): merged blocks seen from far away; small sheds vanish.
            r2: BuildingLod { dp_m: 4.0, min_area_m2: 50.0 },
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

impl Default for Lights {
    fn default() -> Self {
        Self {
            cluster_on_agl_m: 3300.0,
            cluster_off_agl_m: 2700.0,
            cluster_cell_m: 100.0,
            profiles: LightProfiles::default(),
            landuse_fallback: LanduseFallback::default(),
        }
    }
}

impl Default for LightProfiles {
    fn default() -> Self {
        let p = |color, kinds: &[&str]| LightProfile { color, kinds: strings(kinds) };
        Self {
            // Warm 2700 K.
            residential: p(
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
            ),
            // Cool 4500 K.
            office: p([0.86, 0.9, 1.0], &["office", "commercial", "bank"]),
            // Bright white.
            retail: p(
                [1.0, 0.98, 0.95],
                &["retail", "supermarket", "kiosk", "shop", "mall", "store", "department_store", "restaurant"],
            ),
            // Sodium orange.
            industrial: p(
                [1.0, 0.55, 0.15],
                &[
                    "industrial",
                    "warehouse",
                    "factory",
                    "manufacture",
                    "hangar",
                    "storage_tank",
                    "depot",
                    "garage",
                    "garages",
                    "shed",
                    "barn",
                    "farm_auxiliary",
                    "service",
                    "transformer_tower",
                    "boathouse",
                    "greenhouse",
                    "parking",
                ],
            ),
            // Warm floodlight ~3000 K.
            civic: p(
                [1.0, 0.706, 0.42],
                &[
                    "church",
                    "cathedral",
                    "chapel",
                    "mosque",
                    "synagogue",
                    "temple",
                    "religious",
                    "monastery",
                    "shrine",
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

impl Default for Streaming {
    fn default() -> Self {
        Self { max_fetch_per_band: 4, max_mesh_per_band: 2, redeliveries_per_frame: 1, retry_base_s: 0.5, retry_max_s: 30.0 }
    }
}

impl Default for Fades {
    fn default() -> Self {
        Self { cap_s: 3.0 }
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

// ---------------------------------------------------------------------------------------------
// Ranges (single source for validation; the JSON Schema must agree, see tests)

/// Allowed range per numeric key. `*` matches any one key (free-key maps and the r0..r2
/// objects); `[]` is every element of an array.
pub const RANGES: &[(&str, f64, f64)] = &[
    ("profile_version", 1.0, 1_000_000.0),
    ("bands.r0_edge_m", 50.0, 100_000.0),
    ("bands.r1_edge_m", 50.0, 100_000.0),
    ("bands.r2_edge_m", 50.0, 100_000.0),
    ("bands.r3_edge_m", 50.0, 100_000.0),
    ("bands.hysteresis", 1.01, 2.0),
    ("bands.look_ahead_s", 0.0, 120.0),
    ("bands.look_ahead_clamp", 0.0, 1.0),
    ("bands.default_tile_height_m", 0.0, 400.0),
    ("buildings.*.dp_m", 0.0, 50.0),
    ("buildings.*.min_area_m2", 0.0, 100_000.0),
    ("ground.r0.area_dp_m", 0.0, 50.0),
    ("ground.r1.area_dp_m", 0.0, 50.0),
    ("ground.r2.area_dp_m", 0.0, 50.0),
    ("ground.r0.min_area_m2", 0.0, 1_000_000.0),
    ("ground.r1.min_area_m2", 0.0, 1_000_000.0),
    ("ground.r2.min_area_m2", 0.0, 1_000_000.0),
    ("ground.r0.road_dp_m", 0.0, 50.0),
    ("ground.r1.road_dp_m", 0.0, 50.0),
    ("ground.r2.road_dp_m", 0.0, 50.0),
    ("ground.r0.lift_step_m", 0.0, 0.5),
    ("ground.r1.lift_step_m", 0.0, 0.5),
    ("ground.r2.lift_step_m", 0.0, 0.5),
    ("ground.roads.*.width_m", 0.5, 100.0),
    ("ground.roads.other_width_m", 0.5, 100.0),
    ("ground.roads.link_width_factor", 0.1, 1.0),
    ("ground.roads.link_min_width_m", 0.0, 100.0),
    ("ground.areas.*.level", 0.0, 7.0),
    ("ground.other_area_level", 0.0, 7.0),
    ("lights.cluster_on_agl_m", 0.0, 100_000.0),
    ("lights.cluster_off_agl_m", 0.0, 100_000.0),
    ("lights.cluster_cell_m", 1.0, 10_000.0),
    ("lights.profiles.*.color[]", 0.0, 1.0),
    ("streaming.max_fetch_per_band", 1.0, 64.0),
    ("streaming.max_mesh_per_band", 1.0, 64.0),
    ("streaming.redeliveries_per_frame", 1.0, 64.0),
    ("streaming.retry_base_s", 0.05, 60.0),
    ("streaming.retry_max_s", 0.05, 3600.0),
    ("fades.cap_s", 0.1, 60.0),
    ("origin.recenter_distance_m", 500.0, 50_000.0),
    ("cache.cap_mb", 1.0, 1_000_000.0),
    ("cache.connect_timeout_s", 0.1, 600.0),
    ("cache.request_timeout_s", 0.1, 3600.0),
    ("cache.open_retry_s", 0.0, 3600.0),
    ("cache.validate_retry_s", 1.0, 86_400.0),
];

/// Objects whose keys are free names (new entries allowed; entries are validated against an
/// existing entry of the defaults).
const FREE_KEY_MAPS: &[&str] = &["ground.areas"];

fn pattern_matches(pattern: &str, path: &str) -> bool {
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

fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// Range-check every numeric leaf of a profile in JSON form.
fn check_ranges(v: &Value, path: &str, errs: &mut Vec<String>) {
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

// ---------------------------------------------------------------------------------------------
// Compiled profile

/// A validated [`WorldProfile`] plus the lookup tables the workers use. Cheap to share as
/// `Arc<Profile>`; derefs to the data tree.
#[derive(Clone, Debug)]
pub struct Profile {
    world: WorldProfile,
    band_params: BandParams,
    lod: [Option<LodParams>; RING_COUNT],
    ground_lod: [Option<GroundParams>; RING_COUNT],
    kind_profile: AHashMap<String, u8>,
    /// Indexed by `landuse_index`.
    landuse_profile: [u8; 7],
    /// rgba (a = 1) by profile id 0..4.
    profile_colors: [[f32; 4]; 5],
    area_kinds: AHashMap<String, (u8, u8)>,
    other_area_level: u8,
    road_kinds: AHashMap<String, u8>,
    /// Full width by road class id (index 1..=6).
    road_widths: [f64; 7],
    minor_paths: AHashSet<String>,
    lit_values: AHashSet<String>,
    cluster_cell_m: f32,
}

impl PartialEq for Profile {
    fn eq(&self, other: &Self) -> bool {
        self.world == other.world
    }
}

impl Deref for Profile {
    type Target = WorldProfile;
    fn deref(&self) -> &WorldProfile {
        &self.world
    }
}

impl Default for Profile {
    fn default() -> Self {
        Profile::new(WorldProfile::default()).expect("the default world profile is valid")
    }
}

/// Shared default profile for tests (building one allocates the lookup tables).
#[cfg(test)]
pub(crate) fn test_default() -> &'static Profile {
    static P: std::sync::OnceLock<Profile> = std::sync::OnceLock::new();
    P.get_or_init(Profile::default)
}

fn landuse_index(l: Option<Landuse>) -> usize {
    match l {
        Some(Landuse::Residential) => 0,
        Some(Landuse::Commercial) => 1,
        Some(Landuse::Retail) => 2,
        Some(Landuse::Industrial) => 3,
        Some(Landuse::Civic) => 4,
        Some(Landuse::Other) => 5,
        None => 6,
    }
}

impl Profile {
    /// Validate (ranges and cross-key rules) and compile. Errors name the key, the value and
    /// the allowed range; several problems are reported together.
    pub fn new(world: WorldProfile) -> Result<Self, String> {
        let mut errs = Vec::new();
        let json = serde_json::to_value(&world).map_err(|e| format!("profile: {e}"))?;
        check_ranges(&json, "", &mut errs);
        let b = &world.bands;
        let edges = [b.r0_edge_m, b.r1_edge_m, b.r2_edge_m, b.r3_edge_m];
        for r in 1..RING_COUNT {
            if edges[r] <= edges[r - 1] {
                errs.push(format!(
                    "bands.r{r}_edge_m = {} must be greater than bands.r{}_edge_m = {}",
                    fmt_num(edges[r]),
                    r - 1,
                    fmt_num(edges[r - 1])
                ));
            }
        }
        let l = &world.lights;
        if l.cluster_off_agl_m > l.cluster_on_agl_m {
            errs.push(format!(
                "lights.cluster_off_agl_m = {} must not exceed lights.cluster_on_agl_m = {}",
                fmt_num(l.cluster_off_agl_m),
                fmt_num(l.cluster_on_agl_m)
            ));
        }
        let s = &world.streaming;
        if s.retry_max_s < s.retry_base_s {
            errs.push(format!(
                "streaming.retry_max_s = {} must not be below streaming.retry_base_s = {}",
                fmt_num(s.retry_max_s),
                fmt_num(s.retry_base_s)
            ));
        }

        // Light profiles: kind -> id (a kind in two profiles is ambiguous).
        let mut kind_profile = AHashMap::new();
        let lp = &l.profiles;
        let named = [
            ("residential", &lp.residential, PROFILE_RESIDENTIAL),
            ("office", &lp.office, PROFILE_OFFICE),
            ("retail", &lp.retail, PROFILE_RETAIL),
            ("industrial", &lp.industrial, PROFILE_INDUSTRIAL),
            ("civic", &lp.civic, PROFILE_CIVIC),
        ];
        let mut profile_colors = [[1.0f32; 4]; 5];
        for (name, p, id) in named {
            profile_colors[id as usize] = [p.color[0] as f32, p.color[1] as f32, p.color[2] as f32, 1.0];
            for k in &p.kinds {
                if let Some(prev) = kind_profile.insert(k.clone(), id) {
                    if prev != id {
                        errs.push(format!("lights.profiles.{name}.kinds: \"{k}\" is already in another profile"));
                    }
                }
            }
        }
        let f = &l.landuse_fallback;
        let landuse_profile =
            [f.residential.id(), f.commercial.id(), f.retail.id(), f.industrial.id(), f.civic.id(), f.other.id(), f.none.id()];

        // Ground areas: kind -> (class, level).
        let g = &world.ground;
        let mut area_kinds = AHashMap::new();
        for (name, a) in &g.areas {
            for k in &a.kinds {
                if area_kinds.insert(k.clone(), (a.class.id(), a.level)).is_some() {
                    errs.push(format!("ground.areas.{name}.kinds: \"{k}\" is already in another area group"));
                }
            }
        }
        // Roads: highway -> class.
        let rd = &g.roads;
        let mut road_kinds = AHashMap::new();
        let mut road_widths = [rd.other_width_m; 7];
        for (name, grp, class) in [
            ("motorway", &rd.motorway, RoadClass::Motorway),
            ("primary", &rd.primary, RoadClass::Primary),
            ("secondary", &rd.secondary, RoadClass::Secondary),
            ("tertiary", &rd.tertiary, RoadClass::Tertiary),
            ("residential", &rd.residential, RoadClass::Residential),
        ] {
            road_widths[class.id() as usize] = grp.width_m;
            for h in &grp.highways {
                if h.ends_with("_link") {
                    errs.push(format!(
                        "ground.roads.{name}.highways: \"{h}\": list base values only, `_link` variants are included"
                    ));
                }
                if road_kinds.insert(h.clone(), class.id()).is_some() {
                    errs.push(format!("ground.roads.{name}.highways: \"{h}\" is already in another road class"));
                }
            }
        }
        if !errs.is_empty() {
            return Err(errs.join("; "));
        }

        let lod_of = |b: &BuildingLod| LodParams { dp_m: b.dp_m, min_area_m2: b.min_area_m2 };
        let ground_of = |p: &GroundLod| GroundParams {
            area_dp_m: p.area_dp_m,
            min_area_m2: p.min_area_m2,
            road_dp_m: p.road_dp_m,
            max_road_class: p.max_road_class.id(),
            minor_paths: p.minor_paths,
            step_m: p.lift_step_m as f32,
        };
        let bw = &world.buildings;
        Ok(Self {
            band_params: BandParams {
                edge_m: edges,
                hysteresis: b.hysteresis,
                look_ahead_s: b.look_ahead_s,
                look_ahead_clamp: b.look_ahead_clamp,
                default_tile_height_m: b.default_tile_height_m,
            },
            lod: [Some(lod_of(&bw.r0)), Some(lod_of(&bw.r1)), Some(lod_of(&bw.r2)), None],
            ground_lod: [Some(ground_of(&g.r0)), Some(ground_of(&g.r1)), Some(ground_of(&g.r2)), None],
            kind_profile,
            landuse_profile,
            profile_colors,
            area_kinds,
            other_area_level: g.other_area_level,
            road_kinds,
            road_widths,
            minor_paths: rd.minor_paths.iter().cloned().collect(),
            lit_values: rd.lit_values.iter().cloned().collect(),
            cluster_cell_m: l.cluster_cell_m as f32,
            world,
        })
    }

    /// The data tree.
    pub fn world(&self) -> &WorldProfile {
        &self.world
    }

    /// The resolved profile as JSON (for `world_profile()` and tools).
    pub fn to_json(&self) -> Value {
        serde_json::to_value(&self.world).unwrap_or(Value::Null)
    }

    /// Band edges, hysteresis and look-ahead for the cover (`rings`).
    pub fn bands(&self) -> &BandParams {
        &self.band_params
    }

    /// Building meshing parameters of a ring; `None` for R3 (light points only).
    pub fn lod(&self, ring: usize) -> Option<LodParams> {
        self.lod.get(ring).copied().flatten()
    }

    /// Ground meshing parameters of a ring; `None` for R3 (no ground).
    pub fn ground_params(&self, ring: usize) -> Option<GroundParams> {
        self.ground_lod.get(ring).copied().flatten()
    }

    /// Light profile id of an OSM `building` value (`None` = unknown, resolve via landuse).
    pub fn profile_for_kind(&self, kind: &str) -> Option<u8> {
        self.kind_profile.get(kind).copied()
    }

    /// Light profile id of a building of unknown kind, by its landuse.
    pub fn landuse_profile(&self, l: Option<Landuse>) -> u8 {
        self.landuse_profile[landuse_index(l)]
    }

    /// Light color (rgba, a = 1) of a profile id; residential for anything unknown.
    pub fn profile_color(&self, profile: u8) -> [f32; 4] {
        self.profile_colors.get(profile as usize).copied().unwrap_or(self.profile_colors[PROFILE_RESIDENTIAL as usize])
    }

    pub fn cluster_cell_m(&self) -> f32 {
        self.cluster_cell_m
    }

    /// `landuse` layer `kind` -> (ground class, stacking level).
    pub fn ground_area_class(&self, kind: &str) -> (u8, u8) {
        self.area_kinds.get(kind).copied().unwrap_or((GROUND_OTHER, self.other_area_level))
    }

    /// `highway` value -> (road class, is a `_link`, is a minor path).
    pub fn road_class(&self, highway: &str) -> (u8, bool, bool) {
        let link = highway.ends_with("_link");
        let base = highway.strip_suffix("_link").unwrap_or(highway);
        let class = self.road_kinds.get(base).copied().unwrap_or(ROAD_OTHER);
        let minor = class == ROAD_OTHER && self.minor_paths.contains(base);
        (class, link, minor)
    }

    /// OSM `lit` value -> lit.
    pub fn is_lit(&self, v: &str) -> bool {
        self.lit_values.contains(v.trim())
    }

    /// Full strip width (m) of a road; `_link` roads get `link_width_factor` of it in 0.1 m
    /// steps, at least `link_min_width_m`.
    pub fn road_width_m(&self, class: u8, link: bool) -> f64 {
        let w = self.road_widths[class.clamp(ROAD_MOTORWAY, ROAD_OTHER) as usize];
        if link {
            let r = &self.world.ground.roads;
            ((w * (r.link_width_factor * 10.0)).round() / 10.0).max(r.link_min_width_m)
        } else {
            w
        }
    }

    /// Retry delay after `attempts` failures: base doubled per attempt (at most 2^6), capped.
    pub fn backoff(&self, attempts: u32) -> std::time::Duration {
        let s = &self.world.streaming;
        let base_ms = (s.retry_base_s * 1000.0).round() as u64;
        let max_ms = (s.retry_max_s * 1000.0).round() as u64;
        let ms = base_ms.saturating_mul(1u64 << attempts.saturating_sub(1).min(6));
        std::time::Duration::from_millis(ms.min(max_ms))
    }
}

// ---------------------------------------------------------------------------------------------
// Loading: JSON text + overrides -> Profile

/// Kind of a JSON value, for type errors.
fn kind_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(n) if n.is_u64() || n.is_i64() => "an integer",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

fn join(path: &str, k: &str) -> String {
    if path.is_empty() {
        k.to_string()
    } else {
        format!("{path}.{k}")
    }
}

/// Check `v` (from a file or overrides) against the defaults' shape `base`: unknown keys
/// (error, or warning + removal when `lenient`) and JSON types. Integral floats are turned into
/// integers where the default is an integer (Godot Dictionaries carry JSON numbers as floats).
fn check_shape(
    v: &mut Value,
    base: &Value,
    path: &str,
    lenient: bool,
    errs: &mut Vec<String>,
    warns: &mut Vec<String>,
) {
    match (v, base) {
        (Value::Object(m), Value::Object(b)) => {
            let free = FREE_KEY_MAPS.contains(&path);
            let keys: Vec<String> = m.keys().cloned().collect();
            for k in keys {
                let p = join(path, &k);
                let template = match b.get(&k) {
                    Some(t) => Some(t),
                    None if free => b.values().next(),
                    None => None,
                };
                match template {
                    Some(t) => {
                        if let Some(x) = m.get_mut(&k) {
                            check_shape(x, t, &p, lenient, errs, warns);
                        }
                    }
                    None if lenient => {
                        warns.push(format!("unknown key `{p}` ignored (written for a newer profile_version)"));
                        m.remove(&k);
                    }
                    None => errs.push(format!("unknown key `{p}`")),
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            if let Some(t) = b.first() {
                for x in a.iter_mut() {
                    check_shape(x, t, &format!("{path}[]"), lenient, errs, warns);
                }
            }
        }
        (x @ Value::Number(_), Value::Number(bn)) => {
            let n = x.as_f64().unwrap_or(f64::NAN);
            if bn.is_u64() || bn.is_i64() {
                if !(n.fract() == 0.0 && n.is_finite()) {
                    errs.push(format!("{} = {} must be an integer", path.trim_end_matches("[]"), fmt_num(n)));
                } else if n < 0.0 {
                    // Range checks name the allowed range later; keep the value for the message.
                    *x = Value::from(n as i64);
                } else {
                    *x = Value::from(n as u64);
                }
            }
        }
        (x, b) => {
            if std::mem::discriminant(x) != std::mem::discriminant(b) {
                errs.push(format!("{} must be {}, got {}", path.trim_end_matches("[]"), kind_name(b), kind_name(x)));
            }
        }
    }
}

fn merge(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(x) => merge(x, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, o) => *b = o,
    }
}

/// Version policy + shape check of one source (file or overrides), merged onto `acc`.
fn apply_source(acc: &mut Value, defaults: &Value, mut src: Value, label: &str, log: &dyn Log) -> Result<(), String> {
    let Value::Object(m) = &src else {
        return Err(format!("{label}: the profile must be a JSON object, got {}", kind_name(&src)));
    };
    let version = match m.get("profile_version") {
        None => PROFILE_VERSION as u64,
        Some(Value::Number(n)) if n.as_f64().is_some_and(|f| f.fract() == 0.0 && f >= 1.0) => n.as_f64().unwrap_or(1.0) as u64,
        Some(v) => {
            return Err(format!("{label}: profile_version must be a positive integer, got {v}"));
        }
    };
    let lenient = version > PROFILE_VERSION as u64;
    let mut errs = Vec::new();
    let mut warns = Vec::new();
    check_shape(&mut src, defaults, "", lenient, &mut errs, &mut warns);
    for w in warns {
        log.warn(&format!("WorldStreamer: {label} (profile_version {version}, library reads {PROFILE_VERSION}): {w}"));
    }
    if !errs.is_empty() {
        return Err(format!("{label}: {}", errs.join("; ")));
    }
    if let Value::Object(m) = &mut src {
        m.remove("profile_version");
    }
    merge(acc, src);
    Ok(())
}

/// Parse JSON text (with the position of a syntax error).
pub fn parse_json(text: &str, label: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|e| format!("{label}: invalid JSON: {e}"))
}

/// Build a profile: defaults, then the file (if any), then the overrides (if any). Both are
/// partial; objects merge deeply. `file` = (label for messages, JSON text). Warnings (unknown
/// keys of a newer profile version) go to `log`.
pub fn resolve(file: Option<(&str, &str)>, overrides: Option<&Value>, log: &dyn Log) -> Result<Profile, String> {
    let defaults = serde_json::to_value(WorldProfile::default()).map_err(|e| format!("profile: {e}"))?;
    let mut acc = defaults.clone();
    if let Some((label, text)) = file {
        let v = parse_json(text, label)?;
        apply_source(&mut acc, &defaults, v, label, log)?;
    }
    if let Some(o) = overrides {
        apply_source(&mut acc, &defaults, o.clone(), "profile_overrides", log)?;
    }
    if let Value::Object(m) = &mut acc {
        m.insert("profile_version".into(), Value::from(PROFILE_VERSION));
    }
    // Ranges first, on the JSON form: messages then name the key even for values the typed
    // fields could not hold (e.g. a negative count).
    let mut errs = Vec::new();
    check_ranges(&acc, "", &mut errs);
    if !errs.is_empty() {
        return Err(format!("profile: {}", errs.join("; ")));
    }
    let world: WorldProfile = serde_json::from_value(acc).map_err(|e| format!("profile: {e}"))?;
    Profile::new(world).map_err(|e| format!("profile: {e}"))
}

/// [`resolve`] with the file read from disk (CLI, Rust hosts).
pub fn load_file(path: &str, overrides: Option<&Value>, log: &dyn Log) -> Result<Profile, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    resolve(Some((path, &text)), overrides, log)
}

/// Keys of a JSON object tree as dotted paths (arrays are leaves). Used by the schema tests.
pub fn key_paths(v: &Value) -> Vec<String> {
    fn walk(v: &Value, path: &str, out: &mut Vec<String>) {
        if let Value::Object(m) = v {
            for (k, x) in m {
                let p = join(path, k);
                out.push(p.clone());
                walk(x, &p, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(v, "", &mut out);
    out.sort();
    out
}

/// Empty JSON object (no overrides).
pub fn empty_object() -> Value {
    Value::Object(Map::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::NullLog;
    use std::sync::Mutex;

    const DEFAULT_JSON: &str = include_str!("../../../addons/simworld/profiles/default.json");
    const SCHEMA_JSON: &str = include_str!("../../../addons/simworld/profiles/world-profile.schema.json");

    #[derive(Default)]
    struct CaptureLog(Mutex<Vec<String>>);
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
        assert_eq!(b.edge_m, [600.0, 2500.0, 7000.0, 25_000.0]);
        assert_eq!((b.hysteresis, b.look_ahead_s, b.look_ahead_clamp, b.default_tile_height_m), (1.15, 20.0, 0.5, 60.0));
        assert_eq!(p.lod(0), Some(LodParams { dp_m: 0.5, min_area_m2: 0.0 }));
        assert_eq!(p.lod(1), Some(LodParams { dp_m: 2.0, min_area_m2: 20.0 }));
        assert_eq!(p.lod(2), Some(LodParams { dp_m: 4.0, min_area_m2: 50.0 }));
        assert_eq!(p.lod(3), None);
        let gp = |a, m, r, c, minor, s| Some(GroundParams { area_dp_m: a, min_area_m2: m, road_dp_m: r, max_road_class: c, minor_paths: minor, step_m: s });
        assert_eq!(p.ground_params(0), gp(0.5, 0.0, 0.5, ROAD_OTHER, true, 0.02f32));
        assert_eq!(p.ground_params(1), gp(2.0, 200.0, 2.0, ROAD_OTHER, false, 0.06f32));
        assert_eq!(p.ground_params(2), gp(6.0, 2500.0, 5.0, ROAD_RESIDENTIAL, false, 0.15f32));
        assert_eq!(p.ground_params(3), None);
        assert_eq!(p.profile_color(PROFILE_RESIDENTIAL), [1.0, 0.655, 0.341, 1.0]);
        assert_eq!(p.profile_color(PROFILE_OFFICE), [0.86, 0.9, 1.0, 1.0]);
        assert_eq!(p.profile_color(PROFILE_RETAIL), [1.0, 0.98, 0.95, 1.0]);
        assert_eq!(p.profile_color(PROFILE_INDUSTRIAL), [1.0, 0.55, 0.15, 1.0]);
        assert_eq!(p.profile_color(PROFILE_CIVIC), [1.0, 0.706, 0.42, 1.0]);
        assert_eq!(p.profile_color(crate::mesher::PROFILE_UNKNOWN), [1.0, 0.655, 0.341, 1.0]);
        assert_eq!(p.cluster_cell_m(), 100.0f32);
        let w = p.world();
        assert_eq!((w.lights.cluster_on_agl_m, w.lights.cluster_off_agl_m), (3300.0, 2700.0));
        assert_eq!((w.streaming.max_fetch_per_band, w.streaming.max_mesh_per_band, w.streaming.redeliveries_per_frame), (4, 2, 1));
        assert_eq!(w.fades.cap_s, 3.0);
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
        let mut schema_keys: Vec<String> = nodes.iter().map(|(p, _)| p.clone()).filter(|p| !p.ends_with("[]")).collect();
        schema_keys.sort();
        schema_keys.dedup();
        // default.json keys with free-map entries generalized to `*`.
        let d = parse_json(DEFAULT_JSON, "d").unwrap();
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
        let file = r#"{ "profile_version": 1, "bands": { "r0_edge_m": 450 }, "lights": { "profiles": { "office": { "color": [0.5, 0.6, 0.7] } } } }"#;
        let ov = serde_json::json!({ "bands": { "hysteresis": 1.2 }, "streaming": { "max_fetch_per_band": 6.0 },
            "ground": { "areas": { "beach": { "class": "other", "level": 1, "kinds": ["beach"] } } } });
        let p = resolve(Some(("f.json", file)), Some(&ov), &NullLog).unwrap();
        let d = WorldProfile::default();
        assert_eq!(p.bands.r0_edge_m, 450.0);
        assert_eq!(p.bands.r1_edge_m, d.bands.r1_edge_m);
        assert_eq!(p.bands.hysteresis, 1.2);
        assert_eq!(p.streaming.max_fetch_per_band, 6, "integral floats (Godot) are accepted as integers");
        assert_eq!(p.lights.profiles.office.color, [0.5, 0.6, 0.7]);
        assert_eq!(p.lights.profiles.office.kinds, d.lights.profiles.office.kinds, "kinds kept");
        assert_eq!(p.ground_area_class("beach"), (GROUND_OTHER, 1));
        assert_eq!(p.ground.areas.len(), d.ground.areas.len() + 1);
        // Overrides win over the file.
        let p = resolve(Some(("f.json", file)), Some(&serde_json::json!({"bands": {"r0_edge_m": 500}})), &NullLog).unwrap();
        assert_eq!(p.bands.r0_edge_m, 500.0);
        // Empty sources = defaults.
        assert_eq!(resolve(Some(("e", "{}")), Some(&empty_object()), &NullLog).unwrap(), Profile::default());
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
        let newer = r#"{ "profile_version": 99, "bands": { "r0_edge_m": 450, "r4_edge_m": 90000 }, "weather": {} }"#;
        let log = CaptureLog::default();
        let p = resolve(Some(("new.json", newer)), None, &log).unwrap();
        assert_eq!(p.bands.r0_edge_m, 450.0);
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
        let e = resolve(None, Some(&serde_json::json!({"bands": {"r0_edge_m": "far"}})), &NullLog).unwrap_err();
        assert!(e.contains("bands.r0_edge_m must be a number, got a string"), "{e}");
        let e = resolve(None, Some(&serde_json::json!({"bands": {"r1_edge_m": 500}})), &NullLog).unwrap_err();
        assert!(e.contains("bands.r1_edge_m = 500 must be greater than bands.r0_edge_m = 600"), "{e}");
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
}
