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
use crate::mesher::{
    LodParams, PROFILE_CIVIC, PROFILE_COUNT, PROFILE_DARK, PROFILE_INDUSTRIAL, PROFILE_LANDMARK, PROFILE_OFFICE,
    PROFILE_RESIDENTIAL, PROFILE_RETAIL, PROFILE_UNKNOWN,
};
use crate::mvt::{
    BuildingTags, Landuse, OsmType, GROUND_COMMERCIAL, GROUND_FOREST, GROUND_GRASS, GROUND_INDUSTRIAL, GROUND_OTHER, GROUND_RESIDENTIAL,
    GROUND_WATER, ROAD_MOTORWAY, ROAD_OTHER, ROAD_PRIMARY, ROAD_RESIDENTIAL, ROAD_SECONDARY, ROAD_TERTIARY,
};
use crate::rings::{BandParams, RING_COUNT};

/// Profile schema version this library reads. Bump when keys are added (and update
/// `default.json`, the JSON Schema and `CHANGELOG.md`).
pub const PROFILE_VERSION: u32 = 7;

/// Default `terrain.tau_px` (Terrain v2 G2): calibrated so the default view (60 deg, 1080 px)
/// over the default relief (amplitude 450 m) holds about as many terrain tiles as the fixed
/// T0..T4 bands did (`scheduler::tests::terrain_cover_counts`).
pub const TERRAIN_TAU_PX_DEFAULT: f64 = 0.9;

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

/// Procedural terrain (profile_version 5). Heights come from a seeded function
/// (`terrain::field_height`), no data. The `bands` table, `skirt_m`, `shading`, `horizon_*` and
/// `max_fetch` / `max_mesh` belong to the terrain-mesh layer (api 1.5, `terrain::tin`, the
/// terrain `Layer` of the scheduler).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TerrainProfile {
    /// Terrain on. Ignored (with one warning) unless the host passes `layers: ["terrain", ...]`.
    pub enabled: bool,
    /// Seed of the height function (same seed and keys = same world).
    pub seed: u32,
    /// Relief scale (m): heights are about `amplitude_m * (0.5 + s)`, `s` mostly in [-1, 1].
    pub amplitude_m: f64,
    /// Size of the largest hills (m).
    pub feature_size_m: f64,
    /// Noise octaves (detail levels, each half the size of the previous).
    pub octaves: u32,
    /// Strength of ridged mountain chains (inside a low-frequency mountain mask).
    pub ridge_share: f64,
    /// Depth of the valleys cut along meandering lines (1 = down to the valley floor).
    pub valley_share: f64,
    /// Share of the land raised into flat-topped plateaus.
    pub plateau_share: f64,
    /// Nothing is lower than this (flat sea, lakes and river plains), m.
    pub sea_level_m: f64,
    /// Radius of the flat zone around the configure origin (m; 0 with blend 0 = none).
    pub flat_radius_m: f64,
    /// Width of the smooth blend from the flat zone into the relief (m).
    pub flat_blend_m: f64,
    /// Vertical scale of every height (terrain, building bases, lights, `ground_height_at`).
    pub exaggeration: f64,
    /// Added to every height (m).
    pub height_offset_m: f64,
    /// Water vertices take the minimum height within this radius (m) and water is not subdivided;
    /// 0 (default) = water drapes like every other area (plain height, subdivided). For terrain
    /// whose lakes are flat in the height function itself (a DEM); the procedural terrain has none.
    pub water_snap_m: f64,
    /// Max edge length of draped ground geometry where the surface is not flat (m).
    pub drape_step_m: f64,
    /// Upper bound of the runtime terrain detail scale (`set_terrain_detail`, since G2 a
    /// multiplier of `tau_px`: 2 = coarser cover).
    pub detail_scale_max: f64,
    /// Screen-space error threshold (px) of the terrain layer's cover (Terrain v2 G2): a terrain
    /// tile refines when its error proxy (`terrain::height::ErrorProxy`) would show more than
    /// `tau_px` x the `set_terrain_detail` scale pixels at its slant distance in the host's view
    /// (`set_view`), and coarsens again at that / `bands.hysteresis`. The terrain bands'
    /// `edge_m` no longer refine (the last one stays the layer's outer limit; every edge still
    /// sizes the ring's look-ahead clamp).
    pub tau_px: f64,
    /// Tile budget of the terrain cover (leaves; 0 = no cap; review M1): over it the plan
    /// skips the splits with the smallest screen error (the foundation guard's never, the 2:1
    /// balance may add a few on top).
    pub max_tiles: u32,
    /// Skirt depth of terrain tiles (m; 0 = automatic: `2 * max_error_m * detail + 1`, at most 60).
    pub skirt_m: f64,
    /// Terrain mesh shading (smooth adds a NORMAL array).
    pub shading: TerrainShading,
    /// Drop terrain tiles below the horizon of a real-size Earth (roots and whole sibling groups).
    pub horizon_cull: bool,
    /// Extra distance past the horizon before a terrain tile is culled (m).
    pub horizon_margin_m: f64,
    /// Terrain dispatch limit per band (the terrain has no fetch stage, so `max_mesh` bounds it).
    pub max_fetch: u32,
    /// Terrain mesh jobs in flight per band.
    pub max_mesh: u32,
    /// Finest zoom of the terrain layer (ring 0).
    pub near_zoom: u32,
    /// Coarsest zoom of the terrain layer (the roots).
    pub far_zoom: u32,
    /// One entry per terrain zoom, `near_zoom` first (`near_zoom - far_zoom + 1` entries).
    pub bands: Vec<TerrainBand>,
}

/// One terrain band (ring i = zoom `near_zoom - i`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TerrainBand {
    /// Outer slant-distance edge (m); strictly increasing over the array. Since api 1.10 (G2)
    /// only the last one acts on the cover (the terrain layer's outer limit); every edge still
    /// sizes its ring's look-ahead clamp. The terrain cover refines by `terrain.tau_px`.
    pub edge_m: f64,
    /// Max vertical error of the terrain mesh (m): the low-poly knob.
    pub max_error_m: f64,
    /// Seed grid spacing so the visual earth curvature bends flat tiles (m; 0 = corners only, else >= 500).
    pub max_edge_m: f64,
    /// Hard triangle cap per tile.
    pub max_triangles: u32,
    /// Cells per tile side of the height patch the TIN is built from (a power of two, 16..512);
    /// 0 = by zoom (z14 64, z13 128, coarser 256: about 22 m at z12..z14). Since perf review 2
    /// the coarse bands default to 128 (z12 44 m, z11 88 m, z10 176 m at 55 N): a quarter of the
    /// height evaluations, the drawn surface still within the vector drape lift above it there.
    pub patch_cells: u32,
}

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

/// Terrain mesh shading.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerrainShading {
    /// Normals from screen derivatives in the shader (no NORMAL array).
    Flat,
    /// Area-weighted vertex normals (a NORMAL array).
    Smooth,
}

