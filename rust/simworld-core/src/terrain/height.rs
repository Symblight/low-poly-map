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

use crate::gen_cache::GenCache;

use crate::budget::{MemoryBudget, HEIGHT_GRID_LEN};
use crate::geo::{tile_width_m, tile_x_to_lon, tile_y_to_lat, TileKey};
use crate::math;

use super::{sample, Terrain, NOISE_RADIUS_M, PLATEAU_LEVEL, RIDGE_GAIN, VALLEY_FLOOR};

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

// ---------------------------------------------------------------------------------------------
// Error proxy
// ---------------------------------------------------------------------------------------------

/// Tiles touched by a seed-world edit (runway strips, water rule) coarser than this zoom get the
/// proxy "whole tile range" (an upper bound of any interpolation error), so the cover takes them
/// down to this zoom wherever their range is visible at all (the error floor of edited tiles).
pub const EDIT_MIN_ZOOM: u8 = 12;

/// Calibration (see [`ErrorProxy`]); fixed numbers, part of the proxy's definition (changing one
/// changes every SSE decision). Fitted against the brute-force error at 4 x density on 1497
/// tiles z10..16 (`height_tests::error_proxy_vs_measured`): measured / (step s / S) <= 1.01 on
/// unedited tiles and <= 2.04 on edited ones; measured / e_an <= 0.30.
const C_SMOOTH: f64 = 3.0;
const C_CREASE: f64 = 2.0;
const K_STEP: f64 = 1.25;
const K_AN: f64 = 0.4;
const K_EDIT: f64 = 3.0;

/// One spectral component of the field: amplitude (m), wavelength (m), crease or smooth.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Comp {
    amp: f64,
    lambda: f64,
    crease: bool,
}

impl Comp {
    /// Interpolation error bound at grid spacing `s`: smooth components fall with `s^2`
    /// (bilinear error = s^2 / 8 x curvature), creases with `s` (a slope jump J costs J s / 4);
    /// both saturate at the component's amplitude once it is unresolved.
    fn error(&self, s: f64) -> f64 {
        let r = s / self.lambda;
        let f = if self.crease { C_CREASE * r } else { C_SMOOTH * r * r };
        self.amp * f.min(1.0)
    }
}

/// Which seed-world edits can reach a tile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EditTouch {
    pub strips: bool,
    pub water: bool,
}

impl EditTouch {
    pub fn any(&self) -> bool {
        self.strips || self.water
    }
}

/// The coarse 9 x 9 probe of a tile (the lattice of [`Terrain::tile_range`], spacing `W / 8`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Probe {
    pub lo: f64,
    pub hi: f64,
    /// Largest difference between neighbouring probe samples.
    pub step: f64,
}

impl Probe {
    /// `hi - lo + step`: the probe range padded by half a step on both sides (as
    /// [`Terrain::tile_range`]); bilinear interpolation stays inside the sample range, so no
    /// interpolation error exceeds it.
    pub fn range(&self) -> f64 {
        self.hi - self.lo + self.step
    }
}

