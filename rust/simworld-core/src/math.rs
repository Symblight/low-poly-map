//! Platform-independent transcendental functions (`docs/spec-world-seed.md` section 7, decided in
//! S0: `geo` -> `libm`).
//!
//! `f64::sin`, `ln`, `atan2`, `hypot`, `exp` ... call the platform maths library (Apple libm,
//! glibc, MSVC CRT, Bionic). Those are not required to be correctly rounded and do differ in the
//! last bits between platforms, so a position, a terrain height or a hash input computed with
//! them is not bit-identical between a Mac, an iPhone and a Linux CI runner. Everything in the
//! core that turns a coordinate into another coordinate or a height goes through this module
//! instead: the pure-Rust `libm` crate (a port of musl's libm) is the same code on every target.
//! Its only architecture-specific paths are `sqrt`, `fma` and `rint`, which are IEEE-exact
//! everywhere, so results are bit-identical on x86_64, aarch64 and wasm. Rust never contracts
//! `a * b + c` into an FMA on its own, so plain `+ - * / sqrt` stay exact as written.
//!
//! `to_radians` / `to_degrees` / `sqrt` / `abs` / `floor` stay on `f64` (pure IEEE operations).
//! The test `std_transcendentals_are_not_used` keeps `f64::sin` and friends out of the core's
//! non-test code.

/// sin(x), x in radians.
#[inline]
pub fn sin(x: f64) -> f64 {
    libm::sin(x)
}

/// cos(x), x in radians.
#[inline]
pub fn cos(x: f64) -> f64 {
    libm::cos(x)
}

/// (sin(x), cos(x)), x in radians.
#[inline]
pub fn sin_cos(x: f64) -> (f64, f64) {
    libm::sincos(x)
}

#[inline]
pub fn tan(x: f64) -> f64 {
    libm::tan(x)
}

#[inline]
pub fn atan(x: f64) -> f64 {
    libm::atan(x)
}

#[inline]
pub fn atan2(y: f64, x: f64) -> f64 {
    libm::atan2(y, x)
}

#[inline]
pub fn sinh(x: f64) -> f64 {
    libm::sinh(x)
}

/// Natural logarithm.
#[inline]
pub fn ln(x: f64) -> f64 {
    libm::log(x)
}

#[inline]
pub fn exp(x: f64) -> f64 {
    libm::exp(x)
}

/// sqrt(x² + y²) without undue overflow.
#[inline]
pub fn hypot(x: f64, y: f64) -> f64 {
    libm::hypot(x, y)
}

/// f32 [`hypot`].
#[inline]
pub fn hypotf(x: f32, y: f32) -> f32 {
    libm::hypotf(x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixed inputs -> fixed bits. These are the `libm` (musl) results; they must be identical on
    /// every CI target (x86_64, aarch64). A failure on one target means a platform maths call
    /// slipped in or `libm` changed its algorithms (then: regenerate goldens on purpose).
    #[test]
    fn libm_bits_are_pinned() {
        let lat = 54.6872f64.to_radians();
        let got = [
            sin(lat).to_bits(),
            cos(lat).to_bits(),
            tan(lat).to_bits(),
            ln(1.0 / cos(lat) + tan(lat)).to_bits(),
            atan(sinh(0.987_654_321)).to_bits(),
            atan2(0.3, -0.7).to_bits(),
            hypot(3.0e-3, 4.0e-3).to_bits(),
            exp(-0.25).to_bits(),
        ];
        let (s, c) = sin_cos(lat);
        assert_eq!((s.to_bits(), c.to_bits()), (got[0], got[1]), "sin_cos differs from sin / cos");
        let want: [u64; 8] = PINNED;
        if want != got {
            panic!("libm bits changed:\n  got  {got:#018x?}\n  want {want:#018x?}");
        }
    }

    const PINNED: [u64; 8] = [
        0x3fea_1cbd_ce37_a85b,
        0x3fe2_7f4d_9c11_a1ac,
        0x3ff6_963f_af66_549e,
        0x3ff2_50e8_cca9_5d01,
        0x3feb_7288_a2c5_e8a3,
        0x4005_e4c3_6ca0_118a,
        0x3f74_7ae1_47ae_147b,
        0x3fe8_ebef_9eac_820b,
    ];

    /// The core's non-test code never calls the platform transcendentals directly (see module
    /// docs); every call goes through this module. Test files and `#[cfg(test)]` blocks may.
    #[test]
    fn std_transcendentals_are_not_used() {
        const BANNED: &[&str] = &[
            ".sin()", ".cos()", ".tan()", ".sin_cos()", ".asin()", ".acos()", ".atan()", ".atan2(", ".sinh()", ".cosh()",
            ".tanh()", ".ln()", ".log(", ".log2()", ".log10()", ".exp()", ".exp2()", ".powf(", ".hypot(", ".cbrt()",
            ".ln_1p()", ".exp_m1()",
        ];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![root.clone()];
        let mut bad = Vec::new();
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                let name = p.file_name().unwrap().to_string_lossy().to_string();
                // Test files: `*tests*.rs` and everything inside a `*tests*` folder.
                let in_tests = p.strip_prefix(&root).unwrap().components().any(|c| c.as_os_str().to_string_lossy().contains("tests"));
                if !name.ends_with(".rs") || in_tests || name == "math.rs" {
                    continue;
                }
                let text = std::fs::read_to_string(&p).unwrap();
                // Everything from the first `#[cfg(test)]` on is test code (the module layout of
                // this crate keeps test modules at the end of a file).
                let code = text.split("#[cfg(test)]").next().unwrap_or("");
                for (i, line) in code.lines().enumerate() {
                    let l = line.split("//").next().unwrap_or("");
                    if BANNED.iter().any(|b| l.contains(b)) {
                        bad.push(format!("{}:{}: {}", p.display(), i + 1, line.trim()));
                    }
                }
            }
        }
        assert!(bad.is_empty(), "platform maths in core code (use crate::math):\n{}", bad.join("\n"));
    }
}
