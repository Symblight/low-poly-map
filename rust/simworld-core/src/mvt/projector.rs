//! Tile pixels -> ENU metres with terrain heights, for a whole tile or a chunk cut from it.

use super::{centroid_px, point_in_rings, ring_area2, Building, PxPolygons, RawBuilding, ENCODER_BUFFER_FRACTION};
use crate::geo::{Frame, TileKey};
use crate::terrain::Terrain;

/// Largest spacing (m) of the base samples along a footprint edge ([`TileProjector::base_height`]).
pub const BASE_EDGE_STEP_M: f64 = 10.0;
/// At most this many parts per footprint edge (long walls of huge buildings).
const BASE_EDGE_MAX_PARTS: usize = 64;
/// Level of the global base-sampling lattice: points `i / 2^level` in fractional Mercator
/// coordinates (2^-21 of the circumference: 19 m Mercator, about 11 m on the ground at 55 deg).
pub const BASE_LATTICE_LEVEL: i32 = 21;
/// Most lattice points per footprint bounding box (the lattice coarsens by 2 until it fits).
const BASE_LATTICE_MAX: i64 = 64;

/// Projects tile pixels to ENU relative to the tile center, with the terrain height there.
/// Workers get their heights only through this (`Terrain` is part of the job): flat (0) unless
/// built [`TileProjector::with_terrain`].
///
/// Chunks (Tiles v2, step T2): a projector can be [`TileProjector::cut`] to a finer tile (a
/// render chunk) inside its pixel tile: pixels stay those of the data tile (exact Web Mercator,
/// so the chunk rectangle is axis-aligned and seams clip bit for bit), while the output origin,
/// the clip rectangle, the band and the metres per pixel follow the chunk.
#[derive(Clone, Debug)]
pub struct TileProjector {
    /// The tile the pixels belong to (the data tile of a cut projector).
    key: TileKey,
    frame: Frame,
    extent: f64,
    /// Output origin: center of the target tile (the chunk of a cut projector).
    center: [f64; 2],
    terrain: Terrain,
    /// The target tile (`key` unless cut) and its rectangle in `key`'s pixels.
    target: TileKey,
    rect: crate::mesher::ClipRect,
}

