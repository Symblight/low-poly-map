//! Main-thread tile state machine (no Godot types, no threads): maintains the quadtree cover,
//! turns it into jobs, applies worker results, and queues ready tiles / unloads / LOD fades for
//! `WorldStreamer`.
//!
//! # Cover
//! The wanted set is a quadtree: z13 roots within 25 km slant distance, each node split into
//! its 4 children while its box is inside the finer band (`rings`). Godot holds the leaves
//! (plus parents during a swap) and evaluates the contract rule from keys: a tile is
//! *present* if it is uploaded or all 4 children are present; it is visible iff uploaded and
//! (z13 or all 4 siblings present) and not all 4 children present, or whenever it is uploaded
//! and has a `poll_fades` entry.
//!
//! # Guarantees towards Godot (contract "Quadtree cover and sibling swap" + "Dithered LOD fades")
//! 1. Refining creates all 4 children at once (one sibling group).
//! 2. Every requested tile is delivered, empty ones too.
//! 3. Refine: when the last of the 4 children is delivered (all 4 uploaded), a fade starts in
//!    the same `poll_ready` call (children 0, parent 1). The parent is unloaded only when the
//!    fade completes, in the same frame its final 0.0 is reported.
//! 4. Coarsen: the dropped parent is fetched (disk cache) and delivered again while its
//!    subtree stays frozen (no new requests or deliveries in it); its delivery starts the
//!    reverse fade (parent 0, children 1); the children are unloaded, in one `poll_unload`
//!    batch, in the frame the fade completes. A parent that is still uploaded with its group
//!    incomplete (children invisible) needs no fade: the subtree goes right away.
//!
//! A root leaving the 25 km disk takes its whole subtree in one batch (no fade).
//!
//! Delivery order: a tile is handed out only when its parent is "effective" (present, and
//! its own sibling group present up to the root) and its parent is not part of a fade, so a
//! sibling group can only become present under a covered parent, never overlaps a coarser
//! visible tile, and every tile takes part in at most one fade. Among those, fresh tiles go
//! nearest to the camera first (`order_key`: the sibling group's slant distance, then the
//! tile's own; no band ranking), and jobs are dispatched in the same order.
//!
//! First fill: a tile delivered while it is already split (a stand-in on the way to finer
//! detail: start, teleport, flying into an unloaded region) is swapped for its 4 children
//! at once, without a fade, when the last of them is delivered; so the next level is never
//! held back by a fade and a first fill has no fades at all. A tile that was a leaf when
//! delivered fades on refine as in guarantee 3.
//!
//! # Fades
//! One fade per swap, keyed by the coarse tile P: `t` = visible fraction of the 4 children,
//! P shows `1 - t`. `t` is driven by P's slant distance `d` from the camera itself (not the
//! look-ahead point, which jumps by up to half a band radius when the velocity changes)
//! across the band's hysteresis gap `0.15 * edge`: refine `t = max(t, t0 + (d0 - d) / gap)` (approaching), coarsen
//! `t = min(t, t0 - (d - d0) / gap)` (receding), anchored where the fade started or last
//! changed direction. It only moves forward, so hovering freezes it; after `fades.cap_s` it is
//! force-completed in its current direction. If the split decision flips during a fade, the
//! fade reverses from its current `t` (no pop). Coarsening is bottom-up: a node merges only
//! once none of its children is split, so the two sides of a fade always differ by one zoom
//! level (the shader's parity rule needs that).
//!
//! At most `fades.max_active` swaps fade at once, both layers together (`World` passes the
//! other layer's count in `set_fades_elsewhere`): the host's dither variant discards, which
//! turns off hidden surface removal on tile-based GPUs. A swap that finds no free slot
//! completes at once, without a fade, like a first-fill swap: a refine drops the parent in
//! the frame its last child is delivered, a coarsen unloads the children in the frame the
//! parent is delivered again. Swaps take slots in delivery order (nearest group first).
//!
//! # Lights
//! Tiles of the bands `lights.min_ring..=3` (default R1..R3) carry single and clustered lights. Crossing 3300 m AGL upwards (2700 m down)
//! switches the mode; already delivered lit tiles are re-delivered under the same key:
//! R3 from the light sets kept here (no mesh), R2 by re-running fetch (disk cache) + mesh.
//!
//! Lifecycle per entry: Queued -> Fetching -> Fetched -> Decoding -> Ready -> (delivered) Idle.
//! Synthetic tiles skip the fetch stage. Errors -> Failed with exponential backoff -> Queued;
//! after `DECODE_ATTEMPTS` decode failures in a row -> Broken (no more jobs; the coarser tile
//! stays). A fetch or mesh job whose entry is removed or reset while it runs (its generation,
//! the job id, went stale) is handed to `World` for `Worker::cancel` (`drain_cancelled`), so the
//! slot frees up before the request or the build finishes (a cancelled mesh job replies
//! `WorkerMsg::Dropped`).
//!
//! # Data tiles (Tiles v2, step T2a)
//! With [`Scheduler::with_data_levels`] (an archive source) the chunks finer than the detail
//! level (R0 z16, R1 z15 with the compatibility levels) are cut from their detail data tile
//! instead of being fetched one by one ([`DataStore`], `scheduler/data.rs`): Queued -> WaitData
//! (data tile fetching / decoding) -> Decoding ([`Job::MeshChunk`], depending on the data tile's
//! decode job or on the resident tile) -> Ready. One fetch and one decode per data tile; the
//! store's tiles live in the memory budget (`World` calls [`Scheduler::sync_memory`]). R2 / R3
//! keep their own fetch as before. The seed world stays per tile.
//!
//! Two-level sources (T2b, [`crate::data::DataLevels::two_level`]): R2 / R3 chunks are cut from
//! the overview data tile the same way (one fetch / decode per overview tile; a light-mode
//! re-delivery re-meshes from the resident tile, the store refetches it from the disk cache
//! only if the budget evicted it). Landmarks are either folded into the chunks (R0 / R1 chunks
//! then wait for their detail AND overview tile, both pinned) or, with the host's opt-in
//! ([`Scheduler::with_landmark_layer`]), drawn by the landmark layer (`scheduler/landmarks.rs`,
//! keys `l/z/x/y`, delivered through this scheduler's `pop_ready` / `drain_landmark_unload`).
//! An archive source starts with unknown levels ([`Scheduler::with_data_pending`]): no vector
//! job is dispatched until the archive opened ([`Scheduler::set_data_levels`]). The synthetic
//! city (T2c) is a two-level source whose data tiles are generated by their decode job.
//!
//! # Layers (api 1.5)
//! One `Scheduler` runs one quadtree layer ([`Layer`]): the vector world (zooms 13..16, the
//! behaviour above, unchanged) or the terrain mesh (zooms `terrain.far_zoom..=near_zoom`, ring =
//! index in `terrain.bands`, no fetch stage, [`Job::Terrain`] jobs, no lights). Every guarantee
//! above holds per layer with the layer's root and leaf zoom. The terrain layer adds the horizon
//! cull (roots and whole sibling groups, with the band hysteresis) and detail re-deliveries
//! (`set_terrain_detail`: held tiles are re-meshed and handed out again under the same key).

