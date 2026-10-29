//! Water index and the terrain-water rule (`docs/spec-world-seed.md` sections 4.4 "Load-time
//! index" and 5.2a). Built in S0, wired into [`crate::Terrain`] in S1 (`seed::terrain`), which
//! also uses [`WaterIndex::tile_polygons`] for the vector water areas and [`WaterIndex::water_at`]
//! for the terrain colour.
//!
//! **Rivers (S1 decision):** with `rivers_flat` false (the default) rivers take no part in the
//! terrain rule: a river point is land for [`WaterIndex::apply`], river edges are not coast in the
//! distance raster, so a long river never becomes a flat trench at its lowest point. Rivers stay
//! water for [`WaterIndex::water_at`] (ground class, terrain colour) and are drawn as vector water
//! areas draped on the relief.
//!
//! * **Index.** Every water polygon is clipped into z11 Web Mercator cells by recursive quadtree
//!   descent (z0 -> z11, Sutherland-Hodgman against each child's lon/lat rectangle, so a vertex is
//!   touched about 12 times however large the polygon). A cell is *land* (absent), *water*
//!   (wholly inside one polygon, nothing stored) or *mixed* (stores its clipped rings plus 64
//!   latitude bands of edge references for the point test; 64 since S1, 16 in S0). The grid is dense over the file's
//!   bbox: one `u32` per cell.
//! * **Water test** ([`WaterIndex::water_at`]): outside the bbox the file's `outside` decides; in
//!   a water cell the answer is that polygon; in a mixed cell an even-odd ray test against the
//!   edges of the point's band.
//! * **Distance raster.** For every mixed cell and its land / mixed neighbours, `n x n` samples
//!   (corners inclusive, so neighbouring cells share their border samples bit for bit) of the
//!   **signed** distance to the nearest real water edge (negative inside water; edges created by
//!   clipping and edges along the file bbox are not coast), clamped to `±blend_m`, as `i16`
//!   decimetres, plus a `u8` index of the nearest water: 3 bytes a sample, 12 KB at 64 x 64.
//!   The distance of a sample is computed in a frame at the sample's own latitude from the
//!   clipped edges of the 3 x 3 cells around it, so it does not depend on which cell computes it.
//!   A raster with no sample within `blend_m` of water is not stored. (Signed, not the spec's
//!   unsigned distance: an unsigned field interpolated bilinearly is ~half the sample spacing at
//!   the coast line, which would leave a step of `(field - w) * smoothstep(~90 m)` there; the
//!   signed field interpolates to about 0 on the coast.)
//! * **Rule** ([`WaterIndex::apply`]): in the sea `sea_level_m`, in a lake (or river) its level;
//!   on land `w + (field - w) * smoothstep(0, blend, d)` with `d` the bilinear signed distance and
//!   `w` the bilinear level of the four samples' nearest waters (a sample with no water within
//!   `blend_m` takes the mean of the others), so the height is continuous across cells and
//!   across the line where the nearest water changes.

use super::files::WaterKind;
use super::geom::Rect;
use crate::geo::{tile_x_to_lon, tile_y_to_lat};

mod build;
mod cell;
mod query;

/// Zoom of the index cells.
pub const INDEX_ZOOM: u8 = 11;
/// Latitude bands per mixed cell for the point test.
pub const BANDS: usize = 64;

/// Raster parameters (profile `seed.water` in S1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaterParams {
    /// Land within this distance of water slopes down to it (m).
    pub blend_m: f64,
    /// Samples per raster side (64 desktop, 32 iOS).
    pub raster_n: usize,
    /// Sea level (m), `terrain.sea_level_m`.
    pub sea_level_m: f64,
    /// Rivers are flat at their level in the terrain rule like lakes (S0 prototype behaviour).
    /// False in S1: rivers are vector water only (module docs).
    pub rivers_flat: bool,
}

impl Default for WaterParams {
    fn default() -> Self {
        Self { blend_m: 400.0, raster_n: 64, sea_level_m: 0.0, rivers_flat: false }
    }
}

/// One water body (= one polygon of the file).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaterBody {
    pub kind: WaterKind,
    /// Surface height (m): `sea_level_m` for the sea; for lakes and rivers the minimum of the
    /// terrain field over the outline vertices, quantized to 1 cm.
    pub level: f64,
}

/// What a point is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaterHit {
    Land,
    /// Outside the file's bbox with `outside: "sea"`.
    OutsideSea,
    /// Polygon index of the water file.
    Body(u32),
}

#[derive(Clone, Debug)]
struct ClippedRing {
    /// Index into the cell's `polys`.
    local: u16,
    pts: Vec<[f64; 2]>,
}

#[derive(Clone, Debug)]
struct MixedCell {
    rect: Rect,
    /// Global polygon ids present in the cell, ascending.
    polys: Vec<u32>,
    rings: Vec<ClippedRing>,
    /// Edge references (ring, start vertex) per latitude band, CSR.
    band_start: [u32; BANDS + 1],
    band_edges: Vec<(u32, u32)>,
}

#[derive(Clone, Debug)]
struct Raster {
    /// Signed distance, decimetres (negative inside water).
    dist_dm: Vec<i16>,
    /// Diagonal of one raster step (m, at the cell's wider edge) plus the decimetre rounding:
    /// four samples all farther than this from water mean the point between them is land.
    diag_m: f64,
    nearest: Vec<u8>,
    /// Global polygon ids `nearest` indexes.
    waters: Vec<u32>,
}

/// Measured size of an index (section 10a budgets).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WaterStats {
    pub grid_cells: usize,
    pub water_cells: usize,
    pub mixed_cells: usize,
    pub raster_cells: usize,
    pub ring_points: usize,
    pub band_refs: usize,
    /// Bytes of the dense grid + mixed cells (rings, bands).
    pub index_bytes: usize,
    /// Bytes of the distance rasters.
    pub raster_bytes: usize,
}

/// The load-time water index of one water file.
#[derive(Clone, Debug)]
pub struct WaterIndex {
    params: WaterParams,
    bbox: [f64; 4],
    outside_sea: bool,
    bodies: Vec<WaterBody>,
    /// Cell range covered by the dense grid.
    x0: u32,
    y0: u32,
    nx: u32,
    ny: u32,
    /// 0 = land; `1 + poly` = water; `MIXED | idx` = mixed.
    grid: Vec<u32>,
    mixed: Vec<MixedCell>,
    /// Per grid cell: raster index or `u32::MAX`.
    raster_of: Vec<u32>,
    /// Latitude of the north edge of grid row `j` (`ny + 1` values, descending; the last is the
    /// south edge of the last row): the point -> row lookup is a binary search, not a Mercator
    /// projection.
    row_lat: Vec<f64>,
    rasters: Vec<Raster>,
}

const MIXED: u32 = 0x8000_0000;
const NONE: u32 = u32::MAX;

fn tile_rect(x: u32, y: u32, z: u8) -> Rect {
    [tile_x_to_lon(x as f64, z), tile_y_to_lat((y + 1) as f64, z), tile_x_to_lon((x + 1) as f64, z), tile_y_to_lat(y as f64, z)]
}

/// Per-polygon output of the descent.
enum Piece {
    Full,
    Rings(Vec<Vec<[f64; 2]>>),
}

#[cfg(test)]
#[path = "water_tests.rs"]
mod tests;
