//! Tile set: every tile of the archive, read into memory, and the flight order over it.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use bytes::Bytes;
use simworld_core::geo::TileKey;

struct MemBackend(Bytes);

impl pmtiles::AsyncBackend for MemBackend {
    fn read(
        &self,
        offset: usize,
        length: usize,
    ) -> impl std::future::Future<Output = pmtiles::PmtResult<pmtiles::BackendResponse>> + Send {
        let end = (offset + length).min(self.0.len());
        let b = self.0.slice(offset.min(end)..end);
        async move { Ok(pmtiles::BackendResponse::new(b)) }
    }
}

fn lon_to_x(lon: f64, z: u8) -> f64 {
    (lon + 180.0) / 360.0 * (1u64 << z) as f64
}

fn lat_to_y(lat: f64, z: u8) -> f64 {
    let r = lat.to_radians();
    (1.0 - (r.tan() + 1.0 / r.cos()).ln() / std::f64::consts::PI) / 2.0 * (1u64 << z) as f64
}

pub(super) fn load_tiles(rt: &tokio::runtime::Runtime, path: &Path) -> Vec<(TileKey, Bytes)> {
    let data = Bytes::from(fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())));
    rt.block_on(async {
        let r = pmtiles::AsyncPmTilesReader::try_from_source(MemBackend(data)).await.expect("pmtiles header");
        let h = r.get_header();
        let mut out = Vec::new();
        for z in h.min_zoom..=h.max_zoom {
            let (x0, x1) = (lon_to_x(h.min_longitude, z) as u32, lon_to_x(h.max_longitude, z) as u32);
            let (y0, y1) = (lat_to_y(h.max_latitude, z) as u32, lat_to_y(h.min_latitude, z) as u32);
            for x in x0..=x1 {
                for y in y0..=y1 {
                    let c = pmtiles::TileCoord::new(z, x, y).unwrap();
                    if let Some(b) = r.get_tile(c).await.expect("tile") {
                        out.push((TileKey::new(z, x, y), b));
                    }
                }
            }
        }
        out
    })
}

/// Tile keys in the order an autopilot flight north from the test-bench origin first wants
/// them: camera steps of 100 m, at each step every tile whose center lies inside the band
/// radius (R0 z16 0.6 km ... R3 z13 25 km), finer band first; each key once. Only keys present
/// in `have`.
pub(super) fn flight_order(have: &HashMap<TileKey, usize>) -> Vec<TileKey> {
    const ORIGIN: (f64, f64) = (54.6872, 25.2797);
    const BANDS: [(u8, f64); 4] = [(16, 600.0), (15, 2500.0), (14, 7000.0), (13, 25000.0)];
    const M_PER_DEG_LAT: f64 = 111_320.0;
    let m_per_deg_lon = M_PER_DEG_LAT * ORIGIN.0.to_radians().cos();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let max_lat = 54.92;
    let mut lat = ORIGIN.0;
    while lat <= max_lat {
        for (z, r) in BANDS {
            let n = (1u64 << z) as f64;
            let dlat = r / M_PER_DEG_LAT;
            let dlon = r / m_per_deg_lon;
            let (x0, x1) = (lon_to_x(ORIGIN.1 - dlon, z) as u32, lon_to_x(ORIGIN.1 + dlon, z) as u32);
            let (y0, y1) = (lat_to_y(lat + dlat, z) as u32, lat_to_y(lat - dlat, z) as u32);
            let mut ring = Vec::new();
            for x in x0..=x1 {
                for y in y0..=y1 {
                    let clon = (x as f64 + 0.5) / n * 360.0 - 180.0;
                    let cy = (y as f64 + 0.5) / n;
                    let clat = (std::f64::consts::PI * (1.0 - 2.0 * cy)).sinh().atan().to_degrees();
                    let d = (((clat - lat) * M_PER_DEG_LAT).powi(2) + ((clon - ORIGIN.1) * m_per_deg_lon).powi(2)).sqrt();
                    if d <= r {
                        ring.push((d, TileKey::new(z, x, y)));
                    }
                }
            }
            ring.sort_by(|a, b| a.0.total_cmp(&b.0));
            for (_, k) in ring {
                if have.contains_key(&k) && seen.insert(k) {
                    out.push(k);
                }
            }
        }
        lat += 100.0 / M_PER_DEG_LAT;
    }
    out
}
