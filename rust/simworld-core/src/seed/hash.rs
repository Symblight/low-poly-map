//! The seed world's one hash (`docs/spec-world-seed.md` section 7): `H(parts…) -> u64`, a
//! counter-based hash (no stateful RNG), so every random choice of the generator depends only on
//! a seed, a purpose tag and a stable id, never on order, threads or what else is in the world.
//!
//! Folding: the state starts at [`H_INIT`]; each part folds a **type tag**, then its value:
//!
//! | Part | Folded words |
//! |---|---|
//! | unsigned integer ([`Part::U`]) | `1`, the value as `u64` |
//! | signed integer ([`Part::I`]) | `2`, the value as two's complement `u64` |
//! | string ([`Part::S`]) | `3`, the byte length, FNV-1a 64 of the UTF-8 bytes |
//! | parent seed ([`Part::Seed`]) | `4`, the seed |
//!
//! and after the last part the **part count**. One fold is `state = splitmix64(state ^ word)`
//! (`mvt::splitmix64`: add the golden gamma, then the SplitMix64 finalizer). So `H("a", 1)`,
//! `H("a1")`, `H(1, "a")`, `H("a", 1, 0)` and `H("a", 1i)` (signed) all differ; the test vector
//! pins these shapes. Never `std::hash`, `DefaultHasher` or `RandomState` (not stable across Rust
//! versions). Integer literals in [`seed_hash!`] are typed by Rust (`1` is `i32`, so signed): write
//! `1u64` where the spec means an unsigned id.

use crate::mvt::splitmix64;

/// Initial state of every hash: ASCII "SIMWORLD".
pub const H_INIT: u64 = 0x5349_4D57_4F52_4C44;

const TAG_U: u64 = 1;
const TAG_I: u64 = 2;
const TAG_S: u64 = 3;
const TAG_SEED: u64 = 4;

/// A 64-bit seed used as a parent part of another hash (tag 4). Keeps "this is a seed" apart from
/// "this is a number" in the hash input.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Seed(pub u64);

/// One hash input part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part<'a> {
    U(u64),
    I(i64),
    S(&'a str),
    Seed(Seed),
}

macro_rules! part_from {
    ($variant:ident, $cast:ty: $($t:ty),*) => {$(
        impl From<$t> for Part<'_> {
            #[inline]
            fn from(v: $t) -> Self {
                Part::$variant(v as $cast)
            }
        }
    )*};
}
part_from!(U, u64: u8, u16, u32, u64, usize);
part_from!(I, i64: i8, i16, i32, i64, isize);

impl<'a> From<&'a str> for Part<'a> {
    fn from(v: &'a str) -> Self {
        Part::S(v)
    }
}

impl<'a> From<&'a String> for Part<'a> {
    fn from(v: &'a String) -> Self {
        Part::S(v.as_str())
    }
}

impl From<Seed> for Part<'_> {
    fn from(v: Seed) -> Self {
        Part::Seed(v)
    }
}

/// FNV-1a 64 over bytes.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// Incremental form of [`h`] (same result for the same parts).
#[derive(Clone, Copy, Debug)]
pub struct Hasher {
    state: u64,
    count: u64,
}

impl Default for Hasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Hasher {
    pub const fn new() -> Self {
        Self { state: H_INIT, count: 0 }
    }

    #[inline]
    fn fold(&mut self, w: u64) {
        self.state = splitmix64(self.state ^ w);
    }

    pub fn part(&mut self, p: Part<'_>) -> &mut Self {
        match p {
            Part::U(v) => {
                self.fold(TAG_U);
                self.fold(v);
            }
            Part::I(v) => {
                self.fold(TAG_I);
                self.fold(v as u64);
            }
            Part::S(s) => {
                self.fold(TAG_S);
                self.fold(s.len() as u64);
                self.fold(fnv1a64(s.as_bytes()));
            }
            Part::Seed(Seed(v)) => {
                self.fold(TAG_SEED);
                self.fold(v);
            }
        }
        self.count += 1;
        self
    }

    pub fn finish(&self) -> u64 {
        splitmix64(self.state ^ self.count)
    }
}

