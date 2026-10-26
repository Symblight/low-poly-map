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
//! P3 (Config): every tuning value lives in the world [`profile`] (JSON, compiled-in defaults
//! equal to the spike's constants), resolved by [`World::configure_world`].

pub mod diag;
pub mod fetch;
pub mod geo;
pub mod ground;
pub mod lights;
pub mod mesher;
pub mod mvt;
pub mod profile;
pub mod rings;
pub mod scheduler;
pub mod synthetic;
pub mod worker;
pub mod world;

#[cfg(test)]
mod golden_tests;
#[cfg(test)]
mod integration_tests;

pub use diag::{Diagnostics, Log, NullLog};
pub use profile::{Profile, WorldProfile, PROFILE_VERSION};
pub use world::{ConfigureResult, ReadyTileView, World, WorldOptions, WorldStats, OSM_ATTRIBUTION};

/// Version of this crate (`simworld-core`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Contract (`docs/contract.md`) major version. A major bump means a breaking change.
pub const API_MAJOR: u32 = 0;
/// Contract (`docs/contract.md`) minor version. A minor bump is additive only.
/// Independent of the crate version; must equal the `api_version:` line of `docs/contract.md`
/// (pinned by `tests::api_version_matches_contract_doc`).
pub const API_MINOR: u32 = 6;

#[cfg(test)]
mod tests {
    /// `docs/contract.md` declares `api_version: <major>.<minor>` on its own line; the constants
    /// and the document must change together (with `CHANGELOG.md`).
    #[test]
    fn api_version_matches_contract_doc() {
        let doc = include_str!("../../../docs/contract.md");
        let line = doc
            .lines()
            .find_map(|l| l.trim().strip_prefix("api_version:"))
            .expect("docs/contract.md has no `api_version:` line");
        let (major, minor) = line.trim().trim_matches('`').split_once('.').expect("api_version is major.minor");
        assert_eq!(
            (major.parse::<u32>().unwrap(), minor.parse::<u32>().unwrap()),
            (super::API_MAJOR, super::API_MINOR),
            "docs/contract.md api_version and simworld_core::API_MAJOR/API_MINOR disagree"
        );
    }
}
