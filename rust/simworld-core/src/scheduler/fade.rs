//! LOD swap fades: the distance-driven fade value of one swap (module doc of `scheduler`,
//! "Fades") and the hysteresis gap that drives it.

use std::time::{Duration, Instant};

use crate::rings::Layer;

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
    pub(super) fn new(refine: bool, d: f64, now: Instant) -> Self {
        let t = if refine { 0.0 } else { 1.0 };
        Self { refine, t, t0: t, d0: d, started: now }
    }

    /// Change direction, continuing from the current `t` at distance `d`.
    pub(super) fn retarget(&mut self, refine: bool, d: f64) {
        if self.refine != refine {
            self.refine = refine;
            self.t0 = self.t;
            self.d0 = d;
        }
    }

    /// Advance with the current slant distance `d` over the hysteresis gap `gap` (m).
    /// Returns true when the fade has completed (target reached or the safety cap `cap`).
    pub(super) fn advance(&mut self, d: f64, gap: f64, now: Instant, cap: Duration) -> bool {
        if d.is_finite() && gap > 0.0 {
            if !self.d0.is_finite() {
                // No anchor yet (started without a distance): anchor here. A NaN `d0` would
                // turn the drive into `NaN.min(1.0)` = 1 and complete the fade at once.
                self.t0 = self.t;
                self.d0 = d;
            }
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
    pub(super) fn reported(&self) -> f32 {
        let q = (self.t.clamp(0.0, 1.0) * FADE_QUANTUM).round() / FADE_QUANTUM;
        let step = 1.0 / FADE_QUANTUM;
        (if self.refine { q.min(1.0 - step) } else { q.max(step) }) as f32
    }
}

/// The hysteresis gap (m) that drives the fade of a coarse tile of `ring` (>= 1).
pub fn fade_gap_m(l: &Layer, ring: usize) -> f64 {
    l.edge_m[ring.max(1) - 1] * (l.hysteresis - 1.0)
}
