//! Water index and the terrain-water rule (`docs/spec-world-seed.md` sections 4.4 "Load-time
//! index" and 5.2a). Built in S0, wired into [`crate::Terrain`] in S1 (`seed::terrain`), which
//! also uses [`WaterIndex::tile_polygons`] for the vector water areas and [`WaterIndex::water_at`]
//! for the terrain colour.
//!
//! **Rivers (S1 decision):** with `rivers_flat` false (the default) rivers take no part in the
//! terrain rule: a river point is land for [`WaterIndex::apply`], river edges are not coast in the
//! distance raster, so a long river never becomes a flat trench at its lowest point. Rivers stay
//! water for [`WaterIndex::water_at`] (ground class, terrain colour) and are drawn as vector water
//! areas draped on the relief.
//!
//! * **Index.** Every water polygon is clipped into z11 Web Mercator cells by recursive quadtree
//!   descent (z0 -> z11, Sutherland-Hodgman against each child's lon/lat rectangle, so a vertex is
//!   touched about 12 times however large the polygon). A cell is *land* (absent), *water*
//!   (wholly inside one polygon, nothing stored) or *mixed* (stores its clipped rings plus 64
//!   latitude bands of edge references for the point test; 64 since S1, 16 in S0). The grid is dense over the file's
//!   bbox: one `u32` per cell.
//! * **Water test** ([`WaterIndex::water_at`]): outside the bbox the file's `outside` decides; in
//!   a water cell the answer is that polygon; in a mixed cell an even-odd ray test against the
//!   edges of the point's band.
//! * **Distance raster.** For every mixed cell and its land / mixed neighbours, `n x n` samples
//!   (corners inclusive, so neighbouring cells share their border samples bit for bit) of the
//!   **signed** distance to the nearest real water edge (negative inside water; edges created by
//!   clipping and edges along the file bbox are not coast), clamped to `±blend_m`, as `i16`
//!   decimetres, plus a `u8` index of the nearest water: 3 bytes a sample, 12 KB at 64 x 64.
//!   The distance of a sample is computed in a frame at the sample's own latitude from the
//!   clipped edges of the 3 x 3 cells around it, so it does not depend on which cell computes it.
//!   A raster with no sample within `blend_m` of water is not stored. (Signed, not the spec's
//!   unsigned distance: an unsigned field interpolated bilinearly is ~half the sample spacing at
//!   the coast line, which would leave a step of `(field - w) * smoothstep(~90 m)` there; the
//!   signed field interpolates to about 0 on the coast.)
//! * **Rule** ([`WaterIndex::apply`]): in the sea `sea_level_m`, in a lake (or river) its level;
//!   on land `w + (field - w) * smoothstep(0, blend, d)` with `d` the bilinear signed distance and
//!   `w` the bilinear level of the four samples' nearest waters (a sample with no water within
//!   `blend_m` takes the mean of the others), so the height is continuous across cells and
//!   across the line where the nearest water changes.

use super::files::{q_cm, Outside, WaterFile, WaterKind};
use super::geom::{clip_ring, m_per_deg_lon, on_same_side, point_segment_dist, signed_area, Rect, M_PER_DEG};
use crate::geo::{lat_to_tile_y, lon_to_tile_x, tile_x_to_lon, tile_y_to_lat};
use crate::terrain::noise::smoothstep;

/// Zoom of the index cells.
pub const INDEX_ZOOM: u8 = 11;
/// Latitude bands per mixed cell for the point test.
pub const BANDS: usize = 64;

/// Raster parameters (profile `seed.water` in S1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaterParams {
    /// Land within this distance of water slopes down to it (m).
    pub blend_m: f64,
    /// Samples per raster side (64 desktop, 32 iOS).
    pub raster_n: usize,
    /// Sea level (m), `terrain.sea_level_m`.
    pub sea_level_m: f64,
    /// Rivers are flat at their level in the terrain rule like lakes (S0 prototype behaviour).
    /// False in S1: rivers are vector water only (module docs).
    pub rivers_flat: bool,
}

impl Default for WaterParams {
    fn default() -> Self {
        Self { blend_m: 400.0, raster_n: 64, sea_level_m: 0.0, rivers_flat: false }
    }
}

/// One water body (= one polygon of the file).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaterBody {
    pub kind: WaterKind,
    /// Surface height (m): `sea_level_m` for the sea; for lakes and rivers the minimum of the
    /// terrain field over the outline vertices, quantized to 1 cm.
    pub level: f64,
}

/// What a point is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaterHit {
    Land,
    /// Outside the file's bbox with `outside: "sea"`.
    OutsideSea,
    /// Polygon index of the water file.
    Body(u32),
}

#[derive(Clone, Debug)]
struct ClippedRing {
    /// Index into the cell's `polys`.
    local: u16,
    pts: Vec<[f64; 2]>,
}

