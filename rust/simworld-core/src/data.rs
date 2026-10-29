//! Data tiles (Tiles and LOD v2, steps T2a / T2b / T2c): the unit of download and decode is no
//! longer the render chunk. A data tile is fetched and decoded once ([`DataTile`]: buildings,
//! roads, landuse and the light inputs as plain Rust structs); the render chunks z/x/y the host
//! sees are cut from it in memory ([`build_chunk`]), as many as need it, without another fetch
//! or decode.
//!
//! Coordinates: a data tile keeps its geometry in its own pixel space (exact Web Mercator, y
//! down, one common extent for every layer, f64). That space is tile independent in the sense
//! that matters here: a chunk is an axis-aligned rectangle in it (a power-of-two fraction of the
//! extent), so clipping a building to a chunk is exact and the 4 children of a chunk partition it
//! bit for bit, as the per-tile clipping did before. Projection to ENU happens per chunk, relative
//! to the chunk center ([`crate::mvt::TileProjector::cut`]).
//!
//! Ownership: geometry (buildings, blocks, landmarks, ground areas, roads) is clipped exactly to
//! the chunk rectangle; per-building outputs (the light point, beacons) belong to the one chunk
//! whose half-open rectangle contains the building's centroid (an overview light point: the
//! point), so the 4 siblings never double a light; street lamps belong to the chunk containing
//! the lamp.
//!
//! Levels ([`DataLevels`]):
//! * Two-level archive (metadata `simworld_levels`, built by `simworld-cli bake`; step T2b):
//!   R0 (z16) / R1 (z15) are cut from the **detail** z14 tile (every non-landmark footprint),
//!   R2 (z14) / R3 (z13) from the **overview** z11 tile: `blocks` -> R2 building meshes,
//!   `lights` (one point per building) -> R2 / R3 light points and clusters, `roads` / `landuse`
//!   -> ground. **Landmarks** (roof >= 60 m, full geometry, overview `landmarks` layer, not in
//!   detail) are drawn either by the landmark layer (`l/z/x/y` at the overview zoom,
//!   [`build_landmark_tile`]; a host opt-in) or, without the opt-in, folded into every chunk
//!   that covers them (R0 / R1 then depend on the detail AND the overview tile). Either way every
//!   building is drawn exactly once per band.
//! * Compatibility (an archive without `simworld_levels`, zooms 13..16, [`DataLevels::COMPAT`]):
//!   detail = z14 feeds R0 / R1; R2 and R3 keep their own z14 / z13 tiles exactly as before; no
//!   landmark layer.
//! * The synthetic city (step T2c, [`DataLevels::SYNTHETIC`]): both levels generated in memory
//!   ([`synthetic_data_tile`]) and fed through the same chunk path.

use std::sync::Arc;
use std::time::Instant;

use crate::geo::{Frame, TileKey};
use crate::ground::mesh_ground;
use crate::lights::{beacon_points_in, cluster, light_points_in, LightSet};
use crate::mesher::{mesh_building_iter, MeshData};
use crate::mvt::{decode_overview_on, decode_tile_on, Building, GroundFeatures, TileProjector};
use crate::profile::Profile;
use crate::terrain::Terrain;
use crate::worker::TileData;

mod bases;
mod synthetic;
pub use bases::LazyBases;
use bases::select_idx;
pub use synthetic::{synthetic_chunk, synthetic_data_tile};

/// The level of a data tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataLevel {
    /// Every footprint (R0 / R1 chunks; R2 / R3 too in the compatibility path's own tiles).
    Detail,
    /// Blocks, light points, landmarks, coarse ground (R2 / R3 chunks, landmark tiles).
    Overview,
}

/// The data levels of a tile source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataLevels {
    /// Zoom of the detail data tiles (every footprint); chunks finer than it are cut from it.
    pub detail: u8,
    /// Zoom of the overview data tiles (two-level sources: R2 / R3 chunks and landmark tiles).
    pub overview: u8,
    /// A two-level source (`simworld_levels`): R2 / R3 come from the overview level.
    pub two_level: bool,
    /// The overview level has a `landmarks` layer (landmarks are not in detail).
    pub landmarks: bool,
}

