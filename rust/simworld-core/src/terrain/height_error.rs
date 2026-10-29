//! Error proxy: the pure per-key bound of the terrain-mesh error the SSE cover refines by.

use crate::gen_cache::GenCache;

use crate::geo::{tile_width_m, TileKey};
use crate::terrain::field::{PLATEAU_LEVEL, RIDGE_GAIN, VALLEY_FLOOR};
use crate::terrain::{sample, Terrain};

use super::{parent_key, GRID_QUADS};

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

    /// The 9 x 9 probe of `key` (81 height evaluations; [`sample::probe`]).
    pub fn probe(&self, key: TileKey) -> Probe {
        sample::probe(&self.terrain, key)
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
