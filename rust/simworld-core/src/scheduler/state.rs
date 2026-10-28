//! Per-tile state of the scheduler and the plain data it hands out (ready tiles, stats).

use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;

use crate::geo::{RectEn, TileKey};
use crate::latency::TileTimes;
use crate::lights::LightSet;
use crate::mesher::MeshData;
use crate::rings::{LayerKind, MAX_RINGS};
use crate::terrain::tin::TerrainTile;
use crate::worker::TileData;

/// Worker CPU time per ring (sum, max, count) since the scheduler started: the terrain layer's
/// per-tile cost (`Diagnostics` profile log, benches).
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct CpuStats {
    pub n: [u32; MAX_RINGS],
    pub sum_us: [u64; MAX_RINGS],
    pub max_us: [u64; MAX_RINGS],
}

impl CpuStats {
    pub(super) fn record(&mut self, ring: usize, us: u64) {
        if ring < MAX_RINGS {
            self.n[ring] += 1;
            self.sum_us[ring] += us;
            self.max_us[ring] = self.max_us[ring].max(us);
        }
    }

    /// Mean and max (ms) of a ring; `None` without samples.
    pub fn mean_max_ms(&self, ring: usize) -> Option<(f64, f64)> {
        (ring < MAX_RINGS && self.n[ring] > 0)
            .then(|| (self.sum_us[ring] as f64 / self.n[ring] as f64 / 1000.0, self.max_us[ring] as f64 / 1000.0))
    }

    /// `cpu ms mean/max (n) by ring: T0 a/b (n) | ...` over the first `rings` rings.
    pub fn report(&self, label: &str, rings: usize) -> String {
        let mut out = format!("{label} cpu ms mean/max (n) by ring:");
        for r in 0..rings.min(MAX_RINGS) {
            match self.mean_max_ms(r) {
                Some((mean, max)) => out.push_str(&format!(" | {r}: {mean:.1}/{max:.1} ({})", self.n[r])),
                None => out.push_str(&format!(" | {r}: -")),
            }
        }
        out
    }
}

#[derive(Debug)]
pub enum TileState {
    /// Nothing to do (delivered).
    Idle,
    Queued,
    Fetching,
    Fetched(Bytes),
    Decoding,
    Ready(Box<TileData>),
    Failed { retry_at: Instant },
    /// Gave up after [`DECODE_ATTEMPTS`] decode failures in a row: decoding is a pure function
    /// of the bytes (and the terrain build of the key), so retrying only burns work. No more
    /// jobs; the sibling group never completes, so the coarser tile above keeps standing in.
    /// A new entry (the tile leaves the cover and comes back) tries again.
    Broken,
}

impl TileState {
    pub(super) fn label(&self) -> &'static str {
        match self {
            TileState::Idle | TileState::Ready(_) => "ready",
            TileState::Queued => "queued",
            TileState::Fetching => "fetching",
            TileState::Fetched(_) | TileState::Decoding => "meshing",
            TileState::Failed { .. } | TileState::Broken => "failed",
        }
    }
}

/// Decode (or terrain build) failures in a row after which a tile is [`TileState::Broken`].
/// Fetch failures retry forever (with backoff): the source may come back.
pub(super) const DECODE_ATTEMPTS: u32 = 3;

