//! Delivery to the host: ready tiles in the merged cross-layer order, fades and unloads.

use super::*;

/// An empty mesh (the `terrain` of vector tiles).
static EMPTY_MESH: MeshData = MeshData { positions: Vec::new(), normals: Vec::new(), colors: Vec::new(), uvs: Vec::new(), indices: Vec::new(), surface: None };

/// One finished tile, borrowed for the duration of the `pop_ready` callback.
pub struct ReadyTileView<'a> {
    /// `World` (vector tile, key `z/x/y`) or `Terrain` (terrain tile, key `t/z/x/y`).
    pub layer: LayerKind,
    pub key: TileKey,
    /// Vector: 0..3 (`16 - z`); terrain: index in `terrain.bands` (`near_zoom - z`).
    pub ring: usize,
    /// Tile root in the current local frame (x east, y up, z -north; y is 0).
    pub origin: [f64; 3],
    /// Building mesh relative to `origin` (empty for R3 and empty tiles).
    pub mesh: &'a MeshData,
    /// Ground mesh relative to `origin` (empty for tiles without ground features).
    pub ground: &'a MeshData,
    /// Light sprite positions relative to `origin` (R2/R3 only).
    pub lights: &'a [[f32; 3]],
    /// One rgba per light (rgb = color, a = intensity weight).
    pub light_colors: &'a [[f32; 4]],
    /// Street lamp positions relative to `origin` (bands `lights.street.min_ring..=3`).
    pub street_lights: &'a [[f32; 3]],
    /// One rgba per street lamp (rgb = `lights.street.color`, a = intensity weight).
    pub street_light_colors: &'a [[f32; 4]],
    /// Aircraft warning beacons relative to `origin` (bands `lights.beacons.min_ring..=3`).
    pub beacon_lights: &'a [[f32; 3]],
    /// One rgba per beacon (rgb = color, a = blink phase 0..1 of its building).
    pub beacon_light_colors: &'a [[f32; 4]],
    /// Airport lights of a seed world relative to `origin` (api 1.7; empty elsewhere).
    pub airport_lights: &'a [[f32; 3]],
    /// One rgba per airport light (rgb = color, a = intensity weight).
    pub airport_light_colors: &'a [[f32; 4]],
    /// Terrain tiles: VERTEX + INDEX (+ NORMAL with smooth shading) relative to `origin`; empty
    /// for vector tiles.
    pub terrain: &'a MeshData,
    /// Terrain tiles: lowest / highest surface vertex (m above MSL); 0 for vector tiles.
    pub h_min: f64,
    pub h_max: f64,
}

impl ReadyTileView<'_> {
    pub fn layer_key(&self) -> LayerKey {
        LayerKey { layer: self.layer, key: self.key }
    }
}

