//! M10 World seed (`docs/spec-world-seed.md`): phases S0 (foundations) and S1 (manifest load,
//! water and countryside ground in all bands, the terrain coupling, airports; api 1.7).
//!
//! * [`hash`]: the one counter-based hash `H(parts…)` with typed parts and the seed hierarchy
//!   (section 7), pinned by a test vector.
//! * [`files`]: typed forms, loaders and the load-time validation (section 4.6) of `world.json`,
//!   `cities/<id>.json`, `airports.json` (with `airports.overrides`), `water.json` and
//!   `roads.json`. The schemas are `addons/simworld/seed/*.schema.json`, checked by [`schema`].
//! * [`water`]: the z11 water index, the signed distance raster and the terrain-water rule of
//!   section 5.2a; the water polygons of a tile.
//! * [`airport`]: generated airports (section 5.1) and the runway strip rule of the terrain.
//! * [`countryside`]: forest / farmland / meadow patches with a tint (section 5.2).
//! * [`terrain`]: the seed world's modifiers of the height function (water rule + strips).
//! * [`world`]: [`SeedWorld`] (load, cache key, terrain colour class) and the tile generator
//!   [`world::generate`] `(key, &SeedWorld)`.
//! * [`geom`]: rectangle clipping, Douglas-Peucker on rings, areas (shared with `simworld-cli seed
//!   water`).

pub mod airport;
pub mod countryside;
pub mod files;
pub mod geom;
pub mod hash;
pub mod schema;
pub mod terrain;
pub mod water;
pub mod world;

pub use world::{SeedWorld, SEED_GEN_VERSION, SEED_PHASES};

#[cfg(test)]
mod drape_tests;