#[derive(Debug)]
pub struct Entry {
    pub key: TileKey,
    pub ring: usize,
    pub state: TileState,
    pub job: u64,
    pub attempts: u32,
    /// Decode failures in a row (reset by a successful decode); see [`DECODE_ATTEMPTS`].
    pub decode_failures: u32,
    /// Slant distance (m) to the look-ahead point of the tile's band (cover decisions).
    pub prio_m: f64,
    /// Slant distance (m) from the camera itself (no look-ahead) to the tile box: the delivery
    /// and job order (nearest first, both layers alike).
    pub cam_d: f64,
    pub(super) mark: u64,
    /// The node's children are part of the cover.
    pub split: bool,
    /// Own box height once decoded.
    pub height: Option<f32>,
    /// Godot holds this key (delivered and not unloaded).
    pub held: bool,
    /// First fill: the tile was delivered while the cover already wanted its children (a
    /// stand-in on the way to finer detail, e.g. right after a teleport or flying into an
    /// unloaded region). When its 4 children are uploaded it is swapped out at once (no fade),
    /// so the next level is not held back by a fade. Cleared when the node stops being split;
    /// a tile that was a leaf when delivered keeps the dithered swap fade.
    pub placeholder: bool,
    /// Light mode (clustered?) of the version Godot holds.
    pub lights_mode: bool,
    /// R3: re-deliver from `lights` (mode change).
    pub relight: bool,
    /// R3: light sets kept for re-delivery.
    pub lights: Option<Arc<LightSet>>,
    /// R3: ground mesh kept for re-delivery (a light re-delivery replaces the whole tile).
    pub ground: Option<Arc<MeshData>>,
    /// Merging back into this (dropped) node: it is being re-delivered; subtree frozen.
    pub coarsening: bool,
    /// Inside a coarsening subtree: kept as is until the ancestor's fade completes.
    pub frozen: bool,
    /// Slant distance (m) of the box from the finer band's look-ahead point (split decision;
    /// 0 for R0).
    pub split_d: f64,
    /// Box height (m) used this frame (own, or inherited while unknown).
    pub box_h: f64,
    /// Terrain [min, max] under the tile (m above MSL; `Terrain::tile_range`, computed once when
    /// the entry is created; `[0, 0]` with the terrain off). The slant-distance box is
    /// `[terrain_box[0], terrain_box[1] + box_h]`.
    pub terrain_box: [f64; 2],
    /// ENU rect and tile center (fixed frame).
    pub rect: RectEn,
    pub center: [f64; 2],
    /// Stage timestamps of the current way to the host (latency stats).
    pub times: TileTimes,
    /// When the node was split (latency stat `swap`, taken when its refine fade starts).
    pub split_at: Option<Instant>,
    /// Terrain layer: detail generation of the version the host holds.
    pub detail_gen: u32,
    /// Triangles of the version the host holds (terrain layer; 0 for vector tiles).
    pub tris: u32,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub queued: [i32; MAX_RINGS],
    pub fetching: [i32; MAX_RINGS],
    pub meshing: [i32; MAX_RINGS],
    pub loaded: [i32; MAX_RINGS],
    pub failed: [i32; MAX_RINGS],
    /// Triangles of every tile the host holds (terrain layer).
    pub tris: u64,
}

pub struct ReadyTile {
    pub layer: LayerKind,
    pub key: TileKey,
    pub ring: usize,
    pub center: [f64; 2],
    pub mesh: MeshData,
    /// Ground mesh (empty for tiles without ground features). R3 light re-deliveries resend
    /// the kept one.
    pub ground: Arc<MeshData>,
    pub lights: Arc<LightSet>,
    pub clustered: bool,
    /// Already uploaded once (light mode change, terrain detail change).
    pub redelivery: bool,
    /// Terrain layer: the terrain mesh (`None` for vector tiles).
    pub terrain: Option<Box<TerrainTile>>,
}

impl ReadyTile {
    pub fn light_arrays(&self) -> (&[[f32; 3]], &[[f32; 4]]) {
        self.lights.get(self.clustered)
    }
    pub fn street_light_arrays(&self) -> (&[[f32; 3]], &[[f32; 4]]) {
        self.lights.get_street(self.clustered)
    }
    /// Beacons are never clustered.
    pub fn beacon_arrays(&self) -> (&[[f32; 3]], &[[f32; 4]]) {
        (&self.lights.beacon_positions, &self.lights.beacon_colors)
    }
    /// Airport lights of a seed world (api 1.7; empty elsewhere). Never clustered.
    pub fn airport_arrays(&self) -> (&[[f32; 3]], &[[f32; 4]]) {
        (&self.lights.airport_positions, &self.lights.airport_colors)
    }
}

impl Entry {
    pub(super) fn blank(key: TileKey, ring: usize, rect: RectEn, center: [f64; 2], box_h: f64) -> Self {
        Self {
            key,
            ring,
            state: TileState::Queued,
            job: 0,
            attempts: 0,
            decode_failures: 0,
            prio_m: 0.0,
            cam_d: 0.0,
            mark: 0,
            split: false,
            height: None,
            held: false,
            placeholder: false,
            lights_mode: false,
            relight: false,
            lights: None,
            ground: None,
            coarsening: false,
            frozen: false,
            split_d: 0.0,
            box_h,
            terrain_box: [0.0, 0.0],
            rect,
            center,
            times: TileTimes::default(),
            split_at: None,
            detail_gen: 0,
            tris: 0,
        }
    }
}
