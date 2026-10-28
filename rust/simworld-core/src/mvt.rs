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

use std::collections::HashMap;
use std::io::Read;

use mvt_reader::feature::Value;

use crate::geo::{Frame, TileKey};
use crate::profile::Profile;
use crate::terrain::Terrain;

pub const BUILDINGS_LAYER: &str = "buildings";
pub const LANDUSE_LAYER: &str = "landuse";
pub const ROADS_LAYER: &str = "roads";

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

// ---------------------------------------------------------------------------------------------
// Ground features (M5): areas from `landuse`, strips from `roads`

/// Ground classes; contract vertex COLOR `r` = class / 16.
pub const GROUND_GRASS: u8 = 0;
pub const GROUND_FOREST: u8 = 1;
pub const GROUND_WATER: u8 = 2;
pub const GROUND_RESIDENTIAL: u8 = 3;
pub const GROUND_COMMERCIAL: u8 = 4;
pub const GROUND_INDUSTRIAL: u8 = 5;
pub const GROUND_OTHER: u8 = 6;
pub const GROUND_ROAD: u8 = 8;

/// Road classes; contract vertex COLOR `b` = road class / 8 (0 = not a road).
pub const ROAD_MOTORWAY: u8 = 1;
pub const ROAD_PRIMARY: u8 = 2;
pub const ROAD_SECONDARY: u8 = 3;
pub const ROAD_TERTIARY: u8 = 4;
pub const ROAD_RESIDENTIAL: u8 = 5;
pub const ROAD_OTHER: u8 = 6;

// `landuse` kind -> (ground class, stacking level), `highway` -> road class and the `lit`
// values come from the world profile (`ground.areas`, `ground.roads`; `Profile::ground_area_class`,
// `Profile::road_class`, `Profile::is_lit`). Default levels stack bottom to top so the more
// specific cover wins where polygons overlap: 0 nature_reserve (a protection boundary, often
// spanning forest, water and houses), 1 other landuse, 2 residential, 3 industrial,
// 4 commercial, 5 grass/park, 6 forest, 7 water (ponds in parks, rivers in reserves). Roads use
// levels 8..13 above all areas (`ground::road_level`).

/// Whether a `landuse` layer kind takes part in the building kind / levels fallback. The
/// leisure / natural / riverbank kinds added for the ground mesh do not: before M5 the layer
/// held only `landuse` tags, and a building inside a park must still resolve against the
/// residential (or other) landuse around it, not against the park.
pub fn feeds_building_fallback(kind: &str) -> bool {
    !matches!(
        kind,
        "park" | "garden" | "playground" | "pitch" | "golf_course" | "nature_reserve" | "recreation_ground"
            | "water" | "wood" | "grassland" | "scrub" | "wetland" | "riverbank"
    )
}

/// One area polygon (exterior + holes) in tile pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct GroundArea {
    pub class: u8,
    /// Stacking level (see `Profile::ground_area_class`).
    pub level: u8,
    pub id: u64,
    pub rings: Vec<Vec<[f64; 2]>>,
    /// Ground COLOR `g` = tint / 8 (api 1.7; 0..7, the countryside patchwork of a seed world;
    /// always 0 for archive and synthetic-city areas).
    pub tint: u8,
    /// Ground COLOR `b` = sub-kind / 8 (api 1.7; class 7 `aeroway`: 0 field, 1 runway paved,
    /// 2 runway grass, 3 taxiway, 4 apron; 0 for every other area).
    pub sub: u8,
    /// Cut by the tile's [`GroundFeatures::cutouts`] (a seed world's countryside and water under
    /// an airport fence); false for every archive and synthetic-city area.
    pub cut: bool,
}

/// One road feature (one or more polylines) in tile pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct RoadLine {
    pub class: u8,
    pub link: bool,
    pub minor: bool,
    pub lit: bool,
    pub id: u64,
    pub lines: Vec<Vec<[f64; 2]>>,
}

/// Everything the ground mesher needs, in tile pixel space.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GroundFeatures {
    pub areas: Vec<GroundArea>,
    pub roads: Vec<RoadLine>,
    /// Convex quads (tile pixels, a seed world's airport fences) removed from every area with
    /// `cut` set, so inside a fence only the airport's own areas stack (contract section 9
    /// "Airports"). Empty for every archive and synthetic-city tile.
    pub cutouts: Vec<[[f64; 2]; 4]>,
}

impl GroundFeatures {
    pub fn is_empty(&self) -> bool {
        self.areas.is_empty() && self.roads.is_empty()
    }
    /// Deterministic order regardless of encoder order: areas bottom level first, roads by
    /// class (minor first), ties by id and first vertex.
    pub fn sort(&mut self) {
        let first = |v: &Vec<Vec<[f64; 2]>>| v.first().and_then(|r| r.first()).copied().unwrap_or([0.0; 2]);
        self.areas.sort_by(|a, b| {
            (a.level, a.class, a.id)
                .cmp(&(b.level, b.class, b.id))
                .then(first(&a.rings)[0].total_cmp(&first(&b.rings)[0]))
                .then(first(&a.rings)[1].total_cmp(&first(&b.rings)[1]))
        });
        self.roads.sort_by(|a, b| {
            (std::cmp::Reverse(a.class), a.id)
                .cmp(&(std::cmp::Reverse(b.class), b.id))
                .then(first(&a.lines)[0].total_cmp(&first(&b.lines)[0]))
                .then(first(&a.lines)[1].total_cmp(&first(&b.lines)[1]))
        });
    }
}

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

/// Largest spacing (m) of the base samples along a footprint edge ([`TileProjector::base_height`]).
pub const BASE_EDGE_STEP_M: f64 = 10.0;
/// At most this many parts per footprint edge (long walls of huge buildings).
const BASE_EDGE_MAX_PARTS: usize = 64;
/// Level of the global base-sampling lattice: points `i / 2^level` in fractional Mercator
/// coordinates (2^-21 of the circumference: 19 m Mercator, about 11 m on the ground at 55 deg).
pub const BASE_LATTICE_LEVEL: i32 = 21;
/// Most lattice points per footprint bounding box (the lattice coarsens by 2 until it fits).
const BASE_LATTICE_MAX: i64 = 64;

