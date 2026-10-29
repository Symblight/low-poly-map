use super::sim::GodotSim;
use super::*;
use bytes::Bytes;
use crate::fetch::{FetchError, Source};
use crate::lights::LightSet;
use crate::mesher::MeshData;
use crate::terrain::tin::TerrainTile;
use crate::worker::TileData;
use crate::rings::{parent, ring_for_zoom, siblings, BandParams, LEAF_ZOOM, RING_COUNT, ROOT_ZOOM};
use crate::worker::build_tile;
use std::collections::{HashSet, VecDeque};

pub(super) const VILNIUS: (f64, f64) = (54.6872, 25.2797);

mod cover;
mod data_tiles;
mod dispatch;
mod fades;
mod fakes;
mod terrain_cover;
mod terrain_sse;

use fakes::*;

/// Harness frame time (s): `now` advances by this per frame.
const DT: f64 = 1.0 / 30.0;

/// One harness frame in contract order: update -> poll_ready(2) -> poll_fades ->
/// poll_unload, applied to the simulated Godot side. Returns (delivered, unloaded).
pub(super) struct Harness {
    pub(super) s: Scheduler,
    w: FakeWorker,
    /// Memory budget of the data tiles (Tiles v2 tests; synced like `World` does).
    mem: Option<crate::budget::MemoryBudget>,
    g: GodotSim,
    /// The layer under test (zooms for the contract checks).
    l: Layer,
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
    /// Both sides of every fade still active at the start of this frame (finals excluded).
    prev_active: HashSet<TileKey>,
    /// Visible tiles at the end of the previous frame.
    prev_visible: HashSet<TileKey>,
    /// Fresh (non re-delivery) tiles of this frame.
    fresh: Vec<TileKey>,
    refine_fades: usize,
    coarsen_fades: usize,
    /// Every delivery (key, re-delivery?) in order.
    deliveries: Vec<(TileKey, bool)>,
    /// Tiles that were split (finer detail already wanted) when they were delivered fresh:
    /// first-fill stand-ins, recorded at delivery from the cover state.
    delivered_split: HashSet<TileKey>,
    /// First-fill swaps: a stand-in unloaded at once (no fade) when its 4 children arrived.
    first_fill_swaps: usize,
    /// Assert on every fresh delivery that no deliverable ready tile was nearer (order key).
    check_order: bool,
    /// Most fades active at once, and swaps completed without a fade (`fades.max_active`).
    max_fades_seen: usize,
    capped_seen: usize,
    /// Landmark tiles (`l/` keys, api 1.11) the host holds, deliveries and unloads.
    lm_held: HashSet<TileKey>,
    lm_delivered: usize,
    lm_unloaded: usize,
}

impl Harness {
    pub(super) fn new() -> Self {
        let frame = Frame::new(VILNIUS.0, VILNIUS.1);
        Self {
            s: Scheduler::new(frame, false, dp()),
            w: FakeWorker::new(),
            mem: None,
            g: GodotSim::default(),
            l: layer(),
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
            prev_active: HashSet::new(),
            prev_visible: HashSet::new(),
            fresh: Vec::new(),
            refine_fades: 0,
            coarsen_fades: 0,
            deliveries: Vec::new(),
            delivered_split: HashSet::new(),
            first_fill_swaps: 0,
            check_order: true,
            max_fades_seen: 0,
            capped_seen: 0,
            lm_held: HashSet::new(),
            lm_delivered: 0,
            lm_unloaded: 0,
        }
    }

    fn root_of(&self, mut k: TileKey) -> TileKey {
        while let Some(p) = self.l.parent(k) {
            k = p;
        }
        k
    }

    fn fade(&self, k: TileKey) -> Option<f32> {
        self.fades.iter().rev().find(|f| f.0 == k).map(|f| f.1)
    }

