//! The city keys of the profile's `seed` section (`docs/spec-world-seed.md` sections 5.1 step 3,
//! 5.3, 6.2, 9.2): `seed.city` (city plan), `seed.styles.<style>` (district street and building
//! styles) and `seed.airport.approach` (the approach surfaces that cap building heights).

use serde::{Deserialize, Serialize};

/// City plans (section 5.3): arterials, radials, placement and the plan cache.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SeedCity {
    /// Share of the non-tree arterial candidates dropped (T and X junctions instead of a
    /// honeycomb); the spanning tree of the candidates is always kept.
    pub arterial_drop: f64,
    /// Arterial midpoints move across the hex edge by up to this share of the cell size.
    pub edge_warp: f64,
    /// Neighbouring cells of one district merge (no arterial between them) up to this area (m²).
    pub max_district_m2: f64,
    /// At most one arterial bridge per this distance (km); other water crossings end at the shore.
    pub bridge_spacing_km: f64,
    /// Arterial and radial water crossings longer than this are not bridged (m).
    pub bridge_max_m: f64,
    /// Placement score penalty (rings) per share of a cell inside an approach surface, for
    /// districts taller than `approach_tall_m`.
    pub approach_penalty: f64,
    /// Districts whose `max_height_m` exceeds this avoid approach surfaces (m).
    pub approach_tall_m: f64,
    /// Radial step length (m).
    pub radial_step_m: f64,
    /// Radial heading random walk per step (degrees, either way).
    pub radial_turn_deg: f64,
    /// A radial to a linked airport ends after this many km outside the city.
    pub access_max_km: f64,
    /// Buildings keep this distance from the edge of an arterial or radial (m).
    pub clearance_m: f64,
    /// Storey height: levels = height / floor_m (m).
    pub floor_m: f64,
    /// Height range of an old town's landmark buildings (`church`, `tower`), m.
    pub landmark_height_m: [f64; 2],
    /// District (cell) plans kept in memory; a plan beyond it is rebuilt on demand.
    pub plan_cache: u32,
}

impl Default for SeedCity {
    fn default() -> Self {
        Self {
            arterial_drop: 0.25,
            edge_warp: 0.12,
            max_district_m2: 1_200_000.0,
            bridge_spacing_km: 2.0,
            bridge_max_m: 600.0,
            approach_penalty: 4.0,
            approach_tall_m: 30.0,
            radial_step_m: 100.0,
            radial_turn_deg: 4.0,
            access_max_km: 30.0,
            clearance_m: 2.0,
            floor_m: 3.0,
            landmark_height_m: [30.0, 55.0],
            plan_cache: 1024,
        }
    }
}

/// One approach surface class (ICAO Annex 14, first section, simplified).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SeedApproachClass {
    /// Widening per side per metre of distance from the inner edge.
    pub divergence: f64,
    /// Length of the surface from its inner edge (m).
    pub length_m: f64,
    /// Rise per metre of distance from the inner edge.
    pub slope: f64,
}

impl Default for SeedApproachClass {
    fn default() -> Self {
        SeedApproach::default().long
    }
}

/// Approach surfaces (section 5.1 step 3): buildings inside are capped at the surface.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SeedApproach {
    /// Paved runways at least `long_min_runway_m` long.
    pub long: SeedApproachClass,
    /// Shorter or unpaved runways.
    pub short: SeedApproachClass,
    pub long_min_runway_m: f64,
    /// A capped building lower than this is dropped (m).
    pub min_building_m: f64,
}

impl Default for SeedApproach {
    fn default() -> Self {
        Self {
            long: SeedApproachClass { divergence: 0.15, length_m: 3000.0, slope: 0.02 },
            short: SeedApproachClass { divergence: 0.10, length_m: 1600.0, slope: 0.05 },
            long_min_runway_m: 1200.0,
            min_building_m: 3.0,
        }
    }
}

/// One district style (section 6.2): a warped street lattice, blocks split into lots, one
/// building per kept lot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SeedStyle {
    /// Street spacing range (m); each lattice line takes a value in it by hash.
    pub spacing_m: [f64; 2],
    /// Lattice points move up to this share of the spacing (irregular blocks).
    pub jitter: f64,
    /// Amplitude of the smooth street warp (m).
    pub warp_m: f64,
    /// OSM `highway` value of the local streets (resolved through `ground.roads`).
    pub street: String,
    /// `highway` value of a loop inside every block (`""` = none; microdistrict service roads).
    pub loop_street: String,
    /// Share of the local streets with street lamps (a district's `lit` overrides it).
    pub street_lit: f64,
    /// Share of the lattice street segments left out (merged blocks, dead ends).
    pub street_drop: f64,
    /// Lot frontage range (m).
    pub frontage_m: [f64; 2],
    /// Building depth range from the lot front (m).
    pub depth_m: [f64; 2],
    /// Building distance from the street edge (m).
    pub setback_m: f64,
    /// Gap between buildings of neighbouring lots (m; 0 = a closed street front).
    pub side_gap_m: f64,
    /// Storey range of the buildings.
    pub floors: [f64; 2],
    /// Storeys = min + (max - min) * u^height_exp (u uniform): 1 even, above 1 few tall.
    pub height_exp: f64,
    /// Share of the lots that get a building (scaled by the district density / 0.6).
    pub keep: f64,
    /// Share of the buildings that are L-shaped.
    pub l_share: f64,
    /// OSM `building` kinds, chosen by hash (resolved through the light profiles).
    pub kinds: Vec<String>,
    /// OSM `landuse` kind of the district's ground area (resolved through `ground.areas`).
    pub landuse: String,
    /// Default height cap of a district of this style (m), when neither it nor the city sets one.
    pub max_height_m: f64,
    /// Default density (built share; scales `keep`).
    pub density: f64,
}