/// Raised parts (`min_height` > 0.25 m) stand on the building under them (api 1.7): a raised
/// part whose centroid lies inside the footprint of a building that stands on the ground takes
/// that building's base (the highest such base when several overlap), so its bottom stays
/// `min_height` above the lower part's base like its roof stays `min_height` above it, instead
/// of following the (smaller) part footprint's own highest point. A raised part over nothing
/// keeps its own base. No-op with the terrain off (every base is 0).
pub fn stack_raised_parts(buildings: &mut [Building]) {
    let raised: Vec<usize> = (0..buildings.len()).filter(|&i| buildings[i].min_height > 0.25).collect();
    for i in raised {
        let c = buildings[i].centroid_px;
        let mut best: Option<f32> = None;
        for (j, o) in buildings.iter().enumerate() {
            if j == i || o.min_height > 0.25 {
                continue;
            }
            if o.polygons.iter().any(|p| point_in_rings(c, p)) {
                best = Some(best.map_or(o.base_m, |b| b.max(o.base_m)));
            }
        }
        if let Some(b) = best {
            buildings[i].base_m = b;
        }
    }
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

/// Signed shoelace area (pixel units; positive = counter-clockwise in a y-up system).
pub fn ring_area2(ring: &[[f64; 2]]) -> f64 {
    let mut a = 0.0;
    for i in 0..ring.len() {
        let p = ring[i];
        let q = ring[(i + 1) % ring.len()];
        a += p[0] * q[1] - q[0] * p[1];
    }
    a
}

/// Area-weighted centroid of all rings (signed, so holes subtract). Falls back to the vertex
/// mean for degenerate geometry.
pub fn centroid_px(polys: &[Vec<Vec<[f64; 2]>>]) -> Option<[f64; 2]> {
    let (mut a, mut cx, mut cy) = (0.0, 0.0, 0.0);
    let (mut n, mut sx, mut sy) = (0usize, 0.0, 0.0);
    for poly in polys {
        for ring in poly {
            for i in 0..ring.len() {
                let p = ring[i];
                let q = ring[(i + 1) % ring.len()];
                let cross = p[0] * q[1] - q[0] * p[1];
                a += cross;
                cx += (p[0] + q[0]) * cross;
                cy += (p[1] + q[1]) * cross;
                n += 1;
                sx += p[0];
                sy += p[1];
            }
        }
    }
    if n == 0 {
        return None;
    }
    if a.abs() > 1e-9 {
        Some([cx / (3.0 * a), cy / (3.0 * a)])
    } else {
        Some([sx / n as f64, sy / n as f64])
    }
}

/// Projects tile pixels to ENU relative to the tile center, with the terrain height there.
/// Workers get their heights only through this (`Terrain` is part of the job): flat (0) unless
/// built [`TileProjector::with_terrain`].
///
/// Chunks (Tiles v2, step T2): a projector can be [`TileProjector::cut`] to a finer tile (a
/// render chunk) inside its pixel tile: pixels stay those of the data tile (exact Web Mercator,
/// so the chunk rectangle is axis-aligned and seams clip bit for bit), while the output origin,
/// the clip rectangle, the band and the metres per pixel follow the chunk.
#[derive(Clone, Debug)]
pub struct TileProjector {
    /// The tile the pixels belong to (the data tile of a cut projector).
    key: TileKey,
    frame: Frame,
    extent: f64,
    /// Output origin: center of the target tile (the chunk of a cut projector).
    center: [f64; 2],
    terrain: Terrain,
    /// The target tile (`key` unless cut) and its rectangle in `key`'s pixels.
    target: TileKey,
    rect: crate::mesher::ClipRect,
}

impl TileProjector {
    /// A projector over flat terrain (every height 0).
    pub fn new(key: TileKey, frame: Frame, extent: f64) -> Self {
        Self {
            key,
            frame,
            extent,
            center: frame.tile_center(key),
            terrain: Terrain::flat(),
            target: key,
            rect: crate::mesher::ClipRect::tile(extent),
        }
    }
    /// The same pixels (of this projector's tile) seen from `chunk`, a tile at or inside it:
    /// output relative to the chunk center, clip rectangle = the chunk, band and metres per
    /// pixel of the chunk. `None` if `chunk` is not inside this projector's pixel tile.
    pub fn cut(&self, chunk: TileKey) -> Option<Self> {
        if chunk.z < self.key.z || chunk.z - self.key.z > 20 {
            return None;
        }
        let dz = chunk.z - self.key.z;
        if (chunk.x >> dz, chunk.y >> dz) != (self.key.x, self.key.y) {
            return None;
        }
        let n = (1u64 << dz) as f64;
        let s = self.extent / n;
        let (ix, iy) = ((chunk.x - (self.key.x << dz)) as f64, (chunk.y - (self.key.y << dz)) as f64);
        let rect = crate::mesher::ClipRect { x0: ix * s, y0: iy * s, x1: (ix + 1.0) * s, y1: (iy + 1.0) * s };
        Some(Self { center: self.frame.tile_center(chunk), target: chunk, rect, ..self.clone() })
    }
    /// The target tile: the projector's own tile, or the chunk it was [`TileProjector::cut`] to.
    /// Bands, drape margins and airport fields follow it.
    #[allow(clippy::misnamed_getters)]
    pub fn key(&self) -> TileKey {
        self.target
    }
    /// The tile the pixel coordinates belong to (the data tile of a cut projector).
    pub fn pixel_key(&self) -> TileKey {
        self.key
    }
    /// The target tile's rectangle in pixels (the whole tile unless cut).
    pub fn rect(&self) -> crate::mesher::ClipRect {
        self.rect
    }
    /// Whether this is a chunk cut from a larger pixel tile.
    pub fn is_cut(&self) -> bool {
        self.target != self.key
    }
    /// Ownership of a building by its centroid, and its centroid relative to the target tile:
    /// the decode result (`owned`, `centroid`) for a whole tile; for a chunk, the chunk whose
    /// half-open rectangle contains the centroid (each building belongs to exactly one of the
    /// chunks of its data tile).
    pub fn place(&self, b: &Building) -> Option<[f32; 2]> {
        if !self.is_cut() {
            return b.owned.then_some(b.centroid);
        }
        let (r, c) = (&self.rect, b.centroid_px);
        (c[0] >= r.x0 && c[0] < r.x1 && c[1] >= r.y0 && c[1] < r.y1).then(|| self.project(c))
    }
    /// The same projector sampling `terrain` for every height.
    pub fn with_terrain(mut self, terrain: Terrain) -> Self {
        self.terrain = terrain;
        self
    }
    pub fn terrain(&self) -> &Terrain {
        &self.terrain
    }
    /// WGS84 lat/lon of a tile pixel.
    pub fn lat_lon(&self, px: [f64; 2]) -> (f64, f64) {
        self.key.frac_to_lat_lon(px[0] / self.extent, px[1] / self.extent)
    }
    pub fn extent(&self) -> f64 {
        self.extent
    }
    pub fn center(&self) -> [f64; 2] {
        self.center
    }
    /// Tile pixel -> (east, north) meters relative to the tile center (f64).
    pub fn project64(&self, px: [f64; 2]) -> [f64; 2] {
        self.project_h(px).0
    }
    /// Tile pixel -> ((east, north) relative to the tile center, terrain height there).
    pub fn project_h(&self, px: [f64; 2]) -> ([f64; 2], f64) {
        let (lat, lon) = self.lat_lon(px);
        let [e, n] = self.frame.enu(lat, lon);
        ([e - self.center[0], n - self.center[1]], self.terrain.height(lat, lon))
    }
    /// [`TileProjector::project_h`] with the water-snapped height (`Terrain::water_height`).
    pub fn project_water(&self, px: [f64; 2]) -> ([f64; 2], f64) {
        let (lat, lon) = self.lat_lon(px);
        let [e, n] = self.frame.enu(lat, lon);
        ([e - self.center[0], n - self.center[1]], self.terrain.water_height(lat, lon))
    }
    /// Terrain height (m) at a tile pixel.
    pub fn ground_at(&self, px: [f64; 2]) -> f64 {
        let (lat, lon) = self.lat_lon(px);
        self.terrain.height(lat, lon)
    }
    /// Building base (m), api 1.7: the HIGHEST terrain height under the footprint as decoded
    /// (before the mesher's tile clipping and simplification, so every band and every tile the
    /// building is meshed in samples the same outline; only the encoder's own buffer cut,
    /// `Building::clipped`, can differ): every outer-ring vertex, points along every outer edge
    /// at most [`BASE_EDGE_STEP_M`] apart (at least the midpoint), the centroid when it lies
    /// inside, and the points of a global Mercator lattice inside the footprint
    /// ([`BASE_LATTICE_LEVEL`], coarsened by powers of two to at most [`BASE_LATTICE_MAX`]
    /// points), so a hill top inside a large footprint counts. With the terrain off, the height
    /// at `centroid` (0, nothing else is sampled).
    pub fn base_height(&self, polygons: &PxPolygons, centroid: [f64; 2]) -> f64 {
        if !self.terrain.is_active() {
            return self.ground_at(centroid);
        }
        let mut hi = f64::NEG_INFINITY;
        let step_px = BASE_EDGE_STEP_M / self.m_per_px();
        let (mut b0, mut b1) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for p in polygons {
            let Some(outer) = p.first() else { continue };
            let n = outer.len();
            for i in 0..n {
                let (a, b) = (outer[i], outer[(i + 1) % n]);
                hi = hi.max(self.ground_at(a));
                for k in 0..2 {
                    b0[k] = b0[k].min(a[k]);
                    b1[k] = b1[k].max(a[k]);
                }
                let len = crate::math::hypot(b[0] - a[0], b[1] - a[1]);
                let parts = ((len / step_px).ceil() as usize).clamp(2, BASE_EDGE_MAX_PARTS);
                for j in 1..parts {
                    let t = j as f64 / parts as f64;
                    hi = hi.max(self.ground_at([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]));
                }
            }
        }
        if !hi.is_finite() {
            return self.ground_at(centroid);
        }
        let inside = |q: [f64; 2]| polygons.iter().any(|p| point_in_rings(q, p));
        if inside(centroid) {
            hi = hi.max(self.ground_at(centroid));
        }
        // Global lattice (fractional Mercator coordinates i / 2^level), the same points at every
        // zoom: tile pixel = (i / 2^level * 2^z - x) * extent.
        let (z, ext) = (self.key.z as i32, self.extent);
        let to_g = |px: f64, t: u32| (t as f64 + px / ext) / (1u64 << z) as f64;
        let (gx0, gx1) = (to_g(b0[0], self.key.x), to_g(b1[0], self.key.x));
        let (gy0, gy1) = (to_g(b0[1], self.key.y), to_g(b1[1], self.key.y));
        let mut level = BASE_LATTICE_LEVEL;
        let range = |lv: i32| {
            let s = (1u64 << lv) as f64;
            let (ix0, ix1) = ((gx0 * s).ceil() as i64, (gx1 * s).floor() as i64);
            let (iy0, iy1) = ((gy0 * s).ceil() as i64, (gy1 * s).floor() as i64);
            (ix0, ix1, iy0, iy1)
        };
        let mut r = range(level);
        while level > 0 && (r.1 - r.0 + 1).max(0) * (r.3 - r.2 + 1).max(0) > BASE_LATTICE_MAX {
            level -= 1;
            r = range(level);
        }
        let s = (1u64 << level) as f64;
        let zs = (1u64 << z) as f64;
        for iy in r.2..=r.3 {
            for ix in r.0..=r.1 {
                let q = [(ix as f64 / s * zs - self.key.x as f64) * ext, (iy as f64 / s * zs - self.key.y as f64) * ext];
                if inside(q) {
                    hi = hi.max(self.ground_at(q));
                }
            }
        }
        hi
    }
    pub fn project(&self, px: [f64; 2]) -> [f32; 2] {
        let [e, n] = self.project64(px);
        [e as f32, n as f32]
    }
    /// Meters per pixel at the target tile's center (for tolerances and areas).
    pub fn m_per_px(&self) -> f64 {
        if self.is_cut() {
            let (lat, _) = self.target.center_lat_lon();
            let px = self.rect.x1 - self.rect.x0;
            return crate::geo::tile_width_m(lat, self.target.z) / px;
        }
        let (lat, _) = self.key.center_lat_lon();
        crate::geo::tile_width_m(lat, self.key.z) / self.extent
    }

    pub fn finish(&self, raw: RawBuilding) -> Option<Building> {
        let c = centroid_px(&raw.polygons)?;
        let owned = c[0] >= 0.0 && c[0] < self.extent && c[1] >= 0.0 && c[1] < self.extent;
        let mut polygons = raw.polygons;
        polygons.retain(|p| !p.is_empty() && p[0].len() >= 3);
        if polygons.is_empty() {
            return None;
        }
        // A vertex on (or beyond) the encoder's buffer line: the footprint was cut there.
        let buf = self.extent * ENCODER_BUFFER_FRACTION;
        let (lo, hi) = (-buf + 1.0, self.extent + buf - 1.0);
        let clipped = polygons
            .iter()
            .flatten()
            .flatten()
            .any(|p| p[0] <= lo || p[0] >= hi || p[1] <= lo || p[1] >= hi);
        let area_px2: f64 = polygons
            .iter()
            .map(|p| ring_area2(&p[0]).abs() - p[1..].iter().map(|h| ring_area2(h).abs()).sum::<f64>())
            .sum::<f64>()
            * 0.5;
        let mpp = self.m_per_px();
        let base_m = self.base_height(&polygons, c) as f32;
        Some(Building {
            osm_type: raw.osm_type,
            id: raw.id,
            kind: raw.kind,
            height: raw.height,
            levels: raw.levels,
            min_height: raw.min_height,
            landuse: raw.landuse,
            tags: raw.tags,
            polygons,
            clipped,
            footprint_m2: (area_px2.max(0.0) * mpp * mpp) as f32,
            centroid_px: c,
            centroid: self.project(c),
            base_m,
            owned,
        })
    }
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Float(f) => Some(*f as f64),
        Value::Double(d) => Some(*d),
        Value::Int(i) | Value::SInt(i) => Some(*i as f64),
        Value::UInt(u) => Some(*u as f64),
        Value::String(s) => {
            // Lenient: "12", "12.5 m", "12,5".
            let t: String = s
                .trim()
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',' || *c == '-')
                .map(|c| if c == ',' { '.' } else { c })
                .collect();
            t.parse().ok()
        }
        _ => None,
    }
    .filter(|x: &f64| x.is_finite())
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => None,
        other => num(other).map(|n| n.to_string()),
    }
}

