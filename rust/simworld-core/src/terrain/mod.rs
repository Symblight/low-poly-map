//! Procedural terrain (M9 phase 1, `docs/spec-terrain.md` with the design change "procedural,
//! no data"): one pure, seeded height function for the whole world.
//!
//! * [`field_height`] is the function: `(FieldParams, lat, lon) -> meters above MSL`. No state,
//!   no cache, no I/O. It samples 3D integer-hash value noise ([`noise`]) on a sphere of the mean
//!   Earth radius, so it is defined everywhere, isotropic, independent of the floating origin and
//!   of any tile grid, and bit-identical on every thread and in every call order (fixed f64
//!   operation sequence, no fused multiply-add, no reductions).
//! * Shapes on top of the fractal hills: ridged mountain chains inside a low-frequency mountain
//!   mask (`ridge_share`), plateaus with escarpments (`plateau_share`), meandering valleys cut
//!   along the zero lines of a low-frequency noise (`valley_share`; their floors reach the sea
//!   level and become flat river plains), and a sea level below which everything is flat.
//! * [`Terrain`] = field parameters + seed + the configuration's flat zone (a disk around the
//!   configure origin, blended smoothly into the relief, so a city centre or an airport stays
//!   level) + exaggeration / offset + the drape parameters of the vector world (drape margin,
//!   foundation depth, drape step, water snap). `Copy`, a few hundred bytes: every worker job
//!   gets its own copy, the main thread keeps one in `World` / `Scheduler`. With the terrain off
//!   ([`Terrain::flat`]) every height is exactly 0.0 and nothing is sampled.
//! * [`PatchSampler`] (`sample`): the heights of a tile on a regular grid plus one sample of
//!   border, with bilinear lookup, for the terrain-mesh (TIN) phase; and [`Terrain::tile_range`],
//!   the deterministic coarse min/max of a tile for the slant-distance boxes of the cover.
//! * [`tin`] (phase 2, the terrain-mesh layer): a vendored Delatin greedy-insertion TIN over a
//!   tile's patch, curvature seeds, skirts, optional smooth normals ([`tin::build_terrain_tile`]).
//!
//! Units: meters; local `y` = meters above mean sea level.

pub mod noise;
pub mod sample;
pub mod tin;

pub use sample::PatchSampler;

use crate::geo::TileKey;
use crate::profile::TerrainProfile;
use crate::rings::RING_COUNT;

/// Sphere radius of the noise domain (mean Earth radius, m). Not a geodesy constant: it only
/// fixes where the noise lives, so it must never change (it would move every hill).
const NOISE_RADIUS_M: f64 = 6_371_008.8;
/// Meters per degree of latitude on the noise sphere.
const M_PER_DEG: f64 = NOISE_RADIUS_M * std::f64::consts::PI / 180.0;

/// Valley floors pull the shape value towards this (below the sea line, so wide valleys end in
/// flat plains at `sea_level_m`).
const VALLEY_FLOOR: f64 = -0.55;
/// Plateau tops sit at this shape value and keep a tenth of the relief beneath them.
const PLATEAU_LEVEL: f64 = 0.45;
const PLATEAU_KEEP: f64 = 0.1;
/// Ridge contribution per unit of the ridged sum (range of `RIDGE_GAIN * r - RIDGE_BIAS`).
const RIDGE_GAIN: f64 = 1.6;
const RIDGE_BIAS: f64 = 0.3;

/// Parameters of the height field itself (profile `terrain.*`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldParams {
    pub seed: u64,
    /// Relief scale: heights are about `amplitude_m * (0.5 + s)` with the shape value `s`
    /// mostly within [-1, 1] (ridges reach above).
    pub amplitude_m: f64,
    /// Size of the largest hills (the first noise octave's lattice spacing), m.
    pub feature_size_m: f64,
    pub octaves: u32,
    pub ridge_share: f64,
    pub valley_share: f64,
    pub plateau_share: f64,
    /// Nothing is lower than this (flat sea, lakes and river plains), m.
    pub sea_level_m: f64,
}

