//! Recently delivered tiles kept in memory (perf review 2, item 6).
//!
//! In level flight a key comes back within seconds of leaving the host: a refined parent is
//! dropped when its children's fade completes, and once the camera has passed, the coarsening
//! needs it again (guarantee 4: the parent is delivered again before its children fade out).
//! At 250 m/s that is about 45 keys a minute, each fetched, decoded and meshed again. That is
//! the LOD cycle working as designed, not hysteresis flapping (measured: the keys come back on
//! the far side of the camera, see `perf_stream_tests`).
//!
//! A tile's built output (meshes, lights, terrain mesh) is shared (`Arc`) between the delivery
//! and this cache, so keeping it costs no copy. The output is a pure function of the key and
//! the scheduler's profile, view-independent and holds both light modes, so a kept output is
//! exactly what a rebuild would give. An entry is kept from the delivery on; once the tile left
//! the host it lives `memory.recent_tiles_s` more seconds. Each layer's cache is capped at
//! `memory.recent_tiles_mb`: over it, entries of tiles the host still holds go first (oldest
//! delivery first; they may never come back), then the oldest departures. 0 MB = off.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ahash::AHashMap;

use super::{Scheduler, TileState};
use crate::geo::TileKey;
use crate::lights::LightSet;
use crate::mesher::MeshData;
use crate::terrain::tin::TerrainTile;
use crate::worker::TileData;

/// The shareable output of one built tile.
#[derive(Clone, Debug)]
pub struct Kept {
    pub mesh: Arc<MeshData>,
    pub ground: Arc<MeshData>,
    pub lights: Arc<LightSet>,
    pub terrain: Option<Arc<TerrainTile>>,
    pub max_height: f32,
    pub building_count: usize,
}

impl Kept {
    /// The shareable parts of a built tile (no copy).
    pub fn of(d: &TileData) -> Self {
        Self {
            mesh: d.mesh.clone(),
            ground: d.ground.clone(),
            lights: d.lights.clone(),
            terrain: d.terrain.clone(),
            max_height: d.max_height,
            building_count: d.building_count,
        }
    }

    /// Approximate heap bytes (meshes and light arrays).
    fn bytes(&self) -> u64 {
        let l = &self.lights;
        let pts = l.positions.len() + l.clustered_positions.len() + l.street_positions.len() + l.clustered_street_positions.len();
        let mesh = self.mesh.bytes() + self.ground.bytes() + self.terrain.as_ref().map_or(0, |t| t.mesh.bytes());
        (mesh + pts * 28) as u64
    }

    /// The tile data a fresh build would hand over (shares the buffers).
    pub fn to_data(&self) -> TileData {
        TileData {
            building_count: self.building_count,
            lights: self.lights.clone(),
            mesh: self.mesh.clone(),
            ground: self.ground.clone(),
            max_height: self.max_height,
            cpu_us: 0,
            terrain: self.terrain.clone(),
        }
    }
}

/// Counters for `debug_stats()["recent_tiles"]`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecentStats {
    /// Deliveries served from the cache since the scheduler started.
    pub hits: u64,
    /// Entries in the cache and their bytes.
    pub entries: usize,
    pub bytes: u64,
}

/// The cache (see the module docs).
#[derive(Debug)]
struct Entry {
    /// Delivery time while held, then the time it left the host.
    at: Instant,
    gone: bool,
    kept: Kept,
    bytes: u64,
}

#[derive(Debug)]
pub(super) struct RecentTiles {
    map: AHashMap<TileKey, Entry>,
    bytes: u64,
    cap: u64,
    max_age: Duration,
    hits: u64,
    /// Outputs dropped from the cache, to be freed off the main thread.
    released: Vec<Kept>,
}