impl TileProjector {
    /// A projector over flat terrain (every height 0).
    pub fn new(key: TileKey, frame: Frame, extent: f64) -> Self {
        Self {
            key,
            frame,
            extent,
            center: frame.tile_center(key),
            terrain: Terrain::flat(),
            target: key,
            rect: crate::mesher::ClipRect::tile(extent),
        }
    }
    /// The same pixels (of this projector's tile) seen from `chunk`, a tile at or inside it:
    /// output relative to the chunk center, clip rectangle = the chunk, band and metres per
    /// pixel of the chunk. `None` if `chunk` is not inside this projector's pixel tile.
    pub fn cut(&self, chunk: TileKey) -> Option<Self> {
        if chunk.z < self.key.z || chunk.z - self.key.z > 20 {
            return None;
        }
        let dz = chunk.z - self.key.z;
        if (chunk.x >> dz, chunk.y >> dz) != (self.key.x, self.key.y) {
            return None;
        }
        let n = (1u64 << dz) as f64;
        let s = self.extent / n;
        let (ix, iy) = ((chunk.x - (self.key.x << dz)) as f64, (chunk.y - (self.key.y << dz)) as f64);
        let rect = crate::mesher::ClipRect { x0: ix * s, y0: iy * s, x1: (ix + 1.0) * s, y1: (iy + 1.0) * s };
        Some(Self { center: self.frame.tile_center(chunk), target: chunk, rect, ..self.clone() })
    }
    /// The target tile: the projector's own tile, or the chunk it was [`TileProjector::cut`] to.
    /// Bands, drape margins and airport fields follow it.
    #[allow(clippy::misnamed_getters)]
    pub fn key(&self) -> TileKey {
        self.target
    }
    /// The target tile's rectangle in pixels (the whole tile unless cut).
    pub fn rect(&self) -> crate::mesher::ClipRect {
        self.rect
    }
    /// Whether this is a chunk cut from a larger pixel tile.
    pub fn is_cut(&self) -> bool {
        self.target != self.key
    }
    /// Ownership of a building by its centroid, and its centroid relative to the target tile:
    /// the decode result (`owned`, `centroid`) for a whole tile; for a chunk, the chunk whose
    /// half-open rectangle contains the centroid (each building belongs to exactly one of the
    /// chunks of its data tile).
    pub fn place(&self, b: &Building) -> Option<[f32; 2]> {
        if !self.is_cut() {
            return b.owned.then_some(b.centroid);
        }
        let (r, c) = (&self.rect, b.centroid_px);
        (c[0] >= r.x0 && c[0] < r.x1 && c[1] >= r.y0 && c[1] < r.y1).then(|| self.project(c))
    }
    /// The same projector sampling `terrain` for every height.
    pub fn with_terrain(mut self, terrain: Terrain) -> Self {
        self.terrain = terrain;
        self
    }
    pub fn terrain(&self) -> &Terrain {
        &self.terrain
    }
    /// WGS84 lat/lon of a tile pixel.
    pub fn lat_lon(&self, px: [f64; 2]) -> (f64, f64) {
        self.key.frac_to_lat_lon(px[0] / self.extent, px[1] / self.extent)
    }
    pub fn extent(&self) -> f64 {
        self.extent
    }
    pub fn center(&self) -> [f64; 2] {
        self.center
    }
    /// Tile pixel -> (east, north) meters relative to the tile center (f64).
    pub fn project64(&self, px: [f64; 2]) -> [f64; 2] {
        self.project_h(px).0
    }
    /// Tile pixel -> ((east, north) relative to the tile center, terrain height there).
    pub fn project_h(&self, px: [f64; 2]) -> ([f64; 2], f64) {
        let (lat, lon) = self.lat_lon(px);
        let [e, n] = self.frame.enu(lat, lon);
        ([e - self.center[0], n - self.center[1]], self.terrain.height(lat, lon))
    }
    /// [`TileProjector::project_h`] with the water-snapped height (`Terrain::water_height`).
    pub fn project_water(&self, px: [f64; 2]) -> ([f64; 2], f64) {
        let (lat, lon) = self.lat_lon(px);
        let [e, n] = self.frame.enu(lat, lon);
        ([e - self.center[0], n - self.center[1]], self.terrain.water_height(lat, lon))
    }
    /// Terrain height (m) at a tile pixel.
    pub fn ground_at(&self, px: [f64; 2]) -> f64 {
        let (lat, lon) = self.lat_lon(px);
        self.terrain.height(lat, lon)
    }
    /// Building base (m), api 1.7: the HIGHEST terrain height under the footprint as decoded
    /// (before the mesher's tile clipping and simplification, so every band and every tile the
    /// building is meshed in samples the same outline; only the encoder's own buffer cut,
    /// `Building::clipped`, can differ): every outer-ring vertex, points along every outer edge
    /// at most [`BASE_EDGE_STEP_M`] apart (at least the midpoint), the centroid when it lies
    /// inside, and the points of a global Mercator lattice inside the footprint
    /// ([`BASE_LATTICE_LEVEL`], coarsened by powers of two to at most [`BASE_LATTICE_MAX`]
    /// points), so a hill top inside a large footprint counts. With the terrain off, the height
    /// at `centroid` (0, nothing else is sampled).
    pub fn base_height(&self, polygons: &PxPolygons, centroid: [f64; 2]) -> f64 {
        if !self.terrain.is_active() {
            return self.ground_at(centroid);
        }
        let mut hi = f64::NEG_INFINITY;
        let step_px = BASE_EDGE_STEP_M / self.m_per_px();
        let (mut b0, mut b1) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for p in polygons {
            let Some(outer) = p.first() else { continue };
            let n = outer.len();
            for i in 0..n {
                let (a, b) = (outer[i], outer[(i + 1) % n]);
                hi = hi.max(self.ground_at(a));
                for k in 0..2 {
                    b0[k] = b0[k].min(a[k]);
                    b1[k] = b1[k].max(a[k]);
                }
                let len = crate::math::hypot(b[0] - a[0], b[1] - a[1]);
                let parts = ((len / step_px).ceil() as usize).clamp(2, BASE_EDGE_MAX_PARTS);
                for j in 1..parts {
                    let t = j as f64 / parts as f64;
                    hi = hi.max(self.ground_at([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]));
                }
            }
        }
        if !hi.is_finite() {
            return self.ground_at(centroid);
        }
        let inside = |q: [f64; 2]| polygons.iter().any(|p| point_in_rings(q, p));
        if inside(centroid) {
            hi = hi.max(self.ground_at(centroid));
        }
        // Global lattice (fractional Mercator coordinates i / 2^level), the same points at every
        // zoom: tile pixel = (i / 2^level * 2^z - x) * extent.
        let (z, ext) = (self.key.z as i32, self.extent);
        let to_g = |px: f64, t: u32| (t as f64 + px / ext) / (1u64 << z) as f64;
        let (gx0, gx1) = (to_g(b0[0], self.key.x), to_g(b1[0], self.key.x));
        let (gy0, gy1) = (to_g(b0[1], self.key.y), to_g(b1[1], self.key.y));
        let mut level = BASE_LATTICE_LEVEL;
        let range = |lv: i32| {
            let s = (1u64 << lv) as f64;
            let (ix0, ix1) = ((gx0 * s).ceil() as i64, (gx1 * s).floor() as i64);
            let (iy0, iy1) = ((gy0 * s).ceil() as i64, (gy1 * s).floor() as i64);
            (ix0, ix1, iy0, iy1)
        };
        let mut r = range(level);
        while level > 0 && (r.1 - r.0 + 1).max(0) * (r.3 - r.2 + 1).max(0) > BASE_LATTICE_MAX {
            level -= 1;
            r = range(level);
        }
        let s = (1u64 << level) as f64;
        let zs = (1u64 << z) as f64;
        for iy in r.2..=r.3 {
            for ix in r.0..=r.1 {
                let q = [(ix as f64 / s * zs - self.key.x as f64) * ext, (iy as f64 / s * zs - self.key.y as f64) * ext];
                if inside(q) {
                    hi = hi.max(self.ground_at(q));
                }
            }
        }
        hi
    }
    pub fn project(&self, px: [f64; 2]) -> [f32; 2] {
        let [e, n] = self.project64(px);
        [e as f32, n as f32]
    }
    /// Meters per pixel at the target tile's center (for tolerances and areas).
    pub fn m_per_px(&self) -> f64 {
        if self.is_cut() {
            let (lat, _) = self.target.center_lat_lon();
            let px = self.rect.x1 - self.rect.x0;
            return crate::geo::tile_width_m(lat, self.target.z) / px;
        }
        let (lat, _) = self.key.center_lat_lon();
        crate::geo::tile_width_m(lat, self.key.z) / self.extent
    }

