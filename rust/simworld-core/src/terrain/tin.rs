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
//! [`build_terrain_tile`] turns a tile into a [`TerrainTile`]: patch -> TIN -> vertices
//! (tile-local metres relative to the tile centre, the projection of `TileProjector`) -> skirts.

use crate::geo::{Frame, TileKey, MERCATOR_CIRCUMFERENCE};
use crate::mesher::MeshData;
use crate::profile::{Profile, TerrainShading};

use super::sample::{lattice_lat_lon, lattice_lat_lon_f};
use super::Terrain;

/// Upper bound of the automatic skirt depth (m).
pub const SKIRT_MAX_M: f64 = 60.0;

/// Cells per tile side of the height patch for a zoom: about 22 m spacing at z12..z14
/// (z14 64, z13 128, z12 256 cells; spec table 5.2), at most 256 (z11 44 m, z10 88 m), at
/// least 16 (z16).
pub fn patch_cells(z: u8) -> usize {
    let shift = (z as i32 - 10).clamp(0, 6) as u32;
    (1024usize >> shift).clamp(16, 256)
}

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

    /// Per-vertex sink (m) that puts the whole TIN on or under the grid heights (api 1.7,
    /// contract section 9a). Triangles in index order, samples in raster order (edges
    /// included): where the triangle's plane through its unsunk vertices lies `e` above a
    /// sample and the sinks so far lower it there by less, the missing part `d` is added to the
    /// three vertices in proportion to their barycentric weight `w` (`d x w_i / sum w^2`, the
    /// smallest change that closes it), so a sample next to one vertex mostly sinks that
    /// vertex. Sinks only grow, so a sample settled earlier stays settled; deterministic.
    pub fn vertex_sinks(&self) -> Vec<f64> {
        let mut sink = vec![0.0f64; self.coords.len()];
        for t in 0..self.triangle_count() {
            let ids = [self.triangles[t * 3] as usize, self.triangles[t * 3 + 1] as usize, self.triangles[t * 3 + 2] as usize];
            let (p0, p1, p2) = (self.coords[ids[0]], self.coords[ids[1]], self.coords[ids[2]]);
            let a = orient(p0, p1, p2);
            if a <= 0 {
                continue;
            }
            let af = a as f64;
            let (h0, h1, h2) = (self.height_at(p0), self.height_at(p1), self.height_at(p2));
            let min_x = p0[0].min(p1[0]).min(p2[0]);
            let min_y = p0[1].min(p1[1]).min(p2[1]);
            let max_x = p0[0].max(p1[0]).max(p2[0]);
            let max_y = p0[1].max(p1[1]).max(p2[1]);
            for y in min_y..=max_y {
                let row = y as usize * self.width;
                for x in min_x..=max_x {
                    let q = [x, y];
                    let (w0, w1, w2) = (orient(p1, p2, q), orient(p2, p0, q), orient(p0, p1, q));
                    if w0 < 0 || w1 < 0 || w2 < 0 {
                        continue;
                    }
                    let w = [w0 as f64 / af, w1 as f64 / af, w2 as f64 / af];
                    let z = h0 * w[0] + h1 * w[1] + h2 * w[2];
                    let e = z - self.h[row + x as usize] as f64;
                    if e <= 0.0 {
                        continue;
                    }
                    let d = e - (w[0] * sink[ids[0]] + w[1] * sink[ids[1]] + w[2] * sink[ids[2]]);
                    if d <= 0.0 {
                        continue;
                    }
                    let k = d / (w[0] * w[0] + w[1] * w[1] + w[2] * w[2]);
                    for i in 0..3 {
                        sink[ids[i]] += k * w[i];
                    }
                }
            }
        }
        sink
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

/// Seed positions along one side: `count` grid indices from 0 to `last`, evenly spread.
fn seed_line(count: usize, last: usize) -> Vec<usize> {
    let count = count.clamp(2, last + 1);
    (0..count).map(|k| ((k * last) as f64 / (count - 1) as f64).round() as usize).collect()
}

/// Seeds per side for a tile `width_m` wide: `ceil(width / max_edge) + 1` (2 = corners only when
/// `max_edge_m` is 0), reduced until the seed triangles fit under `max_triangles`.
pub fn seed_count(width_m: f64, max_edge_m: f64, cells: usize, max_triangles: usize) -> usize {
    let mut s = if max_edge_m > 0.0 { (width_m / max_edge_m).ceil() as usize + 1 } else { 2 };
    s = s.clamp(2, cells + 1);
    while s > 2 && 2 * (s - 1) * (s - 1) > max_triangles {
        s -= 1;
    }
    s
}

/// One finished terrain tile (worker output).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TerrainTile {
    /// VERTEX + INDEX (+ NORMAL with smooth shading), tile-local metres relative to the tile
    /// centre (Godot axes, y = height above MSL); surface first, then the skirt.
    pub mesh: MeshData,
    /// Lowest / highest surface vertex (m above MSL; skirts excluded).
    pub h_min: f32,
    pub h_max: f32,
    /// Surface triangles (the TIN; at most `max_triangles`).
    pub surface_triangles: u32,
    /// Largest vertical error left in the TIN (m).
    pub max_error: f32,
    /// Skirt depth used (m).
    pub skirt_m: f32,
}

