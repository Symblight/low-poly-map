//! The shared two-cities test world (spec 11a.1), loaded once per test binary.

use std::sync::Arc;

use super::SeedWorld;
use crate::profile::Profile;
use crate::terrain::Terrain;

/// The shared two-cities world (`low-poly-map/world/two-cities/`, spec 11a.1) for tests: one
/// copy for the bench scene and `cargo test`.
pub(crate) const TEST_WORLD: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../low-poly-map/world/two-cities/world.json");

/// Profile overrides every seed world needs (terrain on, no flat zone).
pub(crate) fn test_profile_json() -> serde_json::Value {
    serde_json::json!({ "terrain": { "enabled": true, "flat_radius_m": 0.0, "flat_blend_m": 0.0 } })
}

/// The two-cities world loaded once per test binary: (profile, terrain with the seed rules, world).
pub(crate) fn test_world() -> &'static (Profile, Terrain, Arc<SeedWorld>) {
    static W: std::sync::OnceLock<(Profile, Terrain, Arc<SeedWorld>)> = std::sync::OnceLock::new();
    W.get_or_init(|| {
        let p = crate::profile::resolve(None, Some(&test_profile_json()), &crate::NullLog).unwrap();
        let field = Terrain::from_profile(&p, 54.6872, 25.2797);
        let read = |path: &str| std::fs::read_to_string(path).map_err(|e| e.to_string());
        let w = SeedWorld::load(TEST_WORLD, &read, &p, &field, None).unwrap_or_else(|e| panic!("{e}"));
        let t = field.with_seed(w.terrain.clone());
        (p, t, w)
    })
}