impl World {
    /// Hand at most `max` finished tiles to `f`, in priority order. Returns how many were
    /// handed out. The host converts each tile inside `f` (its time counts in the profile line).
    /// With the terrain layer, both layers share the budget in one order: fresh tiles before
    /// re-deliveries, then strictly by slant distance from the camera (ties: vector world);
    /// re-deliveries of both layers share the per-frame limit.
    pub fn pop_ready(&mut self, max: usize, mut f: impl FnMut(ReadyTileView<'_>)) -> usize {
        let timer = self.call_timer();
        self.drain_worker();
        let t_drained = Instant::now();
        let mut tris = 0usize;
        let local = self.local;
        let per_frame = self.profile.streaming.redeliveries_per_frame as usize;
        let Some(sched) = self.scheduler.as_mut() else { return 0 };
        let mut tsched = self.terrain_sched.as_mut();
        let mut n = 0;
        while n < max {
            // Re-deliveries are rate-limited per frame, not per call.
            let allow = self.redelivered_this_frame < per_frame;
            let w = sched.best_ready(allow);
            let t = tsched.as_deref_mut().and_then(|s| s.best_ready(allow));
            let pick_terrain = terrain_first(w.map(|(_, r, d)| (r, d)), t.map(|(_, r, d)| (r, d)));
            let tile = if pick_terrain {
                let (k, re, _) = t.expect("picked");
                let elsewhere = sched.active_fades();
                tsched.as_deref_mut().and_then(|s| {
                    s.set_fades_elsewhere(elsewhere);
                    s.take_ready(k, re)
                })
            } else {
                let Some((k, re, _)) = w else { break };
                sched.set_fades_elsewhere(tsched.as_deref().map_or(0, |s| s.active_fades()));
                sched.take_ready(k, re)
            };
            let Some(t) = tile else { break };
            n += 1;
            if t.redelivery {
                self.redelivered_this_frame += 1;
            }
            let tile_tris = t.mesh.triangle_count() + t.ground.triangle_count() + t.terrain.as_ref().map_or(0, |x| x.triangle_count());
            tris += tile_tris;
            let cpu0 = if self.diag.profile { thread_cpu_us() } else { 0 };
            f(view_of(&t, local));
            drop(t); // shared with the recent-tile cache: usually a reference count
            if self.diag.profile {
                self.poll_profile.record(thread_cpu_us().saturating_sub(cpu0), tile_tris);
            }
        }
        if let Some(t) = timer {
            self.end_call_with("poll_ready", timer, || {
                let d = t.us_until(t_drained);
                format!("(drain {d}us, convert {n} tiles / {tris} tris {}us)", t.us_until(Instant::now()) - d)
            });
        }
        // A delivery can drop a refined parent whose re-fetch was in flight (`try_drop`).
        self.cancel_orphans();
        n
    }

    /// Tile keys the host must remove (including tiles left over from a previous `configure`).
    /// The slice is valid until the next call.
    pub fn drain_unload(&mut self) -> &[LayerKey] {
        self.unload_buf.clear();
        self.unload_buf.append(&mut self.stale_unloads);
        for (s, layer) in [(&mut self.scheduler, LayerKind::World), (&mut self.terrain_sched, LayerKind::Terrain)] {
            if let Some(s) = s {
                self.key_tmp.clear();
                s.drain_unload(&mut self.key_tmp);
                self.unload_buf.extend(self.key_tmp.iter().map(|&key| LayerKey { layer, key }));
            }
        }
        if let Some(s) = &mut self.scheduler {
            self.key_tmp.clear();
            s.drain_landmark_unload(&mut self.key_tmp);
            self.unload_buf.extend(self.key_tmp.iter().map(|&key| LayerKey::landmarks(key)));
        }
        &self.unload_buf
    }

    /// Every tile in a LOD transition: key -> visible fraction `fade` (0..1). The incoming side
    /// ramps 0 -> 1, the outgoing side 1 -> 0, the two sum to 1. Tiles absent have fade 1.
    /// Finished fades report their final values once (incoming 1.0; outgoing 0.0, then unloaded
    /// by this frame's `drain_unload`). Finals come first; a later entry for the same key wins.
    /// The slice is valid until the next call.
    pub fn drain_fades(&mut self) -> &[(LayerKey, f32)] {
        self.fade_buf.clear();
        for (s, layer) in [(&mut self.scheduler, LayerKind::World), (&mut self.terrain_sched, LayerKind::Terrain)] {
            if let Some(s) = s {
                self.fade_tmp.clear();
                s.drain_fades(&mut self.fade_tmp);
                self.fade_buf.extend(self.fade_tmp.iter().map(|&(key, f)| (LayerKey { layer, key }, f)));
            }
        }
        &self.fade_buf
    }
}

/// A scheduler tile as the host view (origin in the current local frame).
fn view_of(t: &ReadyTile, local: LocalSpace) -> ReadyTileView<'_> {
    let [ox, oz] = local.to_local(t.center[0], t.center[1]);
    let (lp, lc) = t.light_arrays();
    let (sp, sc) = t.street_light_arrays();
    let (bp, bc) = t.beacon_arrays();
    let (ap, ac) = t.airport_arrays();
    let (terrain, h_min, h_max) = match &t.terrain {
        Some(x) => (&x.mesh, x.h_min as f64, x.h_max as f64),
        None => (&EMPTY_MESH, 0.0, 0.0),
    };
    ReadyTileView {
        layer: t.layer,
        key: t.key,
        ring: t.ring,
        origin: [ox, 0.0, oz],
        mesh: &t.mesh,
        ground: &t.ground,
        lights: lp,
        light_colors: lc,
        street_lights: sp,
        street_light_colors: sc,
        beacon_lights: bp,
        beacon_light_colors: bc,
        airport_lights: ap,
        airport_light_colors: ac,
        terrain,
        h_min,
        h_max,
    }
}
