//! The strip rule of the height function (section 5.1 step 9).

use super::{AirportGeo, LocalFrame, Obb};
use crate::terrain::noise::smoothstep;

/// One airport's part of the strip rule.
#[derive(Clone, Debug, PartialEq)]
struct StripAirport {
    frame: LocalFrame,
    fence: Obb,
    runways: Vec<StripRunway>,
    /// lat/lon box [w, s, e, n] of the fence plus the shoulder (the strip rule's reach).
    bbox: [f64; 4],
}

#[derive(Clone, Debug, PartialEq)]
struct StripRunway {
    a: [f64; 2],
    dir: [f64; 2],
    length_m: f64,
    half_m: f64,
    le_thr_s: f64,
    he_thr_s: f64,
    le_thr_h: f64,
    he_thr_h: f64,
    /// Index (in the airport's runways) of the runway whose profile gives this strip its
    /// height: itself, or the longest runway of its group of overlapping strips.
    plane: usize,
}

/// Per runway of `g` (same order), the index of the runway whose centreline profile levels its
/// strip: itself, or the longest runway of its group of overlapping strips (see
/// [`StripIndex`]). Height there: `g.runways[i].height_at(along-axis distance on runway i)`.
pub fn runway_planes(g: &AirportGeo) -> Vec<usize> {
    let mut v: Vec<StripRunway> = g
        .runways
        .iter()
        .map(|r| StripRunway {
            a: r.a,
            dir: r.dir,
            length_m: r.length_m,
            half_m: r.strip_half_m,
            le_thr_s: r.le_thr_s,
            he_thr_s: r.he_thr_s,
            le_thr_h: r.le_thr_h,
            he_thr_h: r.he_thr_h,
            plane: 0,
        })
        .collect();
    group_planes(&mut v);
    v.iter().map(|r| r.plane).collect()
}

impl StripRunway {
    /// Along-axis distance of plane point `p` from the le end.
    #[inline]
    fn s_of(&self, p: [f64; 2]) -> f64 {
        let q = [p[0] - self.a[0], p[1] - self.a[1]];
        q[0] * self.dir[0] + q[1] * self.dir[1]
    }

    /// Strip rectangle corners (s in [0, length], |t| <= half), grown by `pad` metres.
    fn corners(&self, pad: f64) -> [[f64; 2]; 4] {
        let v = [-self.dir[1], self.dir[0]];
        let at = |s: f64, t: f64| [self.a[0] + self.dir[0] * s + v[0] * t, self.a[1] + self.dir[1] * s + v[1] * t];
        let (s0, s1, h) = (-pad, self.length_m + pad, self.half_m + pad);
        [at(s0, -h), at(s1, -h), at(s1, h), at(s0, h)]
    }
}

/// Whether two strip rectangles overlap or come within `pad` (separating-axis test).
fn strips_touch(x: &StripRunway, y: &StripRunway, pad: f64) -> bool {
    let (cx, cy) = (x.corners(pad * 0.5), y.corners(pad * 0.5));
    let axes = [x.dir, [-x.dir[1], x.dir[0]], y.dir, [-y.dir[1], y.dir[0]]];
    axes.iter().all(|ax| {
        let proj = |c: &[[f64; 2]; 4]| {
            c.iter().map(|p| p[0] * ax[0] + p[1] * ax[1]).fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| (lo.min(v), hi.max(v)))
        };
        let ((a0, a1), (b0, b1)) = (proj(&cx), proj(&cy));
        a0 <= b1 && b0 <= a1
    })
}

/// Group the strips of one airport that overlap (or come within 1 m): each strip takes the
/// profile of its group's longest runway (ties: the first key), so the height function is
/// continuous over crossing / close parallel runways and approximate runways stacked on the
/// reference point (S1 fix; before, the first key won and the height stepped across a runway).
fn group_planes(runways: &mut [StripRunway]) {
    let n = runways.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while p[r] != r {
            r = p[r];
        }
        p[i] = r;
        r
    }
    for i in 0..n {
        for j in i + 1..n {
            if strips_touch(&runways[i], &runways[j], 1.0) {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                if a != b {
                    parent[a.max(b)] = a.min(b);
                }
            }
        }
    }
    for i in 0..n {
        let root = find(&mut parent, i);
        // Longest member of the group (ties: the lowest index = first key).
        let mut best = root;
        for j in 0..n {
            if find(&mut parent, j) == root && runways[j].length_m > runways[best].length_m + 1e-9 {
                best = j;
            }
        }
        runways[i].plane = best;
    }
}

