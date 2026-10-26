//! SimWorld core: the engine-free streamed low-poly OSM world.
//!
//! Rust owns the world; a host (the `simworld-godot` binding, the CLI, later others) only
//! displays what [`World`] hands it. This crate has no engine dependency, reads no environment
//! variables and logs only through the [`Log`] trait passed to [`World::new`].
//!
//! Milestone 4 (LOD): quadtree cover by 3D slant distance (R0..R3, sibling swap), PMTiles fetch
//! plus disk cache, MVT decode, per-tile merged meshes (R0 full, R1 boxes, R2 coarse blocks),
//! and R2/R3 light points with a clustered light carpet above ~3000 m AGL.
//! Milestone 5 (Ground): one flat ground mesh per R0-R2 tile (landuse / park / water areas and
//! buffered road strips) and the `ground_height` terrain hook (flat for now).

pub mod diag;
pub mod fetch;
pub mod geo;
pub mod ground;
pub mod lights;
pub mod mesher;
pub mod mvt;
pub mod rings;
pub mod scheduler;
pub mod synthetic;
pub mod worker;
pub mod world;

#[cfg(test)]
mod integration_tests;

pub use diag::{Diagnostics, Log, NullLog};
pub use world::{ReadyTileView, World, WorldStats};

/// Version of this crate (`simworld-core`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
