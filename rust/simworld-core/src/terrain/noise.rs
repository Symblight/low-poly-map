//! Integer-hash value noise in 3D (f64), the building block of the procedural terrain field.
//!
//! Deterministic by construction: lattice values come from a 64-bit integer hash of the lattice
//! coordinates and a seed (no tables, no process-random state), and every evaluation is a fixed
//! sequence of f64 operations (no fused multiply-add, no parallel reductions), so the same input
//! gives the same bits on every thread and in every call order.

use crate::mvt::splitmix64;

/// Value at an integer lattice point, uniform in [-1, 1).
#[inline]
fn lattice(ix: i64, iy: i64, iz: i64, seed: u64) -> f64 {
    let h = seed
        ^ (ix as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (iy as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ (iz as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
    let h = splitmix64(h);
    (h >> 11) as f64 * (2.0 / (1u64 << 53) as f64) - 1.0
}

/// Quintic fade `6t^5 - 15t^4 + 10t^3` (C2: no creases at lattice planes).
#[inline]
fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

#[inline]
fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// Smooth value noise at `p` (lattice units), in [-1, 1].
pub fn value3(p: [f64; 3], seed: u64) -> f64 {
    let (fx, fy, fz) = (p[0].floor(), p[1].floor(), p[2].floor());
    let (ix, iy, iz) = (fx as i64, fy as i64, fz as i64);
    let (tx, ty, tz) = (fade(p[0] - fx), fade(p[1] - fy), fade(p[2] - fz));
    let c = |dx: i64, dy: i64, dz: i64| lattice(ix + dx, iy + dy, iz + dz, seed);
    let x00 = lerp(c(0, 0, 0), c(1, 0, 0), tx);
    let x10 = lerp(c(0, 1, 0), c(1, 1, 0), tx);
    let x01 = lerp(c(0, 0, 1), c(1, 0, 1), tx);
    let x11 = lerp(c(0, 1, 1), c(1, 1, 1), tx);
    lerp(lerp(x00, x10, ty), lerp(x01, x11, ty), tz)
}

/// Per-octave seed and lattice offset (the offsets keep the octaves' lattice planes apart, so
/// their flat spots at lattice points never line up).
/// The literals are pinned by the height goldens: they are not meant as pi / 10 and pi / 4 (the
/// lint's guess), and swapping in `FRAC_PI_4` would change the last bits of every height.
#[inline]
#[allow(clippy::approx_constant)]
fn octave(seed: u64, o: u32) -> (u64, [f64; 3]) {
    let s = splitmix64(seed ^ (o as u64 + 1).wrapping_mul(0xD1B5_4A32_D192_ED03));
    let k = (o + 1) as f64;
    (s, [0.569_840_290_998 * k, 0.314_159_265_359 * k, 0.785_398_163_397 * k])
}

/// Fractal sum of `octaves` octaves (lacunarity 2, gain 0.5), normalized to [-1, 1].
pub fn fbm(p: [f64; 3], octaves: u32, seed: u64) -> f64 {
    let (mut sum, mut norm, mut amp, mut freq) = (0.0, 0.0, 1.0, 1.0);
    for o in 0..octaves.max(1) {
        let (s, off) = octave(seed, o);
        sum += amp * value3([p[0] * freq + off[0], p[1] * freq + off[1], p[2] * freq + off[2]], s);
        norm += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    sum / norm
}

/// Ridged fractal sum: `(1 - |v|)^2` per octave, normalized to [0, 1] (sharp crests where the
/// underlying noise crosses zero).
pub fn ridged(p: [f64; 3], octaves: u32, seed: u64) -> f64 {
    let (mut sum, mut norm, mut amp, mut freq) = (0.0, 0.0, 1.0, 1.0);
    for o in 0..octaves.max(1) {
        let (s, off) = octave(seed, o);
        let v = 1.0 - value3([p[0] * freq + off[0], p[1] * freq + off[1], p[2] * freq + off[2]], s).abs();
        sum += amp * v * v;
        norm += amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    sum / norm
}

/// Cubic smoothstep (0 below `e0`, 1 above `e1`; a step when `e0 == e1`).
#[inline]
pub fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    if e1 <= e0 {
        return if x < e0 { 0.0 } else { 1.0 };
    }
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_noise_range_continuity_and_lattice_values() {
        let mut lo = f64::MAX;
        let mut hi = f64::MIN;
        for i in 0..2000 {
            let p = [i as f64 * 0.137 - 50.0, i as f64 * 0.071 + 3.0, -(i as f64) * 0.093];
            let v = value3(p, 7);
            lo = lo.min(v);
            hi = hi.max(v);
            // Continuity: a tiny step changes the value a little.
            let w = value3([p[0] + 1e-7, p[1], p[2]], 7);
            assert!((v - w).abs() < 1e-5);
        }
        assert!(lo >= -1.0 && hi <= 1.0 && lo < -0.5 && hi > 0.5, "{lo} {hi}");
        // At a lattice point the noise is the lattice value.
        assert_eq!(value3([3.0, -2.0, 5.0], 9), lattice(3, -2, 5, 9));
        // Seeds decorrelate.
        assert_ne!(value3([0.3, 0.4, 0.5], 1), value3([0.3, 0.4, 0.5], 2));
    }

    #[test]
    fn fractal_sums_stay_normalized() {
        for i in 0..500 {
            let p = [i as f64 * 0.37, i as f64 * 0.11, 1.0 - i as f64 * 0.23];
            let f = fbm(p, 6, 3);
            let r = ridged(p, 6, 3);
            assert!((-1.0..=1.0).contains(&f) && (0.0..=1.0).contains(&r));
        }
        assert_eq!(smoothstep(0.0, 1.0, 0.5), 0.5);
        assert_eq!(smoothstep(1.0, 1.0, 0.5), 0.0);
        assert_eq!(smoothstep(1.0, 1.0, 1.0), 1.0);
    }
}