impl StripRunway {
    fn height_at(&self, s: f64) -> f64 {
        let s = s.clamp(0.0, self.length_m);
        let span = self.he_thr_s - self.le_thr_s;
        if span <= 1.0 {
            return (self.le_thr_h + self.he_thr_h) * 0.5;
        }
        self.le_thr_h + (self.he_thr_h - self.le_thr_h) * ((s - self.le_thr_s) / span)
    }
}

/// Grid cell size of the strip index (degrees).
const CELL_DEG: f64 = 0.02;

/// Every airport's runway strips with a coarse lat/lon grid, so a sample outside airports costs a
/// bounds check and one lookup.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StripIndex {
    airports: Vec<StripAirport>,
    shoulder_m: f64,
    /// Grid origin (west, south) and size.
    w: f64,
    s: f64,
    nx: usize,
    ny: usize,
    /// CSR: per cell the range of `items` (airport indices, ascending).
    start: Vec<u32>,
    items: Vec<u32>,
}

impl StripIndex {
    pub fn new(airports: &[AirportGeo], shoulder_m: f64) -> Self {
        let list: Vec<(&AirportGeo, StripAirport)> = airports
            .iter()
            .filter(|g| !g.runways.is_empty())
            .map(|g| {
                let mut runways: Vec<StripRunway> = g
                    .runways
                    .iter()
                    .map(|r| StripRunway {
                        a: r.a,
                        dir: r.dir,
                        length_m: r.length_m,
                        half_m: r.strip_half_m,
                        le_thr_s: r.le_thr_s,
                        he_thr_s: r.he_thr_s,
                        le_thr_h: r.le_thr_h,
                        he_thr_h: r.he_thr_h,
                        plane: 0,
                    })
                    .collect();
                group_planes(&mut runways);
                (g, StripAirport { frame: g.frame, fence: g.fence, runways, bbox: g.terrain_bbox })
            })
            .collect();
        if list.is_empty() {
            return Self { shoulder_m, ..Default::default() };
        }
        let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for (g, _) in &list {
            let t = g.terrain_bbox;
            b = [b[0].min(t[0]), b[1].min(t[1]), b[2].max(t[2]), b[3].max(t[3])];
        }
        let w = (b[0] / CELL_DEG).floor() * CELL_DEG;
        let s = (b[1] / CELL_DEG).floor() * CELL_DEG;
        let nx = (((b[2] - w) / CELL_DEG).floor() as usize) + 1;
        let ny = (((b[3] - s) / CELL_DEG).floor() as usize) + 1;
        let mut per: Vec<Vec<u32>> = vec![Vec::new(); nx * ny];
        for (i, (g, _)) in list.iter().enumerate() {
            let t = g.terrain_bbox;
            let (x0, x1) = (((t[0] - w) / CELL_DEG).floor() as usize, (((t[2] - w) / CELL_DEG).floor() as usize).min(nx - 1));
            let (y0, y1) = (((t[1] - s) / CELL_DEG).floor() as usize, (((t[3] - s) / CELL_DEG).floor() as usize).min(ny - 1));
            for y in y0..=y1 {
                for x in x0..=x1 {
                    per[y * nx + x].push(i as u32);
                }
            }
        }
        let mut start = Vec::with_capacity(nx * ny + 1);
        let mut items = Vec::new();
        start.push(0);
        for v in per {
            items.extend(v);
            start.push(items.len() as u32);
        }
        Self { airports: list.into_iter().map(|(_, a)| a).collect(), shoulder_m, w, s, nx, ny, start, items }
    }

    pub fn is_empty(&self) -> bool {
        self.airports.is_empty()
    }

