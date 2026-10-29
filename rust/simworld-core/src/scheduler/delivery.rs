//! Delivery to the host: ready-tile order, hand-out, unload and cancel batches.

use super::*;

impl Scheduler {

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
        // Nothing is taken or dropped during this call: one presence memo for every tile.
        let mut memo = std::mem::take(&mut self.presence);
        memo.clear();
        for &k in &self.ready {
            let e = &self.entries[&k];
            if e.held || e.frozen || !self.deliverable_in(k, &mut memo) {
                continue;
            }
            let o = self.order_key(e);
            if best.is_none_or(|b| near_cmp(o, b).is_lt()) {
                best = Some(o);
            }
        }
        // Landmark tiles (api 1.11): fresh deliveries by their own distance, no parent rule.
        self.presence = memo;
        for e in self.landmarks.entries.values() {
            if matches!(e.state, LmState::Ready(_)) {
                let o = (e.cam_d, e.cam_d, 0, e.key);
                if best.is_none_or(|b| near_cmp(o, b).is_lt()) {
                    best = Some(o);
                }
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
        if self.layer.kind == LayerKind::World && self.layer.ring_for_zoom(k.z).is_none() {
            return self.take_landmark(k);
        }
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
            self.latency.record_delivery(e.ring, &e.times, Instant::now(), !e.delivered_once);
        }
        e.delivered_once = true;
        let (mesh, ground, lights, terrain) = match std::mem::replace(&mut e.state, TileState::Idle) {
            TileState::Ready(data) => {
                let d = *data;
                if kind != LayerKind::Landmarks {
                    let kept = Kept::of(&d);
                    self.recent.keep(k, kept, self.now);
                }
                (d.mesh, d.ground, d.lights, d.terrain)
            }
            other => {
                e.state = other;
                (Arc::default(), e.ground.clone().unwrap_or_default(), e.lights.clone()?, None)
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

    /// Hand out a ready landmark tile (`l/` key; ring 0, never a re-delivery).
    fn take_landmark(&mut self, k: TileKey) -> Option<ReadyTile> {
        let clustered = self.clustered;
        let e = self.landmarks.entries.get_mut(&k)?;
        let LmState::Ready(d) = std::mem::replace(&mut e.state, LmState::Idle) else {
            return None;
        };
        let d = *d;
        e.held = true;
        self.landmarks.delivered += 1;
        Some(ReadyTile {
            layer: LayerKind::Landmarks,
            key: k,
            ring: 0,
            center: e.center,
            mesh: d.mesh,
            ground: d.ground,
            lights: d.lights,
            clustered,
            redelivery: false,
            terrain: None,
        })
    }

    /// Keys of delivered landmark tiles that left the range (`l/z/x/y`).
    pub fn drain_landmark_unload(&mut self, out: &mut Vec<TileKey>) {
        out.append(&mut self.landmarks.unloading);
    }

    /// Keys of every landmark tile the host holds (for a reconfigure).
    pub fn delivered_landmark_keys(&self) -> Vec<TileKey> {
        self.landmarks.entries.values().filter(|e| e.held).map(|e| e.key).chain(self.landmarks.unloading.iter().copied()).collect()
    }

    /// Visit every landmark tile in range: (key, state label, ENU rect).
    pub fn for_each_landmark(&self, mut f: impl FnMut(TileKey, &'static str, &RectEn)) {
        for e in self.landmarks.entries.values() {
            let label = match e.state {
                _ if e.held => "ready",
                LmState::Wait => "fetching",
                LmState::Building | LmState::Ready(_) => "meshing",
                LmState::Failed { .. } | LmState::Broken => "failed",
                LmState::Empty => "empty",
                LmState::Idle => "ready",
            };
            f(e.key, label, &e.rect);
        }
        for k in &self.landmarks.unloading {
            f(*k, "unloading", &self.frame.tile_rect(*k));
        }
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

}
