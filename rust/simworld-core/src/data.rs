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
use crate::mesher::{mesh_building_iter, ClipRect, MeshData};
use crate::mvt::{decode_overview_on, decode_tile_on, Building, GroundFeatures, TileProjector};
use crate::profile::Profile;
use crate::terrain::Terrain;
use crate::worker::TileData;

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

/// Decode an overview data tile ([`crate::mvt::decode_overview_on`]).
pub fn decode_overview_tile(key: TileKey, frame: Frame, terrain: &Terrain, bytes: &[u8], profile: &Profile) -> Result<DataTile, String> {
    let t0 = Instant::now();
    let d = decode_overview_on(bytes, key, frame, terrain, profile)?;
    Ok(DataTile::overview(key, d.blocks, d.landmarks, d.lights, d.light_landmark, d.ground, d.proj, t0))
}

/// Decode a data tile of `level`.
pub fn decode_level(key: TileKey, level: DataLevel, frame: Frame, terrain: &Terrain, bytes: &[u8], profile: &Profile) -> Result<DataTile, String> {
    match level {
        DataLevel::Detail => decode_data_tile(key, frame, terrain, bytes, profile),
        DataLevel::Overview => decode_overview_tile(key, frame, terrain, bytes, profile),
    }
}

/// The synthetic city's data tile `key` of `level` (step T2c): generated, not decoded.
pub fn synthetic_data_tile(key: TileKey, level: DataLevel, frame: Frame, terrain: &Terrain, profile: &Profile) -> DataTile {
    let t0 = Instant::now();
    match level {
        DataLevel::Detail => {
            let (buildings, ground, proj) = crate::synthetic::detail_level(key, frame, terrain, profile);
            DataTile::detail(key, buildings, ground, proj, t0)
        }
        DataLevel::Overview => {
            let o = crate::synthetic::overview_level(key, frame, terrain, profile);
            DataTile::overview(key, o.blocks, o.landmarks, o.lights, o.light_landmark, o.ground, o.proj, t0)
        }
    }
}

/// A synthetic city chunk exactly as the streaming path builds it (T2c): generated data tile(s)
/// of [`DataLevels::SYNTHETIC`], cut by [`build_chunk_with`]; landmarks folded in unless
/// `landmark_layer` (the host draws the landmark layer). Golden tests and reference builds.
pub fn synthetic_chunk(chunk: TileKey, ring: usize, frame: Frame, terrain: &Terrain, profile: &Profile, landmark_layer: bool) -> Result<TileData, String> {
    let l = DataLevels::SYNTHETIC;
    let dk = l.data_of(chunk).ok_or_else(|| format!("{chunk}: not a synthetic chunk"))?;
    let dt = synthetic_data_tile(dk, l.level_of(dk), frame, terrain, profile);
    let lk = if landmark_layer { None } else { l.landmarks_of(chunk) };
    let other = lk.filter(|k| *k != dk).map(|k| synthetic_data_tile(k, l.level_of(k), frame, terrain, profile));
    let lm = match (lk, &other) {
        (None, _) => None,
        (Some(_), Some(o)) => Some(o),
        (Some(_), None) => Some(&dt),
    };
    build_chunk_with(&dt, lm, chunk, ring, profile, &|| false).map(|d| d.expect("never stopped"))
}

/// Indices-free selection of the buildings whose bbox meets `r`.
fn select<'a>(buildings: &'a [Building], bboxes: &[[f64; 4]], r: ClipRect) -> Vec<&'a Building> {
    buildings
        .iter()
        .zip(bboxes)
        .filter(|(_, bb)| !(bb[2] <= r.x0 || bb[0] >= r.x1 || bb[3] <= r.y0 || bb[1] >= r.y1))
        .map(|(b, _)| b)
        .collect()
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
    let sel = select(&dt.buildings, &dt.bboxes, r);
    let lm = match lm {
        Some(l) => {
            let p = l.proj.cut(chunk).ok_or_else(|| format!("chunk {chunk} is not inside landmark tile {}", l.key))?;
            let s = select(&l.landmarks, &l.landmark_bboxes, p.rect());
            Some((l, p, s))
        }
        None => None,
    };
    if stop() {
        return Ok(None);
    }
    let mut mesh = MeshData::default();
    if let Some(p) = profile.lod(ring) {
        mesh = mesh_building_iter(sel.iter().copied(), &proj, r, p, profile);
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
        lights.set_streets(&dt.ground.roads, &proj, profile);
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
        mesh,
        ground,
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
        mesh,
        ground: MeshData::default(),
        max_height,
        cpu_us: t0.us(),
        terrain: None,
    }))
}

