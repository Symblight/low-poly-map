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
//! keep their own fetch as before. The synthetic city and the seed world stay per tile.
//!
//! # Layers (api 1.5)
//! One `Scheduler` runs one quadtree layer ([`Layer`]): the vector world (zooms 13..16, the
//! behaviour above, unchanged) or the terrain mesh (zooms `terrain.far_zoom..=near_zoom`, ring =
//! index in `terrain.bands`, no fetch stage, [`Job::Terrain`] jobs, no lights). Every guarantee
//! above holds per layer with the layer's root and leaf zoom. The terrain layer adds the horizon
//! cull (roots and whole sibling groups, with the band hysteresis) and detail re-deliveries
//! (`set_terrain_detail`: held tiles are re-meshed and handed out again under the same key).

mod data;
mod fade;
pub mod sim;
mod state;
#[cfg(test)]
mod tests;

pub use data::{DataStats, DataStore};
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
use crate::mesher::MeshData;
use crate::profile::Profile;
use crate::rings::{above_horizon, children, horizon_line_m, look_ahead, Layer, LayerKind, ViewPoint, MAX_BOX_HEIGHT_M, MAX_RINGS};
use crate::terrain::Terrain;
use crate::jobs::Priority;
use crate::worker::{DataRef, Job, Payload, WorkerMsg};

// Tuning (world profile): `streaming.max_fetch_per_band` / `max_mesh_per_band` (jobs in flight
// per band), `lights.cluster_on_agl_m` / `cluster_off_agl_m` (light carpet switch),
// `fades.cap_s` (fade safety cap), `streaming.retry_*` (backoff), `bands.*` (cover).

pub struct Scheduler {
    frame: Frame,
    synthetic: bool,
    /// Heights of the tile boxes and view points (flat unless set with `with_terrain`).
    terrain: Terrain,
    /// `Terrain::tile_range` of root candidates without an entry (a pure function of the key,
    /// so the cache never changes a result; cleared when it grows large).
    terrain_cache: AHashMap<TileKey, [f64; 2]>,
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
    /// Terrain layer: detail scale of new jobs and its generation (bumped by
    /// `set_terrain_detail`).
    detail: f64,
    detail_gen: u32,
    /// Worker CPU per ring.
    cpu: CpuStats,
    /// Tiles v2 data tiles (archive sources only; `None` = every tile fetched on its own).
    data: Option<DataStore>,
    /// Reused buffers of the data needs pass and of the data jobs.
    data_needs: Vec<(TileKey, f64, bool)>,
    data_decodes: Vec<(TileKey, u64, bytes::Bytes)>,
    data_fetches: Vec<(TileKey, u64)>,
}

impl Scheduler {
    /// The vector layer (zooms 13..16; `synthetic` = no fetch stage).
    pub fn new(frame: Frame, synthetic: bool, profile: Arc<Profile>) -> Self {
        let layer = Layer::world(profile.bands(), profile.streaming.max_fetch_per_band, profile.streaming.max_mesh_per_band);
        Self::with_layer(frame, synthetic, profile, layer)
    }

    /// The terrain-mesh layer of `profile` on `terrain` (active): no fetch stage, terrain jobs.
    pub fn new_terrain(frame: Frame, profile: Arc<Profile>, terrain: Terrain) -> Self {
        let layer = Layer::terrain(&profile.terrain, profile.bands());
        Self::with_layer(frame, true, profile, layer).with_terrain(terrain)
    }

    fn with_layer(frame: Frame, synthetic: bool, profile: Arc<Profile>, layer: Layer) -> Self {
        Self {
            frame,
            synthetic,
            terrain: Terrain::flat(),
            terrain_cache: AHashMap::new(),
            layer,
            fade_cap: Duration::from_secs_f64(profile.fades.cap_s),
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
            cpu: CpuStats::default(),
            data: None,
            data_needs: Vec::new(),
            data_decodes: Vec::new(),
            data_fetches: Vec::new(),
        }
    }