#[derive(Clone, Debug)]
struct MixedCell {
    rect: Rect,
    /// Global polygon ids present in the cell, ascending.
    polys: Vec<u32>,
    rings: Vec<ClippedRing>,
    /// Edge references (ring, start vertex) per latitude band, CSR.
    band_start: [u32; BANDS + 1],
    band_edges: Vec<(u32, u32)>,
}

#[derive(Clone, Debug)]
struct Raster {
    /// Signed distance, decimetres (negative inside water).
    dist_dm: Vec<i16>,
    /// Diagonal of one raster step (m, at the cell's wider edge) plus the decimetre rounding:
    /// four samples all farther than this from water mean the point between them is land.
    diag_m: f64,
    nearest: Vec<u8>,
    /// Global polygon ids `nearest` indexes.
    waters: Vec<u32>,
}

/// Measured size of an index (section 10a budgets).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WaterStats {
    pub grid_cells: usize,
    pub water_cells: usize,
    pub mixed_cells: usize,
    pub raster_cells: usize,
    pub ring_points: usize,
    pub band_refs: usize,
    /// Bytes of the dense grid + mixed cells (rings, bands).
    pub index_bytes: usize,
    /// Bytes of the distance rasters.
    pub raster_bytes: usize,
}

/// The load-time water index of one water file.
#[derive(Clone, Debug)]
pub struct WaterIndex {
    params: WaterParams,
    bbox: [f64; 4],
    outside_sea: bool,
    bodies: Vec<WaterBody>,
    /// Cell range covered by the dense grid.
    x0: u32,
    y0: u32,
    nx: u32,
    ny: u32,
    /// 0 = land; `1 + poly` = water; `MIXED | idx` = mixed.
    grid: Vec<u32>,
    mixed: Vec<MixedCell>,
    /// Per grid cell: raster index or `u32::MAX`.
    raster_of: Vec<u32>,
    /// Latitude of the north edge of grid row `j` (`ny + 1` values, descending; the last is the
    /// south edge of the last row): the point -> row lookup is a binary search, not a Mercator
    /// projection.
    row_lat: Vec<f64>,
    rasters: Vec<Raster>,
}

const MIXED: u32 = 0x8000_0000;
const NONE: u32 = u32::MAX;

fn tile_rect(x: u32, y: u32, z: u8) -> Rect {
    [tile_x_to_lon(x as f64, z), tile_y_to_lat((y + 1) as f64, z), tile_x_to_lon((x + 1) as f64, z), tile_y_to_lat(y as f64, z)]
}

/// Per-polygon output of the descent.
enum Piece {
    Full,
    Rings(Vec<Vec<[f64; 2]>>),
}

impl WaterIndex {
    /// Build the index for `file`. `field(lat, lon)` is the terrain height before any water rule
    /// (lake levels); pass `|_, _| 0.0` without terrain.
    pub fn build(file: &WaterFile, params: WaterParams, field: &dyn Fn(f64, f64) -> f64) -> Self {
        let z = INDEX_ZOOM;
        let [w, s, e, n] = file.bbox;
        let max = (1u32 << z) - 1;
        let x0 = (lon_to_tile_x(w, z).floor().max(0.0) as u32).min(max);
        let x1 = (lon_to_tile_x(e, z).floor().max(0.0) as u32).min(max);
        let y0 = (lat_to_tile_y(n, z).floor().max(0.0) as u32).min(max);
        let y1 = (lat_to_tile_y(s, z).floor().max(0.0) as u32).min(max);
        let (nx, ny) = (x1 - x0 + 1, y1 - y0 + 1);
        let mut idx = Self {
            params,
            bbox: file.bbox,
            outside_sea: file.outside == Outside::Sea,
            bodies: Vec::with_capacity(file.polygons.len()),
            x0,
            y0,
            nx,
            ny,
            grid: vec![0; (nx * ny) as usize],
            mixed: Vec::new(),
            raster_of: vec![NONE; (nx * ny) as usize],
            rasters: Vec::new(),
            row_lat: (0..=ny).map(|j| tile_y_to_lat((y0 + j) as f64, z)).collect(),
        };
        // Water levels.
        for p in &file.polygons {
            let level = match p.kind {
                WaterKind::Sea => params.sea_level_m,
                WaterKind::Lake | WaterKind::River => {
                    let lo = p.rings.iter().flatten().map(|v| field(v[1], v[0])).fold(f64::INFINITY, f64::min);
                    q_cm(lo.max(params.sea_level_m))
                }
            };
            idx.bodies.push(WaterBody { kind: p.kind, level });
        }
        // Descent, per polygon, collecting per-cell pieces (cells visited in a fixed order).
        let mut pieces: std::collections::BTreeMap<(u32, u32), Vec<(u32, Piece)>> = Default::default();
        for (pid, p) in file.polygons.iter().enumerate() {
            let mut full_tiles = Vec::new();
            descend(&p.rings, 0, 0, 0, &idx, pid as u32, &mut pieces, &mut full_tiles);
            for (tz, tx, ty) in full_tiles {
                let shift = z - tz;
                let (ax, ay) = (tx << shift, ty << shift);
                let span = 1u32 << shift;
                for cy in ay.max(y0)..(ay + span).min(y0 + ny) {
                    for cx in ax.max(x0)..(ax + span).min(x0 + nx) {
                        pieces.entry((cx, cy)).or_default().push((pid as u32, Piece::Full));
                    }
                }
            }
        }
        for ((cx, cy), list) in pieces {
            let g = idx.grid_index(cx, cy).expect("piece inside grid");
            if list.iter().all(|(_, p)| matches!(p, Piece::Full)) {
                // The lowest id, preferring a body of the terrain rule over a river.
                let rule = |p: &u32| idx.params.rivers_flat || idx.bodies[*p as usize].kind != WaterKind::River;
                let pid = list.iter().map(|(p, _)| *p).filter(rule).min().or_else(|| list.iter().map(|(p, _)| *p).min()).unwrap_or(0);
                idx.grid[g] = 1 + pid;
                continue;
            }
            let rect = tile_rect(cx, cy, z);
            let mut polys: Vec<u32> = list.iter().map(|(p, _)| *p).collect();
            polys.sort_unstable();
            polys.dedup();
            let mut rings = Vec::new();
            for (pid, piece) in list {
                let local = polys.binary_search(&pid).unwrap_or(0) as u16;
                match piece {
                    Piece::Full => rings.push(ClippedRing {
                        local,
                        pts: vec![[rect[0], rect[1]], [rect[2], rect[1]], [rect[2], rect[3]], [rect[0], rect[3]]],
                    }),
                    Piece::Rings(rs) => rings.extend(rs.into_iter().map(|pts| ClippedRing { local, pts })),
                }
            }
            let cell = MixedCell::new(rect, polys, rings);
            idx.grid[g] = MIXED | idx.mixed.len() as u32;
            idx.mixed.push(cell);
        }
        idx.build_rasters();
        idx
    }

