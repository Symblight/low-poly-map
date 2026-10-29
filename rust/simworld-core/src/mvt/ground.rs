//! Ground features (M5): areas from `landuse`, strips from `roads`.

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