mod boxes;
mod data;
mod data_mem;
mod data_needs;
mod fade;
mod landmarks;
mod plan_memo;
mod presence;
mod recent;
pub mod sim;
mod state;
mod terrain_plan;
mod view;
mod delivery;
mod dispatch;
mod inspect;
mod levels;
mod results;
mod swaps;
mod update;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod budget_tests;
#[cfg(test)]
mod plan_memo_tests;

pub use boxes::{ProbeBatch, ProbeResult};
pub use recent::{Kept, RecentStats};
pub use data::{DataStats, DataStore};
pub use landmarks::LandmarkStats;
use landmarks::{LandmarkLayer, LmState};
use data_needs::Need;
use terrain_plan::SplitCandidate;
pub use fade::{fade_gap_m, Fade};
use data::DataState;
pub use state::{CpuStats, Entry, ReadyTile, Stats, TileState};
use state::DECODE_ATTEMPTS;

use std::sync::Arc;
use std::time::{Duration, Instant};

use ahash::AHashMap;

use crate::fetch::FetchErrorKind;
use crate::geo::{Frame, RectEn, TileKey};
use crate::latency::{Latency, Stage, TileTimes};
use crate::profile::Profile;
use crate::rings::{children, Layer, LayerKind, ViewPoint, MAX_BOX_HEIGHT_M, MAX_RINGS, RING_COUNT};
use crate::gen_cache::GenCache;
use crate::terrain::height::ErrorProxy;
use crate::terrain::Terrain;
use crate::jobs::Priority;
use crate::data::DataLevels;
use crate::worker::{DataRef, Job, LmRef, Payload, WorkerMsg};

/// Entries of the tile-box memo ([`Scheduler::terrain_box_of`]).
const TERRAIN_CACHE_CAP: usize = 8192;

// Tuning (world profile): `streaming.max_fetch_per_band` / `max_mesh_per_band` (jobs in flight
// per band), `lights.cluster_on_agl_m` / `cluster_off_agl_m` (light carpet switch),
// `fades.cap_s` (fade safety cap), `streaming.retry_*` (backoff), `bands.*` + `lod.*` (cover).