    fn grid_index(&self, cx: u32, cy: u32) -> Option<usize> {
        (cx >= self.x0 && cy >= self.y0 && cx < self.x0 + self.nx && cy < self.y0 + self.ny)
            .then(|| ((cy - self.y0) * self.nx + (cx - self.x0)) as usize)
    }

    pub fn bodies(&self) -> &[WaterBody] {
        &self.bodies
    }

    pub fn params(&self) -> &WaterParams {
        &self.params
    }

    fn in_bbox(&self, lat: f64, lon: f64) -> bool {
        lon >= self.bbox[0] && lon <= self.bbox[2] && lat >= self.bbox[1] && lat <= self.bbox[3]
    }

    /// Grid index of the cell holding a point (`None` outside the grid). The column is the same
    /// as `floor(lon_to_tile_x)`; the row is found among the rows' edge latitudes (identical to
    /// the tile rectangles the cells were clipped with).
    fn cell_of(&self, lat: f64, lon: f64) -> Option<usize> {
        let cx = lon_to_tile_x(lon, INDEX_ZOOM).floor();
        if cx < self.x0 as f64 || cx >= (self.x0 + self.nx) as f64 {
            return None;
        }
        // row_lat is descending: rows j with row_lat[j] >= lat > row_lat[j + 1].
        if lat > self.row_lat[0] || lat <= self.row_lat[self.ny as usize] {
            return None;
        }
        let j = self.row_lat.partition_point(|&l| l >= lat).saturating_sub(1).min(self.ny as usize - 1);
        Some(j * self.nx as usize + (cx as u32 - self.x0) as usize)
    }

    /// Which water a point is in.
    pub fn water_at(&self, lat: f64, lon: f64) -> WaterHit {
        self.water_in(self.cell_of(lat, lon), lat, lon)
    }

    fn water_in(&self, cell: Option<usize>, lat: f64, lon: f64) -> WaterHit {
        if !self.in_bbox(lat, lon) {
            return if self.outside_sea { WaterHit::OutsideSea } else { WaterHit::Land };
        }
        let Some(g) = cell else {
            return WaterHit::Land;
        };
        match self.grid[g] {
            0 => WaterHit::Land,
            v if v & MIXED != 0 => match self.mixed[(v & !MIXED) as usize].test([lon, lat]) {
                Some(p) => WaterHit::Body(p),
                None => WaterHit::Land,
            },
            v => WaterHit::Body(v - 1),
        }
    }

    /// The terrain-water rule (section 5.2a) on top of `field` = the height at (lat, lon) before
    /// any water rule.
    pub fn apply(&self, field: f64, lat: f64, lon: f64) -> f64 {
        self.apply_with(lat, lon, || field)
    }