impl Default for TerrainBand {
    fn default() -> Self {
        Self { edge_m: 2500.0, max_error_m: 0.5, max_edge_m: 0.0, max_triangles: 4096, patch_cells: 0 }
    }
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

// ---------------------------------------------------------------------------------------------
// Ranges (single source for validation; the JSON Schema must agree, see tests)

/// Allowed range per numeric key. `*` matches any one key (free-key maps and the r0..r2
/// objects); `[]` is every element of an array.
pub const RANGES: &[(&str, f64, f64)] = &[
    ("profile_version", 1.0, 1_000_000.0),
    ("bands.max_distance_m", 50.0, 100_000.0),
    ("bands.hysteresis", 1.01, 2.0),
    ("bands.look_ahead_s", 0.0, 120.0),
    ("bands.look_ahead_clamp", 0.0, 1.0),
    ("bands.default_tile_height_m", 0.0, 400.0),
    ("lod.tau_px", 0.5, 64.0),
    ("lod.max_tiles", 0.0, 1_000_000.0),
    ("lod.error_m.*", 0.01, 1000.0),
    ("lod.rear_tau_scale", 1.0, 16.0),
    ("lod.adaptive.frame_budget_ms", 1.0, 1000.0),
    ("lod.adaptive.min_scale", 0.05, 1.0),
    ("lod.adaptive.max_scale", 1.0, 16.0),
    ("lod.adaptive.smoothing_s", 0.0, 60.0),
    ("buildings.*.dp_m", 0.0, 50.0),
    ("buildings.*.min_area_m2", 0.0, 100_000.0),
    ("buildings.*.min_height_m", 0.0, 500.0),
    ("buildings.slender_tower.ratio", 0.5, 50.0),
    ("buildings.slender_tower.max_area_m2", 0.0, 100_000.0),
    ("buildings.min_window_wall_m", 0.0, 100.0),
    ("ground.r0.area_dp_m", 0.0, 50.0),
    ("ground.r1.area_dp_m", 0.0, 50.0),
    ("ground.r2.area_dp_m", 0.0, 50.0),
    ("ground.r3.area_dp_m", 0.0, 50.0),
    ("ground.r0.min_area_m2", 0.0, 1_000_000.0),
    ("ground.r1.min_area_m2", 0.0, 1_000_000.0),
    ("ground.r2.min_area_m2", 0.0, 1_000_000.0),
    ("ground.r3.min_area_m2", 0.0, 1_000_000.0),
    ("ground.r0.road_dp_m", 0.0, 50.0),
    ("ground.r1.road_dp_m", 0.0, 50.0),
    ("ground.r2.road_dp_m", 0.0, 50.0),
    ("ground.r3.road_dp_m", 0.0, 50.0),
    ("ground.r0.lift_step_m", 0.0, 0.5),
    ("ground.r1.lift_step_m", 0.0, 0.5),
    ("ground.r2.lift_step_m", 0.0, 0.5),
    ("ground.r3.lift_step_m", 0.0, 0.5),
    ("ground.roads.*.width_m", 0.5, 100.0),
    ("ground.roads.other_width_m", 0.5, 100.0),
    ("ground.roads.link_width_factor", 0.1, 1.0),
    ("ground.roads.link_min_width_m", 0.0, 100.0),
    ("ground.areas.*.level", 0.0, 7.0),
    ("ground.other_area_level", 0.0, 7.0),
    ("lights.cluster_on_agl_m", 0.0, 100_000.0),
    ("lights.cluster_off_agl_m", 0.0, 100_000.0),
    ("lights.cluster_cell_m", 1.0, 10_000.0),
    ("lights.min_ring", 0.0, 3.0),
    ("lights.street.min_ring", 0.0, 4.0),
    ("lights.street.color[]", 0.0, 1.0),
    ("lights.street.alt_color[]", 0.0, 1.0),
    ("lights.street.alt_share", 0.0, 1.0),
    ("lights.street.spacing_m", 5.0, 1000.0),
    ("lights.street.height_m", 0.0, 100.0),
    ("lights.street.untagged_max_class", 0.0, 6.0),
    ("lights.beacons.min_ring", 0.0, 4.0),
    ("lights.beacons.min_height_m", 1.0, 1000.0),
    ("lights.beacons.level_spacing_m", 0.0, 500.0),
    ("lights.beacons.corner_min_size_m", 0.0, 500.0),
    ("lights.beacons.color[]", 0.0, 1.0),
    ("lights.beacons.alt_color[]", 0.0, 1.0),
    ("lights.beacons.alt_share", 0.0, 1.0),
    ("lights.profiles.*.color[]", 0.0, 1.0),
    ("lights.profiles.*.windows.width_m", 0.5, 50.0),
    ("lights.profiles.*.windows.lit_share[]", 0.0, 1.0),
    ("lights.profiles.*.floodlight.strength", 0.0, 100.0),
    ("lights.profiles.*.floodlight.fade_height_m", 0.5, 1000.0),
    ("streaming.max_fetch_per_band", 1.0, 64.0),
    ("streaming.max_mesh_per_band", 1.0, 64.0),
    ("streaming.redeliveries_per_frame", 1.0, 64.0),
    ("streaming.worker_threads", 0.0, 64.0),
    ("streaming.retry_base_s", 0.05, 60.0),
    ("streaming.retry_max_s", 0.05, 3600.0),
    ("fades.cap_s", 0.1, 60.0),
    ("fades.max_active", 0.0, 256.0),
    ("origin.recenter_distance_m", 500.0, 50_000.0),
    ("cache.cap_mb", 1.0, 1_000_000.0),
    ("cache.connect_timeout_s", 0.1, 600.0),
    ("cache.request_timeout_s", 0.1, 3600.0),
    ("cache.open_retry_s", 0.0, 3600.0),
    ("cache.validate_retry_s", 1.0, 86_400.0),
    ("memory.budget_mb", 32.0, 65_536.0),
    ("memory.data_idle_s", 0.0, 3600.0),
    ("memory.recent_tiles_mb", 0.0, 65_536.0),
    ("memory.recent_tiles_s", 0.0, 600.0),
    ("terrain.seed", 0.0, 4_294_967_295.0),
    ("terrain.amplitude_m", 0.0, 3000.0),
    ("terrain.feature_size_m", 200.0, 200_000.0),
    ("terrain.octaves", 1.0, 10.0),
    ("terrain.ridge_share", 0.0, 1.0),
    ("terrain.valley_share", 0.0, 1.0),
    ("terrain.plateau_share", 0.0, 1.0),
    ("terrain.sea_level_m", -1000.0, 3000.0),
    ("terrain.flat_radius_m", 0.0, 100_000.0),
    ("terrain.flat_blend_m", 0.0, 100_000.0),
    ("terrain.exaggeration", 0.1, 5.0),
    ("terrain.height_offset_m", -500.0, 500.0),
    ("terrain.water_snap_m", 0.0, 200.0),
    ("terrain.drape_step_m", 5.0, 200.0),
    ("terrain.detail_scale_max", 1.0, 8.0),
    ("terrain.tau_px", 0.5, 64.0),
    ("terrain.max_tiles", 0.0, 1_000_000.0),
    ("terrain.skirt_m", 0.0, 200.0),
    ("terrain.horizon_margin_m", 0.0, 50_000.0),
    ("terrain.max_fetch", 1.0, 16.0),
    ("terrain.max_mesh", 1.0, 16.0),
    ("terrain.near_zoom", 8.0, 16.0),
    ("terrain.far_zoom", 8.0, 16.0),
    ("terrain.bands[].edge_m", 100.0, 500_000.0),
    ("terrain.bands[].max_error_m", 0.05, 200.0),
    ("terrain.bands[].max_edge_m", 0.0, 50_000.0),
    ("terrain.bands[].max_triangles", 2.0, 65_000.0),
    ("terrain.bands[].patch_cells", 0.0, 512.0),
    ("seed.aeroway_class", 6.0, 7.0),
    ("seed.water.blend_m", 50.0, 5000.0),
    ("seed.water.raster_n", 8.0, 256.0),
    ("seed.airport.shoulder_m", 0.0, 2000.0),
    ("seed.airport.strip_end_m", 0.0, 500.0),
    ("seed.airport.strip_half_m.*", 10.0, 500.0),
    ("seed.airport.fence_margin_m", 0.0, 2000.0),
    ("seed.airport.taxi_offset_m.*", 0.0, 1000.0),
    ("seed.airport.taxiway_width_m.*", 3.0, 100.0),
    ("seed.airport.runway_width_m.*", 5.0, 150.0),
    ("seed.airport.default_length_m", 100.0, 6000.0),
    ("seed.airport.max_slope", 0.0, 0.1),
    ("seed.airport.lights.height_m", 0.0, 10.0),
    ("seed.airport.lights.edge_spacing_m", 10.0, 500.0),
    ("seed.airport.lights.approach_length_m", 0.0, 3000.0),
    ("seed.airport.lights.approach_spacing_m", 5.0, 500.0),
    ("seed.airport.lights.approach_min_runway_m", 0.0, 6000.0),
    ("seed.airport.lights.taxiway_spacing_m", 10.0, 500.0),
    ("seed.airport.lights.intensity", 0.0, 1.0),
    ("seed.airport.lights.edge_color[]", 0.0, 1.0),
    ("seed.airport.lights.threshold_color[]", 0.0, 1.0),
    ("seed.airport.lights.end_color[]", 0.0, 1.0),
    ("seed.airport.lights.approach_color[]", 0.0, 1.0),
    ("seed.airport.lights.taxiway_color[]", 0.0, 1.0),
    ("seed.countryside.forest_share", 0.0, 1.0),
    ("seed.countryside.water_forest_share", 0.0, 1.0),
    ("seed.countryside.water_distance_m", 0.0, 5000.0),
    ("seed.countryside.meadow_share", 0.0, 1.0),
    ("seed.countryside.noise_cells", 1.0, 64.0),
    ("seed.countryside.tint_levels", 1.0, 8.0),
    ("seed.countryside.edge_warp", 0.0, 0.4),
];

/// Objects whose keys are free names (new entries allowed; entries are validated against an
/// existing entry of the defaults).
const FREE_KEY_MAPS: &[&str] = &["ground.areas", "buildings.overrides"];

/// The shape template for [`check_shape`]: the defaults plus one example entry in every free-key
/// map that is empty by default (so a new entry has a type to check against).
fn shape_template(defaults: &Value) -> Value {
    let mut t = defaults.clone();
    if let Some(Value::Object(o)) = t.pointer_mut("/buildings/overrides") {
        if o.is_empty() {
            o.insert("w0".into(), Value::from("residential"));
        }
    }
    t
}

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
#[derive(Clone, Debug, PartialEq)]
enum StateRule {
    /// `historic` / `historic=value`.
    Historic(Option<String>),
    /// `man_made` / `man_made=value`.
    ManMade(Option<String>),
    Ruins,
    Disused,
    Abandoned,
}

impl StateRule {
    fn parse(s: &str) -> Option<Self> {
        let (k, v) = match s.split_once('=') {
            Some((k, v)) => (k.trim(), Some(v.trim())),
            None => (s.trim(), None),
        };
        if v.is_some_and(str::is_empty) {
            return None;
        }
        let flag = |r: Self| match v {
            None | Some("yes") => Some(r),
            _ => None,
        };
        match k {
            "historic" => Some(Self::Historic(v.map(str::to_string))),
            "man_made" => Some(Self::ManMade(v.map(str::to_string))),
            "ruins" => flag(Self::Ruins),
            "disused" => flag(Self::Disused),
            "abandoned" => flag(Self::Abandoned),
            _ => None,
        }
    }
    fn matches(&self, t: &BuildingTags) -> bool {
        let text = |have: &Option<String>, want: &Option<String>| match (have, want) {
            (Some(h), Some(w)) => h == w,
            (Some(_), None) => true,
            (None, _) => false,
        };
        match self {
            Self::Historic(v) => text(&t.historic, v),
            Self::ManMade(v) => text(&t.man_made, v),
            Self::Ruins => t.ruins,
            Self::Disused => t.disused,
            Self::Abandoned => t.abandoned,
        }
    }
}

/// `n123` / `w123` / `r123` -> (OSM type tag, id).
fn parse_osm_ref(s: &str) -> Option<(u64, u64)> {
    let mut c = s.chars();
    let t = match c.next()? {
        'n' => OsmType::Node,
        'w' => OsmType::Way,
        'r' => OsmType::Relation,
        _ => return None,
    };
    let digits = c.as_str();
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((t.tag(), digits.parse().ok()?))
}

/// Hours checked by the lit-share rise warning: 18 h .. 23 h, 0 h .. 3 h.
const EVENING_HOURS: [usize; 10] = [18, 19, 20, 21, 22, 23, 0, 1, 2, 3];

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
        let lod = &world.lod;
        let errors = [lod.error_m.r1, lod.error_m.r2, lod.error_m.r3];
        for r in 1..errors.len() {
            if errors[r] <= errors[r - 1] {
                errs.push(format!(
                    "lod.error_m.r{} = {} must be greater than lod.error_m.r{} = {}",
                    r + 1,
                    fmt_num(errors[r]),
                    r,
                    fmt_num(errors[r - 1])
                ));
            }
        }
        let a = &lod.adaptive;
        if !(a.min_scale <= 1.0 && 1.0 <= a.max_scale) {
            errs.push(format!(
                "lod.adaptive.min_scale = {} <= 1 <= lod.adaptive.max_scale = {} does not hold",
                fmt_num(a.min_scale),
                fmt_num(a.max_scale)
            ));
        }
        let l = &world.lights;
        if l.cluster_off_agl_m > l.cluster_on_agl_m {
            errs.push(format!(
                "lights.cluster_off_agl_m = {} must not exceed lights.cluster_on_agl_m = {}",
                fmt_num(l.cluster_off_agl_m),
                fmt_num(l.cluster_on_agl_m)
            ));
        }
        let t = &world.terrain;
        if t.near_zoom < t.far_zoom {
            errs.push(format!("terrain.near_zoom = {} must not be below terrain.far_zoom = {}", t.near_zoom, t.far_zoom));
        } else if t.bands.len() != (t.near_zoom - t.far_zoom + 1) as usize {
            errs.push(format!(
                "terrain.bands has {} entries, needs {} (one per zoom from terrain.near_zoom = {} down to terrain.far_zoom = {})",
                t.bands.len(),
                t.near_zoom - t.far_zoom + 1,
                t.near_zoom,
                t.far_zoom
            ));
        }
        for (i, tb) in t.bands.iter().enumerate() {
            if i > 0 && tb.edge_m <= t.bands[i - 1].edge_m {
                errs.push(format!(
                    "terrain.bands[{i}].edge_m = {} must be greater than terrain.bands[{}].edge_m = {}",
                    fmt_num(tb.edge_m),
                    i - 1,
                    fmt_num(t.bands[i - 1].edge_m)
                ));
            }
            if tb.patch_cells != 0 && (tb.patch_cells < 16 || !tb.patch_cells.is_power_of_two()) {
                errs.push(format!("terrain.bands[{i}].patch_cells = {} must be 0 (by zoom) or a power of two from 16 to 512", tb.patch_cells));
            }
            if tb.max_edge_m != 0.0 && tb.max_edge_m < 500.0 {
                errs.push(format!(
                    "terrain.bands[{i}].max_edge_m = {} must be 0 (corners only) or at least 500",
                    fmt_num(tb.max_edge_m)
                ));
            }
        }
        let s = &world.streaming;
        if s.retry_max_s < s.retry_base_s {
            errs.push(format!(
                "streaming.retry_max_s = {} must not be below streaming.retry_base_s = {}",
                fmt_num(s.retry_max_s),
                fmt_num(s.retry_base_s)
            ));
        }

