//! The `lights` section of the profile data tree: clustering, street lamps, beacons, the light
//! profiles with their windows and floodlight, and the landuse fallback.

use serde::{Deserialize, Serialize};

use super::LightProfileName;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Lights {
    /// Light clustering (100 m light carpet) switches on above this AGL (m) ...
    pub cluster_on_agl_m: f64,
    /// ... and off below this one.
    pub cluster_off_agl_m: f64,
    /// Cell size (m) of the clustered light carpet.
    pub cluster_cell_m: f64,
    /// Finest band that carries light sprites (0..3): bands `min_ring..=3` get lights, finer ones none. Default 1 = R1..R3
    /// (profile_version 4; was 2 in version 3): R0 has windows only, R1 has windows up close and the host cross-fades
    /// windows and sprites over its blend range (docs/spec-night-windows.md W4); 2 = R2/R3 only. Added in profile_version 3.
    pub min_ring: u32,
    /// Street lamps along roads (their own sprite set, `street_lights` in the ready tile).
    pub street: StreetLights,
    /// Aircraft warning beacons on tall buildings (their own sprite set, `beacon_lights` in the ready tile).
    pub beacons: Beacons,
    pub profiles: LightProfiles,
    /// Profile of buildings whose kind is in no profile, by the landuse around them.
    pub landuse_fallback: LanduseFallback,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StreetLights {
    /// Finest band with street lamps (0..3); bands `min_ring..=3` get them, 4 = none. The archive keeps only major roads at
    /// z13 (R3), so R3 lamps line the motorways .. secondaries.
    pub min_ring: u32,
    /// Sprite color (linear rgb 0..1), sodium orange by default.
    pub color: [f64; 3],
    /// Second lamp type (white LED by default) and the share of roads that have it, chosen per road by its OSM id, so a
    /// street keeps one lamp type along its length. 0 = every road in `color`.
    pub alt_color: [f64; 3],
    pub alt_share: f64,
    /// Distance between lamps along a road centerline (m).
    pub spacing_m: f64,
    /// Multiplier of `spacing_m` per band R0..R3 (api 1.11): a lamp of a band with scale `k` stands for `k` lamps (alpha
    /// `intensity(k)`), so the far bands carry fewer, brighter sprites. Default `[1, 1, 1, 2]`.
    pub spacing_scale: [f64; 4],
    /// Lamp height above the road (m).
    pub height_m: f64,
    /// Roads of this class or a more important one (1 motorway .. 5 residential, 6 other) get lamps without a `lit` tag;
    /// a road tagged lit always does. 0 = tagged roads only.
    pub untagged_max_class: u32,
}

impl Default for StreetLights {
    fn default() -> Self {
        Self {
            min_ring: 0,
            color: [1.0, 0.42, 0.1],
            alt_color: [0.78, 0.86, 1.0],
            alt_share: 0.3,
            spacing_m: 35.0,
            spacing_scale: [1.0, 1.0, 1.0, 2.0],
            height_m: 9.0,
            untagged_max_class: 5,
        }
    }
}

/// Aircraft warning beacons, placed from geometry alone (the resolved roof height the mesher extrudes to and the footprint),
/// never from tags, so a synthesised city gets them by the same rule.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Beacons {
    /// Finest band with beacons (0..3); bands `min_ring..=3` get them, 4 = none.
    pub min_ring: u32,
    /// Buildings whose roof is at least this high (m above their base) get beacons on the roof corners.
    pub min_height_m: f64,
    /// Taller buildings also get a ring of beacons every `level_spacing_m` up the facade (ICAO-style intermediate levels: at
    /// `level_spacing_m`, 2x, ... while at least half a spacing below the roof). 0 = roof only.
    pub level_spacing_m: f64,
    /// Footprints narrower than this (m, along either axis) get one beacon in the middle of the roof instead of the corners.
    pub corner_min_size_m: f64,
    /// Beacon color (linear rgb 0..1), red by default.
    pub color: [f64; 3],
    /// Second color (green by default) and the share of beacon buildings that have it, chosen per building by its OSM id.
    pub alt_color: [f64; 3],
    pub alt_share: f64,
}

impl Default for Beacons {
    fn default() -> Self {
        Self {
            min_ring: 0,
            min_height_m: 60.0,
            level_spacing_m: 45.0,
            corner_min_size_m: 15.0,
            color: [1.0, 0.04, 0.02],
            alt_color: [0.1, 1.0, 0.3],
            alt_share: 0.15,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LightProfiles {
    pub residential: LightProfile,
    pub office: LightProfile,
    pub retail: LightProfile,
    pub industrial: LightProfile,
    pub civic: LightProfile,
    /// Castles, towers, churches, ...: no windows, a floodlit facade, light points in `color`.
    pub landmark: LightProfile,
    /// Ruins, garages, silos, chimneys, ...: no windows and no light point (`color` unused).
    pub dark: LightProfile,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LightProfile {
    /// Light sprite color (linear rgb 0..1).
    pub color: [f64; 3],
    /// OSM `building`, `historic` and `man_made` values of this profile (one table).
    pub kinds: Vec<String>,
    /// Lit windows (residential, office, retail, civic only; absent = no windows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows: Option<WindowSpec>,
    /// Floodlit facade (landmark only, required there).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floodlight: Option<Floodlight>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowSpec {
    /// Target window column width (m); read by the mesher's wall layout.
    pub width_m: f64,
    /// Share of lit windows per local hour 0..23 (24 values, 0..1); read by the host shader.
    pub lit_share: Vec<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Floodlight {
    /// Emission multiplier at the base of the facade (host shader).
    pub strength: f64,
    /// Height (m above the wall bottom) over which the floodlight fades out.
    pub fade_height_m: f64,
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
