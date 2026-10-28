use super::sim::GodotSim;
use super::*;
use bytes::Bytes;
use crate::fetch::{FetchError, Source};
use crate::lights::LightSet;
use crate::terrain::tin::TerrainTile;
use crate::worker::TileData;
use crate::rings::{parent, ring_for_zoom, siblings, BandParams, LEAF_ZOOM, RING_COUNT, ROOT_ZOOM};
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
fn layer() -> Layer {
    Layer::world(&bands(), 4, 2)
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
            ..LightSet::default()
        }
    } else {
        LightSet::default()
    };
    TileData {
        building_count: 0,
        lights: Arc::new(lights),
        mesh: MeshData::default(),
        // One triangle, a different count per band, so re-deliveries can be checked.
        ground: MeshData {
            positions: vec![[0.0; 3]; 3 + ring],
            indices: vec![0, 1, 2],
            ..MeshData::default()
        },
        max_height: fake_height(k),
        cpu_us: 0,
        terrain: None,
    }
}

/// Stand-in terrain tile: 2 triangles, `4 + ring` vertices (re-deliveries can be checked).
fn fake_terrain(ring: usize) -> TileData {
    let mesh = MeshData { positions: vec![[0.0; 3]; 4 + ring], indices: vec![0, 1, 2, 0, 2, 3], ..MeshData::default() };
    TileData {
        terrain: Some(Box::new(TerrainTile { mesh, surface_triangles: 2, ..TerrainTile::default() })),
        ..TileData::default()
    }
}

/// Worker stand-in: jobs finish after a fixed number of frames (fetch 3, decode 2).
struct FakeWorker {
    pending: VecDeque<(u64, WorkerMsg)>,
    frame: u64,
    real: Option<Frame>,
    /// Profile of the real builds (`real`); None = the test default.
    profile: Option<Arc<Profile>>,
    /// Terrain jobs: real TINs on this terrain (with `real` and `profile`), else stand-ins.
    terrain: Option<Terrain>,
    /// Data tiles (Tiles v2): the archive's bytes per data tile key (absent = empty tile).
    data_src: AHashMap<TileKey, Bytes>,
    /// Decoded data tiles by decode job (decoded at submit, so a dependent chunk job finds it).
    data_out: AHashMap<u64, Arc<crate::data::DataTile>>,
    /// Data jobs seen: fetches, decodes.
    data_jobs: (u32, u32),
    /// Zoom of every per-tile `Fetch`.
    fetch_zooms: Vec<u8>,
    /// Last building mesh built per chunk cut from a data tile.
    chunk_meshes: std::collections::BTreeMap<TileKey, MeshData>,
    /// Whether each chunk job folded landmarks in (T2b).
    chunk_lm: std::collections::BTreeMap<TileKey, bool>,
    /// Data tiles are generated (the synthetic city, T2c) instead of decoded.
    synthetic_data: bool,
}

