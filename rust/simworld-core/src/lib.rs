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
//! buffered road strips).
//! M9 Terrain, phase 1 (`docs/spec-terrain.md`, procedural): one seeded height function
//! ([`terrain::Terrain`]) carried explicitly by every worker job and by the cover; building bases,
//! foundations, draped ground and lights stand on it when the host requests the layer `terrain`.
//! M9 Terrain, phase 2 (api 1.5): the terrain-mesh layer, a second quadtree (`"t/z/x/y"` keys)
//! of Delatin TINs with skirts ([`terrain::tin`]), horizon cull, `set_terrain_detail`.
//! M10 World seed, phases S0 + S1 (api 1.7, `docs/spec-world-seed.md`): a [`seed::SeedWorld`]
//! from hand-written files (`configure_world` option `seed_world`) generates every vector tile
//! (water, countryside, airports with lights) and levels runways and water in the terrain.
//! P3 (Config): every tuning value lives in the world [`profile`] (JSON, compiled-in defaults
//! equal to the spike's constants), resolved by [`World::configure_world`].

pub mod budget;
pub mod data;
pub mod diag;
pub mod fetch;
pub mod gen_cache;
pub mod geo;
pub mod jobs;
pub mod ground;
pub mod latency;
pub mod lights;
pub mod lock;
pub mod math;
pub mod mesher;
pub mod mvt;
pub(crate) mod polyclip;
pub mod profile;
pub mod rings;
pub mod cover_budget;
pub mod scheduler;
pub mod seed;
pub mod status;
pub mod synthetic;
pub mod terrain;
pub mod worker;
pub mod world;

#[cfg(test)]
mod data_world_tests;
#[cfg(test)]
mod streaming_review_tests;
#[cfg(test)]
mod golden_tests;
#[cfg(test)]
mod height_golden_tests;
#[cfg(test)]
mod integration_tests;
#[cfg(test)]
mod lifecycle_tests;
#[cfg(test)]
mod perf_tests;
#[cfg(test)]
mod perf_stream_tests;

pub use diag::{Diagnostics, Log, NullLog};
pub use profile::{Profile, WorldProfile, PROFILE_VERSION};
pub use status::{Status, StatusState};
pub use terrain::Terrain;
pub use world::{
    AirportInfo, ConfigureResult, ReadyTileView, RunwayEndInfo, RunwayInfo, SeedCityInfo, SeedFileReader, SeedInfo, World, WorldOptions,
    WorldStats, OSM_ATTRIBUTION,
};

/// Version of this crate (`simworld-core`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Contract (`docs/contract.md`) major version. A major bump means a breaking change.
pub const API_MAJOR: u32 = 1;
/// Contract (`docs/contract.md`) minor version. A minor bump is additive only.
/// Independent of the crate version; must equal the `api_version:` line of `docs/contract.md`
/// (pinned by `tests::api_version_matches_contract_doc`).
pub const API_MINOR: u32 = 11;

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
