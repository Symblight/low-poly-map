//! The seed world's modifiers of the height function (`docs/spec-world-seed.md` sections 5.1
//! step 9 and 5.2a): the water rule and the runway strips, both data, so the terrain stays a
//! pure function of (profile, files, point).
//!
//! Order (S1, a correction of the spec's "after the field and the runway strips"): **field ->
//! water rule -> strips**. The strips are applied last so a runway on reclaimed land or near the
//! coast stays exactly level ("airports cut water"), and their threshold heights are sampled from
//! field + water (the height before any strip). Evaluation is water first: a point in the sea or a
//! lake returns its level without sampling the field (section 10a cost budget).

use std::sync::Arc;

use super::airport::StripIndex;
use super::water::WaterIndex;

/// Water index + runway strips, shared (`Arc`) by the `Terrain` of every job.
#[derive(Debug)]
pub struct SeedTerrain {
    pub water: Arc<WaterIndex>,
    pub strips: StripIndex,
}

impl SeedTerrain {
    /// Height at a point: `field()` is the terrain before any seed rule (evaluated only when
    /// the point is not in water and not deep inside a fence).
    #[inline]
    pub fn height(&self, lat: f64, lon: f64, field: impl FnOnce() -> f64) -> f64 {
        match self.strips.at(lat, lon) {
            Some((h, k)) if k <= 0.0 => h,
            Some((h, k)) => {
                let base = self.water.apply_with(lat, lon, field);
                h + (base - h) * k
            }
            None => self.water.apply_with(lat, lon, field),
        }
    }

    /// The water level where the height function is a sea or lake level (the water rule
    /// applies and no airport strip or shoulder touches the point), else `None`. The terrain
    /// mesh never sinks such a vertex (contract section 9a).
    pub fn water_level(&self, lat: f64, lon: f64) -> Option<f64> {
        if self.strips.at(lat, lon).is_some() {
            return None;
        }
        self.water.rule_level_at(lat, lon)
    }

    /// The height before any strip (field + water rule): what the strips level to.
    pub fn height_before_strips(&self, lat: f64, lon: f64, field: impl FnOnce() -> f64) -> f64 {
        self.water.apply_with(lat, lon, field)
    }
}
