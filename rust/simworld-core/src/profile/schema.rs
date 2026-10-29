//! The data tree of the world profile: the top level and the band, LOD, building, ground,
//! road, streaming, fade, origin, cache and memory sections (lights, terrain and seed have their
//! own files).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::mesher::{
    PROFILE_CIVIC, PROFILE_DARK, PROFILE_INDUSTRIAL, PROFILE_LANDMARK, PROFILE_OFFICE, PROFILE_RESIDENTIAL, PROFILE_RETAIL,
};
use crate::mvt::{
    GROUND_COMMERCIAL, GROUND_FOREST, GROUND_GRASS, GROUND_INDUSTRIAL, GROUND_OTHER, GROUND_RESIDENTIAL, GROUND_WATER, ROAD_MOTORWAY,
    ROAD_OTHER, ROAD_PRIMARY, ROAD_RESIDENTIAL, ROAD_SECONDARY, ROAD_TERTIARY,
};

use super::{Lights, SeedProfile, TerrainProfile};

// ---------------------------------------------------------------------------------------------
// Data tree

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WorldProfile {
    /// Schema version of the file (see module docs). The resolved profile always carries
    /// [`PROFILE_VERSION`].
    pub profile_version: u32,
    pub bands: Bands,
    /// Screen-space-error LOD of the vector layer (profile_version 7, api 1.10, Tiles and LOD v2
    /// step T3a): replaces the fixed band edges `bands.r0..r2_edge_m`.
    pub lod: Lod,
    pub buildings: Buildings,
    pub ground: Ground,
    pub lights: Lights,
    pub streaming: Streaming,
    pub fades: Fades,
    pub origin: Origin,
    pub cache: Cache,
    /// Memory budget of the core's CPU-side pools (profile_version 7, Terrain v2 section 6).
    pub memory: Memory,
    /// Procedural terrain (profile_version 5, docs/spec-terrain.md). Off by default; takes effect
    /// only when the host also requests the layer `"terrain"` (`configure_world` option `layers`).
    pub terrain: TerrainProfile,
    /// World seed (profile_version 6, docs/spec-world-seed.md section 9.2): how generated
    /// airports, countryside and the terrain-water rule look. Used only by a world configured
    /// with the `seed_world` option.
    pub seed: SeedProfile,
}

/// LOD bands: the outer limit of the cover, hysteresis, look-ahead. The edges between the bands
/// come from the screen-space error (`lod`, profile_version 7); `r0..r2_edge_m` are gone and
/// `r3_edge_m` is `max_distance_m` (an older file migrates, see [`resolve`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Bands {
    /// Outer edge of the whole cover (R3's edge): nothing is requested beyond it.
    pub max_distance_m: f64,
    /// A tile changes band only once its slant distance is this factor past the edge.
    pub hysteresis: f64,
    /// Look-ahead horizon (s): camera + velocity x this (3D).
    pub look_ahead_s: f64,
    /// Look-ahead offset clamp as a fraction of the band's edge.
    pub look_ahead_clamp: f64,
    /// Box height (m) of a tile whose own and ancestors' heights are still unknown.
    pub default_tile_height_m: f64,
}

/// Screen-space-error LOD (profile_version 7). A tile of ring r > 0 refines while
/// `error_m.r{r} * H / (2 d tan(fov_y / 2)) > tau_eff` (H, fov_y from the host's `set_view`;
/// tau_eff = `tau_px` x the `set_lod_quality` scale), coarsens at `tau_eff / bands.hysteresis`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Lod {
    /// SSE threshold (px) at quality 1.
    pub tau_px: f64,
    /// Tile budget of the vector cover (estimated tiles, all rings; 0 = no cap): a view or
    /// quality whose cover would exceed it runs with a higher tau (`rings::BandParams::
    /// capped_quality`; review M1). The default view needs about 450.
    pub max_tiles: u32,
    /// Geometric error per coarse level (m): what is lost when that chunk is not refined.
    pub error_m: LodErrors,
    /// Rear-hemisphere tau multiplier (stored; used from step T4).
    pub rear_tau_scale: f64,
    /// Frame-budget scaling of tau (stored; used from step T5).
    pub adaptive: LodAdaptive,
}

/// `lod.error_m`: error of the R1, R2, R3 chunk against the next finer level (strictly increasing).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LodErrors {
    pub r1: f64,
    pub r2: f64,
    pub r3: f64,
}

/// `lod.adaptive` (stored; the adaptive tau of step T5).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LodAdaptive {
    pub enabled: bool,
    pub frame_budget_ms: f64,
    pub min_scale: f64,
    pub max_scale: f64,
    pub smoothing_s: f64,
}