    pub(super) fn frame(&mut self, cam: [f64; 3], vel: [f64; 3]) -> (Vec<TileKey>, Vec<TileKey>) {
        self.now += Duration::from_secs_f64(DT);
        self.prev_fades = self.g.fades.clone();
        self.prev_active = self.s.fades().flat_map(|(k, _)| std::iter::once(k).chain(children(k))).collect();
        self.prev_visible = self.g.uploaded.iter().copied().filter(|&k| self.g.visible(k)).collect();
        self.fresh.clear();
        self.w.step(&mut self.s, self.now);
        if let Some(m) = &mut self.mem {
            self.s.sync_memory(m);
        }
        self.s.update(cam, vel, self.now, &mut self.jobs);
        super::boxes::tests::answer_probes(&mut self.s);
        if let Some(m) = &mut self.mem {
            self.s.sync_memory(m);
        }
        self.w.submit(&mut self.jobs);
        for e in self.s.entries() {
            self.ever_requested.insert(e.key);
        }
        let mut got = Vec::new();
        let mut re = 0;
        for _ in 0..2 {
            // Nearest deliverable fresh tile waiting right now (group distance, key).
            let nearest = self.check_order.then(|| {
                let s = &self.s;
                s.entries
                    .values()
                    .filter(|e| matches!(e.state, TileState::Ready(_)) && !e.held && !e.frozen && s.deliverable(e.key))
                    .map(|e| (s.order_key(e).0, e.key))
                    .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)))
            });
            let Some(t) = self.s.pop_ready(re < 1) else { break };
            if t.layer == LayerKind::Landmarks {
                // Additive layer: one fresh delivery per key while held, ring 0, never faded.
                assert!(!t.redelivery && t.ring == 0, "landmark tile {} re-delivered", t.key);
                assert!(self.lm_held.insert(t.key), "landmark tile {} delivered twice while held", t.key);
                assert!(!t.mesh.is_empty() && t.ground.is_empty(), "landmark tile {}: mesh only", t.key);
                self.lm_delivered += 1;
                continue;
            }
            if let Some(Some((d, k))) = nearest {
                if !t.redelivery {
                    let got = self.s.camera_distance(t.key);
                    assert!(got <= d + 1e-6, "{} at {got:.0} m delivered before the nearer {k} at {d:.0} m", t.key);
                }
            }
            if t.redelivery {
                re += 1;
                self.redelivered_total += 1;
                assert!(self.g.uploaded.contains(&t.key), "re-delivery of a tile Godot does not hold");
                if t.layer == LayerKind::World && self.w.real.is_none() {
                    // A re-delivery replaces the whole tile: it must carry the ground again (R3 from the kept mesh).
                    assert_eq!(t.ground.vertex_count(), 3 + t.ring, "re-delivery of {} lost its ground", t.key);
                } else if t.layer == LayerKind::Terrain {
                    assert!(t.terrain.as_ref().is_some_and(|m| !m.mesh.is_empty()), "terrain re-delivery of {} without a mesh", t.key);
                }
            } else {
                assert!(!self.g.uploaded.contains(&t.key), "fresh delivery of an uploaded tile {}", t.key);
                self.fresh.push(t.key);
                let e = &self.s.entries[&t.key];
                if e.split && !e.coarsening {
                    self.delivered_split.insert(t.key);
                } else {
                    self.delivered_split.remove(&t.key);
                }
            }
            assert_eq!(t.layer, self.l.kind);
            assert_eq!(t.terrain.is_some(), t.layer == LayerKind::Terrain);
            self.delivered_total += 1;
            self.ever_delivered.insert(t.key);
            self.deliveries.push((t.key, t.redelivery));
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
        let mut lu = Vec::new();
        self.s.drain_landmark_unload(&mut lu);
        for k in lu {
            assert!(self.lm_held.remove(&k), "unload of a landmark tile the host does not hold: {k}");
            self.lm_unloaded += 1;
        }
        for (k, _) in &self.fades {
            assert!(self.l.ring_for_zoom(k.z).is_some(), "{k}: a landmark key in poll_fades");
        }
        (got, un)
    }

    /// Contract checks that must hold on every frame.
    pub(super) fn check_frame(&mut self, un: &[TileKey], ctx: &str) {
        let errs = self.g.check();
        assert!(errs.is_empty(), "{ctx}: {:?}", &errs[..errs.len().min(5)]);
        // (1) Sibling groups: every tracked non-root tile has its 3 siblings tracked.
        for e in self.s.entries() {
            if e.key.z > self.l.root_zoom {
                for sib in self.l.siblings(e.key) {
                    assert!(self.s.entries.contains_key(&sib), "{ctx}: {} tracked without sibling {sib}", e.key);
                }
            }
        }
        // (3) A tile unloaded while tiles below it stay uploaded had all 4 children present.
        // (4) A sibling group unloaded on its own (coarsening) leaves its parent uploaded.
        let unl: HashSet<TileKey> = un.iter().copied().collect();
        for &k in un {
            let below = self.g.uploaded.iter().any(|&u| {
                let mut a = self.l.parent(u);
                while let Some(p) = a {
                    if p == k {
                        return true;
                    }
                    a = self.l.parent(p);
                }
                false
            });
            if below {
                assert!(children(k).iter().all(|&c| self.g.present(c)), "{ctx}: {k} unloaded before its children were present");
                self.parent_drops += 1;
            }
            if let Some(p) = self.l.parent(k) {
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
        self.first_fill_swaps += un.iter().filter(|&&k| self.first_fill_swap(k, &unl)).count();
        self.check_fades(un, ctx);
        self.max_fades_seen = self.max_fades_seen.max(self.s.active_fades());
        self.capped_seen += self.s.capped_swaps().len();
    }

    /// Fade contract checks for this frame.
    fn check_fades(&self, un: &[TileKey], ctx: &str) {
        let unl: HashSet<TileKey> = un.iter().copied().collect();
        for &k in un {
            let subtree_gone = !self.g.present(self.root_of(k));
            if subtree_gone {
                continue; // a root left the 25 km disk: whole subtree, no fade
            }
            // No unload before completion: a fading tile leaves only with its final 0.0.
            if self.prev_active.contains(&k) {
                assert_eq!(self.fade(k), Some(0.0), "{ctx}: {k} unloaded while fading ({:?})", self.prev_fades.get(&k));
            }
            // No pop-out: a tile visible last frame leaves only by fading out, except a
            // first-fill stand-in (delivered while already split) whose 4 children were all
            // delivered: it swaps out at once, without a fade.
            if self.prev_visible.contains(&k) && self.first_fill_swap(k, &unl) {
                continue;
            }
            // A swap past `fades.max_active` (refine: k is the coarse tile; coarsen: its parent).
            if self.capped_swap(k) {
                continue;
            }
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
        // No pop-in: a fresh non-root tile visible this frame has a fade entry, starting at 0
        // (except under a first-fill stand-in swapped out this frame).
        for &k in &self.fresh {
            if self.l.parent(k).is_some_and(|p| unl.contains(&p) && self.first_fill_swap(p, &unl)) {
                assert!(self.g.fades.get(&k).is_none(), "{ctx}: first-fill {k} has a fade entry");
                continue;
            }
            if self.capped_swap(k) {
                assert!(self.g.fades.get(&k).is_none(), "{ctx}: capped swap {k} has a fade entry");
                continue;
            }
            if k.z > self.l.root_zoom && self.g.visible(k) {
                let f = self.g.fades.get(&k);
                assert!(f.is_some(), "{ctx}: fresh {k} visible without a fade entry");
                if !self.prev_fades.contains_key(&k) {
                    assert_eq!(f, Some(&0.0), "{ctx}: fresh {k} starts at {f:?}");
                }
            }
        }
        // At most `fades.max_active` swaps fade at once.
        let max = self.s.max_fades;
        assert!(self.s.active_fades() <= max, "{ctx}: {} fades active, cap {max}", self.s.active_fades());
        // Each tile in at most one fade, fades exactly one zoom level deep.
        let coarse: HashSet<TileKey> = self.s.fades().map(|(k, _)| k).collect();
        for &k in &coarse {
            assert!(self.l.parent(k).is_none_or(|p| !coarse.contains(&p)), "{ctx}: nested fades at {k}");
            for c in children(k) {
                assert!(!coarse.contains(&c), "{ctx}: nested fades at {c}");
            }
        }
    }

    /// `k` took part in a swap that found `fades.max_active` reached this frame: the coarse
    /// tile itself or one of its children.
    fn capped_swap(&self, k: TileKey) -> bool {
        let c = self.s.capped_swaps();
        c.contains(&k) || self.l.parent(k).is_some_and(|p| c.contains(&p))
    }

    /// `k` was unloaded this frame as a first-fill stand-in: delivered while already split,
    /// not fading, and all 4 children uploaded and visible now (no hole).
    fn first_fill_swap(&self, k: TileKey, unl: &HashSet<TileKey>) -> bool {
        unl.contains(&k)
            && self.delivered_split.contains(&k)
            && !self.prev_active.contains(&k)
            && k.z < self.l.leaf_zoom
            && children(k).iter().all(|c| self.g.uploaded.contains(c) && self.g.visible(*c))
    }

    /// `p` is the root of a subtree that left the cover entirely in this batch (an
    /// ancestor unloaded too, or `p` is a root that is no longer present at all).
    fn dropped_parent(&self, p: TileKey, unl: &HashSet<TileKey>) -> bool {
        let mut a = Some(p);
        while let Some(x) = a {
            if unl.contains(&x) {
                return true;
            }
            a = self.l.parent(x);
        }
        !self.g.present(self.root_of(p))
    }

    pub(super) fn settle(&mut self, cam: [f64; 3], max_frames: usize) -> usize {
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

// ---- Terrain layer (api 1.5) ---------------------------------------------------------

pub(super) fn terrain_setup(f: impl FnOnce(&mut crate::profile::TerrainProfile)) -> (Arc<Profile>, Terrain) {
    let mut w = crate::profile::WorldProfile::default();
    w.terrain.enabled = true;
    f(&mut w.terrain);
    let p = Profile::new(w).unwrap();
    let t = Terrain::from_profile(&p, VILNIUS.0, VILNIUS.1);
    (Arc::new(p), t)
}

pub(super) fn terrain_harness(p: Arc<Profile>, t: Terrain) -> Harness {
    let mut h = Harness::new();
    h.s = Scheduler::new_terrain(Frame::new(VILNIUS.0, VILNIUS.1), p, t);
    h.l = *h.s.layer();
    h.g = GodotSim::for_zooms(h.l.root_zoom, h.l.leaf_zoom);
    h
}

fn held_per_ring(h: &Harness) -> Vec<usize> {
    let mut v = vec![0; h.l.rings()];
    for e in h.s.entries().filter(|e| e.held) {
        v[e.ring] += 1;
    }
    v
}

// ---- Terrain SSE cover (Terrain v2 G2) ------------------------------------------------------

pub(super) fn seed_terrain_harness(lat: f64, lon: f64, f: impl FnOnce(&mut crate::profile::WorldProfile)) -> Harness {
    let (p, t, _) = crate::seed::world::test_world();
    let mut w = p.world().clone();
    f(&mut w);
    let p = Arc::new(Profile::new(w).unwrap());
    let mut h = Harness::new();
    h.s = Scheduler::new_terrain(Frame::new(lat, lon), p, t.clone());
    h.l = *h.s.layer();
    h.g = GodotSim::for_zooms(h.l.root_zoom, h.l.leaf_zoom);
    h
}