/// Pure, memoized estimate of the vertical error (m) of drawing tile `key` as a
/// [`GRID_QUADS`]-quad grid instead of its children (Terrain v2 section 3; decision G1: a pure
/// function of key + terrain + edit touches, not "measured parent / 2", so the cover never
/// depends on build order).
///
/// With `s` = grid spacing (`W / N`, `W` = tile ground width at the edge nearer the equator),
/// `S = W / 8` the probe spacing and the 9 x 9 probe (lo, hi, step):
///
/// ```text
/// R      = hi - lo + step                                (padded range, bounds any error)
/// e_loc  = K_STEP * step * s / S                         (local: 0 on flat sea / lakes / plains)
/// e_an   = sum_c amp_c min(1, C_SMOOTH (s/l_c)^2)        (smooth: fbm octaves, valleys, plateaus)
///        + sum_c amp_c min(1, C_CREASE s/l_c)            (creases: ridged octaves)
/// base   = min(e_loc, K_AN e_an)                         (the field is never rougher than e_an)
/// raw    = R                                             edited and z < EDIT_MIN_ZOOM
///        = min(R, max(base, K_EDIT step s / S))          edited (z >= EDIT_MIN_ZOOM)
///        = min(R, base)                                  otherwise
/// error  = min(raw(key), error(parent(key)))             below the root zoom
/// ```
///
/// Components come from the profile (`E` = amplitude x exaggeration): fbm octave k: amp
/// `E 0.5^k / norm`, wavelength `feature / 2^k`; ridged octave k: amp
/// `E ridge_share GAIN 0.5^k / norm`, wavelength `feature / (0.8 2^k)`; valleys: amp
/// `E valley_share (PLATEAU - FLOOR)`, wavelength 2 x the cut width; plateau escarpments: amp
/// `E plateau_share / 2`, wavelength 2 x the mask edge. "Edited" = [`ErrorProxy::edit_touch`]
/// (runway strip boxes, water index cells with water or a blend raster; hierarchical: a child
/// is edited only if its parent is).
///
/// Monotonic: the last line makes `error(child) <= error(parent)` by construction. In `raw`,
/// `e_an` is monotonic in `s` (halves to a quarter / half) and `R` shrinks to a sub-range; `e_loc`
/// comes from different probe points per zoom and is not monotonic in general (the property test
/// counts how often a raw child exceeds its raw parent). The clamp lowers a child only where its
/// parent's raw value was lower, and the edit floor makes every edited ancestor below
/// EDIT_MIN_ZOOM carry its full range.
///
/// Known limit: where two runway strips overlap, the strip rule steps ("first key wins",
/// `docs/spec-world-seed.md` 11c (8)); a step's error does not shrink with zoom, so no grid
/// level draws it within a fixed tolerance.
#[derive(Debug)]
pub struct ErrorProxy {
    terrain: Terrain,
    root_zoom: u8,
    comps: Vec<Comp>,
    memo: GenCache<TileKey, f64>,
    /// Apply the edit floors (default). Off for the TIN-path cover (G2, see
    /// [`ErrorProxy::without_edit_floor`]).
    edit_floor: bool,
}

/// Memo entries kept at most (a few MB; evicted gradually, [`GenCache`]).
const MEMO_CAP: usize = 1 << 16;

impl ErrorProxy {
    /// Proxy for `terrain` (copied), monotonic clamp from `root_zoom` down.
    pub fn new(terrain: &Terrain, root_zoom: u8) -> Self {
        let f = terrain.field();
        let e = f.amplitude_m * terrain.exaggeration;
        let mut comps = Vec::new();
        if terrain.is_active() && e > 0.0 {
            let oct = f.octaves.max(1);
            let norm: f64 = (0..oct).map(|k| 0.5f64.powi(k as i32)).sum();
            for k in 0..oct {
                let w = 0.5f64.powi(k as i32);
                comps.push(Comp { amp: e * w / norm, lambda: f.feature_size_m * w, crease: false });
            }
            if f.ridge_share > 0.0 {
                let oct_r = oct.min(6);
                let norm_r: f64 = (0..oct_r).map(|k| 0.5f64.powi(k as i32)).sum();
                for k in 0..oct_r {
                    let w = 0.5f64.powi(k as i32);
                    let amp = e * f.ridge_share * RIDGE_GAIN * w / norm_r;
                    comps.push(Comp { amp, lambda: f.feature_size_m * w / 0.8, crease: true });
                }
            }
            // Gradient scale of value noise: |dv/dx| <= 2 x 1.875 / lattice spacing.
            const GRAD: f64 = 3.75;
            if f.valley_share > 0.0 {
                let width = 0.14 / (GRAD / (f.feature_size_m / 0.45));
                comps.push(Comp { amp: e * f.valley_share * (PLATEAU_LEVEL - VALLEY_FLOOR), lambda: 2.0 * width, crease: false });
            }
            if f.plateau_share > 0.0 {
                let width = 0.12 / (GRAD / (f.feature_size_m / 0.3));
                comps.push(Comp { amp: e * f.plateau_share * 0.5, lambda: 2.0 * width, crease: false });
            }
        }
        Self { terrain: terrain.clone(), root_zoom, comps, memo: GenCache::new(MEMO_CAP), edit_floor: true }
    }

    /// The proxy without the edit floors: every tile takes the unedited formula
    /// (`min(R, base)`), edited or not. The terrain cover of the TIN render path uses it
    /// (Terrain v2 G2): the TIN of an edited tile is exact on a levelled airport fence
    /// (`lift_weight` scaling) and pins water vertices at their level, so an edit adds no drawn
    /// error beyond the band's `max_error_m` and needs no forced refinement; with the floors the
    /// two-cities seed world held about 2100..3200 terrain tiles instead of about 350. It also
    /// leaves the overlapping-strip height step (2.28 m, see the type docs) to the analytic cap,
    /// so it never drives refinement. The grid render path (G3) draws the 64-quad grid itself
    /// and switches the floors back on.
    pub fn without_edit_floor(mut self) -> Self {
        self.edit_floor = false;
        self.memo.clear();
        self
    }