    /// Whether a body takes part in the terrain rule (rivers only with `rivers_flat`).
    #[inline]
    fn in_rule(&self, p: u32) -> bool {
        self.params.rivers_flat || self.bodies[p as usize].kind != WaterKind::River
    }

    /// Water level of the body the terrain rule puts the point in (`None` = land for the rule).
    fn rule_level(&self, cell: Option<usize>, lat: f64, lon: f64) -> Option<f64> {
        if !self.in_bbox(lat, lon) {
            return self.outside_sea.then_some(self.params.sea_level_m);
        }
        let g = cell?;
        match self.grid[g] {
            0 => None,
            v if v & MIXED != 0 => {
                let m = &self.mixed[(v & !MIXED) as usize];
                let p = if self.params.rivers_flat { m.test([lon, lat]) } else { m.test_where([lon, lat], |p| self.in_rule(p)) }?;
                Some(self.bodies[p as usize].level)
            }
            v => self.in_rule(v - 1).then(|| self.bodies[(v - 1) as usize].level),
        }
    }

    /// Level of the sea or lake the terrain rule puts the point in (the height there is exactly
    /// this level), `None` = land for the rule (rivers too, unless `rivers_flat`).
    pub fn rule_level_at(&self, lat: f64, lon: f64) -> Option<f64> {
        self.rule_level(self.cell_of(lat, lon), lat, lon)
    }

    /// [`WaterIndex::apply`] with the field evaluated only when needed (not in water): the
    /// terrain's water-first order (section 10a budget).
    pub fn apply_with(&self, lat: f64, lon: f64, field: impl FnOnce() -> f64) -> f64 {
        let cell = self.cell_of(lat, lon);
        let raster = match cell {
            Some(g) if self.raster_of[g] != NONE && self.in_bbox(lat, lon) => Some((g, self.raster_of[g])),
            _ => None,
        };
        let Some((g, ri)) = raster else {
            return match self.rule_level(cell, lat, lon) {
                Some(l) => l,
                None => field(),
            };
        };
        let r = &self.rasters[ri as usize];
        let rect = self.cell_rect(g);
        let n = self.params.raster_n;
        let fx = ((lon - rect[0]) / (rect[2] - rect[0]) * (n - 1) as f64).clamp(0.0, (n - 1) as f64);
        let fy = ((lat - rect[1]) / (rect[3] - rect[1]) * (n - 1) as f64).clamp(0.0, (n - 1) as f64);
        let (i, j) = ((fx.floor() as usize).min(n - 2), (fy.floor() as usize).min(n - 2));
        let (tx, ty) = (fx - i as f64, fy - j as f64);
        let blend = self.params.blend_m;
        let k0 = j * n + i;
        let near = [k0, k0 + 1, k0 + n, k0 + n + 1].map(|k| r.dist_dm[k] as f64 * 0.1);
        // Fast path: the signed distance is 1-Lipschitz, so four samples farther from water than a
        // raster diagonal mean land (no polygon test); all at blend_m means untouched.
        let min = near.iter().copied().fold(f64::INFINITY, f64::min);
        if min < r.diag_m {
            if let Some(l) = self.rule_level(cell, lat, lon) {
                return l;
            }
        } else if min >= blend {
            return field();
        }
        let field = field();
        let ks = [k0, k0 + 1, k0 + n, k0 + n + 1];
        let d = near;
        // Level of each corner's nearest water; a corner with no water within blend_m takes the
        // mean of the others (its own weight is gone at d = blend anyway, and using `field` there
        // would add a slope of `field / spacing` to the blend).
        let mut w = [f64::NAN; 4];
        let (mut sum, mut cnt) = (0.0, 0);
        for c in 0..4 {
            if d[c] < blend {
                w[c] = self.bodies[r.waters[r.nearest[ks[c]] as usize] as usize].level;
                sum += w[c];
                cnt += 1;
            }
        }
        if cnt == 0 {
            return field;
        }
        let mean = sum / cnt as f64;
        for x in &mut w {
            if x.is_nan() {
                *x = mean;
            }
        }
        let lerp2 = |v: [f64; 4]| {
            let a = v[0] + (v[1] - v[0]) * tx;
            let b = v[2] + (v[3] - v[2]) * tx;
            a + (b - a) * ty
        };
        let (d, w) = (lerp2(d), lerp2(w));
        w + (field - w) * smoothstep(0.0, blend, d)
    }