    /// Bytes of the index (section 10a budget).
    pub fn bytes(&self) -> usize {
        self.start.len() * 4 + self.items.len() * 4 + self.airports.iter().map(|a| 128 + a.runways.len() * 80).sum::<usize>()
    }

    /// Airport indices whose terrain box covers the point's grid cell.
    #[inline]
    fn candidates(&self, lat: f64, lon: f64) -> &[u32] {
        if self.nx == 0 {
            return &[];
        }
        let fx = (lon - self.w) / CELL_DEG;
        let fy = (lat - self.s) / CELL_DEG;
        if !(fx >= 0.0 && fy >= 0.0 && fx < self.nx as f64 && fy < self.ny as f64) {
            return &[];
        }
        let c = fy as usize * self.nx + fx as usize;
        &self.items[self.start[c] as usize..self.start[c + 1] as usize]
    }

    /// The strip rule at a point: `Some((strip height, k))` with `k` the weight of the
    /// surrounding terrain (0 inside a fence, smoothstep to 1 at `shoulder_m` outside it);
    /// `None` = no airport influence. Several airports: the nearest fence (ties: the first by
    /// ident).
    #[inline]
    pub fn at(&self, lat: f64, lon: f64) -> Option<(f64, f64)> {
        let c = self.candidates(lat, lon);
        if c.is_empty() {
            return None;
        }
        let mut best: Option<(f64, f64)> = None;
        let mut best_d = f64::INFINITY;
        for &i in c {
            let ap = &self.airports[i as usize];
            let p = ap.frame.xy(lat, lon);
            let d = ap.fence.distance(p);
            if d >= self.shoulder_m && !(d == 0.0 && self.shoulder_m == 0.0) || d >= best_d {
                continue;
            }
            // Nearest strip (distance to its rectangle; ties: the earlier = lower key).
            let (mut h, mut sd) = (0.0, f64::INFINITY);
            for r in &ap.runways {
                let q = [p[0] - r.a[0], p[1] - r.a[1]];
                let s = q[0] * r.dir[0] + q[1] * r.dir[1];
                let t = -q[0] * r.dir[1] + q[1] * r.dir[0];
                let ds = (-s).max(s - r.length_m).max(0.0);
                let dt = (t.abs() - r.half_m).max(0.0);
                let dd = ds * ds + dt * dt;
                if dd < sd {
                    sd = dd;
                    let pl = &ap.runways[r.plane];
                    h = if std::ptr::eq(pl, r) { r.height_at(s) } else { pl.height_at(pl.s_of(p)) };
                }
            }
            best_d = d;
            best = Some((h, if d <= 0.0 { 0.0 } else { smoothstep(0.0, self.shoulder_m, d) }));
        }
        best
    }

    /// Drape-lift weight at a point (contract section 9 "Airports"): the strip rule's terrain
    /// weight `k` (0 inside a fence, smoothstep to 1 at `shoulder_m` outside it), 1 beyond every
    /// airport's reach. The drawn ground and the terrain mesh tolerance both scale by it.
    #[inline]
    pub fn lift_weight(&self, lat: f64, lon: f64) -> f64 {
        self.at(lat, lon).map_or(1.0, |(_, k)| k)
    }

    /// Whether a lat/lon box [w, s, e, n] overlaps any airport's fence + shoulder box (outside,
    /// [`StripIndex::lift_weight`] is 1 everywhere and the plain drape applies).
    pub fn touches(&self, b: [f64; 4]) -> bool {
        self.airports.iter().any(|a| a.bbox[0] <= b[2] && b[0] <= a.bbox[2] && a.bbox[1] <= b[3] && b[1] <= a.bbox[3])
    }

    /// Whether the point is inside an airport fence (ground class 7 on the terrain).
    pub fn in_fence(&self, lat: f64, lon: f64) -> bool {
        self.candidates(lat, lon).iter().any(|&i| {
            let ap = &self.airports[i as usize];
            ap.fence.distance(ap.frame.xy(lat, lon)) <= 0.0
        })
    }
}
