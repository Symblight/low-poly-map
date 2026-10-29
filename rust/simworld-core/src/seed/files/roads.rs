//! `roads.json`: road polylines between cities.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoadClassName {
    Motorway,
    Trunk,
    Primary,
    Secondary,
    Tertiary,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Road {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub class: RoadClassName,
    #[serde(default)]
    pub lit: f64,
    #[serde(default)]
    pub from: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
    pub points: Vec<[f64; 2]>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoadsFile {
    pub format: u32,
    pub roads: Vec<Road>,
}
