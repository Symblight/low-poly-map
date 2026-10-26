//! Geodesy and tile math. Pure f64, no Godot types.
//!
//! * Web Mercator z/x/y (slippy map) tile math.
//! * WGS84 lat/lon <-> local ENU (east, north) meters around a fixed world frame origin
//!   (exact ellipsoidal ENU tangent plane via ECEF; the up component is dropped).
//! * Godot local space: x = east, y = up, z = -north, relative to the *current* floating
//!   origin, which is the frame origin translated by `offset` (re-centering only translates,
//!   it never rotates the frame, so already-uploaded tiles stay valid after a shift).

use std::fmt;

/// WGS84 semi-major axis (m).
pub const WGS84_A: f64 = 6_378_137.0;
/// WGS84 flattening.
pub const WGS84_F: f64 = 1.0 / 298.257_223_563;
/// Web Mercator equator length (m) = 2*pi*a.
pub const MERCATOR_CIRCUMFERENCE: f64 = 2.0 * std::f64::consts::PI * WGS84_A;
/// Max latitude representable in Web Mercator.
pub const MERCATOR_MAX_LAT: f64 = 85.051_128_779_806_59;
const MEAN_EARTH_RADIUS: f64 = 6_371_008.8;

fn e2() -> f64 {
    WGS84_F * (2.0 - WGS84_F)
}

// ---------------------------------------------------------------------------------------
// Tiles
// ---------------------------------------------------------------------------------------

/// A Web Mercator tile address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileKey {
    pub z: u8,
    pub x: u32,
    pub y: u32,
}

impl TileKey {
    pub const fn new(z: u8, x: u32, y: u32) -> Self {
        Self { z, x, y }
    }

    /// Parse `"z/x/y"`.
    pub fn parse(s: &str) -> Option<Self> {
        let mut it = s.split('/');
        let z = it.next()?.parse().ok()?;
        let x = it.next()?.parse().ok()?;
        let y = it.next()?.parse().ok()?;
        if it.next().is_some() {
            return None;
        }
        Some(Self { z, x, y })
    }

    /// Latitude/longitude of the north-west corner.
    pub fn nw_lat_lon(&self) -> (f64, f64) {
        (tile_y_to_lat(self.y as f64, self.z), tile_x_to_lon(self.x as f64, self.z))
    }

    /// Latitude/longitude of the tile center.
    pub fn center_lat_lon(&self) -> (f64, f64) {
        (
            tile_y_to_lat(self.y as f64 + 0.5, self.z),
            tile_x_to_lon(self.x as f64 + 0.5, self.z),
        )
    }

    /// Lat/lon of a point given in fractional tile units (0..1 inside the tile, y down).
    pub fn frac_to_lat_lon(&self, fx: f64, fy: f64) -> (f64, f64) {
        (
            tile_y_to_lat(self.y as f64 + fy, self.z),
            tile_x_to_lon(self.x as f64 + fx, self.z),
        )
    }
}

impl fmt::Display for TileKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.z, self.x, self.y)
    }
}

#[inline]
pub fn tiles_at(z: u8) -> f64 {
    (1u64 << z) as f64
}

/// Fractional tile x for a longitude.
pub fn lon_to_tile_x(lon: f64, z: u8) -> f64 {
    (lon + 180.0) / 360.0 * tiles_at(z)
}

/// Fractional tile y for a latitude (y grows southwards).
pub fn lat_to_tile_y(lat: f64, z: u8) -> f64 {
    let lat = lat.clamp(-MERCATOR_MAX_LAT, MERCATOR_MAX_LAT).to_radians();
    (1.0 - (lat.tan() + 1.0 / lat.cos()).ln() / std::f64::consts::PI) / 2.0 * tiles_at(z)
}

pub fn tile_x_to_lon(x: f64, z: u8) -> f64 {
    x / tiles_at(z) * 360.0 - 180.0
}

pub fn tile_y_to_lat(y: f64, z: u8) -> f64 {
    let n = std::f64::consts::PI * (1.0 - 2.0 * y / tiles_at(z));
    n.sinh().atan().to_degrees()
}

