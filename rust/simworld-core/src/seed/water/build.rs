//! Building the index: quadtree descent of every polygon into z11 cells, distance rasters.

use super::{tile_rect, ClippedRing, MixedCell, Piece, Raster, WaterBody, WaterIndex, WaterParams, INDEX_ZOOM, MIXED, NONE};
use crate::geo::{lat_to_tile_y, lon_to_tile_x, tile_y_to_lat};
use crate::seed::files::{q_cm, Outside, WaterFile, WaterKind};
use crate::seed::geom::{clip_ring, m_per_deg_lon, on_same_side, point_segment_dist, signed_area, M_PER_DEG};

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
}

/// Distance (m) from a lon/lat point to a lon/lat segment, in an equirectangular frame at the
/// point's latitude (the same for every cell that evaluates this point).
#[inline]
pub(super) fn dist_m(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
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