/// A boolean tile attribute: MVT bool, or the strings `yes` / `true` / `1` (and numbers != 0).
fn flag(v: &Value) -> Option<bool> {
    match v {
        Value::Bool(b) => Some(*b),
        Value::String(s) => match s.trim() {
            "yes" | "true" | "1" => Some(true),
            "no" | "false" | "0" => Some(false),
            _ => None,
        },
        Value::Null => None,
        other => num(other).map(|n| n != 0.0),
    }
}

/// The optional building tags of a feature (absent attributes = absent tags).
fn building_tags(props: &HashMap<String, Value>) -> BuildingTags {
    let s = |k: &str| props.get(k).and_then(text).map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    let b = |k: &str| props.get(k).and_then(flag).unwrap_or(false);
    BuildingTags {
        historic: s("historic"),
        man_made: s("man_made"),
        windows: props.get("windows").and_then(flag),
        ruins: b("ruins"),
        disused: b("disused"),
        abandoned: b("abandoned"),
    }
}

fn ring_px(ls: &geo_types::LineString<f32>) -> Vec<[f64; 2]> {
    let mut pts: Vec<[f64; 2]> = ls.0.iter().map(|c| [c.x as f64, c.y as f64]).collect();
    // Drop the closing duplicate.
    if pts.len() > 1 && pts.first() == pts.last() {
        pts.pop();
    }
    pts
}