impl TerrainTile {
    pub fn triangle_count(&self) -> usize {
        self.mesh.triangle_count()
    }
}

/// Build the terrain mesh of `key` (ring `ring` of the terrain layer) at `detail` times the
/// band's `max_error_m`. Pure: same key, profile, terrain, frame and detail give the same bytes.
pub fn build_terrain_tile(
    key: TileKey,
    ring: usize,
    frame: Frame,
    terrain: &Terrain,
    profile: &Profile,
    detail: f64,
) -> Result<TerrainTile, String> {
    build_terrain_tile_colored(key, ring, frame, terrain, profile, detail, None)
}

/// COLOR `r` of water in a seed world's terrain (class 2 / 8, contract 9a).
pub const WATER_COLOR_R: f32 = 2.0 / 8.0;

/// Per-vertex terrain COLOR from (lat, lon) (a seed world's ground class and tint, api 1.7).
pub type VertexColor<'a> = &'a dyn Fn(f64, f64) -> [f32; 4];

/// [`build_terrain_tile`] with an optional COLOR per vertex (`color(lat, lon)` of each surface
/// vertex; skirt vertices copy their top vertex). `None` = no COLOR array (every non-seed world).
pub fn build_terrain_tile_colored(
    key: TileKey,
    ring: usize,
    frame: Frame,
    terrain: &Terrain,
    profile: &Profile,
    detail: f64,
    color: Option<VertexColor<'_>>,
) -> Result<TerrainTile, String> {
    let tp = &profile.terrain;
    let band = tp.bands.get(ring).ok_or_else(|| format!("terrain ring {ring} has no band"))?;
    let cells = patch_cells(key.z);
    let n = cells + 1;
    let patch = terrain.patch(key, n);
    let mut heights = Vec::with_capacity(n * n);
    for j in 0..n as i64 {
        for i in 0..n as i64 {
            heights.push(patch.get(i, j) as f32);
        }
    }
    let (lat_c, _) = key.center_lat_lon();
    let width_m = MERCATOR_CIRCUMFERENCE * crate::math::cos(lat_c.to_radians()) / (1u64 << key.z) as f64;
    let max_tris = band.max_triangles.max(2) as usize;
    let seeds = seed_line(seed_count(width_m, band.max_edge_m, cells, max_tris), cells);
    let max_error = band.max_error_m * detail;
    // A seed world's airport on this tile (contract section 9 "Airports"): the error bound of a
    // sample is `max_error x lift_weight` (at least 1 cm), so the TIN is exact on the levelled
    // fence, where the vector ground has no drape lift, and never above the lowered ground in
    // the shoulder. Every other tile keeps the plain metric.
    let scale: Option<Vec<f32>> = terrain.lift_field_touches(key).then(|| {
        let floor = (super::LIFT_FIELD_MIN_ERROR_M / max_error.max(1e-9)).min(1.0);
        let mut sc = Vec::with_capacity(n * n);
        for j in 0..n as i64 {
            for i in 0..n as i64 {
                let (lat, lon) = lattice_lat_lon(key, cells as u32, i, j);
                sc.push((1.0 / terrain.lift_weight(lat, lon).max(floor)) as f32);
            }
        }
        sc
    });
    let mut tin = Delatin::new_scaled(&heights, scale.as_deref(), n, n, &seeds, &seeds);
    let left = tin.run(max_error, max_tris);
    // Sink (contract section 9a, api 1.7): every vertex goes down by the largest excess of its
    // triangles over the grid (0 where the TIN is never above it, e.g. flat land and the sea),
    // so the mesh never lies above the height function the vector world is draped on, at any
    // detail. The error bound per sample (scaled by a seed world's lift weight) bounds the sink.
    let sinks = tin.vertex_sinks();
    let sunk: Vec<f32> = tin
        .coords()
        .iter()
        .zip(&sinks)
        .map(|(c, &e)| {
            let h = heights[c[1] as usize * n + c[0] as usize];
            if e > 0.0 {
                (h as f64 - e) as f32
            } else {
                h
            }
        })
        .collect();
    // Skirt: a sunk edge lies within `2 x error` under the exact height (its own error plus the
    // sink), so across a seam to the next coarser band the gap is bounded by that band's error.
    let coarser = tp.bands.get(ring + 1).map_or(band.max_error_m, |b| b.max_error_m).max(band.max_error_m) * detail;
    let skirt = if tp.skirt_m > 0.0 { tp.skirt_m } else { (2.0 * coarser + 1.0).min(SKIRT_MAX_M) };
    let smooth = tp.shading == TerrainShading::Smooth;
    Ok(assemble(key, cells, frame, &sunk, &tin, skirt, smooth, left, color))
}