/// Ground width (m) of one tile at `lat` and zoom `z`.
pub fn tile_width_m(lat: f64, z: u8) -> f64 {
    MERCATOR_CIRCUMFERENCE * lat.to_radians().cos() / tiles_at(z)
}

/// Tile containing a point.
pub fn tile_for(lat: f64, lon: f64, z: u8) -> TileKey {
    let max = (1u64 << z) as i64 - 1;
    let x = (lon_to_tile_x(lon, z).floor() as i64).clamp(0, max) as u32;
    let y = (lat_to_tile_y(lat, z).floor() as i64).clamp(0, max) as u32;
    TileKey::new(z, x, y)
}

// ---------------------------------------------------------------------------------------
// Terrain hook
// ---------------------------------------------------------------------------------------

/// Terrain height (m above the ENU ground plane) at a WGS84 point: THE single elevation
/// function of the crate. Building bases, ground-mesh vertices, light points, the slant-distance
/// query and `WorldStreamer.ground_height_at` all go through it, so a later DEM spike only swaps
/// this body. Flat (0) for now. Must stay cheap, pure and deterministic (called per vertex on
/// worker threads and per tile on the main thread).
#[inline]
pub fn ground_height(lat: f64, lon: f64) -> f64 {
    let _ = (lat, lon);
    0.0
}

// ---------------------------------------------------------------------------------------
// ENU frame
// ---------------------------------------------------------------------------------------

/// Axis-aligned rectangle in ENU meters (east/north).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectEn {
    pub min_e: f64,
    pub min_n: f64,
    pub max_e: f64,
    pub max_n: f64,
}

impl RectEn {
    pub fn width(&self) -> f64 {
        self.max_e - self.min_e
    }
    pub fn height(&self) -> f64 {
        self.max_n - self.min_n
    }
}

/// Fixed local tangent frame (ENU) at a WGS84 origin. All world math lives in this frame.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub lat0: f64,
    pub lon0: f64,
    sin_lat: f64,
    cos_lat: f64,
    sin_lon: f64,
    cos_lon: f64,
    ecef0: [f64; 3],
}

fn geodetic_to_ecef(lat: f64, lon: f64, h: f64) -> [f64; 3] {
    let (sl, cl) = lat.to_radians().sin_cos();
    let (so, co) = lon.to_radians().sin_cos();
    let n = WGS84_A / (1.0 - e2() * sl * sl).sqrt();
    [(n + h) * cl * co, (n + h) * cl * so, (n * (1.0 - e2()) + h) * sl]
}

fn ecef_to_geodetic(p: [f64; 3]) -> (f64, f64) {
    let lon = p[1].atan2(p[0]);
    let r = p[0].hypot(p[1]);
    let e2 = e2();
    let mut lat = p[2].atan2(r * (1.0 - e2));
    for _ in 0..6 {
        let s = lat.sin();
        let n = WGS84_A / (1.0 - e2 * s * s).sqrt();
        let h = r / lat.cos() - n;
        lat = p[2].atan2(r * (1.0 - e2 * n / (n + h)));
    }
    (lat.to_degrees(), lon.to_degrees())
}

impl Frame {
    pub fn new(lat0: f64, lon0: f64) -> Self {
        let lat0 = lat0.clamp(-MERCATOR_MAX_LAT, MERCATOR_MAX_LAT);
        let (sin_lat, cos_lat) = lat0.to_radians().sin_cos();
        let (sin_lon, cos_lon) = lon0.to_radians().sin_cos();
        Self {
            lat0,
            lon0,
            sin_lat,
            cos_lat,
            sin_lon,
            cos_lon,
            ecef0: geodetic_to_ecef(lat0, lon0, 0.0),
        }
    }