impl DataLevels {
    /// An archive without `simworld_levels` metadata (zooms 13..16): detail z14, no overview.
    pub const COMPAT: Self = Self { detail: 14, overview: 13, two_level: false, landmarks: false };
    /// The synthetic city (T2c): detail z14 + overview z11 with landmarks, like a baked archive.
    pub const SYNTHETIC: Self = Self::two_level(14, 11, true);

    /// A two-level source with these zooms.
    pub const fn two_level(detail: u8, overview: u8, landmarks: bool) -> Self {
        Self { detail, overview, two_level: true, landmarks }
    }

    /// The detail data tile a chunk is cut from (chunks finer than the detail zoom, i.e. R0
    /// z16 and R1 z15), else `None`.
    pub fn detail_of(&self, chunk: TileKey) -> Option<TileKey> {
        if chunk.z <= self.detail {
            return None;
        }
        let dz = chunk.z - self.detail;
        Some(TileKey::new(self.detail, chunk.x >> dz, chunk.y >> dz))
    }

    /// The overview data tile a chunk is cut from (two-level sources: zooms `overview..=detail`,
    /// i.e. R2 z14 and R3 z13), else `None`.
    pub fn overview_of(&self, chunk: TileKey) -> Option<TileKey> {
        if !self.two_level || chunk.z > self.detail || chunk.z < self.overview {
            return None;
        }
        Some(self.overview_ancestor(chunk))
    }

    /// The data tile a chunk is cut from (`None`: the chunk is a tile of its own).
    pub fn data_of(&self, chunk: TileKey) -> Option<TileKey> {
        self.detail_of(chunk).or_else(|| self.overview_of(chunk))
    }

    /// The overview tile holding the landmarks of a chunk (two-level sources with landmarks).
    pub fn landmarks_of(&self, chunk: TileKey) -> Option<TileKey> {
        (self.two_level && self.landmarks && chunk.z >= self.overview).then(|| self.overview_ancestor(chunk))
    }

    fn overview_ancestor(&self, k: TileKey) -> TileKey {
        let dz = k.z - self.overview;
        TileKey::new(self.overview, k.x >> dz, k.y >> dz)
    }

    /// Level of data tile `k` of this source.
    pub fn level_of(&self, k: TileKey) -> DataLevel {
        if self.two_level && k.z == self.overview {
            DataLevel::Overview
        } else {
            DataLevel::Detail
        }
    }
}

/// One decoded data tile, shared (`Arc`) by every chunk job that is cut from it.
#[derive(Debug)]
pub struct DataTile {
    pub key: TileKey,
    pub level: DataLevel,
    /// Detail: every footprint (incl. the encoder's buffer copies). Overview: the merged blocks.
    /// Decode order (sorted). `centroid` / `owned` refer to the whole data tile; chunks use
    /// [`TileProjector::place`] instead.
    pub buildings: Vec<Building>,
    /// `buildings` whose `base_m` is still open (the synthetic overview's blocks; resolved by
    /// the chunks that mesh them). Empty: every base is final.
    pub lazy_bases: LazyBases,
    /// Pixel bounding box `[x0, y0, x1, y1]` of each building's outer rings (chunk candidates).
    pub bboxes: Vec<[f64; 4]>,
    /// Overview: the landmarks (full geometry) and their bounding boxes; empty for detail.
    pub landmarks: Vec<Building>,
    pub landmark_bboxes: Vec<[f64; 4]>,
    /// Overview: one light point per building ([`crate::mvt::light_point`]) and whether its
    /// building is one of `landmarks`; empty for detail (lights come from the footprints).
    pub lights: Vec<Building>,
    pub light_landmark: Vec<bool>,
    /// Ground areas and roads (always decoded: every band that is cut from the tile may need them).
    pub ground: GroundFeatures,
    /// Projector of the whole data tile (pixel space, terrain).
    pub proj: TileProjector,
    /// Estimated heap bytes (the memory budget's item size).
    pub bytes: u64,
    /// Worker CPU time of the decode (microseconds).
    pub cpu_us: u64,
}

