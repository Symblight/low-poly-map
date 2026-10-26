//! LOD bands as a quadtree cover by 3D slant distance (design doc "Tile grid and 3D LOD").
//!
//! Pure functions, no Godot types, no allocation beyond caller-provided buffers. The tree
//! state itself (which node is split) lives in `scheduler`; this module answers "should this
//! node be split / this root exist" for one frame.
//!
//! Distances are measured in Web Mercator world units scaled to meters at the view point's
//! latitude (one `cos(lat)` per frame, no trig per tile). The scale error grows with the
//! north-south offset: ~0.6 % at 25 km at Vilnius latitude, irrelevant for band selection.
//!
//! Bands (ring = 16 - zoom): R0 z16 < 600 m, R1 z15 < 2.5 km, R2 z14 < 7 km, R3 z13 < 25 km.
//! A tile of ring r > 0 is split into its 4 children (ring r - 1) when its box is closer than
//! the finer band's outer edge, measured from that finer band's look-ahead point. Hysteresis:
//! a split tile merges again only once its distance exceeds the edge by 15 %; a z13 root is
//! added below 25 km and dropped beyond 28.75 km.

use crate::geo::{lat_to_tile_y, lon_to_tile_x, TileKey, MERCATOR_CIRCUMFERENCE};

pub const RING_COUNT: usize = 4;
/// Coarsest zoom (R3); tiles of this zoom are the roots of the cover.
pub const ROOT_ZOOM: u8 = 13;
/// Finest zoom (R0).
pub const LEAF_ZOOM: u8 = 16;
/// Outer slant-distance edge of each band (m), indexed by ring.
pub const BAND_EDGE_M: [f64; RING_COUNT] = [600.0, 2500.0, 7000.0, 25_000.0];
/// A tile changes band only once its slant distance is this factor past the band edge.
pub const HYSTERESIS: f64 = 1.15;
/// Look-ahead horizon (s): center = camera + velocity * LOOK_AHEAD_S (3D), clamped.
pub const LOOK_AHEAD_S: f64 = 20.0;
/// Box height (m) of a tile whose own and ancestors' heights are still unknown.
pub const DEFAULT_TILE_HEIGHT_M: f64 = 60.0;
/// Upper bound for any building (m), used only to bound the root candidate search.
const MAX_BOX_HEIGHT_M: f64 = 400.0;

/// Ring index for a zoom level.
pub fn ring_for_zoom(z: u8) -> Option<usize> {
    (ROOT_ZOOM..=LEAF_ZOOM).contains(&z).then(|| (LEAF_ZOOM - z) as usize)
}

pub fn zoom_for_ring(ring: usize) -> u8 {
    LEAF_ZOOM - ring as u8
}

/// The 4 children of a tile in contract order NW, NE, SW, SE.
pub fn children(k: TileKey) -> [TileKey; 4] {
    let (z, x, y) = (k.z + 1, 2 * k.x, 2 * k.y);
    [TileKey::new(z, x, y), TileKey::new(z, x + 1, y), TileKey::new(z, x, y + 1), TileKey::new(z, x + 1, y + 1)]
}

pub fn parent(k: TileKey) -> Option<TileKey> {
    (k.z > ROOT_ZOOM).then(|| TileKey::new(k.z - 1, k.x / 2, k.y / 2))
}

/// The tile's sibling group (its parent's children), including itself.
pub fn siblings(k: TileKey) -> [TileKey; 4] {
    match parent(k) {
        Some(p) => children(p),
        None => [k; 4],
    }
}

/// 3D look-ahead (ENU e, n, altitude): camera + velocity * 20 s, offset clamped to
/// `max_offset_m` (3D length), altitude clamped to the ground.
pub fn look_ahead(cam: [f64; 3], vel: [f64; 3], max_offset_m: f64) -> [f64; 3] {
    let mut d = vel.map(|v| if v.is_finite() { v * LOOK_AHEAD_S } else { 0.0 });
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    if len > max_offset_m && len > 0.0 {
        let s = max_offset_m / len;
        d = d.map(|v| v * s);
    }
    [cam[0] + d[0], cam[1] + d[1], (cam[2] + d[2]).max(0.0)]
}