    fn enu_full(&self, lat: f64, lon: f64) -> [f64; 3] {
        let p = geodetic_to_ecef(lat, lon, 0.0);
        let d = [p[0] - self.ecef0[0], p[1] - self.ecef0[1], p[2] - self.ecef0[2]];
        let e = -self.sin_lon * d[0] + self.cos_lon * d[1];
        let n = -self.sin_lat * self.cos_lon * d[0] - self.sin_lat * self.sin_lon * d[1]
            + self.cos_lat * d[2];
        let u = self.cos_lat * self.cos_lon * d[0]
            + self.cos_lat * self.sin_lon * d[1]
            + self.sin_lat * d[2];
        [e, n, u]
    }

    /// Ground point (h = 0) -> (east, north) meters in this frame.
    pub fn enu(&self, lat: f64, lon: f64) -> [f64; 2] {
        let v = self.enu_full(lat, lon);
        [v[0], v[1]]
    }

    /// (east, north) meters -> lat/lon of the ground point with that horizontal ENU position.
    pub fn lat_lon(&self, e: f64, n: f64) -> (f64, f64) {
        // Surface sits below the tangent plane by ~d^2/2R; refine u with the forward map.
        let mut u = -(e * e + n * n) / (2.0 * MEAN_EARTH_RADIUS);
        let mut ll = (self.lat0, self.lon0);
        for _ in 0..2 {
            let dx = -self.sin_lon * e - self.sin_lat * self.cos_lon * n
                + self.cos_lat * self.cos_lon * u;
            let dy = self.cos_lon * e - self.sin_lat * self.sin_lon * n
                + self.cos_lat * self.sin_lon * u;
            let dz = self.cos_lat * n + self.sin_lat * u;
            ll = ecef_to_geodetic([self.ecef0[0] + dx, self.ecef0[1] + dy, self.ecef0[2] + dz]);
            u = self.enu_full(ll.0, ll.1)[2];
        }
        ll
    }

    /// ENU of the tile center.
    pub fn tile_center(&self, key: TileKey) -> [f64; 2] {
        let (lat, lon) = key.center_lat_lon();
        self.enu(lat, lon)
    }

    /// Bounding rectangle (ENU) of the tile's four corners.
    pub fn tile_rect(&self, key: TileKey) -> RectEn {
        let mut r = RectEn {
            min_e: f64::INFINITY,
            min_n: f64::INFINITY,
            max_e: f64::NEG_INFINITY,
            max_n: f64::NEG_INFINITY,
        };
        for (fx, fy) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
            let (lat, lon) = key.frac_to_lat_lon(fx, fy);
            let [e, n] = self.enu(lat, lon);
            r.min_e = r.min_e.min(e);
            r.max_e = r.max_e.max(e);
            r.min_n = r.min_n.min(n);
            r.max_n = r.max_n.max(n);
        }
        r
    }
}

/// Converts between the fixed ENU frame and Godot local space (x = east, z = -north) relative
/// to the current floating origin `offset` (ENU meters of the local origin).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LocalSpace {
    pub offset_e: f64,
    pub offset_n: f64,
}

