//! View points: a band's look-ahead point in Mercator world units, slant distances to tile
//! boxes and the root candidates around it.

use crate::geo::{lat_to_tile_y, lon_to_tile_x, TileKey, MERCATOR_CIRCUMFERENCE};

use super::{BandParams, Layer, MAX_BOX_HEIGHT_M, RING_COUNT, ROOT_ZOOM};

/// A view point (one band's look-ahead) in Mercator world units [0, 1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewPoint {
    pub wx: f64,
    pub wy: f64,
    /// Height (m above MSL): AGL + terrain under the view point (equal to AGL with the terrain
    /// off).
    pub alt: f64,
    /// Meters per world unit at the view point's latitude.
    pub m_per_unit: f64,
}

impl ViewPoint {
    pub fn new(lat: f64, lon: f64, alt: f64) -> Self {
        Self {
            wx: lon_to_tile_x(lon, 0),
            wy: lat_to_tile_y(lat, 0),
            alt,
            m_per_unit: MERCATOR_CIRCUMFERENCE * crate::math::cos(lat.to_radians()),
        }
    }

    /// Horizontal distance (m) from the view point to the tile's footprint (0 inside).
    pub fn horizontal_m(&self, k: TileKey) -> f64 {
        let s = 1.0 / (1u64 << k.z) as f64;
        let (x0, y0) = (k.x as f64 * s, k.y as f64 * s);
        let dx = (x0 - self.wx).max(self.wx - (x0 + s)).max(0.0);
        let dy = (y0 - self.wy).max(self.wy - (y0 + s)).max(0.0);
        crate::math::hypot(dx, dy) * self.m_per_unit
    }

    /// Slant distance (m) to the nearest point of the tile box footprint x [0, box_h] (flat
    /// terrain). Same as `slant_box_m(k, 0.0, box_h)`.
    pub fn slant_m(&self, k: TileKey, box_h: f64) -> f64 {
        self.slant_box_m(k, 0.0, 0.0 + box_h)
    }

    /// Slant distance (m) to the nearest point of the tile box footprint x [lo, hi] (heights in
    /// meters above MSL, the scale of `alt`; see `Scheduler::views`). The scheduler passes
    /// `lo` = the tile's terrain minimum and `hi` = its terrain maximum + building height, both
    /// from `Terrain::tile_range` (deterministic per key; `[0, 0]` with the terrain off).
    pub fn slant_box_m(&self, k: TileKey, lo: f64, hi: f64) -> f64 {
        let v = (self.alt - hi).max(lo - self.alt).max(0.0);
        crate::math::hypot(self.horizontal_m(k), v)
    }

    /// Append every z13 tile that could be a root (slant < R3 edge * hysteresis) to `out`, with
    /// box tops at most [`MAX_BOX_HEIGHT_M`] (flat terrain).
    pub fn root_candidates(&self, b: &BandParams, out: &mut Vec<TileKey>) {
        self.root_candidates_below(b, MAX_BOX_HEIGHT_M, out)
    }

    /// [`ViewPoint::root_candidates`] for boxes whose tops are at most `max_top_m` above MSL
    /// (highest terrain + [`MAX_BOX_HEIGHT_M`] with the terrain active).
    pub fn root_candidates_below(&self, b: &BandParams, max_top_m: f64, out: &mut Vec<TileKey>) {
        self.root_candidates_at(b.edge_m[RING_COUNT - 1] * b.hysteresis, ROOT_ZOOM, max_top_m, out)
    }

    /// Root candidates of a layer: every `root_zoom` tile whose box (top at most `max_top_m`)
    /// can be closer than the layer's reach times the hysteresis.
    pub fn root_candidates_layer(&self, l: &Layer, max_top_m: f64, out: &mut Vec<TileKey>) {
        self.root_candidates_at(l.reach_m() * l.hysteresis, l.root_zoom, max_top_m, out)
    }

    pub(crate) fn root_candidates_at(&self, reach: f64, root_zoom: u8, max_top_m: f64, out: &mut Vec<TileKey>) {
        let v = (self.alt - max_top_m).max(0.0);
        if v >= reach {
            return;
        }
        let r_units = (reach * reach - v * v).sqrt() / self.m_per_unit;
        let n = 1i64 << root_zoom;
        let to_tile = |w: f64| (w * n as f64).floor() as i64;
        let (x0, x1) = (to_tile(self.wx - r_units).max(0), to_tile(self.wx + r_units).min(n - 1));
        let (y0, y1) = (to_tile(self.wy - r_units).max(0), to_tile(self.wy + r_units).min(n - 1));
        for y in y0..=y1 {
            for x in x0..=x1 {
                out.push(TileKey::new(root_zoom, x as u32, y as u32));
            }
        }
    }
}
