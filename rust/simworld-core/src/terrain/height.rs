//! Terrain v2 step G1: [`HeightTile`] (one regular height grid per terrain tile) and the pure
//! per-key error proxy [`ErrorProxy`] the SSE cover (G2) will refine by.
//!
//! Wired since G2: the terrain layer's cover refines by [`ErrorProxy`] (`scheduler`); the
//! HeightTile is not used yet (render path G3). `ground_height_at`, the drape and building bases stay on the exact height function
//! ([`Terrain::height`]); a HeightTile is for drawing (R16 texture layer) and collision.
//!
//! # Lattice
//!
//! A tile `t/z/x/y` has [`GRID_QUADS`] = N quads per side, so N + 1 samples per side with the
//! tile edges included, plus one border sample one step outside every edge: [`GRID_SIDE`] =
//! N + 3 samples per side, index `i`, `j` in `-1..=N + 1` (column east, row south). Sample
//! `(i, j)` lies at the global lattice index `(x * N + i, y * N + j)` of zoom `z`, i.e. at
//! `tile_x_to_lon(gx / N, z)`, `tile_y_to_lat(gy / N, z)` (the same lattice as
//! [`super::sample::lattice_lat_lon`] with `cells = N`). Because N and the zoom steps are powers
//! of two, `gx / N / 2^z` is exact in f64, so a child's even interior sample `(2a, 2b)` computes
//! exactly the same lat/lon (and therefore the same height bits) as the parent's sample
//! `(ox + a, oy + b)` with `ox`, `oy` = 0 or N / 2 by quadrant: 33 x 33 samples are shared and
//! copied, not evaluated. A border sample is its neighbour's first interior sample (same
//! lattice), bit for bit.
//!
//! # Quantization
//!
//! Next to the f64 grid every tile carries an R16 copy with ONE world-wide quantum
//! ([`HeightQuant::for_terrain`]: offset = lowest possible height, quantum = height span / 65535
//! from [`Terrain::bounds`]), so neighbouring tiles quantize a shared sample identically and the
//! GPU decodes every layer with the same two numbers. Default terrain: 967.5 m span -> 1.48 cm.

use crate::budget::{MemoryBudget, HEIGHT_GRID_LEN};
use crate::geo::{tile_x_to_lon, tile_y_to_lat, TileKey};
use crate::math;

use super::{Terrain, NOISE_RADIUS_M};

/// Quads per side of a height tile (profile-independent for now; `terrain.grid_quads` later).
pub const GRID_QUADS: usize = 64;
/// Samples per side: N + 1 inside (edges shared with the neighbours) + one border each side.
pub const GRID_SIDE: usize = GRID_QUADS + 3;
/// Samples per tile.
pub const GRID_LEN: usize = GRID_SIDE * GRID_SIDE;
/// Samples a child shares with its parent (every second interior sample).
pub const SHARED_WITH_PARENT: usize = (GRID_QUADS / 2 + 1) * (GRID_QUADS / 2 + 1);

const _: () = assert!(GRID_LEN == HEIGHT_GRID_LEN, "budget height buffers must fit a height tile");

/// World-wide R16 quantization: `h = offset_m + v * quantum_m`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeightQuant {
    pub offset_m: f64,
    pub quantum_m: f64,
}

impl HeightQuant {
    /// From [`Terrain::bounds`] (the exact bounds of the formula incl. exaggeration, offset and
    /// the seed world's water levels): every height the terrain can return encodes without
    /// clamping. A flat terrain (span 0) gets quantum 1 m and encodes everything as 0.
    pub fn for_terrain(t: &Terrain) -> Self {
        let [lo, hi] = t.bounds();
        let span = hi - lo;
        let quantum_m = if span.is_finite() && span > 0.0 { span / 65535.0 } else { 1.0 };
        Self { offset_m: if lo.is_finite() { lo } else { 0.0 }, quantum_m }
    }

    #[inline]
    pub fn encode(&self, h: f64) -> u16 {
        ((h - self.offset_m) / self.quantum_m).round().clamp(0.0, 65535.0) as u16
    }

    #[inline]
    pub fn decode(&self, v: u16) -> f64 {
        self.offset_m + v as f64 * self.quantum_m
    }
}

