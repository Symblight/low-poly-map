//! Tile boxes and terrain error proxies of the cover (perf review 2, item 1).
//!
//! The terrain [min, max] under a tile (`Terrain::tile_range`) and the terrain layer's error
//! proxy both come from the tile's 9 x 9 probe (81 height evaluations, about 0.1 ms). They are
//! pure per key and memoized, but cold (the first frame after `configure_world`, new root
//! candidates at the cover's edge) a frame needed hundreds of them: 24-25 ms in the first
//! `update_camera`, 2-3 ms spikes in flight.
//!
//! **Inline** (default, tests and tools): a miss is probed on the spot, as before.
//! **Async** ([`Scheduler::with_async_boxes`], the `World`): a miss is requested instead; the
//! scheduler hands the requests out as a [`ProbeBatch`] that the world runs on the CPU pool
//! (`Worker::probe`), and the next `update` takes the probes in. Until a value is known the
//! cover waits for it, never guesses: a root candidate without a box is not added, a node does
//! not start a split until its 4 children's boxes (and, terrain, its own error proxy) are known,
//! and what is already in the cover keeps its state (an entry carries its box). So a cold area
//! fills a frame or two later, never with a hole, and once the values are known every decision
//! is the inline one (the values are bit-identical: same probe, same formulas).

use ahash::AHashSet;
use crossbeam_channel::{Receiver, Sender};

use super::Scheduler;
use crate::geo::TileKey;
use crate::rings::{children, ViewPoint};
use crate::terrain::height::Probe;

/// A probe result; `None` = the job was dropped (pool shut down): the key may be asked again.
pub type ProbeResult = (TileKey, Option<Probe>);

/// Keys to probe on a pool thread; the results go back through `reply`.
pub struct ProbeBatch {
    pub keys: Vec<TileKey>,
    pub reply: Sender<Vec<ProbeResult>>,
}

/// A node within this factor of its refine distance asks for its children's boxes ahead of the
/// split (async mode), so a split in flight is not held up by a probe round trip.
const PREFETCH_FACTOR: f64 = 1.5;

/// The async request state.
pub(super) struct BoxRequests {
    reply: Sender<Vec<ProbeResult>>,
    results: Receiver<Vec<ProbeResult>>,
    /// Requested and not answered yet.
    pending: AHashSet<TileKey>,
    /// Requested this frame (the next batch).
    want: Vec<TileKey>,
    /// Probes that arrived before their parent's error proxy (terrain layer, retried).
    early: Vec<(TileKey, Probe)>,
    /// Keys requested since the scheduler started.
    pub requested: u64,
    /// Probe batches taken in since the scheduler started: the known boxes and error proxies
    /// change only when this does (a terrain plan that waited on a probe stays valid until then).
    generation: u64,
}

impl BoxRequests {
    fn new() -> Self {
        let (reply, results) = crossbeam_channel::unbounded();
        Self { reply, results, pending: AHashSet::new(), want: Vec::new(), early: Vec::new(), requested: 0, generation: 0 }
    }

    fn ask(&mut self, k: TileKey) {
        if self.pending.insert(k) {
            self.want.push(k);
            self.requested += 1;
        }
    }
}

impl Scheduler {
    /// Probe cold tile boxes off the main thread (see the module docs). Flat worlds never probe.
    pub fn with_async_boxes(mut self) -> Self {
        self.enable_async_boxes();
        self
    }

    fn enable_async_boxes(&mut self) {
        if self.terrain.is_active() {
            self.box_requests = Some(BoxRequests::new());
        }
    }

    /// The probe requests of this frame (async mode; `None` when there are none).
    pub fn take_probe_batch(&mut self) -> Option<ProbeBatch> {
        let r = self.box_requests.as_mut()?;
        if r.want.is_empty() {
            return None;
        }
        Some(ProbeBatch { keys: std::mem::take(&mut r.want), reply: r.reply.clone() })
    }

    /// No probe requested or on the way (the cover is final as far as boxes go).
    pub(super) fn probes_idle(&self) -> bool {
        self.box_requests.as_ref().is_none_or(|r| r.pending.is_empty() && r.want.is_empty())
    }

    /// Probe batches taken in so far (0 inline): what a plan that waited on a probe depends on.
    pub(super) fn probe_generation(&self) -> u64 {
        self.box_requests.as_ref().map_or(0, |r| r.generation)
    }

    /// Probes requested and not answered yet (tests, stats).
    pub fn probes_pending(&self) -> usize {
        self.box_requests.as_ref().map_or(0, |r| r.pending.len())
    }

