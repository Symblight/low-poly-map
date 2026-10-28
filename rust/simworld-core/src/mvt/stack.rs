//! Raised building parts stand on the building under them ([`stack_raised_parts`]).
//!
//! With more than a few raised parts the grounded footprints go into a uniform grid of their
//! bounding boxes, so each raised part tests only the footprints whose box holds its centroid
//! (about O(n) for a tile instead of raised x grounded polygon tests: whole z14 data tiles carry
//! thousands of buildings). A few raised parts (the common case) scan the footprints directly,
//! which is cheaper than building the grid. Both visit the candidates in ascending index order,
//! so they fold the same bases in the same order.

use super::{point_in_rings, Building};

/// Height (m) above which a part counts as raised.
const RAISED_MIN_HEIGHT_M: f32 = 0.25;
/// Up to this many raised parts the footprints are scanned without the grid.
const DIRECT_SCAN_MAX_RAISED: usize = 4;

/// Raised parts (`min_height` > 0.25 m) stand on the building under them (api 1.7): a raised
/// part whose centroid lies inside the footprint of a building that stands on the ground takes
/// that building's base (the highest such base when several overlap), so its bottom stays
/// `min_height` above the lower part's base like its roof stays `min_height` above it, instead
/// of following the (smaller) part footprint's own highest point. A raised part over nothing
/// keeps its own base. No-op with the terrain off (every base is 0).
pub fn stack_raised_parts(buildings: &mut [Building]) {
    let raised: Vec<usize> = (0..buildings.len()).filter(|&i| is_raised(&buildings[i])).collect();
    if raised.is_empty() {
        return;
    }
    let grid = (raised.len() > DIRECT_SCAN_MAX_RAISED).then(|| BoxGrid::new(buildings));
    for i in raised {
        let c = buildings[i].centroid_px;
        let mut best: Option<f32> = None;
        let mut consider = |o: &Building| {
            if !is_raised(o) && o.polygons.iter().any(|p| point_in_rings(c, p)) {
                best = Some(best.map_or(o.base_m, |b| b.max(o.base_m)));
            }
        };
        match &grid {
            Some(g) => g.cell_at(c).iter().filter(|&&j| g.boxes[j as usize].contains(c)).for_each(|&j| consider(&buildings[j as usize])),
            None => buildings.iter().for_each(&mut consider),
        }
        if let Some(b) = best {
            buildings[i].base_m = b;
        }
    }
}

/// A raised part (`min_height` > 0.25 m; a NaN `min_height` counts as grounded).
fn is_raised(b: &Building) -> bool {
    b.min_height > RAISED_MIN_HEIGHT_M
}

/// Bounding box in tile pixels (inclusive). `point_in_rings` is false outside it.
#[derive(Clone, Copy, Debug)]
struct PxBox {
    min: [f64; 2],
    max: [f64; 2],
}

impl PxBox {
    const EMPTY: PxBox = PxBox { min: [f64::INFINITY; 2], max: [f64::NEG_INFINITY; 2] };

    fn of(b: &Building) -> PxBox {
        let mut r = Self::EMPTY;
        for p in b.polygons.iter().flatten().flatten() {
            r.add(&PxBox { min: *p, max: *p });
        }
        r
    }

    /// Grow to hold `o`.
    fn add(&mut self, o: &PxBox) {
        for a in 0..2 {
            self.min[a] = self.min[a].min(o.min[a]);
            self.max[a] = self.max[a].max(o.max[a]);
        }
    }

    fn is_empty(&self) -> bool {
        !(self.min[0] <= self.max[0] && self.min[1] <= self.max[1])
    }

    fn contains(&self, p: [f64; 2]) -> bool {
        p[0] >= self.min[0] && p[0] <= self.max[0] && p[1] >= self.min[1] && p[1] <= self.max[1]
    }
}

/// Uniform grid over the boxes of the grounded buildings: each cell lists the buildings whose
/// box overlaps it, in ascending index order (one flat list, `start[c]..start[c + 1]`).
struct BoxGrid {
    boxes: Vec<PxBox>,
    origin: [f64; 2],
    cell: [f64; 2],
    n: usize,
    start: Vec<u32>,
    items: Vec<u32>,
}

impl BoxGrid {
    fn new(buildings: &[Building]) -> Self {
        let mut all = PxBox::EMPTY;
        let boxes: Vec<PxBox> = buildings
            .iter()
            .map(|b| {
                if is_raised(b) {
                    return PxBox::EMPTY; // raised parts are never a base
                }
                let r = PxBox::of(b);
                if !r.is_empty() {
                    all.add(&r);
                }
                r
            })
            .collect();
        let grounded = boxes.iter().filter(|r| !r.is_empty()).count();
        let n = ((grounded as f64).sqrt().ceil() as usize).clamp(1, 256);
        let mut g = BoxGrid { boxes, origin: all.min, cell: [1.0; 2], n, start: vec![0; n * n + 1], items: Vec::new() };
        if all.is_empty() {
            return g;
        }
        g.cell = [0, 1].map(|a| ((all.max[a] - all.min[a]) / n as f64).max(f64::MIN_POSITIVE));
        // Counting pass, prefix sums, then the fill pass (ascending building index per cell).
        let mut start = vec![0u32; n * n + 1];
        g.for_each_cell(|c, _| start[c + 1] += 1);
        for c in 0..n * n {
            start[c + 1] += start[c];
        }
        let mut next = start.clone();
        let mut items = vec![0u32; start[n * n] as usize];
        g.for_each_cell(|c, j| {
            items[next[c] as usize] = j as u32;
            next[c] += 1;
        });
        g.start = start;
        g.items = items;
        g
    }

    /// `f(cell, building)` for every cell each grounded box overlaps, buildings ascending.
    fn for_each_cell(&self, mut f: impl FnMut(usize, usize)) {
        for (j, r) in self.boxes.iter().enumerate() {
            if r.is_empty() {
                continue;
            }
            let (x0, y0) = (self.index(r.min[0], 0), self.index(r.min[1], 1));
            let (x1, y1) = (self.index(r.max[0], 0), self.index(r.max[1], 1));
            for y in y0..=y1 {
                for x in x0..=x1 {
                    f(y * self.n + x, j);
                }
            }
        }
    }

    /// Cell index of coordinate `v` on axis `a` (clamped to the grid).
    fn index(&self, v: f64, a: usize) -> usize {
        (((v - self.origin[a]) / self.cell[a]).floor().max(0.0) as usize).min(self.n - 1)
    }

    /// The buildings whose box may hold `p` (empty for a non-finite point).
    fn cell_at(&self, p: [f64; 2]) -> &[u32] {
        if !(p[0].is_finite() && p[1].is_finite()) {
            return &[];
        }
        let c = self.index(p[1], 1) * self.n + self.index(p[0], 0);
        &self.items[self.start[c] as usize..self.start[c + 1] as usize]
    }
}

#[cfg(test)]
mod tests;
