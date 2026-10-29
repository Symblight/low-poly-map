//! `world.json`: the manifest, the airport selection and the airport / runway overrides.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::Runway;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: u32,
    pub world_version: i64,
    pub world_seed: String,
    pub water: String,
    pub airports: AirportSelection,
    pub cities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roads: Option<String>,
}

fn default_airport_types() -> Vec<String> {
    ["large_airport", "medium_airport", "small_airport"].iter().map(|s| s.to_string()).collect()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AirportSelection {
    pub file: String,
    #[serde(default = "default_airport_types")]
    pub types: Vec<String>,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub overrides: BTreeMap<String, AirportOverride>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AirportOverride {
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub elevation_ft: Option<f64>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub runways: BTreeMap<String, RunwayOverride>,
    #[serde(default)]
    pub add: Vec<Runway>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunwayOverride {
    pub le_lat: Option<f64>,
    pub le_lon: Option<f64>,
    pub he_lat: Option<f64>,
    pub he_lon: Option<f64>,
    pub width_ft: Option<f64>,
    pub length_ft: Option<f64>,
    pub surface: Option<String>,
    pub lighted: Option<bool>,
    pub closed: Option<bool>,
    #[serde(rename = "le_heading_degT")]
    pub le_heading_deg_t: Option<f64>,
    pub le_displaced_threshold_ft: Option<f64>,
    pub he_displaced_threshold_ft: Option<f64>,
    #[serde(default)]
    pub remove: bool,
}