    /// Water polygons over a vector tile (zoom >= 11, so the tile lies in one index cell), in
    /// lon/lat: the cell's clipped rings (outer ring first, then holes) per body, the whole tile
    /// rectangle for a cell inside one body, plus the parts of the tile outside the file's bbox
    /// when `outside` is `"sea"`. Ground areas of the seed world (section 5.2).
    pub fn tile_polygons(&self, key: crate::geo::TileKey) -> Vec<(WaterKind, Vec<Vec<[f64; 2]>>)> {
        let mut out = Vec::new();
        if key.z < INDEX_ZOOM {
            return out;
        }
        let z = key.z;
        let t = [tile_x_to_lon(key.x as f64, z), tile_y_to_lat((key.y + 1) as f64, z), tile_x_to_lon((key.x + 1) as f64, z), tile_y_to_lat(key.y as f64, z)];
        let rect_ring = |r: Rect| vec![[r[0], r[1]], [r[2], r[1]], [r[2], r[3]], [r[0], r[3]]];
        let bb = self.bbox;
        // Outside the bbox: up to four rectangles (west, east, south, north strips) of the tile.
        if self.outside_sea {
            let mut parts: Vec<Rect> = Vec::new();
            if t[0] < bb[0] {
                parts.push([t[0], t[1], t[2].min(bb[0]), t[3]]);
            }
            if t[2] > bb[2] {
                parts.push([t[0].max(bb[2]), t[1], t[2], t[3]]);
            }
            let (x0, x1) = (t[0].max(bb[0]), t[2].min(bb[2]));
            if x0 < x1 {
                if t[1] < bb[1] {
                    parts.push([x0, t[1], x1, t[3].min(bb[1])]);
                }
                if t[3] > bb[3] {
                    parts.push([x0, t[1].max(bb[3]), x1, t[3]]);
                }
            }
            for r in parts.into_iter().filter(|r| r[0] < r[2] && r[1] < r[3]) {
                out.push((WaterKind::Sea, vec![rect_ring(r)]));
            }
        }
        let shift = z - INDEX_ZOOM;
        let Some(g) = self.grid_index(key.x >> shift, key.y >> shift) else { return out };
        match self.grid[g] {
            0 => {}
            v if v & MIXED != 0 => {
                let m = &self.mixed[(v & !MIXED) as usize];
                let mut i = 0;
                while i < m.rings.len() {
                    let local = m.rings[i].local;
                    let mut rings = Vec::new();
                    while i < m.rings.len() && m.rings[i].local == local {
                        rings.push(m.rings[i].pts.clone());
                        i += 1;
                    }
                    let pid = m.polys[local as usize];
                    out.push((self.bodies[pid as usize].kind, rings));
                }
            }
            v => {
                let r = [t[0].max(bb[0]), t[1].max(bb[1]), t[2].min(bb[2]), t[3].min(bb[3])];
                if r[0] < r[2] && r[1] < r[3] {
                    out.push((self.bodies[(v - 1) as usize].kind, vec![rect_ring(r)]));
                }
            }
        }
        out
    }

    /// Signed distance (m) to the nearest water edge from the raster (bilinear; negative in water;
    /// `blend_m` where no raster).
    pub fn raster_distance_m(&self, lat: f64, lon: f64) -> f64 {
        let Some(g) = self.cell_of(lat, lon) else {
            return self.params.blend_m;
        };
        let ri = self.raster_of[g];
        if ri == NONE {
            return self.params.blend_m;
        }
        let r = &self.rasters[ri as usize];
        let rect = self.cell_rect(g);
        let n = self.params.raster_n;
        let fx = ((lon - rect[0]) / (rect[2] - rect[0]) * (n - 1) as f64).clamp(0.0, (n - 1) as f64);
        let fy = ((lat - rect[1]) / (rect[3] - rect[1]) * (n - 1) as f64).clamp(0.0, (n - 1) as f64);
        let (i, j) = ((fx.floor() as usize).min(n - 2), (fy.floor() as usize).min(n - 2));
        let (tx, ty) = (fx - i as f64, fy - j as f64);
        let d = |k: usize| r.dist_dm[k] as f64 * 0.1;
        let k = j * n + i;
        let a = d(k) + (d(k + 1) - d(k)) * tx;
        let b = d(k + n) + (d(k + n + 1) - d(k + n)) * tx;
        a + (b - a) * ty
    }

    fn cell_rect(&self, g: usize) -> Rect {
        let (i, j) = (g as u32 % self.nx, (g as u32 / self.nx) as usize);
        let z = INDEX_ZOOM;
        [tile_x_to_lon((self.x0 + i) as f64, z), self.row_lat[j + 1], tile_x_to_lon((self.x0 + i + 1) as f64, z), self.row_lat[j]]
    }

    /// Exact distance (m) from a point to the nearest real water edge within `blend_m` (brute
    /// force over the 3 x 3 cells; for tests and measurements).
    pub fn exact_distance_m(&self, lat: f64, lon: f64) -> f64 {
        let Some(g) = self.cell_of(lat, lon) else {
            return self.params.blend_m;
        };
        let (cx, cy) = ((self.x0 + g as u32 % self.nx) as i64, (self.y0 + g as u32 / self.nx) as i64);
        let mut best = self.params.blend_m;
        for (_, a, b, _) in self.edges_around(cx, cy) {
            best = best.min(dist_m([lon, lat], a, b));
        }
        best
    }