pub struct Scheduler {
    frame: Frame,
    synthetic: bool,
    /// Heights of the tile boxes and view points (flat unless set with `with_terrain`).
    terrain: Terrain,
    /// `Terrain::tile_range` of root candidates without an entry (a pure function of the key,
    /// so the cache never changes a result; bounded, evicts gradually).
    terrain_cache: GenCache<TileKey, [f64; 2]>,
    profile: Arc<Profile>,
    /// The quadtree layer this scheduler runs (zooms, edges, hysteresis, limits, horizon cull).
    layer: Layer,
    fade_cap: Duration,
    /// `fades.max_active`: most swaps fading at once, both layers together.
    max_fades: usize,
    /// Swaps fading in the other layer (set by `World` before each call that may start one).
    fades_elsewhere: usize,
    /// Coarse tiles of the swaps that found no free slot since the last `update` (completed
    /// at once, without a fade) and their total.
    capped: Vec<TileKey>,
    capped_total: u64,
    entries: AHashMap<TileKey, Entry>,
    ready: Vec<TileKey>,
    unloading: Vec<Entry>,
    /// Refined parents dropped from Godot (entries stay in the tree, not held).
    dropped: Vec<TileKey>,
    /// Jobs (fetch or mesh) still in flight that no entry waits for any more (the tile left the
    /// cover or its pending work became moot): `World` cancels them (`Worker::cancel`) so their
    /// slots free up at once.
    cancelled: Vec<u64>,
    next_job: u64,
    epoch: u64,
    clustered: bool,
    fetch_inflight: [u32; MAX_RINGS],
    decode_inflight: [u32; MAX_RINGS],
    roots: Vec<TileKey>,
    stack: Vec<(TileKey, f64, bool)>,
    cand: Vec<(usize, bool, f64, TileKey)>,
    /// Active LOD fades, keyed by the coarse tile of the swap.
    fades: AHashMap<TileKey, Fade>,
    /// Final values of fades completed this frame (incoming 1.0, outgoing 0.0).
    fade_final: Vec<(TileKey, f32)>,
    /// Time of the last `update` (fade start / safety cap).
    now: Instant,
    /// The camera itself as a view point (no look-ahead): drives the fades.
    cam_view: Option<ViewPoint>,
    /// Stage latencies of fresh deliveries, per band.
    latency: Latency,
    /// Terrain layer: `set_terrain_detail` scale (since G2 a multiplier of `terrain.tau_px`)
    /// and the generation of the meshes the host holds (never bumped since G2: the TIN no longer
    /// depends on the scale, so nothing is re-meshed).
    detail: f64,
    detail_gen: u32,
    /// Terrain layer (G2): the pure per-key error proxy the cover refines by.
    proxy: Option<ErrorProxy>,
    /// Recently delivered outputs, served again without a rebuild (`recent`).
    recent: recent::RecentTiles,
    /// Reused presence memo (`presence`; valid within one pass only).
    presence: presence::Presence,
    /// Async probes of cold boxes / error proxies (`boxes`; `None` = probed inline).
    box_requests: Option<boxes::BoxRequests>,
    /// Terrain layer: the last plan met a value still on the way (not reusable).
    plan_incomplete: bool,
    /// Terrain layer: the host's view (`set_view`) and the refine distance per meter of error,
    /// `H / (2 tan(fov_y / 2) tau_eff)` with tau_eff = `terrain.tau_px` x `detail`.
    terrain_view: crate::rings::LodView,
    sse_m_per_m: f64,
    /// Terrain layer: foundation guard per ring (`terrain::foundation_guard_m`): a tile refines
    /// inside this distance whatever its error.
    guard_m: [f64; MAX_RINGS],
    /// Terrain layer: this frame's target split set (SSE plan + 2:1 balance) and the roots in
    /// the cover (reused buffers), and how many splits the balance added.
    t_split: ahash::AHashSet<TileKey>,
    t_roots: ahash::AHashSet<TileKey>,
    t_work: Vec<TileKey>,
    t_heap: std::collections::BinaryHeap<SplitCandidate>,
    /// Terrain layer: when the last plan can be reused (`plan_memo`).
    plan_memo: plan_memo::PlanMemo,
    /// Tests: recompute every reused plan and assert it equals the fresh one.
    #[cfg(test)]
    pub(crate) verify_plan_reuse: bool,
    balance_forced: usize,
    /// Terrain layer: SSE splits the tile budget (`terrain.max_tiles`) left out in the last
    /// `update` (review M1).
    budget_skipped: usize,
    /// Worker CPU per ring.
    cpu: CpuStats,
    /// Tiles v2 data tiles (archive sources only; `None` = every tile fetched on its own).
    data: Option<DataStore>,
    /// Reused buffers of the data needs pass and of the data jobs.
    data_needs: Vec<(TileKey, f64, Need)>,
    data_decodes: Vec<(TileKey, u64, bytes::Bytes)>,
    data_fetches: Vec<(TileKey, u64)>,
    /// The host draws the landmark layer (`layers` option `"landmarks"`, api 1.11): landmarks
    /// are not folded into the chunks.
    landmark_layer: bool,
    landmarks: LandmarkLayer,
}

