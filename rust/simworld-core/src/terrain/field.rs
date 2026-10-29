//! The height field: fractal hills plus ridged mountain chains, plateaus and valleys on the
//! noise sphere ([`field_height`], [`FieldParams`]).

use super::{noise, NOISE_RADIUS_M};
use crate::profile::TerrainProfile;

/// Valley floors pull the shape value towards this (below the sea line, so wide valleys end in
/// flat plains at `sea_level_m`).
pub(super) const VALLEY_FLOOR: f64 = -0.55;
/// Plateau tops sit at this shape value and keep a tenth of the relief beneath them.
pub(super) const PLATEAU_LEVEL: f64 = 0.45;
const PLATEAU_KEEP: f64 = 0.1;
/// Ridge contribution per unit of the ridged sum (range of `RIDGE_GAIN * r - RIDGE_BIAS`).
pub(super) const RIDGE_GAIN: f64 = 1.6;
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