    /// Terrain [min, max] under `k`, or `None` while its probe is on the way (async mode only;
    /// the miss is requested). `[0, 0]` with the terrain off.
    pub(super) fn box_known(&mut self, k: TileKey) -> Option<[f64; 2]> {
        if !self.terrain.is_active() {
            return Some([0.0, 0.0]);
        }
        if let Some(e) = self.entries.get(&k) {
            return Some(e.terrain_box);
        }
        if let Some(g) = self.terrain_cache.get(&k) {
            return Some(g);
        }
        match &mut self.box_requests {
            Some(r) => {
                r.ask(k);
                None
            }
            None => Some(self.terrain_box_of(k)),
        }
    }

    /// Terrain layer: the error proxy of `k`, or `None` while its probe is on the way (async
    /// mode; requested). 0 on the vector layer.
    pub(super) fn error_known(&mut self, k: TileKey) -> Option<f64> {
        let p = self.proxy.as_mut()?;
        if p.contains(k) || self.box_requests.is_none() {
            return Some(p.error_m(k));
        }
        // The clamp reads the parent's proxy: ask for the ancestors that lack one too.
        let root = self.layer.root_zoom;
        let r = self.box_requests.as_mut().expect("async");
        let mut a = k;
        loop {
            r.ask(a);
            if a.z <= root || a.z == 0 {
                break;
            }
            a = TileKey::new(a.z - 1, a.x / 2, a.y / 2);
            if p.contains(a) {
                break;
            }
        }
        None
    }

    /// Whether `k`'s 4 children have known boxes (asking for the missing ones). Inline mode:
    /// always. Async: only asked while `near` (within [`PREFETCH_FACTOR`] of the refine
    /// distance), else `false` without asking.
    pub(super) fn children_boxes_known(&mut self, k: TileKey, near: bool) -> bool {
        if self.box_requests.is_none() || !self.terrain.is_active() {
            return true;
        }
        if !near {
            return false;
        }
        let mut all = true;
        for c in children(k) {
            all &= self.box_known(c).is_some();
        }
        all
    }

    /// Vector layer: whether `k` may start a split as far as boxes go (its 4 children's boxes
    /// known; asked for while it is within the prefetch range of `refine` seen from `view`).
    /// A node already split has them in its children's entries.
    pub(super) fn split_boxes_known(&mut self, k: TileKey, view: &ViewPoint, inherited_h: f64, refine: f64) -> bool {
        if self.box_requests.is_none() {
            return true;
        }
        let near = match self.entries.get(&k) {
            Some(e) => e.split || self.near_split(e.split_d, refine),
            None => match self.box_known(k) {
                Some(g) => self.near_split(view.slant_box_m(k, g[0], g[1] + inherited_h), refine),
                None => true,
            },
        };
        self.children_boxes_known(k, near)
    }

    /// Whether distance `d` is within the prefetch range of refine distance `refine`.
    pub(super) fn near_split(&self, d: f64, refine: f64) -> bool {
        d < refine * self.layer.hysteresis.max(1.0) * PREFETCH_FACTOR
    }

    /// Take in the probes that arrived (start of `update`): boxes into the memo, error proxies
    /// (terrain layer) parents first.
    pub(super) fn absorb_probes(&mut self) {
        let Some(r) = self.box_requests.as_mut() else { return };
        let mut got: Vec<(TileKey, Probe)> = std::mem::take(&mut r.early);
        while let Ok(batch) = r.results.try_recv() {
            r.generation += 1;
            for (k, p) in batch {
                r.pending.remove(&k);
                if let Some(p) = p {
                    got.push((k, p));
                }
            }
        }
        if got.is_empty() {
            return;
        }
        got.sort_unstable_by_key(|(k, _)| *k);
        let root = self.layer.root_zoom;
        for (k, p) in got {
            if !self.entries.contains_key(&k) {
                let g = self.terrain.range_from_probe(p.lo, p.hi, p.step);
                self.terrain_cache.insert(k, g);
            }
            if let Some(proxy) = self.proxy.as_mut() {
                let parent = (k.z > root && k.z > 0).then(|| TileKey::new(k.z - 1, k.x / 2, k.y / 2));
                let r = self.box_requests.as_mut().expect("async");
                match parent {
                    // The parent's probe is still on the way: keep this one for later.
                    Some(pk) if !proxy.contains(pk) && r.pending.contains(&pk) => r.early.push((k, p)),
                    _ => {
                        proxy.error_m_probed(k, &p);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "boxes_tests.rs"]
pub(super) mod tests;