impl FieldParams {
    pub fn from_profile(t: &TerrainProfile) -> Self {
        Self {
            seed: t.seed as u64,
            amplitude_m: t.amplitude_m,
            feature_size_m: t.feature_size_m,
            octaves: t.octaves,
            ridge_share: t.ridge_share,
            valley_share: t.valley_share,
            plateau_share: t.plateau_share,
            sea_level_m: t.sea_level_m,
        }
    }

    /// Bounds of [`field_height`] for these parameters: [min, max] (exact bounds of the formula,
    /// not of a particular world).
    pub fn bounds(&self) -> [f64; 2] {
        let s_max = 1.0 + (RIDGE_GAIN - RIDGE_BIAS) * self.ridge_share;
        let s_min = -1.0 - RIDGE_BIAS * self.ridge_share;
        [
            (self.amplitude_m * (0.5 + s_min)).max(self.sea_level_m),
            (self.amplitude_m * (0.5 + s_max)).max(self.sea_level_m),
        ]
    }

    fn sub_seed(&self, k: u64) -> u64 {
        crate::mvt::splitmix64(self.seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ k)
    }
}

/// Point on the noise sphere (m) for a WGS84 lat/lon (degrees).
#[inline]
pub fn sphere_point(lat: f64, lon: f64) -> [f64; 3] {
    let (sl, cl) = crate::math::sin_cos(lat.to_radians());
    let (so, co) = crate::math::sin_cos(lon.to_radians());
    [NOISE_RADIUS_M * cl * co, NOISE_RADIUS_M * cl * so, NOISE_RADIUS_M * sl]
}

/// THE height function (before the flat zone, exaggeration and offset): meters above MSL at a
/// WGS84 point. Pure: same parameters and point, same bits, on any thread.
pub fn field_height(p: &FieldParams, lat: f64, lon: f64) -> f64 {
    field_at(p, sphere_point(lat, lon))
}

/// [`field_height`] at a noise-sphere point.
pub fn field_at(p: &FieldParams, q: [f64; 3]) -> f64 {
    if p.amplitude_m <= 0.0 {
        return 0.0f64.max(p.sea_level_m); // what the formula gives with a zero amplitude
    }
    let inv = 1.0 / p.feature_size_m;
    let u = [q[0] * inv, q[1] * inv, q[2] * inv];
    let scaled = |k: f64| [u[0] * k, u[1] * k, u[2] * k];
    // Rolling hills.
    let mut s = noise::fbm(u, p.octaves, p.sub_seed(1));
    // Ridged mountain chains, only inside the mountain mask (skipped where it is 0).
    if p.ridge_share > 0.0 {
        let mask = noise::smoothstep(-0.2, 0.35, noise::value3(scaled(0.25), p.sub_seed(2)));
        if mask > 0.0 {
            let r = noise::ridged(scaled(0.8), p.octaves.min(6), p.sub_seed(3));
            s += p.ridge_share * mask * (RIDGE_GAIN * r - RIDGE_BIAS);
        }
    }
    // Plateaus: flat-topped highlands with escarpments at the mask edge.
    if p.plateau_share > 0.0 {
        let t = 1.0 - 1.2 * p.plateau_share;
        let m = noise::smoothstep(t, t + 0.12, noise::value3(scaled(0.3), p.sub_seed(4)));
        if m > 0.0 {
            let top = PLATEAU_LEVEL + PLATEAU_KEEP * (s - PLATEAU_LEVEL);
            s += (top - s) * m;
        }
    }
    // Valleys along the zero lines of a low-frequency noise (they cut plateaus too).
    if p.valley_share > 0.0 {
        let v = noise::value3(scaled(0.45), p.sub_seed(5)).abs();
        let w = 1.0 - noise::smoothstep(0.0, 0.14, v);
        if w > 0.0 {
            s += (VALLEY_FLOOR - s) * (p.valley_share * w);
        }
    }
    (p.amplitude_m * (0.5 + s)).max(p.sea_level_m)
}

