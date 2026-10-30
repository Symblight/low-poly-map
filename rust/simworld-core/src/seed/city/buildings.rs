//! Stage 4, buildings (`docs/spec-seed-city-pipeline.md` section 7; generator 3:
//! `docs/spec-world-seed.md` sections 6.4-6.5): one building per lot.
//!
//! A row lot gets a building in its front part: setback from the street, a side gap, a depth
//! from the style, sometimes an L shape; a civic lot a school (3 floors) or kindergarten (2) at
//! the block centre. Kinds, storeys (`floors`, `height_exp`, few tall), the district's
//! `max_height_m` and the approach surfaces (a cap, not a removal; below `min_building_m` the
//! building is dropped) give the height. A building is kept only when its centroid is in this
//! cell, its corners lie in this cell or a same-district neighbour, on dry land outside fences
//! and clear of arterials and radials. An old town's first cell turns its `landmarks` most
//! central buildings into a church and a tower.

use crate::geo::{lat_to_tile_y, lon_to_tile_x, TileKey};
use crate::math::{exp, ln};
use crate::mvt::{centroid_px, footprint_base, tile_m_per_px, PxPolygons};
use crate::rings::{LEAF_ZOOM, ROOT_ZOOM};
use crate::seed::world::{px_of, SeedWorld, EXTENT};
use crate::seed::city::hex::{add, DIRS};
use crate::seed::city::lines::{bilinear, boxes_overlap, dist_to_line, grow, len};
use crate::seed::city::lots::{LotShape, Lots};
use crate::seed::city::network::local::LocalNetwork;
use crate::seed::city::{CellInput, RoadKind, StageHash};
use crate::seed::airport::{ApproachSurface, LocalFrame};
use crate::seed::geom::{point_in_ring, signed_area};
use crate::seed::hash::{unit, Seed};

/// Plan metres the approach-surface query box grows past the cell box. A footprint's corners
/// may lie in a same-district neighbour cell, outside the cell box (the centroid stays in the
/// cell; with the default styles the largest overhang on the two-cities world is 44.6 m, pinned
/// by `every_building_lies_inside_its_cells_approach_box`). Profile ranges allow lots of hundreds
/// of metres, so a footprint whose plan box leaves the grown box queries the surfaces with its
/// own box instead: correct for any profile, the margin only keeps that path rare.
pub const APPROACH_MARGIN_M: f64 = 100.0;

/// Headroom (m) under an approach cap for the mesher's base: [`mesh_base`] rebuilds the base in
/// the tile of the first vertex only, the mesher in every tile the footprint overlaps, and the
/// edge split there can differ by one (`m_per_px` differs by ~8e-5 relative between neighbouring
/// tiles), which puts the meshed base up to centimetres higher.
const BASE_MARGIN_M: f64 = 0.05;

/// The approach query box of a cell (plan box [x0, y0, x1, y1]) grown by [`APPROACH_MARGIN_M`]:
/// lat/lon [w, s, e, n] (the frame is equirectangular, so the box maps exactly).
pub fn approach_box(frame: &LocalFrame, cell_bbox: [f64; 4]) -> [f64; 4] {
    let b = grow(cell_bbox, APPROACH_MARGIN_M);
    let ((s, w), (n, e)) = (frame.ll([b[0], b[1]]), frame.ll([b[2], b[3]]));
    [w, s, e, n]
}

/// A generated building (counter-clockwise ring, plane offsets from the cell centre).
#[derive(Clone, Debug, PartialEq)]
pub struct PlanBuilding {
    pub id: u64,
    pub kind: String,
    pub height_m: f32,
    pub levels: f32,
    /// Capped by an approach surface.
    pub capped: bool,
    pub ring: Vec<[f32; 2]>,
}

/// Stage hash of a cell's buildings.
pub fn hash(buildings: &[PlanBuilding]) -> u64 {
    let mut h = StageHash::new("buildings");
    for b in buildings {
        h.u(b.id).s(&b.kind).f32(b.height_m).f32(b.levels).u(b.capped as u64).pts32(&b.ring);
    }
    h.finish()
}

/// The base the mesher will give a footprint (plan ring, `ll` = plan -> (lat, lon)): its base
/// rule ([`footprint_base`], contract section 7 "[1.7] Per-vertex walls", the highest exact
/// terrain under the footprint) in the pixels of the tile of its first vertex at every zoom with
/// a building mesh (R2-R0), the highest of them. The rule samples global points, so tiles and
/// zooms agree but for rounding and the edge split ([`BASE_MARGIN_M`]); `px` is scratch.
fn mesh_base(w: &SeedWorld, ring: &[[f64; 2]], ll: &mut dyn FnMut([f64; 2]) -> (f64, f64), px: &mut PxPolygons) -> f64 {
    let mut hi = f64::NEG_INFINITY;
    for z in ROOT_ZOOM + 1..=LEAF_ZOOM {
        let pts = &mut px[0][0];
        pts.clear();
        let mut key = None;
        for p in ring {
            let (lat, lon) = ll(*p);
            let k = *key.get_or_insert_with(|| TileKey::new(z, lon_to_tile_x(lon, z) as u32, lat_to_tile_y(lat, z) as u32));
            pts.push(px_of(k, lat, lon));
        }
        let Some(k) = key else { return 0.0 };
        let Some(c) = centroid_px(px) else { continue };
        hi = hi.max(footprint_base(&w.height, k, EXTENT, tile_m_per_px(k, EXTENT), px, c));
    }
    hi
}