    /// Real water edges (poly, a, b) of the mixed cells in the 3 x 3 block around a cell.
    fn edges_around(&self, cx: i64, cy: i64) -> Vec<(u32, [f64; 2], [f64; 2], Rect)> {
        let mut out = Vec::new();
        let bb = [self.bbox[0], self.bbox[1], self.bbox[2], self.bbox[3]];
        for dy in -1..=1 {
            for dx in -1..=1 {
                let (x, y) = (cx + dx, cy + dy);
                if x < 0 || y < 0 {
                    continue;
                }
                let Some(g) = self.grid_index(x as u32, y as u32) else { continue };
                let v = self.grid[g];
                if v & MIXED == 0 || v == 0 {
                    continue;
                }
                let m = &self.mixed[(v & !MIXED) as usize];
                for r in &m.rings {
                    let n = r.pts.len();
                    for i in 0..n {
                        let (a, b) = (r.pts[i], r.pts[(i + 1) % n]);
                        if on_same_side(a, b, m.rect) || on_same_side(a, b, bb) {
                            continue;
                        }
                        out.push((m.polys[r.local as usize], a, b, m.rect));
                    }
                }
            }
        }
        out
    }

    fn build_rasters(&mut self) {
        let n = self.params.raster_n.max(2);
        self.params.raster_n = n;
        let blend = self.params.blend_m;
        let mut want: Vec<usize> = Vec::new();
        for (g, v) in self.grid.iter().enumerate() {
            if v & MIXED == 0 || *v == 0 {
                continue;
            }
            let (cx, cy) = ((self.x0 + g as u32 % self.nx) as i64, (self.y0 + g as u32 / self.nx) as i64);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (x, y) = (cx + dx, cy + dy);
                    if x < 0 || y < 0 {
                        continue;
                    }
                    if let Some(h) = self.grid_index(x as u32, y as u32) {
                        let s = self.grid[h];
                        if s == 0 || s & MIXED != 0 {
                            want.push(h);
                        }
                    }
                }
            }
        }
        want.sort_unstable();
        want.dedup();
        for g in want {
            let rect = self.cell_rect(g);
            let (cx, cy) = ((self.x0 + g as u32 % self.nx) as i64, (self.y0 + g as u32 / self.nx) as i64);
            let mut edges = self.edges_around(cx, cy);
            edges.retain(|e| self.in_rule(e.0));
            let mut dist = vec![blend; n * n];
            let mut near = vec![NONE; n * n];
            // Sample coordinates; the last one is the rectangle edge itself (shared with the neighbour).
            let lon_at = |i: usize| if i == n - 1 { rect[2] } else { rect[0] + (rect[2] - rect[0]) * (i as f64 / (n - 1) as f64) };
            let lat_at = |j: usize| if j == n - 1 { rect[3] } else { rect[1] + (rect[3] - rect[1]) * (j as f64 / (n - 1) as f64) };
            let lons: Vec<f64> = (0..n).map(lon_at).collect();
            let lats: Vec<f64> = (0..n).map(lat_at).collect();
            let (dlon, dlat) = ((rect[2] - rect[0]) / (n - 1) as f64, (rect[3] - rect[1]) / (n - 1) as f64);
            let kx_min = m_per_deg_lon(rect[3].abs().max(rect[1].abs()) + 1.0);
            for (pid, a, b, _) in &edges {
                // Samples within `blend` of the edge's bbox.
                let pad_lon = blend / kx_min;
                let pad_lat = blend / M_PER_DEG;
                let (lo_x, hi_x) = (a[0].min(b[0]) - pad_lon, a[0].max(b[0]) + pad_lon);
                let (lo_y, hi_y) = (a[1].min(b[1]) - pad_lat, a[1].max(b[1]) + pad_lat);
                if hi_x < rect[0] || lo_x > rect[2] || hi_y < rect[1] || lo_y > rect[3] {
                    continue;
                }
                let i0 = (((lo_x - rect[0]) / dlon).floor().max(0.0) as usize).min(n - 1);
                let i1 = (((hi_x - rect[0]) / dlon).ceil().max(0.0) as usize).min(n - 1);
                let j0 = (((lo_y - rect[1]) / dlat).floor().max(0.0) as usize).min(n - 1);
                let j1 = (((hi_y - rect[1]) / dlat).ceil().max(0.0) as usize).min(n - 1);
                for (j, &lat) in lats.iter().enumerate().take(j1 + 1).skip(j0) {
                    for (i, &lon) in lons.iter().enumerate().take(i1 + 1).skip(i0) {
                        let d = dist_m([lon, lat], *a, *b);
                        let k = j * n + i;
                        // Ties: the lower polygon id, so the result is independent of edge order.
                        if d < dist[k] || (d == dist[k] && d < blend && *pid < near[k]) {
                            dist[k] = d;
                            near[k] = *pid;
                        }
                    }
                }
            }
            if near.iter().all(|p| *p == NONE) {
                continue; // nothing within blend_m: the rule leaves this cell alone
            }
            for j in 0..n {
                for i in 0..n {
                    if self.rule_level(self.cell_of(lats[j], lons[i]), lats[j], lons[i]).is_some() {
                        dist[j * n + i] = -dist[j * n + i];
                    }
                }
            }
            let mut waters: Vec<u32> = near.iter().copied().filter(|p| *p != NONE).collect();
            waters.sort_unstable();
            waters.dedup();
            waters.truncate(255);
            let sx = (rect[2] - rect[0]) / (n - 1) as f64 * m_per_deg_lon(rect[1].abs().min(rect[3].abs()));
            let sy = (rect[3] - rect[1]) / (n - 1) as f64 * M_PER_DEG;
            let raster = Raster {
                diag_m: (sx * sx + sy * sy).sqrt() + 0.2,
                dist_dm: dist.iter().map(|d| (d.clamp(-blend, blend) * 10.0).round().clamp(-32767.0, 32767.0) as i16).collect(),
                nearest: near.iter().map(|p| waters.binary_search(p).map_or(0, |i| i as u8)).collect(),
                waters,
            };
            self.raster_of[g] = self.rasters.len() as u32;
            self.rasters.push(raster);
        }
    }

    /// Section 4.6 warnings that need the water: a city centre in water, a city footprint more
    /// than 50 % water (sampled on a 41 x 41 grid over the footprint circle).
    pub fn city_warnings(&self, cities: &[super::files::City]) -> Vec<String> {
        let mut out = Vec::new();
        for c in cities {
            let (lat, lon) = (c.center.lat, c.center.lon);
            if self.water_at(lat, lon) != WaterHit::Land {
                out.push(format!("city {}: centre ({lat}, {lon}) is in water", c.id));
            }
            let r = c.radius_km * 1000.0;
            let (kx, ky) = (m_per_deg_lon(lat), M_PER_DEG);
            let (mut inside, mut wet) = (0u32, 0u32);
            for j in -20i32..=20 {
                for i in -20i32..=20 {
                    let (x, y) = (i as f64 / 20.0 * r, j as f64 / 20.0 * r);
                    if x * x + y * y > r * r {
                        continue;
                    }
                    inside += 1;
                    if self.water_at(lat + y / ky, lon + x / kx) != WaterHit::Land {
                        wet += 1;
                    }
                }
            }
            if wet * 2 > inside {
                out.push(format!("city {}: footprint is {:.0} % water", c.id, wet as f64 * 100.0 / inside as f64));
            }
        }
        out
    }

    /// Sizes for the budget table (section 10a).
    pub fn stats(&self) -> WaterStats {
        let mut s = WaterStats {
            grid_cells: self.grid.len(),
            water_cells: self.grid.iter().filter(|v| **v != 0 && **v & MIXED == 0).count(),
            mixed_cells: self.mixed.len(),
            raster_cells: self.rasters.len(),
            ..Default::default()
        };
        let mut idx = self.grid.len() * 4 + self.raster_of.len() * 4 + self.bodies.len() * std::mem::size_of::<WaterBody>();
        for m in &self.mixed {
            idx += std::mem::size_of::<MixedCell>() + m.polys.len() * 4 + m.band_edges.len() * 8;
            s.band_refs += m.band_edges.len();
            for r in &m.rings {
                idx += std::mem::size_of::<ClippedRing>() + r.pts.len() * 16;
                s.ring_points += r.pts.len();
            }
        }
        s.index_bytes = idx;
        s.raster_bytes = self.rasters.iter().map(|r| std::mem::size_of::<Raster>() + r.dist_dm.len() * 2 + r.nearest.len() + r.waters.len() * 4).sum();
        s
    }
}