    /// Whether the edit floors apply.
    pub fn edit_floor(&self) -> bool {
        self.edit_floor
    }

    /// [`ErrorProxy::new`] with the root zoom of a resolved profile (`terrain.far_zoom`).
    pub fn from_profile(terrain: &Terrain, p: &crate::profile::Profile) -> Self {
        Self::new(terrain, p.terrain.far_zoom.min(255) as u8)
    }

    /// Memo entries.
    pub fn len(&self) -> usize {
        self.memo.len()
    }

    pub fn is_empty(&self) -> bool {
        self.memo.is_empty()
    }

    /// The memoized, monotonic proxy (m).
    pub fn error_m(&mut self, key: TileKey) -> f64 {
        if let Some(e) = self.memo.get(&key) {
            return e;
        }
        let raw = self.raw_error_m(key);
        self.finish(key, raw)
    }

    /// [`ErrorProxy::error_m`] with the key's probe already taken (`probe` must be
    /// `self.probe(key)`; the scheduler shares it with [`Terrain::tile_range`]). Same value.
    pub fn error_m_probed(&mut self, key: TileKey, probe: &Probe) -> f64 {
        if let Some(e) = self.memo.get(&key) {
            return e;
        }
        let raw = self.raw_from_probe(key, probe);
        self.finish(key, raw)
    }

    /// Whether `key` is memoized.
    pub fn contains(&self, key: TileKey) -> bool {
        self.memo.contains(&key)
    }

    /// The terrain the proxy samples.
    pub fn terrain(&self) -> &Terrain {
        &self.terrain
    }

    fn finish(&mut self, key: TileKey, raw: f64) -> f64 {
        let e = if key.z > self.root_zoom && key.z > 0 { raw.min(self.error_m(parent_key(key))) } else { raw };
        self.memo.insert(key, e);
        e
    }

    /// Which seed edits reach `key` (both false outside a seed world).
    pub fn edit_touch(&self, key: TileKey) -> EditTouch {
        match self.terrain.seed() {
            Some(s) if self.terrain.is_active() => {
                EditTouch { strips: self.terrain.lift_field_touches(key), water: s.water.touches_tile(key) }
            }
            _ => EditTouch::default(),
        }
    }

    /// The 9 x 9 probe of `key` (81 height evaluations).
    pub fn probe(&self, key: TileKey) -> Probe {
        const P: usize = 8;
        let mut h = [[0.0f64; P + 1]; P + 1];
        for (j, row) in h.iter_mut().enumerate() {
            for (i, v) in row.iter_mut().enumerate() {
                let (lat, lon) = sample::lattice_lat_lon(key, P as u32, i as i64, j as i64);
                *v = self.terrain.height(lat, lon);
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

    /// Grid spacing (m) of `key`: ground width at the edge nearer the equator / N.
    pub fn spacing_m(key: TileKey) -> f64 {
        let (n, _) = key.frac_to_lat_lon(0.0, 0.0);
        let (s, _) = key.frac_to_lat_lon(1.0, 1.0);
        let lat = if n.signum() != s.signum() { 0.0 } else if n.abs() < s.abs() { n } else { s };
        tile_width_m(lat, key.z) / GRID_QUADS as f64
    }

    /// `e_an` of the type docs at grid spacing `s` (the profile-only part).
    pub fn analytic_m(&self, s: f64) -> f64 {
        self.comps.iter().map(|c| c.error(s)).sum()
    }

    /// The unclamped proxy of `key` (pure, not memoized; see the type docs).
    pub fn raw_error_m(&self, key: TileKey) -> f64 {
        if !self.terrain.is_active() {
            return 0.0;
        }
        self.raw_from_probe(key, &self.probe(key))
    }

    /// [`ErrorProxy::raw_error_m`] from the key's probe.
    fn raw_from_probe(&self, key: TileKey, pr: &Probe) -> f64 {
        if !self.terrain.is_active() {
            return 0.0;
        }
        let range = pr.range();
        let edited = self.edit_floor && self.edit_touch(key).any();
        if edited && key.z < EDIT_MIN_ZOOM {
            return range;
        }
        // s / S = 8 / N.
        let local = pr.step * 8.0 / GRID_QUADS as f64;
        let mut e = (K_STEP * local).min(K_AN * self.analytic_m(Self::spacing_m(key)));
        if edited {
            e = e.max(K_EDIT * local);
        }
        e.min(range).max(0.0)
    }
}

#[cfg(test)]
#[path = "height_tests.rs"]
mod tests;