#[cfg(test)]
pub(crate) mod fixture {
    //! A small synthetic archive area for the data-tile tests: one z14 tile with buildings
    //! (grid, chunk-edge crossers, a jagged outline, a hole, a tower), landuse areas and roads,
    //! in z14 pixels, and the same features re-expressed in the pixels of any tile inside it
    //! (what an archive's z15 / z16 tiles hold for the same area, unclipped).
    use crate::geo::{tile_for, TileKey};
    use crate::mvt::test_mvt::{encode_layer_ext, Feat, Val};

    pub const VILNIUS: (f64, f64) = (54.6872, 25.2797);

    /// The z14 tile under the Vilnius origin.
    pub fn key14() -> TileKey {
        tile_for(VILNIUS.0, VILNIUS.1, 14)
    }

    fn rect(x0: i32, y0: i32, w: i32, h: i32) -> Vec<(i32, i32)> {
        // Clockwise in screen space (y down) = MVT exterior.
        vec![(x0, y0), (x0 + w, y0), (x0 + w, y0 + h), (x0, y0 + h)]
    }

    const KINDS: [&str; 6] = ["apartments", "house", "office", "retail", "yes", "industrial"];

    /// (buildings, landuse, roads) in z14 pixels (extent 4096).
    fn features() -> (Vec<Feat>, Vec<Feat>, Vec<Feat>) {
        let mut b = Vec::new();
        let mut id = 1000u64;
        // Grid over the whole tile: every z16 chunk gets buildings.
        for i in 0..12 {
            for j in 0..12 {
                id += 1;
                let (x, y) = (60 + i * 335, 40 + j * 333);
                let h = 6.0 + ((i * 7 + j * 3) % 9) as f64 * 3.0;
                b.push(Feat {
                    id,
                    rings: vec![rect(x, y, 40 + (i % 3) * 12, 30 + (j % 4) * 9)],
                    props: vec![("id", Val::D(id as f64)), ("osm_type", Val::S("way")), ("kind", Val::S(KINDS[((i + j) % 6) as usize])), ("height", Val::D(h))],
                });
            }
        }
        // Crossers: the z16 edges (1024 multiples) and the z15 / z16 corner at 2048.
        for (k, (x, y, w, hh)) in [(1000, 500, 60, 40), (2030, 2030, 40, 40), (3060, 1200, 70, 50), (500, 3060, 50, 70), (1010, 1010, 30, 30)].into_iter().enumerate() {
            let id = 5000 + k as u64;
            b.push(Feat {
                id,
                rings: vec![rect(x, y, w, hh)],
                props: vec![("id", Val::D(id as f64)), ("osm_type", Val::S("way")), ("kind", Val::S("apartments")), ("levels", Val::D(5.0))],
            });
        }
        // A jagged outline (Douglas-Peucker drops the 1 px teeth at R0, keeps the 3 px ones).
        let mut jag = Vec::new();
        for t in 0..20 {
            jag.push((1500 + t * 6, 700 + if t % 2 == 0 { 0 } else if t % 4 == 1 { 1 } else { 3 }));
        }
        jag.extend([(1620, 700), (1620, 790), (1500, 790)]);
        b.push(Feat {
            id: 6001,
            rings: vec![jag],
            props: vec![("id", Val::D(6001.0)), ("osm_type", Val::S("way")), ("kind", Val::S("office")), ("height", Val::D(24.0))],
        });
        // A courtyard (hole) in a building crossing the z16 edge at x = 3072. The hole itself
        // stays off the edge (per-ring clip path, pinned by the goldens); a hole reaching a clip
        // edge is clipped with its outer ring as a whole since `polyclip` (3c751bd).
        let mut hole = rect(3085, 2600, 40, 50);
        hole.reverse();
        b.push(Feat {
            id: 6002,
            rings: vec![rect(3000, 2560, 140, 130), hole],
            props: vec![("id", Val::D(6002.0)), ("osm_type", Val::S("relation")), ("kind", Val::S("apartments")), ("height", Val::D(18.0))],
        });
        // A tower (beacons).
        b.push(Feat {
            id: 6003,
            rings: vec![rect(2200, 1500, 80, 80)],
            props: vec![("id", Val::D(6003.0)), ("osm_type", Val::S("way")), ("kind", Val::S("office")), ("height", Val::D(95.0))],
        });
        let mut whole = rect(-64, -64, 4224, 4224);
        let l = vec![
            Feat { id: 10, rings: vec![std::mem::take(&mut whole)], props: vec![("id", Val::D(10.0)), ("kind", Val::S("residential"))] },
            Feat { id: 11, rings: vec![rect(900, 900, 400, 300)], props: vec![("id", Val::D(11.0)), ("kind", Val::S("park"))] },
            Feat { id: 12, rings: vec![rect(1950, 3000, 300, 500), { let mut h = rect(2100, 3150, 80, 80); h.reverse(); h }], props: vec![("id", Val::D(12.0)), ("kind", Val::S("water"))] },
            Feat { id: 13, rings: vec![rect(2500, 300, 700, 600)], props: vec![("id", Val::D(13.0)), ("kind", Val::S("industrial"))] },
        ];
        let r = vec![
            Feat { id: 20, rings: vec![vec![(-64, 2000), (1500, 2100), (2600, 1980), (4160, 2050)]], props: vec![("id", Val::D(20.0)), ("kind", Val::S("primary")), ("lit", Val::S("yes"))] },
            Feat { id: 21, rings: vec![vec![(1200, -64), (1260, 1800), (1180, 4160)]], props: vec![("id", Val::D(21.0)), ("kind", Val::S("residential"))] },
            Feat { id: 22, rings: vec![vec![(300, 3000), (900, 3020), (1700, 2900)]], props: vec![("id", Val::D(22.0)), ("kind", Val::S("footway"))] },
        ];
        (b, l, r)
    }