/// Smallest vertical error the terrain mesh may keep inside a seed world's airport fence (m):
/// there the drawn ground has no drape margin, only its stacking level (contract section 9
/// "Airports"); the levelled fence is planar per strip, so the TIN reaches it cheaply.
pub const LIFT_FIELD_MIN_ERROR_M: f64 = 0.01;

/// The seed world's height modifiers (water rule, runway strips; api 1.7), shared by every copy
/// of a [`Terrain`]. Two hooks are equal when they are the same allocation.
#[derive(Clone, Debug)]
pub struct SeedHook(pub std::sync::Arc<crate::seed::terrain::SeedTerrain>);

impl PartialEq for SeedHook {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

/// The world's terrain for one configuration (see module docs). Cheap to clone (a few hundred
/// bytes plus, in a seed world, one `Arc`).
#[derive(Clone, Debug, PartialEq)]
pub struct Terrain {
    active: bool,
    field: FieldParams,
    /// Flat zone: noise-sphere center, radius and blend width (m), height inside (field height
    /// at the center, before exaggeration).
    flat_center: [f64; 3],
    flat_radius_m: f64,
    flat_blend_m: f64,
    flat_height: f64,
    exaggeration: f64,
    offset_m: f64,
    water_snap_m: f64,
    drape_step_m: f64,
    /// Drape margin per vector band (m; [`DRAPE_REFERENCE`]): ground vertices sit this much
    /// above the exact height, so a draped edge that dips under the surface by up to the margin
    /// still stays on or above it.
    margin_m: [f32; RING_COUNT],
    /// Foundation depth per vector band (m; [`DRAPE_REFERENCE`]).
    foundation_m: [f32; RING_COUNT],
    /// [min, max] of [`Terrain::height`] (exaggeration and offset applied).
    bounds: [f64; 2],
    /// World seed (api 1.7): water rule and runway strips on top of the field (`None` = the
    /// plain field, every non-seed world).
    seed: Option<SeedHook>,
}

impl Default for Terrain {
    fn default() -> Self {
        Self::flat()
    }
}

impl Terrain {
    /// Terrain off: every height is exactly 0, nothing is sampled, no margin, no foundations.
    pub const fn flat() -> Self {
        Self {
            active: false,
            field: FieldParams {
                seed: 0,
                amplitude_m: 0.0,
                feature_size_m: 1.0,
                octaves: 1,
                ridge_share: 0.0,
                valley_share: 0.0,
                plateau_share: 0.0,
                sea_level_m: 0.0,
            },
            flat_center: [0.0; 3],
            flat_radius_m: 0.0,
            flat_blend_m: 0.0,
            flat_height: 0.0,
            exaggeration: 1.0,
            offset_m: 0.0,
            water_snap_m: 0.0,
            drape_step_m: 25.0,
            margin_m: [0.0; RING_COUNT],
            foundation_m: [0.0; RING_COUNT],
            bounds: [0.0, 0.0],
            seed: None,
        }
    }

    /// Active terrain from the profile section `terrain` and the configure origin (center of the
    /// flat zone; re-centering does not move it). The drape margin and foundation depth come from
    /// the frozen [`DRAPE_REFERENCE`], not from the profile's bands.
    pub fn new(t: &TerrainProfile, origin_lat: f64, origin_lon: f64) -> Self {
        let field = FieldParams::from_profile(t);
        let flat_center = sphere_point(origin_lat, origin_lon);
        let flat_height = field_at(&field, flat_center);
        let [lo, hi] = field.bounds();
        let (ex, off) = (t.exaggeration, t.height_offset_m);
        Self {
            active: true,
            field,
            flat_center,
            flat_radius_m: t.flat_radius_m,
            flat_blend_m: t.flat_blend_m,
            flat_height,
            exaggeration: ex,
            offset_m: off,
            water_snap_m: t.water_snap_m,
            drape_step_m: t.drape_step_m,
            margin_m: DRAPE_REFERENCE.margin_m,
            foundation_m: DRAPE_REFERENCE.foundation_m,
            bounds: [lo * ex + off, hi * ex + off],
            seed: None,
        }
    }