fn poly_px(p: &geo_types::Polygon<f32>) -> Vec<Vec<[f64; 2]>> {
    let mut rings = vec![ring_px(p.exterior())];
    rings.extend(p.interiors().iter().map(ring_px));
    rings
}

fn feature_polys(g: &geo_types::Geometry<f32>) -> Option<PxPolygons> {
    match g {
        geo_types::Geometry::Polygon(p) => Some(vec![poly_px(p)]),
        geo_types::Geometry::MultiPolygon(mp) => Some(mp.0.iter().map(poly_px).collect()),
        _ => None,
    }
}

fn feature_lines(g: &geo_types::Geometry<f32>) -> Option<Vec<Vec<[f64; 2]>>> {
    let pts = |ls: &geo_types::LineString<f32>| ls.0.iter().map(|c| [c.x as f64, c.y as f64]).collect::<Vec<_>>();
    match g {
        geo_types::Geometry::LineString(l) => Some(vec![pts(l)]),
        geo_types::Geometry::MultiLineString(ml) => Some(ml.0.iter().map(pts).collect()),
        _ => None,
    }
}

/// Inflate gzip if the payload starts with the gzip magic, else return as is.
pub fn maybe_gunzip(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
        let mut out = Vec::with_capacity(bytes.len() * 3);
        flate2::read::MultiGzDecoder::new(bytes)
            .read_to_end(&mut out)
            .map_err(|e| format!("gunzip: {e}"))?;
        Ok(out)
    } else {
        Ok(bytes.to_vec())
    }
}

// ---------------------------------------------------------------------------------------------
// Landuse point-in-polygon index

struct LandusePoly {
    class: Landuse,
    /// min x, min y, max x, max y (pixels).
    bbox: [f64; 4],
    area: f64,
    id: u64,
    rings: Vec<Vec<[f64; 2]>>,
}

/// Uniform grid over the tile (plus buffer) with bbox-prefiltered candidates per cell, so a
/// dense z13 tile (4.5k buildings, hundreds of landuse polygons) resolves in well under 1 ms.
pub struct LanduseIndex {
    polys: Vec<LandusePoly>,
    cells: Vec<Vec<u32>>,
    min: f64,
    cell: f64,
}

const LANDUSE_GRID: usize = 16;

/// Even-odd point-in-polygon over all rings (holes included).
pub fn point_in_rings(p: [f64; 2], rings: &[Vec<[f64; 2]>]) -> bool {
    let mut inside = false;
    for ring in rings {
        let n = ring.len();
        if n < 3 {
            continue;
        }
        let mut j = n - 1;
        for i in 0..n {
            let (a, b) = (ring[i], ring[j]);
            if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
                inside = !inside;
            }
            j = i;
        }
    }
    inside
}

impl LanduseIndex {
    pub fn empty() -> Self {
        Self { polys: Vec::new(), cells: Vec::new(), min: 0.0, cell: 1.0 }
    }

    /// `extent` = layer extent; the grid spans [-extent/8, extent*9/8] to cover the buffer.
    pub fn new(polys_in: Vec<(Landuse, u64, Vec<Vec<[f64; 2]>>)>, extent: f64) -> Self {
        let min = -extent / 8.0;
        let cell = extent * 1.25 / LANDUSE_GRID as f64;
        let mut polys = Vec::with_capacity(polys_in.len());
        let mut cells = vec![Vec::new(); LANDUSE_GRID * LANDUSE_GRID];
        for (class, id, rings) in polys_in {
            if rings.is_empty() || rings[0].len() < 3 {
                continue;
            }
            let mut bbox = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
            for p in &rings[0] {
                bbox[0] = bbox[0].min(p[0]);
                bbox[1] = bbox[1].min(p[1]);
                bbox[2] = bbox[2].max(p[0]);
                bbox[3] = bbox[3].max(p[1]);
            }
            let area = ring_area2(&rings[0]).abs() - rings[1..].iter().map(|r| ring_area2(r).abs()).sum::<f64>();
            let idx = polys.len() as u32;
            let c0 = Self::cell_of(min, cell, bbox[0]);
            let c1 = Self::cell_of(min, cell, bbox[2]);
            let r0 = Self::cell_of(min, cell, bbox[1]);
            let r1 = Self::cell_of(min, cell, bbox[3]);
            for r in r0..=r1 {
                for c in c0..=c1 {
                    cells[r * LANDUSE_GRID + c].push(idx);
                }
            }
            polys.push(LandusePoly { class, bbox, area, id, rings });
        }
        Self { polys, cells, min, cell }
    }

    fn cell_of(min: f64, cell: f64, v: f64) -> usize {
        (((v - min) / cell).floor().max(0.0) as usize).min(LANDUSE_GRID - 1)
    }