/// Distance (m) from a lon/lat point to a lon/lat segment, in an equirectangular frame at the
/// point's latitude (the same for every cell that evaluates this point).
#[inline]
fn dist_m(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let kx = m_per_deg_lon(p[1]);
    let f = |q: [f64; 2]| [(q[0] - p[0]) * kx, (q[1] - p[1]) * M_PER_DEG];
    point_segment_dist([0.0, 0.0], f(a), f(b))
}

#[allow(clippy::too_many_arguments)]
fn descend(
    rings: &[Vec<[f64; 2]>],
    z: u8,
    x: u32,
    y: u32,
    idx: &WaterIndex,
    pid: u32,
    out: &mut std::collections::BTreeMap<(u32, u32), Vec<(u32, Piece)>>,
    full: &mut Vec<(u8, u32, u32)>,
) {
    // Skip tiles that do not touch the index grid.
    let shift = INDEX_ZOOM - z;
    let (ax, ay, span) = (x << shift, y << shift, 1u32 << shift);
    if ax + span <= idx.x0 || ay + span <= idx.y0 || ax >= idx.x0 + idx.nx || ay >= idx.y0 + idx.ny {
        return;
    }
    let rect = tile_rect(x, y, z);
    let clipped: Vec<Vec<[f64; 2]>> = rings.iter().map(|r| clip_ring(r, rect)).filter(|r| !r.is_empty()).collect();
    if clipped.is_empty() {
        return;
    }
    let real = clipped.iter().any(|r| (0..r.len()).any(|i| !on_same_side(r[i], r[(i + 1) % r.len()], rect)));
    if !real {
        // Only rectangle edges: every ring is the whole tile or nothing; odd count = inside.
        let area = (rect[2] - rect[0]) * (rect[3] - rect[1]);
        let fulls = clipped.iter().filter(|r| signed_area(r).abs() > 0.5 * area).count();
        if fulls % 2 == 1 {
            full.push((z, x, y));
        }
        return;
    }
    if z == INDEX_ZOOM {
        out.entry((x, y)).or_default().push((pid, Piece::Rings(clipped)));
        return;
    }
    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        descend(&clipped, z + 1, 2 * x + dx, 2 * y + dy, idx, pid, out, full);
    }
}

