//! Countryside ground (`docs/spec-world-seed.md` section 5.2): forest / farmland / meadow patches
//! on the z15 grid (about 700 m at 55 N), with warped edges so fields are not squares.
//!
//! * **Patch geometry.** Patch `(cx, cy)` is the z15 cell with its four corners moved by
//!   `edge_warp` (hash of the lattice point) and two extra points on each edge moved across it
//!   (hash of the edge). Every corner and edge point is a function of its lattice point / edge
//!   alone, so neighbouring patches share their boundary exactly (no gaps, no overlaps), and the
//!   coordinates are global z15 Mercator units: a tile at any zoom converts them with a power of
//!   two (exact), so z13..z16 agree bit for bit.
//! * **Class.** Two octaves of value noise on the patch grid (lattice spacing `noise_cells`, hash
//!   of the lattice point, bilinear) plus the patch's own hash, thresholded: forest below
//!   `forest_share` (`water_forest_share` within `water_distance_m` of water), else meadow with
//!   `meadow_share`, else farmland. `farmland` and `meadow` both resolve to the grass class, so
//!   every patch also gets a **tint** (ground COLOR `g` = tint / 8; 0..`tint_levels - 1`).
//! * Seeded by `H(root, "countryside")`: `world_seed` re-rolls the countryside, nothing else does.

use super::hash::{unit, Seed};
use crate::geo::TileKey;
use crate::profile::SeedCountryside;

/// Zoom of the patch grid.
pub const PATCH_ZOOM: u8 = 15;

/// Countryside kinds (OSM landuse values, resolved through the profile's `ground.areas`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PatchKind {
    Forest,
    Farmland,
    Meadow,
}

impl PatchKind {
    pub fn landuse(self) -> &'static str {
        match self {
            PatchKind::Forest => "forest",
            PatchKind::Farmland => "farmland",
            PatchKind::Meadow => "meadow",
        }
    }
}

/// The countryside of one world.
#[derive(Clone, Debug, PartialEq)]
pub struct Countryside {
    seed: Seed,
    p: SeedCountryside,
}

impl Countryside {
    pub fn new(seed: Seed, p: SeedCountryside) -> Self {
        Self { seed, p }
    }

    pub fn params(&self) -> &SeedCountryside {
        &self.p
    }

    /// Displaced lattice point `(gx, gy)` (z15 units).
    fn corner(&self, gx: i64, gy: i64) -> [f64; 2] {
        let w = self.p.edge_warp;
        let h = crate::seed_hash!(self.seed, "corner", gx, gy);
        let dx = (unit(h) * 2.0 - 1.0) * w;
        let dy = (unit(crate::mvt::splitmix64(h)) * 2.0 - 1.0) * w;
        [gx as f64 + dx, gy as f64 + dy]
    }

    /// The two interior points of the edge from lattice point `(gx, gy)` to its east
    /// (`vertical` false) or south (`vertical` true) neighbour, in that direction.
    fn edge_points(&self, gx: i64, gy: i64, vertical: bool) -> [[f64; 2]; 2] {
        let a = self.corner(gx, gy);
        let b = if vertical { self.corner(gx, gy + 1) } else { self.corner(gx + 1, gy) };
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        // Perpendicular (not normalized: the edge is about one unit long).
        let n = [-dy, dx];
        let h = crate::seed_hash!(self.seed, "edge", gx, gy, vertical as u32);
        let o1 = (unit(h) * 2.0 - 1.0) * self.p.edge_warp * 0.5;
        let o2 = (unit(crate::mvt::splitmix64(h)) * 2.0 - 1.0) * self.p.edge_warp * 0.5;
        let at = |t: f64, o: f64| [a[0] + dx * t + n[0] * o, a[1] + dy * t + n[1] * o];
        [at(1.0 / 3.0, o1), at(2.0 / 3.0, o2)]
    }

    /// Ring of patch `(cx, cy)` in z15 units (y down): clockwise on screen like an MVT exterior.
    pub fn patch_ring(&self, cx: i64, cy: i64) -> Vec<[f64; 2]> {
        let mut r = Vec::with_capacity(12);
        // Top edge, west -> east.
        r.push(self.corner(cx, cy));
        r.extend(self.edge_points(cx, cy, false));
        // East edge, north -> south.
        r.push(self.corner(cx + 1, cy));
        r.extend(self.edge_points(cx + 1, cy, true));
        // Bottom edge, east -> west (the canonical edge runs west -> east: reversed).
        r.push(self.corner(cx + 1, cy + 1));
        let [p, q] = self.edge_points(cx, cy + 1, false);
        r.extend([q, p]);
        // West edge, south -> north (reversed).
        r.push(self.corner(cx, cy + 1));
        let [p, q] = self.edge_points(cx, cy, true);
        r.extend([q, p]);
        r
    }

