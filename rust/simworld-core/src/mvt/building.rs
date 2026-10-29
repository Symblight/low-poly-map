//! Buildings: the decoded building, its optional tags and the raw pixel-space form.

use super::{Landuse, OsmType, PxPolygons};

/// Optional OSM tags of a building beyond `kind` (tile attributes, see module docs). All absent
/// on archives built before the night-windows schema.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuildingTags {
    /// Raw `historic=*` value (`castle`, `ruins`, `city_gate`, ...).
    pub historic: Option<String>,
    /// Raw `man_made=*` value (`tower`, `chimney`, `water_tower`, ...).
    pub man_made: Option<String>,
    /// `windows=yes|no` (else `window=yes|no`); `None` = not tagged.
    pub windows: Option<bool>,
    /// `ruins=yes`.
    pub ruins: bool,
    /// `disused=yes`.
    pub disused: bool,
    /// Any `abandoned:building=*`.
    pub abandoned: bool,
}

/// Encoder buffer of the `buildings` layer as a fraction of the tile extent (planetiler
/// `buffer: 4` of 256 px = 64 of 4096 units; tileset structure, `data/schema/simplane.yml`).
pub const ENCODER_BUFFER_FRACTION: f64 = 4.0 / 256.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Building {
    pub osm_type: OsmType,
    pub id: u64,
    pub kind: String,
    pub height: Option<f32>,
    pub levels: Option<f32>,
    pub min_height: f32,
    /// Landuse at the building centroid (`None` = no landuse polygon there).
    pub landuse: Option<Landuse>,
    /// `historic`, `man_made`, `windows` and the state tags.
    pub tags: BuildingTags,
    /// Footprint as received (clipped by the encoder to tile + buffer), tile pixel space.
    pub polygons: PxPolygons,
    /// The encoder cut the footprint at the tile + buffer boundary (a vertex lies on it), so
    /// `polygons` and `footprint_m2` are only a piece of the building and differ per tile and
    /// zoom.
    pub clipped: bool,
    /// Footprint area as received (exterior minus holes, all polygons), m² (at the tile
    /// center's Mercator scale). Only meaningful when `clipped` is false.
    pub footprint_m2: f32,
    /// Area centroid in tile pixels.
    pub centroid_px: [f64; 2],
    /// Area centroid (east, north) relative to the tile center, meters.
    pub centroid: [f32; 2],
    /// Building base (m above MSL): the HIGHEST terrain height under the footprint
    /// (`TileProjector::base_height`, api 1.7; 0 with the terrain off). The roof is at
    /// `base_m + height` and the window floors hang from it; walls reach down to the exact
    /// ground at each of their vertices (`mesher`). Light points and beacons use
    /// `base_m + height` too, so they stay on the roof. A raised part (`min_height` > 0.25 m)
    /// takes the base of the building under it ([`stack_raised_parts`]).
    pub base_m: f32,
    /// True for exactly one of the tiles the building appears in (the tile containing its
    /// centroid). Buffer copies in neighbouring tiles are `false`. Used for light points; the
    /// mesh instead clips every building to the tile rectangle (see `mesher`).
    pub owned: bool,
}

impl Building {
    /// Deterministic seed in 0..1 from (osm_type, id).
    pub fn seed01(&self) -> f32 {
        (splitmix64(self.id ^ (self.osm_type.tag() << 59)) >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// Stable 64-bit mix (no process-random state, so output is identical across runs).
pub fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Building in tile pixel space before projection (shared by MVT decode and synthetic city).
#[derive(Clone, Debug)]
pub struct RawBuilding {
    pub osm_type: OsmType,
    pub id: u64,
    pub kind: String,
    pub height: Option<f32>,
    pub levels: Option<f32>,
    pub min_height: f32,
    pub landuse: Option<Landuse>,
    pub tags: BuildingTags,
    pub polygons: PxPolygons,
}
