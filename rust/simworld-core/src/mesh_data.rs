//! The merged per-tile mesh (`MeshData`): flat vertex buffers in `Mesh.ARRAY_MAX` order, built by
//! the mesher, the ground and the terrain TIN on the job pool. Positions are relative to the tile
//! origin (tile centre, y = 0 / height above MSL for terrain), so a mesh never depends on the
//! floating origin and is built once per key.
//!
//! **Engine form [1.11]** (`configure_world` option `engine_surfaces`, perf review 3 item 1):
//! [`MeshData::pack_engine_surface`] encodes the arrays once more into the renderer's own vertex
//! layout ([`EngineSurface`], `crate::surface`) and frees them, so a tile is held in one form
//! only. The counts ([`MeshData::vertex_count`], [`MeshData::triangle_count`]) answer for either
//! form.

use crate::surface::EngineSurface;

/// Merged mesh in `Mesh.ARRAY_MAX` order (VERTEX, NORMAL, COLOR, TEX_UV, INDEX), positions
/// relative to the tile origin (tile center, y = 0).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshData {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub colors: Vec<[f32; 4]>,
    pub uvs: Vec<[f32; 2]>,
    pub indices: Vec<i32>,
    /// The engine form of the arrays (they are then empty): `pack_engine_surface`.
    pub surface: Option<Box<EngineSurface>>,
}

impl MeshData {
    /// No triangles (in either form).
    pub fn is_empty(&self) -> bool {
        self.index_count() == 0
    }
    pub fn vertex_count(&self) -> usize {
        match &self.surface {
            Some(s) => s.vertex_count as usize,
            None => self.positions.len(),
        }
    }
    pub fn index_count(&self) -> usize {
        match &self.surface {
            Some(s) => s.index_count as usize,
            None => self.indices.len(),
        }
    }
    pub fn triangle_count(&self) -> usize {
        self.index_count() / 3
    }
    /// Approximate heap bytes of the buffers (for budget reporting).
    pub fn bytes(&self) -> usize {
        self.positions.len() * (12 + 12 + 16 + 8) + self.indices.len() * 4 + self.surface.as_ref().map_or(0, |s| s.bytes())
    }
    /// Append `other` (same origin; indices offset). Both must have UVs or neither (building
    /// meshes always have them). Array form only (meshes are packed after they are complete).
    pub fn append(&mut self, other: MeshData) {
        debug_assert!(self.surface.is_none() && other.surface.is_none(), "append to a packed mesh");
        if other.positions.is_empty() {
            return;
        }
        if self.positions.is_empty() {
            *self = other;
            return;
        }
        let base = self.positions.len() as i32;
        self.positions.extend(other.positions);
        self.normals.extend(other.normals);
        self.colors.extend(other.colors);
        self.uvs.extend(other.uvs);
        self.indices.extend(other.indices.into_iter().map(|i| i + base));
    }
    pub(crate) fn shrink(&mut self) {
        self.positions.shrink_to_fit();
        self.normals.shrink_to_fit();
        self.colors.shrink_to_fit();
        self.uvs.shrink_to_fit();
        self.indices.shrink_to_fit();
    }
    /// Replace the arrays by their engine form (`EngineSurface::encode`); an empty mesh stays
    /// empty (no surface). Idempotent.
    pub fn pack_engine_surface(&mut self) {
        if self.surface.is_some() || self.indices.is_empty() {
            return;
        }
        self.surface = Some(Box::new(EngineSurface::encode(self)));
        self.positions = Vec::new();
        self.normals = Vec::new();
        self.colors = Vec::new();
        self.uvs = Vec::new();
        self.indices = Vec::new();
    }
}