impl Scheduler {
    /// The vector layer (zooms 13..16; `synthetic` = no fetch stage).
    pub fn new(frame: Frame, synthetic: bool, profile: Arc<Profile>) -> Self {
        let layer = Layer::world(profile.bands(), profile.streaming.max_fetch_per_band, profile.streaming.max_mesh_per_band);
        Self::with_layer(frame, synthetic, profile, layer)
    }

    /// Vector layer: set the band edges from the host's view and the quality scale (contract
    /// `set_view` / `set_lod_quality`, api 1.10; [`rings::BandParams::edges_for`]). The next
    /// `update` uses them; the hysteresis, look-ahead clamp and fade gap follow the new edges.
    /// Terrain layer (G2): the view of the SSE cover (`quality` is the vector layer's
    /// `set_lod_quality` and does not apply; the terrain's scale is `set_detail`) and the
    /// foundation guard from the vector edges of that view and quality.
    pub fn set_lod_view(&mut self, view: &crate::rings::LodView, quality: f64) {
        match self.layer.kind {
            LayerKind::World => {
                let e = self.profile.bands().edges_for(view, quality);
                self.layer.edge_m[..RING_COUNT].copy_from_slice(&e);
            }
            LayerKind::Terrain => {
                self.terrain_view = *view;
                let b = self.profile.bands();
                let edges = b.edges_for(view, quality);
                self.guard_m = crate::terrain::foundation_guard_m(&self.profile.terrain.bands, &edges, b.hysteresis);
                self.update_sse_scale();
            }
            LayerKind::Landmarks => {}
        }
    }

    /// The terrain-mesh layer of `profile` on `terrain` (active): no fetch stage, terrain jobs,
    /// the SSE cover at the default view and scale 1 (see [`Scheduler::set_lod_view`],
    /// [`Scheduler::set_detail`]).
    pub fn new_terrain(frame: Frame, profile: Arc<Profile>, terrain: Terrain) -> Self {
        let layer = Layer::terrain(&profile.terrain, profile.bands());
        let mut s = Self::with_layer(frame, true, profile, layer).with_terrain(terrain);
        // TIN render path: no edit floors (see `ErrorProxy::without_edit_floor`).
        s.proxy = Some(ErrorProxy::new(&s.terrain, s.layer.root_zoom).without_edit_floor());
        s.set_lod_view(&crate::rings::LodView::default(), 1.0);
        s
    }

    fn update_sse_scale(&mut self) {
        let tau = self.profile.terrain.tau_px * self.detail;
        self.sse_m_per_m = crate::rings::sse_refine_distance(1.0, &self.terrain_view, tau);
    }

    /// Terrain layer: the refine distance (m) of tile `k` of ring `ring` (> 0): the SSE refine
    /// distance of its error proxy, at least the foundation guard. Pure in key, view, scale and
    /// profile.
    pub fn terrain_refine_m(&mut self, k: TileKey, ring: usize) -> f64 {
        let e = self.terrain_error_m(k);
        (e * self.sse_m_per_m).max(self.guard_m[ring])
    }

    /// Terrain layer: the error proxy of `k` (m; 0 on the vector layer). Memoized; the probe
    /// is normally taken once with the tile's box ([`Scheduler::terrain_box_of`]).
    pub fn terrain_error_m(&mut self, k: TileKey) -> f64 {
        self.proxy.as_mut().map_or(0.0, |p| p.error_m(k))
    }

    /// Terrain layer: the foundation guard per ring (m).
    pub fn foundation_guard(&self) -> [f64; MAX_RINGS] {
        self.guard_m
    }