    /// Cut the chunks finer than `levels.detail` from their detail data tile (Tiles v2, T2a).
    /// Vector layer with an archive source only (a synthetic scheduler ignores it).
    pub fn with_data_levels(mut self, levels: crate::data::DataLevels) -> Self {
        if self.layer.kind == LayerKind::World && !self.synthetic {
            let max = self.profile.streaming.max_fetch_per_band * 2;
            self.data = Some(DataStore::new(levels, max));
        }
        self
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

    /// Unregister the data tiles from the budget (before the scheduler is dropped).
    pub fn release_memory(&mut self, mem: &mut crate::budget::MemoryBudget) {
        if let Some(d) = &mut self.data {
            d.release(mem);
        }
    }

    /// The detail data tile chunk `k` is cut from (`None`: the tile is fetched on its own).
    fn data_key(&self, k: TileKey) -> Option<TileKey> {
        self.data.as_ref().and_then(|d| d.levels.detail_of(k))
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

    /// Delivery order of the nearest chunk waiting for data tile `k` (infinite if none).
    pub fn data_distance(&self, k: TileKey) -> f64 {
        self.data.as_ref().and_then(|d| d.get(k)).map_or(f64::INFINITY, |e| e.want)
    }

    /// Data needs of this frame: which data tiles the chunks of the cover wait for or use;
    /// cancels the data work nobody needs (see [`DataStore::end_needs`]).
    fn update_data_needs(&mut self, now: Instant) {
        if self.data.is_none() {
            return;
        }
        let mut needs = std::mem::take(&mut self.data_needs);
        needs.clear();
        for e in self.entries.values() {
            let Some(dk) = self.data_key(e.key) else { continue };
            match e.state {
                TileState::Queued | TileState::WaitData if !e.frozen => needs.push((dk, self.order_key(e).0, false)),
                TileState::Decoding => needs.push((dk, f64::INFINITY, true)),
                _ => {}
            }
        }
        let cam = self.cam_view;
        let ds = self.data.as_mut().expect("checked");
        ds.begin_needs(now);
        for &(dk, order, in_use) in &needs {
            if in_use {
                ds.use_tile(dk);
            } else {
                ds.want(dk, order);
            }
        }
        ds.end_needs(&mut self.cancelled, |k| cam.map_or(f64::INFINITY, |v| v.slant_box_m(k, 0.0, 0.0)));
        self.data_needs = needs;
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

    /// Terrain layer: mesh new jobs at `scale` times the bands' `max_error_m` (the caller clamps)
    /// and re-mesh every tile the host holds, re-delivered under the same key (pending work is
    /// restarted; hidden split parents are refreshed if they ever show again). Returns whether
    /// anything changed (no-op on the vector layer and for the current scale).
    pub fn set_detail(&mut self, scale: f64) -> bool {
        if self.layer.kind != LayerKind::Terrain || !scale.is_finite() || scale == self.detail {
            return false;
        }
        self.detail = scale;
        self.detail_gen += 1;
        let mut next = self.next_job;
        for e in self.entries.values_mut() {
            match e.state {
                TileState::Decoding | TileState::Ready(_) | TileState::Fetched(_) => {
                    // Built (or being built) at the old detail: orphan the result, build again.
                    orphan_job(&mut self.cancelled, e);
                    e.state = TileState::Queued;
                    e.job = next;
                    next += 1;
                }
                TileState::Idle if e.held && !e.split => e.state = TileState::Queued,
                _ => {}
            }
        }
        self.next_job = next;
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

    /// Terrain [min, max] under a tile: the entry's, else the cache, else `Terrain::tile_range`
    /// (cached). `[0, 0]` with the terrain off.
    fn terrain_box_of(&mut self, k: TileKey) -> [f64; 2] {
        if !self.terrain.is_active() {
            return [0.0, 0.0];
        }
        if let Some(e) = self.entries.get(&k) {
            return e.terrain_box;
        }
        if let Some(g) = self.terrain_cache.get(&k) {
            return *g;
        }
        if self.terrain_cache.len() >= 8192 {
            self.terrain_cache.clear();
        }
        let g = self.terrain.tile_range(k);
        self.terrain_cache.insert(k, g);
        g
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

    /// Look-ahead view point per band (entries past the layer's ring count repeat the last
    /// one). `cam` = (east, north, AGL), `vel` = (east, north, up).
    pub fn views(&self, cam: [f64; 3], vel: [f64; 3]) -> [ViewPoint; MAX_RINGS] {
        self.views_ground(cam, vel).0
    }

    /// [`Scheduler::views`] plus the terrain height under each view point.
    fn views_ground(&self, cam: [f64; 3], vel: [f64; 3]) -> ([ViewPoint; MAX_RINGS], [f64; MAX_RINGS]) {
        let l = &self.layer;
        let last = l.rings() - 1;
        let mut ground = [0.0; MAX_RINGS];
        // The camera's MSL altitude (AGL + the terrain under it); the look-ahead moves it by the
        // velocity, so a hill under the camera does not become a hill under every view point.
        let (clat, clon) = self.frame.lat_lon(cam[0], cam[1]);
        let cam_msl = [cam[0], cam[1], cam[2] + self.terrain.height(clat, clon)];
        let views = std::array::from_fn(|i| {
            let r = i.min(last);
            let la = look_ahead(cam_msl, vel, l.look_ahead_clamp_m(r), l.look_ahead_s);
            let (lat, lon) = self.frame.lat_lon(la[0], la[1]);
            // Terrain scale of the tile boxes (terrain hook; 0 while flat). Never below ground.
            let g = self.terrain.height(lat, lon);
            ground[i] = g;
            ViewPoint::new(lat, lon, la[2].max(g))
        });
        (views, ground)
    }

    /// Horizon cull of the layer (none unless `horizon_margin_m` is set): whether tile `k` with
    /// terrain box `g` is above the horizon seen from `view` over ground `ground`, with
    /// hysteresis (`current` = it is in the cover now).
    fn horizon_ok(&self, view: &ViewPoint, ground: f64, k: TileKey, g: [f64; 2], current: bool) -> bool {
        match self.layer.horizon_margin_m {
            None => true,
            Some(margin) => {
                let line = horizon_line_m(view.alt, ground, g[0], g[1] + self.layer.default_tile_height_m, margin);
                above_horizon(view.horizontal_m(k), line, self.layer.hysteresis, current)
            }
        }
    }

    /// Recompute the cover around the camera and push new jobs into `jobs`.
    /// `cam` = (east, north, AGL) meters, `vel` = (east, north, up) m/s.
    pub fn update(&mut self, cam: [f64; 3], vel: [f64; 3], now: Instant, jobs: &mut Vec<Job>) {
        self.epoch += 1;
        self.now = now;
        let l = self.layer;
        let kind = l.kind;
        let top = l.rings() - 1;
        let default_h = l.default_tile_height_m;
        let detail_gen = self.detail_gen;
        // Final fade values are reported once, by the `poll_fades` of the frame they completed in.
        self.fade_final.clear();
        self.capped.clear();
        if kind == LayerKind::World {
            self.update_light_mode(cam[2]);
        }
        let (views, grounds) = self.views_ground(cam, vel);
        let cam_view = self.views(cam, [0.0; 3])[0];
        self.cam_view = Some(cam_view);

        // 1. Roots and the tree below them (hysteresis on both; terrain: horizon cull too).
        self.roots.clear();
        let max_top = if self.terrain.is_active() { self.terrain.max_height() + MAX_BOX_HEIGHT_M } else { MAX_BOX_HEIGHT_M };
        views[top].root_candidates_layer(&l, max_top, &mut self.roots);
        for i in 0..self.roots.len() {
            let k = self.roots[i];
            let (existing, h) = match self.entries.get(&k) {
                Some(e) => (true, e.height.map_or(default_h, |h| h as f64)),
                None => (false, default_h),
            };
            let g = self.terrain_box_of(k);
            let d = views[top].slant_box_m(k, g[0], g[1] + h);
            if l.inside(d, l.edge_m[top], existing) && self.horizon_ok(&views[top], grounds[top], k, g, existing) {
                self.stack.push((k, default_h, false));
            }
        }
        while let Some((k, inherited_h, frozen)) = self.stack.pop() {
            let Some(ring) = l.ring_for_zoom(k.z) else { continue };
            if frozen {
                // Keep the subtree exactly as it is until the coarsening ancestor is back.
                let e = self.touch(k, ring);
                e.frozen = true;
                let (split, h) = (e.split, e.height.map_or(inherited_h, |h| h as f64));
                if split {
                    for c in children(k) {
                        self.stack.push((c, h, true));
                    }
                }
                continue;
            }
            let present = self.present(k);
            let clustered = self.clustered;
            let min_lit_ring = self.profile.lit_min_ring();
            let fading = self.fades.contains_key(&k);
            // Bottom-up coarsening: a node merges only once none of its children is split.
            let kids_split = k.z < l.leaf_zoom
                && children(k).iter().any(|c| self.entries.get(c).is_some_and(|e| e.split));
            // Horizon cull of whole sibling groups: split only while one of the 4 children is
            // above the horizon (seen from the finer band's look-ahead point).
            let group_visible = ring == 0 || l.horizon_margin_m.is_none() || {
                let current = self.entries.get(&k).is_some_and(|e| e.split);
                let mut any = false;
                for c in children(k) {
                    let g = self.terrain_box_of(c);
                    if self.horizon_ok(&views[ring - 1], grounds[ring - 1], c, g, current) {
                        any = true;
                        break;
                    }
                }
                any
            };
            let mut orphan = false;
            let mut cancel: Option<u64> = None;
            let mut retarget: Option<bool> = None;
            let (split, freeze, h) = {
                let e = self.touch(k, ring);
                e.frozen = false;
                let h = e.height.map_or(inherited_h, |h| h as f64);
                e.box_h = h;
                let (lo, hi) = (e.terrain_box[0], e.terrain_box[1] + h);
                e.prio_m = views[ring].slant_box_m(k, lo, hi);
                e.cam_d = cam_view.slant_box_m(k, lo, hi);
                e.split_d = if ring > 0 { views[ring - 1].slant_box_m(k, lo, hi) } else { 0.0 };
                let want = ring > 0 && group_visible && l.inside(e.split_d, l.edge_m[ring - 1], e.split);
                let mut freeze = false;
                if want {
                    if e.coarsening {
                        // Split again before the coarsening finished: keep the children.
                        e.coarsening = false;
                        if fading {
                            retarget = Some(true); // coarsen fade -> refine fade
                        } else if !e.held && !matches!(e.state, TileState::Idle) {
                            // Cancel the pending re-delivery (orphan any in-flight result).
                            if matches!(e.state, TileState::Fetching | TileState::Decoding) {
                                cancel = Some(e.job);
                            }
                            e.state = TileState::Idle;
                            orphan = true;
                        }
                    }
                    if !e.split {
                        e.split_at = Some(now);
                    }
                    e.split = true;
                } else if e.split {
                    if fading {
                        // Refine fade (parent held) turns into a coarsen fade, or a coarsen fade
                        // continues: the children stay frozen until it completes.
                        e.coarsening = true;
                        retarget = Some(false);
                        freeze = true;
                    } else if e.held || !present {
                        // Parent still uploaded and its group incomplete (children invisible),
                        // or nothing below is present: drop the subtree now.
                        e.split = false;
                        e.split_at = None;
                        e.coarsening = false;
                        if e.held {
                            // Visible again: catch up on light-mode flips / detail changes
                            // missed while split.
                            Self::refresh_held(e, kind, clustered, min_lit_ring, detail_gen);
                        }
                    } else if kids_split && !e.coarsening {
                        // Wait for the children to merge first (keeps fades one level deep).
                    } else {
                        // Guarantee 4: deliver the parent again, then fade the children out.
                        if !e.coarsening && matches!(e.state, TileState::Idle) {
                            e.state = TileState::Queued;
                            e.times = TileTimes::wanted(now);
                        }
                        e.coarsening = true;
                        freeze = true;
                    }
                }
                if !e.split {
                    e.placeholder = false;
                }
                (e.split, freeze, h)
            };
            self.cancelled.extend(cancel);
            if orphan {
                let job = self.next_job;
                self.next_job += 1;
                if let Some(e) = self.entries.get_mut(&k) {
                    e.job = job;
                }
            }
            if let Some(refine) = retarget {
                let d = self.fade_distance(k);
                if let Some(f) = self.fades.get_mut(&k) {
                    f.retarget(refine, d);
                }
            }
            if split {
                for c in children(k) {
                    self.stack.push((c, h, freeze));
                }
            }
        }

        // 2. Everything not visited left the cover: subtrees go in one unload batch.
        let epoch = self.epoch;
        let unloading = &mut self.unloading;
        let ready = &mut self.ready;
        let cancelled = &mut self.cancelled;
        self.entries.retain(|_, e| {
            if e.mark == epoch {
                return true;
            }
            orphan_job(cancelled, e);
            if e.held {
                let mut blank = Entry::blank(e.key, e.ring, e.rect, e.center, default_h);
                blank.terrain_box = e.terrain_box;
                let mut u = std::mem::replace(e, blank);
                u.state = TileState::Idle;
                u.split = false;
                u.placeholder = false;
                u.relight = false;
                unloading.push(u);
            }
            if matches!(e.state, TileState::Ready(_)) {
                ready.retain(|k| *k != e.key);
            }
            false
        });
        // Fades whose swap lost a side (a root left the disk: whole subtree unloaded) end here.
        let entries = &self.entries;
        self.fades.retain(|k, _| {
            entries.get(k).is_some_and(|e| e.held && e.split)
                && children(*k).iter().all(|c| entries.get(c).is_some_and(|e| e.held))
        });

        // 3. Retries whose backoff elapsed.
        for e in self.entries.values_mut() {
            if let TileState::Failed { retry_at } = e.state {
                if now >= retry_at {
                    e.state = TileState::Queued;
                }
            }
        }

        // 4. Refined parents whose 4 children are uploaded start their fade (guarantee 3).
        self.cand.clear();
        for e in self.entries.values() {
            if e.held && e.split && !e.coarsening && !e.frozen && !self.fades.contains_key(&e.key) {
                self.cand.push((e.ring, false, 0.0, e.key));
            }
        }
        for i in 0..self.cand.len() {
            let k = self.cand[i].3;
            self.try_start_refine(k);
        }

        // 5. Advance the fades; completed ones swap for good.
        self.advance_fades(now);

        // 6. Data tiles (Tiles v2): what the chunks of the cover wait for.
        self.update_data_needs(now);

        self.dispatch(jobs);
    }

    fn update_light_mode(&mut self, agl: f64) {
        let (on, off) = (self.profile.lights.cluster_on_agl_m, self.profile.lights.cluster_off_agl_m);
        let want = if agl > on {
            true
        } else if agl < off {
            false
        } else {
            self.clustered
        };
        if want == self.clustered {
            return;
        }
        self.clustered = want;
        let min_ring = self.profile.lit_min_ring();
        for e in self.entries.values_mut() {
            // A split parent is hidden behind its children: it is re-lit if it ever returns.
            if !e.held || e.split || e.ring < min_ring {
                continue;
            }
            Self::mark_relight(e, want);
        }
    }

    /// A held lit entry (band >= `lights.min_ring`) whose delivered lights differ from `mode` gets re-delivered.
    fn mark_relight(e: &mut Entry, mode: bool) {
        let stale = e.lights_mode != mode;
        if e.ring == 3 && e.lights.is_some() {
            e.relight = stale;
        } else if stale && matches!(e.state, TileState::Idle) {
            // R2 (or an R3 without kept lights): rebuild from the disk cache.
            e.state = TileState::Queued;
        }
    }

    /// Get or create the entry for a visited node. (An entry that left the cover is never
    /// restored from `unloading`: the host drains it in the same frame, contract call order.)
    fn touch(&mut self, k: TileKey, ring: usize) -> &mut Entry {
        let epoch = self.epoch;
        if !self.entries.contains_key(&k) {
            let terrain_box = self.terrain_box_of(k);
            let mut e =
                Entry::blank(k, ring, self.frame.tile_rect(k), self.frame.tile_center(k), self.layer.default_tile_height_m);
            e.terrain_box = terrain_box;
            e.times = TileTimes::wanted(self.now);
            self.terrain_cache.remove(&k);
            self.entries.insert(k, e);
        }
        let e = self.entries.get_mut(&k).expect("inserted above");
        e.mark = epoch;
        e
    }

    fn dispatch(&mut self, jobs: &mut Vec<Job>) {
        // Data tiles first: decodes of fetched tiles (their chunk jobs below can depend on them).
        if let Some(ds) = &mut self.data {
            ds.take_decodes(&mut self.next_job, &mut self.data_decodes);
            jobs.extend(self.data_decodes.drain(..).map(|(key, job, bytes)| Job::DecodeData { key, job, bytes }));
        }
        self.dispatch_tiles(jobs);
        if let Some(ds) = &mut self.data {
            ds.take_fetches(&mut self.next_job, &mut self.data_fetches);
            jobs.extend(self.data_fetches.drain(..).map(|(key, job)| Job::FetchData { key, job }));
        }
    }

    fn dispatch_tiles(&mut self, jobs: &mut Vec<Job>) {
        let max_fetch = self.layer.max_fetch;
        let max_decode = self.layer.max_mesh;
        let full = (0..self.layer.rings())
            .all(|r| self.decode_inflight[r] >= max_decode && (self.synthetic || self.fetch_inflight[r] >= max_fetch));
        if full {
            return;
        }
        self.cand.clear();
        for e in self.entries.values() {
            if !e.frozen && matches!(e.state, TileState::Queued | TileState::Fetched(_) | TileState::WaitData) {
                self.cand.push((e.ring, e.held, self.order_key(e).0, e.key));
            }
        }
        if self.cand.is_empty() {
            return;
        }
        // Fresh tiles before re-deliveries, then the group distance from the camera (the
        // delivery order, `order_key`), ring and key as deterministic tie-breakers. Slots are per band, so a band's
        // jobs never wait for another band's; the order matters for the shared worker queue,
        // where the near tiles' ancestors (nearest in their own band) go first.
        self.cand
            .sort_unstable_by(|a, b| a.1.cmp(&b.1).then(a.2.total_cmp(&b.2)).then(a.0.cmp(&b.0)).then(a.3.cmp(&b.3)));
        for i in 0..self.cand.len() {
            let (ring, _, _, key) = self.cand[i];
            if let Some(dk) = self.data_key(key) {
                self.dispatch_chunk(key, ring, dk, max_decode, jobs);
                continue;
            }
            let Some(e) = self.entries.get_mut(&key) else { continue };
            let decode_stage = self.synthetic || matches!(e.state, TileState::Fetched(_));
            if decode_stage {
                if self.decode_inflight[ring] >= max_decode {
                    continue;
                }
                let payload = match std::mem::replace(&mut e.state, TileState::Decoding) {
                    TileState::Fetched(b) => Payload::Bytes(b),
                    _ => Payload::Synthetic,
                };
                e.job = self.next_job;
                e.times.decode_start = Some(self.now);
                self.next_job += 1;
                self.decode_inflight[ring] += 1;
                match self.layer.kind {
                    LayerKind::World => jobs.push(Job::Decode { key, ring, job: e.job, payload }),
                    LayerKind::Terrain => jobs.push(Job::Terrain { key, ring, job: e.job, detail: self.detail }),
                }
            } else {
                if self.fetch_inflight[ring] >= max_fetch {
                    continue;
                }
                e.state = TileState::Fetching;
                e.times.fetch_start = Some(self.now);
                e.job = self.next_job;
                self.next_job += 1;
                self.fetch_inflight[ring] += 1;
                jobs.push(Job::Fetch { key, ring, job: e.job });
            }
        }
    }

    /// A chunk cut from data tile `dk` (Tiles v2): a mesh job on the resident or decoding data
    /// tile, or wait for it (the store fetches it).
    fn dispatch_chunk(&mut self, key: TileKey, ring: usize, dk: TileKey, max_decode: u32, jobs: &mut Vec<Job>) {
        let now = self.now;
        let Some(ds) = self.data.as_mut() else { return };
        let Some(e) = self.entries.get_mut(&key) else { return };
        let (data, source) = match ds.get(dk) {
            Some(d) => match &d.state {
                DataState::Resident(a) => (Some(DataRef::Ready(a.clone())), d.source),
                DataState::Decoding { job } => (Some(DataRef::After(*job)), d.source),
                DataState::Broken => {
                    // The data tile cannot be decoded: like a broken tile, the coarser one stays.
                    e.state = TileState::Broken;
                    return;
                }
                _ => (None, None),
            },
            None => (None, None),
        };
        let Some(data) = data else {
            if !matches!(e.state, TileState::WaitData) {
                e.state = TileState::WaitData;
                e.times.fetch_start.get_or_insert(now);
            }
            return;
        };
        if self.decode_inflight[ring] >= max_decode {
            return;
        }
        // Latency: `fetch_*` = waiting for the data tile (fetch + decode), when it had to wait.
        if e.times.fetch_start.is_some() {
            e.times.fetched = Some(now);
            e.times.source = source;
        }
        e.state = TileState::Decoding;
        e.job = self.next_job;
        e.times.decode_start = Some(now);
        self.next_job += 1;
        self.decode_inflight[ring] += 1;
        ds.pin(dk);
        jobs.push(Job::MeshChunk { key, ring, job: e.job, data });
    }

    /// Apply a worker result. Returns a warning to log (main thread) on failure.
    pub fn handle(&mut self, msg: WorkerMsg, now: Instant) -> Option<String> {
        match msg {
            WorkerMsg::Info(_) | WorkerMsg::Warn(_) | WorkerMsg::CacheDisabled(_) | WorkerMsg::Error(_) => None,
            WorkerMsg::DataFetched { key, job, result } => {
                let profile = self.profile.clone();
                self.data.as_mut().and_then(|d| d.fetched(key, job, result, now, &profile))
            }
            WorkerMsg::DataDecoded { key, job, result } => {
                let profile = self.profile.clone();
                self.data.as_mut().and_then(|d| d.decoded(key, job, result, now, &profile))
            }
            WorkerMsg::DataDropped { key, job } => {
                if let Some(d) = self.data.as_mut() {
                    d.dropped(key, job);
                }
                None
            }
            WorkerMsg::Fetched { key, ring, job, result } => {
                self.fetch_inflight[ring] = self.fetch_inflight[ring].saturating_sub(1);
                let e = self.entries.get_mut(&key).filter(|e| e.job == job)?;
                if !matches!(e.state, TileState::Fetching) {
                    return None;
                }
                match result {
                    Ok((bytes, source)) => {
                        e.times.fetched = Some(now);
                        e.times.source = Some(source);
                        e.state = TileState::Fetched(bytes);
                        None
                    }
                    // Only orphaned jobs are cancelled; should a current one be, ask again.
                    Err(err) if err.kind == FetchErrorKind::Cancelled => {
                        e.state = TileState::Queued;
                        None
                    }
                    Err(err) => Some(Self::fail(e, now, "fetch", &err.message, &self.profile)),
                }
            }
            WorkerMsg::Dropped { key, ring, job, .. } => {
                // A cancelled mesh job: only orphaned generations are cancelled; should a
                // current one be (shutdown race, or its data tile's decode was cancelled or
                // finished before the job was queued), ask again.
                self.decode_inflight[ring] = self.decode_inflight[ring].saturating_sub(1);
                self.unpin_data(key);
                let e = self.entries.get_mut(&key).filter(|e| e.job == job)?;
                if matches!(e.state, TileState::Decoding) {
                    e.state = TileState::Queued;
                }
                None
            }
            WorkerMsg::Decoded { key, ring, job, result } | WorkerMsg::TerrainBuilt { key, ring, job, result } => {
                self.decode_inflight[ring] = self.decode_inflight[ring].saturating_sub(1);
                self.unpin_data(key);
                if let Ok(d) = &result {
                    self.cpu.record(ring, d.cpu_us);
                }
                let e = self.entries.get_mut(&key).filter(|e| e.job == job)?;
                if !matches!(e.state, TileState::Decoding) {
                    return None;
                }
                match result {
                    Ok(data) => {
                        e.attempts = 0;
                        e.decode_failures = 0;
                        e.height = Some(data.max_height);
                        // The delivered surface replaces the 9 x 9 estimate where it is wider
                        // (never narrower: a tighter box would only churn the cover). The TIN
                        // is within `max_error` of the height function.
                        if let Some(t) = &data.terrain {
                            let err = t.max_error as f64;
                            e.terrain_box[0] = e.terrain_box[0].min(t.h_min as f64 - err);
                            e.terrain_box[1] = e.terrain_box[1].max(t.h_max as f64 + err);
                        }
                        e.times.ready = Some(now);
                        e.times.cpu_us = data.cpu_us;
                        e.state = TileState::Ready(Box::new(data));
                        self.ready.push(key);
                        None
                    }
                    Err(err) => {
                        e.decode_failures += 1;
                        if e.decode_failures < DECODE_ATTEMPTS {
                            return Some(Self::fail(e, now, "decode", &err, &self.profile));
                        }
                        e.state = TileState::Broken;
                        Some(format!(
                            "WorldStreamer: decode {} failed {} times, giving up (the coarser tile stays): {err}",
                            e.key, e.decode_failures
                        ))
                    }
                }
            }
        }
    }

    /// A chunk job of `key` ended: release its pin on the data tile (data-cut chunks only).
    fn unpin_data(&mut self, key: TileKey) {
        if let Some(dk) = self.data_key(key) {
            if let Some(d) = self.data.as_mut() {
                d.unpin(dk);
            }
        }
    }

    fn fail(e: &mut Entry, now: Instant, stage: &str, err: &str, profile: &Profile) -> String {
        e.attempts += 1;
        let wait = profile.backoff(e.attempts);
        e.state = TileState::Failed { retry_at: now + wait };
        format!(
            "WorldStreamer: {stage} {} failed (attempt {}, retry in {:.1}s): {err}",
            e.key,
            e.attempts,
            wait.as_secs_f32()
        )
    }

    fn held(&self, k: TileKey) -> bool {
        self.entries.get(&k).is_some_and(|e| e.held)
    }

    /// Contract "present": uploaded, or all 4 children present.
    pub fn present(&self, k: TileKey) -> bool {
        self.held(k) || (k.z < self.layer.leaf_zoom && children(k).iter().all(|&c| self.present(c)))
    }

    /// Present, and its sibling group present all the way up to the root: children
    /// delivered under it can never overlap a coarser visible tile.
    fn effective(&self, k: TileKey) -> bool {
        let mut k = k;
        loop {
            if !self.present(k) {
                return false;
            }
            let Some(p) = self.layer.parent(k) else { return true };
            if !self.layer.siblings(k).iter().all(|&s| self.present(s)) {
                return false;
            }
            k = p;
        }
    }

    /// All 4 children are uploaded (held directly, not merely present).
    fn children_held(&self, k: TileKey) -> bool {
        k.z < self.layer.leaf_zoom && children(k).iter().all(|&c| self.held(c))
    }

    /// `k` is one side of an active fade (the coarse tile or one of its children).
    pub fn in_fade(&self, k: TileKey) -> bool {
        self.fades.contains_key(&k) || self.layer.parent(k).is_some_and(|p| self.fades.contains_key(&p))
    }

    /// Start the refine fade of a held, refined parent once all 4 children are uploaded
    /// (guarantee 3). Children 0, parent 1; the parent stays uploaded until it completes.
    fn try_start_refine(&mut self, k: TileKey) {
        let ok = !self.fades.contains_key(&k)
            && self.entries.get(&k).is_some_and(|e| e.held && e.split && !e.coarsening && !e.frozen);
        if !ok {
            return;
        }
        let placeholder = self.entries.get(&k).is_some_and(|e| e.placeholder);
        if self.children_held(k) && !placeholder {
            // Latency `swap`: split decided -> this fade starts (not the first call, which
            // usually comes in the frame of the split, long before the children are uploaded).
            if let Some(e) = self.entries.get_mut(&k) {
                if let Some(t) = e.split_at.take() {
                    self.latency.record(Stage::Swap, e.ring, self.now.saturating_duration_since(t).as_secs_f32() * 1000.0);
                }
            }
            if self.fade_slot_free() {
                let d = self.fade_distance(k);
                self.fades.insert(k, Fade::new(true, d, self.now));
            } else {
                // `fades.max_active` reached: swap without a fade.
                self.note_capped(k);
                self.try_drop(k);
            }
        } else {
            // First fill (a stand-in delivered while already split): swap out at once, so its
            // children's own children can follow in the next frames instead of waiting for a
            // fade. Otherwise present through deeper tiles (not reachable with one-level
            // fades): swap at once too. `try_drop` waits until all 4 children are present.
            self.try_drop(k);
        }
    }

    /// Swaps fading in this layer.
    pub fn active_fades(&self) -> usize {
        self.fades.len()
    }

    /// Swaps fading in the other layer; they count against `fades.max_active` here too.
    pub fn set_fades_elsewhere(&mut self, n: usize) {
        self.fades_elsewhere = n;
    }

    /// Another swap may start a fade without exceeding `fades.max_active`.
    fn fade_slot_free(&self) -> bool {
        self.fades.len() + self.fades_elsewhere < self.max_fades
    }

    fn note_capped(&mut self, k: TileKey) {
        self.capped.push(k);
        self.capped_total += 1;
    }

    /// Coarse tiles swapped without a fade (cap reached) since the last `update`.
    pub fn capped_swaps(&self) -> &[TileKey] {
        &self.capped
    }

    /// Swaps completed without a fade because `fades.max_active` was reached, since start.
    pub fn capped_total(&self) -> u64 {
        self.capped_total
    }

    /// Slant distance (m) from the camera (no look-ahead) to a tile's box: the fade drive.
    fn fade_distance(&self, k: TileKey) -> f64 {
        match (&self.cam_view, self.entries.get(&k)) {
            (Some(v), Some(e)) => v.slant_box_m(k, e.terrain_box[0], e.terrain_box[1] + e.box_h),
            _ => f64::NAN, // no anchor: `Fade::advance` anchors at the first finite distance
        }
    }

    /// Advance every fade with its coarse tile's slant distance; complete the finished ones.
    fn advance_fades(&mut self, now: Instant) {
        let mut done: Vec<(TileKey, bool)> = Vec::new();
        let (entries, cam, layer, cap) = (&self.entries, &self.cam_view, &self.layer, self.fade_cap);
        for (k, f) in self.fades.iter_mut() {
            let (Some(e), Some(v)) = (entries.get(k), cam) else { continue };
            if f.advance(v.slant_box_m(*k, e.terrain_box[0], e.terrain_box[1] + e.box_h), fade_gap_m(layer, e.ring), now, cap) {
                done.push((*k, f.refine));
            }
        }
        // Deterministic completion order (map iteration order is not).
        done.sort_unstable_by_key(|d| d.0);
        for (k, refine) in done {
            self.fades.remove(&k);
            if refine {
                self.fade_final.push((k, 0.0));
                self.fade_final.extend(children(k).map(|c| (c, 1.0)));
                self.try_drop(k);
            } else {
                self.fade_final.push((k, 1.0));
                self.fade_final.extend(children(k).map(|c| (c, 0.0)));
                self.finish_coarsen(k);
            }
        }
    }

    /// Coarsen fade done: the parent is a leaf again, its subtree unloads this frame.
    fn finish_coarsen(&mut self, k: TileKey) {
        let clustered = self.clustered;
        let min_ring = self.profile.lit_min_ring();
        let (kind, gen, leaf) = (self.layer.kind, self.detail_gen, self.layer.leaf_zoom);
        if let Some(e) = self.entries.get_mut(&k) {
            e.split = false;
            e.coarsening = false;
            if e.held {
                Self::refresh_held(e, kind, clustered, min_ring, gen);
            }
        }
        let mut stack: Vec<TileKey> = children(k).to_vec();
        while let Some(c) = stack.pop() {
            let Some(mut e) = self.entries.remove(&c) else { continue };
            orphan_job(&mut self.cancelled, &e);
            if e.split && c.z < leaf {
                stack.extend(children(c));
            }
            // A key in `dropped` is still held by the host and stays there for `drain_unload`.
            if e.held {
                e.state = TileState::Idle;
                e.split = false;
                e.relight = false;
                self.unloading.push(e);
            }
        }
    }

    /// Fade values for `poll_fades`: tiles whose fade completed this frame (final values),
    /// then every tile in an active fade (key -> visible fraction). Finals are reported once.
    pub fn drain_fades(&mut self, out: &mut Vec<(TileKey, f32)>) {
        out.append(&mut self.fade_final);
        for (&k, f) in &self.fades {
            let t = f.reported();
            out.push((k, 1.0 - t));
            out.extend(children(k).map(|c| (c, t)));
        }
    }

    /// Active fades (tests / debugging).
    pub fn fades(&self) -> impl Iterator<Item = (TileKey, &Fade)> {
        self.fades.iter().map(|(k, f)| (*k, f))
    }

    /// Unload a held, refined parent at once (all 4 children present): the end of a refine
    /// fade, or the fallback when no fade is possible.
    fn try_drop(&mut self, k: TileKey) {
        let ok = self.entries.get(&k).is_some_and(|e| e.held && e.split && !e.coarsening && !e.frozen)
            && children(k).iter().all(|&c| self.present(c));
        if !ok {
            return;
        }
        let job = self.next_job;
        if let Some(e) = self.entries.get_mut(&k) {
            e.held = false;
            e.relight = false;
            e.lights = None;
            e.ground = None;
            // Latency `swap` is sampled only when a refine fade starts (a stand-in swaps at once).
            e.split_at = None;
            // A pending re-delivery (light mode, retry) is moot now: leaving the state set
            // would fetch, mesh and hand out a parent that is hidden behind its children.
            // The new job id orphans any worker result still in flight (counters are
            // already settled in `handle` before the id check).
            if !matches!(e.state, TileState::Idle) {
                orphan_job(&mut self.cancelled, e);
                e.state = TileState::Idle;
                e.job = job;
                self.next_job += 1;
            }
            self.dropped.push(k);
        }
    }

    /// Root, or the parent is effective and not part of a fade (no tile in two fades).
    fn deliverable(&self, k: TileKey) -> bool {
        k.z == self.layer.root_zoom || self.layer.parent(k).is_some_and(|p| self.effective(p) && !self.in_fade(p))
    }

    /// Best tile to hand to Godot: fresh tiles whose parent is effective, nearest to the
    /// camera first (slant distance; ring, key as tie-breakers); with `allow_redelivery` and no
    /// fresh tile, a light-mode / terrain-detail re-delivery.
    pub fn pop_ready(&mut self, allow_redelivery: bool) -> Option<ReadyTile> {
        let (k, redelivery, _) = self.best_ready(allow_redelivery)?;
        self.take_ready(k, redelivery)
    }

    /// The tile [`Scheduler::pop_ready`] would hand out next, without handing it out: (key,
    /// re-delivery?, slant distance from the camera). Only drops bookkeeping
    /// that is stale anyway (finished keys, light re-deliveries made moot by a mode flip back).
    pub fn best_ready(&mut self, allow_redelivery: bool) -> Option<(TileKey, bool, f64)> {
        let entries = &self.entries;
        self.ready
            .retain(|k| entries.get(k).is_some_and(|e| matches!(e.state, TileState::Ready(_))));
        // Fresh deliveries strictly by distance from the camera (both layers alike).
        let near_cmp = |a: (f64, f64, usize, TileKey), b: (f64, f64, usize, TileKey)| {
            a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.cmp(&b.2)).then(a.3.cmp(&b.3))
        };
        let mut best: Option<(f64, f64, usize, TileKey)> = None;
        for &k in &self.ready {
            let e = &self.entries[&k];
            if e.held || e.frozen || !self.deliverable(k) {
                continue;
            }
            let o = self.order_key(e);
            if best.is_none_or(|b| near_cmp(o, b).is_lt()) {
                best = Some(o);
            }
        }
        if let Some((d, _, _, k)) = best {
            return Some((k, false, d));
        }
        let rank = |e: &Entry| (e.ring, e.prio_m, e.key);
        if !allow_redelivery {
            return None;
        }
        let clustered = self.clustered;
        let terrain = self.layer.kind == LayerKind::Terrain;
        let mut best: Option<(usize, f64, TileKey)> = None;
        let mut stale: Vec<TileKey> = Vec::new();
        for e in self.entries.values() {
            if !e.held || e.frozen {
                continue;
            }
            let pending = match e.state {
                // Terrain: a held tile re-meshed at the new detail.
                TileState::Ready(_) if terrain => true,
                TileState::Ready(_) if e.lights_mode == clustered => {
                    stale.push(e.key); // mode flipped back meanwhile
                    false
                }
                TileState::Ready(_) => true,
                _ => e.relight,
            };
            // Terrain detail re-deliveries go nearest first (all bands alike).
            let r = if terrain { (0, e.prio_m, e.key) } else { rank(e) };
            if pending && best.is_none_or(|b| crate::rings::priority_cmp(r, b).is_lt()) {
                best = Some(r);
            }
        }
        for k in stale {
            if let Some(e) = self.entries.get_mut(&k) {
                e.state = TileState::Idle;
            }
        }
        let (_, _, k) = best?;
        // Cross-layer comparison (`World::pop_ready`) by the camera distance, as fresh tiles.
        Some((k, true, self.camera_distance(k)))
    }

    /// Hand out `k` as chosen by [`Scheduler::best_ready`] (fresh or re-delivery).
    pub fn take_ready(&mut self, k: TileKey, redelivery: bool) -> Option<ReadyTile> {
        if redelivery {
            return self.take(k, true);
        }
        let t = self.take(k, false);
        if t.is_some() {
            // Dropped earlier in this frame and not yet drained: Godot keeps it now. Only
            // after a successful take, or a key the host still holds would never be unloaded.
            self.dropped.retain(|d| *d != k);
        }
        let coarsening = self.entries.get(&k).is_some_and(|e| e.coarsening && e.split);
        if coarsening {
            // Guarantee 4: the parent is back; it fades in while its children fade out.
            if self.children_held(k) && self.fade_slot_free() {
                let d = self.fade_distance(k);
                self.fades.insert(k, Fade::new(false, d, self.now));
            } else if self.children_held(k) {
                // `fades.max_active` reached: the children go in this frame's `poll_unload`.
                self.note_capped(k);
                self.finish_coarsen(k);
            } else if let Some(e) = self.entries.get_mut(&k) {
                // Not reachable with bottom-up coarsening; the next update drops the subtree.
                e.coarsening = false;
                e.split = false;
            }
        } else if let Some(p) = self.layer.parent(k) {
            // The last child of a group arrived: the parent's refine fade starts now, so
            // this frame's `poll_fades` already has the children at 0.
            self.try_start_refine(p);
        }
        t
    }

    fn take(&mut self, k: TileKey, redelivery: bool) -> Option<ReadyTile> {
        let clustered = self.clustered;
        let (kind, gen) = (self.layer.kind, self.detail_gen);
        let e = self.entries.get_mut(&k)?;
        if !redelivery && !e.held {
            self.latency.record_delivery(e.ring, &e.times, Instant::now());
        }
        let (mesh, ground, lights, terrain) = match std::mem::replace(&mut e.state, TileState::Idle) {
            TileState::Ready(data) => {
                let d = *data;
                (d.mesh, Arc::new(d.ground), d.lights, d.terrain)
            }
            other => {
                e.state = other;
                (MeshData::default(), e.ground.clone().unwrap_or_default(), e.lights.clone()?, None)
            }
        };
        if !redelivery {
            // A stand-in on the way to finer detail (see `Entry::placeholder`); a coarsening
            // parent coming back is a detail change and fades.
            e.placeholder = e.split && !e.coarsening;
        }
        e.held = true;
        e.relight = false;
        e.lights_mode = clustered;
        if kind == LayerKind::World && e.ring == 3 {
            e.lights = Some(lights.clone());
            e.ground = Some(ground.clone());
        }
        if kind == LayerKind::Terrain {
            e.detail_gen = gen;
            e.tris = terrain.as_ref().map_or(0, |t| t.triangle_count() as u32);
        }
        Some(ReadyTile { layer: kind, key: k, ring: e.ring, center: e.center, mesh, ground, lights, clustered, redelivery, terrain })
    }

    /// Keys of delivered tiles that left the cover (whole subtrees per call).
    pub fn drain_unload(&mut self, out: &mut Vec<TileKey>) {
        out.append(&mut self.dropped);
        out.extend(self.unloading.drain(..).map(|e| e.key));
    }

    /// Jobs of orphaned fetches still in flight, for `Worker::cancel` (see `cancelled`).
    pub fn drain_cancelled(&mut self, out: &mut Vec<u64>) {
        out.append(&mut self.cancelled);
    }

    /// Keys of every tile Godot currently holds (for a reconfigure).
    pub fn delivered_keys(&self) -> Vec<TileKey> {
        self.entries
            .values()
            .filter(|e| e.held)
            .map(|e| e.key)
            .chain(self.unloading.iter().map(|e| e.key))
            .chain(self.dropped.iter().copied())
            .collect()
    }

    /// Nothing in flight or waiting: every tracked tile is delivered with the current lights.
    pub fn settled(&self) -> bool {
        self.unloading.is_empty()
            && self.dropped.is_empty()
            && self.fades.is_empty()
            && self.entries.values().all(|e| {
                matches!(e.state, TileState::Idle)
                    && !e.relight
                    && !e.coarsening
                    && !e.frozen
                    && if e.split { !e.held } else { e.held }
            })
    }

    pub fn stats(&self) -> Stats {
        let mut s = Stats::default();
        for e in self.entries.values() {
            let r = e.ring;
            if e.held {
                s.loaded[r] += 1;
                s.tris += e.tris as u64;
            }
            match e.state {
                TileState::Queued => s.queued[r] += 1,
                TileState::Fetching | TileState::WaitData => s.fetching[r] += 1,
                TileState::Fetched(_) | TileState::Decoding => s.meshing[r] += 1,
                TileState::Failed { .. } | TileState::Broken => s.failed[r] += 1,
                TileState::Idle | TileState::Ready(_) => {}
            }
        }
        s
    }

    /// Visit every tracked tile: (key, ring, state label, ENU rect).
    pub fn for_each_tile(&self, mut f: impl FnMut(TileKey, usize, &'static str, &RectEn)) {
        for e in self.entries.values() {
            if !e.held && matches!(e.state, TileState::Idle) {
                continue; // dropped parent: Godot does not hold it
            }
            let label = if e.held { "ready" } else { e.state.label() };
            f(e.key, e.ring, label, &e.rect);
        }
        for e in &self.unloading {
            f(e.key, e.ring, "unloading", &e.rect);
        }
    }

    /// Order distance (m) of a tracked tile (see [`Scheduler::order_key`]), or infinity for an
    /// unknown key.
    pub fn camera_distance(&self, k: TileKey) -> f64 {
        self.entries.get(&k).map_or(f64::INFINITY, |e| self.order_key(e).0)
    }

    /// Delivery / job order of a tile, smallest first: (group distance, own distance, ring,
    /// key). The group distance is the slant distance from the camera to the parent's box
    /// (= the nearest of the 4 siblings; a root's own distance): a tile shows only once its
    /// whole sibling group is uploaded, so the group is what becomes visible and the far
    /// sibling of a near group goes before a nearer tile of a farther group. Ring and key are
    /// deterministic tie-breakers.
    fn order_key(&self, e: &Entry) -> (f64, f64, usize, TileKey) {
        let group = self.layer.parent(e.key).and_then(|p| self.entries.get(&p)).map_or(e.cam_d, |p| p.cam_d.min(e.cam_d));
        (group, e.cam_d, e.ring, e.key)
    }

    /// Queue priority of a job of tile `k` in the shared CPU pool: fresh tiles on demand,
    /// re-deliveries (the host holds the key) as prefetch; then the delivery order
    /// ([`Scheduler::order_key`] group distance). The same order `dispatch` lists the jobs in.
    pub fn job_priority(&self, k: TileKey) -> Priority {
        match self.entries.get(&k) {
            Some(e) if e.held => Priority::prefetch(self.order_key(e).0),
            Some(e) => Priority::on_demand(self.order_key(e).0),
            None => Priority::prefetch(f64::INFINITY),
        }
    }

    /// Tracked entries (tests / budget tools).
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }

    pub fn inflight(&self) -> ([u32; MAX_RINGS], [u32; MAX_RINGS]) {
        (self.fetch_inflight, self.decode_inflight)
    }
}

/// Note the job of a fetch or mesh job that `e` is about to orphan (removed, or its state reset).
fn orphan_job(cancelled: &mut Vec<u64>, e: &Entry) {
    if matches!(e.state, TileState::Fetching | TileState::Decoding) {
        cancelled.push(e.job);
    }
}