        // Light profiles: kind -> id (a kind in two profiles is ambiguous). One table for
        // `building`, `historic` and `man_made` values.
        let mut kind_profile = AHashMap::new();
        let lp = &l.profiles;
        let named = [
            ("residential", &lp.residential, PROFILE_RESIDENTIAL),
            ("office", &lp.office, PROFILE_OFFICE),
            ("retail", &lp.retail, PROFILE_RETAIL),
            ("industrial", &lp.industrial, PROFILE_INDUSTRIAL),
            ("civic", &lp.civic, PROFILE_CIVIC),
            ("landmark", &lp.landmark, PROFILE_LANDMARK),
            ("dark", &lp.dark, PROFILE_DARK),
        ];
        let mut profile_colors = [[1.0f32; 4]; PROFILE_COUNT];
        let mut window_width = [None; PROFILE_COUNT];
        let mut warnings = Vec::new();
        for (name, p, id) in named {
            profile_colors[id as usize] = [p.color[0] as f32, p.color[1] as f32, p.color[2] as f32, 1.0];
            for k in &p.kinds {
                if let Some(prev) = kind_profile.insert(k.clone(), id) {
                    if prev != id {
                        errs.push(format!("lights.profiles.{name}.kinds: \"{k}\" is already in another profile"));
                    }
                }
            }
            match (&p.windows, id) {
                (Some(_), PROFILE_INDUSTRIAL | PROFILE_LANDMARK | PROFILE_DARK) => errs.push(format!(
                    "lights.profiles.{name}.windows: the {name} profile has no windows (a `windows=yes` building borrows its landuse fallback profile's)"
                )),
                (Some(w), _) => {
                    window_width[id as usize] = Some(w.width_m as f32);
                    if w.lit_share.len() != 24 {
                        errs.push(format!(
                            "lights.profiles.{name}.windows.lit_share has {} values, needs 24 (one per local hour 0..23)",
                            w.lit_share.len()
                        ));
                    } else if let Some(i) =
                        (1..EVENING_HOURS.len()).find(|&i| w.lit_share[EVENING_HOURS[i]] > w.lit_share[EVENING_HOURS[i - 1]])
                    {
                        let (a, b) = (EVENING_HOURS[i - 1], EVENING_HOURS[i]);
                        warnings.push(format!(
                            "lights.profiles.{name}.windows.lit_share rises from {a} h ({}) to {b} h ({}): between 18 h and 3 h it should only fall, or windows switch on again while the evening goes on",
                            fmt_num(w.lit_share[a]),
                            fmt_num(w.lit_share[b])
                        ));
                    }
                }
                (None, _) => {}
            }
            match (&p.floodlight, id) {
                (None, PROFILE_LANDMARK) => errs.push("lights.profiles.landmark.floodlight is required".into()),
                (Some(_), i) if i != PROFILE_LANDMARK => {
                    errs.push(format!("lights.profiles.{name}.floodlight: only the landmark profile has a floodlight"))
                }
                _ => {}
            }
        }
        // Id 5 ("unknown") never reaches the output; give it residential's color.
        profile_colors[PROFILE_UNKNOWN as usize] = profile_colors[PROFILE_RESIDENTIAL as usize];