    pub fn len(&self) -> usize {
        self.polys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.polys.is_empty()
    }

    /// Landuse at a pixel position: the smallest containing polygon (most specific), ties by id.
    pub fn lookup(&self, p: [f64; 2]) -> Option<Landuse> {
        if self.polys.is_empty() {
            return None;
        }
        let c = Self::cell_of(self.min, self.cell, p[0]);
        let r = Self::cell_of(self.min, self.cell, p[1]);
        let mut best: Option<&LandusePoly> = None;
        for &i in &self.cells[r * LANDUSE_GRID + c] {
            let lp = &self.polys[i as usize];
            let b = lp.bbox;
            if p[0] < b[0] || p[0] > b[2] || p[1] < b[1] || p[1] > b[3] {
                continue;
            }
            if let Some(cur) = best {
                if (lp.area, lp.id) >= (cur.area, cur.id) {
                    continue;
                }
            }
            if point_in_rings(p, &lp.rings) {
                best = Some(lp);
            }
        }
        best.map(|lp| lp.class)
    }
}

// ---------------------------------------------------------------------------------------------

fn layer_extent(l: &mvt_reader::layer::Layer) -> f64 {
    if l.extent == 0 {
        4096.0
    } else {
        l.extent as f64
    }
}

/// Scale every point by `k` (a layer whose extent differs from the tile's common extent; the
/// common extent is the largest, so `k` is a power of two for the usual extents and exact).
fn scale_rings(rings: &mut [Vec<[f64; 2]>], k: f64) {
    if k != 1.0 {
        for p in rings.iter_mut().flatten() {
            p[0] *= k;
            p[1] *= k;
        }
    }
}

/// The common pixel extent of a tile: the largest extent of the layers the decoder reads. Each
/// layer may have its own extent (the Tiles v2 overview level mixes 4096 and 16384); every layer
/// is scaled to the common one, so all geometry of a tile shares one pixel space.
fn common_extent(layers: &[mvt_reader::layer::Layer]) -> f64 {
    layers
        .iter()
        .filter(|l| l.name == BUILDINGS_LAYER || l.name == LANDUSE_LAYER || l.name == ROADS_LAYER)
        .map(layer_extent)
        .fold(None, |a: Option<f64>, e| Some(a.map_or(e, |a| a.max(e))))
        .unwrap_or(4096.0)
}

/// `landuse` layer -> building fallback index, plus ground areas when `ground` is set.
fn decode_landuse(
    reader: &mvt_reader::Reader,
    layers: &[mvt_reader::layer::Layer],
    ground: Option<&mut Vec<GroundArea>>,
    profile: &Profile,
    extent: f64,
) -> Result<LanduseIndex, String> {
    let Some(layer) = layers.iter().find(|l| l.name == LANDUSE_LAYER) else {
        return Ok(LanduseIndex::empty());
    };
    let k = extent / layer_extent(layer);
    let features = reader
        .get_features(layer.layer_index)
        .map_err(|e| format!("mvt landuse: {e:?}"))?;
    let empty = HashMap::new();
    let mut polys = Vec::new();
    let mut areas = ground;
    for f in features {
        let Some(pp) = feature_polys(&f.geometry) else { continue };
        let props = f.properties.as_ref().unwrap_or(&empty);
        let kind = props.get("kind").and_then(text).unwrap_or_default();
        let id = props.get("id").and_then(num).filter(|v| *v >= 0.0).map(|v| v as u64).or(f.id).unwrap_or(0);
        let fallback = feeds_building_fallback(&kind);
        let class = Landuse::parse(&kind);
        let (gclass, level) = profile.ground_area_class(&kind);
        for mut rings in pp {
            if rings.is_empty() || rings[0].len() < 3 {
                continue;
            }
            scale_rings(&mut rings, k);
            match areas.as_deref_mut() {
                Some(a) => {
                    if fallback {
                        polys.push((class, id, rings.clone()));
                    }
                    a.push(GroundArea { class: gclass, level, id, rings, tint: 0, sub: 0, cut: false });
                }
                None if fallback => polys.push((class, id, rings)),
                None => {}
            }
        }
    }
    Ok(LanduseIndex::new(polys, extent))
}

/// `roads` layer -> road polylines.
fn decode_roads(
    reader: &mvt_reader::Reader,
    layers: &[mvt_reader::layer::Layer],
    out: &mut Vec<RoadLine>,
    profile: &Profile,
    extent: f64,
) -> Result<(), String> {
    let Some(layer) = layers.iter().find(|l| l.name == ROADS_LAYER) else {
        return Ok(());
    };
    let k = extent / layer_extent(layer);
    let features = reader
        .get_features(layer.layer_index)
        .map_err(|e| format!("mvt roads: {e:?}"))?;
    let empty = HashMap::new();
    for f in features {
        let Some(mut lines) = feature_lines(&f.geometry) else { continue };
        lines.retain(|l| l.len() >= 2);
        if lines.is_empty() {
            continue;
        }
        scale_rings(&mut lines, k);
        let props = f.properties.as_ref().unwrap_or(&empty);
        let kind = props.get("kind").and_then(text).unwrap_or_default();
        let (class, link, minor) = profile.road_class(&kind);
        let lit = props.get("lit").and_then(text).is_some_and(|v| profile.is_lit(&v));
        let id = props.get("id").and_then(num).filter(|v| *v >= 0.0).map(|v| v as u64).or(f.id).unwrap_or(0);
        out.push(RoadLine { class, link, minor, lit, id, lines });
    }
    Ok(())
}

/// Decode the `buildings` layer of an MVT tile (gzip or raw) into buildings; each building's
/// landuse is resolved from the tile's `landuse` layer at its centroid.
pub fn decode_buildings(bytes: &[u8], key: TileKey, frame: Frame, profile: &Profile) -> Result<Vec<Building>, String> {
    decode_tile(bytes, key, frame, false, false, profile).map(|t| t.buildings)
}

/// One decoded tile: buildings, ground features (only when requested) and the projector.
#[derive(Debug)]
pub struct DecodedTile {
    pub buildings: Vec<Building>,
    pub ground: GroundFeatures,
    pub proj: TileProjector,
}

/// Decode buildings (always) and, with `want_ground`, the `landuse` areas, with `want_roads` the `roads` lines (ground
/// meshes need both; street lamps need roads only, also on R3). Flat terrain; see [`decode_tile_on`].
pub fn decode_tile(
    bytes: &[u8],
    key: TileKey,
    frame: Frame,
    want_ground: bool,
    want_roads: bool,
    profile: &Profile,
) -> Result<DecodedTile, String> {
    decode_tile_on(bytes, key, frame, &Terrain::flat(), want_ground, want_roads, profile)
}