/// The buildings of one cell's lots, sorted by id.
pub fn build(inp: &CellInput, net: &LocalNetwork, lots: &Lots) -> Vec<PlanBuilding> {
    let (plan, w, d, s, cell) = (inp.plan, inp.w, inp.d, inp.style, inp.cell);
    let cp = &w.params.city;
    let frame = plan.frame;
    let mut out: Vec<PlanBuilding> = Vec::new();

    // Context: same-district neighbours, nearby city roads, approach surfaces.
    let same: Vec<&Vec<[f64; 2]>> = DIRS
        .iter()
        .filter_map(|dd| plan.cells.get(&add(inp.a, *dd)))
        .filter(|c| c.district == Some(inp.district))
        .map(|c| &c.poly)
        .collect();
    let roads: Vec<(&[[f64; 2]], f64)> = plan
        .roads
        .iter()
        .filter(|r| boxes_overlap(grow(r.bbox, 40.0), cell.bbox))
        .map(|r| {
            let rk = if r.kind == RoadKind::Arterial { w.city_kinds.arterial } else { w.city_kinds.radial };
            (r.pts.as_slice(), rk.width_m * 0.5 + cp.clearance_m)
        })
        .collect();
    let ll = |p: [f64; 2]| {
        let (la, lo) = frame.ll(p);
        [lo, la]
    };
    // Surfaces near the cell, its box grown by `APPROACH_MARGIN_M`; a footprint leaving that
    // box queries its own (`wide_hit`: one of them met a surface, which also bars landmarks).
    let grown = grow(cell.bbox, APPROACH_MARGIN_M);
    let approach = w.approach.within(approach_box(&frame, cell.bbox));
    let mut wide_hit = false;
    let corner_ok = |p: [f64; 2]| (inp.in_cell(p) || same.iter().any(|r| point_in_ring(p, r))) && !inp.wet(p) && roads.iter().all(|(l, c)| dist_to_line(p, l) >= *c);
    let floor_m = cp.floor_m;
    // Pixel ring scratch of `mesh_base` (one footprint at a time).
    let mut px: PxPolygons = vec![vec![Vec::with_capacity(8)]];
    let mut make = |id: u64, kind: &str, floors: f64, ring: Vec<[f64; 2]>, out: &mut Vec<PlanBuilding>| {
        let n = ring.len() as f64;
        let c = [ring.iter().map(|p| p[0]).sum::<f64>() / n, ring.iter().map(|p| p[1]).sum::<f64>() / n];
        if !inp.in_cell(c) || !ring.iter().all(|p| corner_ok(*p)) {
            return;
        }
        let mut floors = floors.round().max(1.0);
        if floors * floor_m > d.max_height_m {
            floors = (d.max_height_m / floor_m).floor().max(1.0);
        }
        let mut height = floors * floor_m;
        let mut capped = false;
        let rb = ring.iter().fold([f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY], |m, p| {
            [m[0].min(p[0]), m[1].min(p[1]), m[2].max(p[0]), m[3].max(p[1])]
        });
        let own;
        let near: &[&ApproachSurface] = if rb[0] >= grown[0] && rb[1] >= grown[1] && rb[2] <= grown[2] && rb[3] <= grown[3] {
            &approach
        } else {
            let (sw, ne) = (ll([rb[0], rb[1]]), ll([rb[2], rb[3]]));
            own = w.approach.within([sw[0], sw[1], ne[0], ne[1]]);
            wide_hit |= !own.is_empty();
            &own
        };
        if !near.is_empty() {
            let cap = near.iter().filter_map(|s| s.cap_over(ring.iter().map(|p| frame.ll(*p)))).fold(f64::INFINITY, f64::min);
            if cap.is_finite() {
                let base = mesh_base(w, &ring, &mut |p| frame.ll(p), &mut px);
                let allowed = cap - base - BASE_MARGIN_M;
                if allowed < w.params.airport.approach.min_building_m {
                    return;
                }
                if height > allowed {
                    height = allowed;
                    floors = (allowed / floor_m).floor().max(1.0);
                    capped = true;
                }
            }
        }
        let mut ring = ring;
        if signed_area(&ring) < 0.0 {
            ring.reverse();
        }
        out.push(PlanBuilding { id, kind: kind.to_string(), height_m: height as f32, levels: floors as f32, capped, ring: ring.iter().map(|p| inp.rel(*p)).collect() });
    };

    for lot in &lots.list {
        let b = &net.blocks[lot.block as usize];
        let (c, h) = (&b.c, lot.h);
        match lot.shape {
            LotShape::Civic => {
                let q = |s: f64, t: f64| bilinear(c, s, t);
                let (sx, sy) = (20.0 / b.lu, 12.0 / b.lv);
                let ring = vec![q(0.5 - sx, 0.5 - sy), q(0.5 + sx, 0.5 - sy), q(0.5 + sx, 0.5 + sy), q(0.5 - sx, 0.5 + sy)];
                let (kind, floors) = if unit(h) < 0.5 { ("school", 3.0) } else { ("kindergarten", 2.0) };
                make((1u64 << 40) | (h >> 24), kind, floors, ring, &mut out);
            }
            LotShape::Row { a, front: b_front, mid: b_mid, sign } => {
                let long_u = b.lu >= b.lv;
                let (ll_m, ls_m) = if long_u { (b.lu, b.lv) } else { (b.lv, b.lu) };
                let to_st = |a: f64, b: f64| if long_u { (a, b) } else { (b, a) };
                let gap = s.side_gap_m * 0.5 / ll_m;
                let (a0, a1) = (a[0] + gap, a[1] - gap);
                let depth = s.depth_m[0] + (s.depth_m[1] - s.depth_m[0]) * unit(crate::seed_hash!(Seed(h), "depth"));
                let bn = b_front + sign * s.setback_m / ls_m;
                let mut bf = b_front + sign * (s.setback_m + depth) / ls_m;
                bf = if sign > 0.0 { bf.min(b_mid) } else { bf.max(b_mid) };
                let (wid, dep) = ((a1 - a0) * ll_m, (bf - bn) * sign * ls_m);
                if wid < 4.0 || dep < 4.0 {
                    continue;
                }
                let p = |a: f64, b: f64| {
                    let (s, t) = to_st(a, b);
                    bilinear(c, s, t)
                };
                let ring = if wid >= 10.0 && dep >= 10.0 && unit(crate::seed_hash!(Seed(h), "shape")) < s.l_share {
                    let (am, bm) = ((a0 + a1) * 0.5, (bn + bf) * 0.5);
                    vec![p(a0, bn), p(a1, bn), p(a1, bm), p(am, bm), p(am, bf), p(a0, bf)]
                } else {
                    vec![p(a0, bn), p(a1, bn), p(a1, bf), p(a0, bf)]
                };
                let kind = &s.kinds[((unit(crate::seed_hash!(Seed(h), "kind")) * s.kinds.len() as f64) as usize).min(s.kinds.len() - 1)];
                let (fmin, fmax) = (s.floors[0].min(s.floors[1]), s.floors[0].max(s.floors[1]));
                let u = unit(crate::seed_hash!(Seed(h), "floors")).max(1e-12);
                let mut floors = fmin + (fmax - fmin) * exp(s.height_exp * ln(u));
                if s.height_exp > 1.0 {
                    let cen = bilinear(c, 0.5, 0.5);
                    let t = (len(cen, d.origin) / (1.5 * plan.grid.cell_m)).clamp(0.0, 1.0);
                    floors = fmin + (floors - fmin) * (1.0 - 0.5 * t);
                }
                make((1u64 << 40) | (h >> 24), kind, floors, ring, &mut out);
            }
        }
    }
    // An old town's landmarks: its most central buildings in the first cell.
    // Not under an approach surface: a landmark is taller than the cap there.
    if d.landmarks > 0 && d.cells.first() == Some(&inp.a) && approach.is_empty() && !wide_hit {
        let mut idx: Vec<usize> = (0..out.len()).collect();
        let dist2 = |b: &PlanBuilding| {
            let n = b.ring.len() as f32;
            let (x, y) = (b.ring.iter().map(|p| p[0]).sum::<f32>() / n, b.ring.iter().map(|p| p[1]).sum::<f32>() / n);
            x * x + y * y
        };
        idx.sort_by(|&x, &y| dist2(&out[x]).total_cmp(&dist2(&out[y])).then(out[x].id.cmp(&out[y].id)));
        let [hmin, hmax] = cp.landmark_height_m;
        for (n, &bi) in idx.iter().take(d.landmarks as usize).enumerate() {
            let b = &mut out[bi];
            if b.capped {
                continue;
            }
            b.kind = if n % 2 == 0 { "church" } else { "tower" }.to_string();
            b.height_m = (hmin + (hmax - hmin) * unit(crate::seed_hash!(d.id, "landmark", n))) as f32;
            b.levels = (b.height_m as f64 / floor_m).floor().max(1.0) as f32;
        }
    }
    out.sort_by_key(|b| b.id);
    out
}