impl RecentTiles {
    pub fn new(cap_mb: f64, max_age_s: f64) -> Self {
        Self {
            map: AHashMap::new(),
            bytes: 0,
            cap: (cap_mb.max(0.0) * 1_048_576.0) as u64,
            max_age: Duration::from_secs_f64(max_age_s.max(0.0)),
            hits: 0,
            released: Vec::new(),
        }
    }

    /// Keep `k`'s output (a delivery); the clock starts now.
    pub fn keep(&mut self, k: TileKey, kept: Kept, now: Instant) {
        if self.cap == 0 {
            return;
        }
        let b = kept.bytes();
        if b > self.cap {
            self.remove(k);
            return;
        }
        if let Some(old) = self.map.insert(k, Entry { at: now, gone: false, kept, bytes: b }) {
            self.bytes -= old.bytes;
            self.released.push(old.kept);
        }
        self.bytes += b;
        while self.bytes > self.cap {
            let Some(first) = self.map.iter().min_by_key(|(key, e)| (e.gone, e.at, **key)).map(|(key, _)| *key) else { break };
            self.remove(first);
        }
    }

    /// `k` left the host now (dropped or unloaded): its age limit starts.
    pub fn left(&mut self, k: TileKey, now: Instant) {
        if let Some(e) = self.map.get_mut(&k) {
            e.at = now;
            e.gone = true;
        }
    }

    /// The kept output of `k` (a hit): held, or gone for at most the age limit.
    pub fn get(&mut self, k: TileKey, now: Instant) -> Option<Kept> {
        let e = self.map.get(&k)?;
        if e.gone && now.saturating_duration_since(e.at) > self.max_age {
            return None;
        }
        self.hits += 1;
        Some(e.kept.clone())
    }

    /// Drop the departed entries past the age limit.
    pub fn expire(&mut self, now: Instant) {
        if self.map.is_empty() {
            return;
        }
        let max = self.max_age;
        let old: Vec<TileKey> = self.map.iter().filter(|(_, e)| e.gone && now.saturating_duration_since(e.at) > max).map(|(k, _)| *k).collect();
        for k in old {
            self.remove(k);
        }
    }

    fn remove(&mut self, k: TileKey) {
        if let Some(e) = self.map.remove(&k) {
            self.bytes -= e.bytes;
            self.released.push(e.kept);
        }
    }

    /// Outputs dropped since the last call (free them off the main thread).
    pub fn take_released(&mut self) -> Vec<Kept> {
        std::mem::take(&mut self.released)
    }

    pub fn stats(&self) -> RecentStats {
        RecentStats { hits: self.hits, entries: self.map.len(), bytes: self.bytes }
    }
}

impl Scheduler {
    /// Dispatch: a wanted tile whose output is in the recent cache is ready at once, without a
    /// fetch or a mesh job (`dispatch_tiles`). Returns whether it was served.
    pub(super) fn serve_recent(&mut self, k: TileKey) -> bool {
        let now = self.now;
        let wanted = self.entries.get(&k).is_some_and(|e| matches!(e.state, TileState::Queued | TileState::Fetched(_) | TileState::WaitData));
        if !wanted {
            return false;
        }
        let Some(kept) = self.recent.get(k, now) else { return false };
        let Some(e) = self.entries.get_mut(&k) else { return false };
        e.attempts = 0;
        e.decode_failures = 0;
        e.height = Some(kept.max_height);
        e.times.ready = Some(now);
        e.state = TileState::Ready(Box::new(kept.to_data()));
        self.ready.push(k);
        true
    }

    /// Kept outputs the recent-tile cache dropped since the last call, for the caller to free
    /// off the main thread (`Worker::release`).
    pub fn take_released_tiles(&mut self) -> Vec<Kept> {
        self.recent.take_released()
    }

    /// Recent-tile cache counters (`debug_stats()["recent_tiles"]`).
    pub fn recent_stats(&self) -> RecentStats {
        self.recent.stats()
    }
}

#[cfg(test)]
#[path = "recent_tests.rs"]
mod tests;
