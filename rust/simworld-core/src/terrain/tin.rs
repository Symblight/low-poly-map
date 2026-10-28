//! Terrain mesh (M9 phase 2, `docs/spec-terrain.md` 5.3): a greedy-insertion TIN over the
//! tile's height patch, skirts, optional smooth normals.
//!
//! [`Delatin`] is a vendored port of Mapbox's `delatin` (ISC licence, (c) Vladimir Agafonkin;
//! after Michael Fogleman's `hmm`; Garland & Heckbert 1995, "Fast Polygonal Approximation of
//! Terrains and Height Fields"): a Delaunay triangulation of a regular height grid that
//! repeatedly inserts the grid point with the largest vertical error until the error is below
//! a bound. Changes against the original: `f32` heights, exact integer geometry (`i64`
//! orientation and in-circle tests on grid indices), a triangle cap, a start from a regular
//! seed grid (the curvature seeds), an explicit stack instead of recursion, and a total order on
//! the priority queue (error, then triangle id), so the output is a pure function of the input.
//!
//! Building a terrain tile from it (patch -> TIN -> sink -> vertices -> skirts) is
//! [`build_terrain_tile`] in `tin_build.rs`, re-exported here.

#[path = "tin_build.rs"]
mod build;
pub use build::*;

/// Bisection steps of the closing pass of [`Delatin::vertex_sinks`] (`E_t / 2^24`: below f32).
const SINK_BISECTIONS: u32 = 24;

/// Exact orientation of three grid points (> 0 for the triangles Delatin builds).
#[inline]
fn orient(a: [i64; 2], b: [i64; 2], c: [i64; 2]) -> i64 {
    (b[0] - c[0]) * (a[1] - c[1]) - (b[1] - c[1]) * (a[0] - c[0])
}

/// Exact in-circle test: `p` strictly inside the circumcircle of `a, b, c`.
#[inline]
fn in_circle(a: [i64; 2], b: [i64; 2], c: [i64; 2], p: [i64; 2]) -> bool {
    let (dx, dy) = (a[0] - p[0], a[1] - p[1]);
    let (ex, ey) = (b[0] - p[0], b[1] - p[1]);
    let (fx, fy) = (c[0] - p[0], c[1] - p[1]);
    let ap = dx * dx + dy * dy;
    let bp = ex * ex + ey * ey;
    let cp = fx * fx + fy * fy;
    dx * (ey * cp - bp * fy) - dy * (ex * cp - bp * fx) + ap * (ex * fy - ey * fx) < 0
}

/// Greedy-insertion TIN over a `width x height` grid of `f32` heights (row-major).
pub struct Delatin<'a> {
    h: &'a [f32],
    /// Optional per-sample error scale (row-major like `h`): a sample's error counts
    /// `scale` times (a seed world's airports: 1 / the lift weight, contract section 9
    /// "Airports"). `None` = the original metric, bit for bit.
    scale: Option<&'a [f32]>,
    width: usize,
    /// Vertex grid positions.
    coords: Vec<[i64; 2]>,
    /// 3 vertex indices per triangle (slots are reused by flips; all are live).
    triangles: Vec<u32>,
    /// Opposite halfedge per halfedge, -1 on the hull.
    halfedges: Vec<i32>,
    /// Candidate point (largest error) per triangle.
    candidates: Vec<[i64; 2]>,
    /// Triangle -> heap position (-1 = not in the heap).
    queue_index: Vec<i32>,
    /// Heap of triangles by error (largest first, ties by smaller triangle id).
    queue: Vec<u32>,
    errors: Vec<f64>,
    /// Triangles waiting for their candidate.
    pending: Vec<u32>,
    stack: Vec<usize>,
}

impl<'a> Delatin<'a> {
    /// Triangulate the regular seed grid `xs` x `ys` (sorted grid indices including the first and
    /// last column / row). `xs = [0, w-1]`, `ys = [0, h-1]` is the original two-triangle start.
    pub fn new(h: &'a [f32], width: usize, height: usize, xs: &[usize], ys: &[usize]) -> Self {
        Self::new_scaled(h, None, width, height, xs, ys)
    }