    /// `f` expressed in the pixels (extent `ext`) of tile `k` inside the z14 tile.
    fn map(f: Vec<Feat>, k: TileKey, ext: i32) -> Vec<Feat> {
        let z = key14();
        let dz = k.z - z.z;
        let n = 1i32 << dz;
        let (ox, oy) = (((k.x - (z.x << dz)) as i32) * 4096 / n, ((k.y - (z.y << dz)) as i32) * 4096 / n);
        // z14 px -> k px: (p - o) * n * ext / 4096 (exact for the power-of-two cases used).
        let m = |p: (i32, i32)| (((p.0 - ox) as i64 * n as i64 * ext as i64 / 4096) as i32, ((p.1 - oy) as i64 * n as i64 * ext as i64 / 4096) as i32);
        f.into_iter().map(|mut x| {
            x.rings = x.rings.into_iter().map(|r| r.into_iter().map(m).collect()).collect();
            x
        }).collect()
    }

    /// MVT bytes of tile `k` (the z14 tile or a tile inside it) with these per-layer extents
    /// (buildings, landuse, roads).
    pub fn tile_bytes_ext(k: TileKey, ext: [u32; 3]) -> Vec<u8> {
        let (b, l, r) = features();
        let mut t = encode_layer_ext("buildings", &map(b, k, ext[0] as i32), false, ext[0]);
        t.extend(encode_layer_ext("landuse", &map(l, k, ext[1] as i32), false, ext[1]));
        t.extend(encode_layer_ext("roads", &map(r, k, ext[2] as i32), true, ext[2]));
        t
    }

    /// MVT bytes of tile `k` at extent 4096 (every layer).
    pub fn tile_bytes(k: TileKey) -> Vec<u8> {
        tile_bytes_ext(k, [4096; 3])
    }

    // ---- Two-level archive (T2b): the same area as a baked detail z14 + overview z11 pair.

    /// The overview (z11) tile holding [`key14`].
    pub fn key11() -> TileKey {
        let k = key14();
        TileKey::new(11, k.x >> 3, k.y >> 3)
    }

    /// The fixture's landmarks (roof >= 60 m): the 95 m tower.
    pub const LANDMARK_IDS: [u64; 1] = [6003];

    fn is_landmark(f: &Feat) -> bool {
        LANDMARK_IDS.contains(&f.id)
    }

    /// z14 pixels (extent 4096) -> overview z11 pixels at extent `ext` (rounded).
    fn to11(f: Vec<Feat>, ext: i32) -> Vec<Feat> {
        let (z, o) = (key14(), key11());
        let (ox, oy) = (((z.x - o.x * 8) * 4096) as f64, ((z.y - o.y * 8) * 4096) as f64);
        let m = |p: (i32, i32)| {
            let k = ext as f64 / (4096.0 * 8.0);
            (((p.0 as f64 + ox) * k).round() as i32, ((p.1 as f64 + oy) * k).round() as i32)
        };
        f.into_iter().map(|mut x| {
            x.rings = x.rings.into_iter().map(|r| r.into_iter().map(m).collect()).collect();
            x
        }).collect()
    }