pub(crate) fn bbox(b: &Building) -> [f64; 4] {
    let mut r = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for p in b.polygons.iter().filter_map(|p| p.first()).flatten() {
        r[0] = r[0].min(p[0]);
        r[1] = r[1].min(p[1]);
        r[2] = r[2].max(p[0]);
        r[3] = r[3].max(p[1]);
    }
    r
}

/// Heap estimate of a decoded tile (points dominate; a fixed overhead per feature).
fn estimate_bytes(buildings: &[Building], ground: &GroundFeatures) -> u64 {
    const PT: u64 = 16;
    const FEATURE: u64 = 160;
    let b: u64 = buildings
        .iter()
        .map(|b| FEATURE + 32 + b.kind.len() as u64 + b.polygons.iter().flatten().map(|r| r.len() as u64 * PT + 24).sum::<u64>())
        .sum();
    let a: u64 = ground.areas.iter().map(|a| FEATURE + a.rings.iter().map(|r| r.len() as u64 * PT + 24).sum::<u64>()).sum();
    let r: u64 = ground.roads.iter().map(|r| FEATURE + r.lines.iter().map(|l| l.len() as u64 * PT + 24).sum::<u64>()).sum();
    256 + b + a + r
}

impl DataTile {
    /// A detail tile from decoded parts.
    pub fn detail(key: TileKey, buildings: Vec<Building>, ground: GroundFeatures, proj: TileProjector, t0: Instant) -> Self {
        let bboxes = buildings.iter().map(bbox).collect();
        let bytes = estimate_bytes(&buildings, &ground);
        Self {
            key,
            level: DataLevel::Detail,
            buildings,
            lazy_bases: LazyBases::default(),
            bboxes,
            landmarks: Vec::new(),
            landmark_bboxes: Vec::new(),
            lights: Vec::new(),
            light_landmark: Vec::new(),
            ground,
            proj,
            bytes,
            cpu_us: t0.elapsed().as_micros() as u64,
        }
    }

    /// An overview tile from decoded parts.
    #[allow(clippy::too_many_arguments)]
    pub fn overview(
        key: TileKey,
        blocks: Vec<Building>,
        landmarks: Vec<Building>,
        lights: Vec<Building>,
        light_landmark: Vec<bool>,
        ground: GroundFeatures,
        proj: TileProjector,
        t0: Instant,
    ) -> Self {
        let bboxes = blocks.iter().map(bbox).collect();
        let landmark_bboxes = landmarks.iter().map(bbox).collect();
        let bytes = estimate_bytes(&blocks, &ground) + estimate_bytes(&landmarks, &GroundFeatures::default()) + lights.len() as u64 * 200;
        Self {
            key,
            level: DataLevel::Overview,
            buildings: blocks,
            lazy_bases: LazyBases::default(),
            bboxes,
            landmarks,
            landmark_bboxes,
            lights,
            light_landmark,
            ground,
            proj,
            bytes,
            cpu_us: t0.elapsed().as_micros() as u64,
        }
    }

    /// The bases of `buildings` are computed when a chunk meshes them (their `base_m` is a
    /// placeholder until then; [`LazyBases`]).
    pub fn with_open_bases(mut self) -> Self {
        self.lazy_bases = LazyBases::open(self.buildings.len());
        self
    }

    /// Whether the tile has landmarks (a landmark tile of it is delivered only then).
    pub fn has_landmarks(&self) -> bool {
        !self.landmarks.is_empty()
    }
}

/// Decode a detail data tile (gzip or raw MVT; empty bytes = an empty tile) on `terrain`:
/// buildings, ground areas and roads, every layer in one pixel space (per-layer extents are
/// scaled to the largest, [`crate::mvt::decode_tile_on`]).
pub fn decode_data_tile(key: TileKey, frame: Frame, terrain: &Terrain, bytes: &[u8], profile: &Profile) -> Result<DataTile, String> {
    let t0 = Instant::now();
    let d = decode_tile_on(bytes, key, frame, terrain, true, true, profile)?;
    Ok(DataTile::detail(key, d.buildings, d.ground, d.proj, t0))
}