/// Building meshing per band (R3 has no buildings) and light-profile resolution rules
/// (profile_version 4, docs/spec-night-windows.md section 1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Buildings {
    pub r0: BuildingLod,
    pub r1: BuildingLod,
    pub r2: BuildingLod,
    /// Light profile by OSM id (`n123`, `w123`, `r123`); wins over every other rule.
    pub overrides: BTreeMap<String, LightProfileName>,
    /// State tags that make a building `dark` whatever its kind: `key` (present / yes) or
    /// `key=value` over the tile attributes `historic`, `man_made`, `ruins`, `disused`, `abandoned`.
    pub dark_states: Vec<String>,
    /// Shape heuristic for generic kinds with a tagged height or levels and an unclipped footprint.
    pub slender_tower: SlenderTower,
    /// Wall parts lower than this (m, extrusion height) get no windows.
    pub min_window_wall_m: f64,
}

/// A generic building is a tower (`landmark`) when `height >= ratio * sqrt(area)` and
/// `area < max_area_m2`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlenderTower {
    pub ratio: f64,
    pub max_area_m2: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildingLod {
    /// Douglas-Peucker tolerance (m).
    pub dp_m: f64,
    /// Drop buildings with a smaller footprint (m^2).
    pub min_area_m2: f64,
    /// Drop buildings whose resolved roof height (m above ground: tagged height, else levels
    /// heuristic, else fallback) is lower. Mesh only; lights keep every building. 0 = keep all.
    /// Added in profile_version 2 (a version 1 file without it keeps the default).
    #[serde(default)]
    pub min_height_m: f64,
}

/// Ground meshing: per-band parameters, road classes and widths, area classes and levels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Ground {
    pub r0: GroundLod,
    pub r1: GroundLod,
    pub r2: GroundLod,
    /// R3 (api 1.3): a coarse ground so a far tile is never see-through while its R2 children load.
    pub r3: GroundLod,
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
    Landmark,
    Dark,
}

impl LightProfileName {
    pub fn id(self) -> u8 {
        match self {
            Self::Residential => PROFILE_RESIDENTIAL,
            Self::Office => PROFILE_OFFICE,
            Self::Retail => PROFILE_RETAIL,
            Self::Industrial => PROFILE_INDUSTRIAL,
            Self::Civic => PROFILE_CIVIC,
            Self::Landmark => PROFILE_LANDMARK,
            Self::Dark => PROFILE_DARK,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Streaming {
    /// Fetches in flight per band.
    pub max_fetch_per_band: u32,
    /// Mesh (decode) jobs in flight per band. Default 8 since review M2 (was 2: results come
    /// back a frame later, so a band built at most ~2 chunks per frame and a cold fill of ~250
    /// R3 chunks took ~2 s at 60 Hz although every data tile was resident after 0.2 s).
    pub max_mesh_per_band: u32,
    /// Light-mode re-deliveries per frame.
    pub redeliveries_per_frame: u32,
    /// Worker threads of the CPU job pool (decode, mesh, terrain); 0 = automatic: cores - 2,
    /// at least 2, at most 8 (perf review 2; was fixed at cores - 2 clamped to 2..=4). A mobile
    /// host sets its own count. Read at configure.
    pub worker_threads: u32,
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
    /// Most LOD swaps fading at once, both layers together (profile_version 6): the dither
    /// variant discards, which turns off hidden surface removal on tile-based GPUs. A swap
    /// beyond the cap completes at once, without a fade. 0 = no fades at all.
    pub max_active: u32,
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

/// One byte budget for the core's CPU-side pools (decoded data tiles, HeightTiles, collision
/// patches; `budget::MemoryBudget`). GPU memory is not part of it. One number for every
/// platform: an iOS host passes `memory.budget_mb` = 256 as a profile override.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Memory {
    /// Budget (MiB).
    pub budget_mb: f64,
    /// A decoded data tile no tile of the cover maps to is dropped after this long (s; review
    /// M3). 0 = at once. The budget still evicts by priority on top.
    pub data_idle_s: f64,
    /// Recently delivered tiles kept in memory so a key that comes back (a coarsening parent
    /// after the camera passed) is delivered without a rebuild (MiB per layer, 0 = off; perf
    /// review 2 item 6, `scheduler::recent`). Separate from `budget_mb`.
    pub recent_tiles_mb: f64,
    /// How long a kept tile stays after it left the host (s).
    pub recent_tiles_s: f64,
}