impl FakeWorker {
    fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            frame: 0,
            real: None,
            profile: None,
            terrain: None,
            data_src: AHashMap::new(),
            data_out: AHashMap::new(),
            data_jobs: (0, 0),
            fetch_zooms: Vec::new(),
            chunk_meshes: Default::default(),
            chunk_lm: Default::default(),
            synthetic_data: false,
        }
    }
    fn submit(&mut self, jobs: &mut Vec<Job>) {
        for j in jobs.drain(..) {
            let (delay, msg) = match j {
                Job::Fetch { key, ring, job } => {
                    self.fetch_zooms.push(key.z);
                    (3, WorkerMsg::Fetched { key, ring, job, result: Ok((Bytes::new(), Source::Network)) })
                }
                Job::Decode { key, ring, job, payload } => {
                    let result = match self.real {
                        Some(f) => build_tile(key, ring, f, &payload, self.profile.as_deref().unwrap_or(crate::profile::test_default())),
                        None => Ok(fake_data(key, ring)),
                    };
                    (2, WorkerMsg::Decoded { key, ring, job, result })
                }
                Job::FetchData { key, job } => {
                    self.data_jobs.0 += 1;
                    let bytes = self.data_src.get(&key).cloned().unwrap_or_default();
                    (3, WorkerMsg::DataFetched { key, job, result: Ok((bytes, Source::Network)) })
                }
                Job::DecodeData { key, job, bytes, level } => {
                    self.data_jobs.1 += 1;
                    let f = self.real.unwrap_or(Frame::new(VILNIUS.0, VILNIUS.1));
                    let p = self.profile.clone().unwrap_or_else(dp);
                    let result = if self.synthetic_data {
                        Ok(crate::data::synthetic_data_tile(key, level, f, &Terrain::flat(), &p))
                    } else {
                        crate::data::decode_level(key, level, f, &Terrain::flat(), &bytes, &p)
                    }
                    .map(Arc::new);
                    if let Ok(d) = &result {
                        self.data_out.insert(job, d.clone());
                    }
                    (2, WorkerMsg::DataDecoded { key, job, result })
                }
                Job::MeshChunk { key, ring, job, data, lm } => {
                    let get = |r: crate::worker::DataRef| match r {
                        crate::worker::DataRef::Ready(d) => Some(d),
                        crate::worker::DataRef::After(j) => self.data_out.get(&j).cloned(),
                    };
                    let d = get(data);
                    let (same, other, missing) = match lm {
                        crate::worker::LmRef::None => (false, None, false),
                        crate::worker::LmRef::Same => (true, None, false),
                        crate::worker::LmRef::Other(r) => {
                            let o = get(r);
                            let m = o.is_none();
                            (false, o, m)
                        }
                    };
                    let p = self.profile.clone().unwrap_or_else(dp);
                    match d.filter(|_| !missing) {
                        Some(d) => {
                            self.chunk_lm.insert(key, same || other.is_some());
                            let lmt: Option<&crate::data::DataTile> = if same { Some(&d) } else { other.as_deref() };
                            let result = crate::data::build_chunk_with(&d, lmt, key, ring, &p, &|| false).map(|o| o.expect("not stopped"));
                            if let Ok(t) = &result {
                                self.chunk_meshes.insert(key, t.mesh.clone());
                            }
                            (2, WorkerMsg::Decoded { key, ring, job, result })
                        }
                        None => (1, WorkerMsg::Dropped { layer: LayerKind::World, key, ring, job }),
                    }
                }
                Job::MeshLandmarks { key, job, data } => {
                    let d = match data {
                        crate::worker::DataRef::Ready(d) => Some(d),
                        crate::worker::DataRef::After(j) => self.data_out.get(&j).cloned(),
                    };
                    let p = self.profile.clone().unwrap_or_else(dp);
                    match d {
                        Some(d) => {
                            let result = crate::data::build_landmark_tile(&d, &p, &|| false).map(|o| o.expect("not stopped"));
                            (2, WorkerMsg::LandmarksBuilt { key, job, result })
                        }
                        None => (1, WorkerMsg::Dropped { layer: LayerKind::Landmarks, key, ring: 0, job }),
                    }
                }
                Job::Terrain { key, ring, job, detail } => {
                    let result = match (self.real, &self.profile, &self.terrain) {
                        (Some(f), Some(p), Some(t)) => crate::worker::build_terrain_data(key, ring, f, t, p, detail, None),
                        _ => Ok(fake_terrain(ring)),
                    };
                    (2, WorkerMsg::TerrainBuilt { key, ring, job, result })
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
    fn new() -> Self {
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

    fn frame(&mut self, cam: [f64; 3], vel: [f64; 3]) -> (Vec<TileKey>, Vec<TileKey>) {
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
    fn check_frame(&mut self, un: &[TileKey], ctx: &str) {
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
    // Altitudes right at an edge for the tile below (fake heights 20..110 m).
    for alt in [650.0, 2560.0, 7050.0] {
        hold_at_edge_without_flip_flop(Harness::new, alt);
    }
}

/// api 1.10: the same hold with a 30 deg view: the SSE edges move out by tan 30 / tan 15 and the
/// hysteresis keeps holding the cover steady at the moved edges.
#[test]
fn holding_at_sse_edges_of_a_narrow_view_does_not_flip_flop() {
    let view = crate::rings::LodView::new(30.0, 1080.0, [0.0, 0.0, -1.0]).unwrap();
    let edges = bands().edges_for(&view, 1.0);
    let narrow = || {
        let mut h = Harness::new();
        h.s.set_lod_view(&view, 1.0);
        h
    };
    for edge in &edges[..3] {
        hold_at_edge_without_flip_flop(narrow, edge + 50.0);
    }
}

/// Circle 60 m around a point at `alt` with +-20 m altitude jitter: after one settle lap no tile
/// enters and leaves the cover, and the hold exercises the hysteresis band.
fn hold_at_edge_without_flip_flop(make: impl Fn() -> Harness, alt: f64) {
    {
        let mut h = make();
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
        let edge_m = h.s.layer().edge_m;
        for _ in 0..3 * lap {
            t += dt;
            let (_, un) = h.frame(pos(t), [0.0; 3]);
            h.check_frame(&un, "hold");
            let views = h.s.views(pos(t), [0.0; 3]);
            for e in h.s.entries().filter(|e| e.split) {
                let d = views[e.ring - 1].slant_m(e.key, e.height.map_or(bands().default_tile_height_m, |x| x as f64));
                if d >= edge_m[e.ring - 1] {
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

/// api 1.10 equivalence: the SSE cover at the default view (1080 px, 60 deg, tau 4) delivers and
/// unloads exactly what the fixed 600 / 2500 / 7000 / 25000 m edges did, frame by frame, on the
/// settle + climb / descent / 250 m/s flight.
#[test]
fn sse_cover_at_the_default_view_equals_the_fixed_edges() {
    let mut a = Harness::new();
    a.s.set_lod_view(&crate::rings::LodView::default(), 1.0);
    let mut b = Harness::new();
    b.s.layer.edge_m[..RING_COUNT].copy_from_slice(&[600.0, 2500.0, 7000.0, 25_000.0]);
    let dt = 1.0 / 30.0;
    let mut cam = [0.0, 0.0, 300.0];
    let steps: [([f64; 3], usize); 6] = [
        ([0.0, 0.0, 0.0], 600),
        ([0.0, 0.0, 60.0], (7700.0 / 60.0 / dt) as usize),
        ([0.0, 0.0, -60.0], (7700.0 / 60.0 / dt) as usize),
        ([250.0, 0.0, 0.0], (40.0 / dt) as usize),
        ([-120.0, 90.0, 5.0], (30.0 / dt) as usize),
        ([0.0, 0.0, 0.0], 300),
    ];
    let mut frames = 0usize;
    for (vel, n) in steps {
        for _ in 0..n {
            for i in 0..3 {
                cam[i] += vel[i] * dt;
            }
            let (ga, ua) = a.frame(cam, vel);
            let (gb, ub) = b.frame(cam, vel);
            assert_eq!(ga, gb, "frame {frames}: deliveries differ");
            let sorted = |mut v: Vec<TileKey>| {
                v.sort();
                v
            };
            assert_eq!(sorted(ua), sorted(ub), "frame {frames}: unloads differ");
            let fades = |h: &Harness| {
                let mut f: Vec<(TileKey, u32)> = h.fades.iter().map(|&(k, x)| (k, x.to_bits())).collect();
                f.sort();
                f
            };
            assert_eq!(fades(&a), fades(&b), "frame {frames}: fades differ");
            frames += 1;
        }
    }
    assert_eq!(a.deliveries, b.deliveries);
    assert!(a.delivered_total > 1000, "the flight exercised the cover ({} deliveries)", a.delivered_total);
}

/// api 1.10: the view moves the cover. At 1000 m over the city the default view has no R0 under
/// the camera (R0 edge 600 m); a 30 deg view (R0 edge ~1293 m) has; quality 2 (R0 edge 300 m)
/// at 400 m has none where the default has R0.
#[test]
fn set_lod_view_moves_the_settled_cover() {
    let under_ring = |h: &mut Harness, cam: [f64; 3]| {
        h.settle(cam, 20_000);
        let v = h.view(cam);
        let under = h.g.tile_at(v.wx, v.wy);
        assert_eq!(under.len(), 1);
        ring_for_zoom(under[0].z).unwrap()
    };
    let narrow = crate::rings::LodView::new(30.0, 1080.0, [0.0, 0.0, -1.0]).unwrap();
    let mut h = Harness::new();
    assert_eq!(under_ring(&mut h, [0.0, 0.0, 1000.0]), 1);
    h.s.set_lod_view(&narrow, 1.0);
    assert_eq!(under_ring(&mut h, [0.0, 0.0, 1000.0]), 0, "fov 30: R0 reaches past 1000 m");
    let mut h = Harness::new();
    assert_eq!(under_ring(&mut h, [0.0, 0.0, 400.0]), 0);
    h.s.set_lod_view(&crate::rings::LodView::default(), 2.0);
    assert_eq!(under_ring(&mut h, [0.0, 0.0, 400.0]), 1, "quality 2: R0 edge 300 m");
    // Back to the default: the edges are exactly the profile's again.
    h.s.set_lod_view(&crate::rings::LodView::default(), 1.0);
    assert_eq!(h.s.layer().edge_m[..RING_COUNT], bands().edge_m);
    // The terrain layer keeps its fixed bands.
    let (p, t) = terrain_setup(|_| {});
    let mut ts = Scheduler::new_terrain(Frame::new(VILNIUS.0, VILNIUS.1), p, t);
    let before = ts.layer().edge_m;
    ts.set_lod_view(&narrow, 2.0);
    assert_eq!(ts.layer().edge_m, before);
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
    let lights = synthetic_city_settles(dp());
    assert!(lights[0] == 0 && lights[1] > 0 && lights[2] > 0 && lights[3] > 0, "{lights:?}");
}

/// `lights.min_ring` = 2 (the old default, profile_version 3): R2/R3 only.
#[test]
fn lights_min_ring_2_lights_r2_r3_only() {
    let mut w = crate::profile::WorldProfile::default();
    w.lights.min_ring = 2;
    let lights = synthetic_city_settles(Arc::new(Profile::new(w).unwrap()));
    assert!(lights[0] == 0 && lights[1] == 0 && lights[2] > 0 && lights[3] > 0, "{lights:?}");
}

/// `lights.min_ring` = 0: R0 tiles carry lights through the whole pipeline too, and a settled cover has no
/// cover errors (the lights do not touch the visibility rule).
#[test]
fn lights_min_ring_0_lights_r0_through_the_scheduler() {
    let mut w = crate::profile::WorldProfile::default();
    w.lights.min_ring = 0;
    let lights = synthetic_city_settles(Arc::new(Profile::new(w).unwrap()));
    assert!(lights[0] > 0 && lights[1] > 0 && lights[2] > 0 && lights[3] > 0, "{lights:?}");
}

/// Streams the synthetic city around 300 m AGL until settled; returns the light counts per ring.
fn synthetic_city_settles(profile: Arc<Profile>) -> [usize; RING_COUNT] {
    let mut h = Harness::new();
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    h.s = Scheduler::new(frame, true, profile.clone());
    h.w.real = Some(frame);
    h.w.profile = Some(profile);
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
    // Every band has ground since api 1.3 (R3: the coarse `ground.r3`).
    assert!(ground.iter().all(|&g| g > 0));
    lights
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

/// Step J1: mesh jobs are cancelled like fetches. A removed tile's running mesh job is reported
/// by `drain_cancelled`; its `Dropped` reply frees the slot without a failure; a late result of
/// the stale generation is ignored. Re-deliveries queue as prefetch, fresh tiles on demand.
#[test]
fn orphaned_mesh_jobs_are_cancelled_and_free_their_slots() {
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let mut s = Scheduler::new(frame, true, dp());
    let t0 = Instant::now();
    let mut jobs = Vec::new();
    s.update([0.0, 0.0, 8000.0], [0.0; 3], t0, &mut jobs);
    let decodes: Vec<(TileKey, usize, u64)> = jobs
        .drain(..)
        .map(|j| match j {
            Job::Decode { key, ring, job, .. } => (key, ring, job),
            _ => panic!("decode expected (synthetic)"),
        })
        .collect();
    assert!(!decodes.is_empty());
    for d in &decodes {
        let p = s.job_priority(d.0);
        assert_eq!(p.urgency, crate::jobs::Urgency::OnDemand, "fresh tiles are on demand");
    }
    let mut ids = Vec::new();
    s.drain_cancelled(&mut ids);
    assert!(ids.is_empty(), "nothing orphaned while the cover stands");
    let far = [200_000.0, 0.0, 8000.0];
    s.update(far, [0.0; 3], t0, &mut jobs);
    s.drain_cancelled(&mut ids);
    let mut want: Vec<u64> = decodes.iter().map(|d| d.2).collect();
    want.sort_unstable();
    ids.sort_unstable();
    assert_eq!(ids, want, "every running mesh job of a removed tile is reported");
    let (k0, r0, j0) = decodes[0];
    for &(key, ring, job) in &decodes[1..] {
        assert!(s.handle(WorkerMsg::Dropped { layer: LayerKind::World, key, ring, job }, t0).is_none());
    }
    // A result that slipped through before the cancel took effect is dropped by its id.
    assert!(s.handle(WorkerMsg::Decoded { key: k0, ring: r0, job: j0, result: Ok(fake_data(k0, r0)) }, t0).is_none());
    assert_eq!(s.inflight().1, [0; MAX_RINGS], "every slot is free again");
    assert!(s.pop_ready(true).is_none(), "nothing stale is ready");
    assert_eq!(s.stats().failed.iter().sum::<i32>(), 0);
}

#[test]
fn orphaned_fetches_are_cancelled_and_free_their_slots() {
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let mut s = Scheduler::new(frame, false, dp());
    let t0 = Instant::now();
    let mut jobs = Vec::new();
    s.update([0.0, 0.0, 8000.0], [0.0; 3], t0, &mut jobs);
    let fetches: Vec<(TileKey, usize, u64)> = jobs
        .drain(..)
        .map(|j| match j {
            Job::Fetch { key, ring, job } => (key, ring, job),
            _ => panic!("fetch expected"),
        })
        .collect();
    assert_eq!(fetches.len(), 4, "R3 roots at 8 km fill the band's fetch slots");
    let mut ids = Vec::new();
    s.drain_cancelled(&mut ids);
    assert!(ids.is_empty(), "nothing orphaned while the cover stands");
    // Teleport 200 km east: every root leaves the disk while its fetch runs.
    let far = [200_000.0, 0.0, 8000.0];
    s.update(far, [0.0; 3], t0, &mut jobs);
    s.drain_cancelled(&mut ids);
    let mut want: Vec<u64> = fetches.iter().map(|f| f.2).collect();
    want.sort_unstable();
    ids.sort_unstable();
    assert_eq!(ids, want, "every running fetch of a removed tile is reported");
    assert!(jobs.is_empty(), "the slots stay taken until the replies arrive");
    // The cancelled replies free the slots without a failure or a warning.
    for (key, ring, job) in fetches {
        let result = Err(FetchError::new(FetchErrorKind::Cancelled, "cancelled"));
        assert!(s.handle(WorkerMsg::Fetched { key, ring, job, result }, t0).is_none());
    }
    assert_eq!(s.inflight().0[3], 0);
    s.update(far, [0.0; 3], t0, &mut jobs);
    assert_eq!(jobs.len(), 4, "the new area gets the freed slots");
    assert_eq!(s.stats().failed[3], 0);
    ids.clear();
    s.drain_cancelled(&mut ids);
    assert!(ids.is_empty(), "nothing new orphaned");
}

#[test]
fn repeated_decode_failures_park_the_tile() {
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let mut s = Scheduler::new(frame, true, dp());
    let cam = [0.0, 0.0, 8000.0];
    let mut t = Instant::now();
    let mut jobs = Vec::new();
    s.update(cam, [0.0; 3], t, &mut jobs);
    // The nearest root goes first, so it also wins the free slot on every retry.
    let Job::Decode { key, ring, mut job, .. } = jobs.remove(0) else { panic!("decode expected") };
    jobs.clear();
    let decode_of = |jobs: &[Job]| {
        jobs.iter().find_map(|j| match j {
            Job::Decode { key: k, job, .. } if *k == key => Some(*job),
            _ => None,
        })
    };
    for attempt in 1..=DECODE_ATTEMPTS {
        let w = s.handle(WorkerMsg::Decoded { key, ring, job, result: Err("bad tile".into()) }, t).unwrap();
        if attempt < DECODE_ATTEMPTS {
            assert!(w.contains("retry in"), "{w}");
            t += Duration::from_secs(60);
            s.update(cam, [0.0; 3], t, &mut jobs);
            job = decode_of(&jobs).expect("retried after the backoff");
            jobs.clear();
        } else {
            assert!(w.contains("giving up"), "{w}");
        }
    }
    // Parked: counted as failed, never dispatched again while it stays in the cover.
    for _ in 0..3 {
        t += Duration::from_secs(60);
        s.update(cam, [0.0; 3], t, &mut jobs);
        assert!(decode_of(&jobs).is_none(), "a broken tile gets no more jobs");
        jobs.clear();
    }
    assert_eq!(s.stats().failed[ring], 1);
    assert!(matches!(s.entries.get(&key).map(|e| &e.state), Some(TileState::Broken)));
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
    let gap = fade_gap_m(&layer(), 1); // 90 m (R0 edge 600 m)
    assert!((gap - 90.0).abs() < 1e-9);
    assert!((fade_gap_m(&layer(), 3) - 1050.0).abs() < 1e-9);
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
    // Descend at 180 m/s (6 m per frame) to 300 m: fast enough that the distance-driven
    // fade completes well inside the safety cap (fades.cap_s, 0.75 s by default).
    let obs = fly(&mut h, p, 1000.0, 300.0, 6.0);
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
    // Driven by distance (~90 m gap at 180 m/s = ~0.5 s), so before the cap fires.
    let frames = e - s;
    let cap_frames = (fade_cap().as_secs_f64() / DT).round() as usize;
    assert!((5..cap_frames).contains(&frames), "refine fade took {frames} frames (cap {cap_frames})");
    eprintln!("refine fade of {p}: {frames} frames");
    // Latency `swap` is sampled when the refine fade starts (4 children at 2 per frame: at
    // least one frame after the split), not in the frame the split is decided (0 ms).
    let sw = h.s.latency().summary(Stage::Swap, 1);
    assert!(sw.n >= 1 && sw.p50 >= (DT * 1000.0) as f32 - 0.1, "swap R1 p50 {} ms (n {})", sw.p50, sw.n);
}

#[test]
fn fade_without_anchor_anchors_at_the_first_distance() {
    let t0 = Instant::now();
    let gap = fade_gap_m(&layer(), 1);
    for refine in [true, false] {
        let mut f = Fade::new(refine, f64::NAN, t0);
        let start = f.t;
        assert!(!f.advance(500.0, gap, t0, fade_cap()), "a NaN anchor must not complete the fade");
        assert_eq!(f.t, start);
        let d = if refine { 500.0 - gap / 2.0 } else { 500.0 + gap / 2.0 };
        assert!(!f.advance(d, gap, t0, fade_cap()));
        assert!((f.t - 0.5).abs() < 1e-12);
    }
}

#[test]
fn coarsen_fade_redelivers_parent_then_unloads_children() {
    let mut h = Harness::new();
    h.settle([0.0, 0.0, 300.0], 20_000);
    let p = r1_under(&h, [0.0, 0.0, 300.0]);
    assert!(!h.g.uploaded.contains(&p) && children(p).iter().all(|c| h.g.visible(*c)));
    // Climb at 180 m/s so the distance-driven fade completes inside the safety cap.
    let obs = fly(&mut h, p, 300.0, 1000.0, 6.0);
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
    let cap_frames = (fade_cap().as_secs_f64() / DT).round() as usize;
    assert!((5..cap_frames).contains(&frames), "coarsen fade took {frames} frames (cap {cap_frames})");
    eprintln!("coarsen fade of {p}: {frames} frames");
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
    // A cold start at 450 m is a first fill: R3 -> R0 with no fade at all (each stand-in
    // swaps out as soon as its 4 children are in).
    let mut h = Harness::new();
    let cam = [0.0, 0.0, 450.0];
    for _ in 0..20_000 {
        let (_, un) = h.frame(cam, [0.0; 3]);
        h.check_frame(&un, "cold start");
        assert_eq!(h.s.fades().count(), 0, "a first fill has no fades");
        if h.s.settled() {
            break;
        }
    }
    assert!(h.s.settled() && h.first_fill_swaps > 0 && h.refine_fades == 0);
    // Settled at 3000 m (leaves at every band), then a jump down to 450 m and hovering:
    // the leaves that now split fade (detail change), each sitting at 0 until the cap.
    let mut h = Harness::new();
    h.settle([0.0, 0.0, 3000.0], 20_000);
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

// ---- Terrain layer (api 1.5) ---------------------------------------------------------

fn terrain_setup(f: impl FnOnce(&mut crate::profile::TerrainProfile)) -> (Arc<Profile>, Terrain) {
    let mut w = crate::profile::WorldProfile::default();
    w.terrain.enabled = true;
    f(&mut w.terrain);
    let p = Profile::new(w).unwrap();
    let t = Terrain::from_profile(&p, VILNIUS.0, VILNIUS.1);
    (Arc::new(p), t)
}

fn terrain_harness(p: Arc<Profile>, t: Terrain) -> Harness {
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

#[test]
fn terrain_layer_shape() {
    let (p, t) = terrain_setup(|_| {});
    let s = Scheduler::new_terrain(Frame::new(VILNIUS.0, VILNIUS.1), p.clone(), t);
    let l = s.layer();
    assert_eq!((l.kind, l.root_zoom, l.leaf_zoom, l.rings()), (LayerKind::Terrain, 10, 14, 5));
    assert_eq!(l.edge_m[..5], [2500.0, 7000.0, 25_000.0, 60_000.0, 130_000.0]);
    assert_eq!((l.max_fetch, l.max_mesh, l.default_tile_height_m), (4, 2, 0.0));
    assert_eq!(l.horizon_margin_m, Some(2000.0));
    assert_eq!(l.hysteresis, p.bands().hysteresis);
    assert_eq!(l.ring_for_zoom(12), Some(2));
    assert_eq!(l.parent(TileKey::new(10, 5, 5)), None);
    // The vector layer is exactly the old constants.
    let w = Scheduler::new(Frame::new(VILNIUS.0, VILNIUS.1), true, dp());
    assert_eq!((w.layer().root_zoom, w.layer().leaf_zoom, w.layer().rings()), (ROOT_ZOOM, LEAF_ZOOM, RING_COUNT));
    assert_eq!(w.layer().edge_m[..4], bands().edge_m);
    assert_eq!(w.layer().horizon_margin_m, None);
}

/// The contract's sibling-swap, fade and unload guarantees hold on the terrain layer
/// (per-layer `GodotSim` with root zoom 10): settle, climb to 8 km, descend, cruise at
/// 250 m/s, every frame checked.
/// First fill at a static camera, both layers: (1) every fresh delivery is the nearest
/// deliverable ready tile (group distance from the camera, see `order_key`), whatever its
/// ring; (2) no fade at all (each stand-in swaps out when its 4 children are in), so the
/// finest tile under the camera arrives within a few frames of its parent instead of one
/// fade cap per level; (3) the visibility rule holds on every frame and the fill settles
/// with an exact cover.
#[test]
fn first_fill_is_nearest_first_without_fades() {
    let (p, t) = terrain_setup(|_| {});
    for (name, mut h) in [("world", Harness::new()), ("terrain", terrain_harness(p, t.clone()))] {
        let cam = [0.0, 0.0, 300.0];
        let mut under_at: Vec<(u8, usize)> = Vec::new();
        let mut frames = 0;
        h.check_order = true;
        for f in 0..20_000 {
            let (got, un) = h.frame(cam, [0.0; 3]);
            h.check_frame(&un, name);
            assert_eq!(h.s.fades().count(), 0, "{name} f{f}: a first fill has no fades");
            assert!(h.fades.is_empty(), "{name} f{f}: poll_fades lists {:?}", &h.fades[..h.fades.len().min(4)]);
            for k in got {
                if k == crate::geo::tile_for(VILNIUS.0, VILNIUS.1, k.z) {
                    under_at.push((k.z, f));
                }
            }
            frames = f;
            if h.s.settled() {
                break;
            }
        }
        assert!(h.s.settled(), "{name}: did not settle");
        assert!(h.g.check().is_empty());
        assert!(h.first_fill_swaps > 0 && h.refine_fades == 0 && h.coarsen_fades == 0, "{name}");
        let leaf = under_at.iter().find(|u| u.0 == h.l.leaf_zoom).map(|u| u.1).expect("leaf under the camera");
        let root = under_at.iter().find(|u| u.0 == h.l.root_zoom).map(|u| u.1).expect("root under the camera");
        let cap = (fade_cap().as_secs_f64() / DT).round() as usize;
        let levels = (h.l.leaf_zoom - h.l.root_zoom) as usize;
        eprintln!(
            "{name} first fill: root under the camera at frame {root}, leaf at {leaf} ({levels} levels; one fade cap per level = {} frames more), settled at {frames}, {} deliveries",
            levels * cap,
            h.delivered_total
        );
        // FakeWorker: fetch 3 + decode 2 frames per tile, 2 deliveries per frame; the old
        // rule waited a fade cap (0.75 s = 22 frames) per level on top.
        assert!(leaf - root <= levels * 8, "{name}: leaf {leaf} vs root {root}");
    }
}

/// Cover churn, both layers: a static camera never unloads a tile and delivers it again
/// (first fill and 10 s after it: nothing but the stand-ins leaves, nothing comes back, and
/// once settled nothing moves at all); in level flight north at 80 and 250 m/s every key
/// delivered again after an unload is a coarsen swap (guarantee 4: its 4 children are
/// unloaded within the fade cap), never a flip-flop of the cover.
#[test]
fn no_cover_churn_static_or_level_flight() {
    let (p, t) = terrain_setup(|_| {});
    for terrain in [false, true] {
        let mk = || if terrain { terrain_harness(p.clone(), t.clone()) } else { Harness::new() };
        let name = if terrain { "terrain" } else { "world" };
        // Static: first fill, then 10 s more.
        let mut h = mk();
        let cam = [0.0, 0.0, 300.0];
        let mut unloaded: HashSet<TileKey> = HashSet::new();
        let mut settled_at = None;
        for f in 0..(20_000usize) {
            let (got, un) = h.frame(cam, [0.0; 3]);
            h.check_frame(&un, name);
            for k in &got {
                assert!(!unloaded.contains(k), "{name} static f{f}: {k} delivered again after an unload");
            }
            for k in &un {
                // Only first-fill stand-ins leave (their children are uploaded).
                assert!(children(*k).iter().all(|c| h.g.uploaded.contains(c)), "{name} static f{f}: {k} unloaded");
            }
            unloaded.extend(un.iter().copied());
            if settled_at.is_none() && h.s.settled() {
                settled_at = Some(f);
            }
            if let Some(s0) = settled_at {
                if f > s0 {
                    assert!(got.is_empty() && un.is_empty(), "{name} static f{f}: settled cover moved ({got:?} / {un:?})");
                }
                if f >= s0 + (10.0 / DT) as usize {
                    break;
                }
            }
        }
        assert!(settled_at.is_some(), "{name}: static camera never settled");
        // Level flight north.
        for speed in [80.0, 250.0] {
            let mut h = mk();
            let mut cam = [0.0, 0.0, 300.0];
            h.settle(cam, 20_000);
            let cap = (fade_cap().as_secs_f64() / DT).ceil() as usize + 2;
            let mut unloaded_at: AHashMap<TileKey, usize> = AHashMap::new();
            let mut expect: Vec<(TileKey, usize)> = Vec::new();
            let (mut again, mut swaps) = (0, 0);
            let frames = (20.0 / DT) as usize;
            for f in 0..frames {
                cam[1] += speed * DT;
                let (got, un) = h.frame(cam, [0.0, speed, 0.0]);
                h.check_frame(&un, name);
                for k in got {
                    if h.fresh.contains(&k) && unloaded_at.contains_key(&k) {
                        again += 1;
                        expect.push((k, f));
                    }
                }
                for k in un {
                    unloaded_at.insert(k, f);
                }
                expect.retain(|&(k, f0)| {
                    let done = children(k).iter().all(|c| unloaded_at.get(c).is_some_and(|&u| u >= f0));
                    if done {
                        swaps += 1;
                        return false;
                    }
                    assert!(f <= f0 + cap, "{name} {speed} m/s: {k} delivered again at f{f0} without a coarsen swap");
                    true
                });
            }
            eprintln!("{name} level flight {speed} m/s: {again} keys delivered again after an unload, {swaps} of them coarsen swaps");
        }
    }
}

#[test]
fn terrain_layer_keeps_the_sibling_swap_guarantees() {
    let (p, t) = terrain_setup(|_| {});
    let mut h = terrain_harness(p, t.clone());
    let dt = 1.0 / 30.0;
    let mut cam = [0.0, 0.0, 300.0];
    h.settle(cam, 20_000);
    let at300 = held_per_ring(&h);
    let mut camera_holes = 0usize;
    let steps: [([f64; 3], usize, &str); 4] = [
        ([0.0, 0.0, 60.0], (7700.0 / 60.0 / dt) as usize, "climb"),
        ([0.0, 0.0, -60.0], (7700.0 / 60.0 / dt) as usize, "descent"),
        ([0.0, 0.0, 0.0], 300, "hold low"),
        ([250.0, 0.0, 0.0], (60.0 / dt) as usize, "250 m/s"),
    ];
    for (vel, n, name) in steps {
        for _ in 0..n {
            for a in 0..3 {
                cam[a] += vel[a] * dt;
            }
            let (_, un) = h.frame(cam, vel);
            h.check_frame(&un, name);
            let v = h.view(cam);
            let under = h.g.tile_at(v.wx, v.wy);
            let fading_pair = under.len() == 2 && under.iter().all(|k| h.g.fades.contains_key(k));
            if under.len() != 1 && !fading_pair {
                camera_holes += 1;
            }
        }
    }
    h.settle(cam, 20_000);
    assert!(h.g.check().is_empty());
    assert_eq!(h.ever_requested, h.ever_delivered, "every requested terrain tile was delivered");
    assert_eq!(camera_holes, 0, "point under the camera uncovered");
    assert!(h.parent_drops > 0 && h.coarsen_swaps > 0, "refines and coarsens happened");
    assert!(h.refine_fades > 0 && h.coarsen_fades > 0);
    assert_eq!(h.redelivered_total, 0, "no light re-deliveries on the terrain layer");
    eprintln!(
        "terrain flight: held per ring at 300 m {at300:?}, {} deliveries, {} refine / {} coarsen fades",
        h.delivered_total, h.refine_fades, h.coarsen_fades
    );
}

#[test]
fn horizon_line_and_hysteresis() {
    let line = crate::rings::horizon_line_m(300.0, 0.0, 0.0, 0.0, 0.0);
    assert!((line - (2.0 * crate::rings::HORIZON_EARTH_RADIUS_M * 300.0).sqrt()).abs() < 1e-6);
    assert!((line - 61_827.0).abs() < 1.0, "{line}");
    // Just inside / just outside, hysteresis on the way out.
    assert!(crate::rings::above_horizon(line - 10.0, line, 1.15, false));
    assert!(!crate::rings::above_horizon(line + 10.0, line, 1.15, false));
    assert!(crate::rings::above_horizon(line + 10.0, line, 1.15, true));
    assert!(!crate::rings::above_horizon(line * 1.15 + 10.0, line, 1.15, true));
    // A tall tile is seen farther; the reference is the lower of camera ground and tile.
    let hill = crate::rings::horizon_line_m(300.0, 0.0, 0.0, 500.0, 2000.0);
    assert!(hill > line + 2000.0 + 79_000.0);
    let plateau = crate::rings::horizon_line_m(800.0, 500.0, 0.0, 10.0, 0.0);
    assert!((plateau - crate::rings::horizon_line_m(800.0, 0.0, 0.0, 10.0, 0.0)).abs() < 1e-9);
}

/// Horizon cull on the terrain layer: fewer roots than without it, every root above the
/// horizon, the hysteresis keeps roots when the camera sinks a little, groups are culled
/// whole (never a single child).
#[test]
fn horizon_cull_roots_groups_and_hysteresis() {
    // Pinned to the former default relief (160 m): taller relief widens the horizon so far that nothing is culled.
    let (p_on, t) = terrain_setup(|tp| tp.amplitude_m = 160.0);
    let (p_off, _) = terrain_setup(|tp| {
        tp.amplitude_m = 160.0;
        tp.horizon_cull = false;
    });
    let roots = |h: &Harness| -> HashSet<TileKey> { h.s.entries().filter(|e| e.key.z == h.l.root_zoom).map(|e| e.key).collect() };
    let mut on = terrain_harness(p_on.clone(), t.clone());
    on.settle([0.0, 0.0, 300.0], 20_000);
    let mut off = terrain_harness(p_off, t.clone());
    off.settle([0.0, 0.0, 300.0], 20_000);
    let (r_on, r_off) = (roots(&on), roots(&off));
    assert!(r_on.len() < r_off.len() && r_on.is_subset(&r_off), "cull {} vs no cull {}", r_on.len(), r_off.len());
    let check_above = |h: &Harness, cam: [f64; 3]| {
        let (views, ground) = h.s.views_ground(cam, [0.0; 3]);
        let top = h.l.rings() - 1;
        for e in h.s.entries().filter(|e| e.key.z == h.l.root_zoom) {
            assert!(h.s.horizon_ok(&views[top], ground[top], e.key, e.terrain_box, true), "{} below the horizon", e.key);
        }
    };
    check_above(&on, [0.0, 0.0, 300.0]);
    // Sink to 250 m: the line moves in by ~5 km, inside the 15 % hysteresis: nothing drops,
    // while a cover built from scratch at 250 m has fewer roots.
    on.settle([0.0, 0.0, 250.0], 20_000);
    let kept = roots(&on);
    assert_eq!(kept, r_on, "hysteresis keeps the roots");
    let mut fresh = terrain_harness(p_on.clone(), t.clone());
    fresh.settle([0.0, 0.0, 250.0], 20_000);
    let r_fresh = roots(&fresh);
    assert!(r_fresh.len() < kept.len() && r_fresh.is_subset(&kept), "fresh {} vs kept {}", r_fresh.len(), kept.len());
    // Down to 20 m AGL: far roots go (past the line * hysteresis), the rest stay above.
    on.settle([0.0, 0.0, 20.0], 20_000);
    assert!(roots(&on).len() < kept.len());
    check_above(&on, [0.0, 0.0, 20.0]);
    eprintln!(
        "horizon: roots at 300 m {} (no cull {}), fresh at 250 m {}, at 20 m {}",
        r_on.len(),
        r_off.len(),
        r_fresh.len(),
        roots(&on).len()
    );
    // Whole groups: a split node has at least one child above the horizon (with the
    // hysteresis), and groups with children that are below the plain line on their own are
    // kept whole. Low relief (20 m) so the line (~40 km at 20 m AGL) cuts through T3 groups.
    let (p_low, t) = terrain_setup(|tp| tp.amplitude_m = 20.0);
    let mut on = terrain_harness(p_low, t.clone());
    let cam = [0.0, 0.0, 20.0];
    on.settle(cam, 20_000);
    let (views, ground) = on.s.views_ground(cam, [0.0; 3]);
    let mut mixed = 0usize;
    for e in on.s.entries().filter(|e| e.split) {
        let kids = children(e.key);
        let test = |current: bool| -> Vec<bool> {
            kids.iter()
                .map(|c| {
                    let g = on.s.entries.get(c).map_or_else(|| t.tile_range(*c), |x| x.terrain_box);
                    on.s.horizon_ok(&views[e.ring - 1], ground[e.ring - 1], *c, g, current)
                })
                .collect()
        };
        assert!(test(true).iter().any(|a| *a), "{} split with all children below the horizon", e.key);
        assert!(kids.iter().all(|c| on.s.entries.contains_key(c)), "{}: group not whole", e.key);
        if test(false).iter().any(|a| !*a) {
            mixed += 1;
        }
    }
    eprintln!("horizon, low relief at 20 m: {mixed} groups kept whole with a child below the horizon");
    assert!(mixed > 0, "no sibling group straddles the horizon at 20 m");
}

/// `set_detail` re-meshes and re-delivers every held terrain tile exactly once (same keys,
/// as re-deliveries, nothing unloaded), with jobs at the new detail; a no-op elsewhere.
#[test]
fn terrain_detail_redelivers_every_held_key_once() {
    let (p, t) = terrain_setup(|_| {});
    let mut h = terrain_harness(p, t.clone());
    let cam = [0.0, 0.0, 300.0];
    h.settle(cam, 20_000);
    let held: HashSet<TileKey> = h.s.delivered_keys().into_iter().collect();
    assert!(held.len() > 50);
    let tris_before = h.s.stats().tris;
    assert!(tris_before > 0);
    h.deliveries.clear();
    assert!(h.s.set_detail(2.0));
    assert!(!h.s.set_detail(2.0), "same scale: nothing to do");
    assert_eq!(h.s.detail(), 2.0);
    let mut jobs = Vec::new();
    h.s.update(cam, [0.0; 3], h.now, &mut jobs);
    assert!(!jobs.is_empty() && jobs.iter().all(|j| matches!(j, Job::Terrain { detail, .. } if *detail == 2.0)));
    h.w.submit(&mut jobs);
    let mut unloads = 0;
    for _ in 0..20_000 {
        let (_, un) = h.frame(cam, [0.0; 3]);
        unloads += un.len();
        h.check_frame(&un, "detail");
        if h.s.settled() {
            break;
        }
    }
    assert!(h.s.settled());
    assert_eq!(unloads, 0);
    assert!(h.deliveries.iter().all(|(_, re)| *re), "only re-deliveries");
    let mut counts: AHashMap<TileKey, usize> = AHashMap::new();
    for (k, _) in &h.deliveries {
        *counts.entry(*k).or_default() += 1;
    }
    assert_eq!(counts.keys().copied().collect::<HashSet<_>>(), held, "every held key, nothing else");
    assert!(counts.values().all(|&n| n == 1), "each exactly once");
    // The vector layer has no detail.
    assert!(!Scheduler::new(Frame::new(VILNIUS.0, VILNIUS.1), true, dp()).set_detail(2.0));
    eprintln!("detail 2.0: {} held terrain tiles re-delivered once each", held.len());
}

/// Budget measurement (informative): `cargo test --release -p simworld-core terrain_budget --
/// --ignored --nocapture`. Default profile with the terrain on (amplitude 160 m, flat zone
/// at the origin), real TINs built on one thread: worker CPU per tile per band and the
/// resident (held) terrain tiles and triangles at 300 m and 3000 m AGL, settled.
#[test]
#[ignore]
fn terrain_budget() {
    let (p, t) = terrain_setup(|_| {});
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    for alt in [300.0, 3000.0] {
        let mut h = terrain_harness(p.clone(), t.clone());
        h.w.real = Some(frame);
        h.w.profile = Some(p.clone());
        h.w.terrain = Some(t.clone());
        let t0 = Instant::now();
        h.settle([0.0, 0.0, alt], 20_000);
        let held = held_per_ring(&h);
        let mut tris = vec![0u64; h.l.rings()];
        for e in h.s.entries().filter(|e| e.held) {
            tris[e.ring] += e.tris as u64;
        }
        eprintln!(
            "terrain at {alt} m AGL: resident per ring {held:?} (total {}), triangles per ring {tris:?} (total {}), settled in {:.1} s",
            held.iter().sum::<usize>(),
            tris.iter().sum::<u64>(),
            t0.elapsed().as_secs_f64()
        );
        eprintln!("  {}", h.s.cpu_stats().report("terrain", h.l.rings()));
    }
}

/// `fades.max_active`: never more swaps fading at once than the cap; a swap past it completes
/// at once (no fade entry, no hole); 0 turns fades off. The other contract checks hold as usual.
#[test]
fn fade_cap_limits_concurrent_swaps() {
    let mut runs = Vec::new();
    for cap in [0u32, 1, 4, 256] {
        let mut w = crate::profile::WorldProfile::default();
        w.fades.max_active = cap;
        let mut h = Harness::new();
        h.s = Scheduler::new(Frame::new(VILNIUS.0, VILNIUS.1), false, Arc::new(Profile::new(w).unwrap()));
        let dt = 1.0 / 30.0;
        let mut cam = [0.0, 0.0, 300.0];
        h.settle(cam, 20_000);
        let mut camera_holes = 0usize;
        let steps: [([f64; 3], usize, &str); 3] = [
            ([250.0, 0.0, 0.0], (30.0 / dt) as usize, "250 m/s"),
            ([0.0, 0.0, 60.0], (3000.0 / 60.0 / dt) as usize, "climb"),
            ([-250.0, 0.0, -60.0], (3000.0 / 60.0 / dt) as usize, "descent"),
        ];
        for (vel, n, name) in steps {
            for _ in 0..n {
                for a in 0..3 {
                    cam[a] += vel[a] * dt;
                }
                let (_, un) = h.frame(cam, vel);
                h.check_frame(&un, name);
                let v = h.view(cam);
                let under = h.g.tile_at(v.wx, v.wy);
                let fading_pair = under.len() == 2 && under.iter().all(|k| h.g.fades.contains_key(k));
                if under.len() != 1 && !fading_pair {
                    camera_holes += 1;
                }
            }
        }
        h.settle(cam, 20_000);
        assert!(h.g.check().is_empty());
        assert_eq!(camera_holes, 0, "cap {cap}: point under the camera uncovered");
        assert!(h.max_fades_seen <= cap as usize, "cap {cap}: {} fades at once", h.max_fades_seen);
        assert_eq!(h.s.capped_total() as usize, h.capped_seen, "cap {cap}: every capped swap seen");
        match cap {
            0 => assert_eq!(h.refine_fades + h.coarsen_fades, 0, "cap 0: no fades"),
            256 => assert_eq!(h.capped_seen, 0, "cap 256 is never reached here"),
            _ => assert!(h.max_fades_seen == cap as usize && h.capped_seen > 0, "cap {cap} is reached"),
        }
        runs.push((cap, h.refine_fades + h.coarsen_fades, h.capped_seen, h.max_fades_seen));
    }
    for (cap, faded, capped, peak) in runs {
        eprintln!("fade cap {cap}: {faded} swaps faded, {capped} without a fade, peak {peak} at once");
    }
}

// ---------------------------------------------------------------------------------------------
// Tiles v2, step T2a: R0 / R1 cut from detail data tiles

/// A harness whose vector scheduler cuts R0 / R1 from z14 data tiles (the compatibility levels);
/// the z14 tile under the origin holds the `data::fixture` area, every other data tile is empty.
/// R2 / R3 stay fake per-tile jobs. `budget`: the data tiles' memory budget in bytes.
fn data_harness(budget: u64) -> Harness {
    let mut h = Harness::new();
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    h.s = Scheduler::new(frame, false, dp()).with_data_levels(crate::data::DataLevels::COMPAT);
    h.w.real = Some(frame);
    let k14 = crate::data::fixture::key14();
    h.w.data_src.insert(k14, crate::data::fixture::tile_bytes(k14).into());
    h.mem = Some(crate::budget::MemoryBudget::new(budget));
    h
}

/// The detail data tiles of every chunk the cover ever wanted.
fn data_tiles_of(h: &Harness) -> HashSet<TileKey> {
    h.ever_requested.iter().filter_map(|k| crate::data::DataLevels::COMPAT.detail_of(*k)).collect()
}

/// One fetch and one decode per data tile, however many R0 / R1 chunks are cut from it; no
/// per-tile fetch of a z15 / z16 key; every requested chunk is delivered (guarantee 2); the
/// chunks under the camera carry the fixture's buildings; the decoded tiles are in the memory
/// budget and pinned only while chunk jobs run.
#[test]
fn data_tiles_are_fetched_and_decoded_once_for_all_their_chunks() {
    let mut h = data_harness(512 << 20);
    let cam = [0.0, 0.0, 300.0];
    let frames = h.settle(cam, 20_000);
    let tiles = data_tiles_of(&h);
    let chunks = h.ever_requested.iter().filter(|k| k.z >= 15).count();
    assert!(tiles.len() >= 2 && chunks >= 4 * tiles.len(), "{chunks} chunks from {} data tiles", tiles.len());
    let n = tiles.len() as u32;
    assert_eq!(h.w.data_jobs, (n, n), "one fetch and one decode per data tile");
    let st = h.s.data_stats();
    assert_eq!((st.decoded, st.fetched, st.loaded, st.fetching), (n as u64, n as u64, n, 0));
    assert!(h.w.fetch_zooms.iter().all(|&z| z <= 14), "a chunk was fetched on its own: {:?}", h.w.fetch_zooms);
    assert_eq!(h.ever_requested, h.ever_delivered);
    let under = crate::geo::tile_for(VILNIUS.0, VILNIUS.1, 16);
    assert!(h.w.chunk_meshes.get(&under).is_some_and(|m| !m.is_empty()), "R0 under the camera has the fixture's buildings");
    let mem = h.mem.as_ref().unwrap().stats();
    let pool = mem.pool(crate::budget::Pool::DataTiles);
    assert_eq!((pool.items, pool.pinned_items, pool.evictions), (n, 0, 0));
    assert!(pool.bytes > 20_000, "{}", pool.bytes);
    eprintln!("settled in {frames} frames: {chunks} chunks cut from {n} data tiles ({} KB)", pool.bytes / 1024);
}

/// Under a budget too small for one data tile, every decoded tile is evicted as soon as no chunk
/// job pins it, and the chunks still waiting fetch and decode it again: the cover completes with
/// the same chunk meshes as without eviction, and nothing pinned stays over the budget.
#[test]
fn evicted_data_tiles_are_fetched_again_and_give_the_same_chunks() {
    let cam = [0.0, 0.0, 300.0];
    let mut a = data_harness(512 << 20);
    a.settle(cam, 20_000);
    let mut b = data_harness(1);
    b.settle(cam, 40_000);
    assert_eq!(b.ever_requested, b.ever_delivered);
    let n = data_tiles_of(&b).len() as u64;
    let st = b.s.data_stats();
    assert!(st.decoded > n && st.fetched == st.decoded, "re-fetched after eviction: {st:?} for {n} tiles");
    assert_eq!(st.loaded, 0, "nothing stays resident over the budget");
    assert_eq!(a.w.chunk_meshes, b.w.chunk_meshes, "same chunks with and without eviction");
    let mem = b.mem.as_ref().unwrap().stats();
    let pool = mem.pool(crate::budget::Pool::DataTiles);
    assert_eq!((pool.items, pool.pinned_items, mem.over_budget_bytes), (0, 0, 0));
    assert!(pool.evictions > n, "{pool:?}");
    eprintln!("budget 1 byte: {} decodes for {n} data tiles, {} evictions", st.decoded, pool.evictions);
}

/// A data fetch nobody needs any more (the camera left before it arrived) is cancelled, and its
/// late reply changes nothing.
#[test]
fn unneeded_data_fetches_are_cancelled() {
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let mut s = Scheduler::new(frame, false, dp()).with_data_levels(crate::data::DataLevels::COMPAT);
    let t0 = Instant::now();
    let mut jobs = Vec::new();
    s.update([0.0, 0.0, 300.0], [0.0; 3], t0, &mut jobs);
    let fetches: Vec<(TileKey, u64)> =
        jobs.iter().filter_map(|j| if let Job::FetchData { key, job } = j { Some((*key, *job)) } else { None }).collect();
    assert!(!fetches.is_empty() && fetches.iter().all(|(k, _)| k.z == 14));
    assert!(jobs.iter().all(|j| !matches!(j, Job::Fetch { key, .. } if key.z >= 15)));
    assert_eq!(s.data_stats().fetching, fetches.len() as u32);
    // Teleport 60 km east: no chunk of the old area is wanted.
    jobs.clear();
    s.update([60_000.0, 0.0, 300.0], [0.0; 3], t0 + Duration::from_millis(30), &mut jobs);
    let mut cancelled = Vec::new();
    s.drain_cancelled(&mut cancelled);
    for (_, job) in &fetches {
        assert!(cancelled.contains(job), "fetch {job} not cancelled");
    }
    // The cancelled replies free the slots; a late success is ignored (the job is stale).
    for (i, (key, job)) in fetches.iter().enumerate() {
        let result = if i == 0 { Ok((Bytes::new(), Source::Network)) } else { Err(FetchError::new(crate::fetch::FetchErrorKind::Cancelled, "x")) };
        assert!(s.handle(WorkerMsg::DataFetched { key: *key, job: *job, result }, t0).is_none());
    }
    assert!(s.data().unwrap().get(fetches[0].0).is_none_or(|e| !matches!(e.state, DataState::Fetched(_))));
}

// ---------------------------------------------------------------------------------------------
// Tiles v2, step T2b: a two-level source (detail z14 + overview z11) and the landmark layer

/// A harness on the two-level fixture (`data::fixture`): the detail z14 tile under the origin
/// and its overview z11 tile hold data, every other data tile is empty. `landmarks` = the host
/// draws the landmark layer.
fn v2_harness(landmarks: bool) -> Harness {
    use crate::data::fixture::{detail_v2_bytes, key11, key14, overview_bytes};
    let mut h = Harness::new();
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    h.s = Scheduler::new(frame, false, dp()).with_data_levels(crate::data::DataLevels::two_level(14, 11, true)).with_landmark_layer(landmarks);
    h.w.real = Some(frame);
    h.w.data_src.insert(key14(), detail_v2_bytes().into());
    h.w.data_src.insert(key11(), overview_bytes().into());
    h.mem = Some(crate::budget::MemoryBudget::new(512 << 20));
    h
}

/// Every band is cut from the two data levels: no per-tile fetch at all, one fetch and one
/// decode per data tile (detail for R0 / R1, overview for R2 / R3 and the landmarks), every
/// requested chunk delivered; R2 / R3 carry the overview's blocks and light points. Landmarks:
/// with the layer, one `l/` tile (the only overview tile with landmarks), delivered once, and no
/// chunk folds them in; without it, no `l/` key and every chunk with data folds them in.
#[test]
fn two_level_source_cuts_every_band_from_two_data_tile_levels() {
    use crate::data::fixture::{key11, key14};
    for landmarks in [true, false] {
        let mut h = v2_harness(landmarks);
        let cam = [0.0, 0.0, 300.0];
        h.settle(cam, 20_000);
        assert!(h.w.fetch_zooms.is_empty(), "a chunk was fetched on its own: {:?}", h.w.fetch_zooms);
        assert_eq!(h.ever_requested, h.ever_delivered);
        let levels = crate::data::DataLevels::two_level(14, 11, true);
        let mut tiles: HashSet<TileKey> = h.ever_requested.iter().filter_map(|k| levels.data_of(*k)).collect();
        if !landmarks {
            tiles.extend(h.ever_requested.iter().filter_map(|k| levels.landmarks_of(*k)));
        }
        if landmarks {
            tiles.extend(h.lm_held.iter().copied());
        }
        let n = tiles.len() as u32;
        assert!(tiles.iter().any(|k| k.z == 11) && tiles.iter().any(|k| k.z == 14), "{tiles:?}");
        assert_eq!(h.w.data_jobs, (n, n), "one fetch and one decode per data tile ({n})");
        let r2 = h.w.chunk_meshes.get(&key14()).expect("the R2 chunk over the fixture");
        assert!(!r2.is_empty(), "R2 from the overview blocks");
        if landmarks {
            assert_eq!(h.lm_held, [key11()].into_iter().collect(), "one landmark tile");
            assert_eq!(h.lm_delivered, 1);
            assert!(h.w.chunk_lm.values().all(|&f| !f), "the landmark layer draws them");
            let st = h.s.landmark_stats();
            assert_eq!((st.loaded, st.pending, st.delivered), (1, 0, 1), "{st:?}");
            assert!(st.empty > 0, "empty overview tiles are built but not delivered");
        } else {
            assert_eq!(h.lm_delivered, 0);
            assert!(h.w.chunk_lm.values().all(|&f| f), "every chunk folds the landmarks in");
            assert_eq!(h.s.landmark_stats(), Default::default());
        }
        eprintln!("landmark layer {landmarks}: {} chunks from {n} data tiles", h.ever_requested.len());
    }
}

/// The landmark tile is held while its box is within `bands.max_distance_m` and unloaded once the
/// camera is past it (with the hysteresis); flying back delivers it again, once. Never in
/// `poll_fades` (checked every frame by the harness).
#[test]
fn landmark_tile_unloads_beyond_range_and_comes_back_once() {
    let mut h = v2_harness(true);
    h.settle([0.0, 0.0, 300.0], 20_000);
    assert_eq!((h.lm_delivered, h.lm_unloaded, h.lm_held.len()), (1, 0, 1));
    // 60 km east: the z11 tile (~11 km wide) is beyond 25 km * 1.15.
    h.settle([60_000.0, 0.0, 300.0], 40_000);
    assert_eq!((h.lm_delivered, h.lm_unloaded, h.lm_held.len()), (1, 1, 0));
    assert_eq!(h.s.delivered_landmark_keys(), Vec::<TileKey>::new());
    h.settle([0.0, 0.0, 300.0], 40_000);
    assert_eq!((h.lm_delivered, h.lm_unloaded, h.lm_held.len()), (2, 1, 1));
}

/// Light-mode switch on overview chunks: the held R2 / R3 tiles are re-delivered with the other
/// light version, R2 re-meshed from the resident overview tile: no data fetch, no decode.
#[test]
fn relight_remeshes_from_the_resident_overview_tile_without_fetching() {
    let mut h = v2_harness(true);
    h.settle([0.0, 0.0, 2000.0], 20_000);
    let jobs = h.w.data_jobs;
    let re = h.redelivered_total;
    // Above `lights.cluster_on_agl_m` (3300 m): clustered lights.
    h.settle([0.0, 0.0, 3600.0], 20_000);
    assert!(h.s.clustered());
    assert!(h.redelivered_total > re, "lit tiles re-delivered");
    let held_overview = h.s.delivered_keys().iter().filter(|k| k.z <= 14).count();
    assert!(held_overview > 0);
    // New overview / detail tiles may enter the cover at the new height; the re-lit ones were
    // all resident already.
    let st = h.s.data_stats();
    assert_eq!(st.fetched, h.w.data_jobs.0 as u64);
    assert_eq!(h.w.data_jobs.0 - jobs.0, h.w.data_jobs.1 - jobs.1, "every fetch decoded once");
    eprintln!("relight: {} re-deliveries, data jobs {:?} -> {:?}", h.redelivered_total - re, jobs, h.w.data_jobs);
    // Pure re-light at the same height band: go back down (unclustered), nothing new to fetch.
    let jobs2 = h.w.data_jobs;
    h.settle([0.0, 0.0, 2000.0], 20_000);
    assert!(!h.s.clustered());
    assert_eq!(h.w.data_jobs.0, jobs2.0, "re-light fetched a data tile");
}

/// Archive source before it opened: nothing is dispatched until the levels are known.
#[test]
fn pending_levels_dispatch_nothing_until_the_archive_opened() {
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let mut s = Scheduler::new(frame, false, dp()).with_data_pending();
    let mut jobs = Vec::new();
    let t0 = Instant::now();
    for i in 0..5 {
        s.update([0.0, 0.0, 300.0], [0.0; 3], t0 + Duration::from_millis(i * 30), &mut jobs);
    }
    assert!(jobs.is_empty() && s.data_pending() && s.entries().count() > 0);
    assert!(s.set_data_levels(crate::data::DataLevels::COMPAT));
    assert!(!s.set_data_levels(crate::data::DataLevels::two_level(14, 11, true)), "set once");
    s.update([0.0, 0.0, 300.0], [0.0; 3], t0 + Duration::from_millis(200), &mut jobs);
    assert!(jobs.iter().any(|j| matches!(j, Job::Fetch { .. })) && jobs.iter().any(|j| matches!(j, Job::FetchData { .. })));
}