/// Decode an overview data tile ([`crate::mvt::decode_overview_on`]); the block bases stay open
/// until a chunk meshes them ([`DataTile::with_open_bases`]).
pub fn decode_overview_tile(key: TileKey, frame: Frame, terrain: &Terrain, bytes: &[u8], profile: &Profile) -> Result<DataTile, String> {
    let t0 = Instant::now();
    let d = decode_overview_on(bytes, key, frame, terrain, profile)?;
    Ok(DataTile::overview(key, d.blocks, d.landmarks, d.lights, d.light_landmark, d.ground, d.proj, t0).with_open_bases())
}

/// Decode a data tile of `level`.
pub fn decode_level(key: TileKey, level: DataLevel, frame: Frame, terrain: &Terrain, bytes: &[u8], profile: &Profile) -> Result<DataTile, String> {
    match level {
        DataLevel::Detail => decode_data_tile(key, frame, terrain, bytes, profile),
        DataLevel::Overview => decode_overview_tile(key, frame, terrain, bytes, profile),
    }
}

/// [`build_chunk_with`] without landmarks folded in (compatibility path, tests).
pub fn build_chunk(dt: &DataTile, chunk: TileKey, ring: usize, profile: &Profile, stop: &dyn Fn() -> bool) -> Result<Option<TileData>, String> {
    build_chunk_with(dt, None, chunk, ring, profile, stop)
}

/// The render chunk `chunk` (band `ring`) cut from data tile `dt`: building mesh, ground mesh and
/// lights as [`crate::worker::build_tile`] makes them for a tile of its own, with the geometry
/// clipped to the chunk rectangle and per-building lights owned by centroid (module docs).
///
/// `lm` = the overview tile whose landmarks this chunk draws (landmarks folded into the chunks:
/// no landmark layer), or `None` (the landmark layer draws them, or the source has none). For an
/// overview chunk `lm` is `dt` itself; without it the landmarks' light points are skipped too.
/// `stop` is checked between the stages; `Ok(None)` = stopped.
pub fn build_chunk_with(
    dt: &DataTile,
    lm: Option<&DataTile>,
    chunk: TileKey,
    ring: usize,
    profile: &Profile,
    stop: &dyn Fn() -> bool,
) -> Result<Option<TileData>, String> {
    let t0 = crate::diag::CpuClock::start();
    let proj = dt.proj.cut(chunk).ok_or_else(|| format!("chunk {chunk} is not inside data tile {}", dt.key))?;
    let r = proj.rect();
    let idx = select_idx(&dt.bboxes, r);
    let sel: Vec<&Building> = idx.iter().map(|&i| &dt.buildings[i]).collect();
    let lm = match lm {
        Some(l) => {
            let p = l.proj.cut(chunk).ok_or_else(|| format!("chunk {chunk} is not inside landmark tile {}", l.key))?;
            let s: Vec<&Building> = select_idx(&l.landmark_bboxes, p.rect()).into_iter().map(|i| &l.landmarks[i]).collect();
            Some((l, p, s))
        }
        None => None,
    };
    if stop() {
        return Ok(None);
    }
    let mut mesh = MeshData::default();
    if let Some(p) = profile.lod(ring) {
        let based = dt.lazy_bases.based(&dt.buildings, &idx, &dt.proj);
        mesh = mesh_building_iter(based.iter().map(|b| b.as_ref()), &proj, r, p, profile);
        if let Some((_, lp, ls)) = &lm {
            mesh.append(mesh_building_iter(ls.iter().copied(), lp, lp.rect(), p, profile));
        }
    }
    if stop() {
        return Ok(None);
    }
    let ground = mesh_ground(&dt.ground, &proj, ring, profile);
    if stop() {
        return Ok(None);
    }
    let overview = dt.level == DataLevel::Overview;
    // Overview light points of this chunk (landmarks' only when they are drawn here).
    let lit: Vec<&Building> = if overview {
        dt.lights.iter().zip(&dt.light_landmark).filter(|(_, &is_lm)| !is_lm || lm.is_some()).map(|(b, _)| b).collect()
    } else {
        Vec::new()
    };
    let mut lights = LightSet::default();
    if ring >= profile.lights.min_ring as usize {
        let (mut p, mut c) = if overview { light_points_in(lit.iter().copied(), &proj, profile) } else { light_points_in(dt.buildings.iter(), &proj, profile) };
        if let (false, Some((_, lp, ls))) = (overview, &lm) {
            let (p2, c2) = light_points_in(ls.iter().copied(), lp, profile);
            p.extend(p2);
            c.extend(c2);
        }
        let (cp, cc) = cluster(&p, &c, profile.cluster_cell_m());
        (lights.positions, lights.colors, lights.clustered_positions, lights.clustered_colors) = (p, c, cp, cc);
    }
    if ring >= profile.lights.street.min_ring as usize {
        lights.set_streets(&dt.ground.roads, &proj, ring, profile);
    }
    if ring >= profile.lights.beacons.min_ring as usize {
        // Beacons from footprint geometry: detail footprints, and landmarks drawn here (never
        // from blocks, whose height is an average).
        let (mut p, mut c) = if overview { (Vec::new(), Vec::new()) } else { beacon_points_in(dt.buildings.iter(), &proj, profile) };
        if let Some((_, lp, ls)) = &lm {
            let (p2, c2) = beacon_points_in(ls.iter().copied(), lp, profile);
            p.extend(p2);
            c.extend(c2);
        }
        (lights.beacon_positions, lights.beacon_colors) = (p, c);
    }
    let h = |b: &&Building| crate::mesher::resolve(b, profile).height;
    let mut max_height = sel.iter().map(h).fold(0.0f32, f32::max);
    if let Some((_, _, ls)) = &lm {
        max_height = ls.iter().map(h).fold(max_height, f32::max);
    }
    if overview {
        // Light points stand for every building (R3 has no mesh): the box covers their roofs.
        max_height = lit.iter().filter(|b| proj.place(b).is_some()).map(h).fold(max_height, f32::max);
    }
    let building_count = sel.len() + lm.as_ref().map_or(0, |l| l.2.len());
    Ok(Some(TileData {
        building_count,
        lights: Arc::new(lights),
        mesh: Arc::new(mesh),
        ground: Arc::new(ground),
        max_height,
        cpu_us: t0.us(),
        terrain: None,
    }))
}

