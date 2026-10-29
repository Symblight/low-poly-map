//! Accessors for the host, tests and tools: scheduler, memory budget, terrain detail and layer
//! state, ground height, tile sources and `debug_tiles`.

use super::*;

impl World {

    /// The worker (tests).
    #[cfg(test)]
    pub(crate) fn worker(&self) -> Option<&Worker> {
        self.worker.as_ref()
    }

    /// The vector layer's scheduler (tests and tools).
    pub fn scheduler(&self) -> Option<&Scheduler> {
        self.scheduler.as_ref()
    }

    /// Tests and benches: read an archive per tile in every band (the api 1.7 behaviour; a
    /// two-level archive then shows nothing). Takes effect at the next configure.
    #[cfg(test)]
    pub(crate) fn set_per_tile_archive(&mut self, on: bool) {
        self.per_tile_archive = on;
    }

    /// The memory budget (Terrain v2 section 6): pool owners register their items with a byte
    /// size and priority, pin them while a job depends on them, and drain what was evicted.
    pub fn memory(&self) -> &MemoryBudget {
        &self.memory
    }

    pub fn memory_mut(&mut self) -> &mut MemoryBudget {
        &mut self.memory
    }

    pub(super) fn clamp_detail(&self, scale: f64) -> f64 {
        scale.clamp(TERRAIN_DETAIL_MIN, self.profile.terrain.detail_scale_max.max(TERRAIN_DETAIL_MIN))
    }

    /// Contract `set_terrain_detail(scale)` (api 1.5; meaning changed in 1.10, Terrain v2 G2):
    /// the terrain cover's SSE threshold is `terrain.tau_px` x `scale`, clamped to
    /// `[0.25, terrain.detail_scale_max]` (1 = the profile; 2 = coarser, fewer tiles). The cover
    /// follows from the next `update_camera` through ordinary refines and coarsens (new keys,
    /// fades, unloads); held terrain tiles are NOT re-meshed or re-delivered any more (a tile's
    /// TIN uses its band's `max_error_m` at every scale). The vector world is untouched. The
    /// scale is kept across `configure_world` (re-clamped to the new profile). Non-finite input
    /// is ignored. Returns the scale in effect.
    pub fn set_terrain_detail(&mut self, scale: f64) -> f64 {
        if scale.is_finite() {
            self.terrain_detail = self.clamp_detail(scale);
            if let Some(t) = &mut self.terrain_sched {
                t.set_detail(self.terrain_detail);
            }
        }
        self.terrain_detail
    }

    /// The terrain detail scale in effect (see [`World::set_terrain_detail`]).
    pub fn terrain_detail(&self) -> f64 {
        self.terrain_detail
    }

    /// Whether the terrain-mesh layer runs (terrain active and configured).
    pub fn landmark_layer_active(&self) -> bool {
        self.scheduler.as_ref().is_some_and(|s| s.landmark_layer_active())
    }

    /// The terrain-mesh layer runs (api 1.5).
    pub fn terrain_layer_active(&self) -> bool {
        self.terrain_sched.is_some()
    }

    /// Worker CPU per terrain ring since configure (`None` without the terrain layer).
    pub fn terrain_cpu(&self) -> Option<&CpuStats> {
        self.terrain_sched.as_ref().map(|t| t.cpu_stats())
    }

    /// Terrain height (m) under a point in current local x/z meters: `Terrain::height` at the
    /// point's lat/lon (through the current floating origin). 0 with the terrain off (the default); a few µs.
    pub fn ground_height_at(&self, local_x: f64, local_z: f64) -> f64 {
        let Some(f) = &self.frame else { return 0.0 };
        if !local_x.is_finite() || !local_z.is_finite() {
            return 0.0;
        }
        let [e, n] = self.local.to_enu(local_x, local_z);
        let (lat, lon) = f.lat_lon(e, n);
        self.terrain.height(lat, lon)
    }

    /// The terrain of the current configuration (flat when off, before configure and after
    /// `shutdown`). A copy can be sampled from any thread.
    pub fn terrain(&self) -> &Terrain {
        &self.terrain
    }

    /// The seed world of the current configuration (api 1.7).
    pub fn seed_world(&self) -> Option<&Arc<crate::seed::SeedWorld>> {
        self.seed.as_ref()
    }

    /// Where the tiles of a layer come from (`debug_tiles` `source`, api 1.7): vector tiles
    /// `"seed"` (seed world), `"archive"` (`tile_url`) or `"synthetic"` (the offline city);
    /// terrain tiles `"terrain"`. S5 adds `"real:<city>"` per z13 tile.
    pub fn tile_source(&self, layer: LayerKind) -> &'static str {
        match layer {
            LayerKind::Terrain => "terrain",
            LayerKind::World if self.seed.is_some() => "seed",
            LayerKind::World | LayerKind::Landmarks if self.worker.as_ref().is_some_and(|w| w.is_synthetic()) => "synthetic",
            LayerKind::World | LayerKind::Landmarks => "archive",
        }
    }

    /// Visit every tracked tile: key, ring, state name, rect `[x0, z0, x1, z1]` in current local
    /// x/z meters.
    pub fn debug_tiles(&self, mut f: impl FnMut(LayerKey, usize, &'static str, [f64; 4])) {
        let local = self.local;
        for (s, layer) in [(&self.scheduler, LayerKind::World), (&self.terrain_sched, LayerKind::Terrain)] {
            if let Some(s) = s {
                s.for_each_tile(|key, ring, state, rect| f(LayerKey { layer, key }, ring, state, local.rect_to_local(rect)));
            }
        }
        if let Some(s) = &self.scheduler {
            s.for_each_landmark(|key, state, rect| f(LayerKey::landmarks(key), 0, state, local.rect_to_local(rect)));
        }
    }
}
