//! The per-frame cover update: wanted set, splits and merges, light mode, entries.

use super::*;

impl Scheduler {

    /// Recompute the cover around the camera and push new jobs into `jobs`.
    /// `cam` = (east, north, AGL) meters, `vel` = (east, north, up) m/s.
    pub fn update(&mut self, cam: [f64; 3], vel: [f64; 3], now: Instant, jobs: &mut Vec<Job>) {
        self.epoch += 1;
        self.now = now;
        self.absorb_probes();
        self.recent.expire(now);
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
        let (views, grounds, cam_view) = self.views_ground(cam, vel);
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
            // A cold root candidate waits for its box (async boxes; an entry always has one).
            let Some(g) = self.box_known(k) else { continue };
            let d = views[top].slant_box_m(k, g[0], g[1] + h);
            if l.inside(d, l.edge_m[top], existing) && self.horizon_ok(&views[top], grounds[top], k, g, existing) {
                self.stack.push((k, default_h, false));
            }
        }
        if kind == LayerKind::Terrain {
            self.plan_terrain_splits(&views, &grounds);
        }
        // No `held` flag changes during the walk: one presence memo for all of it.
        let mut presence = std::mem::take(&mut self.presence);
        presence.clear();
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
            let present = self.present_in(k, &mut presence);
            let clustered = self.clustered;
            let min_lit_ring = self.profile.lit_min_ring();
            let fading = self.fades.contains_key(&k);
            // Bottom-up coarsening: a node merges only once none of its children is split.
            let kids_split = k.z < l.leaf_zoom
                && children(k).iter().any(|c| self.entries.get(c).is_some_and(|e| e.split));
            // Horizon cull of whole sibling groups: split only while one of the 4 children is
            // above the horizon (seen from the finer band's look-ahead point).
            // (Terrain: decided by the plan, `plan_terrain_splits`.)
            let group_visible = ring == 0 || kind == LayerKind::Terrain || l.horizon_margin_m.is_none() || {
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
            // Refine distance: the band edge (vector layer) or the tile's SSE distance (terrain).
            // (Terrain: NaN = its error proxy is on the way, the entry keeps the last value.)
            let refine_m = match (ring, kind) {
                (0, _) => 0.0,
                (_, LayerKind::World | LayerKind::Landmarks) => l.edge_m[ring - 1],
                (_, LayerKind::Terrain) => self.error_known(k).map_or(f64::NAN, |e| (e * self.sse_m_per_m).max(self.guard_m[ring])),
            };
            let planned = kind == LayerKind::Terrain && ring > 0 && self.t_split.contains(&k);
            // Vector layer: a new split waits for its children's boxes (async boxes).
            let kids_known = kind == LayerKind::Terrain || ring == 0 || self.split_boxes_known(k, &views[ring - 1], inherited_h, refine_m);
            let (split, freeze, h) = {
                let e = self.touch(k, ring);
                if !refine_m.is_nan() {
                    e.refine_m = refine_m;
                }
                e.frozen = false;
                let h = e.height.map_or(inherited_h, |h| h as f64);
                e.box_h = h;
                let (lo, hi) = (e.terrain_box[0], e.terrain_box[1] + h);
                e.prio_m = views[ring].slant_box_m(k, lo, hi);
                e.cam_d = cam_view.slant_box_m(k, lo, hi);
                e.split_d = if ring > 0 { views[ring - 1].slant_box_m(k, lo, hi) } else { 0.0 };
                let want = match kind {
                    LayerKind::World | LayerKind::Landmarks => {
                        ring > 0 && group_visible && (e.split || kids_known) && l.inside(e.split_d, refine_m, e.split)
                    }
                    LayerKind::Terrain => planned,
                };
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

        self.presence = presence;
        // 2. Everything not visited left the cover: subtrees go in one unload batch.
        let epoch = self.epoch;
        let unloading = &mut self.unloading;
        let ready = &mut self.ready;
        let cancelled = &mut self.cancelled;
        let recent = &mut self.recent;
        self.entries.retain(|_, e| {
            if e.mark == epoch {
                return true;
            }
            orphan_job(cancelled, e);
            if e.held {
                recent.left(e.key, now);
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

        // 6. The landmark layer (api 1.11): the overview tiles in range.
        if kind == LayerKind::World {
            self.update_landmarks(cam_view, now);
        }

        // 7. Data tiles (Tiles v2): what the chunks of the cover (and the landmarks) wait for.
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
    pub(super) fn mark_relight(e: &mut Entry, mode: bool) {
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
    pub(super) fn touch(&mut self, k: TileKey, ring: usize) -> &mut Entry {
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

}