/// The landmark tile of overview data tile `dt` (key `l/z/x/y` = the overview key; api 1.11):
/// every landmark meshed at full R0 quality with windows (`buildings.r0` LOD), clipped to the
/// tile rectangle (a landmark crossing an overview edge is split exactly between the two tiles),
/// one light point per landmark owned by centroid (both light versions equal: the light mode
/// never re-delivers a landmark tile) and its beacons. No ground, no street lamps. An overview
/// tile without landmarks gives an empty [`TileData`] (`building_count` 0: not delivered).
pub fn build_landmark_tile(dt: &DataTile, profile: &Profile, stop: &dyn Fn() -> bool) -> Result<Option<TileData>, String> {
    let t0 = crate::diag::CpuClock::start();
    if dt.landmarks.is_empty() {
        return Ok(Some(TileData::default()));
    }
    let proj = &dt.proj;
    let mesh = match profile.lod(0) {
        Some(p) => mesh_building_iter(dt.landmarks.iter(), proj, proj.rect(), p, profile),
        None => MeshData::default(),
    };
    if stop() {
        return Ok(None);
    }
    let mut lights = LightSet::default();
    let (p, c) = light_points_in(dt.landmarks.iter(), proj, profile);
    (lights.clustered_positions, lights.clustered_colors) = (p.clone(), c.clone());
    (lights.positions, lights.colors) = (p, c);
    if profile.lights.beacons.min_ring <= 3 {
        (lights.beacon_positions, lights.beacon_colors) = beacon_points_in(dt.landmarks.iter(), proj, profile);
    }
    let max_height = dt.landmarks.iter().map(|b| crate::mesher::resolve(b, profile).height).fold(0.0f32, f32::max);
    Ok(Some(TileData {
        building_count: dt.landmarks.len(),
        lights: Arc::new(lights),
        mesh: Arc::new(mesh),
        ground: Arc::default(),
        max_height,
        cpu_us: t0.us(),
        terrain: None,
    }))
}

#[cfg(test)]
pub(crate) mod fixture;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod levels_tests;