/// `H(parts…)`.
pub fn h(parts: &[Part<'_>]) -> u64 {
    let mut s = Hasher::new();
    for &p in parts {
        s.part(p);
    }
    s.finish()
}

/// `H(parts…)` with each argument converted by `Part::from`: `seed_hash!(Seed(root), "city", id, variant)`.
#[macro_export]
macro_rules! seed_hash {
    ($($p:expr),* $(,)?) => {
        $crate::seed::hash::h(&[$($crate::seed::hash::Part::from($p)),*])
    };
}

/// Uniform in [0, 1): the top 53 bits of `h` / 2^53.
#[inline]
pub fn unit(h: u64) -> f64 {
    (h >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
}

// ---------------------------------------------------------------------------------------------
// Seed hierarchy (section 7)

/// `root = H("world", world_seed)`.
pub fn world_root(world_seed: &str) -> Seed {
    Seed(seed_hash!("world", world_seed))
}

/// `city = H(root, "city", id, variant)` (variant signed: the file's int).
pub fn city_seed(root: Seed, city_id: &str, variant: i64) -> Seed {
    Seed(seed_hash!(root, "city", city_id, variant))
}

/// `airport = H("airport", ident)`: not under the world root, so re-rolling never moves an airport.
pub fn airport_seed(ident: &str) -> Seed {
    Seed(seed_hash!("airport", ident))
}

/// `district = H(city, "district", q, r, part)` (axial cell coordinates, signed).
pub fn district_seed(city: Seed, q: i32, r: i32, part: u32) -> Seed {
    Seed(seed_hash!(city, "district", q, r, part))
}

/// `edge = H(city, "edge", sorted cell pair)`: the pair sorted, so both cells hash the same edge.
pub fn edge_seed(city: Seed, a: (i32, i32), b: (i32, i32)) -> Seed {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    Seed(seed_hash!(city, "edge", lo.0, lo.1, hi.0, hi.1))
}

/// `building = H(district, "building", lot)`.
pub fn building_seed(district: Seed, lot: u32) -> Seed {
    Seed(seed_hash!(district, "building", lot))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The test vector (section 12): fixed inputs -> fixed u64s, on every CI target. Changing any
    /// of these re-rolls every seed world: only on purpose, with a `SEED_GEN_VERSION` bump.
    #[test]
    fn test_vector() {
        let root = world_root("simplane-baltics");
        let city = city_seed(root, "kaunas", 0);
        let got: Vec<(&str, u64)> = vec![
            ("H()", seed_hash!()),
            ("H(\"\")", seed_hash!("")),
            ("H(\"a\")", seed_hash!("a")),
            ("H(\"a\", 1u)", seed_hash!("a", 1u64)),
            ("H(\"a1\")", seed_hash!("a1")),
            ("H(1u, \"a\")", seed_hash!(1u64, "a")),
            ("H(\"a\", 1u, 0u)", seed_hash!("a", 1u64, 0u64)),
            ("H(\"a\", 1i)", seed_hash!("a", 1i64)),
            ("H(\"a\", -1i)", seed_hash!("a", -1i64)),
            ("H(seed 1)", seed_hash!(Seed(1))),
            ("H(\"\u{e9}\")", seed_hash!("\u{e9}")),
            ("root", root.0),
            ("city", city.0),
            ("city variant 1", city_seed(root, "kaunas", 1).0),
            ("airport EYKA", airport_seed("EYKA").0),
            ("district", district_seed(city, -3, 2, 0).0),
            ("edge", edge_seed(city, (0, 1), (1, 0)).0),
            ("building", building_seed(district_seed(city, -3, 2, 0), 17).0),
        ];
        let want: &[u64] = &TEST_VECTOR;
        let printed: Vec<String> = got.iter().map(|(n, v)| format!("    {v:#018x}, // {n}")).collect();
        assert_eq!(got.len(), want.len(), "test vector length; current values:\n{}", printed.join("\n"));
        for ((name, v), w) in got.iter().zip(want) {
            assert_eq!(v, w, "{name} changed; current values:\n{}", printed.join("\n"));
        }
        // All distinct (the arity shapes must not collide).
        let mut all: Vec<u64> = got.iter().map(|g| g.1).collect();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), got.len(), "two shapes hash the same");
    }

    const TEST_VECTOR: [u64; 18] = [
        0x1729_44b5_23e8_0516, // H()
        0x6562_576c_6bf1_0961, // H("")
        0xd0c2_040b_79e3_64d6, // H("a")
        0xcc40_ebd2_cc79_dfea, // H("a", 1u)
        0x64c6_01b9_fb74_634e, // H("a1")
        0x5c20_6177_2292_88d5, // H(1u, "a")
        0x7c8a_d4c6_e9f4_097d, // H("a", 1u, 0u)
        0x3836_8188_aaf6_ab50, // H("a", 1i)
        0x8407_79a6_4995_86bb, // H("a", -1i)
        0x93de_7988_a824_4e44, // H(seed 1)
        0x4062_441b_2c16_d771, // H("é")
        0x422d_83ff_faf2_4768, // root
        0x45bd_fadf_d756_8dcb, // city
        0xeced_2b19_f003_ffbc, // city variant 1
        0x1ad7_66c8_6652_e2c5, // airport EYKA
        0xb330_c335_3883_94d7, // district
        0xa3c5_b7a3_bd2a_33be, // edge
        0x9690_7039_84eb_9d11, // building
    ];

    #[test]
    fn incremental_equals_slice_and_edge_is_symmetric() {
        let mut s = Hasher::new();
        s.part("x".into()).part(5u32.into()).part((-2i32).into());
        assert_eq!(s.finish(), seed_hash!("x", 5u32, -2i32));
        let c = Seed(42);
        assert_eq!(edge_seed(c, (2, -1), (1, 0)), edge_seed(c, (1, 0), (2, -1)));
        // u32 / u64 / usize fold the same (tag 1); i32 / i64 the same (tag 2).
        assert_eq!(seed_hash!(7u32), seed_hash!(7u64));
        assert_eq!(seed_hash!(7usize), seed_hash!(7u8));
        assert_eq!(seed_hash!(-7i32), seed_hash!(-7i64));
        assert_ne!(seed_hash!(7i32), seed_hash!(7u32));
        // A string part is its bytes, not its Rust type.
        assert_eq!(seed_hash!(&String::from("ab")), seed_hash!("ab"));
    }

    #[test]
    fn unit_is_in_range_and_uniform_enough() {
        assert_eq!(unit(0), 0.0);
        assert!(unit(u64::MAX) < 1.0);
        let n = 20_000u64;
        let mut buckets = [0u32; 10];
        for i in 0..n {
            let u = unit(seed_hash!("unit", i));
            buckets[(u * 10.0) as usize] += 1;
        }
        for b in buckets {
            assert!((b as f64 - n as f64 / 10.0).abs() < n as f64 * 0.01, "{buckets:?}");
        }
    }
}