/// TIN -> mesh: vertices in tile-local metres (`heights` = the sunk height per TIN vertex),
/// surface triangles clockwise seen from above, outward skirts, optional area-weighted normals.
#[allow(clippy::too_many_arguments)]
fn assemble(
    key: TileKey,
    cells: usize,
    frame: Frame,
    heights: &[f32],
    tin: &Delatin<'_>,
    skirt_m: f64,
    smooth: bool,
    max_error: f64,
    color: Option<VertexColor<'_>>,
) -> TerrainTile {
    let center = frame.tile_center(key);
    let coords = tin.coords();
    let mut m = MeshData::default();
    let (mut h_min, mut h_max) = (f32::INFINITY, f32::NEG_INFINITY);
    m.positions.reserve(coords.len() + coords.len() / 4);
    for (vi, c) in coords.iter().enumerate() {
        let (lat, lon) = lattice_lat_lon(key, cells as u32, c[0], c[1]);
        let [e, nn] = frame.enu(lat, lon);
        let y = heights[vi];
        h_min = h_min.min(y);
        h_max = h_max.max(y);
        m.positions.push([(e - center[0]) as f32, y, -((nn - center[1]) as f32)]);
        if let Some(c) = color {
            m.colors.push(c(lat, lon));
        }
    }
    // Surface: Godot front face = clockwise seen from above, i.e. ((c - a) x (b - a)).y > 0.
    let tris = tin.triangles();
    let up = |m: &MeshData, a: u32, b: u32, c: u32| {
        let (pa, pb, pc) = (m.positions[a as usize], m.positions[b as usize], m.positions[c as usize]);
        (pc[2] - pa[2]) as f64 * (pb[0] - pa[0]) as f64 - (pc[0] - pa[0]) as f64 * (pb[2] - pa[2]) as f64
    };
    m.indices.reserve(tris.len() + 64);
    for t in tris.chunks_exact(3) {
        let (a, b, c) = (t[0], t[1], t[2]);
        if up(&m, a, b, c) >= 0.0 {
            m.indices.extend_from_slice(&[a as i32, b as i32, c as i32]);
        } else {
            m.indices.extend_from_slice(&[a as i32, c as i32, b as i32]);
        }
    }
    let surface_triangles = (m.indices.len() / 3) as u32;
    let surface_vertices = m.positions.len();
    // Smooth normals: area-weighted (unnormalized cross products) over the surface triangles.
    if smooth {
        let mut acc = vec![[0.0f64; 3]; surface_vertices];
        for t in m.indices.chunks_exact(3) {
            let [a, b, c] = [t[0] as usize, t[1] as usize, t[2] as usize];
            let (pa, pb, pc) = (m.positions[a], m.positions[b], m.positions[c]);
            let u = [(pc[0] - pa[0]) as f64, (pc[1] - pa[1]) as f64, (pc[2] - pa[2]) as f64];
            let v = [(pb[0] - pa[0]) as f64, (pb[1] - pa[1]) as f64, (pb[2] - pa[2]) as f64];
            let g = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
            for i in [a, b, c] {
                for k in 0..3 {
                    acc[i][k] += g[k];
                }
            }
        }
        m.normals = acc
            .iter()
            .map(|g| {
                let l = (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt();
                if l > 0.0 {
                    [(g[0] / l) as f32, (g[1] / l) as f32, (g[2] / l) as f32]
                } else {
                    [0.0, 1.0, 0.0]
                }
            })
            .collect();
    }
    // Coast (seed COLOR, contract 9a): a surface triangle whose vertices are partly water and
    // partly land would blend the two classes along its whole length (land spikes into the sea,
    // hair lines across lakes). Such a triangle takes one side from the water test at its
    // centroid: water -> every vertex water, land -> its water vertices take the land colour at
    // the centroid. The vertices that change colour are duplicated (shared by the triangles of
    // the same tile that need the same copy), so the coast is a hard edge along the TIN edges and
    // no triangle is added.
    if let Some(color) = color {
        let is_water = |c: &[f32; 4]| c[0] == WATER_COLOR_R;
        let mut copies: ahash::AHashMap<(u32, [u32; 4]), u32> = ahash::AHashMap::new();
        for t in 0..surface_triangles as usize {
            let tri = [m.indices[3 * t] as usize, m.indices[3 * t + 1] as usize, m.indices[3 * t + 2] as usize];
            let wet = tri.map(|v| is_water(&m.colors[v]));
            if wet[0] == wet[1] && wet[1] == wet[2] {
                continue;
            }
            let (fi, fj) = tri.iter().fold((0.0, 0.0), |(a, b), &v| (a + coords[v][0] as f64, b + coords[v][1] as f64));
            let (lat, lon) = lattice_lat_lon_f(key, cells as u32, fi / 3.0, fj / 3.0);
            let cc = color(lat, lon);
            let water_t = is_water(&cc);
            for k in 0..3 {
                if wet[k] == water_t {
                    continue;
                }
                let v = tri[k];
                let target = cc;
                let id = *copies.entry((v as u32, target.map(f32::to_bits))).or_insert_with(|| {
                    let p = m.positions[v];
                    m.positions.push(p);
                    m.colors.push(target);
                    if smooth {
                        let nv = m.normals[v];
                        m.normals.push(nv);
                    }
                    (m.positions.len() - 1) as u32
                });
                m.indices[3 * t + k] = id as i32;
            }
        }
    }
    // Skirt: one bottom copy per hull vertex, a quad per hull edge, facing outwards (away from
    // the tile centre, which is the local origin).
    let half = tin.halfedges();
    let mut bottom = vec![-1i32; surface_vertices];
    let drop = skirt_m as f32;
    for (e, &opp) in half.iter().enumerate() {
        if opp >= 0 {
            continue;
        }
        let a = tris[e] as usize;
        let b = tris[e - e % 3 + (e + 1) % 3] as usize;
        for v in [a, b] {
            if bottom[v] < 0 {
                bottom[v] = m.positions.len() as i32;
                let p = m.positions[v];
                m.positions.push([p[0], p[1] - drop, p[2]]);
                if color.is_some() {
                    let cv = m.colors[v];
                    m.colors.push(cv);
                }
                if smooth {
                    let nv = m.normals[v];
                    m.normals.push(nv);
                }
            }
        }
        let (ta, tb, ba, bb) = (a as i32, b as i32, bottom[a], bottom[b]);
        // Outward: the quad's normal points along the edge midpoint (from the tile centre).
        let (pa, pb) = (m.positions[a], m.positions[b]);
        let mid = [(pa[0] + pb[0]) as f64 * 0.5, (pa[2] + pb[2]) as f64 * 0.5];
        // Normal of (ta, tb, bb) by ((c - a) x (b - a)), horizontal part.
        let pbb = m.positions[bb as usize];
        let u = [(pbb[0] - pa[0]) as f64, (pbb[1] - pa[1]) as f64, (pbb[2] - pa[2]) as f64];
        let v = [(pb[0] - pa[0]) as f64, (pb[1] - pa[1]) as f64, (pb[2] - pa[2]) as f64];
        let gx = u[1] * v[2] - u[2] * v[1];
        let gz = u[0] * v[1] - u[1] * v[0];
        if gx * mid[0] + gz * mid[1] >= 0.0 {
            m.indices.extend_from_slice(&[ta, tb, bb, ta, bb, ba]);
        } else {
            m.indices.extend_from_slice(&[ta, bb, tb, ta, ba, bb]);
        }
    }
    TerrainTile {
        mesh: m,
        h_min,
        h_max,
        surface_triangles,
        max_error: max_error as f32,
        skirt_m: skirt_m as f32,
    }
}

#[cfg(test)]
#[path = "tin_tests.rs"]
mod tests;
