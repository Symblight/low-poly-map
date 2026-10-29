//! Read-only views for the host and tools: settled state, stats, tiles, distances, priorities.

use super::*;

impl Scheduler {

    /// Nothing in flight or waiting: every tracked tile is delivered with the current lights.
    pub fn settled(&self) -> bool {
        self.probes_idle()
            && self.landmarks.unloading.is_empty()
            && self.landmarks.entries.values().all(|e| matches!(e.state, LmState::Idle | LmState::Empty | LmState::Broken))
            && self.unloading.is_empty()
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
    pub(super) fn order_key(&self, e: &Entry) -> (f64, f64, usize, TileKey) {
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
