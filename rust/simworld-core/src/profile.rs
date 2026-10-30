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

use std::ops::Deref;

use ahash::{AHashMap, AHashSet};
use serde_json::Value;

use crate::ground::GroundParams;
use crate::mesher::{LodParams, PROFILE_COUNT, PROFILE_RESIDENTIAL};
use crate::mvt::{BuildingTags, Landuse, OsmType, GROUND_OTHER, ROAD_MOTORWAY, ROAD_OTHER};
use crate::rings::{BandParams, RING_COUNT};

/// Profile schema version this library reads. Bump when keys are added (and update
/// `default.json`, the JSON Schema and `CHANGELOG.md`).
pub const PROFILE_VERSION: u32 = 7;

/// Default `terrain.tau_px` (Terrain v2 G2): calibrated so the default view (60 deg, 1080 px)
/// over the default relief (amplitude 450 m) holds about as many terrain tiles as the fixed
/// T0..T4 bands did (`scheduler::tests::terrain_cover::terrain_cover_counts`).
pub const TERRAIN_TAU_PX_DEFAULT: f64 = 0.9;

mod compile;
mod defaults;
mod light_defaults;
mod light_schema;
mod ranges;
mod resolve;
mod schema;
mod seed_city_schema;
mod seed_schema;
mod terrain_schema;
#[cfg(test)]
mod defaults_tests;
#[cfg(test)]
mod resolve_tests;

pub use light_schema::*;
pub use ranges::{range_of, RANGES};
pub use resolve::{empty_object, key_paths, load_file, parse_json, resolve};
pub use schema::*;
pub use seed_city_schema::*;
pub use seed_schema::*;
pub use terrain_schema::*;
use compile::{landuse_index, StateRule};

// ---------------------------------------------------------------------------------------------
// Compiled profile

/// A validated [`WorldProfile`] plus the lookup tables the workers use. Cheap to share as
/// `Arc<Profile>`; derefs to the data tree.
#[derive(Clone, Debug)]
pub struct Profile {
    world: WorldProfile,
    band_params: BandParams,
    building_lod: [Option<LodParams>; RING_COUNT],
    ground_lod: [Option<GroundParams>; RING_COUNT],
    kind_profile: AHashMap<String, u8>,
    /// Indexed by `landuse_index`.
    landuse_profile: [u8; 7],
    /// rgba (a = 1) by profile id 0..7 (5 = unknown -> residential's).
    profile_colors: [[f32; 4]; PROFILE_COUNT],
    /// Window column width (m) by profile id; `None` = no windows.
    window_width: [Option<f32>; PROFILE_COUNT],
    /// (OSM type tag, id) -> profile id (`buildings.overrides`).
    overrides: AHashMap<(u64, u64), u8>,
    dark_states: Vec<StateRule>,
    slender_ratio: f32,
    slender_max_area_m2: f32,
    min_window_wall_m: f32,
    /// Non-fatal findings (logged by [`resolve`]).
    warnings: Vec<String>,
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

/// One compiled `buildings.dark_states` entry.
impl Profile {
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
        self.building_lod.get(ring).copied().flatten()
    }

    /// Ground meshing parameters of a ring (every band since api 1.3; `None` only out of range).
    pub fn ground_params(&self, ring: usize) -> Option<GroundParams> {
        self.ground_lod.get(ring).copied().flatten()
    }

    /// Light profile id of an OSM `building`, `historic` or `man_made` value (`None` = not in
    /// any profile's `kinds`).
    pub fn profile_for_kind(&self, kind: &str) -> Option<u8> {
        self.kind_profile.get(kind).copied()
    }

    /// Light profile forced for this OSM element by `buildings.overrides`.
    pub fn override_profile(&self, osm_type: OsmType, id: u64) -> Option<u8> {
        if self.overrides.is_empty() {
            return None;
        }
        self.overrides.get(&(osm_type.tag(), id)).copied()
    }

    /// The building's state tags match one of `buildings.dark_states`.
    pub fn is_dark_state(&self, tags: &BuildingTags) -> bool {
        self.dark_states.iter().any(|r| r.matches(tags))
    }

    /// `buildings.slender_tower`: `height >= ratio * sqrt(area)` and `0 < area < max_area_m2`.
    pub fn is_slender_tower(&self, height_m: f32, area_m2: f32) -> bool {
        area_m2 > 0.0 && area_m2 < self.slender_max_area_m2 && height_m >= self.slender_ratio * area_m2.sqrt()
    }

    /// Window column width (m) of a profile id; `None` = the profile has no windows.
    pub fn window_width_m(&self, profile: u8) -> Option<f32> {
        self.window_width.get(profile as usize).copied().flatten()
    }

    /// Wall parts lower than this (m) get no windows (`buildings.min_window_wall_m`).
    pub fn min_window_wall_m(&self) -> f32 {
        self.min_window_wall_m
    }

    /// Non-fatal findings of validation (e.g. a `lit_share` that rises in the evening).
    pub fn warnings(&self) -> &[String] {
        &self.warnings
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

    /// Finest band with any light set (building lights or street lamps): the bands whose tiles change with the light mode.
    pub fn lit_min_ring(&self) -> usize {
        self.lights.min_ring.min(self.lights.street.min_ring).min(self.lights.beacons.min_ring) as usize
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

    /// Idle time after which an unneeded decoded data tile is dropped (`memory.data_idle_s`).
    pub fn data_idle(&self) -> std::time::Duration {
        std::time::Duration::from_secs_f64(self.world.memory.data_idle_s)
    }
}