    /// [`Delatin::new`] with a per-sample error scale (see [`Delatin`] `scale`).
    pub fn new_scaled(h: &'a [f32], scale: Option<&'a [f32]>, width: usize, height: usize, xs: &[usize], ys: &[usize]) -> Self {
        assert!(width >= 2 && height >= 2 && h.len() >= width * height);
        assert!(scale.is_none_or(|s| s.len() >= width * height));
        let mut d = Self {
            h,
            scale,
            width,
            coords: Vec::new(),
            triangles: Vec::new(),
            halfedges: Vec::new(),
            candidates: Vec::new(),
            queue_index: Vec::new(),
            queue: Vec::new(),
            errors: Vec::new(),
            pending: Vec::new(),
            stack: Vec::new(),
        };
        let (nx, ny) = (xs.len(), ys.len());
        for &y in ys {
            for &x in xs {
                d.coords.push([x as i64, y as i64]);
            }
        }
        let id = |i: usize, j: usize| (j * nx + i) as u32;
        // Directed edge (from, to) -> halfedge, to link the opposite sides.
        let mut edges: ahash::AHashMap<(u32, u32), usize> = ahash::AHashMap::new();
        for j in 0..ny - 1 {
            for i in 0..nx - 1 {
                let (p0, p1, p2, p3) = (id(i, j), id(i + 1, j), id(i, j + 1), id(i + 1, j + 1));
                // Same split and orientation as Delatin's start: (p3, p0, p2) and (p0, p3, p1).
                for tri in [[p3, p0, p2], [p0, p3, p1]] {
                    let e = d.add_triangle(tri[0], tri[1], tri[2], -1, -1, -1, None);
                    for k in 0..3 {
                        let (a, b) = (tri[k], tri[(k + 1) % 3]);
                        if let Some(&o) = edges.get(&(b, a)) {
                            d.halfedges[e + k] = o as i32;
                            d.halfedges[o] = (e + k) as i32;
                        }
                        edges.insert((a, b), e + k);
                    }
                }
            }
        }
        d.flush();
        d
    }

    fn height_at(&self, p: [i64; 2]) -> f64 {
        self.h[p[1] as usize * self.width + p[0] as usize] as f64
    }

    /// Insert points until the largest error is at most `max_error` or another step could
    /// exceed `max_triangles` (each step adds at most 2 triangles). Returns the largest
    /// remaining error.
    pub fn run(&mut self, max_error: f64, max_triangles: usize) -> f64 {
        while !self.queue.is_empty() && self.errors[0] > max_error && self.triangle_count() + 2 <= max_triangles {
            self.step();
            self.flush();
        }
        self.max_error()
    }

    pub fn max_error(&self) -> f64 {
        self.errors.first().copied().unwrap_or(0.0)
    }

    pub fn triangle_count(&self) -> usize {
        self.triangles.len() / 3
    }

    pub fn coords(&self) -> &[[i64; 2]] {
        &self.coords
    }

    pub fn triangles(&self) -> &[u32] {
        &self.triangles
    }

    pub fn halfedges(&self) -> &[i32] {
        &self.halfedges
    }

    fn flush(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        for &t in &pending {
            let t = t as usize;
            let a = self.coords[self.triangles[t * 3] as usize];
            let b = self.coords[self.triangles[t * 3 + 1] as usize];
            let c = self.coords[self.triangles[t * 3 + 2] as usize];
            self.find_candidate(a, b, c, t);
        }
        self.pending = pending;
        self.pending.clear();
    }

