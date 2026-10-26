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
//! visible tile, and every tile takes part in at most one fade.
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
//! # Lights
//! R2/R3 tiles carry single and clustered lights. Crossing 3300 m AGL upwards (2700 m down)
//! switches the mode; already delivered R2/R3 tiles are re-delivered under the same key:
//! R3 from the light sets kept here (no mesh), R2 by re-running fetch (disk cache) + mesh.
//!
//! Lifecycle per entry: Queued -> Fetching -> Fetched -> Decoding -> Ready -> (delivered) Idle.
//! Synthetic tiles skip the fetch stage. Errors -> Failed with exponential backoff -> Queued.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ahash::AHashMap;
use bytes::Bytes;

use crate::geo::{Frame, RectEn, TileKey};
use crate::lights::LightSet;
use crate::mesher::MeshData;
use crate::profile::Profile;
use crate::rings::{children, look_ahead, parent, ring_for_zoom, root_wanted, siblings, BandParams, ViewPoint, RING_COUNT, ROOT_ZOOM};
use crate::worker::{Job, Payload, TileData, WorkerMsg};

// Tuning (world profile): `streaming.max_fetch_per_band` / `max_mesh_per_band` (jobs in flight
// per band), `lights.cluster_on_agl_m` / `cluster_off_agl_m` (light carpet switch),
// `fades.cap_s` (fade safety cap), `streaming.retry_*` (backoff), `bands.*` (cover).

/// Reported fade values are quantized to 1/256 so `fade` and `1 - fade` are exact in f32 and
/// the two sides of a swap always sum to exactly 1.
const FADE_QUANTUM: f64 = 256.0;

/// One LOD swap between a coarse tile (the map key) and its 4 children.
#[derive(Debug, Clone, Copy)]
pub struct Fade {
    /// Direction: parent -> children (refine) or children -> parent (coarsen).
    pub refine: bool,
    /// Visible fraction of the children; the parent shows `1 - t`.
    pub t: f64,
    /// Anchor of the distance drive: `t` was `t0` at slant distance `d0`.
    t0: f64,
    d0: f64,
    started: Instant,
}

impl Fade {
    fn new(refine: bool, d: f64, now: Instant) -> Self {
        let t = if refine { 0.0 } else { 1.0 };
        Self { refine, t, t0: t, d0: d, started: now }
    }

    /// Change direction, continuing from the current `t` at distance `d`.
    fn retarget(&mut self, refine: bool, d: f64) {
        if self.refine != refine {
            self.refine = refine;
            self.t0 = self.t;
            self.d0 = d;
        }
    }

    /// Advance with the current slant distance `d` over the hysteresis gap `gap` (m).
    /// Returns true when the fade has completed (target reached or the safety cap `cap`).
    fn advance(&mut self, d: f64, gap: f64, now: Instant, cap: Duration) -> bool {
        if d.is_finite() && gap > 0.0 {
            if self.refine {
                self.t = self.t.max((self.t0 + (self.d0 - d) / gap).min(1.0));
            } else {
                self.t = self.t.min((self.t0 - (d - self.d0) / gap).max(0.0));
            }
        }
        let target = if self.refine { self.t >= 1.0 } else { self.t <= 0.0 };
        target || now.saturating_duration_since(self.started) >= cap
    }

    /// Quantized children fraction as reported to Godot. While active, the outgoing side
    /// never reads exactly 0 (that value means "completed, unloaded this frame").
    fn reported(&self) -> f32 {
        let q = (self.t.clamp(0.0, 1.0) * FADE_QUANTUM).round() / FADE_QUANTUM;
        let step = 1.0 / FADE_QUANTUM;
        (if self.refine { q.min(1.0 - step) } else { q.max(step) }) as f32
    }
}

/// The hysteresis gap (m) that drives the fade of a coarse tile of `ring` (>= 1).
pub fn fade_gap_m(b: &BandParams, ring: usize) -> f64 {
    b.edge_m[ring.max(1) - 1] * (b.hysteresis - 1.0)
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
}

impl TileState {
    fn label(&self) -> &'static str {
        match self {
            TileState::Idle | TileState::Ready(_) => "ready",
            TileState::Queued => "queued",
            TileState::Fetching => "fetching",
            TileState::Fetched(_) | TileState::Decoding => "meshing",
            TileState::Failed { .. } => "failed",
        }
    }
}

#[derive(Debug)]
pub struct Entry {
    pub key: TileKey,
    pub ring: usize,
    pub state: TileState,
    pub job: u64,
    pub attempts: u32,
    /// Slant distance (m) to the look-ahead point of the tile's band.
    pub prio_m: f64,
    mark: u64,
    /// The node's children are part of the cover.
    pub split: bool,
    /// Own box height once decoded.
    pub height: Option<f32>,
    /// Godot holds this key (delivered and not unloaded).
    pub held: bool,
    /// Light mode (clustered?) of the version Godot holds.
    pub lights_mode: bool,
    /// R3: re-deliver from `lights` (mode change).
    pub relight: bool,
    /// R3: light sets kept for re-delivery.
    pub lights: Option<Arc<LightSet>>,
    /// Merging back into this (dropped) node: it is being re-delivered; subtree frozen.
    pub coarsening: bool,
    /// Inside a coarsening subtree: kept as is until the ancestor's fade completes.
    pub frozen: bool,
    /// Slant distance (m) of the box from the finer band's look-ahead point (split decision;
    /// 0 for R0).
    pub split_d: f64,
    /// Box height (m) used this frame (own, or inherited while unknown).
    pub box_h: f64,
    /// ENU rect and tile center (fixed frame).
    pub rect: RectEn,
    pub center: [f64; 2],
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub queued: [i32; RING_COUNT],
    pub fetching: [i32; RING_COUNT],
    pub meshing: [i32; RING_COUNT],
    pub loaded: [i32; RING_COUNT],
    pub failed: [i32; RING_COUNT],
}

pub struct ReadyTile {
    pub key: TileKey,
    pub ring: usize,
    pub center: [f64; 2],
    pub mesh: MeshData,
    /// Ground mesh (empty for R3, for empty tiles and for R3 light re-deliveries).
    pub ground: MeshData,
    pub lights: Arc<LightSet>,
    pub clustered: bool,
    /// Already uploaded once (light mode change).
    pub redelivery: bool,
}

impl ReadyTile {
    pub fn light_arrays(&self) -> (&[[f32; 3]], &[[f32; 4]]) {
        self.lights.get(self.clustered)
    }
}

pub struct Scheduler {
    frame: Frame,
    synthetic: bool,
    profile: Arc<Profile>,
    /// Copies of profile values used per tile per frame.
    bands: BandParams,
    fade_cap: Duration,
    entries: AHashMap<TileKey, Entry>,
    ready: Vec<TileKey>,
    unloading: Vec<Entry>,
    /// Refined parents dropped from Godot (entries stay in the tree, not held).
    dropped: Vec<TileKey>,
    next_job: u64,
    epoch: u64,
    clustered: bool,
    fetch_inflight: [u32; RING_COUNT],
    decode_inflight: [u32; RING_COUNT],
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
}

impl Scheduler {
    pub fn new(frame: Frame, synthetic: bool, profile: Arc<Profile>) -> Self {
        Self {
            frame,
            synthetic,
            bands: *profile.bands(),
            fade_cap: Duration::from_secs_f64(profile.fades.cap_s),
            profile,
            entries: AHashMap::with_capacity(1024),
            ready: Vec::with_capacity(64),
            unloading: Vec::new(),
            dropped: Vec::new(),
            next_job: 1,
            epoch: 0,
            clustered: false,
            fetch_inflight: [0; RING_COUNT],
            decode_inflight: [0; RING_COUNT],
            roots: Vec::with_capacity(512),
            stack: Vec::with_capacity(256),
            cand: Vec::with_capacity(256),
            fades: AHashMap::with_capacity(64),
            fade_final: Vec::with_capacity(64),
            now: Instant::now(),
            cam_view: None,
        }
    }

    pub fn clustered(&self) -> bool {
        self.clustered
    }

    pub fn profile(&self) -> &Arc<Profile> {
        &self.profile
    }

    /// Look-ahead view point per band. `cam` = (east, north, AGL), `vel` = (east, north, up).
    pub fn views(&self, cam: [f64; 3], vel: [f64; 3]) -> [ViewPoint; RING_COUNT] {
        let b = &self.bands;
        std::array::from_fn(|r| {
            let la = look_ahead(cam, vel, b.look_ahead_clamp_m(r), b.look_ahead_s);
            let (lat, lon) = self.frame.lat_lon(la[0], la[1]);
            // AGL -> the terrain scale of the tile boxes (terrain hook; 0 while flat).
            ViewPoint::new(lat, lon, la[2] + crate::geo::ground_height(lat, lon))
        })
    }

