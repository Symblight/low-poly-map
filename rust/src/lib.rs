//! SimPlane world streamer GDExtension.
//!
//! Rust owns the world; Godot only displays what `WorldStreamer` hands it.
//! Milestone 4 (LOD): quadtree cover by 3D slant distance (R0..R3, sibling swap), PMTiles fetch
//! plus disk cache, MVT decode, per-tile merged meshes (R0 full, R1 boxes, R2 coarse blocks),
//! and R2/R3 light points with a clustered light carpet above ~3000 m AGL.
//! Milestone 5 (Ground): one flat ground mesh per R0-R2 tile (landuse / park / water areas and
//! buffered road strips) and the `ground_height` terrain hook (flat for now).

use godot::prelude::*;

pub mod fetch;
pub mod geo;
pub mod ground;
pub mod lights;
pub mod mesher;
pub mod mvt;
pub mod rings;
pub mod scheduler;
mod streamer;
pub mod synthetic;
pub mod worker;

#[cfg(test)]
mod integration_tests;

pub use streamer::WorldStreamer;

/// Crate version plus a build marker, exposed via `debug_stats()["version"]` so the
/// editor can show that a hot reload picked up a fresh build.
pub fn version_string() -> String {
    format!(
        "{} {} ({})",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        env!("SIMPLANE_BUILD_MARKER")
    )
}

struct SimplaneWorld;

#[gdextension]
unsafe impl ExtensionLibrary for SimplaneWorld {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_string_contains_crate_version_and_marker() {
        let v = version_string();
        assert!(v.starts_with("simplane_world "));
        assert!(v.contains(env!("CARGO_PKG_VERSION")));
        assert!(v.contains("build "));
    }
}
