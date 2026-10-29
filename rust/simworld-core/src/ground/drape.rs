//! Drape: the split rule of a ground edge over the terrain, triangle subdivision, road densify.

use ahash::AHashMap;

use crate::mvt::TileProjector;
use crate::rings::ring_for_zoom;

/// Terrain height at a tile pixel plus, with a lift field, the weighted band margin.
#[inline]
pub(super) fn draped_h(proj: &TileProjector, px: [f64; 2], h: f64, field: Option<f64>) -> f64 {
    match field {
        Some(l) => {
            let (lat, lon) = proj.lat_lon(px);
            h + proj.terrain().lift_weight(lat, lon) * l
        }
        None => h,
    }
}

/// Smallest drape tolerance (m): a draped edge on gentle relief is never split for less.
pub const DRAPE_FLAT_TOLERANCE_M: f64 = 0.1;
/// Safety cap of the triangle subdivision depth.
const DRAPE_MAX_DEPTH: u32 = 20;

/// Whether the ground edge a-b (tile pixels, ENU meters, terrain heights) must be split: longer
/// than `terrain.drape_step_m` and, at 1/4, 1/2 or 3/4 of it, the terrain rises more than the
/// band's drape margin above the straight line between the end heights (the margin the ground
/// vertices carry would no longer keep the edge on or above the exact height), or falls more
/// than twice the margin below it (the ground would float). Callers pass the ends in a
/// canonical order so both triangles of a shared edge decide the same.
pub fn drape_split(proj: &TileProjector, pa: [f64; 2], pb: [f64; 2], ea: [f64; 2], eb: [f64; 2], ha: f64, hb: f64) -> bool {
    drape_split_on(proj, pa, pb, ea, eb, ha, hb, None)
}

/// Tolerance of a draped edge inside an airport fence (m), at least: there the height is planar
/// per strip group (kinks only at the strip ends), so the ground follows it to 1 cm in R0 and to
/// half a stacking step in the coarser bands (R1 3 cm, R2 7.5 cm, R3 25 cm at the defaults).
pub const DRAPE_FIELD_TOLERANCE_M: f64 = 0.01;

/// The margin field of one draped feature: the band margin, the feature's stacking offset
/// (slack outside the fence) and the tolerance inside the fence.
#[derive(Clone, Copy)]
struct FieldLift {
    lift: f64,
    slack: f64,
    inside: f64,
}

impl FieldLift {
    fn new(field: Option<f64>, slack: f64, step: f64) -> Option<Self> {
        field.map(|lift| Self { lift, slack, inside: (0.5 * step).max(DRAPE_FIELD_TOLERANCE_M) })
    }
}

/// [`drape_split`] with an optional margin field (`Some((band margin, stacking offset))`,
/// contract section 9 "Airports"): heights are then terrain + weight x margin. Inside a fence
/// (weight 0 at a test point) the tolerance is [`DRAPE_FIELD_TOLERANCE_M`] / half a step both
/// ways; outside it the smallest weighted margin plus half the feature's stacking offset
/// upwards (twice the margin downwards), so the edge stays on or above the exact height.
#[allow(clippy::too_many_arguments)]
fn drape_split_on(proj: &TileProjector, pa: [f64; 2], pb: [f64; 2], ea: [f64; 2], eb: [f64; 2], ha: f64, hb: f64, field: Option<FieldLift>) -> bool {
    let step = proj.terrain().drape_step_m();
    if crate::math::hypot(eb[0] - ea[0], eb[1] - ea[1]) <= step {
        return false;
    }
    if let Some(FieldLift { lift: l, slack, inside }) = field {
        let mut test = [(0.0, 0.0); 3];
        let mut w_min = 1.0f64;
        for (k, t) in [0.25, 0.5, 0.75].into_iter().enumerate() {
            let px = [pa[0] + (pb[0] - pa[0]) * t, pa[1] + (pb[1] - pa[1]) * t];
            let (lat, lon) = proj.lat_lon(px);
            let w = proj.terrain().lift_weight(lat, lon);
            w_min = w_min.min(w);
            test[k] = (t, proj.terrain().height(lat, lon) + w * l);
        }
        // Outside the fence half the feature's stacking offset is slack too; inside it (w = 0)
        // 1 cm (R0) or half a step.
        let (rise, fall) = if w_min > 0.0 {
            ((l * w_min + 0.5 * slack).max(inside), (2.0 * l * w_min + 0.5 * slack).max(inside))
        } else {
            (inside, inside)
        };
        return test.iter().any(|&(t, h)| {
            let d = h - (ha + (hb - ha) * t);
            d > rise || -d > fall
        });
    }
    let margin = ring_for_zoom(proj.key().z)
        .map_or(DRAPE_FLAT_TOLERANCE_M, |r| (proj.terrain().drape_margin_m(r) as f64).max(DRAPE_FLAT_TOLERANCE_M));
    [0.25, 0.5, 0.75].into_iter().any(|t| {
        let px = [pa[0] + (pb[0] - pa[0]) * t, pa[1] + (pb[1] - pa[1]) * t];
        let d = proj.ground_at(px) - (ha + (hb - ha) * t);
        d > margin || -d > 2.0 * margin
    })
}

