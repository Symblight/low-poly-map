//! The tile budget of the vector cover (review M1, profile `lod.max_tiles`): an estimate of the
//! cover's tile count from its band edges, and the quality scale that keeps it within the budget.

use crate::geo::MERCATOR_CIRCUMFERENCE;
use crate::rings::{BandParams, LodView, LEAF_ZOOM, RING_COUNT};

/// Estimated tile count per ring of the vector cover with these band `edges` (m, R0..R3), on
/// flat ground with the camera on it, at a latitude with `cos(lat)` = `cos_lat` (review M1).
///
/// A ring-r tile exists where its parent split (box distance < `edges[r]`; R3: the root is in
/// the cover, box distance < `edges[3]`) and is not split itself (box distance >=
/// `edges[r - 1]`). The squares of side `a` whose box meets a disk of radius `R` cover the
/// Minkowski sum `pi R^2 + 4 a R + a^2`, so ring r holds `(sum(parent side, edges[r]) -
/// sum(own side, edges[r - 1])) / side^2` tiles. At the default view it gives 27 / 65 / 103 /
/// 252 against 24 / 66 / 102 / 254 measured over the synthetic city (fov 15: 1969 against
/// 1982). Conservative above the ground (the camera's height shrinks every horizontal reach).
pub fn estimate_cover_tiles(edges: &[f64; RING_COUNT], cos_lat: f64) -> [f64; RING_COUNT] {
    let pi = std::f64::consts::PI;
    let disk = |r: f64, a: f64| pi * r * r + 4.0 * a * r + a * a;
    std::array::from_fn(|r| {
        let side = MERCATOR_CIRCUMFERENCE * cos_lat / (1u64 << (LEAF_ZOOM as usize - r)) as f64;
        let outer = if r + 1 < RING_COUNT { disk(edges[r], 2.0 * side) } else { disk(edges[r], side) };
        let inner = if r == 0 { 0.0 } else { disk(edges[r - 1], side) };
        ((outer - inner) / (side * side)).max(0.0)
    })
}

impl BandParams {
    /// The quality scale the cover runs with for the host's `quality` (review M1): `quality`
    /// itself while the estimated cover ([`estimate_cover_tiles`] of [`BandParams::edges_for`])
    /// fits `max_tiles`, else the smallest larger scale that fits (bisection in log space,
    /// deterministic; at most 2^20 x `quality`, i.e. R0..R2 shrunk to the 1 m minimum, if the
    /// roots alone exceed the budget). So a narrow field of view, a tall viewport or a low
    /// `set_lod_quality` cannot multiply the cover without bound. `max_tiles` 0 = no cap.
    pub fn capped_quality(&self, view: &LodView, quality: f64, cos_lat: f64) -> f64 {
        let fits = |q: f64| self.max_tiles <= 0.0 || estimate_cover_tiles(&self.edges_for(view, q), cos_lat).iter().sum::<f64>() <= self.max_tiles;
        if fits(quality) {
            return quality;
        }
        let (mut lo, mut hi) = (quality, quality * (1u64 << 20) as f64);
        if !fits(hi) {
            return hi;
        }
        for _ in 0..48 {
            let mid = (lo * hi).sqrt(); // IEEE sqrt: correctly rounded, deterministic
            if fits(mid) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        hi
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rings::MIN_REFINE_DISTANCE_M;

    const VILNIUS: (f64, f64) = (54.6872, 25.2797);

    fn bands() -> BandParams {
        *crate::profile::Profile::default().bands()
    }

    /// Review M1: the cover estimate matches the measured synthetic-city cover, the default view
    /// is uncapped (bit-exact edges), and a narrow fov / low quality is capped to `lod.max_tiles`
    /// with R3 kept.
    #[test]
    fn cover_is_capped_by_the_tile_budget() {
        let b = bands();
        let cos = crate::math::cos(VILNIUS.0.to_radians());
        let def = LodView::default();
        let n = estimate_cover_tiles(&b.edges_for(&def, 1.0), cos);
        let total: f64 = n.iter().sum();
        assert!((430.0..470.0).contains(&total), "default cover {n:?}");
        assert_eq!(b.capped_quality(&def, 1.0, cos), 1.0, "default view uncapped");
        assert_eq!(b.edges_for(&def, b.capped_quality(&def, 1.0, cos)), [600.0, 2500.0, 7000.0, 25_000.0]);
        // fov 30 (the bench's LOD self-test) and quality 0.5 still fit the default budget.
        let fov30 = LodView::new(30.0, 1080.0, [0.0, 0.0, -1.0]).unwrap();
        assert_eq!(b.capped_quality(&fov30, 1.0, cos), 1.0);
        assert_eq!(b.capped_quality(&def, 0.5, cos), 0.5);
        // fov 15: about 1970 tiles uncapped, R3 empty; capped to the budget with R3 kept.
        let fov15 = LodView::new(15.0, 1080.0, [0.0, 0.0, -1.0]).unwrap();
        let raw = estimate_cover_tiles(&b.edges_for(&fov15, 1.0), cos);
        assert!(raw.iter().sum::<f64>() > 1900.0 && raw[3] == 0.0, "{raw:?}");
        let q = b.capped_quality(&fov15, 1.0, cos);
        let capped = estimate_cover_tiles(&b.edges_for(&fov15, q), cos);
        let sum: f64 = capped.iter().sum();
        assert!(q > 1.0 && sum <= b.max_tiles && sum > 0.99 * b.max_tiles, "q {q}: {capped:?}");
        assert!(capped[3] > 50.0, "R3 kept: {capped:?}");
        let e = b.edges_for(&fov15, q);
        assert!(e[0] < e[1] && e[1] < e[2] && e[2] < e[3], "{e:?}");
        // Deterministic, and 0 = no cap.
        assert_eq!(q.to_bits(), b.capped_quality(&fov15, 1.0, cos).to_bits());
        let off = BandParams { max_tiles: 0.0, ..b };
        assert_eq!(off.capped_quality(&fov15, 1.0, cos), 1.0);
        // A budget the roots alone exceed shrinks R0..R2 to the minimum.
        let tiny = BandParams { max_tiles: 10.0, ..b };
        let qt = tiny.capped_quality(&def, 1.0, cos);
        assert_eq!(tiny.edges_for(&def, qt)[..3], [MIN_REFINE_DISTANCE_M; 3]);
    }
}
