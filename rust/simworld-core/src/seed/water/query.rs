//! Queries of the index: water test, terrain rule, tile polygons, distances, warnings, sizes.

use super::build::dist_m;
use super::{ClippedRing, MixedCell, Raster, WaterBody, WaterHit, WaterIndex, WaterParams, WaterStats, INDEX_ZOOM, MIXED, NONE};
use crate::geo::{lon_to_tile_x, tile_x_to_lon, tile_y_to_lat};
use crate::seed::files::WaterKind;
use crate::seed::geom::{m_per_deg_lon, on_same_side, Rect, M_PER_DEG};
use crate::terrain::noise::smoothstep;

impl WaterIndex {
    pub(super) fn grid_index(&self, cx: u32, cy: u32) -> Option<usize> {
        (cx >= self.x0 && cy >= self.y0 && cx < self.x0 + self.nx && cy < self.y0 + self.ny)
            .then(|| ((cy - self.y0) * self.nx + (cx - self.x0)) as usize)
    }

    pub fn bodies(&self) -> &[WaterBody] {
        &self.bodies
    }

    pub fn params(&self) -> &WaterParams {
        &self.params
    }

    /// Whether the terrain water rule can change a height anywhere in tile `key` (any zoom):
    /// the tile overlaps an index cell holding water or a blend raster, or reaches outside the
    /// file's bbox where `outside` is the sea. Conservative (cell resolution, rivers included)
    /// and hierarchical: a tile touches whenever one of its children does (Terrain v2 G1, the
    /// terrain error proxy's edit test).
    pub fn touches_tile(&self, key: crate::geo::TileKey) -> bool {
        let (n, w) = key.frac_to_lat_lon(0.0, 0.0);
        let (s, e) = key.frac_to_lat_lon(1.0, 1.0);
        let b = self.bbox;
        if self.outside_sea && !(w >= b[0] && e <= b[2] && s >= b[1] && n <= b[3]) {
            return true;
        }
        let z = INDEX_ZOOM;
        let (cx0, cx1, cy0, cy1) = if key.z >= z {
            let sh = key.z - z;
            (key.x >> sh, key.x >> sh, key.y >> sh, key.y >> sh)
        } else {
            let sh = z - key.z;
            let span = (1u32 << sh) - 1;
            (key.x << sh, (key.x << sh) + span, key.y << sh, (key.y << sh) + span)
        };
        let (gx0, gy0) = (cx0.max(self.x0), cy0.max(self.y0));
        let (gx1, gy1) = (cx1.min(self.x0 + self.nx - 1), cy1.min(self.y0 + self.ny - 1));
        if gx0 > gx1 || gy0 > gy1 {
            return false;
        }
        for cy in gy0..=gy1 {
            for cx in gx0..=gx1 {
                if let Some(g) = self.grid_index(cx, cy) {
                    if self.grid[g] != 0 || self.raster_of[g] != NONE {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn in_bbox(&self, lat: f64, lon: f64) -> bool {
        lon >= self.bbox[0] && lon <= self.bbox[2] && lat >= self.bbox[1] && lat <= self.bbox[3]
    }

    /// Grid index of the cell holding a point (`None` outside the grid). The column is the same
    /// as `floor(lon_to_tile_x)`; the row is found among the rows' edge latitudes (identical to
    /// the tile rectangles the cells were clipped with).
    pub(super) fn cell_of(&self, lat: f64, lon: f64) -> Option<usize> {
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
    pub(super) fn in_rule(&self, p: u32) -> bool {
        self.params.rivers_flat || self.bodies[p as usize].kind != WaterKind::River
    }

    /// Water level of the body the terrain rule puts the point in (`None` = land for the rule).
    pub(super) fn rule_level(&self, cell: Option<usize>, lat: f64, lon: f64) -> Option<f64> {
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

    pub(super) fn cell_rect(&self, g: usize) -> Rect {
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
    pub(super) fn edges_around(&self, cx: i64, cy: i64) -> Vec<(u32, [f64; 2], [f64; 2], Rect)> {
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

    /// Section 4.6 warnings that need the water: a city centre in water, a city footprint more
    /// than 50 % water (sampled on a 41 x 41 grid over the footprint circle).
    pub fn city_warnings(&self, cities: &[crate::seed::files::City]) -> Vec<String> {
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
