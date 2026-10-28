//! Grid sampling of the terrain over a tile: [`PatchSampler`] (input of the terrain-mesh phase)
//! and the global sample lattice it shares with [`super::Terrain::tile_range`].
//!
//! Lattice: a tile `(z, x, y)` sampled with `cells` cells per side has its samples at global
//! lattice indices `gx = x * cells + i`, `gy = y * cells + j` (zoom z, `cells` per tile), and a
//! sample's position is computed from that integer pair alone. Two tiles of the same zoom and
//! `cells` therefore compute their shared edge samples (and each other's border samples) from
//! the same integers: identical lat/lon, identical heights, bit for bit.

use crate::geo::{tile_x_to_lon, tile_y_to_lat, TileKey};

use super::height::Probe;
use super::Terrain;

/// The 9 x 9 probe of `key` (81 height evaluations on the global z+3 lattice, corners and edges
/// included, so neighbours share their edge samples): lowest and highest sample and the largest
/// difference between neighbouring samples. Feeds [`Terrain::tile_range`] and the error proxy
/// (`height::ErrorProxy`); pure in terrain and key, so it may run on any thread.
pub fn probe(terrain: &Terrain, key: TileKey) -> Probe {
    const P: usize = 8;
    let mut h = [[0.0f64; P + 1]; P + 1];
    for (j, row) in h.iter_mut().enumerate() {
        for (i, v) in row.iter_mut().enumerate() {
            let (lat, lon) = lattice_lat_lon(key, P as u32, i as i64, j as i64);
            *v = terrain.height(lat, lon);
        }
    }
    let (mut lo, mut hi, mut step) = (f64::INFINITY, f64::NEG_INFINITY, 0.0f64);
    for j in 0..=P {
        for i in 0..=P {
            let v = h[j][i];
            lo = lo.min(v);
            hi = hi.max(v);
            if i > 0 {
                step = step.max((v - h[j][i - 1]).abs());
            }
            if j > 0 {
                step = step.max((v - h[j - 1][i]).abs());
            }
        }
    }
    Probe { lo, hi, step }
}

/// WGS84 lat/lon of lattice sample `(i, j)` of `key` with `cells` cells per tile side (`i`, `j`
/// may lie outside `0..=cells` for border samples).
pub fn lattice_lat_lon(key: TileKey, cells: u32, i: i64, j: i64) -> (f64, f64) {
    let c = cells.max(1) as i64;
    let gx = key.x as i64 * c + i;
    let gy = key.y as i64 * c + j;
    (tile_y_to_lat(gy as f64 / c as f64, key.z), tile_x_to_lon(gx as f64 / c as f64, key.z))
}

/// [`lattice_lat_lon`] at fractional lattice indices (e.g. a TIN triangle's centroid).
pub fn lattice_lat_lon_f(key: TileKey, cells: u32, fi: f64, fj: f64) -> (f64, f64) {
    let c = cells.max(1) as f64;
    let gx = key.x as f64 * c + fi;
    let gy = key.y as f64 * c + fj;
    (tile_y_to_lat(gy / c, key.z), tile_x_to_lon(gx / c, key.z))
}

/// Terrain heights of one tile on an `n` x `n` grid (corners included, `n - 1` cells per side)
/// plus one sample of border on every side, sampled once from [`Terrain::height`]. Positions
/// are tile fractions (`fx` east, `fy` south, 0..1 inside the tile, like `TileKey::frac_to_lat_lon`).
/// Bilinear in between. The border samples are the neighbours' first interior samples (same
/// lattice), so a mesher can evaluate errors and normals at the tile edge without seams.
#[derive(Clone, Debug, PartialEq)]
pub struct PatchSampler {
    key: TileKey,
    /// Cells per side (`n - 1`).
    cells: usize,
    /// Row-major `(n + 2)^2` heights, row 0 / column 0 = the border at index -1.
    heights: Vec<f64>,
    /// [min, max] of the interior samples (border excluded).
    range: [f64; 2],
}

impl PatchSampler {
    /// Sample `terrain` over `key` (`n` >= 2 samples per side, corners included).
    pub fn new(terrain: &Terrain, key: TileKey, n: usize) -> Self {
        let cells = n.max(2) - 1;
        let side = cells + 3;
        let mut heights = Vec::with_capacity(side * side);
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for j in -1..=(cells as i64 + 1) {
            for i in -1..=(cells as i64 + 1) {
                let (lat, lon) = lattice_lat_lon(key, cells as u32, i, j);
                let h = terrain.height(lat, lon);
                if (0..=cells as i64).contains(&i) && (0..=cells as i64).contains(&j) {
                    lo = lo.min(h);
                    hi = hi.max(h);
                }
                heights.push(h);
            }
        }
        Self { key, cells, heights, range: [lo, hi] }
    }

    pub fn key(&self) -> TileKey {
        self.key
    }

    /// Samples per side inside the tile (corners included).
    pub fn n(&self) -> usize {
        self.cells + 1
    }

    /// Cells per side.
    pub fn cells(&self) -> usize {
        self.cells
    }

    /// Tile fraction of sample index `i` (column or row; -1 and `n` are the border).
    pub fn frac(&self, i: i64) -> f64 {
        i as f64 / self.cells as f64
    }

    /// Height of sample `(i, j)`, `i`, `j` in `-1..=n` (column east, row south).
    pub fn get(&self, i: i64, j: i64) -> f64 {
        let side = self.cells as i64 + 3;
        let (ci, cj) = ((i + 1).clamp(0, side - 1), (j + 1).clamp(0, side - 1));
        self.heights[(cj * side + ci) as usize]
    }

    /// [min, max] of the interior samples (the tile's box for the cover once meshed).
    pub fn range(&self) -> [f64; 2] {
        self.range
    }

    /// Bilinear height at tile fraction `(fx, fy)` (clamped to the patch incl. its border).
    pub fn sample_frac(&self, fx: f64, fy: f64) -> f64 {
        let c = self.cells as f64;
        let gx = (fx * c).clamp(-1.0, c + 1.0);
        let gy = (fy * c).clamp(-1.0, c + 1.0);
        let (x0, y0) = (gx.floor().min(c), gy.floor().min(c));
        let (tx, ty) = (gx - x0, gy - y0);
        let (i, j) = (x0 as i64, y0 as i64);
        let h00 = self.get(i, j);
        let h10 = self.get(i + 1, j);
        let h01 = self.get(i, j + 1);
        let h11 = self.get(i + 1, j + 1);
        let top = h00 + (h10 - h00) * tx;
        let bottom = h01 + (h11 - h01) * tx;
        top + (bottom - top) * ty
    }

    /// Bilinear height at a tile pixel of an `extent`-pixel tile (MVT space).
    pub fn sample_px(&self, px: [f64; 2], extent: f64) -> f64 {
        self.sample_frac(px[0] / extent, px[1] / extent)
    }
}