/// Half the band's radius: the look-ahead clamp of a band.
pub fn look_ahead_clamp_m(ring: usize) -> f64 {
    BAND_EDGE_M[ring] * 0.5
}

/// A view point (one band's look-ahead) in Mercator world units [0, 1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewPoint {
    pub wx: f64,
    pub wy: f64,
    /// Height (m) on the `ground_height` scale: AGL + terrain under the view point (equal to
    /// AGL while the terrain is flat).
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
            m_per_unit: MERCATOR_CIRCUMFERENCE * lat.to_radians().cos(),
        }
    }

    /// Horizontal distance (m) from the view point to the tile's footprint (0 inside).
    pub fn horizontal_m(&self, k: TileKey) -> f64 {
        let s = 1.0 / (1u64 << k.z) as f64;
        let (x0, y0) = (k.x as f64 * s, k.y as f64 * s);
        let dx = (x0 - self.wx).max(self.wx - (x0 + s)).max(0.0);
        let dy = (y0 - self.wy).max(self.wy - (y0 + s)).max(0.0);
        dx.hypot(dy) * self.m_per_unit
    }

    /// Slant distance (m) to the nearest point of the tile box: footprint x [g, g + box_h], with
    /// g = `geo::ground_height` at the footprint point nearest to the view point (`alt` is the
    /// view point's height on the same vertical scale, see `Scheduler::views`).
    pub fn slant_m(&self, k: TileKey, box_h: f64) -> f64 {
        let s = 1.0 / (1u64 << k.z) as f64;
        let (x0, y0) = (k.x as f64 * s, k.y as f64 * s);
        let (nx, ny) = (self.wx.clamp(x0, x0 + s), self.wy.clamp(y0, y0 + s));
        let g = crate::geo::ground_height(crate::geo::tile_y_to_lat(ny, 0), crate::geo::tile_x_to_lon(nx, 0));
        let v = (self.alt - (g + box_h)).max(g - self.alt).max(0.0);
        self.horizontal_m(k).hypot(v)
    }

    /// Append every z13 tile that could be a root (slant < 25 km * hysteresis) to `out`.
    pub fn root_candidates(&self, out: &mut Vec<TileKey>) {
        let reach = BAND_EDGE_M[RING_COUNT - 1] * HYSTERESIS;
        let v = (self.alt - MAX_BOX_HEIGHT_M).max(0.0);
        if v >= reach {
            return;
        }
        let r_units = (reach * reach - v * v).sqrt() / self.m_per_unit;
        let n = 1i64 << ROOT_ZOOM;
        let to_tile = |w: f64| (w * n as f64).floor() as i64;
        let (x0, x1) = (to_tile(self.wx - r_units).max(0), to_tile(self.wx + r_units).min(n - 1));
        let (y0, y1) = (to_tile(self.wy - r_units).max(0), to_tile(self.wy + r_units).min(n - 1));
        for y in y0..=y1 {
            for x in x0..=x1 {
                out.push(TileKey::new(ROOT_ZOOM, x as u32, y as u32));
            }
        }
    }
}

/// Band thresholds with hysteresis: `current` = the state the tile is in now.
/// Returns whether the tile should be "in" (split / exist) given distance `d` to `edge`.
pub fn inside_with_hysteresis(d: f64, edge: f64, current: bool) -> bool {
    if current {
        d <= edge * HYSTERESIS
    } else {
        d < edge
    }
}

/// Whether a z13 root should exist (`existing` = it does now).
pub fn root_wanted(view: &ViewPoint, k: TileKey, box_h: f64, existing: bool) -> bool {
    inside_with_hysteresis(view.slant_m(k, box_h), BAND_EDGE_M[RING_COUNT - 1], existing)
}