    /// Low-frequency noise of a lattice point of the class field, in [0, 1).
    fn lattice(&self, octave: u32, ix: i64, iy: i64) -> f64 {
        unit(crate::seed_hash!(self.seed, "forest", octave, ix, iy))
    }

    /// Bilinear value noise at patch `(cx, cy)` with lattice spacing `cells` (in patches).
    fn noise(&self, octave: u32, cx: i64, cy: i64, cells: f64) -> f64 {
        let (fx, fy) = ((cx as f64 + 0.5) / cells, (cy as f64 + 0.5) / cells);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let (ix, iy) = (x0 as i64, y0 as i64);
        let a = self.lattice(octave, ix, iy) + (self.lattice(octave, ix + 1, iy) - self.lattice(octave, ix, iy)) * tx;
        let b = self.lattice(octave, ix, iy + 1) + (self.lattice(octave, ix + 1, iy + 1) - self.lattice(octave, ix, iy + 1)) * tx;
        a + (b - a) * ty
    }

    /// Kind and tint of patch `(cx, cy)`. `near_water` = within `water_distance_m` of water.
    pub fn classify(&self, cx: i64, cy: i64, near_water: bool) -> (PatchKind, u8) {
        let cells = self.p.noise_cells.max(1.0);
        let own = unit(crate::seed_hash!(self.seed, "patch", cx, cy));
        let v = 0.55 * self.noise(0, cx, cy, cells) + 0.25 * self.noise(1, cx, cy, (cells / 3.0).max(1.0)) + 0.2 * own;
        // v is a weighted mean of uniforms (mean 0.5, narrower than uniform): stretch it back so
        // the shares come out roughly as asked.
        let v = (0.5 + (v - 0.5) * 1.9).clamp(0.0, 0.999_999);
        let share = if near_water { self.p.water_forest_share } else { self.p.forest_share };
        let kind = if v < share {
            PatchKind::Forest
        } else if unit(crate::seed_hash!(self.seed, "meadow", cx, cy)) < self.p.meadow_share {
            PatchKind::Meadow
        } else {
            PatchKind::Farmland
        };
        let levels = self.p.tint_levels.clamp(1, 8) as f64;
        let tint = ((unit(crate::seed_hash!(self.seed, "tint", cx, cy)) * levels) as u8).min(7);
        (kind, tint)
    }

    /// Patch cells `(cx0..=cx1, cy0..=cy1)` whose (warped) patches may overlap tile `key`.
    pub fn cells_for_tile(key: TileKey) -> ([i64; 2], [i64; 2]) {
        let (x0, y0, x1, y1) = if key.z >= PATCH_ZOOM {
            let sh = key.z - PATCH_ZOOM;
            ((key.x >> sh) as i64, (key.y >> sh) as i64, (key.x >> sh) as i64, (key.y >> sh) as i64)
        } else {
            let sh = PATCH_ZOOM - key.z;
            let (x, y) = ((key.x as i64) << sh, (key.y as i64) << sh);
            (x, y, x + (1 << sh) - 1, y + (1 << sh) - 1)
        };
        // Warped edges reach at most `edge_warp` (< 0.5) into the neighbour.
        ([x0 - 1, x1 + 1], [y0 - 1, y1 + 1])
    }

    /// The patch holding a point given in z15 units (tests the cell and its neighbours against
    /// the warped rings). Falls back to the plain cell.
    pub fn patch_at(&self, gx: f64, gy: f64) -> (i64, i64) {
        let (cx, cy) = (gx.floor() as i64, gy.floor() as i64);
        if self.p.edge_warp <= 0.0 {
            return (cx, cy);
        }
        for (dx, dy) in [(0, 0), (-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, -1), (-1, 1), (1, 1)] {
            let r = self.patch_ring(cx + dx, cy + dy);
            if super::geom::point_in_ring([gx, gy], &r) {
                return (cx + dx, cy + dy);
            }
        }
        (cx, cy)
    }
}

/// z15 units -> pixels of tile `key` (`extent` px per tile side): a power-of-two scale, exact.
#[inline]
pub fn z15_to_px(key: TileKey, g: [f64; 2], extent: f64) -> [f64; 2] {
    let scale = if key.z >= PATCH_ZOOM { (1u64 << (key.z - PATCH_ZOOM)) as f64 } else { 1.0 / (1u64 << (PATCH_ZOOM - key.z)) as f64 };
    [(g[0] * scale - key.x as f64) * extent, (g[1] * scale - key.y as f64) * extent]
}
