//! Vector tile decode -> plain `Building` structs (no Godot types; runs on the job pool threads).
//!
//! Schema (custom planetiler profile):
//! - layer `buildings`: `id` (OSM id, f64/int), `osm_type` ("way"/"relation"), `kind`, `height`,
//!   `levels` (may be fractional), `min_height`, `colour`; since the night-windows schema (W2)
//!   also the optional `historic`, `man_made` (raw tag values), `windows` (bool, from `windows`
//!   else `window`, yes/no only), `ruins`, `disused`, `abandoned` (bools), each written only
//!   when the tag is present. Older archives lack them: every one defaults to "absent".
//! - layer `landuse`: `id`, `kind` = `landuse` tag, or a leisure value (park, garden,
//!   playground, pitch, golf_course, nature_reserve, recreation_ground), or a natural value
//!   (water, wood, grassland, scrub, wetland), or `riverbank`. All of them feed the ground mesh
//!   (`ground_area_class`); only `landuse`-tag kinds feed the building kind / levels fallbacks
//!   (`feeds_building_fallback`), so a pavilion in a park keeps the surrounding residential
//!   landuse exactly as before the leisure/natural kinds were added.
//! - layer `roads` (lines): `id`, `kind` (= `highway`), `lit`. Ground mesh only.
//!
//! The MVT feature id is NOT the OSM id (planetiler encodes osm_id*10+type) and is only a
//! fallback. Geometry stays in tile pixel space (exact Web Mercator, y down): the mesher clips
//! it to the tile / quadrant rectangle there and projects only the surviving vertices
//! (Mercator -> WGS84 -> ENU). Only the centroid is projected here (for light points).

mod attrs;
mod building;
mod decode;
mod geometry;
mod ground;
mod landuse;
mod overview;
mod projector;
mod stack;

pub use building::{splitmix64, Building, BuildingTags, RawBuilding, ENCODER_BUFFER_FRACTION};
pub use decode::{decode_buildings, decode_tile, decode_tile_on, maybe_gunzip, DecodedTile};
pub use geometry::{centroid_px, ring_area2};
pub use ground::{
    feeds_building_fallback, GroundArea, GroundFeatures, RoadLine, GROUND_COMMERCIAL, GROUND_FOREST, GROUND_GRASS,
    GROUND_INDUSTRIAL, GROUND_OTHER, GROUND_RESIDENTIAL, GROUND_ROAD, GROUND_WATER, ROAD_MOTORWAY, ROAD_OTHER,
    ROAD_PRIMARY, ROAD_RESIDENTIAL, ROAD_SECONDARY, ROAD_TERTIARY,
};
pub use landuse::{point_in_rings, LanduseIndex};
pub use overview::{decode_overview_on, landmark_flags, light_point, DecodedOverview, LightAttrs};
pub use projector::{footprint_base, tile_m_per_px, TileProjector, BASE_EDGE_STEP_M, BASE_LATTICE_LEVEL};
pub use stack::stack_raised_parts;

pub const BUILDINGS_LAYER: &str = "buildings";
pub const LANDUSE_LAYER: &str = "landuse";
pub const ROADS_LAYER: &str = "roads";
/// Overview level layers of a two-level archive (Tiles and LOD v2; `data/README.md`).
pub const BLOCKS_LAYER: &str = "blocks";
pub const LANDMARKS_LAYER: &str = "landmarks";
pub const LIGHTS_LAYER: &str = "lights";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OsmType {
    Way,
    Relation,
    Node,
    Unknown,
    Synthetic,
}

impl OsmType {
    fn parse(s: &str) -> Self {
        match s {
            "way" | "w" => Self::Way,
            "relation" | "r" => Self::Relation,
            "node" | "n" => Self::Node,
            _ => Self::Unknown,
        }
    }
    pub fn tag(self) -> u64 {
        match self {
            Self::Way => 1,
            Self::Relation => 2,
            Self::Node => 3,
            Self::Unknown => 4,
            Self::Synthetic => 5,
        }
    }
}

/// Landuse class of the polygon containing a building's centroid (fallback for `kind=yes`
/// and for missing heights).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Landuse {
    Residential,
    Commercial,
    Retail,
    Industrial,
    Civic,
    Other,
}

impl Landuse {
    pub fn parse(kind: &str) -> Self {
        match kind {
            "residential" => Self::Residential,
            "commercial" => Self::Commercial,
            "retail" => Self::Retail,
            "industrial" | "railway" | "port" | "depot" | "garages" | "quarry" => Self::Industrial,
            "religious" | "education" | "institutional" | "civic" | "military" => Self::Civic,
            _ => Self::Other,
        }
    }
}

/// Polygons -> rings (first = exterior) -> points, in tile pixels (y down, `extent` units).
pub type PxPolygons = Vec<Vec<Vec<[f64; 2]>>>;

#[cfg(test)]
pub(crate) mod test_mvt;

#[cfg(test)]
mod tests;
