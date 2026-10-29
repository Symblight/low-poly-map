//! Seed world files (`docs/spec-world-seed.md` section 4): typed forms of `world.json`,
//! `cities/<id>.json`, `airports.json`, `water.json` and `roads.json`, their loaders, and the
//! load-time validation of section 4.6 (the same checks `simworld-cli seed validate` runs).
//!
//! Every file is checked against its JSON Schema in `addons/simworld/seed/` first (errors name the
//! file, the key path and the value), then parsed into the typed form and quantized (coordinates
//! to 1e-7 degrees). The core never touches a file system here: the host hands it text through a
//! reader callback, so the Godot binding (res://, user://) and the CLI share one parser.

mod airports;
mod city;
mod load;
mod manifest;
mod parse;
mod roads;
mod water;

pub use airports::{Airport, AirportFile, Runway};
pub use city::{
    hex_center, City, DistrictRequest, DistrictType, Fill, InReal, Landmark, LandmarkGround, LatLon, Layout, LayoutType,
    Near, NearFeature, RealSource, Style, FOOTPRINT_NOISE,
};
pub use load::{apply_overrides, approx_distance_m, load_world, select_airports, LoadError, Reader, SeedFiles};
pub use manifest::{AirportOverride, AirportSelection, Manifest, RunwayOverride};
pub use parse::{check_text, parse_airports, parse_city, parse_manifest, parse_roads, parse_water, schemas, FileKind};
pub use roads::{Road, RoadClassName, RoadsFile};
pub use water::{clean_ring, Outside, WaterFile, WaterKind, WaterPolygon};

pub const WORLD_SCHEMA: &str = include_str!("../../../../addons/simworld/seed/world.schema.json");
pub const CITY_SCHEMA: &str = include_str!("../../../../addons/simworld/seed/city.schema.json");
pub const AIRPORTS_SCHEMA: &str = include_str!("../../../../addons/simworld/seed/airports.schema.json");
pub const WATER_SCHEMA: &str = include_str!("../../../../addons/simworld/seed/water.schema.json");
pub const ROADS_SCHEMA: &str = include_str!("../../../../addons/simworld/seed/roads.schema.json");

/// (file name, text) of every seed schema, for cross-file `$ref`s.
pub const SCHEMAS: [(&str, &str); 5] = [
    ("world.schema.json", WORLD_SCHEMA),
    ("city.schema.json", CITY_SCHEMA),
    ("airports.schema.json", AIRPORTS_SCHEMA),
    ("water.schema.json", WATER_SCHEMA),
    ("roads.schema.json", ROADS_SCHEMA),
];

/// Quantize degrees to 1e-7 (section 7: inputs are quantized on load, so every decision sees the
/// same bits on every platform).
#[inline]
pub fn q7(deg: f64) -> f64 {
    (deg * 1e7).round() / 1e7
}

/// Quantize meters to 1 cm.
#[inline]
pub fn q_cm(m: f64) -> f64 {
    (m * 100.0).round() / 100.0
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