    /// Recompute the cover around the camera and push new jobs into `jobs`.
    /// `cam` = (east, north, AGL) meters, `vel` = (east, north, up) m/s.
    pub fn update(&mut self, cam: [f64; 3], vel: [f64; 3], now: Instant, jobs: &mut Vec<Job>) {
        self.epoch += 1;
        self.now = now;
        let b = self.bands;
        let default_h = b.default_tile_height_m;
        // Final fade values are reported once, by the `poll_fades` of the frame they completed in.
        self.fade_final.clear();
        self.update_light_mode(cam[2]);
        let views = self.views(cam, vel);
        self.cam_view = Some(self.views(cam, [0.0; 3])[0]);

        // 1. Roots and the tree below them (hysteresis on both).
        self.roots.clear();
        views[RING_COUNT - 1].root_candidates(&b, &mut self.roots);
        for i in 0..self.roots.len() {
            let k = self.roots[i];
            let (existing, h) = match self.entries.get(&k) {
                Some(e) => (true, e.height.map_or(default_h, |h| h as f64)),
                None => (false, default_h),
            };
            if root_wanted(&b, &views[RING_COUNT - 1], k, h, existing) {
                self.stack.push((k, default_h, false));
            }
        }
        while let Some((k, inherited_h, frozen)) = self.stack.pop() {
            let Some(ring) = ring_for_zoom(k.z) else { continue };
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
            let fading = self.fades.contains_key(&k);
            // Bottom-up coarsening: a node merges only once none of its children is split.
            let kids_split = k.z < crate::rings::LEAF_ZOOM
                && children(k).iter().any(|c| self.entries.get(c).is_some_and(|e| e.split));
            let mut orphan = false;
            let mut retarget: Option<bool> = None;
            let (split, freeze, h) = {
                let e = self.touch(k, ring);
                e.frozen = false;
                let h = e.height.map_or(inherited_h, |h| h as f64);
                e.box_h = h;
                e.prio_m = views[ring].slant_m(k, h);
                e.split_d = if ring > 0 { views[ring - 1].slant_m(k, h) } else { 0.0 };
                let want = ring > 0 && b.inside(e.split_d, b.edge_m[ring - 1], e.split);
                let mut freeze = false;
                if want {
                    if e.coarsening {
                        // Split again before the coarsening finished: keep the children.
                        e.coarsening = false;
                        if fading {
                            retarget = Some(true); // coarsen fade -> refine fade
                        } else if !e.held && !matches!(e.state, TileState::Idle) {
                            // Cancel the pending re-delivery (orphan any in-flight result).
                            e.state = TileState::Idle;
                            orphan = true;
                        }
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
                        e.coarsening = false;
                        if e.held && e.ring >= 2 {
                            // Visible again: catch up on light-mode flips missed while split.
                            Self::mark_relight(e, clustered);
                        }
                    } else if kids_split && !e.coarsening {
                        // Wait for the children to merge first (keeps fades one level deep).
                    } else {
                        // Guarantee 4: deliver the parent again, then fade the children out.
                        if !e.coarsening && matches!(e.state, TileState::Idle) {
                            e.state = TileState::Queued;
                        }
                        e.coarsening = true;
                        freeze = true;
                    }
                }
                (e.split, freeze, h)
            };
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
        self.entries.retain(|_, e| {
            if e.mark == epoch {
                return true;
            }
            if e.held {
                let mut u = std::mem::replace(e, Entry::blank(e.key, e.ring, e.rect, e.center, default_h));
                u.state = TileState::Idle;
                u.split = false;
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
        for e in self.entries.values_mut() {
            // A split parent is hidden behind its children: it is re-lit if it ever returns.
            if !e.held || e.split || e.ring < 2 {
                continue;
            }
            Self::mark_relight(e, want);
        }
    }

    /// A held R2/R3 entry whose delivered lights differ from `mode` gets re-delivered.
    fn mark_relight(e: &mut Entry, mode: bool) {
        let stale = e.lights_mode != mode;
        if e.ring == 3 && e.lights.is_some() {
            e.relight = stale;
        } else if stale && matches!(e.state, TileState::Idle) {
            // R2 (or an R3 without kept lights): rebuild from the disk cache.
            e.state = TileState::Queued;
        }
    }

    /// Get or create the entry for a visited node (restoring one that is about to unload).
    fn touch(&mut self, k: TileKey, ring: usize) -> &mut Entry {
        let epoch = self.epoch;
        if !self.entries.contains_key(&k) {
            let e = match self.unloading.iter().position(|u| u.key == k) {
                Some(i) => {
                    // Godot still holds it: restore without refetching.
                    let mut e = self.unloading.swap_remove(i);
                    if e.ring >= 2 {
                        Self::mark_relight(&mut e, self.clustered);
                    }
                    e
                }
                None => Entry::blank(
                    k,
                    ring,
                    self.frame.tile_rect(k),
                    self.frame.tile_center(k),
                    self.bands.default_tile_height_m,
                ),
            };
            self.entries.insert(k, e);
        }
        let e = self.entries.get_mut(&k).expect("inserted above");
        e.mark = epoch;
        e
    }

    fn dispatch(&mut self, jobs: &mut Vec<Job>) {
        let max_fetch = self.profile.streaming.max_fetch_per_band;
        let max_decode = self.profile.streaming.max_mesh_per_band;
        let full = (0..RING_COUNT)
            .all(|r| self.decode_inflight[r] >= max_decode && (self.synthetic || self.fetch_inflight[r] >= max_fetch));
        if full {
            return;
        }
        self.cand.clear();
        for e in self.entries.values() {
            if !e.frozen && matches!(e.state, TileState::Queued | TileState::Fetched(_)) {
                self.cand.push((e.ring, e.held, e.prio_m, e.key));
            }
        }
        if self.cand.is_empty() {
            return;
        }
        // Finer band first, fresh tiles before re-deliveries, then slant distance.
        self.cand
            .sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.total_cmp(&b.2)).then(a.3.cmp(&b.3)));
        for i in 0..self.cand.len() {
            let (ring, _, _, key) = self.cand[i];
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
                self.next_job += 1;
                self.decode_inflight[ring] += 1;
                jobs.push(Job::Decode { key, ring, job: e.job, payload });
            } else {
                if self.fetch_inflight[ring] >= max_fetch {
                    continue;
                }
                e.state = TileState::Fetching;
                e.job = self.next_job;
                self.next_job += 1;
                self.fetch_inflight[ring] += 1;
                jobs.push(Job::Fetch { key, ring, job: e.job });
            }
        }
    }

    /// Apply a worker result. Returns a warning to log (main thread) on failure.
    pub fn handle(&mut self, msg: WorkerMsg, now: Instant) -> Option<String> {
        match msg {
            WorkerMsg::Info(_) | WorkerMsg::Error(_) => None,
            WorkerMsg::Fetched { key, ring, job, result } => {
                self.fetch_inflight[ring] = self.fetch_inflight[ring].saturating_sub(1);
                let e = self.entries.get_mut(&key).filter(|e| e.job == job)?;
                if !matches!(e.state, TileState::Fetching) {
                    return None;
                }
                match result {
                    Ok((bytes, _)) => {
                        e.state = TileState::Fetched(bytes);
                        None
                    }
                    Err(err) => Some(Self::fail(e, now, "fetch", &err, &self.profile)),
                }
            }
            WorkerMsg::Decoded { key, ring, job, result } => {
                self.decode_inflight[ring] = self.decode_inflight[ring].saturating_sub(1);
                let e = self.entries.get_mut(&key).filter(|e| e.job == job)?;
                if !matches!(e.state, TileState::Decoding) {
                    return None;
                }
                match result {
                    Ok(data) => {
                        e.attempts = 0;
                        e.height = Some(data.max_height);
                        e.state = TileState::Ready(Box::new(data));
                        self.ready.push(key);
                        None
                    }
                    Err(err) => Some(Self::fail(e, now, "decode", &err, &self.profile)),
                }
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
        self.held(k) || (k.z < crate::rings::LEAF_ZOOM && children(k).iter().all(|&c| self.present(c)))
    }

    /// Present, and its sibling group present all the way up to the root: children
    /// delivered under it can never overlap a coarser visible tile.
    fn effective(&self, k: TileKey) -> bool {
        let mut k = k;
        loop {
            if !self.present(k) {
                return false;
            }
            let Some(p) = parent(k) else { return true };
            if !siblings(k).iter().all(|&s| self.present(s)) {
                return false;
            }
            k = p;
        }
    }

    /// All 4 children are uploaded (held directly, not merely present).
    fn children_held(&self, k: TileKey) -> bool {
        k.z < crate::rings::LEAF_ZOOM && children(k).iter().all(|&c| self.held(c))
    }

    /// `k` is one side of an active fade (the coarse tile or one of its children).
    pub fn in_fade(&self, k: TileKey) -> bool {
        self.fades.contains_key(&k) || parent(k).is_some_and(|p| self.fades.contains_key(&p))
    }

    /// Start the refine fade of a held, refined parent once all 4 children are uploaded
    /// (guarantee 3). Children 0, parent 1; the parent stays uploaded until it completes.
    fn try_start_refine(&mut self, k: TileKey) {
        let ok = !self.fades.contains_key(&k)
            && self.entries.get(&k).is_some_and(|e| e.held && e.split && !e.coarsening && !e.frozen);
        if !ok {
            return;
        }
        if self.children_held(k) {
            let d = self.fade_distance(k);
            self.fades.insert(k, Fade::new(true, d, self.now));
        } else {
            // Present through deeper tiles (not reachable with one-level fades): swap at once.
            self.try_drop(k);
        }
    }

    /// Slant distance (m) from the camera (no look-ahead) to a tile's box: the fade drive.
    fn fade_distance(&self, k: TileKey) -> f64 {
        match (&self.cam_view, self.entries.get(&k)) {
            (Some(v), Some(e)) => v.slant_m(k, e.box_h),
            _ => f64::NAN, // no progress; the cap still applies
        }
    }

    /// Advance every fade with its coarse tile's slant distance; complete the finished ones.
    fn advance_fades(&mut self, now: Instant) {
        let mut done: Vec<(TileKey, bool)> = Vec::new();
        let (entries, cam, bands, cap) = (&self.entries, &self.cam_view, &self.bands, self.fade_cap);
        for (k, f) in self.fades.iter_mut() {
            let (Some(e), Some(v)) = (entries.get(k), cam) else { continue };
            if f.advance(v.slant_m(*k, e.box_h), fade_gap_m(bands, e.ring), now, cap) {
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
        if let Some(e) = self.entries.get_mut(&k) {
            e.split = false;
            e.coarsening = false;
            if e.held && e.ring >= 2 {
                Self::mark_relight(e, clustered);
            }
        }
        let mut stack: Vec<TileKey> = children(k).to_vec();
        while let Some(c) = stack.pop() {
            let Some(mut e) = self.entries.remove(&c) else { continue };
            if e.split && c.z < crate::rings::LEAF_ZOOM {
                stack.extend(children(c));
            }
            self.dropped.retain(|d| *d != c);
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
            // A pending re-delivery (light mode, retry) is moot now: leaving the state set
            // would fetch, mesh and hand out a parent that is hidden behind its children.
            // The new job id orphans any worker result still in flight (counters are
            // already settled in `handle` before the id check).
            if !matches!(e.state, TileState::Idle) {
                e.state = TileState::Idle;
                e.job = job;
                self.next_job += 1;
            }
            self.dropped.push(k);
        }
    }

    /// Root, or the parent is effective and not part of a fade (no tile in two fades).
    fn deliverable(&self, k: TileKey) -> bool {
        k.z == ROOT_ZOOM || parent(k).is_some_and(|p| self.effective(p) && !self.in_fade(p))
    }

    /// Best tile to hand to Godot: fresh tiles whose parent is effective, finer band first,
    /// then nearest; with `allow_redelivery` and no fresh tile, a light-mode re-delivery.
    pub fn pop_ready(&mut self, allow_redelivery: bool) -> Option<ReadyTile> {
        let entries = &self.entries;
        self.ready
            .retain(|k| entries.get(k).is_some_and(|e| matches!(e.state, TileState::Ready(_))));
        let rank = |e: &Entry| (e.ring, e.prio_m, e.key);
        let mut best: Option<(usize, f64, TileKey)> = None;
        for &k in &self.ready {
            let e = &self.entries[&k];
            if e.held || e.frozen || !self.deliverable(k) {
                continue;
            }
            if best.is_none_or(|b| crate::rings::priority_cmp(rank(e), b).is_lt()) {
                best = Some(rank(e));
            }
        }
        if let Some((_, _, k)) = best {
            // Dropped earlier in this frame and not yet drained: Godot still holds it.
            self.dropped.retain(|d| *d != k);
            let t = self.take(k, false);
            let coarsening = self.entries.get(&k).is_some_and(|e| e.coarsening && e.split);
            if coarsening {
                // Guarantee 4: the parent is back; it fades in while its children fade out.
                if self.children_held(k) {
                    let d = self.fade_distance(k);
                    self.fades.insert(k, Fade::new(false, d, self.now));
                } else if let Some(e) = self.entries.get_mut(&k) {
                    // Not reachable with bottom-up coarsening; the next update drops the subtree.
                    e.coarsening = false;
                    e.split = false;
                }
            } else if let Some(p) = parent(k) {
                // The last child of a group arrived: the parent's refine fade starts now, so
                // this frame's `poll_fades` already has the children at 0.
                self.try_start_refine(p);
            }
            return t;
        }
        if !allow_redelivery {
            return None;
        }
        let clustered = self.clustered;
        let mut best: Option<(usize, f64, TileKey)> = None;
        let mut stale: Vec<TileKey> = Vec::new();
        for e in self.entries.values() {
            if !e.held || e.frozen {
                continue;
            }
            let pending = match e.state {
                TileState::Ready(_) if e.lights_mode == clustered => {
                    stale.push(e.key); // mode flipped back meanwhile
                    false
                }
                TileState::Ready(_) => true,
                _ => e.relight,
            };
            if pending && best.is_none_or(|b| crate::rings::priority_cmp(rank(e), b).is_lt()) {
                best = Some(rank(e));
            }
        }
        for k in stale {
            if let Some(e) = self.entries.get_mut(&k) {
                e.state = TileState::Idle;
            }
        }
        let (_, _, k) = best?;
        self.take(k, true)
    }

    fn take(&mut self, k: TileKey, redelivery: bool) -> Option<ReadyTile> {
        let clustered = self.clustered;
        let e = self.entries.get_mut(&k)?;
        let (mesh, ground, lights) = match std::mem::replace(&mut e.state, TileState::Idle) {
            TileState::Ready(data) => {
                let d = *data;
                (d.mesh, d.ground, d.lights)
            }
            other => {
                e.state = other;
                (MeshData::default(), MeshData::default(), e.lights.clone()?)
            }
        };
        e.held = true;
        e.relight = false;
        e.lights_mode = clustered;
        if e.ring == 3 {
            e.lights = Some(lights.clone());
        }
        Some(ReadyTile { key: k, ring: e.ring, center: e.center, mesh, ground, lights, clustered, redelivery })
    }

    /// Keys of delivered tiles that left the cover (whole subtrees per call).
    pub fn drain_unload(&mut self, out: &mut Vec<TileKey>) {
        out.append(&mut self.dropped);
        out.extend(self.unloading.drain(..).map(|e| e.key));
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
            }
            match e.state {
                TileState::Queued => s.queued[r] += 1,
                TileState::Fetching => s.fetching[r] += 1,
                TileState::Fetched(_) | TileState::Decoding => s.meshing[r] += 1,
                TileState::Failed { .. } => s.failed[r] += 1,
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

    /// Tracked entries (tests / budget tools).
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }

    pub fn inflight(&self) -> ([u32; RING_COUNT], [u32; RING_COUNT]) {
        (self.fetch_inflight, self.decode_inflight)
    }
}

impl Entry {
    fn blank(key: TileKey, ring: usize, rect: RectEn, center: [f64; 2], box_h: f64) -> Self {
        Self {
            key,
            ring,
            state: TileState::Queued,
            job: 0,
            attempts: 0,
            prio_m: 0.0,
            mark: 0,
            split: false,
            height: None,
            held: false,
            lights_mode: false,
            relight: false,
            lights: None,
            coarsening: false,
            frozen: false,
            split_d: 0.0,
            box_h,
            rect,
            center,
        }
    }
}

/// Godot's side of the contract, simulated from keys: which tiles are uploaded, which are
/// present ("uploaded or all 4 children present"), which are visible ("uploaded and (z13 or
/// all 4 siblings present) and not all 4 children present", or uploaded with a fade entry),
/// and whether the visible tiles form an exact cover (overlaps only between the two sides of
/// a fade, with complementary values).
pub mod sim {
    use ahash::{AHashMap, AHashSet};

    use crate::geo::TileKey;
    use crate::rings::{children, parent, siblings, LEAF_ZOOM, ROOT_ZOOM};

    #[derive(Default, Debug, Clone)]
    pub struct GodotSim {
        pub uploaded: AHashSet<TileKey>,
        /// The last `poll_fades` result (replaced as a whole each frame).
        pub fades: AHashMap<TileKey, f32>,
    }

    impl GodotSim {
        pub fn upload(&mut self, k: TileKey) {
            self.uploaded.insert(k);
        }
        pub fn unload(&mut self, k: TileKey) {
            self.uploaded.remove(&k);
            self.fades.remove(&k);
        }
        pub fn set_fades(&mut self, f: &[(TileKey, f32)]) {
            self.fades.clear();
            self.fades.extend(f.iter().copied());
        }
        pub fn present(&self, k: TileKey) -> bool {
            self.uploaded.contains(&k) || (k.z < LEAF_ZOOM && children(k).iter().all(|&c| self.present(c)))
        }
        fn all_present(&self, ks: [TileKey; 4]) -> bool {
            ks.iter().all(|&k| self.present(k))
        }
        pub fn visible(&self, k: TileKey) -> bool {
            self.uploaded.contains(&k)
                && (self.fades.contains_key(&k)
                    || ((k.z == ROOT_ZOOM || self.all_present(siblings(k)))
                        && !(k.z < LEAF_ZOOM && self.all_present(children(k)))))
        }
        /// Both sides of one fade: `p` is `k`'s parent and the two values sum to 1.
        fn complementary(&self, p: TileKey, k: TileKey) -> bool {
            parent(k) == Some(p)
                && match (self.fades.get(&p), self.fades.get(&k)) {
                    (Some(a), Some(b)) => (a + b - 1.0).abs() < 1e-6,
                    _ => false,
                }
        }
        pub fn visible_tiles(&self) -> Vec<TileKey> {
            let mut v: Vec<TileKey> = self.uploaded.iter().copied().filter(|&k| self.visible(k)).collect();
            v.sort();
            v
        }
        fn covered(&self, k: TileKey) -> bool {
            self.visible(k)
                || (k.z < LEAF_ZOOM && self.all_present(children(k)) && children(k).iter().all(|&c| self.covered(c)))
        }
        fn root_of(mut k: TileKey) -> TileKey {
            while let Some(p) = parent(k) {
                k = p;
            }
            k
        }
        /// Overlaps (a visible tile with a visible ancestor), holes (a root holding uploaded
        /// tiles whose area is not completely covered) and orphans (uploaded tiles under a
        /// root that is not present). Empty = exact cover of the present roots.
        pub fn check(&self) -> Vec<String> {
            let mut errs = Vec::new();
            let vis: AHashSet<TileKey> = self.uploaded.iter().copied().filter(|&k| self.visible(k)).collect();
            for &k in &vis {
                let mut a = parent(k);
                while let Some(p) = a {
                    if vis.contains(&p) && !self.complementary(p, k) {
                        errs.push(format!("overlap: {k} and ancestor {p} both visible"));
                    }
                    a = parent(p);
                }
            }
            // Fade entries: only for uploaded tiles; a partial value needs its other side.
            for (&k, &f) in &self.fades {
                if !self.uploaded.contains(&k) {
                    errs.push(format!("fade entry for {k} which is not uploaded"));
                }
                if !(0.0..=1.0).contains(&f) {
                    errs.push(format!("fade {k} = {f} out of range"));
                }
                if f < 1.0 {
                    let up = parent(k).is_some_and(|p| self.complementary(p, k));
                    let down = k.z < LEAF_ZOOM && children(k).iter().all(|&c| self.complementary(k, c));
                    if !up && !down {
                        errs.push(format!("fade {k} = {f} without a complementary other side"));
                    }
                }
            }
            let mut roots: AHashMap<TileKey, TileKey> = AHashMap::new();
            for &k in &self.uploaded {
                roots.entry(Self::root_of(k)).or_insert(k);
            }
            for (r, example) in roots {
                if !self.present(r) {
                    errs.push(format!("orphan: {example} uploaded under non-present root {r}"));
                } else if !self.covered(r) {
                    errs.push(format!("hole under root {r}"));
                }
            }
            errs
        }
        /// The visible tiles containing a point (Mercator world units).
        pub fn tile_at(&self, wx: f64, wy: f64) -> Vec<TileKey> {
            let mut out = Vec::new();
            for z in ROOT_ZOOM..=LEAF_ZOOM {
                let n = (1u64 << z) as f64;
                let k = TileKey::new(z, (wx * n).floor() as u32, (wy * n).floor() as u32);
                if self.visible(k) {
                    out.push(k);
                }
            }
            out
        }
    }
}

#[cfg(test)]
fn sim_root(mut k: TileKey) -> TileKey {
    while let Some(p) = parent(k) {
        k = p;
    }
    k
}

#[cfg(test)]
mod tests {
    use super::sim::GodotSim;
    use super::*;
    use crate::fetch::Source;
    use crate::rings::LEAF_ZOOM;
    use crate::worker::build_tile;
    use std::collections::{HashSet, VecDeque};

    const VILNIUS: (f64, f64) = (54.6872, 25.2797);

    /// The default world profile (the spike's constants).
    fn dp() -> Arc<Profile> {
        Arc::new(crate::profile::test_default().clone())
    }
    fn bands() -> BandParams {
        *crate::profile::test_default().bands()
    }
    fn fade_cap() -> Duration {
        Duration::from_secs_f64(crate::profile::test_default().fades.cap_s)
    }

    /// Box height of fake tiles: deterministic 20..110 m by key (exercises per-tile heights).
    fn fake_height(k: TileKey) -> f32 {
        20.0 + (crate::mvt::splitmix64(((k.z as u64) << 48) ^ ((k.x as u64) << 24) ^ k.y as u64) % 90) as f32
    }

    fn fake_data(k: TileKey, ring: usize) -> TileData {
        let lights = if ring >= 2 {
            LightSet {
                positions: vec![[0.0, 10.0, 0.0], [5.0, 10.0, 5.0]],
                colors: vec![[1.0; 4]; 2],
                clustered_positions: vec![[2.5, 10.0, 2.5]],
                clustered_colors: vec![[1.0; 4]],
            }
        } else {
            LightSet::default()
        };
        TileData {
            building_count: 0,
            lights: Arc::new(lights),
            mesh: MeshData::default(),
            ground: MeshData::default(),
            max_height: fake_height(k),
            cpu_us: 0,
        }
    }

    /// Worker stand-in: jobs finish after a fixed number of frames (fetch 3, decode 2).
    struct FakeWorker {
        pending: VecDeque<(u64, WorkerMsg)>,
        frame: u64,
        real: Option<Frame>,
    }

    impl FakeWorker {
        fn new() -> Self {
            Self { pending: VecDeque::new(), frame: 0, real: None }
        }
        fn submit(&mut self, jobs: &mut Vec<Job>) {
            for j in jobs.drain(..) {
                let (delay, msg) = match j {
                    Job::Fetch { key, ring, job } => {
                        (3, WorkerMsg::Fetched { key, ring, job, result: Ok((Bytes::new(), Source::Network)) })
                    }
                    Job::Decode { key, ring, job, payload } => {
                        let result = match self.real {
                            Some(f) => build_tile(key, ring, f, &payload, crate::profile::test_default()),
                            None => Ok(fake_data(key, ring)),
                        };
                        (2, WorkerMsg::Decoded { key, ring, job, result })
                    }
                };
                self.pending.push_back((self.frame + delay, msg));
            }
        }
        fn step(&mut self, s: &mut Scheduler, now: Instant) {
            self.frame += 1;
            let mut keep = VecDeque::new();
            while let Some((t, m)) = self.pending.pop_front() {
                if t <= self.frame {
                    let _ = s.handle(m, now);
                } else {
                    keep.push_back((t, m));
                }
            }
            self.pending = keep;
        }
    }

    /// Harness frame time (s): `now` advances by this per frame.
    const DT: f64 = 1.0 / 30.0;

    /// One harness frame in contract order: update -> poll_ready(2) -> poll_fades ->
    /// poll_unload, applied to the simulated Godot side. Returns (delivered, unloaded).
    struct Harness {
        s: Scheduler,
        w: FakeWorker,
        g: GodotSim,
        jobs: Vec<Job>,
        now: Instant,
        delivered_total: usize,
        redelivered_total: usize,
        ever_requested: HashSet<TileKey>,
        ever_delivered: HashSet<TileKey>,
        parent_drops: usize,
        coarsen_swaps: usize,
        /// This frame's / the previous frame's `poll_fades` result.
        fades: Vec<(TileKey, f32)>,
        prev_fades: AHashMap<TileKey, f32>,
        /// Visible tiles at the end of the previous frame.
        prev_visible: HashSet<TileKey>,
        /// Fresh (non re-delivery) tiles of this frame.
        fresh: Vec<TileKey>,
        refine_fades: usize,
        coarsen_fades: usize,
    }

    impl Harness {
        fn new() -> Self {
            let frame = Frame::new(VILNIUS.0, VILNIUS.1);
            Self {
                s: Scheduler::new(frame, false, dp()),
                w: FakeWorker::new(),
                g: GodotSim::default(),
                jobs: Vec::new(),
                now: Instant::now(),
                delivered_total: 0,
                redelivered_total: 0,
                ever_requested: HashSet::new(),
                ever_delivered: HashSet::new(),
                parent_drops: 0,
                coarsen_swaps: 0,
                fades: Vec::new(),
                prev_fades: AHashMap::new(),
                prev_visible: HashSet::new(),
                fresh: Vec::new(),
                refine_fades: 0,
                coarsen_fades: 0,
            }
        }

        fn fade(&self, k: TileKey) -> Option<f32> {
            self.fades.iter().rev().find(|f| f.0 == k).map(|f| f.1)
        }

        fn frame(&mut self, cam: [f64; 3], vel: [f64; 3]) -> (Vec<TileKey>, Vec<TileKey>) {
            self.now += Duration::from_secs_f64(DT);
            self.prev_fades = self.g.fades.clone();
            self.prev_visible = self.g.uploaded.iter().copied().filter(|&k| self.g.visible(k)).collect();
            self.fresh.clear();
            self.w.step(&mut self.s, self.now);
            self.s.update(cam, vel, self.now, &mut self.jobs);
            self.w.submit(&mut self.jobs);
            for e in self.s.entries() {
                self.ever_requested.insert(e.key);
            }
            let mut got = Vec::new();
            let mut re = 0;
            for _ in 0..2 {
                let Some(t) = self.s.pop_ready(re < 1) else { break };
                if t.redelivery {
                    re += 1;
                    self.redelivered_total += 1;
                    assert!(self.g.uploaded.contains(&t.key), "re-delivery of a tile Godot does not hold");
                } else {
                    assert!(!self.g.uploaded.contains(&t.key), "fresh delivery of an uploaded tile {}", t.key);
                    self.fresh.push(t.key);
                }
                self.delivered_total += 1;
                self.ever_delivered.insert(t.key);
                got.push(t.key);
            }
            for &k in &got {
                self.g.upload(k);
            }
            self.fades.clear();
            self.s.drain_fades(&mut self.fades);
            self.g.set_fades(&self.fades);
            for (k, f) in self.s.fades() {
                // Fades started this frame (update step 4 or poll_ready): count by direction.
                if !self.prev_fades.contains_key(&k) {
                    if f.refine {
                        self.refine_fades += 1;
                    } else {
                        self.coarsen_fades += 1;
                    }
                }
            }
            let mut un = Vec::new();
            self.s.drain_unload(&mut un);
            for &k in &un {
                assert!(self.g.uploaded.contains(&k), "unload of a tile Godot does not hold: {k}");
                self.g.unload(k);
            }
            (got, un)
        }

        /// Contract checks that must hold on every frame.
        fn check_frame(&mut self, un: &[TileKey], ctx: &str) {
            let errs = self.g.check();
            assert!(errs.is_empty(), "{ctx}: {:?}", &errs[..errs.len().min(5)]);
            // (1) Sibling groups: every tracked non-root tile has its 3 siblings tracked.
            for e in self.s.entries() {
                if e.key.z > ROOT_ZOOM {
                    for sib in siblings(e.key) {
                        assert!(self.s.entries.contains_key(&sib), "{ctx}: {} tracked without sibling {sib}", e.key);
                    }
                }
            }
            // (3) A tile unloaded while tiles below it stay uploaded had all 4 children present.
            // (4) A sibling group unloaded on its own (coarsening) leaves its parent uploaded.
            let unl: HashSet<TileKey> = un.iter().copied().collect();
            for &k in un {
                let below = self.g.uploaded.iter().any(|&u| {
                    let mut a = parent(u);
                    while let Some(p) = a {
                        if p == k {
                            return true;
                        }
                        a = parent(p);
                    }
                    false
                });
                if below {
                    assert!(children(k).iter().all(|&c| self.g.present(c)), "{ctx}: {k} unloaded before its children were present");
                    self.parent_drops += 1;
                }
                if let Some(p) = parent(k) {
                    if !unl.contains(&p) && !self.dropped_parent(p, &unl) && children(p).iter().all(|c| unl.contains(c)) {
                        assert!(self.g.uploaded.contains(&p), "{ctx}: group of {p} unloaded without the parent re-delivered");
                        if k == children(p)[0] {
                            self.coarsen_swaps += 1;
                        }
                    }
                }
            }
            // Godot holds exactly what Rust thinks it holds.
            let held: HashSet<TileKey> = self.s.delivered_keys().into_iter().collect();
            assert_eq!(held.len(), self.g.uploaded.len(), "{ctx}");
            self.check_fades(un, ctx);
        }

        /// Fade contract checks for this frame.
        fn check_fades(&self, un: &[TileKey], ctx: &str) {
            let unl: HashSet<TileKey> = un.iter().copied().collect();
            for &k in un {
                let subtree_gone = !self.g.present(super::sim_root(k));
                if subtree_gone {
                    continue; // a root left the 25 km disk: whole subtree, no fade
                }
                // No unload before completion: a fading tile leaves only with its final 0.0.
                if self.prev_fades.contains_key(&k) {
                    assert_eq!(self.fade(k), Some(0.0), "{ctx}: {k} unloaded while fading ({:?})", self.prev_fades.get(&k));
                }
                // No pop-out: a tile visible last frame leaves only by fading out.
                if self.prev_visible.contains(&k) {
                    assert_eq!(self.fade(k), Some(0.0), "{ctx}: visible {k} unloaded without a fade");
                }
            }
            // Outgoing tiles that reach 0.0 at completion are unloaded in the same frame.
            for &(k, f) in &self.fades {
                if f == 0.0 && self.prev_fades.get(&k).is_some_and(|p| *p > 0.0) {
                    assert!(unl.contains(&k), "{ctx}: {k} reached 0 but was not unloaded");
                }
            }
            // No pop-in: a fresh non-root tile visible this frame has a fade entry, starting at 0.
            for &k in &self.fresh {
                if k.z > ROOT_ZOOM && self.g.visible(k) {
                    let f = self.g.fades.get(&k);
                    assert!(f.is_some(), "{ctx}: fresh {k} visible without a fade entry");
                    if !self.prev_fades.contains_key(&k) {
                        assert_eq!(f, Some(&0.0), "{ctx}: fresh {k} starts at {f:?}");
                    }
                }
            }
            // Each tile in at most one fade, fades exactly one zoom level deep.
            let coarse: HashSet<TileKey> = self.s.fades().map(|(k, _)| k).collect();
            for &k in &coarse {
                assert!(parent(k).is_none_or(|p| !coarse.contains(&p)), "{ctx}: nested fades at {k}");
                for c in children(k) {
                    assert!(!coarse.contains(&c), "{ctx}: nested fades at {c}");
                }
            }
        }

        /// `p` is the root of a subtree that left the cover entirely in this batch (an
        /// ancestor unloaded too, or `p` is a root that is no longer present at all).
        fn dropped_parent(&self, p: TileKey, unl: &HashSet<TileKey>) -> bool {
            let mut a = Some(p);
            while let Some(x) = a {
                if unl.contains(&x) {
                    return true;
                }
                a = parent(x);
            }
            !self.g.present(super::sim_root(p))
        }

        fn settle(&mut self, cam: [f64; 3], max_frames: usize) -> usize {
            for i in 0..max_frames {
                let (_, un) = self.frame(cam, [0.0; 3]);
                self.check_frame(&un, "settle");
                if self.s.settled() {
                    return i;
                }
            }
            panic!("did not settle at {cam:?}");
        }

        fn view(&self, cam: [f64; 3]) -> ViewPoint {
            self.s.views(cam, [0.0; 3])[0]
        }
    }

    /// Settled-cover checks: exact cover, all wanted roots present, each leaf in its band.
    fn check_settled_cover(h: &Harness, cam: [f64; 3]) -> [usize; RING_COUNT] {
        let errs = h.g.check();
        assert!(errs.is_empty(), "{errs:?}");
        let v = h.view(cam);
        let held: HashSet<TileKey> = h.s.delivered_keys().into_iter().collect();
        assert_eq!(held, h.g.uploaded.iter().copied().collect::<HashSet<_>>());
        // Every z13 tile with slant < 25 km (box height known) is an uploaded root.
        let mut cands = Vec::new();
        v.root_candidates(&bands(), &mut cands);
        for k in cands {
            if v.slant_m(k, fake_height(k) as f64) < bands().edge_m[3] {
                assert!(h.g.present(k), "missing root {k}");
            }
        }
        // Each visible leaf is in its band: not closer than the finer edge, and its parent
        // closer than the coarser edge (nominal, the cover was built from scratch here).
        let mut per = [0usize; RING_COUNT];
        for k in h.g.visible_tiles() {
            let ring = ring_for_zoom(k.z).unwrap();
            per[ring] += 1;
            let d = v.slant_m(k, fake_height(k) as f64);
            if ring > 0 {
                assert!(d >= bands().edge_m[ring - 1], "{k} at {d:.0} m should be finer");
            }
            if let Some(p) = parent(k) {
                let dp = v.slant_m(p, fake_height(p) as f64);
                assert!(dp < bands().edge_m[ring] * bands().hysteresis, "{k}: parent {p} at {dp:.0} m");
            }
        }
        // The point under the camera is covered exactly once, by the band its distance says.
        let under = h.g.tile_at(v.wx, v.wy);
        assert_eq!(under.len(), 1, "{under:?}");
        per
    }

    #[test]
    fn settled_cover_is_exact_at_reference_altitudes() {
        for (alt, ring_below) in [(300.0, 0usize), (1000.0, 1), (3000.0, 2), (7000.0, 2), (8000.0, 3)] {
            let mut h = Harness::new();
            let cam = [0.0, 0.0, alt];
            let frames = h.settle(cam, 20_000);
            let per = check_settled_cover(&h, cam);
            let v = h.view(cam);
            let under = h.g.tile_at(v.wx, v.wy)[0];
            assert_eq!(ring_for_zoom(under.z), Some(ring_below), "alt {alt}: below is {under}");
            eprintln!("alt {alt}: settled in {frames} frames, visible per ring {per:?}, uploaded {}", h.g.uploaded.len());
            // Requested == delivered (guarantee 2): every tile ever requested was delivered.
            assert_eq!(h.ever_requested, h.ever_delivered, "alt {alt}");
            // No R0 at 1000 m and above; nothing finer than R2 at 3000 m.
            if alt >= 1000.0 {
                assert_eq!(per[0], 0);
            }
            if alt >= 3000.0 {
                assert_eq!(per[1], 0);
            }
        }
    }

    #[test]
    fn scripted_flight_keeps_the_sibling_swap_guarantees() {
        let mut h = Harness::new();
        let dt = 1.0 / 30.0;
        let mut cam = [0.0, 0.0, 300.0];
        h.settle(cam, 20_000);
        let mut camera_holes = 0usize;
        let mut unloads = 0usize;
        // Climb 300 -> 8000 m at 60 m/s, hold, descend at 60 m/s, then 250 m/s east at 300 m.
        let steps: [([f64; 3], usize, &str); 5] = [
            ([0.0, 0.0, 60.0], (7700.0 / 60.0 / dt) as usize, "climb"),
            ([0.0, 0.0, 0.0], 300, "hold"),
            ([0.0, 0.0, -60.0], (7700.0 / 60.0 / dt) as usize, "descent"),
            ([0.0, 0.0, 0.0], 300, "hold low"),
            ([250.0, 0.0, 0.0], (60.0 / dt) as usize, "250 m/s"),
        ];
        let mut r0_under = 0usize;
        let mut cruise_frames = 0usize;
        for (vel, n, name) in steps {
            for _ in 0..n {
                for a in 0..3 {
                    cam[a] += vel[a] * dt;
                }
                let (_, un) = h.frame(cam, vel);
                unloads += un.len();
                h.check_frame(&un, name);
                let v = h.view(cam);
                let under = h.g.tile_at(v.wx, v.wy);
                // One tile, or the two sides of a fade.
                let fading_pair = under.len() == 2 && under.iter().all(|k| h.g.fades.contains_key(k));
                if under.len() != 1 && !fading_pair {
                    camera_holes += 1;
                }
                if name == "250 m/s" {
                    cruise_frames += 1;
                    if under.iter().any(|k| k.z == LEAF_ZOOM) {
                        r0_under += 1;
                    }
                }
            }
        }
        h.settle(cam, 20_000);
        check_settled_cover(&h, cam);
        assert_eq!(h.ever_requested, h.ever_delivered);
        assert_eq!(camera_holes, 0, "point under the camera uncovered");
        eprintln!(
            "flight: {} deliveries ({} light re-deliveries), {unloads} unloads ({} refined parents dropped, {} coarsening swaps), R0 under camera {r0_under}/{cruise_frames} frames at 250 m/s",
            h.delivered_total, h.redelivered_total, h.parent_drops, h.coarsen_swaps
        );
        eprintln!("flight: {} refine fades, {} coarsen fades", h.refine_fades, h.coarsen_fades);
        assert!(h.parent_drops > 0 && h.coarsen_swaps > 0);
        assert!(h.refine_fades > 0 && h.coarsen_fades > 0);
        assert!(h.redelivered_total > 0, "crossing 3300 m re-delivers R2/R3 lights");
        assert!(r0_under * 10 >= cruise_frames * 9);
    }

    #[test]
    fn holding_at_band_edges_does_not_flip_flop() {
        // Altitudes right at an edge for the tile below (fake heights 20..110 m), circling
        // 60 m around a point with +-20 m altitude jitter.
        for alt in [650.0, 2560.0, 7050.0] {
            let mut h = Harness::new();
            let pos = |t: f64| {
                let a = t * 0.2;
                [60.0 * a.cos(), 60.0 * a.sin(), alt + 20.0 * (t * 0.7).sin()]
            };
            let dt = 1.0 / 30.0;
            let lap = (2.0 * std::f64::consts::PI / 0.2 / dt) as usize;
            let mut t = 0.0;
            // One lap to settle the cover (plus whatever loading needs).
            for _ in 0..lap {
                t += dt;
                h.frame(pos(t), [0.0; 3]);
            }
            for _ in 0..2000 {
                if h.s.settled() {
                    break;
                }
                t += dt;
                h.frame(pos(t), [0.0; 3]);
            }
            let tracked = |h: &Harness| h.s.entries().map(|e| e.key).collect::<HashSet<_>>();
            let before = tracked(&h);
            // A flip-flop is a tile that enters and leaves (or leaves and re-enters) the cover.
            let mut events: std::collections::HashMap<TileKey, Vec<&str>> = Default::default();
            let mut prev = before.clone();
            let mut held_by_hysteresis = 0usize;
            for _ in 0..3 * lap {
                t += dt;
                let (_, un) = h.frame(pos(t), [0.0; 3]);
                h.check_frame(&un, "hold");
                let views = h.s.views(pos(t), [0.0; 3]);
                for e in h.s.entries().filter(|e| e.split) {
                    let d = views[e.ring - 1].slant_m(e.key, e.height.map_or(bands().default_tile_height_m, |x| x as f64));
                    if d >= bands().edge_m[e.ring - 1] {
                        held_by_hysteresis += 1;
                    }
                }
                let now = tracked(&h);
                for k in now.difference(&prev) {
                    events.entry(*k).or_default().push("in");
                }
                for k in prev.difference(&now) {
                    events.entry(*k).or_default().push("out");
                }
                prev = now;
            }
            let flips: Vec<_> = events.iter().filter(|(_, v)| v.len() > 1).collect();
            eprintln!(
                "alt {alt}: {} tiles changed once after the settle lap, {} flip-flops, {held_by_hysteresis} split-node frames past the nominal edge",
                events.len(),
                flips.len()
            );
            assert!(held_by_hysteresis > 0, "alt {alt}: the hold never exercised the hysteresis band");
            assert!(flips.is_empty(), "alt {alt}: flip-flop {flips:?}");
        }
    }

    #[test]
    fn light_mode_switch_redelivers_with_hysteresis_and_rate_limit() {
        let mut h = Harness::new();
        h.settle([0.0, 0.0, 3000.0], 20_000);
        assert!(!h.s.clustered());
        let r23 = h.s.entries().filter(|e| e.ring >= 2).count();
        // 3200 m: inside the hysteresis band, nothing happens.
        h.settle([0.0, 0.0, 3200.0], 20_000);
        assert_eq!(h.redelivered_total, 0);
        // 3400 m: clustered; every held R2/R3 tile comes again, at most one per frame.
        let cam = [0.0, 0.0, 3400.0];
        let mut frames = 0;
        let base = h.redelivered_total;
        loop {
            let before = h.redelivered_total;
            let (_, un) = h.frame(cam, [0.0; 3]);
            h.check_frame(&un, "relight");
            assert!(h.redelivered_total - before <= 1);
            frames += 1;
            if h.s.settled() {
                break;
            }
            assert!(frames < 20_000);
        }
        assert!(h.s.clustered());
        let re = h.redelivered_total - base;
        let still = h.s.entries().filter(|e| e.ring >= 2 && e.held).count();
        assert!(re >= still.min(r23) && re > 0, "re-delivered {re}, held R2/R3 {still}");
        assert!(h.s.entries().filter(|e| e.held && e.ring >= 2).all(|e| e.lights_mode));
        // Back to 2800 m: still clustered (hysteresis); 2600 m: single lights again.
        h.settle([0.0, 0.0, 2800.0], 20_000);
        assert!(h.s.clustered());
        h.settle([0.0, 0.0, 2600.0], 20_000);
        assert!(!h.s.clustered());
        assert!(h.s.entries().filter(|e| e.held && e.ring >= 2).all(|e| !e.lights_mode));
    }

    #[test]
    fn synthetic_city_streams_real_meshes_and_lights() {
        let mut h = Harness::new();
        let frame = Frame::new(VILNIUS.0, VILNIUS.1);
        h.s = Scheduler::new(frame, true, dp());
        h.w.real = Some(frame);
        let mut tris = [0usize; RING_COUNT];
        let mut ground = [0usize; RING_COUNT];
        let mut lights = [0usize; RING_COUNT];
        for i in 0..20_000 {
            h.now += Duration::from_secs_f64(DT);
            h.w.step(&mut h.s, h.now);
            h.s.update([0.0, 0.0, 300.0], [0.0; 3], h.now, &mut h.jobs);
            h.w.submit(&mut h.jobs);
            while let Some(t) = h.s.pop_ready(true) {
                tris[t.ring] += t.mesh.triangle_count();
                ground[t.ring] += t.ground.triangle_count();
                lights[t.ring] += t.light_arrays().0.len();
                h.g.upload(t.key);
            }
            h.fades.clear();
            h.s.drain_fades(&mut h.fades);
            h.g.set_fades(&h.fades);
            let mut un = Vec::new();
            h.s.drain_unload(&mut un);
            for k in un {
                h.g.unload(k);
            }
            assert!(h.g.check().is_empty());
            if h.s.settled() {
                eprintln!("synthetic settled after {i} frames: tris {tris:?} ground tris {ground:?} lights {lights:?}");
                break;
            }
        }
        assert!(h.s.settled());
        assert!(tris[0] > 0 && tris[1] > 0 && tris[2] > 0 && tris[3] == 0);
        assert!(ground[0] > 0 && ground[1] > 0 && ground[2] > 0 && ground[3] == 0);
        assert!(lights[0] == 0 && lights[1] == 0 && lights[2] > 0 && lights[3] > 0);
    }

    #[test]
    fn failures_back_off_and_retry_and_block_only_their_group() {
        let frame = Frame::new(VILNIUS.0, VILNIUS.1);
        let mut s = Scheduler::new(frame, false, dp());
        let t0 = Instant::now();
        let mut jobs = Vec::new();
        s.update([0.0, 0.0, 8000.0], [0.0; 3], t0, &mut jobs);
        // Only R3 roots at 8 km: 4 fetches in flight.
        assert_eq!(jobs.len(), 4);
        for j in jobs.drain(..) {
            let Job::Fetch { key, ring, job } = j else { panic!() };
            let w = s.handle(WorkerMsg::Fetched { key, ring, job, result: Err("boom".into()) }, t0);
            assert!(w.is_some());
        }
        assert_eq!(s.stats().failed[3], 4);
        s.update([0.0, 0.0, 8000.0], [0.0; 3], t0, &mut jobs);
        assert_eq!(jobs.len(), 4, "next tiles go out while the failed ones wait");
        jobs.clear();
        s.update([0.0, 0.0, 8000.0], [0.0; 3], t0 + dp().backoff(1), &mut jobs);
        assert_eq!(s.stats().failed[3], 0);
        assert_eq!(dp().backoff(1), Duration::from_millis(500));
        assert_eq!(dp().backoff(3), Duration::from_millis(2000));
        assert_eq!(dp().backoff(20), Duration::from_secs(30));
    }

    #[test]
    fn stale_results_are_ignored() {
        let frame = Frame::new(VILNIUS.0, VILNIUS.1);
        let mut s = Scheduler::new(frame, false, dp());
        let now = Instant::now();
        let mut jobs = Vec::new();
        s.update([0.0, 0.0, 300.0], [0.0; 3], now, &mut jobs);
        let Job::Fetch { key, ring, job } = jobs[0] else { panic!() };
        // Fly far away: the entry is dropped while its fetch is in flight.
        let mut more = Vec::new();
        s.update([200_000.0, 0.0, 300.0], [0.0; 3], now, &mut more);
        assert!(s.handle(WorkerMsg::Fetched { key, ring, job, result: Ok((Bytes::new(), Source::Network)) }, now).is_none());
        assert!(s.pop_ready(true).is_none());
        assert_eq!(s.inflight().0[ring], dp().streaming.max_fetch_per_band - 1);
    }

    #[test]
    fn dropped_parent_forgets_pending_work() {
        for pending in [TileState::Queued, TileState::Failed { retry_at: Instant::now() }, TileState::Fetched(Bytes::new())] {
            let mut s = Scheduler::new(Frame::new(VILNIUS.0, VILNIUS.1), false, dp());
            let k = TileKey::new(14, 9000, 5000);
            let e = s.touch(k, 2);
            e.held = true;
            e.split = true;
            e.state = pending;
            let job = e.job;
            for c in children(k) {
                s.touch(c, 3).held = true;
            }
            s.try_drop(k);
            let e = &s.entries[&k];
            assert!(!e.held);
            assert!(matches!(e.state, TileState::Idle), "pending work survived the drop: {:?}", e.state);
            assert!(e.job > job, "in-flight results must be orphaned");
            let mut jobs = Vec::new();
            s.dispatch(&mut jobs);
            assert!(jobs.iter().all(|j| !matches!(j, Job::Fetch { key, .. } | Job::Decode { key, .. } if *key == k)));
        }
    }

    #[test]
    fn children_wait_for_an_effective_parent() {
        let mut h = Harness::new();
        // Let everything decode before delivering anything: deliveries must still come
        // root-first per branch, and never under a parent that is part of a fade.
        let cam = [0.0, 0.0, 300.0];
        for _ in 0..3000 {
            h.w.step(&mut h.s, h.now);
            h.s.update(cam, [0.0; 3], h.now, &mut h.jobs);
            h.w.submit(&mut h.jobs);
        }
        let mut order = Vec::new();
        for _ in 0..20_000 {
            // Drain everything deliverable, then one update (fades advance only there).
            loop {
                let fading: HashSet<TileKey> = h.s.fades().map(|(k, _)| k).collect();
                let in_fade = |k: TileKey| fading.contains(&k) || parent(k).is_some_and(|p| fading.contains(&p));
                let Some(t) = h.s.pop_ready(true) else { break };
                if let Some(p) = parent(t.key) {
                    assert!(h.g.present(p), "{} before its parent", t.key);
                    assert!(!in_fade(p), "{} delivered under fading {p}", t.key);
                    for s in siblings(p) {
                        if p.z > ROOT_ZOOM {
                            assert!(h.g.present(s), "{} before parent's sibling {s}", t.key);
                        }
                    }
                }
                h.g.upload(t.key);
                order.push(t.key);
            }
            h.fades.clear();
            h.s.drain_fades(&mut h.fades);
            h.g.set_fades(&h.fades);
            let mut un = Vec::new();
            h.s.drain_unload(&mut un);
            for k in un {
                h.g.unload(k);
            }
            assert!(h.g.check().is_empty(), "{:?}", h.g.check());
            if h.s.settled() {
                break;
            }
            h.now += Duration::from_secs_f64(DT);
            h.w.step(&mut h.s, h.now);
            h.s.update(cam, [0.0; 3], h.now, &mut h.jobs);
            h.w.submit(&mut h.jobs);
        }
        assert!(!order.is_empty());
        assert!(h.s.settled());
    }

    #[test]
    fn far_flight_unloads_everything_in_subtree_batches() {
        let mut h = Harness::new();
        h.settle([0.0, 0.0, 300.0], 20_000);
        let n = h.g.uploaded.len();
        let (_, un) = h.frame([200_000.0, 0.0, 300.0], [0.0; 3]);
        h.check_frame(&un, "teleport");
        assert_eq!(un.len(), n);
        assert!(h.g.uploaded.is_empty());
    }

    // ---- Dithered LOD fades -------------------------------------------------------------

    #[test]
    fn fade_math_refine_coarsen_reverse_and_cap() {
        let t0 = Instant::now();
        let gap = fade_gap_m(&bands(), 1); // 90 m (R0 edge 600 m)
        assert!((gap - 90.0).abs() < 1e-9);
        assert!((fade_gap_m(&bands(), 3) - 1050.0).abs() < 1e-9);
        // Refine: approaching by 45 m = half way; hovering keeps it; receding never undoes it.
        let mut f = Fade::new(true, 580.0, t0);
        assert_eq!((f.reported(), f.t), (0.0, 0.0));
        assert!(!f.advance(535.0, gap, t0, fade_cap()));
        assert!((f.t - 0.5).abs() < 1e-12);
        assert!(!f.advance(535.0, gap, t0 + Duration::from_millis(500), fade_cap()));
        assert!(!f.advance(560.0, gap, t0 + Duration::from_millis(600), fade_cap()));
        assert!((f.t - 0.5).abs() < 1e-12, "receding must not undo a refine fade");
        // Reverse at 560 m: coarsen continues from 0.5; 18 m farther = 0.2 down.
        f.retarget(false, 560.0);
        assert!(!f.advance(578.0, gap, t0 + Duration::from_millis(700), fade_cap()));
        assert!((f.t - 0.3).abs() < 1e-12);
        // Done when the target is reached.
        assert!(f.advance(700.0, gap, t0 + Duration::from_millis(800), fade_cap()));
        assert_eq!(f.t, 0.0);
        // Coarsen from scratch: children 1 -> 0 with distance, parent the complement.
        let mut c = Fade::new(false, 700.0, t0);
        assert_eq!(c.reported(), 1.0);
        assert!(!c.advance(745.0, gap, t0, fade_cap()));
        assert!((c.reported() - 0.5).abs() < 1e-6);
        // Safety cap: a frozen fade completes after the cap (fades.cap_s), not before.
        let mut h = Fade::new(true, 580.0, t0);
        assert!(!h.advance(580.0, gap, t0 + fade_cap() - Duration::from_millis(1), fade_cap()));
        assert!(h.advance(580.0, gap, t0 + fade_cap(), fade_cap()));
        assert_eq!(h.t, 0.0, "the cap completes without inventing progress");
        // While active the outgoing side never reads 0; values are exact complements in f32.
        let mut a = Fade::new(true, 600.0, t0);
        a.advance(600.0 - 0.9999 * gap, gap, t0, fade_cap());
        let r = a.reported();
        assert!(r < 1.0 && (1.0 - r) > 0.0 && r + (1.0 - r) == 1.0);
        let mut b = Fade::new(false, 600.0, t0);
        b.advance(600.0 + 0.9999 * gap, gap, t0, fade_cap());
        assert!(b.reported() > 0.0);
    }

    /// Per-frame view of one swap: parent `p` and its 4 children.
    #[derive(Debug, Clone)]
    struct Obs {
        p_fade: Option<f32>,
        kid_fades: [Option<f32>; 4],
        p_up: bool,
        kids_up: [bool; 4],
        p_unloaded: bool,
        kids_unloaded: [bool; 4],
        active: Option<bool>, // Some(refine) while Rust has a fade for p
    }

    fn observe(h: &Harness, p: TileKey, un: &[TileKey]) -> Obs {
        let kids = children(p);
        Obs {
            p_fade: h.fade(p),
            kid_fades: kids.map(|c| h.fade(c)),
            p_up: h.g.uploaded.contains(&p),
            kids_up: kids.map(|c| h.g.uploaded.contains(&c)),
            p_unloaded: un.contains(&p),
            kids_unloaded: kids.map(|c| un.contains(&c)),
            active: h.s.fades().find(|(k, _)| *k == p).map(|(_, f)| f.refine),
        }
    }

    /// The z15 (R1) tile under the camera at the harness origin.
    fn r1_under(h: &Harness, cam: [f64; 3]) -> TileKey {
        let v = h.view(cam);
        let n = (1u64 << 15) as f64;
        TileKey::new(15, (v.wx * n).floor() as u32, (v.wy * n).floor() as u32)
    }

    /// Fly from `from` to `to` altitude (velocity input 0: the look-ahead stays on the camera,
    /// so the slant distance follows the position exactly), then hold until settled.
    fn fly(h: &mut Harness, p: TileKey, from: f64, to: f64, step: f64) -> Vec<Obs> {
        let mut obs = Vec::new();
        let mut alt = from;
        for _ in 0..20_000 {
            if (to - alt).abs() > step {
                alt += step * (to - alt).signum();
            } else {
                alt = to;
            }
            let (_, un) = h.frame([0.0, 0.0, alt], [0.0; 3]);
            h.check_frame(&un, "fly");
            obs.push(observe(h, p, &un));
            if alt == to && h.s.settled() {
                return obs;
            }
        }
        panic!("did not settle");
    }

    /// First and completion frame of the fade of `p` (completion = the frame with finals).
    fn fade_span(obs: &[Obs]) -> (usize, usize) {
        let s = obs.iter().position(|o| o.active.is_some()).expect("no fade started");
        let e = s + obs[s..].iter().position(|o| o.active.is_none()).expect("fade never completed");
        (s, e)
    }

    #[test]
    fn refine_fade_keeps_parent_until_complete() {
        let mut h = Harness::new();
        h.settle([0.0, 0.0, 1000.0], 20_000);
        let p = r1_under(&h, [0.0, 0.0, 1000.0]);
        assert!(h.g.visible(p) && children(p).iter().all(|c| !h.g.uploaded.contains(c)));
        // Descend at 90 m/s (3 m per frame) to 300 m.
        let obs = fly(&mut h, p, 1000.0, 300.0, 3.0);
        let (s, e) = fade_span(&obs);
        // Start: the frame the 4th child arrives; children 0 (hidden), parent 1.
        assert_eq!(obs[s].active, Some(true));
        assert!(obs[s].kids_up.iter().all(|u| *u) && obs[s].p_up);
        assert!(obs[s - 1].kids_up.iter().any(|u| !*u), "fade starts with the last child");
        assert_eq!(obs[s].p_fade, Some(1.0));
        assert!(obs[s].kid_fades.iter().all(|f| *f == Some(0.0)));
        // Active: parent stays uploaded, values complementary and rising.
        let mut last = 0.0f32;
        for o in &obs[s..e] {
            assert!(o.p_up && !o.p_unloaded, "parent unloaded during its fade");
            let k = o.kid_fades[0].unwrap();
            assert!(o.kid_fades.iter().all(|f| *f == Some(k)));
            assert_eq!(o.p_fade.unwrap() + k, 1.0);
            assert!(k >= last && k < 1.0);
            last = k;
        }
        // Completion: finals (children 1.0, parent 0.0) and the parent's unload, same frame.
        assert_eq!(obs[e].p_fade, Some(0.0));
        assert!(obs[e].kid_fades.iter().all(|f| *f == Some(1.0)));
        assert!(obs[e].p_unloaded && !obs[e].p_up);
        assert!(obs[e + 1].p_fade.is_none() && obs[e + 1].kid_fades.iter().all(|f| f.is_none()));
        // Driven by distance: 90 m gap at 90 m/s = ~1 s, well before the 3 s cap.
        let frames = e - s;
        assert!((20..60).contains(&frames), "refine fade took {frames} frames");
        eprintln!("refine fade of {p}: {frames} frames");
    }

    #[test]
    fn coarsen_fade_redelivers_parent_then_unloads_children() {
        let mut h = Harness::new();
        h.settle([0.0, 0.0, 300.0], 20_000);
        let p = r1_under(&h, [0.0, 0.0, 300.0]);
        assert!(!h.g.uploaded.contains(&p) && children(p).iter().all(|c| h.g.visible(*c)));
        let obs = fly(&mut h, p, 300.0, 1000.0, 3.0);
        let (s, e) = fade_span(&obs);
        assert_eq!(obs[s].active, Some(false));
        // Start: the parent's re-delivery frame; parent 0 (hidden), children 1.
        assert!(obs[s].p_up && !obs[s - 1].p_up, "fade starts with the parent's delivery");
        assert_eq!(obs[s].p_fade, Some(0.0));
        assert!(obs[s].kid_fades.iter().all(|f| *f == Some(1.0)));
        // No child leaves before completion.
        for o in &obs[..e] {
            assert!(o.kids_unloaded.iter().all(|u| !*u), "child unloaded before the fade completed");
        }
        let mut last = 0.0f32;
        for o in &obs[s..e] {
            assert!(o.kids_up.iter().all(|u| *u) && o.p_up);
            let pf = o.p_fade.unwrap();
            assert!(o.kid_fades.iter().all(|f| f.unwrap() + pf == 1.0));
            assert!(pf >= last && pf < 1.0);
            last = pf;
        }
        // Completion: parent 1.0, children 0.0 and unloaded in the same frame.
        assert_eq!(obs[e].p_fade, Some(1.0));
        assert!(obs[e].kid_fades.iter().all(|f| *f == Some(0.0)));
        assert!(obs[e].kids_unloaded.iter().all(|u| *u) && obs[e].p_up);
        assert!(obs[e + 1].p_fade.is_none());
        let frames = e - s;
        assert!((20..60).contains(&frames), "coarsen fade took {frames} frames");
    }

    #[test]
    fn hovering_freezes_a_fade_until_the_safety_cap() {
        let mut h = Harness::new();
        h.settle([0.0, 0.0, 1000.0], 20_000);
        let p = r1_under(&h, [0.0, 0.0, 1000.0]);
        // Descend until p's refine fade is roughly a quarter through, then hover.
        let mut alt = 1000.0;
        let mut start = None;
        let mut frame = 0usize;
        let frozen = loop {
            alt -= 3.0;
            let (_, un) = h.frame([0.0, 0.0, alt], [0.0; 3]);
            h.check_frame(&un, "descend");
            frame += 1;
            let o = observe(&h, p, &un);
            if o.active.is_some() && start.is_none() {
                start = Some(frame);
            }
            if let Some(k) = o.kid_fades[0].filter(|_| o.active.is_some()) {
                if k >= 0.2 {
                    break k;
                }
            }
            assert!(alt > 300.0, "no fade");
        };
        let start = start.unwrap();
        let mut done = None;
        for i in 0..200 {
            // Jittery velocity input moves the look-ahead point by up to 300 m; the fade is
            // driven by the camera position only, so it must stay frozen anyway.
            let vel = if i % 2 == 0 { [0.0, 0.0, -60.0] } else { [0.0; 3] };
            let (_, un) = h.frame([0.0, 0.0, alt], vel);
            h.check_frame(&un, "hover");
            frame += 1;
            let o = observe(&h, p, &un);
            if o.active.is_some() {
                assert_eq!(o.kid_fades[0], Some(frozen), "hovering must freeze the fade");
                assert!(o.p_up);
            } else {
                // Force-completed in its direction: children 1.0, parent unloaded.
                assert!(o.kid_fades.iter().all(|f| *f == Some(1.0)));
                assert!(o.p_unloaded);
                done = Some(frame);
                break;
            }
        }
        let took = done.expect("cap never fired") - start;
        let cap_frames = (fade_cap().as_secs_f64() / DT).round() as usize;
        assert!((cap_frames..=cap_frames + 1).contains(&took), "cap after {took} frames");
    }

    #[test]
    fn safety_cap_completes_fades_started_while_hovering() {
        // A cold start hovering at 450 m refines R3 -> R0 with the camera standing still: every
        // fade sits at 0 and is completed by the cap, one level after the other.
        let mut h = Harness::new();
        let cam = [0.0, 0.0, 450.0];
        let mut started: AHashMap<TileKey, usize> = AHashMap::new();
        let mut durations = Vec::new();
        for i in 0..20_000 {
            let (_, un) = h.frame(cam, [0.0; 3]);
            h.check_frame(&un, "hover start");
            let active: AHashMap<TileKey, f64> = h.s.fades().map(|(k, f)| (k, f.t)).collect();
            for (k, t) in &active {
                assert_eq!(*t, 0.0, "{k}: a hovering fade must not progress");
                started.entry(*k).or_insert(i);
            }
            started.retain(|k, s| {
                if active.contains_key(k) {
                    return true;
                }
                durations.push(i - *s);
                false
            });
            if h.s.settled() {
                break;
            }
        }
        assert!(h.s.settled() && !durations.is_empty());
        let cap_frames = (fade_cap().as_secs_f64() / DT).round() as usize;
        assert!(durations.iter().all(|d| (cap_frames..=cap_frames + 1).contains(d)), "{durations:?}");
        eprintln!("hover start: {} fades, all completed by the cap", durations.len());
    }

    #[test]
    fn reversed_fade_continues_from_its_value() {
        let mut h = Harness::new();
        h.settle([0.0, 0.0, 1000.0], 20_000);
        let p = r1_under(&h, [0.0, 0.0, 1000.0]);
        let mut alt = 1000.0;
        let mut obs = Vec::new();
        // Down until the refine fade is a third through ...
        loop {
            alt -= 3.0;
            let (_, un) = h.frame([0.0, 0.0, alt], [0.0; 3]);
            h.check_frame(&un, "down");
            let o = observe(&h, p, &un);
            let k = o.kid_fades[0].filter(|_| o.active == Some(true));
            obs.push(o);
            if k.is_some_and(|k| k >= 0.33) {
                break;
            }
            assert!(alt > 300.0);
        }
        let at_turn = obs.last().unwrap().kid_fades[0].unwrap();
        // ... then straight back up at 450 m/s (past the 15 % hysteresis within the cap).
        let mut turned = None;
        for _ in 0..2000 {
            alt = (alt + 15.0).min(1000.0);
            let (_, un) = h.frame([0.0, 0.0, alt], [0.0; 3]);
            h.check_frame(&un, "up");
            let o = observe(&h, p, &un);
            if o.active == Some(false) && turned.is_none() {
                turned = Some(o.kid_fades[0].unwrap());
            }
            let end = o.active.is_none() && o.p_fade == Some(1.0);
            obs.push(o);
            if end {
                break;
            }
        }
        let turned = turned.expect("the fade never reversed");
        assert!(turned <= at_turn && turned > 0.0, "reversal jumped: {at_turn} -> {turned}");
        let o = obs.last().unwrap();
        assert!(o.p_up && o.kids_unloaded.iter().all(|u| *u), "coarsened back to the parent");
        assert!(o.kid_fades.iter().all(|f| *f == Some(0.0)));
        // The parent was never unloaded (and so never re-fetched) on the way.
        assert!(obs.iter().all(|o| o.p_up && !o.p_unloaded));
    }

    #[test]
    fn ground_draw_order_is_bottom_level_first() {
        // Godot's Mobile renderer uses GREATER_OR_EQUAL (reversed Z): on a depth tie the later
        // triangle wins, so the ground mesh must be emitted bottom stacking level first.
        let frame = Frame::new(VILNIUS.0, VILNIUS.1);
        let k = crate::geo::tile_for(VILNIUS.0, VILNIUS.1, 14);
        let data = build_tile(k, 2, frame, &Payload::Synthetic, &Profile::default()).unwrap();
        let m = &data.ground;
        assert!(m.triangle_count() > 0);
        let mut last = f32::NEG_INFINITY;
        for tri in m.indices.chunks_exact(3) {
            let y = m.positions[tri[0] as usize][1];
            assert!(y >= last - 1e-4, "ground level {y} after {last}");
            last = last.max(y);
        }
    }
}
