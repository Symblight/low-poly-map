//! Stage 2, roads (`docs/spec-seed-city-pipeline.md` section 3; generator 3:
//! `docs/spec-world-seed.md` section 5.3 steps 5-6 and 6.2).
//!
//! * Major network (city plan, [`build`] -> [`MajorNetwork`]): the arterials on the hex edges
//!   ([`arterials`]), the radials and the airport access roads ([`radials`]).
//! * Local network (district plan, [`local::build`] -> [`local::LocalNetwork`]): the street
//!   lattice of a cell, its blocks and the service loops.

mod arterials;
pub mod local;
mod radials;

use super::context::CityContext;
use super::lines::bbox;
use super::zoning::Zoning;
use super::{RoadKind, RoadPath, StageHash};
use crate::seed::files::City;
use crate::seed::world::SeedWorld;

/// Output of the major road stage: arterials (sorted by id) then radials, and their counts.
#[derive(Clone, Debug, Default)]
pub struct MajorNetwork {
    pub roads: Vec<RoadPath>,
    /// Arterial hex edges kept after the drop (before water) and their connected components
    /// before and after the drop.
    pub arterial_edges: u32,
    pub arterial_components_before: u32,
    pub arterial_components: u32,
    pub bridges: u32,
    pub radials: u32,
    /// Radials that reach a linked airport's fence.
    pub airport_access: u32,
}

impl MajorNetwork {
    /// Stage hash: every road and the counts.
    pub fn hash(&self) -> u64 {
        let mut h = StageHash::new("network");
        for r in &self.roads {
            h.u(r.id).u(r.kind as u64).pts(&r.pts).pts(&[[r.bbox[0], r.bbox[1]], [r.bbox[2], r.bbox[3]]]);
        }
        h.u(self.arterial_edges as u64).u(self.arterial_components_before as u64).u(self.arterial_components as u64);
        h.u(self.bridges as u64).u(self.radials as u64).u(self.airport_access as u64);
        h.finish()
    }
}

/// The major network of `city` on its context and zoning.
pub fn build(ctx: &CityContext, zoning: &Zoning, city: &City, w: &SeedWorld) -> MajorNetwork {
    let mut net = MajorNetwork::default();
    arterials::build(&mut net, ctx, zoning, w);
    radials::build(&mut net, ctx, city, w);
    net
}

fn road(id: u64, kind: RoadKind, pts: Vec<[f64; 2]>) -> RoadPath {
    RoadPath { id, kind, bbox: bbox(&pts), pts }
}