/// The two buffers a [`HeightTile`] is built into. Workers get them filled-in-place: the main
/// thread takes them (the f64 grid from [`MemoryBudget::take_height_f64`]; the budget has no u16
/// pool yet, so the R16 buffer is a plain allocation the owner can recycle through
/// [`HeightTile::into_buffers`]) and hands them to the job with the key.
#[derive(Debug, Default)]
pub struct HeightBuffers {
    pub heights: Vec<f64>,
    pub r16: Vec<u16>,
}

impl HeightBuffers {
    /// Fresh buffers (tests, tools).
    pub fn new() -> Self {
        Self { heights: vec![0.0; GRID_LEN], r16: vec![0; GRID_LEN] }
    }

    /// The f64 grid from the budget's free list (no allocation in steady state) and an R16 buffer.
    pub fn from_budget(budget: &mut MemoryBudget, r16: Option<Vec<u16>>) -> Self {
        Self { heights: budget.take_height_f64(), r16: r16.unwrap_or_else(|| vec![0; GRID_LEN]) }
    }
}

/// One terrain tile's height grid (see module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct HeightTile {
    key: TileKey,
    /// Row-major [`GRID_SIDE`]^2 heights, row 0 / column 0 = the border at index -1.
    heights: Vec<f64>,
    /// The same samples quantized with `quant`.
    r16: Vec<u16>,
    quant: HeightQuant,
    /// [min, max] of the interior samples `0..=N` (border excluded).
    h_min: f64,
    h_max: f64,
    /// Samples copied from the parent (0 or [`SHARED_WITH_PARENT`]).
    copied: u32,
}

#[inline]
fn idx(i: i64, j: i64) -> usize {
    ((j + 1) as usize) * GRID_SIDE + (i + 1) as usize
}

/// The parent key of `key` (z > 0).
pub fn parent_key(key: TileKey) -> TileKey {
    TileKey::new(key.z - 1, key.x >> 1, key.y >> 1)
}

impl Terrain {
    /// [`Terrain::height`] with the noise-sphere point of `(lat, lon)` precomputed by the
    /// caller (bit-identical to `sphere_point(lat, lon)`): the same operations in the same order.
    #[inline]
    pub(crate) fn height_with_point(&self, lat: f64, lon: f64, q: [f64; 3]) -> f64 {
        if !self.active {
            return 0.0;
        }
        if let Some(s) = &self.seed {
            return s.0.height(lat, lon, || self.height_at_point(q));
        }
        self.height_at_point(q)
    }
}

impl HeightTile {
    /// Build the grid of `key` into `bufs`. `parent` (the resident tile of `key`'s parent, any
    /// quantization) donates its 33 x 33 shared samples; any other tile is ignored. Per row the
    /// latitude (`atan(sinh)`) and its `sin_cos`, per column the longitude and its `sin_cos` are
    /// computed once, so a sample costs only the height function itself; every sample equals
    /// `terrain.height(lattice_lat_lon(key, N, i, j))` bit for bit (`height_tests`).
    pub fn build(terrain: &Terrain, key: TileKey, quant: HeightQuant, parent: Option<&HeightTile>, bufs: HeightBuffers) -> Self {
        let HeightBuffers { mut heights, mut r16 } = bufs;
        if heights.len() != GRID_LEN {
            heights.resize(GRID_LEN, 0.0);
        }
        if r16.len() != GRID_LEN {
            r16.resize(GRID_LEN, 0);
        }
        let n = GRID_QUADS as i64;
        let c = n as f64;
        let parent = parent.filter(|p| key.z > 0 && p.key == parent_key(key));
        let (ox, oy) = ((key.x & 1) as i64 * n / 2, (key.y & 1) as i64 * n / 2);
        let active = terrain.is_active();
        // Per column: longitude and its sin / cos.
        let mut lon = [0.0f64; GRID_SIDE];
        let mut slon = [0.0f64; GRID_SIDE];
        let mut clon = [0.0f64; GRID_SIDE];
        if active {
            for ci in 0..GRID_SIDE {
                let gx = key.x as i64 * n + ci as i64 - 1;
                lon[ci] = tile_x_to_lon(gx as f64 / c, key.z);
                (slon[ci], clon[ci]) = math::sin_cos(lon[ci].to_radians());
            }
        }
        let mut copied = 0u32;
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for rj in 0..GRID_SIDE {
            let j = rj as i64 - 1;
            let row = &mut heights[rj * GRID_SIDE..(rj + 1) * GRID_SIDE];
            let (lat, rcl, rsl) = if active {
                let gy = key.y as i64 * n + j;
                let lat = tile_y_to_lat(gy as f64 / c, key.z);
                let (sl, cl) = math::sin_cos(lat.to_radians());
                // sphere_point: [R * cl * co, R * cl * so, R * sl], left to right.
                (lat, NOISE_RADIUS_M * cl, NOISE_RADIUS_M * sl)
            } else {
                (0.0, 0.0, 0.0)
            };
            let shared_row = parent.is_some() && j >= 0 && j <= n && j % 2 == 0;
            for (ci, h) in row.iter_mut().enumerate() {
                let i = ci as i64 - 1;
                *h = if shared_row && i >= 0 && i <= n && i % 2 == 0 {
                    copied += 1;
                    parent.map_or(0.0, |p| p.get(ox + i / 2, oy + j / 2))
                } else if active {
                    terrain.height_with_point(lat, lon[ci], [rcl * clon[ci], rcl * slon[ci], rsl])
                } else {
                    0.0
                };
                if (0..=n).contains(&i) && (0..=n).contains(&j) {
                    lo = lo.min(*h);
                    hi = hi.max(*h);
                }
            }
        }
        for (q, &h) in r16.iter_mut().zip(heights.iter()) {
            *q = quant.encode(h);
        }
        Self { key, heights, r16, quant, h_min: lo, h_max: hi, copied }
    }