    /// The same terrain with the seed world's water rule and runway strips (api 1.7). The flat
    /// zone must be off (a seed world validates `flat_radius_m` = `flat_blend_m` = 0).
    pub fn with_seed(mut self, seed: std::sync::Arc<crate::seed::terrain::SeedTerrain>) -> Self {
        if self.active {
            let sea = seed.water.params().sea_level_m;
            self.bounds[0] = self.bounds[0].min(sea);
            self.bounds[1] = self.bounds[1].max(sea);
            self.seed = Some(SeedHook(seed));
        }
        self
    }

    /// The seed world's height modifiers, if any.
    pub fn seed(&self) -> Option<&std::sync::Arc<crate::seed::terrain::SeedTerrain>> {
        self.seed.as_ref().map(|s| &s.0)
    }

    /// [`Terrain::height`] without the seed world's rules (the field with flat zone,
    /// exaggeration and offset): lake levels and strip thresholds are sampled from it.
    pub fn field_only_height(&self, lat: f64, lon: f64) -> f64 {
        if !self.active {
            return 0.0;
        }
        self.height_at_point(sphere_point(lat, lon))
    }

    /// [`Terrain::new`] from a resolved profile.
    pub fn from_profile(p: &crate::profile::Profile, origin_lat: f64, origin_lon: f64) -> Self {
        Self::new(&p.terrain, origin_lat, origin_lon)
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn field(&self) -> &FieldParams {
        &self.field
    }

    /// Terrain height (m above MSL) at a WGS84 point: field, flat zone, exaggeration, offset.
    /// Exactly 0.0 when the terrain is off. A few hundred ns.
    #[inline]
    pub fn height(&self, lat: f64, lon: f64) -> f64 {
        if !self.active {
            return 0.0;
        }
        if let Some(s) = &self.seed {
            return s.0.height(lat, lon, || self.height_at_point(sphere_point(lat, lon)));
        }
        self.height_at_point(sphere_point(lat, lon))
    }

    fn height_at_point(&self, q: [f64; 3]) -> f64 {
        let mut h = field_at(&self.field, q);
        if self.flat_radius_m > 0.0 || self.flat_blend_m > 0.0 {
            let d = [q[0] - self.flat_center[0], q[1] - self.flat_center[1], q[2] - self.flat_center[2]];
            let dist = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            let w = noise::smoothstep(self.flat_radius_m, self.flat_radius_m + self.flat_blend_m, dist);
            h = self.flat_height + (h - self.flat_height) * w;
        }
        h * self.exaggeration + self.offset_m
    }

    /// Height for a water-area vertex: the minimum over a 5 x 5 window of half-width
    /// `water_snap_m` centred on the point (window spacing `water_snap_m / 2`; meters converted
    /// to degrees at the point's latitude), so a lake outline that samples the bank does not tilt
    /// the water up. Per vertex, so a vertex shared by two tiles gets the same height in both.
    /// Plain [`Terrain::height`] when `water_snap_m` is 0 or the terrain is off.
    pub fn water_height(&self, lat: f64, lon: f64) -> f64 {
        if !self.active || self.water_snap_m <= 0.0 {
            return self.height(lat, lon);
        }
        let step = self.water_snap_m * 0.5;
        let dlat = step / M_PER_DEG;
        let dlon = step / (M_PER_DEG * crate::math::cos(lat.to_radians()).max(1e-6));
        let mut lo = f64::INFINITY;
        for j in -2i32..=2 {
            for i in -2i32..=2 {
                lo = lo.min(self.height(lat + j as f64 * dlat, lon + i as f64 * dlon));
            }
        }
        lo
    }

    /// [min, max] of every height this terrain can return (0, 0 when off).
    pub fn bounds(&self) -> [f64; 2] {
        self.bounds
    }

    /// Highest possible terrain height (m; 0 when off). Used to prune root candidates.
    pub fn max_height(&self) -> f64 {
        self.bounds[1]
    }

    /// Drape margin of the vector band `ring` (0 when off; [`DRAPE_REFERENCE`]): ground
    /// vertices lie at `height + weight x margin + level x step`. Not a lift over the terrain
    /// mesh: the terrain mesh is sunk under the height function instead (contract section 9a),
    /// so the vector world never depends on the terrain LOD.
    pub fn drape_margin_m(&self, ring: usize) -> f32 {
        if self.active {
            self.margin_m.get(ring).copied().unwrap_or(0.0)
        } else {
            0.0
        }
    }

    /// Drape weight at a point (contract section 9 "Airports", api 1.7 S1 fix): 0 inside a
    /// seed world's airport fence, smoothstep to 1 over `seed.airport.shoulder_m` outside it
    /// (the strip rule's own blend), 1 everywhere else and in every non-seed world. The drawn
    /// ground lies at `height + weight x drape_margin_m(ring) + level x step`, and the terrain
    /// mesh's error bound scales by the same weight, so the TIN is exact on a levelled fence.
    #[inline]
    pub fn lift_weight(&self, lat: f64, lon: f64) -> f64 {
        match &self.seed {
            Some(s) if self.active => s.0.strips.lift_weight(lat, lon),
            _ => 1.0,
        }
    }

    /// Whether [`Terrain::lift_weight`] can be below 1 anywhere in tile `key` (a seed world's
    /// airport fence or shoulder overlaps it). False for every non-seed world: then the drape and
    /// the terrain mesh take their plain (pre-1.7) path, bit for bit.
    pub fn lift_field_touches(&self, key: TileKey) -> bool {
        match &self.seed {
            Some(s) if self.active && !s.0.strips.is_empty() => {
                let (n, w) = key.frac_to_lat_lon(0.0, 0.0);
                let (so, e) = key.frac_to_lat_lon(1.0, 1.0);
                s.0.strips.touches([w, so, e, n])
            }
            _ => false,
        }
    }

    /// Foundation depth of buildings in band `ring` ([`DRAPE_REFERENCE`]): every wall of a part
    /// standing on the ground reaches this far below the exact ground at its vertices (0 when
    /// off: no foundation quads).
    pub fn foundation_m(&self, ring: usize) -> f32 {
        if self.active {
            self.foundation_m.get(ring).copied().unwrap_or(0.0)
        } else {
            0.0
        }
    }

    /// Ground edges longer than this are subdivided where the surface is not flat.
    pub fn drape_step_m(&self) -> f64 {
        self.drape_step_m
    }

    pub fn water_snap_m(&self) -> f64 {
        self.water_snap_m
    }

    /// Deterministic coarse [min, max] of the terrain over a tile, for slant-distance boxes:
    /// a 9 x 9 grid on the tile (corners and edges included, on the global z+3 lattice, so
    /// neighbours share their edge samples) padded by half the largest difference between
    /// neighbouring samples, clamped to [`Terrain::bounds`]. A pure function of the key. `[0, 0]`
    /// when off (no sampling).
    pub fn tile_range(&self, key: TileKey) -> [f64; 2] {
        if !self.active {
            return [0.0, 0.0];
        }
        const N: u32 = 8;
        let mut h = [[0.0f64; (N + 1) as usize]; (N + 1) as usize];
        for (j, row) in h.iter_mut().enumerate() {
            for (i, v) in row.iter_mut().enumerate() {
                let (lat, lon) = sample::lattice_lat_lon(key, N, i as i64, j as i64);
                *v = self.height(lat, lon);
            }
        }
        let (mut lo, mut hi, mut step) = (f64::INFINITY, f64::NEG_INFINITY, 0.0f64);
        for j in 0..=N as usize {
            for i in 0..=N as usize {
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
        let pad = 0.5 * step;
        [(lo - pad).max(self.bounds[0]), (hi + pad).min(self.bounds[1])]
    }

    /// Heights of a tile on an `n` x `n` grid (corners included) plus one sample of border on
    /// every side (`n >= 2`), for the terrain mesh. See [`PatchSampler`].
    pub fn patch(&self, key: TileKey, n: usize) -> PatchSampler {
        PatchSampler::new(self, key, n)
    }
}

/// The drape and foundation table of the vector world (frozen 2026-10-28, Tiles v2 + Terrain v2
/// step P0; `docs/spec-terrain.md` "Design change: drape frozen"). Until then both rows were
/// derived at configure time from the terrain bands' `max_error_m`, `terrain.detail_scale_max`
/// and the vector band edges x hysteresis ([`reference::band_error_rule`]); those inputs are
/// about to change meaning (screen-space-error bands), so the drape no longer reads them. The
/// values are exactly what the 1.7 rules gave with the default profile, so default output is
/// bit for bit unchanged (golden hashes). A profile override of those keys no longer moves the
/// drawn ground, the lights or the foundations. Index = vector band R0..R3.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DrapeReference {
    /// Drape margin per vector band (m): a quarter of the 1.7 band error at detail 1, at least
    /// [`crate::ground::DRAPE_FLAT_TOLERANCE_M`]. Ground vertices sit at
    /// `height + weight x margin + level x step`; draped edges are subdivided once the height
    /// along them rises more than the margin above the straight edge (or falls more than twice
    /// it below), so the drawn ground is never under the exact height function at the tested
    /// points and at most `3 x margin` above it, plus the stacking level. The terrain mesh is
    /// sunk under the height function at every detail (contract section 9a), so the margin does
    /// not have to cover any terrain-mesh error.
    pub margin_m: [f32; RING_COUNT],
    /// Foundation depth per vector band (m): `2 x` the 1.7 band error at `detail_scale_max` 2,
    /// `+ 1 m`: walls of a ground-standing part reach this far under the exact ground, so the
    /// sunk terrain mesh (error + sink, each at most the band error) does not open a gap under a
    /// building at the default terrain bands.
    pub foundation_m: [f32; RING_COUNT],
}

/// The frozen table (see [`DrapeReference`]): margins R0 0.125, R1 0.375, R2 1, R3 2.5 m;
/// foundations R0 3, R1 7, R2 17, R3 41 m.
pub const DRAPE_REFERENCE: DrapeReference =
    DrapeReference { margin_m: [0.125, 0.375, 1.0, 2.5], foundation_m: [3.0, 7.0, 17.0, 41.0] };

/// The 1.7 derivation of [`DRAPE_REFERENCE`] from live profile data, kept only so a test can show
/// that the frozen table equals it at the default profile (provenance). Not used by the library.
#[cfg(test)]
pub(crate) mod reference {
    use crate::profile::TerrainBand;
    use crate::rings::{BandParams, RING_COUNT};

    /// Worst terrain-mesh error that could be on screen in vector band N (the old "lift rule",
    /// `docs/spec-terrain.md` 6.2): the largest `max_error_m * detail` of every terrain band
    /// whose slant-distance range `[previous edge, edge]` intersects `[0, rN_edge * hysteresis]`.
    pub fn band_error_rule(bands: &[TerrainBand], detail: f64, vector: &BandParams) -> [f32; RING_COUNT] {
        std::array::from_fn(|ring| {
            let reach = vector.edge_m[ring] * vector.hysteresis;
            let mut inner = 0.0;
            let mut err = 0.0f64;
            for b in bands {
                if inner < reach {
                    err = err.max(b.max_error_m * detail);
                }
                inner = b.edge_m;
            }
            err as f32
        })
    }

    /// 1.7 drape margin: a quarter of [`band_error_rule`] at detail 1, at least 0.1 m.
    pub fn drape_margin_rule(bands: &[TerrainBand], vector: &BandParams) -> [f32; RING_COUNT] {
        band_error_rule(bands, 1.0, vector).map(|e| (e * 0.25).max(crate::ground::DRAPE_FLAT_TOLERANCE_M as f32))
    }

    /// 1.7 foundation depth: `2 x band_error_rule(detail_scale_max) + 1 m`.
    pub fn foundation_rule(bands: &[TerrainBand], detail_scale_max: f64, vector: &BandParams) -> [f32; RING_COUNT] {
        band_error_rule(bands, detail_scale_max, vector).map(|e| 2.0 * e + 1.0)
    }
}

#[cfg(test)]
mod tests;
