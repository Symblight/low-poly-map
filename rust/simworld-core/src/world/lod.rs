//! The host's view and quality for the screen-space-error cover (contract `set_view`,
//! `set_lod_quality`, api 1.10) and the tile budget that caps it (`lod.max_tiles`, review M1).

use super::{World, LOD_QUALITY_MAX, LOD_QUALITY_MIN};
use crate::rings::{LodView, RING_COUNT};

impl World {
    /// Contract `set_view(fov_y_deg, viewport_h_px, forward)` (api 1.10): the view the
    /// screen-space error is measured in. `fov_y_deg` in [1, 179], `viewport_h_px` (physical
    /// pixels) in [1, 65536], `forward` (local axes) finite and non-zero; anything else is
    /// ignored with a rate-limited warning and no state change (returns false). Until it is
    /// called the default view applies (60 deg, 1080 px, -Z). Kept across `configure_world`;
    /// the cover follows from the next `update_camera`, no reconfigure. `forward` is stored for
    /// the rear-hemisphere relaxation (step T4) and does not move the cover yet.
    pub fn set_view(&mut self, fov_y_deg: f64, viewport_h_px: f64, forward: [f64; 3]) -> bool {
        let Some(v) = LodView::new(fov_y_deg, viewport_h_px, forward) else {
            self.warn(&format!(
                "WorldStreamer: set_view({fov_y_deg}, {viewport_h_px}, {forward:?}) ignored: needs fov_y_deg in [1, 179], viewport_h_px in [1, 65536] and a finite non-zero forward"
            ));
            return false;
        };
        self.lod_view = v;
        self.apply_lod_view();
        true
    }

    /// Contract `set_lod_quality(scale)` (api 1.10): live multiplier of `lod.tau_px` (tau_eff =
    /// `tau_px` x scale; 2 = half the refine distances, coarser and cheaper), clamped to
    /// [0.25, 4]. Non-finite input is ignored. Kept across `configure_world`; the cover follows
    /// from the next `update_camera`. Returns the scale in effect.
    pub fn set_lod_quality(&mut self, scale: f64) -> f64 {
        if scale.is_finite() {
            self.lod_quality = scale.clamp(LOD_QUALITY_MIN, LOD_QUALITY_MAX);
            self.apply_lod_view();
        }
        self.lod_quality
    }

    pub(super) fn apply_lod_view(&mut self) {
        let q = self.cover_quality();
        if let Some(s) = &mut self.scheduler {
            s.set_lod_view(&self.lod_view, q);
        }
        // Terrain (G2): the SSE view, and the foundation guard from the vector edges.
        if let Some(t) = &mut self.terrain_sched {
            t.set_lod_view(&self.lod_view, q);
        }
    }

    /// The quality scale the vector cover runs with: the host's `set_lod_quality` scale, raised
    /// when the estimated cover of the view would exceed `lod.max_tiles` (review M1;
    /// [`crate::rings::BandParams::capped_quality`], at the origin's latitude).
    pub fn cover_quality(&self) -> f64 {
        let cos_lat = crate::math::cos(self.origin_lat.to_radians());
        self.profile.bands().capped_quality(&self.lod_view, self.lod_quality, cos_lat)
    }

    /// Estimated tile count of the vector cover per ring at the view and quality in effect
    /// ([`crate::rings::estimate_cover_tiles`]; `debug_stats()["lod_tiles_est"]`).
    pub fn lod_tiles_estimate(&self) -> [f64; RING_COUNT] {
        crate::rings::estimate_cover_tiles(&self.lod_edges_m(), crate::math::cos(self.origin_lat.to_radians()))
    }

    /// The view in effect (see [`World::set_view`]).
    pub fn lod_view(&self) -> LodView {
        self.lod_view
    }

    /// The `set_lod_quality` scale in effect.
    pub fn lod_quality(&self) -> f64 {
        self.lod_quality
    }

    /// Effective SSE threshold (px): `lod.tau_px` x the quality scale (adaptive factors: T5).
    pub fn tau_eff(&self) -> f64 {
        self.profile.lod.tau_px * self.cover_quality()
    }

    /// The vector layer's band edges in effect (m, R0..R3; R3 = `bands.max_distance_m`): the
    /// SSE refine distances of the current view and tau_eff.
    pub fn lod_edges_m(&self) -> [f64; RING_COUNT] {
        self.profile.bands().edges_for(&self.lod_view, self.cover_quality())
    }
}