    pub fn key(&self) -> TileKey {
        self.key
    }

    /// Height of sample `(i, j)`, `i`, `j` in `-1..=N + 1` (clamped).
    #[inline]
    pub fn get(&self, i: i64, j: i64) -> f64 {
        let m = GRID_QUADS as i64 + 1;
        self.heights[idx(i.clamp(-1, m), j.clamp(-1, m))]
    }

    /// The f64 grid, row-major, [`GRID_SIDE`]^2.
    pub fn heights(&self) -> &[f64] {
        &self.heights
    }

    /// The R16 grid, row-major, [`GRID_SIDE`]^2 (decode with [`HeightTile::quant`]).
    pub fn r16(&self) -> &[u16] {
        &self.r16
    }

    pub fn quant(&self) -> HeightQuant {
        self.quant
    }

    /// [min, max] of the interior samples.
    pub fn range(&self) -> [f64; 2] {
        [self.h_min, self.h_max]
    }

    /// Samples copied from the parent instead of evaluated.
    pub fn copied_samples(&self) -> u32 {
        self.copied
    }

    /// Bilinear height at tile fraction `(fx, fy)` in 0..=1 (the drawn grid surface, before
    /// quantization).
    pub fn sample_frac(&self, fx: f64, fy: f64) -> f64 {
        let c = GRID_QUADS as f64;
        let gx = (fx * c).clamp(0.0, c);
        let gy = (fy * c).clamp(0.0, c);
        let (x0, y0) = (gx.floor().min(c - 1.0), gy.floor().min(c - 1.0));
        let (tx, ty) = (gx - x0, gy - y0);
        let (i, j) = (x0 as i64, y0 as i64);
        let top = self.get(i, j) + (self.get(i + 1, j) - self.get(i, j)) * tx;
        let bottom = self.get(i, j + 1) + (self.get(i + 1, j + 1) - self.get(i, j + 1)) * tx;
        top + (bottom - top) * ty
    }

    /// Give the buffers back (to recycle them into the next build).
    pub fn into_buffers(self) -> HeightBuffers {
        HeightBuffers { heights: self.heights, r16: self.r16 }
    }

    /// Return the f64 grid to the budget's free list; the R16 buffer is handed back.
    pub fn release(self, budget: &mut MemoryBudget) -> Vec<u16> {
        budget.give_height_f64(self.heights);
        self.r16
    }
}

#[path = "height_error.rs"]
mod error;
pub use error::*;

#[cfg(test)]
#[path = "height_tests.rs"]
mod tests;
