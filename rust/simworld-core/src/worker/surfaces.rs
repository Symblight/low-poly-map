//! Engine surfaces on the job pool (perf review 3 item 1, `configure_world` option
//! `engine_surfaces`, api 1.11): a finished tile's meshes are packed into the renderer's vertex
//! layout (`crate::surface`) by the job that built them, so the host's upload is a copy.

use std::sync::Arc;

use super::{TileData, Worker};
use crate::mesh_data::MeshData;

impl Worker {
    /// Pack the meshes of every tile built from now on (`engine_surfaces`; set before the first
    /// job).
    pub fn set_engine_surfaces(&mut self, on: bool) {
        self.engine_surfaces = on;
    }
}

/// `d` with its building, ground and terrain meshes packed when `on` (else unchanged). A fresh
/// build owns its meshes, so this packs in place.
pub(super) fn pack(mut d: TileData, on: bool) -> TileData {
    if !on {
        return d;
    }
    pack_arc(&mut d.mesh, |m| m);
    pack_arc(&mut d.ground, |m| m);
    if let Some(t) = d.terrain.as_mut() {
        pack_arc(t, |t| &mut t.mesh);
    }
    d
}

fn pack_arc<T: Clone>(a: &mut Arc<T>, mesh: impl Fn(&mut T) -> &mut MeshData) {
    mesh(Arc::make_mut(a)).pack_engine_surface();
}