    /// Rasterize the triangle over the grid; the point with the largest vertical error to the
    /// triangle's plane becomes its candidate.
    fn find_candidate(&mut self, p0: [i64; 2], p1: [i64; 2], p2: [i64; 2], t: usize) {
        let min_x = p0[0].min(p1[0]).min(p2[0]);
        let min_y = p0[1].min(p1[1]).min(p2[1]);
        let max_x = p0[0].max(p1[0]).max(p2[0]);
        let max_y = p0[1].max(p1[1]).max(p2[1]);
        let m = [min_x, min_y];
        let mut w00 = orient(p1, p2, m);
        let mut w01 = orient(p2, p0, m);
        let mut w02 = orient(p0, p1, m);
        let (a01, b01) = (p1[1] - p0[1], p0[0] - p1[0]);
        let (a12, b12) = (p2[1] - p1[1], p1[0] - p2[0]);
        let (a20, b20) = (p0[1] - p2[1], p2[0] - p0[0]);
        let a = orient(p0, p1, p2) as f64;
        let z0 = self.height_at(p0) / a;
        let z1 = self.height_at(p1) / a;
        let z2 = self.height_at(p2) / a;
        let mut max_error = 0.0f64;
        let mut best = p0;
        for y in min_y..=max_y {
            // Skip to the first column inside the triangle.
            let mut dx = 0i64;
            if w00 < 0 && a12 > 0 {
                dx = dx.max(-w00 / a12);
            }
            if w01 < 0 && a20 > 0 {
                dx = dx.max(-w01 / a20);
            }
            if w02 < 0 && a01 > 0 {
                dx = dx.max(-w02 / a01);
            }
            let mut w0 = w00 + a12 * dx;
            let mut w1 = w01 + a20 * dx;
            let mut w2 = w02 + a01 * dx;
            let mut was_inside = false;
            let row = y as usize * self.width;
            for x in (min_x + dx)..=max_x {
                if w0 >= 0 && w1 >= 0 && w2 >= 0 {
                    was_inside = true;
                    let z = z0 * w0 as f64 + z1 * w1 as f64 + z2 * w2 as f64;
                    let mut dz = (z - self.h[row + x as usize] as f64).abs();
                    if let Some(sc) = self.scale {
                        dz *= sc[row + x as usize] as f64;
                    }
                    if dz > max_error {
                        max_error = dz;
                        best = [x, y];
                    }
                } else if was_inside {
                    break;
                }
                w0 += a12;
                w1 += a20;
                w2 += a01;
            }
            w00 += b12;
            w01 += b20;
            w02 += b01;
        }
        if best == p0 || best == p1 || best == p2 {
            max_error = 0.0;
        }
        self.candidates[t] = best;
        self.queue_push(t, max_error);
    }