/// [`decode_tile`] on `terrain`: building bases and every later height of the tile come from it
/// (the projector carries it).
pub fn decode_tile_on(
    bytes: &[u8],
    key: TileKey,
    frame: Frame,
    terrain: &Terrain,
    want_ground: bool,
    want_roads: bool,
    profile: &Profile,
) -> Result<DecodedTile, String> {
    let empty_tile = || DecodedTile {
        buildings: Vec::new(),
        ground: GroundFeatures::default(),
        proj: TileProjector::new(key, frame, 4096.0).with_terrain(terrain.clone()),
    };
    if bytes.is_empty() {
        return Ok(empty_tile());
    }
    let data = maybe_gunzip(bytes)?;
    let reader = mvt_reader::Reader::new(data).map_err(|e| format!("mvt: {e:?}"))?;
    let layers = reader.get_layer_metadata().map_err(|e| format!("mvt layers: {e:?}"))?;
    let mut ground = GroundFeatures::default();
    // One pixel space per tile: the largest layer extent; other layers are scaled to it.
    let extent = common_extent(&layers);
    let landuse = decode_landuse(&reader, &layers, want_ground.then_some(&mut ground.areas), profile, extent)?;
    if want_roads {
        decode_roads(&reader, &layers, &mut ground.roads, profile, extent)?;
    }
    if want_ground || want_roads {
        ground.sort();
    }
    let proj = TileProjector::new(key, frame, extent).with_terrain(terrain.clone());
    let Some(layer) = layers.iter().find(|l| l.name == BUILDINGS_LAYER) else {
        return Ok(DecodedTile { buildings: Vec::new(), ground, proj });
    };
    let kb = extent / layer_extent(layer);
    let features = reader
        .get_features(layer.layer_index)
        .map_err(|e| format!("mvt features: {e:?}"))?;
    let empty = HashMap::new();
    let mut out = Vec::with_capacity(features.len());
    for f in features {
        let Some(mut polygons) = feature_polys(&f.geometry) else { continue };
        if kb != 1.0 {
            for p in polygons.iter_mut() {
                scale_rings(p, kb);
            }
        }
        let props = f.properties.as_ref().unwrap_or(&empty);
        let id = props
            .get("id")
            .and_then(num)
            .filter(|v| *v >= 0.0)
            .map(|v| v as u64)
            .or(f.id.map(|fid| fid / 10))
            .unwrap_or(0);
        let lu = centroid_px(&polygons).and_then(|c| landuse.lookup(c));
        let raw = RawBuilding {
            osm_type: props
                .get("osm_type")
                .and_then(text)
                .map(|s| OsmType::parse(&s))
                .unwrap_or(OsmType::Unknown),
            id,
            kind: props.get("kind").and_then(text).unwrap_or_else(|| "yes".into()),
            height: props.get("height").and_then(num).map(|v| v as f32),
            levels: props.get("levels").and_then(num).map(|v| v as f32),
            min_height: props.get("min_height").and_then(num).unwrap_or(0.0).max(0.0) as f32,
            landuse: lu,
            tags: building_tags(props),
            polygons,
        };
        if let Some(b) = proj.finish(raw) {
            out.push(b);
        }
    }
    // Deterministic order regardless of encoder order.
    out.sort_by(|a, b| {
        (a.osm_type.tag(), a.id)
            .cmp(&(b.osm_type.tag(), b.id))
            .then(a.centroid_px[0].total_cmp(&b.centroid_px[0]))
            .then(a.centroid_px[1].total_cmp(&b.centroid_px[1]))
    });
    if terrain.is_active() {
        stack_raised_parts(&mut out);
    }
    Ok(DecodedTile { buildings: out, ground, proj })
}

#[cfg(test)]
pub(crate) mod test_mvt {
    //! Minimal MVT (protobuf) encoder for fixtures.

