//! Data levels (Tiles v2), the landmark layer and the memory budget of the data tiles.

use super::*;

impl Scheduler {

    /// Cut chunks from data tiles (Tiles v2): those finer than `levels.detail` from their
    /// detail tile (T2a), with a two-level source R2 / R3 from their overview tile (T2b).
    /// Vector layer only. A synthetic scheduler generates its data tiles (T2c).
    pub fn with_data_levels(mut self, levels: DataLevels) -> Self {
        if self.layer.kind == LayerKind::World {
            let max = self.profile.streaming.max_fetch_per_band * 2;
            self.data = Some(DataStore::new(Some(levels), max, self.synthetic, self.profile.data_idle()));
        }
        self
    }

    /// An archive source whose levels are not known yet: no vector job is dispatched until
    /// [`Scheduler::set_data_levels`].
    pub fn with_data_pending(mut self) -> Self {
        if self.layer.kind == LayerKind::World && !self.synthetic {
            let max = self.profile.streaming.max_fetch_per_band * 2;
            self.data = Some(DataStore::new(None, max, false, self.profile.data_idle()));
        }
        self
    }

    /// The archive opened: its data levels (no-op unless pending). Returns whether they were
    /// applied.
    pub fn set_data_levels(&mut self, levels: DataLevels) -> bool {
        match &mut self.data {
            Some(d) if d.levels.is_none() => {
                d.levels = Some(levels);
                true
            }
            _ => false,
        }
    }

    /// The data levels in use (`None`: per-tile source, or the archive has not opened yet).
    pub fn data_levels(&self) -> Option<DataLevels> {
        self.data.as_ref().and_then(|d| d.levels)
    }

    /// Waiting for the archive's levels: nothing is dispatched.
    pub fn data_pending(&self) -> bool {
        self.data.as_ref().is_some_and(|d| d.levels.is_none())
    }

    /// The host draws the landmark layer (`layers` option `"landmarks"`): with a two-level
    /// source that has landmarks they come as `l/z/x/y` tiles and are not folded into chunks.
    pub fn with_landmark_layer(mut self, on: bool) -> Self {
        self.landmark_layer = on;
        self
    }

    /// The landmark layer runs (opted in, and the source has landmarks).
    pub fn landmark_layer_active(&self) -> bool {
        self.landmark_layer && self.data_levels().is_some_and(|l| l.two_level && l.landmarks)
    }

    /// Landmark layer counters.
    pub fn landmark_stats(&self) -> LandmarkStats {
        self.landmarks.stats()
    }

    /// The data store (Tiles v2), if this scheduler cuts chunks from data tiles.
    pub fn data(&self) -> Option<&DataStore> {
        self.data.as_ref()
    }

    /// Data tile counters (zero without data tiles).
    pub fn data_stats(&self) -> DataStats {
        self.data.as_ref().map(|d| d.stats()).unwrap_or_default()
    }

    /// Apply the data store's queued budget changes and drop what the budget evicted. `World`
    /// calls it after every scheduler call that may change the store.
    pub fn sync_memory(&mut self, mem: &mut crate::budget::MemoryBudget) {
        if let Some(d) = &mut self.data {
            d.sync(mem);
        }
    }

    /// Decoded data tiles dropped since the last call, for the caller to free off the main
    /// thread (`Worker::release`).
    pub fn take_released_data(&mut self) -> Vec<Arc<crate::data::DataTile>> {
        self.data.as_mut().map(|d| d.take_released()).unwrap_or_default()
    }

    /// Unregister the data tiles from the budget (before the scheduler is dropped).
    pub fn release_memory(&mut self, mem: &mut crate::budget::MemoryBudget) {
        if let Some(d) = &mut self.data {
            d.release(mem);
        }
    }

    /// The data tile chunk `k` is cut from (`None`: the tile is fetched on its own).
    pub(super) fn data_key(&self, k: TileKey) -> Option<TileKey> {
        self.data_levels().and_then(|l| l.data_of(k))
    }

    /// The overview tile whose landmarks chunk `k` draws (folded in: no landmark layer).
    pub(super) fn lm_key(&self, k: TileKey) -> Option<TileKey> {
        if self.landmark_layer {
            return None;
        }
        self.data_levels().and_then(|l| l.landmarks_of(k))
    }

    /// Every data tile chunk `k` depends on: its own, plus another one for the landmarks.
    pub(super) fn data_keys(&self, k: TileKey) -> [Option<TileKey>; 2] {
        let dk = self.data_key(k);
        let lk = self.lm_key(k).filter(|l| dk.is_some() && Some(*l) != dk);
        [dk, lk]
    }

    /// Queue priority of a data-tile job: on demand, ordered by the nearest chunk waiting for it.
    pub fn data_job_priority(&self, k: TileKey) -> Priority {
        let want = self.data_distance(k);
        if want.is_finite() {
            Priority::on_demand(want)
        } else {
            Priority::prefetch(f64::INFINITY)
        }
    }

    /// Slant distance from the camera to landmark tile `k` (infinite if not in range).
    pub fn landmark_distance(&self, k: TileKey) -> f64 {
        self.landmarks.entries.get(&k).map_or(f64::INFINITY, |e| e.cam_d)
    }

    /// Delivery order of the nearest chunk waiting for data tile `k` (infinite if none).
    pub fn data_distance(&self, k: TileKey) -> f64 {
        self.data.as_ref().and_then(|d| d.get(k)).map_or(f64::INFINITY, |e| e.want)
    }

}
