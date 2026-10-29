//! Landuse point-in-polygon index.

use super::{ring_area2, Landuse};

struct LandusePoly {
    class: Landuse,
    /// min x, min y, max x, max y (pixels).
    bbox: [f64; 4],
    area: f64,
    id: u64,
    rings: Vec<Vec<[f64; 2]>>,
}

/// Uniform grid over the tile (plus buffer) with bbox-prefiltered candidates per cell, so a
/// dense z13 tile (4.5k buildings, hundreds of landuse polygons) resolves in well under 1 ms.
pub struct LanduseIndex {
    polys: Vec<LandusePoly>,
    cells: Vec<Vec<u32>>,
    min: f64,
    cell: f64,
}

const LANDUSE_GRID: usize = 16;

/// One input polygon of [`LanduseIndex::new`]: class, OSM id, rings (first = exterior).
type LanduseInput = (Landuse, u64, Vec<Vec<[f64; 2]>>);

/// Even-odd point-in-polygon over all rings (holes included).
pub fn point_in_rings(p: [f64; 2], rings: &[Vec<[f64; 2]>]) -> bool {
    let mut inside = false;
    for ring in rings {
        let n = ring.len();
        if n < 3 {
            continue;
        }
        let mut j = n - 1;
        for i in 0..n {
            let (a, b) = (ring[i], ring[j]);
            if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
                inside = !inside;
            }
            j = i;
        }
    }
    inside
}

impl LanduseIndex {
    pub fn empty() -> Self {
        Self { polys: Vec::new(), cells: Vec::new(), min: 0.0, cell: 1.0 }
    }

    /// `extent` = layer extent; the grid spans [-extent/8, extent*9/8] to cover the buffer.
    pub fn new(polys_in: Vec<LanduseInput>, extent: f64) -> Self {
        let min = -extent / 8.0;
        let cell = extent * 1.25 / LANDUSE_GRID as f64;
        let mut polys = Vec::with_capacity(polys_in.len());
        let mut cells = vec![Vec::new(); LANDUSE_GRID * LANDUSE_GRID];
        for (class, id, rings) in polys_in {
            if rings.is_empty() || rings[0].len() < 3 {
                continue;
            }
            let mut bbox = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
            for p in &rings[0] {
                bbox[0] = bbox[0].min(p[0]);
                bbox[1] = bbox[1].min(p[1]);
                bbox[2] = bbox[2].max(p[0]);
                bbox[3] = bbox[3].max(p[1]);
            }
            let area = ring_area2(&rings[0]).abs() - rings[1..].iter().map(|r| ring_area2(r).abs()).sum::<f64>();
            let idx = polys.len() as u32;
            let c0 = Self::cell_of(min, cell, bbox[0]);
            let c1 = Self::cell_of(min, cell, bbox[2]);
            let r0 = Self::cell_of(min, cell, bbox[1]);
            let r1 = Self::cell_of(min, cell, bbox[3]);
            for r in r0..=r1 {
                for c in c0..=c1 {
                    cells[r * LANDUSE_GRID + c].push(idx);
                }
            }
            polys.push(LandusePoly { class, bbox, area, id, rings });
        }
        Self { polys, cells, min, cell }
    }

    fn cell_of(min: f64, cell: f64, v: f64) -> usize {
        (((v - min) / cell).floor().max(0.0) as usize).min(LANDUSE_GRID - 1)
    }

    pub fn len(&self) -> usize {
        self.polys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.polys.is_empty()
    }

    /// Landuse at a pixel position: the smallest containing polygon (most specific), ties by id.
    pub fn lookup(&self, p: [f64; 2]) -> Option<Landuse> {
        if self.polys.is_empty() {
            return None;
        }
        let c = Self::cell_of(self.min, self.cell, p[0]);
        let r = Self::cell_of(self.min, self.cell, p[1]);
        let mut best: Option<&LandusePoly> = None;
        for &i in &self.cells[r * LANDUSE_GRID + c] {
            let lp = &self.polys[i as usize];
            let b = lp.bbox;
            if p[0] < b[0] || p[0] > b[2] || p[1] < b[1] || p[1] > b[3] {
                continue;
            }
            if let Some(cur) = best {
                if (lp.area, lp.id) >= (cur.area, cur.id) {
                    continue;
                }
            }
            if point_in_rings(p, &lp.rings) {
                best = Some(lp);
            }
        }
        best.map(|lp| lp.class)
    }
}