impl Default for SeedStyle {
    fn default() -> Self {
        SeedStyles::default().houses
    }
}

/// The district styles (section 6.2), one per district type and style.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SeedStyles {
    /// `old_town` / `organic`.
    pub old_town: SeedStyle,
    /// `business` / `grid`.
    pub business: SeedStyle,
    /// `residential` / `microdistrict`.
    pub microdistrict: SeedStyle,
    /// `residential` / `houses`.
    pub houses: SeedStyle,
    /// `industrial` / `grid`.
    pub industrial: SeedStyle,
    /// `port` / `quay`.
    pub port: SeedStyle,
    /// `park` / `paths`.
    pub park: SeedStyle,
}

#[allow(clippy::too_many_arguments)]
fn style(
    spacing_m: [f64; 2],
    jitter: f64,
    warp_m: f64,
    street: &str,
    loop_street: &str,
    street_lit: f64,
    street_drop: f64,
    frontage_m: [f64; 2],
    depth_m: [f64; 2],
    setback_m: f64,
    side_gap_m: f64,
    floors: [f64; 2],
    height_exp: f64,
    keep: f64,
    l_share: f64,
    kinds: &[&str],
    landuse: &str,
    max_height_m: f64,
    density: f64,
) -> SeedStyle {
    SeedStyle {
        spacing_m,
        jitter,
        warp_m,
        street: street.into(),
        loop_street: loop_street.into(),
        street_lit,
        street_drop,
        frontage_m,
        depth_m,
        setback_m,
        side_gap_m,
        floors,
        height_exp,
        keep,
        l_share,
        kinds: kinds.iter().map(|k| k.to_string()).collect(),
        landuse: landuse.into(),
        max_height_m,
        density,
    }
}

impl Default for SeedStyles {
    fn default() -> Self {
        let r = "residential";
        Self {
            old_town: style([50.0, 90.0], 0.28, 6.0, r, "", 0.9, 0.2, [10.0, 18.0], [10.0, 14.0], 0.0, 0.0, [2.0, 5.0], 1.0, 0.95, 0.0, &["apartments", "retail", "commercial"], r, 25.0, 0.7),
            business: style([110.0, 150.0], 0.04, 12.0, r, "", 1.0, 0.05, [35.0, 60.0], [20.0, 40.0], 6.0, 8.0, [4.0, 35.0], 6.0, 0.85, 0.15, &["office", "commercial"], "commercial", 120.0, 0.75),
            microdistrict: style([300.0, 450.0], 0.05, 25.0, r, "service", 0.9, 0.0, [50.0, 90.0], [12.0, 14.0], 18.0, 15.0, [5.0, 16.0], 1.0, 0.9, 0.2, &["apartments"], r, 50.0, 0.4),
            houses: style([70.0, 100.0], 0.1, 18.0, r, "", 0.6, 0.15, [16.0, 24.0], [8.0, 12.0], 5.0, 3.0, [1.0, 2.0], 1.0, 0.9, 0.25, &["house", "detached"], r, 10.0, 0.35),
            industrial: style([200.0, 300.0], 0.04, 10.0, r, "", 0.5, 0.1, [60.0, 120.0], [30.0, 70.0], 10.0, 12.0, [2.0, 5.0], 1.0, 0.8, 0.2, &["industrial", "warehouse", "factory"], "industrial", 15.0, 0.5),
            port: style([120.0, 220.0], 0.04, 8.0, r, "", 0.5, 0.1, [50.0, 100.0], [25.0, 50.0], 8.0, 10.0, [2.0, 4.0], 1.0, 0.85, 0.0, &["warehouse"], "port", 15.0, 0.5),
            park: style([150.0, 250.0], 0.2, 30.0, "footway", "", 0.3, 0.3, [40.0, 80.0], [10.0, 15.0], 10.0, 10.0, [1.0, 1.0], 1.0, 0.0, 0.0, &["civic"], "park", 8.0, 0.0),
        }
    }
}
