//! The landmark layer (api 1.11; Tiles and LOD v2 step T2b, spec section 4 "ADD refinement"):
//! one tile per overview data tile that has landmarks, key `l/z/x/y` at the overview zoom.
//!
//! Not a quadtree: root zoom = leaf zoom, so the host's visibility rule degenerates to "visible
//! while uploaded". A tile is wanted while the slant distance from the camera to its box is
//! within `bands.max_distance_m` (the band hysteresis on the way out), built once from the
//! resident overview data tile (the vector layer's [`super::DataStore`]: one fetch / decode
//! serves R2, R3 and the landmarks), delivered once (fresh, nearest first, sharing `poll_ready`
//! with the other layers), never re-delivered (its lights are the same in both light modes),
//! never in `poll_fades`, and unloaded through `poll_unload` when it leaves the range. An
//! overview tile without landmarks is never delivered (and so never unloaded).

use std::time::Instant;

use ahash::AHashMap;

use crate::geo::{Frame, RectEn, TileKey};
use crate::worker::TileData;

#[derive(Debug)]
pub(super) enum LmState {
    /// Wanted, waiting for its overview data tile (or for a mesh slot).
    Wait,
    Building,
    Ready(Box<TileData>),
    /// Delivered (held by the host).
    Idle,
    /// Built: no landmarks (never delivered).
    Empty,
    Failed { retry_at: Instant },
    Broken,
}

#[derive(Debug)]
pub(super) struct LmEntry {
    pub key: TileKey,
    pub state: LmState,
    pub held: bool,
    pub job: u64,
    pub center: [f64; 2],
    pub rect: RectEn,
    /// Slant distance from the camera to the tile box (delivery and job order).
    pub cam_d: f64,
    /// Highest landmark roof (m) once built (the box height).
    pub height: f64,
    pub failures: u32,
    pub mark: u64,
}

impl LmEntry {
    fn new(key: TileKey, frame: &Frame, default_h: f64) -> Self {
        Self {
            key,
            state: LmState::Wait,
            held: false,
            job: 0,
            center: frame.tile_center(key),
            rect: frame.tile_rect(key),
            cam_d: f64::INFINITY,
            height: default_h,
            failures: 0,
            mark: 0,
        }
    }
}

/// Counters of the landmark layer (`debug_stats`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LandmarkStats {
    /// Tiles in range (wanted).
    pub tracked: u32,
    /// Delivered and held by the host.
    pub loaded: u32,
    /// Waiting for data or being meshed.
    pub pending: u32,
    /// Built without landmarks (not delivered).
    pub empty: u32,
    /// Landmark tiles delivered since the scheduler started.
    pub delivered: u64,
}

#[derive(Debug, Default)]
pub(super) struct LandmarkLayer {
    pub entries: AHashMap<TileKey, LmEntry>,
    pub unloading: Vec<TileKey>,
    pub inflight: u32,
    pub delivered: u64,
    pub epoch: u64,
}

impl LandmarkLayer {
    pub fn touch(&mut self, k: TileKey, frame: &Frame, default_h: f64) -> &mut LmEntry {
        let epoch = self.epoch;
        let e = self.entries.entry(k).or_insert_with(|| LmEntry::new(k, frame, default_h));
        e.mark = epoch;
        e
    }

    pub fn stats(&self) -> LandmarkStats {
        let mut s = LandmarkStats { tracked: self.entries.len() as u32, delivered: self.delivered, ..Default::default() };
        for e in self.entries.values() {
            if e.held {
                s.loaded += 1;
            }
            match e.state {
                LmState::Wait | LmState::Building | LmState::Ready(_) | LmState::Failed { .. } => s.pending += 1,
                LmState::Empty => s.empty += 1,
                LmState::Idle | LmState::Broken => {}
            }
        }
        s
    }
}