/// Whether a tile of ring `ring` > 0 should be split into its children (`split` = it is now).
/// `finer_view` is the look-ahead point of band `ring - 1`.
pub fn split_wanted(finer_view: &ViewPoint, k: TileKey, ring: usize, box_h: f64, split: bool) -> bool {
    ring > 0 && inside_with_hysteresis(finer_view.slant_m(k, box_h), BAND_EDGE_M[ring - 1], split)
}

/// Priority ordering key: finer band first, then slant distance to the band's look-ahead
/// point, then key (total order, deterministic).
pub fn priority_cmp(a: (usize, f64, TileKey), b: (usize, f64, TileKey)) -> std::cmp::Ordering {
    a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.cmp(&b.2))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::{tile_for, Frame};

    const VILNIUS: (f64, f64) = (54.6872, 25.2797);

    #[test]
    fn ring_zoom_mapping_and_family() {
        assert_eq!(ring_for_zoom(16), Some(0));
        assert_eq!(ring_for_zoom(13), Some(3));
        assert_eq!(ring_for_zoom(12), None);
        assert_eq!(ring_for_zoom(17), None);
        assert_eq!(zoom_for_ring(2), 14);
        let k = TileKey::new(14, 9342, 5207);
        let c = children(k);
        assert_eq!(c[0], TileKey::new(15, 18684, 10414));
        assert_eq!(c[3], TileKey::new(15, 18685, 10415));
        for ch in c {
            assert_eq!(parent(ch), Some(k));
            assert_eq!(siblings(ch), c);
        }
        assert_eq!(parent(TileKey::new(13, 1, 1)), None);
    }

    #[test]
    fn slant_distance_matches_enu_geometry() {
        let f = Frame::new(VILNIUS.0, VILNIUS.1);
        let v = ViewPoint::new(VILNIUS.0, VILNIUS.1, 300.0);
        // Containing tile: horizontal 0; slant = altitude above the box top.
        let k = tile_for(VILNIUS.0, VILNIUS.1, 16);
        assert_eq!(v.horizontal_m(k), 0.0);
        assert!((v.slant_m(k, 60.0) - 240.0).abs() < 1e-9);
        assert!((v.slant_m(k, 500.0)).abs() < 1e-9, "camera inside the box height");
        // Far tiles: compare against the exact ENU rectangle distance (< 1 % error).
        for (dx, dy) in [(5i64, 0i64), (0, 7), (-6, -3), (2, 4)] {
            let k13 = tile_for(VILNIUS.0, VILNIUS.1, 13);
            let t = TileKey::new(13, (k13.x as i64 + dx) as u32, (k13.y as i64 + dy) as u32);
            let r = f.tile_rect(t);
            let ex = r.min_e.max(-r.max_e).max(0.0);
            let ey = r.min_n.max(-r.max_n).max(0.0);
            let exact = ex.hypot(ey);
            let approx = v.horizontal_m(t);
            assert!((approx - exact).abs() / exact < 0.01, "{t}: {approx} vs {exact}");
        }
    }

    #[test]
    fn look_ahead_includes_vertical_and_clamps_3d() {
        // 250 m/s east * 20 s = 5 km, clamped to 300 m (half of R0's 600 m).
        let p = look_ahead([10.0, 20.0, 300.0], [250.0, 0.0, 0.0], look_ahead_clamp_m(0));
        assert!((p[0] - 310.0).abs() < 1e-9 && (p[1] - 20.0).abs() < 1e-9 && p[2] == 300.0);
        // Climbing 10 m/s: 200 m up, not clamped for R1.
        let p = look_ahead([0.0, 0.0, 300.0], [0.0, 0.0, 10.0], look_ahead_clamp_m(1));
        assert!((p[2] - 500.0).abs() < 1e-9);
        // Diagonal: the 3D length is clamped, direction kept.
        let p = look_ahead([0.0, 0.0, 1000.0], [30.0, 0.0, -40.0], 250.0);
        assert!((p[0] - 150.0).abs() < 1e-9 && (p[2] - 800.0).abs() < 1e-9);
        // Diving below ground clamps to 0.
        let p = look_ahead([0.0, 0.0, 50.0], [0.0, 0.0, -100.0], 1000.0);
        assert_eq!(p[2], 0.0);
    }

    #[test]
    fn hysteresis_band() {
        assert!(inside_with_hysteresis(599.0, 600.0, false));
        assert!(!inside_with_hysteresis(600.0, 600.0, false));
        assert!(inside_with_hysteresis(680.0, 600.0, true));
        assert!(!inside_with_hysteresis(691.0, 600.0, true));
    }

    #[test]
    fn split_rules_by_altitude_directly_below() {
        let c13 = tile_for(VILNIUS.0, VILNIUS.1, 13);
        let c14 = tile_for(VILNIUS.0, VILNIUS.1, 14);
        let c15 = tile_for(VILNIUS.0, VILNIUS.1, 15);
        let at = |alt| ViewPoint::new(VILNIUS.0, VILNIUS.1, alt);
        let h = DEFAULT_TILE_HEIGHT_M;
        // 300 m: everything below splits down to z16.
        assert!(split_wanted(&at(300.0), c15, 1, h, false));
        // 1000 m: no R0 (slant >= 940 m), R1 yes.
        assert!(!split_wanted(&at(1000.0), c15, 1, h, false));
        assert!(split_wanted(&at(1000.0), c14, 2, h, false));
        // 3000 m: R2 below, no R1.
        assert!(!split_wanted(&at(3000.0), c14, 2, h, false));
        assert!(split_wanted(&at(3000.0), c13, 3, h, false));
        // 8000 m: lights only.
        assert!(!split_wanted(&at(8000.0), c13, 3, h, false));
        // R0 never splits.
        assert!(!split_wanted(&at(0.0), tile_for(VILNIUS.0, VILNIUS.1, 16), 0, h, true));
        // Roots: 20 km up still in, 26 km out.
        assert!(root_wanted(&at(20_000.0), c13, h, false));
        assert!(!root_wanted(&at(26_000.0), c13, h, false));
        assert!(root_wanted(&at(26_000.0), c13, h, true));
    }

    #[test]
    fn root_candidates_cover_the_slant_disk() {
        for alt in [300.0, 7000.0, 20_000.0] {
            let v = ViewPoint::new(VILNIUS.0, VILNIUS.1, alt);
            let mut c = Vec::new();
            v.root_candidates(&mut c);
            let set: std::collections::HashSet<_> = c.iter().copied().collect();
            // Every z13 tile within reach whose slant distance is <= 28.75 km is a candidate.
            let ct = tile_for(VILNIUS.0, VILNIUS.1, 13);
            for dy in -12i64..=12 {
                for dx in -12i64..=12 {
                    let k = TileKey::new(13, (ct.x as i64 + dx) as u32, (ct.y as i64 + dy) as u32);
                    if v.slant_m(k, 0.0) <= BAND_EDGE_M[3] * HYSTERESIS {
                        assert!(set.contains(&k), "alt {alt}: {k} missing");
                    }
                }
            }
        }
        let mut c = Vec::new();
        ViewPoint::new(VILNIUS.0, VILNIUS.1, 30_000.0).root_candidates(&mut c);
        assert!(c.is_empty());
    }

    #[test]
    fn priority_finer_band_first_then_distance() {
        let a = (0usize, 900.0, TileKey::new(16, 1, 1));
        let b = (1usize, 10.0, TileKey::new(15, 1, 1));
        let c = (0usize, 100.0, TileKey::new(16, 2, 2));
        let mut v = vec![a, b, c];
        v.sort_by(|x, y| priority_cmp(*x, *y));
        assert_eq!(v, vec![c, a, b]);
    }
}
