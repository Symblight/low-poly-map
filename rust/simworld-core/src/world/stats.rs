//! Counters for a debug overlay ([`WorldStats`]).

use super::*;

/// Counters for a debug overlay.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct WorldStats {
    pub queued: [i32; RING_COUNT],
    pub fetching: [i32; RING_COUNT],
    pub meshing: [i32; RING_COUNT],
    pub loaded: [i32; RING_COUNT],
    /// Disk cache size (MiB).
    pub cache_mb: f64,
    /// WGS84 position of the *current* local origin (moves with re-centering).
    pub origin_lat: f64,
    pub origin_lon: f64,
    /// Terrain layer (0 rings when it is not active): per-ring counters as above and the
    /// triangles of every terrain tile the host holds.
    pub terrain_rings: usize,
    pub terrain_queued: [i32; MAX_RINGS],
    pub terrain_fetching: [i32; MAX_RINGS],
    pub terrain_meshing: [i32; MAX_RINGS],
    pub terrain_loaded: [i32; MAX_RINGS],
    /// Terrain splits the tile budget (`terrain.max_tiles`) left out of the last cover plan.
    pub terrain_budget_skipped: usize,
    pub terrain_tris: u64,
    /// Shared CPU job pool (both layers): jobs queued (ready or waiting on dependencies),
    /// running, and cancelled since the world was configured. Zero when not running.
    pub jobs: JobStats,
    /// Tile-box probes requested and not answered yet, both layers (`scheduler::boxes`).
    pub probes_pending: usize,
    /// Recent-tile cache of both layers (`scheduler::recent`): hits, entries, bytes.
    pub recent: crate::scheduler::RecentStats,
    /// The memory budget of the core's CPU-side pools (`World::memory`).
    pub memory: MemoryStats,
    /// Tiles v2 data tiles (vector layer, archive sources): decoded tiles resident now, and
    /// decodes since the world was configured (a re-decode after an eviction counts again).
    pub data_tiles_loaded: u32,
    pub data_tiles_decoded: u64,
    /// Data tiles dropped after `memory.data_idle_s` unneeded / evicted by the memory budget,
    /// and decodes of a tile evicted before (review M3).
    pub data_tiles_evicted_idle: u64,
    pub data_tiles_evicted_budget: u64,
    pub data_tiles_redecoded: u64,
    /// Data tile fetches completed since configure (disk cache or network).
    pub data_tiles_fetched: u64,
    /// Of those: overview tiles, and tiles absent from the archive (empty).
    pub data_tiles_fetched_overview: u64,
    pub data_tiles_fetched_empty: u64,
    /// The landmark layer (api 1.11; zero unless the host requested it and the source has
    /// landmarks).
    pub landmarks: crate::scheduler::LandmarkStats,
    /// The data levels of the source (`None`: per-tile source, or the archive has not opened yet).
    pub data_levels: Option<crate::data::DataLevels>,
}

fn first4(a: [i32; MAX_RINGS]) -> [i32; RING_COUNT] {
    std::array::from_fn(|i| a[i])
}

impl World {
    pub fn stats(&self) -> WorldStats {
        let st = self.scheduler.as_ref().map(|s| s.stats()).unwrap_or_default();
        let data = self.scheduler.as_ref().map(|s| s.data_stats()).unwrap_or_default();
        let cache_mb = self.worker.as_ref().map_or(0.0, |w| w.cache_bytes() as f64 / 1_048_576.0);
        // Lat/lon of the *current* local origin (moves with re-centering).
        let (origin_lat, origin_lon) = match &self.frame {
            Some(f) if self.local != LocalSpace::default() => f.lat_lon(self.local.offset_e, self.local.offset_n),
            _ => (self.origin_lat, self.origin_lon),
        };
        let ts = self.terrain_sched.as_ref().map(|t| (t.layer().rings(), t.stats()));
        let (terrain_rings, tst) = ts.unwrap_or_default();
        WorldStats {
            queued: first4(st.queued),
            fetching: first4(st.fetching),
            meshing: first4(st.meshing),
            loaded: first4(st.loaded),
            cache_mb,
            origin_lat,
            origin_lon,
            terrain_rings,
            terrain_queued: tst.queued,
            terrain_fetching: tst.fetching,
            terrain_meshing: tst.meshing,
            terrain_loaded: tst.loaded,
            terrain_budget_skipped: self.terrain_sched.as_ref().map_or(0, |t| t.budget_skipped()),
            terrain_tris: tst.tris,
            jobs: self.worker.as_ref().map(|w| w.job_stats()).unwrap_or_default(),
            probes_pending: [self.scheduler.as_ref(), self.terrain_sched.as_ref()].into_iter().flatten().map(|s| s.probes_pending()).sum(),
            recent: [self.scheduler.as_ref(), self.terrain_sched.as_ref()].into_iter().flatten().map(|s| s.recent_stats()).fold(
                crate::scheduler::RecentStats::default(),
                |a, b| crate::scheduler::RecentStats { hits: a.hits + b.hits, entries: a.entries + b.entries, bytes: a.bytes + b.bytes },
            ),
            memory: self.memory.stats(),
            data_tiles_loaded: data.loaded,
            data_tiles_decoded: data.decoded,
            data_tiles_evicted_idle: data.evicted_idle,
            data_tiles_evicted_budget: data.evicted_budget,
            data_tiles_redecoded: data.redecoded,
            data_tiles_fetched: data.fetched,
            data_tiles_fetched_overview: data.fetched_overview,
            data_tiles_fetched_empty: data.fetched_empty,
            landmarks: self.scheduler.as_ref().map(|s| s.landmark_stats()).unwrap_or_default(),
            data_levels: self.scheduler.as_ref().and_then(|s| s.data_levels()),
        }
    }
}