        let bw = &world.buildings;
        let mut overrides = AHashMap::new();
        for (k, v) in &bw.overrides {
            match parse_osm_ref(k) {
                Some(r) => {
                    overrides.insert(r, v.id());
                }
                None => errs.push(format!(
                    "buildings.overrides: \"{k}\" is not an OSM id (n, w or r followed by digits, e.g. w123456)"
                )),
            }
        }
        let mut dark_states = Vec::new();
        for d in &bw.dark_states {
            match StateRule::parse(d) {
                Some(r) => dark_states.push(r),
                None => errs.push(format!(
                    "buildings.dark_states: \"{d}\" is not `key` or `key=value` over historic, man_made, ruins, disused, abandoned (the flags take no value but `yes`)"
                )),
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

        // R2 walls never carry windows (a band rule, not a profile key).
        let lod_of = |b: &BuildingLod, windows: bool| LodParams { dp_m: b.dp_m, min_area_m2: b.min_area_m2, min_height_m: b.min_height_m, windows };
        let ground_of = |p: &GroundLod| GroundParams {
            area_dp_m: p.area_dp_m,
            min_area_m2: p.min_area_m2,
            road_dp_m: p.road_dp_m,
            max_road_class: p.max_road_class.id(),
            minor_paths: p.minor_paths,
            step_m: p.lift_step_m as f32,
        };
        Ok(Self {
            band_params: {
                let mut bp = BandParams {
                    edge_m: [b.max_distance_m; RING_COUNT],
                    error_m: errors,
                    tau_px: lod.tau_px,
                    max_distance_m: b.max_distance_m,
                    hysteresis: b.hysteresis,
                    look_ahead_s: b.look_ahead_s,
                    look_ahead_clamp: b.look_ahead_clamp,
                    default_tile_height_m: b.default_tile_height_m,
                    max_tiles: lod.max_tiles as f64,
                };
                bp.edge_m = bp.edges_for(&crate::rings::LodView::default(), 1.0);
                bp
            },
            building_lod: [Some(lod_of(&bw.r0, true)), Some(lod_of(&bw.r1, true)), Some(lod_of(&bw.r2, false)), None],
            ground_lod: [Some(ground_of(&g.r0)), Some(ground_of(&g.r1)), Some(ground_of(&g.r2)), Some(ground_of(&g.r3))],
            kind_profile,
            landuse_profile,
            profile_colors,
            window_width,
            overrides,
            dark_states,
            slender_ratio: bw.slender_tower.ratio as f32,
            slender_max_area_m2: bw.slender_tower.max_area_m2 as f32,
            min_window_wall_m: bw.min_window_wall_m as f32,
            warnings,
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
                    None => {
                        let hint = if p.starts_with("lights.profiles.") && (p.ends_with(".windows") || p.ends_with(".floodlight")) {
                            " (`windows` exists on residential, office, retail and civic only, `floodlight` on landmark only)"
                        } else {
                            ""
                        };
                        errs.push(format!("unknown key `{p}`{hint}"))
                    }
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

/// The fixed band edges removed in profile_version 7 (api 1.10, screen-space-error LOD).
const REMOVED_BAND_EDGES: [&str; 3] = ["r0_edge_m", "r1_edge_m", "r2_edge_m"];

/// profile_version 7 replaced `bands.r0..r2_edge_m` by the screen-space error (`lod`) and renamed
/// `bands.r3_edge_m` to `bands.max_distance_m`. A source declaring an older version migrates
/// (`r3_edge_m` -> `max_distance_m`, the other edges dropped with a warning); a current one (or
/// one without `profile_version`, e.g. overrides) that still names them is refused. A newer one
/// is left to the lenient shape check (unknown keys ignored with a warning).
fn migrate_band_edges(src: &mut Value, version: u64, errs: &mut Vec<String>, warns: &mut Vec<String>) {
    let Some(Value::Object(b)) = src.get_mut("bands") else { return };
    let old: Vec<String> =
        b.keys().filter(|k| REMOVED_BAND_EDGES.contains(&k.as_str()) || k.as_str() == "r3_edge_m").cloned().collect();
    if old.is_empty() || version > PROFILE_VERSION as u64 {
        return;
    }
    if version == PROFILE_VERSION as u64 {
        for k in old {
            let hint = if k == "r3_edge_m" {
                "renamed to `bands.max_distance_m`".to_string()
            } else {
                format!(
                    "removed in profile_version {PROFILE_VERSION}: the band edges come from the screen-space error, tune `lod.tau_px` / `lod.error_m` instead"
                )
            };
            errs.push(format!("`bands.{k}` is {hint}"));
        }
        return;
    }
    if let Some(v) = b.remove("r3_edge_m") {
        b.insert("max_distance_m".into(), v);
        warns.push(format!("`bands.r3_edge_m` migrated to `bands.max_distance_m` (profile_version {PROFILE_VERSION})"));
    }
    for k in REMOVED_BAND_EDGES {
        if b.remove(k).is_some() {
            warns.push(format!(
                "`bands.{k}` dropped: since profile_version {PROFILE_VERSION} the band edges come from the screen-space error (`lod.tau_px`, `lod.error_m`)"
            ));
        }
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
    migrate_band_edges(&mut src, version, &mut errs, &mut warns);
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
    let template = shape_template(&defaults);
    let mut acc = defaults;
    if let Some((label, text)) = file {
        let v = parse_json(text, label)?;
        apply_source(&mut acc, &template, v, label, log)?;
    }
    if let Some(o) = overrides {
        apply_source(&mut acc, &template, o.clone(), "profile_overrides", log)?;
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
    let p = Profile::new(world).map_err(|e| format!("profile: {e}"))?;
    for w in p.warnings() {
        log.warn(&format!("WorldStreamer: profile: {w}"));
    }
    Ok(p)
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
        // api 1.10: the SSE edges at the default view (1080 px, 60 deg, tau 4) are the spike's
        // fixed edges, bit for bit.
        assert_eq!(b.edge_m, [600.0, 2500.0, 7000.0, 25_000.0]);
        assert_eq!(b.edge_m[3], 25_000.0);
        assert_eq!((b.hysteresis, b.look_ahead_s, b.look_ahead_clamp, b.default_tile_height_m), (1.15, 20.0, 0.5, 60.0));
        assert_eq!(p.lod(0), Some(LodParams { dp_m: 0.5, min_area_m2: 0.0, min_height_m: 0.0, windows: true }));
        assert_eq!(p.lod(1), Some(LodParams { dp_m: 2.0, min_area_m2: 20.0, min_height_m: 0.0, windows: true }));
        assert_eq!(p.lod(2), Some(LodParams { dp_m: 4.0, min_area_m2: 50.0, min_height_m: 0.0, windows: false }));
        assert_eq!(p.lod(3), None);
        let gp = |a, m, r, c, minor, s| Some(GroundParams { area_dp_m: a, min_area_m2: m, road_dp_m: r, max_road_class: c, minor_paths: minor, step_m: s });
        assert_eq!(p.ground_params(0), gp(0.5, 0.0, 0.5, ROAD_OTHER, true, 0.02f32));
        assert_eq!(p.ground_params(1), gp(2.0, 200.0, 2.0, ROAD_OTHER, false, 0.06f32));
        assert_eq!(p.ground_params(2), gp(6.0, 2500.0, 5.0, ROAD_RESIDENTIAL, false, 0.15f32));
        assert_eq!(p.ground_params(3), gp(20.0, 40000.0, 15.0, ROAD_SECONDARY, false, 0.5f32));
        assert_eq!(p.profile_color(PROFILE_RESIDENTIAL), [1.0, 0.655, 0.341, 1.0]);
        assert_eq!(p.profile_color(PROFILE_OFFICE), [0.86, 0.9, 1.0, 1.0]);
        assert_eq!(p.profile_color(PROFILE_RETAIL), [1.0, 0.98, 0.95, 1.0]);
        assert_eq!(p.profile_color(PROFILE_INDUSTRIAL), [1.0, 0.55, 0.15, 1.0]);
        assert_eq!(p.profile_color(PROFILE_CIVIC), [1.0, 0.706, 0.42, 1.0]);
        assert_eq!(p.profile_color(crate::mesher::PROFILE_UNKNOWN), [1.0, 0.655, 0.341, 1.0]);
        assert_eq!(p.cluster_cell_m(), 100.0f32);
        let w = p.world();
        assert_eq!((w.lights.cluster_on_agl_m, w.lights.cluster_off_agl_m), (3300.0, 2700.0));
        assert_eq!((w.streaming.max_fetch_per_band, w.streaming.max_mesh_per_band, w.streaming.redeliveries_per_frame), (4, 8, 1));
        assert_eq!(w.fades.cap_s, 0.75);
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

    /// profile_version 4 (docs/spec-night-windows.md section 4): seven profiles, windows on
    /// residential / office / retail / civic, floodlight on landmark, kind moves, resolution keys.
    #[test]
    fn night_window_profiles_load_with_their_defaults() {
        let p = Profile::default();
        let lp = &p.lights.profiles;
        // Seven named profiles, ids 0-4, 6, 7.
        let ids: Vec<u8> = [
            LightProfileName::Residential,
            LightProfileName::Office,
            LightProfileName::Retail,
            LightProfileName::Industrial,
            LightProfileName::Civic,
            LightProfileName::Landmark,
            LightProfileName::Dark,
        ]
        .map(LightProfileName::id)
        .to_vec();
        assert_eq!(ids, vec![0, 1, 2, 3, 4, 6, 7]);
        assert_eq!(p.profile_color(PROFILE_LANDMARK), [1.0, 0.75, 0.45, 1.0]);
        assert_eq!(p.profile_color(PROFILE_DARK), [0.0, 0.0, 0.0, 1.0]);
        // Windows: widths, 24 hourly shares, none on industrial / landmark / dark.
        let widths: Vec<Option<f32>> = (0..PROFILE_COUNT as u8).map(|i| p.window_width_m(i)).collect();
        assert_eq!(widths, vec![Some(3.0), Some(2.0), Some(4.0), None, Some(3.5), None, None, None]);
        for w in [&lp.residential, &lp.office, &lp.retail, &lp.civic] {
            let w = w.windows.as_ref().unwrap();
            assert_eq!(w.lit_share.len(), 24);
            assert!(w.lit_share.iter().all(|x| (0.0..=1.0).contains(x)));
        }
        assert_eq!(lp.residential.windows.as_ref().unwrap().lit_share[19], 0.45);
        let office = &lp.office.windows.as_ref().unwrap().lit_share;
        assert_eq!((office[0], office[7], office[12], office[21], office[23]), (0.05, 0.375, 0.70, 0.15, 0.10));
        let retail = &lp.retail.windows.as_ref().unwrap().lit_share;
        assert_eq!((retail[3], retail[8], retail[15], retail[21], retail[22]), (0.10, 0.5, 0.90, 0.5, 0.10));
        let civic = &lp.civic.windows.as_ref().unwrap().lit_share;
        assert_eq!((civic[0], civic[7], civic[10], civic[20], civic[23]), (0.03, 0.315, 0.60, 0.10, 0.065));
        assert_eq!(lp.landmark.floodlight, Some(Floodlight { strength: 1.2, fade_height_m: 25.0 }));
        assert!(p.warnings().is_empty(), "{:?}", p.warnings());
        // Kind moves (not copies) out of civic and industrial.
        for k in ["church", "cathedral", "chapel", "mosque", "synagogue", "temple", "monastery", "shrine", "castle", "tower"] {
            assert_eq!(p.profile_for_kind(k), Some(PROFILE_LANDMARK), "{k}");
        }
        for k in ["garage", "garages", "shed", "barn", "service", "transformer_tower", "greenhouse", "storage_tank", "ruins", "chimney"] {
            assert_eq!(p.profile_for_kind(k), Some(PROFILE_DARK), "{k}");
        }
        assert_eq!(p.profile_for_kind("hangar"), Some(PROFILE_INDUSTRIAL));
        assert_eq!(p.profile_for_kind("religious"), Some(PROFILE_CIVIC));
        // Resolution keys.
        assert_eq!(p.min_window_wall_m(), 2.0);
        assert!(p.is_slender_tower(42.0, 49.0) && !p.is_slender_tower(20.0, 49.0) && !p.is_slender_tower(400.0, 150.0));
        assert!(!p.is_slender_tower(42.0, 0.0));
        let tags = |f: fn(&mut BuildingTags)| {
            let mut t = BuildingTags::default();
            f(&mut t);
            t
        };
        assert!(p.is_dark_state(&tags(|t| t.historic = Some("ruins".into()))));
        assert!(!p.is_dark_state(&tags(|t| t.historic = Some("castle".into()))));
        assert!(p.is_dark_state(&tags(|t| t.ruins = true)));
        assert!(p.is_dark_state(&tags(|t| t.disused = true)));
        assert!(p.is_dark_state(&tags(|t| t.abandoned = true)));
        assert!(!p.is_dark_state(&BuildingTags::default()));
        assert_eq!(p.override_profile(OsmType::Way, 1), None);
        // Overrides by OSM id, through JSON (Godot passes them in profile_overrides).
        let o = serde_json::json!({ "buildings": { "overrides": { "w123456": "landmark", "r7": "dark", "n5": "office" },
                                                   "dark_states": ["man_made=chimney", "historic"] } });
        let p = resolve(None, Some(&o), &NullLog).unwrap();
        assert_eq!(p.override_profile(OsmType::Way, 123_456), Some(PROFILE_LANDMARK));
        assert_eq!(p.override_profile(OsmType::Relation, 7), Some(PROFILE_DARK));
        assert_eq!(p.override_profile(OsmType::Node, 5), Some(PROFILE_OFFICE));
        assert_eq!(p.override_profile(OsmType::Relation, 123_456), None, "type is part of the id");
        assert!(p.is_dark_state(&tags(|t| t.man_made = Some("chimney".into()))));
        assert!(!p.is_dark_state(&tags(|t| t.man_made = Some("tower".into()))));
        assert!(p.is_dark_state(&tags(|t| t.historic = Some("castle".into()))), "bare `historic` = any value");
        assert!(!p.is_dark_state(&tags(|t| t.ruins = true)), "the list replaces the defaults");
        // A version 3 file (no new keys) loads with the new defaults.
        let v3 = r#"{ "profile_version": 3, "lights": { "min_ring": 1 } }"#;
        let p = resolve(Some(("v3", v3)), None, &NullLog).unwrap();
        assert_eq!(p.lights.profiles.landmark, Profile::default().lights.profiles.landmark);
        assert_eq!(p.buildings.slender_tower, SlenderTower { ratio: 3.0, max_area_m2: 150.0 });
        // Partial windows override merges deeply (lit_share kept).
        let p = resolve(None, Some(&serde_json::json!({"lights": {"profiles": {"office": {"windows": {"width_m": 2.5}}}}})), &NullLog)
            .unwrap();
        assert_eq!(p.window_width_m(PROFILE_OFFICE), Some(2.5));
        assert_eq!(p.lights.profiles.office.windows.as_ref().unwrap().lit_share.len(), 24);
    }

    #[test]
    fn night_window_profile_validation() {
        let err = |o: serde_json::Value| resolve(None, Some(&o), &NullLog).unwrap_err();
        // lit_share length.
        let e = err(serde_json::json!({"lights": {"profiles": {"retail": {"windows": {"lit_share": [0.5, 0.4]}}}}}));
        assert!(e.contains("lights.profiles.retail.windows.lit_share has 2 values, needs 24"), "{e}");
        // lit_share range.
        let mut share = vec![0.1; 24];
        share[5] = 1.5;
        let e = err(serde_json::json!({"lights": {"profiles": {"retail": {"windows": {"lit_share": share}}}}}));
        assert!(e.contains("lights.profiles.retail.windows.lit_share = 1.5 is out of range [0, 1]"), "{e}");
        // A kind in two profiles (also across the new ones).
        let e = err(serde_json::json!({"lights": {"profiles": {"civic": {"kinds": ["school", "church"]}}}}));
        assert!(e.contains("\"church\" is already in another profile"), "{e}");
        let e = err(serde_json::json!({"lights": {"profiles": {"dark": {"kinds": ["castle"]}}}}));
        assert!(e.contains("\"castle\" is already in another profile"), "{e}");
        // Bad override ids.
        for bad in ["123", "x123", "w", "w12a", "W123", "w-1"] {
            let e = err(serde_json::json!({"buildings": {"overrides": {bad: "landmark"}}}));
            assert!(e.contains(&format!("buildings.overrides: \"{bad}\" is not an OSM id")), "{bad}: {e}");
        }
        let e = err(serde_json::json!({"buildings": {"overrides": {"w1": "castle"}}}));
        assert!(e.contains("castle"), "{e}");
        // dark_states over the exported state attributes only.
        for bad in ["building=ruins", "ruins=no", "historic=", "windows"] {
            let e = err(serde_json::json!({"buildings": {"dark_states": [bad]}}));
            assert!(e.contains(&format!("buildings.dark_states: \"{bad}\"")), "{bad}: {e}");
        }
        // `windows` on dark / landmark / industrial and `floodlight` elsewhere: JSON shape...
        let win = serde_json::json!({"width_m": 3.0, "lit_share": vec![0.1; 24]});
        for name in ["dark", "landmark", "industrial"] {
            let e = err(serde_json::json!({"lights": {"profiles": {name: {"windows": win.clone()}}}}));
            assert!(e.contains(&format!("unknown key `lights.profiles.{name}.windows`")) && e.contains("civic only"), "{e}");
        }
        let e = err(serde_json::json!({"lights": {"profiles": {"civic": {"floodlight": {"strength": 1.0, "fade_height_m": 9.0}}}}}));
        assert!(e.contains("unknown key `lights.profiles.civic.floodlight`"), "{e}");
        // ... and the typed rules (Rust hosts building a WorldProfile directly).
        let mut w = WorldProfile::default();
        w.lights.profiles.dark.windows = w.lights.profiles.residential.windows.clone();
        assert!(Profile::new(w).unwrap_err().contains("lights.profiles.dark.windows: the dark profile has no windows"));
        let mut w = WorldProfile::default();
        w.lights.profiles.landmark.floodlight = None;
        w.lights.profiles.office.floodlight = Some(Floodlight { strength: 1.0, fade_height_m: 5.0 });
        let e = Profile::new(w).unwrap_err();
        assert!(e.contains("lights.profiles.landmark.floodlight is required"), "{e}");
        assert!(e.contains("lights.profiles.office.floodlight: only the landmark profile has a floodlight"), "{e}");
        // Ranges of the new numbers.
        let e = err(serde_json::json!({"buildings": {"slender_tower": {"ratio": 0.1}}}));
        assert!(e.contains("buildings.slender_tower.ratio = 0.1 is out of range"), "{e}");
        let e = err(serde_json::json!({"lights": {"profiles": {"landmark": {"floodlight": {"fade_height_m": 0}}}}}));
        assert!(e.contains("lights.profiles.landmark.floodlight.fade_height_m = 0 is out of range"), "{e}");
    }

    /// A `lit_share` that rises between 18 h and 3 h loads, with one warning naming the hours.
    #[test]
    fn lit_share_rising_in_the_evening_warns() {
        let mut share = Profile::default().lights.profiles.office.windows.as_ref().unwrap().lit_share.clone();
        share[1] = 0.3; // 0 h 0.05 -> 1 h 0.3
        let log = CaptureLog::default();
        let p = resolve(None, Some(&serde_json::json!({"lights": {"profiles": {"office": {"windows": {"lit_share": share}}}}})), &log)
            .unwrap();
        assert_eq!(p.warnings().len(), 1);
        let w = log.0.lock().unwrap();
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("lights.profiles.office.windows.lit_share rises from 0 h (0.05) to 1 h (0.3)"), "{}", w[0]);
        // Rising in the morning (3 h .. 18 h) is fine; so is the 23 h -> 0 h wrap when it falls.
        let log = CaptureLog::default();
        resolve(None, Some(&serde_json::json!({"lights": {"profiles": {"office": {"windows": {"width_m": 2.0}}}}})), &log).unwrap();
        assert!(log.0.lock().unwrap().is_empty());
        let mut share = vec![0.1; 24];
        share[23] = 0.05;
        share[0] = 0.2; // 23 h -> 0 h rises
        let p = resolve(None, Some(&serde_json::json!({"lights": {"profiles": {"civic": {"windows": {"lit_share": share}}}}})), &NullLog)
            .unwrap();
        assert!(p.warnings()[0].contains("rises from 23 h"), "{:?}", p.warnings());
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
        // default.json keys with free-map entries generalized to `*`.
        let d = parse_json(DEFAULT_JSON, "d").unwrap();
        // A free-key map that is empty in default.json (`buildings.overrides`) documents its
        // entries (`map.*`) in the schema only.
        let empty_maps: Vec<String> = FREE_KEY_MAPS
            .iter()
            .filter(|m| d.pointer(&format!("/{}", m.replace('.', "/"))).and_then(Value::as_object).is_some_and(|o| o.is_empty()))
            .map(|m| format!("{m}.*"))
            .collect();
        let mut schema_keys: Vec<String> = nodes
            .iter()
            .map(|(p, _)| p.clone())
            // Keys inside array items (`terrain.bands[].edge_m`) are not object paths of default.json
            // (arrays are leaves there); their ranges are still checked below.
            .filter(|p| !p.contains("[]") && !empty_maps.iter().any(|m| p == m || p.starts_with(&format!("{m}."))))
            .collect();
        schema_keys.sort();
        schema_keys.dedup();
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
        let file = r#"{ "profile_version": 7, "lod": { "error_m": { "r1": 2.0 } }, "lights": { "profiles": { "office": { "color": [0.5, 0.6, 0.7] } } } }"#;
        let ov = serde_json::json!({ "bands": { "hysteresis": 1.2 }, "streaming": { "max_fetch_per_band": 6.0 },
            "ground": { "areas": { "beach": { "class": "other", "level": 1, "kinds": ["beach"] } } } });
        let p = resolve(Some(("f.json", file)), Some(&ov), &NullLog).unwrap();
        let d = WorldProfile::default();
        assert_eq!(p.lod.error_m.r1, 2.0);
        assert_eq!(p.lod.error_m.r2, d.lod.error_m.r2);
        assert_eq!(p.bands.hysteresis, 1.2);
        assert_eq!(p.streaming.max_fetch_per_band, 6, "integral floats (Godot) are accepted as integers");
        assert_eq!(p.lights.profiles.office.color, [0.5, 0.6, 0.7]);
        assert_eq!(p.lights.profiles.office.kinds, d.lights.profiles.office.kinds, "kinds kept");
        assert_eq!(p.ground_area_class("beach"), (GROUND_OTHER, 1));
        assert_eq!(p.ground.areas.len(), d.ground.areas.len() + 1);
        // Overrides win over the file.
        let p = resolve(Some(("f.json", file)), Some(&serde_json::json!({"lod": {"error_m": {"r1": 1.5}}})), &NullLog).unwrap();
        assert_eq!(p.lod.error_m.r1, 1.5);
        // Empty sources = defaults.
        assert_eq!(resolve(Some(("e", "{}")), Some(&empty_object()), &NullLog).unwrap(), Profile::default());
    }

    /// profile_version 2 added `buildings.r0..r2.min_height_m` (default 0 = keep all) and
    /// version 3 added `lights.min_ring` (default 2, version 4: 1); a version 1 file without them still loads
    /// strictly and keeps the defaults.
    #[test]
    fn version_1_files_load_with_the_version_2_defaults() {
        assert_eq!(PROFILE_VERSION, 7);
        let v1 = r#"{ "profile_version": 1, "buildings": { "r1": { "dp_m": 3.0, "min_area_m2": 25.0 } } }"#;
        let p = resolve(Some(("v1", v1)), None, &NullLog).unwrap();
        assert_eq!(p.profile_version, PROFILE_VERSION);
        assert_eq!(p.lod(1), Some(LodParams { dp_m: 3.0, min_area_m2: 25.0, min_height_m: 0.0, windows: true }));
        for r in 0..3 {
            assert_eq!(p.lod(r).unwrap().min_height_m, 0.0);
        }
        // A version 1 file is still strict.
        assert!(resolve(Some(("v1", r#"{ "profile_version": 1, "buildings": { "r1": { "min_heigth_m": 6 } } }"#)), None, &NullLog).is_err());
        // The new key per band, and its range.
        let o = serde_json::json!({ "buildings": { "r1": { "min_height_m": 6.0 }, "r2": { "min_height_m": 12.0 } } });
        let p = resolve(None, Some(&o), &NullLog).unwrap();
        assert_eq!([0, 1, 2].map(|r| p.lod(r).unwrap().min_height_m), [0.0, 6.0, 12.0]);
        assert_eq!(range_of("buildings.r0.min_height_m"), Some((0.0, 500.0)));
        let bad = serde_json::json!({ "buildings": { "r0": { "min_height_m": 500.5 } } });
        let e = resolve(None, Some(&bad), &NullLog).unwrap_err();
        assert!(e.contains("buildings.r0.min_height_m"), "{e}");
        // Fade safety cap default (profile_version 2 era).
        assert_eq!(Profile::default().fades.cap_s, 0.75);
    }

    /// profile_version 7 adds `memory.budget_mb` (default 512 MiB, one number for every
    /// platform); a version 6 file still loads strictly with the default.
    /// profile_version 7 (api 1.10, Tiles and LOD v2 T3a): `lod.*` loads, the default view
    /// reproduces the old edges, ranges and cross-key rules hold.
    #[test]
    fn version_7_lod_keys_load_and_validate() {
        let p = Profile::default();
        assert_eq!(p.lod.tau_px, 4.0);
        assert_eq!((p.lod.error_m.r1, p.lod.error_m.r2, p.lod.error_m.r3), (2.5660011963983367, 10.691671651659735, 29.93668062464726));
        assert_eq!(p.lod.rear_tau_scale, 2.0);
        assert_eq!(p.lod.adaptive, LodAdaptive { enabled: true, frame_budget_ms: 33.3, min_scale: 0.5, max_scale: 3.0, smoothing_s: 2.0 });
        assert_eq!(p.bands.max_distance_m, 25_000.0);
        let ov = serde_json::json!({ "lod": { "tau_px": 8.0, "error_m": { "r1": 3.0, "r2": 12.0, "r3": 40.0 }, "rear_tau_scale": 3.0,
            "adaptive": { "enabled": false, "frame_budget_ms": 16.7, "min_scale": 0.8, "max_scale": 2.0, "smoothing_s": 1.0 } },
            "bands": { "max_distance_m": 20000.0 } });
        let q = resolve(None, Some(&ov), &NullLog).unwrap();
        assert_eq!(q.lod.tau_px, 8.0);
        assert!(!q.lod.adaptive.enabled);
        assert_eq!(q.bands().error_m, [3.0, 12.0, 40.0]);
        assert_eq!(q.bands().edge_m[3], 20000.0);
        let bad = |v: serde_json::Value| resolve(None, Some(&v), &NullLog).unwrap_err();
        assert!(bad(serde_json::json!({"lod": {"tau_px": 0.1}})).contains("lod.tau_px = 0.1 is out of range [0.5, 64]"));
        assert!(bad(serde_json::json!({"lod": {"error_m": {"r3": 10.0}}})).contains("lod.error_m.r3 = 10 must be greater than lod.error_m.r2"));
        let e = resolve(None, Some(&serde_json::json!({"lod": {"adaptive": {"max_scale": 1.0}}})), &NullLog);
        assert!(e.is_ok(), "max_scale 1 is allowed (min <= 1 <= max)");
        assert!(bad(serde_json::json!({"lod": {"adaptive": {"max_scale": 0.9}}})).contains("lod.adaptive.max_scale"));
        assert!(bad(serde_json::json!({"lod": {"hysteresis": 0.15}})).contains("unknown key `lod.hysteresis`"));
    }

    /// The fixed band edges are gone in profile_version 7: a current-version source naming them
    /// is refused with a message saying what replaced them; an older file migrates
    /// (`r3_edge_m` -> `max_distance_m`, r0..r2 dropped with a warning through the log).
    #[test]
    fn removed_band_edges_refused_in_v7_and_migrated_from_older() {
        for src in [r#"{ "profile_version": 7, "bands": { "r0_edge_m": 500 } }"#, r#"{ "bands": { "r1_edge_m": 2000 } }"#] {
            let e = resolve(Some(("p.json", src)), None, &NullLog).unwrap_err();
            assert!(e.contains("p.json") && e.contains("_edge_m` is removed in profile_version 7") && e.contains("lod.tau_px"), "{e}");
        }
        let e = resolve(Some(("p.json", r#"{ "profile_version": 7, "bands": { "r3_edge_m": 20000 } }"#)), None, &NullLog).unwrap_err();
        assert!(e.contains("`bands.r3_edge_m` is renamed to `bands.max_distance_m`"), "{e}");
        let e = resolve(None, Some(&serde_json::json!({"bands": {"r2_edge_m": 6000}})), &NullLog).unwrap_err();
        assert!(e.contains("profile_overrides") && e.contains("bands.r2_edge_m"), "{e}");
        let v6 = r#"{ "profile_version": 6, "bands": { "r0_edge_m": 450, "r1_edge_m": 2000, "r2_edge_m": 6000, "r3_edge_m": 20000, "hysteresis": 1.2 } }"#;
        let log = CaptureLog::default();
        let p = resolve(Some(("v6.json", v6)), None, &log).unwrap();
        assert_eq!(p.bands.max_distance_m, 20000.0);
        assert_eq!(p.bands.hysteresis, 1.2);
        assert_eq!(p.lod, Lod::default(), "the dropped edges do not move the SSE keys");
        let w = log.0.lock().unwrap();
        assert_eq!(w.len(), 4, "{w:?}");
        assert!(w.iter().any(|m| m.contains("bands.r3_edge_m` migrated to `bands.max_distance_m`")), "{w:?}");
        for k in ["r0_edge_m", "r1_edge_m", "r2_edge_m"] {
            assert!(w.iter().any(|m| m.contains(&format!("`bands.{k}` dropped")) && m.contains("v6.json")), "{w:?}");
        }
    }

    #[test]
    fn version_7_adds_the_memory_budget() {
        let v6 = r#"{ "profile_version": 6, "fades": { "max_active": 2 } }"#;
        let p = resolve(Some(("v6", v6)), None, &NullLog).unwrap();
        assert_eq!(p.memory.budget_mb, 512.0);
        assert!(resolve(Some(("v6", r#"{ "profile_version": 6, "memory": { "budget_gb": 1 } }"#)), None, &NullLog).is_err());
        let p = resolve(None, Some(&serde_json::json!({ "memory": { "budget_mb": 256 } })), &NullLog).unwrap();
        assert_eq!(p.memory.budget_mb, 256.0);
        let e = resolve(None, Some(&serde_json::json!({ "memory": { "budget_mb": 8 } })), &NullLog).unwrap_err();
        assert!(e.contains("memory.budget_mb"), "{e}");
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
        let newer = r#"{ "profile_version": 99, "bands": { "max_distance_m": 20000, "r4_edge_m": 90000 }, "weather": {} }"#;
        let log = CaptureLog::default();
        let p = resolve(Some(("new.json", newer)), None, &log).unwrap();
        assert_eq!(p.bands.max_distance_m, 20000.0);
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
        let e = resolve(None, Some(&serde_json::json!({"bands": {"max_distance_m": "far"}})), &NullLog).unwrap_err();
        assert!(e.contains("bands.max_distance_m must be a number, got a string"), "{e}");
        let e = resolve(None, Some(&serde_json::json!({"lod": {"error_m": {"r2": 2.0}}})), &NullLog).unwrap_err();
        assert!(e.contains("lod.error_m.r2 = 2 must be greater than lod.error_m.r1 = 2.566"), "{e}");
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