    fn num(f: &Feat, k: &str) -> Option<f64> {
        f.props.iter().find(|(n, _)| *n == k).and_then(|(_, v)| match v {
            Val::D(d) => Some(*d),
            _ => None,
        })
    }

    fn text(f: &Feat, k: &str) -> Option<&'static str> {
        f.props.iter().find(|(n, _)| *n == k).and_then(|(_, v)| match v {
            Val::S(s) => Some(*s),
            _ => None,
        })
    }

    /// The bake's tagged roof: height, else levels x 3 m.
    fn roof(f: &Feat) -> Option<f64> {
        num(f, "height").or(num(f, "levels").map(|l| l * 3.0))
    }

    fn step5(v: f64) -> f64 {
        ((v / 5.0).round() * 5.0).max(5.0)
    }

    /// The detail z14 tile of a two-level archive: every layer at extent 8192, landmarks left out.
    pub fn detail_v2_bytes() -> Vec<u8> {
        let k = key14();
        let (mut b, l, r) = features();
        b.retain(|f| !is_landmark(f));
        let mut t = encode_layer_ext("buildings", &map(b, k, 8192), false, 8192);
        t.extend(encode_layer_ext("landuse", &map(l, k, 8192), false, 8192));
        t.extend(encode_layer_ext("roads", &map(r, k, 8192), true, 8192));
        t
    }

    /// Ids of the fixture's buildings (all, landmarks included).
    pub fn building_ids() -> Vec<u64> {
        features().0.iter().map(|f| f.id).collect()
    }

    /// The overview z11 tile of a two-level archive: `blocks` (one per non-landmark building,
    /// its bounding box, height rounded to 5 m; extent 4096), `lights` (one point per building at
    /// its bounding-box centre, OSM id / type in the feature id; 4096), `landmarks` (full
    /// geometry, all attributes; 16384), the primary road and the landuse areas (4096).
    pub fn overview_bytes() -> Vec<u8> {
        let (b, l, r) = features();
        let mut blocks = Vec::new();
        let mut lights = Vec::new();
        let mut lms = Vec::new();
        for f in b {
            let ring = &f.rings[0];
            let (x0, x1) = (ring.iter().map(|p| p.0).min().unwrap(), ring.iter().map(|p| p.0).max().unwrap());
            let (y0, y1) = (ring.iter().map(|p| p.1).min().unwrap(), ring.iter().map(|p| p.1).max().unwrap());
            let kind = text(&f, "kind").unwrap_or("yes");
            let relation = text(&f, "osm_type") == Some("relation");
            let tagged = roof(&f);
            let mut props = vec![("kind", Val::S(kind))];
            if let Some(h) = tagged {
                props.push(("height", Val::D(step5(h))));
                // m² at ~2.4 m per z14 pixel near Vilnius (only the slender-tower rule reads it).
                props.push(("area", Val::D(((x1 - x0) * (y1 - y0)) as f64 * 0.37 * 0.37)));
            }
            lights.push(Feat { id: f.id * 10 + if relation { 3 } else { 2 }, rings: vec![vec![((x0 + x1) / 2, (y0 + y1) / 2)]], props });
            if is_landmark(&f) {
                lms.push(f);
                continue;
            }
            let h = step5(tagged.unwrap_or(9.0));
            blocks.push(Feat {
                id: 0,
                rings: vec![vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]],
                props: vec![("id", Val::D(f.id as f64)), ("kind", Val::S(kind)), ("height", Val::D(h)), ("count", Val::D(1.0))],
            });
        }
        let roads: Vec<Feat> = r.into_iter().filter(|f| text(f, "kind") == Some("primary")).collect();
        let mut t = encode_layer_ext("blocks", &to11(blocks, 4096), false, 4096);
        t.extend(encode_layer_ext("lights", &to11(lights, 4096), false, 4096));
        t.extend(encode_layer_ext("landmarks", &to11(lms, 16384), false, 16384));
        t.extend(encode_layer_ext("landuse", &to11(l, 4096), false, 4096));
        t.extend(encode_layer_ext("roads", &to11(roads, 4096), true, 4096));
        t
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod levels_tests;