    fn varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }
    fn tag(out: &mut Vec<u8>, field: u32, wire: u32) {
        varint(out, ((field << 3) | wire) as u64);
    }
    fn bytes_field(out: &mut Vec<u8>, field: u32, b: &[u8]) {
        tag(out, field, 2);
        varint(out, b.len() as u64);
        out.extend_from_slice(b);
    }
    fn zz(v: i32) -> u64 {
        ((v << 1) ^ (v >> 31)) as u32 as u64
    }

    pub enum Val {
        S(&'static str),
        D(f64),
        B(bool),
    }

    pub struct Feat {
        pub id: u64,
        /// polygons -> rings (exterior clockwise in screen space, holes counter-clockwise).
        pub rings: Vec<Vec<(i32, i32)>>,
        pub props: Vec<(&'static str, Val)>,
    }

    /// Encode one polygon layer named `name` with `extent` 4096.
    pub fn encode_layer(name: &str, feats: &[Feat]) -> Vec<u8> {
        encode_layer_typed(name, feats, false)
    }

    /// Encode one layer; `lines` = LINESTRING features (each "ring" is one open line).
    pub fn encode_layer_typed(name: &str, feats: &[Feat], lines: bool) -> Vec<u8> {
        encode_layer_ext(name, feats, lines, 4096)
    }

    /// [`encode_layer_typed`] with the layer's `extent`.
    pub fn encode_layer_ext(name: &str, feats: &[Feat], lines: bool, extent: u32) -> Vec<u8> {
        let mut keys: Vec<&str> = Vec::new();
        let mut vals: Vec<Vec<u8>> = Vec::new();
        let mut layer = Vec::new();
        tag(&mut layer, 15, 0);
        varint(&mut layer, 2);
        bytes_field(&mut layer, 1, name.as_bytes());
        for f in feats {
            let mut fb = Vec::new();
            tag(&mut fb, 1, 0);
            varint(&mut fb, f.id);
            let mut tags = Vec::new();
            for (k, v) in &f.props {
                let ki = keys.iter().position(|x| x == k).unwrap_or_else(|| {
                    keys.push(k);
                    keys.len() - 1
                });
                let mut vb = Vec::new();
                match v {
                    Val::S(s) => bytes_field(&mut vb, 1, s.as_bytes()),
                    Val::D(d) => {
                        tag(&mut vb, 3, 1);
                        vb.extend_from_slice(&d.to_le_bytes());
                    }
                    Val::B(b) => {
                        tag(&mut vb, 7, 0);
                        varint(&mut vb, *b as u64);
                    }
                }
                vals.push(vb);
                varint(&mut tags, ki as u64);
                varint(&mut tags, (vals.len() - 1) as u64);
            }
            bytes_field(&mut fb, 2, &tags);
            tag(&mut fb, 3, 0);
            varint(&mut fb, if lines { 2 } else { 3 }); // LINESTRING / POLYGON
            let mut geom = Vec::new();
            let (mut cx, mut cy) = (0, 0);
            for ring in &f.rings {
                varint(&mut geom, (1 << 3) | 1);
                varint(&mut geom, zz(ring[0].0 - cx));
                varint(&mut geom, zz(ring[0].1 - cy));
                (cx, cy) = ring[0];
                varint(&mut geom, (((ring.len() - 1) as u64) << 3) | 2);
                for p in &ring[1..] {
                    varint(&mut geom, zz(p.0 - cx));
                    varint(&mut geom, zz(p.1 - cy));
                    (cx, cy) = *p;
                }
                if !lines {
                    varint(&mut geom, (1 << 3) | 7);
                }
            }
            bytes_field(&mut fb, 4, &geom);
            bytes_field(&mut layer, 2, &fb);
        }
        for k in &keys {
            bytes_field(&mut layer, 3, k.as_bytes());
        }
        for v in &vals {
            bytes_field(&mut layer, 4, v);
        }
        tag(&mut layer, 5, 0);
        varint(&mut layer, extent as u64);
        let mut tile = Vec::new();
        bytes_field(&mut tile, 3, &layer);
        tile
    }
}

#[cfg(test)]
mod tests {
    use super::test_mvt::*;
    use super::*;
    use crate::geo::Frame;

    fn square(x0: i32, y0: i32, s: i32) -> Vec<(i32, i32)> {
        // Clockwise in screen space (y down) = MVT exterior.
        vec![(x0, y0), (x0 + s, y0), (x0 + s, y0 + s), (x0, y0 + s)]
    }

    fn buildings_layer() -> Vec<u8> {
        encode_layer(
            "buildings",
            &[
                Feat {
                    id: 1234567892,
                    rings: vec![square(1000, 1000, 400)],
                    props: vec![
                        ("id", Val::D(123456789.0)),
                        ("osm_type", Val::S("way")),
                        ("kind", Val::S("apartments")),
                        ("levels", Val::D(2.5)),
                        ("min_height", Val::D(0.0)),
                    ],
                },
                Feat {
                    id: 77,
                    // Straddles the east edge: centroid outside -> not owned.
                    rings: vec![square(4000, 2000, 300)],
                    props: vec![
                        ("id", Val::D(7.0)),
                        ("osm_type", Val::S("relation")),
                        ("kind", Val::S("office")),
                        ("height", Val::D(42.0)),
                    ],
                },
                Feat {
                    id: 99,
                    // kind=yes inside the industrial landuse below.
                    rings: vec![square(3000, 3000, 100)],
                    props: vec![("id", Val::D(9.0)), ("osm_type", Val::S("way")), ("kind", Val::S("yes"))],
                },
            ],
        )
    }

    fn fixture() -> Vec<u8> {
        let mut t = buildings_layer();
        t.extend(encode_layer(
            "landuse",
            &[
                Feat { id: 10, rings: vec![square(-64, -64, 4224)], props: vec![("kind", Val::S("residential"))] },
                Feat { id: 20, rings: vec![square(2800, 2800, 600)], props: vec![("kind", Val::S("industrial"))] },
            ],
        ));
        t
    }

    #[test]
    fn decodes_buildings_layer_properties_ownership_and_landuse() {
        let key = TileKey::new(16, 37370, 20827);
        let (lat, lon) = key.center_lat_lon();
        let frame = Frame::new(lat, lon);
        let bs = decode_buildings(&fixture(), key, frame, &Profile::default()).unwrap();
        assert_eq!(bs.len(), 3);
        let a = bs.iter().find(|b| b.id == 123456789).unwrap();
        assert_eq!(a.osm_type, OsmType::Way);
        assert_eq!(a.kind, "apartments");
        assert_eq!(a.levels, Some(2.5));
        assert_eq!(a.height, None);
        assert!((crate::mesher::resolve(a, &Profile::default()).height - 7.5).abs() < 1e-6);
        assert!(a.owned);
        assert_eq!(a.polygons[0][0].len(), 4);
        assert_eq!(a.centroid_px, [1200.0, 1200.0]);
        // Centroid of square at px (1200,1200): west and north of the tile center.
        assert!(a.centroid[0] < 0.0 && a.centroid[1] > 0.0);
        // Smallest containing landuse wins.
        assert_eq!(a.landuse, Some(Landuse::Residential));
        let y = bs.iter().find(|b| b.id == 9).unwrap();
        assert_eq!(y.landuse, Some(Landuse::Industrial));
        assert_eq!(crate::mesher::resolve(y, &Profile::default()).profile, crate::mesher::PROFILE_INDUSTRIAL);

        let b = bs.iter().find(|b| b.id == 7).unwrap();
        assert_eq!(b.osm_type, OsmType::Relation);
        assert_eq!(crate::mesher::resolve(b, &Profile::default()).height, 42.0);
        assert!(!b.owned);
    }

    /// Night-window attributes (W2 tile schema): decoded when present, absent otherwise (old
    /// archives); `clipped` / `footprint_m2` from the received geometry.
    #[test]
    fn decodes_night_window_attributes_and_clipping() {
        let t = encode_layer(
            "buildings",
            &[
                Feat {
                    id: 10,
                    rings: vec![square(1000, 1000, 100)],
                    props: vec![
                        ("id", Val::D(1.0)),
                        ("osm_type", Val::S("way")),
                        ("kind", Val::S("yes")),
                        ("historic", Val::S("castle")),
                        ("man_made", Val::S("tower")),
                        ("windows", Val::B(false)),
                        ("ruins", Val::B(true)),
                        ("disused", Val::B(true)),
                        ("abandoned", Val::S("yes")),
                    ],
                },
                // Old archive: none of the attributes. Cut by the encoder at the -64 buffer line.
                Feat { id: 20, rings: vec![square(-64, 2000, 200)], props: vec![("id", Val::D(2.0)), ("kind", Val::S("yes"))] },
                Feat { id: 30, rings: vec![square(3000, 3000, 100)], props: vec![("id", Val::D(3.0)), ("windows", Val::B(true))] },
            ],
        );
        let key = TileKey::new(16, 37370, 20827);
        let frame = Frame::new(54.6872, 25.2797);
        let bs = decode_buildings(&t, key, frame, &Profile::default()).unwrap();
        let get = |id| bs.iter().find(|b| b.id == id).unwrap();
        let a = get(1);
        assert_eq!(
            a.tags,
            BuildingTags {
                historic: Some("castle".into()),
                man_made: Some("tower".into()),
                windows: Some(false),
                ruins: true,
                disused: true,
                abandoned: true
            }
        );
        assert!(!a.clipped);
        let mpp = TileProjector::new(key, frame, 4096.0).m_per_px();
        assert!(((a.footprint_m2 as f64) - 100.0 * 100.0 * mpp * mpp).abs() < 0.01, "{}", a.footprint_m2);
        let b = get(2);
        assert_eq!(b.tags, BuildingTags::default());
        assert!(b.clipped);
        assert_eq!(get(3).tags.windows, Some(true));
        assert!(!get(3).clipped);
        // Ruins + disused: dark whatever the historic / man_made values say.
        assert_eq!(crate::mesher::resolve(a, &Profile::default()).profile, crate::mesher::PROFILE_DARK);
    }

    #[test]
    fn gzip_and_determinism() {
        use std::io::Write;
        let raw = fixture();
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&raw).unwrap();
        let gz = enc.finish().unwrap();
        let key = TileKey::new(16, 37370, 20827);
        let frame = Frame::new(54.6872, 25.2797);
        let a = decode_buildings(&raw, key, frame, &Profile::default()).unwrap();
        let b = decode_buildings(&gz, key, frame, &Profile::default()).unwrap();
        assert_eq!(a, b);
        assert_eq!(a[0].seed01(), b[0].seed01());
        assert!(decode_buildings(&[], key, frame, &Profile::default()).unwrap().is_empty());
        assert!(decode_buildings(b"garbage!", key, frame, &Profile::default()).is_err());
        // No landuse layer -> None.
        let nolu = decode_buildings(&buildings_layer(), key, frame, &Profile::default()).unwrap();
        assert!(nolu.iter().all(|b| b.landuse.is_none()));
    }

    #[test]
    fn missing_layer_is_empty_and_seed_distinguishes_type() {
        let t = encode_layer("roads", &[]);
        let frame = Frame::new(54.6872, 25.2797);
        assert!(decode_buildings(&t, TileKey::new(16, 1, 1), frame, &Profile::default()).unwrap().is_empty());
        let mk = |t| Building {
            osm_type: t,
            id: 42,
            kind: String::new(),
            height: None,
            levels: None,
            min_height: 0.0,
            landuse: None,
            tags: BuildingTags::default(),
            polygons: vec![],
            clipped: false,
            footprint_m2: 0.0,
            centroid_px: [0.0; 2],
            centroid: [0.0; 2],
            base_m: 0.0,
            owned: true,
        };
        assert_ne!(mk(OsmType::Way).seed01(), mk(OsmType::Relation).seed01());
        let s = mk(OsmType::Way).seed01();
        assert!((0.0..1.0).contains(&s));
    }

    #[test]
    fn ground_layers_decode_and_parks_do_not_change_building_fallback() {
        let mut hole = square(2600, 600, 200);
        hole.reverse();
        let mut t = encode_layer(
            "buildings",
            &[Feat {
                id: 50,
                rings: vec![square(1000, 1000, 100)],
                props: vec![("id", Val::D(5.0)), ("osm_type", Val::S("way")), ("kind", Val::S("yes"))],
            }],
        );
        t.extend(encode_layer(
            "landuse",
            &[
                Feat { id: 10, rings: vec![square(-64, -64, 4224)], props: vec![("kind", Val::S("residential"))] },
                // Smaller than the residential landuse and containing the building: before M5
                // the layer had no parks, so it must not take over the fallback.
                Feat { id: 11, rings: vec![square(900, 900, 400)], props: vec![("id", Val::D(11.0)), ("kind", Val::S("park"))] },
                Feat {
                    id: 12,
                    rings: vec![square(2400, 400, 600), hole],
                    props: vec![("id", Val::D(12.0)), ("kind", Val::S("water"))],
                },
            ],
        ));
        t.extend(encode_layer_typed(
            "roads",
            &[
                Feat {
                    id: 20,
                    rings: vec![vec![(0, 2000), (2000, 2100), (4096, 2000)]],
                    props: vec![("id", Val::D(20.0)), ("kind", Val::S("primary")), ("lit", Val::S("yes"))],
                },
                Feat { id: 21, rings: vec![vec![(100, 3000), (900, 3000)]], props: vec![("id", Val::D(21.0)), ("kind", Val::S("footway"))] },
            ],
            true,
        ));
        let key = TileKey::new(16, 37370, 20827);
        let frame = Frame::new(54.6872, 25.2797);
        let d = decode_tile(&t, key, frame, true, true, &Profile::default()).unwrap();
        assert_eq!(d.buildings.len(), 1);
        assert_eq!(d.buildings[0].landuse, Some(Landuse::Residential));
        assert_eq!(d.buildings[0].base_m, 0.0);
        let g = &d.ground;
        assert_eq!(g.areas.len(), 3);
        // Sorted bottom level first: residential, park, water.
        assert_eq!(g.areas.iter().map(|a| a.class).collect::<Vec<_>>(), vec![GROUND_RESIDENTIAL, GROUND_GRASS, GROUND_WATER]);
        assert_eq!(g.areas[2].rings.len(), 2, "water keeps its hole");
        assert_eq!(g.roads.len(), 2);
        // Roads sorted minor class first.
        assert_eq!((g.roads[0].class, g.roads[0].minor, g.roads[0].lit), (ROAD_OTHER, true, false));
        assert_eq!((g.roads[1].class, g.roads[1].lit), (ROAD_PRIMARY, true));
        assert_eq!(g.roads[1].lines[0].len(), 3);
        // Without ground: same buildings, no ground.
        let nb = decode_tile(&t, key, frame, false, false, &Profile::default()).unwrap();
        assert!(nb.ground.is_empty());
        assert_eq!(nb.buildings, d.buildings);
        // Meshes: R0 has the footway, R1 not.
        let m0 = crate::ground::mesh_ground(g, &d.proj, 0, &Profile::default());
        let m1 = crate::ground::mesh_ground(g, &d.proj, 1, &Profile::default());
        assert!(m0.triangle_count() > m1.triangle_count() && !m1.is_empty());
        assert!(m0.colors.iter().any(|c| *c == crate::ground::road_color(ROAD_PRIMARY, true)));
    }

    #[test]
    fn landuse_index_holes_and_grid() {
        let outer = vec![[0.0, 0.0], [1000.0, 0.0], [1000.0, 1000.0], [0.0, 1000.0]];
        let hole = vec![[400.0, 400.0], [600.0, 400.0], [600.0, 600.0], [400.0, 600.0]];
        let idx = LanduseIndex::new(
            vec![
                (Landuse::Residential, 1, vec![outer, hole]),
                (Landuse::Retail, 2, vec![vec![[3000.0, 3000.0], [3500.0, 3000.0], [3500.0, 3500.0]]]),
            ],
            4096.0,
        );
        assert_eq!(idx.lookup([100.0, 100.0]), Some(Landuse::Residential));
        assert_eq!(idx.lookup([500.0, 500.0]), None);
        assert_eq!(idx.lookup([3400.0, 3100.0]), Some(Landuse::Retail));
        assert_eq!(idx.lookup([3100.0, 3400.0]), None);
        assert_eq!(idx.lookup([-5000.0, 9000.0]), None);
    }
}