/// Triangle subdivision of one area polygon by edge midpoints (computed in tile pixels and
/// projected), shared through a per-polygon edge map, so neighbouring triangles always split a
/// shared edge the same way (no T-junction cracks).
pub(super) struct Drape<'a> {
    proj: &'a TileProjector,
    px: Vec<[f64; 2]>,
    pub(super) en: Vec<[f64; 2]>,
    pub(super) h: Vec<f64>,
    /// (lower index, higher index) -> midpoint index, or `u32::MAX` = not split.
    mids: AHashMap<(u32, u32), u32>,
    /// Lift field (see [`super::AreaCtx::field`]) and the stacking offset as drape slack.
    field: Option<f64>,
    slack: f64,
    step: f64,
}

impl<'a> Drape<'a> {
    pub(super) fn new(proj: &'a TileProjector, px: &[[f64; 2]], flat: &[f64], h: &[f64], field: Option<f64>, slack: f64, step: f64) -> Self {
        let en = flat.as_chunks::<2>().0.iter().map(|c| [c[0], c[1]]).collect();
        Self { proj, px: px.to_vec(), en, h: h.to_vec(), mids: AHashMap::new(), field, slack, step }
    }

    fn mid(&mut self, a: u32, b: u32) -> Option<u32> {
        let k = (a.min(b), a.max(b));
        if let Some(&m) = self.mids.get(&k) {
            return (m != u32::MAX).then_some(m);
        }
        let (i, j) = (k.0 as usize, k.1 as usize);
        let split = drape_split_on(self.proj, self.px[i], self.px[j], self.en[i], self.en[j], self.h[i], self.h[j], FieldLift::new(self.field, self.slack, self.step));
        let m = if split {
            let p = [(self.px[i][0] + self.px[j][0]) * 0.5, (self.px[i][1] + self.px[j][1]) * 0.5];
            let (en, h) = self.proj.project_h(p);
            self.px.push(p);
            self.en.push(en);
            self.h.push(draped_h(self.proj, p, h, self.field));
            (self.px.len() - 1) as u32
        } else {
            u32::MAX
        };
        self.mids.insert(k, m);
        (m != u32::MAX).then_some(m)
    }

    pub(super) fn triangle(&mut self, t: [u32; 3], depth: u32, out: &mut Vec<[u32; 3]>) {
        if depth >= DRAPE_MAX_DEPTH {
            out.push(t);
            return;
        }
        let [a, b, c] = t;
        let (ab, bc, ca) = (self.mid(a, b), self.mid(b, c), self.mid(c, a));
        let d = depth + 1;
        match (ab, bc, ca) {
            (None, None, None) => out.push(t),
            (Some(ab), Some(bc), Some(ca)) => {
                self.triangle([a, ab, ca], d, out);
                self.triangle([ab, b, bc], d, out);
                self.triangle([ca, bc, c], d, out);
                self.triangle([ab, bc, ca], d, out);
            }
            (Some(ab), None, None) => {
                self.triangle([a, ab, c], d, out);
                self.triangle([ab, b, c], d, out);
            }
            (None, Some(bc), None) => {
                self.triangle([b, bc, a], d, out);
                self.triangle([bc, c, a], d, out);
            }
            (None, None, Some(ca)) => {
                self.triangle([c, ca, b], d, out);
                self.triangle([ca, a, b], d, out);
            }
            (Some(ab), Some(bc), None) => {
                self.triangle([ab, b, bc], d, out);
                self.triangle([a, ab, bc], d, out);
                self.triangle([a, bc, c], d, out);
            }
            (None, Some(bc), Some(ca)) => {
                self.triangle([bc, c, ca], d, out);
                self.triangle([b, bc, ca], d, out);
                self.triangle([b, ca, a], d, out);
            }
            (Some(ab), None, Some(ca)) => {
                self.triangle([ca, a, ab], d, out);
                self.triangle([c, ca, ab], d, out);
                self.triangle([c, ab, b], d, out);
            }
        }
    }
}

/// Densify a road run (tile pixels) for the drape: recursively insert midpoints into segments
/// that [`drape_split`] says must be split. `out` receives the run with the inserted points.
pub(super) fn densify_run(proj: &TileProjector, run: &[[f64; 2]], field: Option<f64>, slack: f64, step: f64, out: &mut Vec<([f64; 2], [f64; 2], f64)>) {
    type Pt = ([f64; 2], [f64; 2], f64);
    #[allow(clippy::too_many_arguments)]
    fn seg(proj: &TileProjector, a: Pt, b: Pt, depth: u32, field: Option<f64>, slack: f64, step: f64, out: &mut Vec<Pt>) {
        if depth < DRAPE_MAX_DEPTH && drape_split_on(proj, a.0, b.0, a.1, b.1, a.2, b.2, FieldLift::new(field, slack, step)) {
            let p = [(a.0[0] + b.0[0]) * 0.5, (a.0[1] + b.0[1]) * 0.5];
            let (en, h) = proj.project_h(p);
            let m = (p, en, draped_h(proj, p, h, field));
            seg(proj, a, m, depth + 1, field, slack, step, out);
            seg(proj, m, b, depth + 1, field, slack, step, out);
        } else {
            out.push(b);
        }
    }
    out.clear();
    let mut prev: Option<Pt> = None;
    for &p in run {
        let (en, h) = proj.project_h(p);
        let cur = (p, en, draped_h(proj, p, h, field));
        match prev {
            None => out.push(cur),
            Some(a) => seg(proj, a, cur, 0, field, slack, step, out),
        }
        prev = Some(cur);
    }
}