    pub fn finish(&self, raw: RawBuilding) -> Option<Building> {
        let c = centroid_px(&raw.polygons)?;
        let owned = c[0] >= 0.0 && c[0] < self.extent && c[1] >= 0.0 && c[1] < self.extent;
        let mut polygons = raw.polygons;
        polygons.retain(|p| !p.is_empty() && p[0].len() >= 3);
        if polygons.is_empty() {
            return None;
        }
        // A vertex on (or beyond) the encoder's buffer line: the footprint was cut there.
        let buf = self.extent * ENCODER_BUFFER_FRACTION;
        let (lo, hi) = (-buf + 1.0, self.extent + buf - 1.0);
        let clipped = polygons
            .iter()
            .flatten()
            .flatten()
            .any(|p| p[0] <= lo || p[0] >= hi || p[1] <= lo || p[1] >= hi);
        let area_px2: f64 = polygons
            .iter()
            .map(|p| ring_area2(&p[0]).abs() - p[1..].iter().map(|h| ring_area2(h).abs()).sum::<f64>())
            .sum::<f64>()
            * 0.5;
        let mpp = self.m_per_px();
        let base_m = self.base_height(&polygons, c) as f32;
        Some(Building {
            osm_type: raw.osm_type,
            id: raw.id,
            kind: raw.kind,
            height: raw.height,
            levels: raw.levels,
            min_height: raw.min_height,
            landuse: raw.landuse,
            tags: raw.tags,
            polygons,
            clipped,
            footprint_m2: (area_px2.max(0.0) * mpp * mpp) as f32,
            centroid_px: c,
            centroid: self.project(c),
            base_m,
            owned,
        })
    }
}