    fn with_layer(frame: Frame, synthetic: bool, profile: Arc<Profile>, layer: Layer) -> Self {
        Self {
            frame,
            synthetic,
            terrain: Terrain::flat(),
            terrain_cache: GenCache::new(TERRAIN_CACHE_CAP),
            layer,
            fade_cap: Duration::from_secs_f64(profile.fades.cap_s),
            recent: recent::RecentTiles::new(profile.memory.recent_tiles_mb, profile.memory.recent_tiles_s),
            max_fades: profile.fades.max_active as usize,
            fades_elsewhere: 0,
            capped: Vec::new(),
            capped_total: 0,
            profile,
            entries: AHashMap::with_capacity(1024),
            ready: Vec::with_capacity(64),
            unloading: Vec::new(),
            dropped: Vec::new(),
            cancelled: Vec::new(),
            next_job: 1,
            epoch: 0,
            clustered: false,
            fetch_inflight: [0; MAX_RINGS],
            decode_inflight: [0; MAX_RINGS],
            roots: Vec::with_capacity(512),
            stack: Vec::with_capacity(256),
            cand: Vec::with_capacity(256),
            fades: AHashMap::with_capacity(64),
            fade_final: Vec::with_capacity(64),
            now: Instant::now(),
            cam_view: None,
            latency: Latency::default(),
            detail: 1.0,
            detail_gen: 0,
            proxy: None,
            box_requests: None,
            plan_incomplete: false,
            presence: Default::default(),
            terrain_view: crate::rings::LodView::default(),
            sse_m_per_m: 0.0,
            guard_m: [0.0; MAX_RINGS],
            t_split: ahash::AHashSet::new(),
            t_roots: ahash::AHashSet::new(),
            t_work: Vec::new(),
            t_heap: std::collections::BinaryHeap::new(),
            plan_memo: Default::default(),
            #[cfg(test)]
            verify_plan_reuse: false,
            budget_skipped: 0,
            balance_forced: 0,
            cpu: CpuStats::default(),
            data: None,
            data_needs: Vec::new(),
            data_decodes: Vec::new(),
            data_fetches: Vec::new(),
            landmark_layer: false,
            landmarks: LandmarkLayer::default(),
        }
    }

    pub fn layer(&self) -> &Layer {
        &self.layer
    }

    /// Worker CPU per ring since start.
    pub fn cpu_stats(&self) -> &CpuStats {
        &self.cpu
    }

    /// Terrain layer: detail scale of the jobs (1 = the profile's `max_error_m`).
    pub fn detail(&self) -> f64 {
        self.detail
    }

    /// Terrain layer (G2): the `set_terrain_detail` scale, a multiplier of `terrain.tau_px`
    /// (the caller clamps; 2 = coarser cover). The cover follows from the next `update` through
    /// the usual refines and coarsens; held tiles are not re-meshed (the TIN of a tile does not
    /// depend on the scale). Returns whether anything changed (no-op on the vector layer and
    /// for the current scale).
    pub fn set_detail(&mut self, scale: f64) -> bool {
        if self.layer.kind != LayerKind::Terrain || !scale.is_finite() || scale == self.detail {
            return false;
        }
        self.detail = scale;
        self.update_sse_scale();
        true
    }

    /// A held tile that shows again (or stays visible) after a change it missed: vector tiles
    /// catch up on light-mode flips (bands >= `lights.min_ring`), terrain tiles on detail
    /// changes. Re-delivered under the same key.
    fn refresh_held(e: &mut Entry, kind: LayerKind, clustered: bool, min_lit_ring: usize, detail_gen: u32) {
        match kind {
            LayerKind::World => {
                if e.ring >= min_lit_ring {
                    Self::mark_relight(e, clustered);
                }
            }
            LayerKind::Terrain => {
                if e.detail_gen != detail_gen && matches!(e.state, TileState::Idle) {
                    e.state = TileState::Queued;
                }
            }
            LayerKind::Landmarks => {}
        }
    }

    /// Use `terrain` for the tile boxes and view-point heights (before the first `update`).
    pub fn with_terrain(mut self, terrain: Terrain) -> Self {
        self.terrain = terrain;
        self.terrain_cache.clear();
        self
    }

    pub fn terrain(&self) -> &Terrain {
        &self.terrain
    }

    pub fn clustered(&self) -> bool {
        self.clustered
    }

    pub fn profile(&self) -> &Arc<Profile> {
        &self.profile
    }

    /// Stage latencies of the last deliveries per band (`debug_stats()["latency"]`).
    pub fn latency(&self) -> &Latency {
        &self.latency
    }
}


/// Note the job of a fetch or mesh job that `e` is about to orphan (removed, or its state reset).
fn orphan_job(cancelled: &mut Vec<u64>, e: &Entry) {
    if matches!(e.state, TileState::Fetching | TileState::Decoding) {
        cancelled.push(e.job);
    }
}
