//! Terrain tiles from the TIN (M9 phase 2, `docs/spec-terrain.md` 5.3; split out of `tin.rs`):
//! the tile's height patch -> [`Delatin`] -> sink under the samples -> vertices (tile-local
//! metres relative to the tile centre, the projection of `TileProjector`) -> skirts, optional
//! smooth normals and seed COLOR. The build can be stopped between its stages and while the
//! patch is sampled ([`build_terrain_tile_until`]), so a cancelled job does not finish.

use crate::geo::{Frame, TileKey, MERCATOR_CIRCUMFERENCE};
use crate::mesher::MeshData;
use crate::profile::{Profile, TerrainBand, TerrainShading};

use super::super::sample::{lattice_lat_lon, lattice_lat_lon_f};
use super::super::Terrain;
use super::Delatin;

/// Upper bound of the automatic skirt depth (m).
pub const SKIRT_MAX_M: f64 = 60.0;

/// Cells per tile side of the height patch for a zoom: about 22 m spacing at z12..z14
/// (z14 64, z13 128, z12 256 cells; spec table 5.2), at most 256 (z11 44 m, z10 88 m), at
/// least 16 (z16).
pub fn patch_cells(z: u8) -> usize {
    let shift = (z as i32 - 10).clamp(0, 6) as u32;
    (1024usize >> shift).clamp(16, 256)
}

/// Cells per side of the height patch of a tile of zoom `z` in `band`: the band's
/// `patch_cells`, or [`patch_cells`] of the zoom when that is 0 (perf review 2 item 3: the
/// coarse bands sample 128 cells, a quarter of the heights; a function of the zoom and the
/// profile only, so neighbours share their lattice).
pub fn band_cells(z: u8, band: &TerrainBand) -> usize {
    if band.patch_cells > 0 {
        band.patch_cells as usize
    } else {
        patch_cells(z)
    }
}

/// [`band_cells`] of tile `key`, at least the zoom's [`patch_cells`] on a tile a seed world's
/// airport touches (contract section 9 "Airports": the TIN is exact to 1 cm on the levelled
/// fence, which the coarse lattice does not resolve).
pub fn tile_cells(key: TileKey, band: &TerrainBand, terrain: &Terrain) -> usize {
    let cells = band_cells(key.z, band);
    if terrain.lift_field_touches(key) {
        cells.max(patch_cells(key.z))
    } else {
        cells
    }
}

/// Seed positions along one side: `count` grid indices from 0 to `last`, evenly spread.
pub(super) fn seed_line(count: usize, last: usize) -> Vec<usize> {
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
    build_terrain_tile_until(key, ring, frame, terrain, profile, detail, color, &|| false).map(|t| t.expect("never stopped"))
}

/// Rows of the height patch sampled between two `stop` checks.
const STOP_ROWS: usize = 16;

/// [`build_terrain_tile_colored`] that checks `stop` while sampling the height patch (every
/// [`STOP_ROWS`] rows, most of the cost) and between the TIN, the sink and the assembly;
/// `Ok(None)` = stopped (a cancelled job, perf review 2 item 3). Same bytes when it runs through.
#[allow(clippy::too_many_arguments)]
pub fn build_terrain_tile_until(
    key: TileKey,
    ring: usize,
    frame: Frame,
    terrain: &Terrain,
    profile: &Profile,
    detail: f64,
    color: Option<VertexColor<'_>>,
    stop: &dyn Fn() -> bool,
) -> Result<Option<TerrainTile>, String> {
    let tp = &profile.terrain;
    let band = tp.bands.get(ring).ok_or_else(|| format!("terrain ring {ring} has no band"))?;
    let cells = tile_cells(key, band, terrain);
    let n = cells + 1;
    // The patch's lattice (`terrain::sample`): sample (i, j) of `cells` per side.
    let mut heights = Vec::with_capacity(n * n);
    for j in 0..n as i64 {
        if (j as usize).is_multiple_of(STOP_ROWS) && stop() {
            return Ok(None);
        }
        for i in 0..n as i64 {
            let (lat, lon) = lattice_lat_lon(key, cells as u32, i, j);
            heights.push(terrain.height(lat, lon) as f32);
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
        let floor = (super::super::LIFT_FIELD_MIN_ERROR_M / max_error.max(1e-9)).min(1.0);
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
    if stop() {
        return Ok(None);
    }
    // Sink (contract section 9a, api 1.7): the vertices go down until the mesh lies on or under
    // every grid sample (`Delatin::vertex_sinks`), so it never lies above the height function
    // the vector world is draped on, at any detail; a sink is at most the TIN's error. A seed
    // world's sea and lake vertices are pinned at their water level.
    let pinned: Option<Vec<bool>> = terrain.seed().map(|sd| {
        tin.coords()
            .iter()
            .map(|c| {
                let (lat, lon) = lattice_lat_lon(key, cells as u32, c[0], c[1]);
                sd.water_level(lat, lon).is_some()
            })
            .collect()
    });
    let sinks = tin.vertex_sinks(pinned.as_deref());
    if stop() {
        return Ok(None);
    }
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
    Ok(Some(assemble(key, cells, frame, &sunk, &tin, skirt, smooth, left, color)))
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