impl MixedCell {
    fn new(rect: Rect, polys: Vec<u32>, rings: Vec<ClippedRing>) -> Self {
        let h = (rect[3] - rect[1]) / BANDS as f64;
        let band = |lat: f64| (((lat - rect[1]) / h).floor().max(0.0) as usize).min(BANDS - 1);
        let mut per: Vec<Vec<(u32, u32)>> = vec![Vec::new(); BANDS];
        for (ri, r) in rings.iter().enumerate() {
            let n = r.pts.len();
            for i in 0..n {
                let (a, b) = (r.pts[i], r.pts[(i + 1) % n]);
                if a[1] == b[1] {
                    continue; // horizontal: never crossed by a horizontal ray (half-open rule)
                }
                for v in per.iter_mut().take(band(a[1].max(b[1])) + 1).skip(band(a[1].min(b[1]))) {
                    v.push((ri as u32, i as u32));
                }
            }
        }
        let mut band_start = [0u32; BANDS + 1];
        let mut band_edges = Vec::new();
        for (k, v) in per.into_iter().enumerate() {
            band_edges.extend(v);
            band_start[k + 1] = band_edges.len() as u32;
        }
        Self { rect, polys, rings, band_start, band_edges }
    }

    /// Even-odd test per polygon against the edges of the point's band: the lowest polygon id
    /// containing the point.
    fn test(&self, p: [f64; 2]) -> Option<u32> {
        self.test_where(p, |_| true)
    }

    /// [`MixedCell::test`] among the polygons `keep` accepts. A point on (or within 1e-9 cell
    /// sizes of) the cell's border is tested just inside it: the clipped rings run along the
    /// border, where the half-open ray test would count a water cell's edge as outside (a point
    /// exactly on a cell row's boundary, e.g. a terrain lattice sample, read as land).
    fn test_where(&self, p: [f64; 2], keep: impl Fn(u32) -> bool) -> Option<u32> {
        let r = self.rect;
        let e = [(r[2] - r[0]) * 1e-9, (r[3] - r[1]) * 1e-9];
        let p = [p[0].clamp(r[0] + e[0], r[2] - e[0]), p[1].clamp(r[1] + e[1], r[3] - e[1])];
        let h = (self.rect[3] - self.rect[1]) / BANDS as f64;
        let b = (((p[1] - self.rect[1]) / h).floor().max(0.0) as usize).min(BANDS - 1);
        let mut parity = [0u64; 4];
        let mut overflow: Vec<bool> = Vec::new();
        if self.polys.len() > 256 {
            overflow = vec![false; self.polys.len()];
        }
        for &(ri, i) in &self.band_edges[self.band_start[b] as usize..self.band_start[b + 1] as usize] {
            let r = &self.rings[ri as usize];
            let n = r.pts.len();
            let (a, c) = (r.pts[i as usize], r.pts[(i as usize + 1) % n]);
            if (a[1] > p[1]) != (c[1] > p[1]) {
                let x = a[0] + (p[1] - a[1]) / (c[1] - a[1]) * (c[0] - a[0]);
                if x > p[0] {
                    let l = r.local as usize;
                    if overflow.is_empty() {
                        parity[l / 64] ^= 1 << (l % 64);
                    } else {
                        overflow[l] = !overflow[l];
                    }
                }
            }
        }
        let first = if overflow.is_empty() {
            (0..self.polys.len()).find(|&l| parity[l / 64] >> (l % 64) & 1 == 1 && keep(self.polys[l]))
        } else {
            (0..self.polys.len()).find(|&l| overflow[l] && keep(self.polys[l]))
        };
        first.map(|l| self.polys[l])
    }
}

#[cfg(test)]
#[path = "water_tests.rs"]
mod tests;