    /// Per-vertex sink (m) that puts the TIN on or under the grid heights (api 1.7, contract
    /// section 9a). Three passes over the triangles in index order, each over the grid samples
    /// inside the triangle (edges included, raster order); `e` = the excess of the triangle's
    /// plane through its unsunk vertices over a sample:
    ///
    /// 1. **Cap:** `E_t` = the largest excess in triangle `t`; a vertex may sink at most the
    ///    largest `E_t` of its triangles (`cap`), which is at most the TIN's largest error, so a
    ///    sunk vertex lies within twice the error under the exact height.
    /// 2. **Spread:** where the sinks so far lower the plane at a sample by less than `e`, the
    ///    missing part `d` goes to the triangle's free vertices in proportion to their
    ///    barycentric weight `w` (`d x w_i / sum w^2`, the smallest change that closes it), each
    ///    clamped to its cap. A sample next to one vertex mostly sinks that vertex.
    /// 3. **Close:** a triangle with a sample still above the sunk plane (a clamp bit) raises
    ///    the sinks of its free vertices to a common floor `u`, the smallest that closes every
    ///    sample in it (bisection in [0, `E_t`]; `u = E_t` closes a triangle without a pinned
    ///    corner, and every cap is at least `E_t`).
    ///
    /// Sinks only grow, so a sample settled earlier stays settled: every triangle without a
    /// pinned vertex lies on or under all its samples. `pinned` vertices (a seed world's sea
    /// and lake vertices, exactly at the water level) never sink; their triangles (the shore)
    /// may keep up to the plain TIN error above a sample. Deterministic.
    pub fn vertex_sinks(&self, pinned: Option<&[bool]>) -> Vec<f64> {
        let n = self.coords.len();
        let free = |i: usize| !pinned.is_some_and(|p| p[i]);
        let tri_count = self.triangle_count();
        // Pass 1: largest excess per triangle, cap per vertex.
        let mut tri_excess = vec![0.0f64; tri_count];
        let mut cap = vec![0.0f64; n];
        for (t, slot) in tri_excess.iter_mut().enumerate() {
            let Some((ids, _)) = self.sink_triangle(t) else { continue };
            let mut ex = 0.0f64;
            self.for_each_sample(t, |_, e| ex = ex.max(e));
            *slot = ex;
            for &i in &ids {
                if free(i) {
                    cap[i] = cap[i].max(ex);
                }
            }
        }
        // Pass 2: spread the missing part by barycentric weight, clamped to the caps. Sinks only
        // grow, so a triangle can still be open after this pass only if a clamp (or a pinned
        // corner) bit while it was processed.
        let mut sink = vec![0.0f64; n];
        let mut clamped = vec![false; tri_count];
        for t in 0..tri_count {
            let Some((ids, _)) = self.sink_triangle(t) else { continue };
            if tri_excess[t] <= 0.0 {
                continue;
            }
            let mut bit = false;
            self.for_each_sample(t, |w, e| {
                if e <= 0.0 {
                    return;
                }
                let d = e - (w[0] * sink[ids[0]] + w[1] * sink[ids[1]] + w[2] * sink[ids[2]]);
                if d <= 0.0 {
                    return;
                }
                let wf = [0, 1, 2].map(|k| if free(ids[k]) { w[k] } else { 0.0 });
                let n2 = wf[0] * wf[0] + wf[1] * wf[1] + wf[2] * wf[2];
                if n2 <= 0.0 {
                    bit = true;
                    return;
                }
                let k = d / n2;
                for i in 0..3 {
                    if wf[i] > 0.0 {
                        let want = sink[ids[i]] + k * wf[i];
                        if want > cap[ids[i]] {
                            bit = true;
                        }
                        sink[ids[i]] = want.min(cap[ids[i]]);
                    }
                }
            });
            clamped[t] = bit;
        }
        // Pass 3: close what the clamps left open.
        for t in 0..tri_count {
            let Some((ids, _)) = self.sink_triangle(t) else { continue };
            if !clamped[t] {
                continue;
            }
            let mut open = false;
            self.for_each_sample(t, |w, e| {
                open |= e > w[0] * sink[ids[0]] + w[1] * sink[ids[1]] + w[2] * sink[ids[2]];
            });
            if !open {
                continue;
            }
            // The smallest common floor `u` of the free sinks that closes every sample
            // (bisection; `u = E_t` closes unless a corner is pinned; every cap is at least `E_t`).
            let closes = |u: f64| {
                let s = ids.map(|i| if free(i) { sink[i].max(u) } else { sink[i] });
                let mut ok = true;
                self.for_each_sample(t, |w, e| ok &= e <= w[0] * s[0] + w[1] * s[1] + w[2] * s[2]);
                ok
            };
            let (mut lo, mut hi) = (0.0f64, tri_excess[t]);
            for _ in 0..SINK_BISECTIONS {
                let mid = 0.5 * (lo + hi);
                if closes(mid) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            for &i in &ids {
                if free(i) {
                    sink[i] = sink[i].max(hi);
                }
            }
        }
        sink
    }

    /// Vertex ids and doubled area of live triangle `t` (`None` if degenerate).
    fn sink_triangle(&self, t: usize) -> Option<([usize; 3], i64)> {
        let ids = [self.triangles[t * 3] as usize, self.triangles[t * 3 + 1] as usize, self.triangles[t * 3 + 2] as usize];
        let a = orient(self.coords[ids[0]], self.coords[ids[1]], self.coords[ids[2]]);
        (a > 0).then_some((ids, a))
    }

    /// Calls `f(barycentric weights, excess of the unsunk plane over the sample)` for every grid
    /// sample inside triangle `t` (edges included), rows then columns.
    fn for_each_sample(&self, t: usize, mut f: impl FnMut([f64; 3], f64)) {
        let Some((ids, a)) = self.sink_triangle(t) else { return };
        let af = a as f64;
        let (p0, p1, p2) = (self.coords[ids[0]], self.coords[ids[1]], self.coords[ids[2]]);
        let (h0, h1, h2) = (self.height_at(p0), self.height_at(p1), self.height_at(p2));
        for y in p0[1].min(p1[1]).min(p2[1])..=p0[1].max(p1[1]).max(p2[1]) {
            let row = y as usize * self.width;
            for x in p0[0].min(p1[0]).min(p2[0])..=p0[0].max(p1[0]).max(p2[0]) {
                let q = [x, y];
                let (w0, w1, w2) = (orient(p1, p2, q), orient(p2, p0, q), orient(p0, p1, q));
                if w0 < 0 || w1 < 0 || w2 < 0 {
                    continue;
                }
                let w = [w0 as f64 / af, w1 as f64 / af, w2 as f64 / af];
                let z = h0 * w[0] + h1 * w[1] + h2 * w[2];
                f(w, z - self.h[row + x as usize] as f64);
            }
        }
    }

    /// Insert the candidate of the triangle with the largest error.
    fn step(&mut self) {
        let t = self.queue_pop();
        let (e0, e1, e2) = (t * 3, t * 3 + 1, t * 3 + 2);
        let (p0, p1, p2) = (self.triangles[e0], self.triangles[e1], self.triangles[e2]);
        let (a, b, c) = (self.coords[p0 as usize], self.coords[p1 as usize], self.coords[p2 as usize]);
        let p = self.candidates[t];
        let pn = self.coords.len() as u32;
        self.coords.push(p);
        if orient(a, b, p) == 0 {
            self.handle_collinear(pn, e0);
        } else if orient(b, c, p) == 0 {
            self.handle_collinear(pn, e1);
        } else if orient(c, a, p) == 0 {
            self.handle_collinear(pn, e2);
        } else {
            let (h0, h1, h2) = (self.halfedges[e0], self.halfedges[e1], self.halfedges[e2]);
            let t0 = self.add_triangle(p0, p1, pn, h0, -1, -1, Some(e0));
            let t1 = self.add_triangle(p1, p2, pn, h1, -1, (t0 + 1) as i32, None);
            let t2 = self.add_triangle(p2, p0, pn, h2, (t0 + 2) as i32, (t1 + 1) as i32, None);
            self.legalize_all(&[t0, t1, t2]);
        }
    }

    /// Add (or overwrite at halfedge `e`) a triangle `a, b, c` with the given opposite halfedges.
    /// Returns its first halfedge.
    #[allow(clippy::too_many_arguments)]
    fn add_triangle(&mut self, a: u32, b: u32, c: u32, ab: i32, bc: i32, ca: i32, e: Option<usize>) -> usize {
        let e = e.unwrap_or(self.triangles.len());
        let t = e / 3;
        if e == self.triangles.len() {
            self.triangles.extend_from_slice(&[a, b, c]);
            self.halfedges.extend_from_slice(&[ab, bc, ca]);
            self.candidates.push([0, 0]);
            self.queue_index.push(-1);
        } else {
            self.triangles[e..e + 3].copy_from_slice(&[a, b, c]);
            self.halfedges[e..e + 3].copy_from_slice(&[ab, bc, ca]);
            self.candidates[t] = [0, 0];
            self.queue_index[t] = -1;
        }
        for (k, h) in [ab, bc, ca].into_iter().enumerate() {
            if h >= 0 {
                self.halfedges[h as usize] = (e + k) as i32;
            }
        }
        self.pending.push(t as u32);
        e
    }

    /// Legalize the given halfedges in order (depth first, as the recursive original).
    fn legalize_all(&mut self, edges: &[usize]) {
        self.stack.clear();
        self.stack.extend(edges.iter().rev());
        while let Some(a) = self.stack.pop() {
            self.legalize(a);
        }
    }

    /// Flip the edge `a` if the pair of triangles is not Delaunay; pushes the two new outer edges.
    fn legalize(&mut self, a: usize) {
        let b = self.halfedges[a];
        if b < 0 {
            return;
        }
        let b = b as usize;
        let (a0, b0) = (a - a % 3, b - b % 3);
        let (al, ar) = (a0 + (a + 1) % 3, a0 + (a + 2) % 3);
        let (bl, br) = (b0 + (b + 2) % 3, b0 + (b + 1) % 3);
        let (p0, pr, pl, p1) = (self.triangles[ar], self.triangles[a], self.triangles[al], self.triangles[bl]);
        let c = |i: u32| self.coords[i as usize];
        if !in_circle(c(p0), c(pr), c(pl), c(p1)) {
            return;
        }
        let (hal, har, hbl, hbr) = (self.halfedges[al], self.halfedges[ar], self.halfedges[bl], self.halfedges[br]);
        self.queue_remove(a0 / 3);
        self.queue_remove(b0 / 3);
        let t0 = self.add_triangle(p0, p1, pl, -1, hbl, hal, Some(a0));
        let t1 = self.add_triangle(p1, p0, pr, t0 as i32, har, hbr, Some(b0));
        // Recursion order: t0 + 1 completely, then t1 + 2.
        self.stack.push(t1 + 2);
        self.stack.push(t0 + 1);
    }

    /// The new point lies on the edge `a`: split the edge (and the triangle across it).
    fn handle_collinear(&mut self, pn: u32, a: usize) {
        let a0 = a - a % 3;
        let (al, ar) = (a0 + (a + 1) % 3, a0 + (a + 2) % 3);
        let (p0, pr, pl) = (self.triangles[ar], self.triangles[a], self.triangles[al]);
        let (hal, har) = (self.halfedges[al], self.halfedges[ar]);
        let b = self.halfedges[a];
        if b < 0 {
            let t0 = self.add_triangle(pn, p0, pr, -1, har, -1, Some(a0));
            let t1 = self.add_triangle(p0, pn, pl, t0 as i32, -1, hal, None);
            self.legalize_all(&[t0 + 1, t1 + 2]);
            return;
        }
        let b = b as usize;
        let b0 = b - b % 3;
        let (bl, br) = (b0 + (b + 2) % 3, b0 + (b + 1) % 3);
        let p1 = self.triangles[bl];
        let (hbl, hbr) = (self.halfedges[bl], self.halfedges[br]);
        self.queue_remove(b0 / 3);
        let t0 = self.add_triangle(p0, pr, pn, har, -1, -1, Some(a0));
        let t1 = self.add_triangle(pr, p1, pn, hbr, -1, (t0 + 1) as i32, Some(b0));
        let t2 = self.add_triangle(p1, pl, pn, hbl, -1, (t1 + 1) as i32, None);
        let t3 = self.add_triangle(pl, p0, pn, hal, (t0 + 2) as i32, (t2 + 1) as i32, None);
        self.legalize_all(&[t0, t1, t2, t3]);
    }

    // ---- priority queue (binary heap, largest error first, ties by smaller triangle id) ----

    fn queue_push(&mut self, t: usize, error: f64) {
        let i = self.queue.len();
        self.queue_index[t] = i as i32;
        self.queue.push(t as u32);
        self.errors.push(error);
        self.queue_up(i);
    }

    fn queue_pop(&mut self) -> usize {
        let n = self.queue.len() - 1;
        self.queue_swap(0, n);
        self.queue_down(0, n);
        self.queue_pop_back()
    }

    fn queue_pop_back(&mut self) -> usize {
        let t = self.queue.pop().expect("non-empty queue") as usize;
        self.errors.pop();
        self.queue_index[t] = -1;
        t
    }

    fn queue_remove(&mut self, t: usize) {
        let i = self.queue_index[t];
        if i < 0 {
            // Not in the heap yet: still pending.
            if let Some(it) = self.pending.iter().position(|&p| p as usize == t) {
                self.pending.swap_remove(it);
            }
            return;
        }
        let i = i as usize;
        let n = self.queue.len() - 1;
        if n != i {
            self.queue_swap(i, n);
            if !self.queue_down(i, n) {
                self.queue_up(i);
            }
        }
        self.queue_pop_back();
    }

    fn queue_less(&self, i: usize, j: usize) -> bool {
        let (a, b) = (self.errors[i], self.errors[j]);
        a > b || (a == b && self.queue[i] < self.queue[j])
    }

    fn queue_swap(&mut self, i: usize, j: usize) {
        let (pi, pj) = (self.queue[i] as usize, self.queue[j] as usize);
        self.queue.swap(i, j);
        self.errors.swap(i, j);
        self.queue_index[pi] = j as i32;
        self.queue_index[pj] = i as i32;
    }

    fn queue_up(&mut self, j0: usize) {
        let mut j = j0;
        while j > 0 {
            let i = (j - 1) / 2;
            if !self.queue_less(j, i) {
                break;
            }
            self.queue_swap(i, j);
            j = i;
        }
    }

    fn queue_down(&mut self, i0: usize, n: usize) -> bool {
        let mut i = i0;
        loop {
            let j1 = 2 * i + 1;
            if j1 >= n {
                break;
            }
            let j2 = j1 + 1;
            let mut j = j1;
            if j2 < n && self.queue_less(j2, j1) {
                j = j2;
            }
            if !self.queue_less(j, i) {
                break;
            }
            self.queue_swap(i, j);
            i = j;
        }
        i > i0
    }
}

#[cfg(test)]
#[path = "tin_tests.rs"]
mod tests;
