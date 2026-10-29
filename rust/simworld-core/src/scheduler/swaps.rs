//! Sibling swaps: refine and coarsen fades, the fade cap and parent drops.

use super::*;

impl Scheduler {

    /// `k` is one side of an active fade (the coarse tile or one of its children).
    pub fn in_fade(&self, k: TileKey) -> bool {
        self.fades.contains_key(&k) || self.layer.parent(k).is_some_and(|p| self.fades.contains_key(&p))
    }

    /// Start the refine fade of a held, refined parent once all 4 children are uploaded
    /// (guarantee 3). Children 0, parent 1; the parent stays uploaded until it completes.
    pub(super) fn try_start_refine(&mut self, k: TileKey) {
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
    pub(super) fn fade_slot_free(&self) -> bool {
        self.fades.len() + self.fades_elsewhere < self.max_fades
    }

    pub(super) fn note_capped(&mut self, k: TileKey) {
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
    pub(super) fn fade_distance(&self, k: TileKey) -> f64 {
        match (&self.cam_view, self.entries.get(&k)) {
            (Some(v), Some(e)) => v.slant_box_m(k, e.terrain_box[0], e.terrain_box[1] + e.box_h),
            _ => f64::NAN, // no anchor: `Fade::advance` anchors at the first finite distance
        }
    }

    /// Advance every fade with its coarse tile's slant distance; complete the finished ones.
    pub(super) fn advance_fades(&mut self, now: Instant) {
        let mut done: Vec<(TileKey, bool)> = Vec::new();
        let (entries, cam, layer, cap) = (&self.entries, &self.cam_view, &self.layer, self.fade_cap);
        for (k, f) in self.fades.iter_mut() {
            let (Some(e), Some(v)) = (entries.get(k), cam) else { continue };
            // Terrain (G2): the gap of the tile's own refine distance.
            let gap = if layer.kind == LayerKind::Terrain { e.refine_m * (layer.hysteresis - 1.0) } else { fade_gap_m(layer, e.ring) };
            if f.advance(v.slant_box_m(*k, e.terrain_box[0], e.terrain_box[1] + e.box_h), gap, now, cap) {
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
    pub(super) fn finish_coarsen(&mut self, k: TileKey) {
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
                self.recent.left(c, self.now);
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
    pub(super) fn try_drop(&mut self, k: TileKey) {
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
            self.recent.left(k, self.now);
        }
    }

}