impl LocalSpace {
    /// Godot local (x, z) -> ENU (e, n).
    pub fn to_enu(&self, x: f64, z: f64) -> [f64; 2] {
        [x + self.offset_e, -z + self.offset_n]
    }
    /// ENU (e, n) -> Godot local (x, z).
    pub fn to_local(&self, e: f64, n: f64) -> [f64; 2] {
        [e - self.offset_e, -(n - self.offset_n)]
    }
    /// ENU rect -> Godot local (min_x, min_z, max_x, max_z).
    pub fn rect_to_local(&self, r: &RectEn) -> [f64; 4] {
        [
            r.min_e - self.offset_e,
            -(r.max_n - self.offset_n),
            r.max_e - self.offset_e,
            -(r.min_n - self.offset_n),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VILNIUS: (f64, f64) = (54.6872, 25.2797);

    #[test]
    fn key_display_and_parse_round_trip() {
        let k = TileKey::new(16, 37369, 20748);
        assert_eq!(k.to_string(), "16/37369/20748");
        assert_eq!(TileKey::parse("16/37369/20748"), Some(k));
        assert_eq!(TileKey::parse("16/1"), None);
        assert_eq!(TileKey::parse("a/b/c"), None);
    }

    #[test]
    fn vilnius_tile_indices() {
        // Known slippy tile for central Vilnius at z16.
        let k = tile_for(VILNIUS.0, VILNIUS.1, 16);
        assert_eq!((k.x, k.y), (37370, 20827)); // = data test tile 16/37370/20827
        // Parent relation holds.
        let k13 = tile_for(VILNIUS.0, VILNIUS.1, 13);
        assert_eq!((k.x >> 3, k.y >> 3), (k13.x, k13.y));
    }

    #[test]
    fn mercator_round_trip() {
        for z in [0u8, 5, 13, 16, 20] {
            let x = lon_to_tile_x(VILNIUS.1, z);
            let y = lat_to_tile_y(VILNIUS.0, z);
            assert!((tile_x_to_lon(x, z) - VILNIUS.1).abs() < 1e-9);
            assert!((tile_y_to_lat(y, z) - VILNIUS.0).abs() < 1e-9);
        }
    }

    #[test]
    fn vilnius_tile_widths() {
        let w16 = tile_width_m(VILNIUS.0, 16);
        assert!((w16 - 353.5).abs() < 2.0, "z16 width {w16}");
        let w14 = tile_width_m(VILNIUS.0, 14);
        assert!((w14 - 1414.0).abs() < 8.0, "z14 width {w14}");
        // ENU rect agrees with the analytic width.
        let f = Frame::new(VILNIUS.0, VILNIUS.1);
        let k = tile_for(VILNIUS.0, VILNIUS.1, 16);
        let r = f.tile_rect(k);
        assert!((r.width() - w16).abs() < 1.0, "rect width {}", r.width());
        assert!((r.height() - w16).abs() < 1.0, "rect height {}", r.height());
        // Origin lies inside its own tile.
        assert!(r.min_e <= 0.0 && r.max_e >= 0.0 && r.min_n <= 0.0 && r.max_n >= 0.0);
    }

    #[test]
    fn enu_round_trip_and_axes() {
        let f = Frame::new(VILNIUS.0, VILNIUS.1);
        assert_eq!(f.enu(VILNIUS.0, VILNIUS.1), [0.0, 0.0]);
        for (e, n) in [(0.0, 0.0), (1000.0, 0.0), (0.0, -5000.0), (12_000.0, 9_000.0), (-15_000.0, 15_000.0)] {
            let (lat, lon) = f.lat_lon(e, n);
            let back = f.enu(lat, lon);
            assert!((back[0] - e).abs() < 1e-3 && (back[1] - n).abs() < 1e-3, "{e},{n} -> {back:?}");
        }
        // North is +n, east is +e.
        let north = f.enu(VILNIUS.0 + 0.01, VILNIUS.1);
        assert!(north[1] > 1100.0 && north[0].abs() < 1.0);
        let east = f.enu(VILNIUS.0, VILNIUS.1 + 0.01);
        assert!(east[0] > 600.0 && east[1].abs() < 1.0);
    }

    #[test]
    fn terrain_hook_is_flat_for_now() {
        assert_eq!(ground_height(VILNIUS.0, VILNIUS.1), 0.0);
        assert_eq!(ground_height(-33.9, 151.2), 0.0);
    }

    #[test]
    fn local_space_is_y_up_minus_z_north() {
        let ls = LocalSpace { offset_e: 100.0, offset_n: 50.0 };
        // A point 10 m north of the local origin has z = -10.
        assert_eq!(ls.to_local(100.0, 60.0), [0.0, -10.0]);
        assert_eq!(ls.to_enu(0.0, -10.0), [100.0, 60.0]);
        let r = RectEn { min_e: 0.0, min_n: 0.0, max_e: 10.0, max_n: 20.0 };
        assert_eq!(ls.rect_to_local(&r), [-100.0, 30.0, -90.0, 50.0]);
    }
}
