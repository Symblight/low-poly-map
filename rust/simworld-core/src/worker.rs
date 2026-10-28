//! Threads: a small tokio runtime for I/O (fetch; the disk cache has its own thread, owned by
//! [`crate::fetch::DiskCache`]) and one [`JobPool`] (priority queue + fixed worker threads,
//! [`crate::jobs`]) for the CPU work of both layers (MVT decode / synthetic generation /
//! meshing / lights, terrain TINs). Results go back to the main thread over one crossbeam
//! channel. Workers only produce plain Rust structs; they never call Godot APIs, and panics are
//! caught and turned into error messages.
//!
//! Every CPU job ends with exactly one reply: its result (`Decoded` / `TerrainBuilt`), or
//! [`WorkerMsg::Dropped`] when it was cancelled ([`Worker::cancel`]: the scheduler orphaned that
//! generation) before or while it ran. Build stages check the cancel flag in between.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use ahash::AHashMap;
use bytes::Bytes;
use crossbeam_channel::{Receiver, Sender};
use tokio::task::AbortHandle;

use crate::data::{build_chunk_with, build_landmark_tile, decode_level, synthetic_data_tile, DataLevel, DataLevels, DataTile};
use crate::jobs::{worker_threads, JobEnd, JobPool, JobSpec, JobStats, Output, PoolConfig, Priority};
use crate::rings::LayerKind;
use crate::fetch::{DiskCache, FetchError, FetchErrorKind, FetchParams, Fetcher, Note, Source};
use crate::geo::{Frame, TileKey};
use crate::lights::LightSet;
use crate::mesher::{mesh_tile, MeshData};
use crate::ground::mesh_ground;
use crate::mvt::{decode_tile_on, Building, DecodedTile, GroundFeatures, TileProjector};
use crate::seed::SeedWorld;
use crate::terrain::tin::{build_terrain_tile_until, TerrainTile};
use crate::terrain::Terrain;
use crate::profile::Profile;
use crate::synthetic;

mod levels_probe;
mod probe;
mod release;

/// Decoded tile, ready for conversion on the main thread. Only flat arrays cross the channel:
/// the per-building structs are dropped on the worker (freeing thousands of small allocations
/// on the main thread cost ~1-3 ms per z13 tile).
#[derive(Debug, Default)]
pub struct TileData {
    /// Buildings decoded in the tile (including buffer copies owned by neighbours).
    pub building_count: usize,
    /// R2/R3: light points (single and clustered), Godot axes relative to the tile origin
    /// (tile center). Empty for R0/R1, which get windows instead.
    pub lights: Arc<LightSet>,
    /// Merged building mesh (tile-root relative), clipped to the tile. Empty for R3.
    pub mesh: MeshData,
    /// Merged ground mesh (areas + road strips, tile-root relative, no UV), clipped to the
    /// tile. Empty for tiles without ground features (every band has ground since api 1.3).
    pub ground: MeshData,
    /// Highest roof top (m above ground) of any building decoded in the tile, including
    /// buffer copies owned by neighbours; 0 for an empty tile. Height of the slant-distance box.
    pub max_height: f32,
    /// Worker-side CPU time (decode + mesh + lights), microseconds.
    pub cpu_us: u64,
    /// Terrain layer only (api 1.5): the terrain mesh of the tile (`None` for vector tiles).
    pub terrain: Option<Box<TerrainTile>>,
}

impl TileData {
    pub fn triangle_count(&self) -> usize {
        self.mesh.triangle_count()
    }
    pub fn vertex_count(&self) -> usize {
        self.mesh.vertex_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_aborts_a_hanging_fetch_and_still_replies() {
        // A server that accepts connections and never answers: the fetch hangs (request
        // timeout 20 s by default) until it is cancelled.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/hang.pmtiles", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let _held: Vec<_> = listener.incoming().collect();
        });
        let w = Worker::new(Frame::new(54.6872, 25.2797), &url, "", Arc::new(Profile::default())).unwrap();
        let key = TileKey::new(16, 37370, 20827);
        let fetched = |w: &Worker| {
            while let Some(m) = w.try_recv() {
                if let WorkerMsg::Fetched { job, result, .. } = m {
                    return Some((job, result));
                }
            }
            None
        };
        w.submit(Job::Fetch { key, ring: 0, job: 7 }, Priority::on_demand(0.0));
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(fetched(&w).is_none(), "the server never answers");
        w.cancel(LayerKind::World, &[7, 99]); // unknown ids are ignored
        let t0 = std::time::Instant::now();
        let (job, result) = loop {
            if let Some(r) = fetched(&w) {
                break r;
            }
            assert!(t0.elapsed() < std::time::Duration::from_secs(5), "no reply after cancel");
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(job, 7);
        assert_eq!(result.unwrap_err().kind, FetchErrorKind::Cancelled);
        assert!(lock(&w.fetches).is_empty(), "the task removed its abort handle");
    }

    #[test]
    fn tile_data_records_max_roof_height() {
        let frame = Frame::new(54.6872, 25.2797);
        let key = crate::geo::tile_for(54.6872, 25.2797, 16);
        let p = Profile::default();
        let d = build_tile(key, 0, frame, &Payload::Synthetic, &p).unwrap();
        let expect = synthetic::generate(key, frame)
            .iter()
            .map(|b| crate::mesher::resolve(b, &p).height)
            .fold(0.0f32, f32::max);
        assert!(d.max_height > 10.0);
        assert_eq!(d.max_height, expect);
        assert_eq!(build_tile(key, 0, frame, &Payload::Bytes(Bytes::new()), &p).unwrap().max_height, 0.0);
        // Building lights from R1 (default lights.min_ring), street lamps on every band.
        assert!(d.lights.positions.is_empty() && !d.lights.street_positions.is_empty() && !d.mesh.is_empty());
        let k14 = crate::geo::tile_for(54.6872, 25.2797, 14);
        let d2 = build_tile(k14, 2, frame, &Payload::Synthetic, &p).unwrap();
        assert!(!d2.lights.is_empty() && !d2.mesh.is_empty());
        assert!(d2.lights.clustered_positions.len() < d2.lights.positions.len());
        let k13 = crate::geo::tile_for(54.6872, 25.2797, 13);
        let d3 = build_tile(k13, 3, frame, &Payload::Synthetic, &p).unwrap();
        assert!(!d3.lights.positions.is_empty() && !d3.lights.street_positions.is_empty() && d3.mesh.is_empty());
        // Ground on every band (synthetic city has streets and districts everywhere); R3 is the coarsest.
        assert!(!d.ground.is_empty() && !d2.ground.is_empty() && !d3.ground.is_empty());
        assert!(d.ground.uvs.is_empty());
        assert_eq!(d.ground, build_tile(key, 0, frame, &Payload::Synthetic, &p).unwrap().ground);
        // Deterministic.
        assert_eq!(d3.lights, build_tile(k13, 3, frame, &Payload::Synthetic, &p).unwrap().lights);
    }

    /// `lights.min_ring` is the finest band that carries lights: lowering it lights R1 (and R0) with exactly the
    /// lights the same tile has as R2 (they do not depend on the band), raising it takes them off R2.
    #[test]
    fn lights_min_ring_selects_the_lit_bands() {
        let frame = Frame::new(54.6872, 25.2797);
        let key = crate::geo::tile_for(54.6872, 25.2797, 15);
        let lit = |min_ring: u32, ring: usize| {
            let mut w = crate::profile::WorldProfile::default();
            w.lights.min_ring = min_ring;
            w.lights.street.min_ring = 4; // building lights only
            w.lights.beacons.min_ring = 4;
            let p = Profile::new(w).unwrap();
            build_tile(key, ring, frame, &Payload::Synthetic, &p).unwrap().lights
        };
        let reference = lit(2, 2);
        assert!(!reference.is_empty());
        assert!(lit(2, 1).is_empty() && lit(2, 0).is_empty(), "min_ring 2: R0/R1 unlit");
        assert_eq!(lit(1, 1), reference, "R1 lit like R2 would be");
        assert!(lit(1, 0).is_empty());
        assert_eq!(lit(0, 0), reference);
        assert!(lit(3, 2).is_empty() && !lit(3, 3).is_empty(), "R3 only");
    }

    /// `buildings.rN.min_height_m` drops low buildings from that band's mesh only: the
    /// remaining vertices all belong to buildings at least that tall (COLOR a = height / 400 m),
    /// lights (single and clustered), ground, the decoded building count and the tile box height
    /// are unchanged; R3 (lights only) is identical.
    #[test]
    fn min_height_filter_drops_low_buildings_from_the_mesh_only() {
        let (lat, lon) = (54.6872, 25.2797);
        let frame = Frame::new(lat, lon);
        let def = Profile::default();
        let over = |ring: usize, h: f64| {
            let v = serde_json::json!({ "buildings": { format!("r{ring}"): { "min_height_m": h } } });
            crate::profile::resolve(None, Some(&v), &crate::NullLog).unwrap()
        };
        let all = crate::profile::resolve(
            None,
            Some(&serde_json::json!({ "buildings": {
                "r0": { "min_height_m": 15.0 }, "r1": { "min_height_m": 15.0 }, "r2": { "min_height_m": 15.0 } } })),
            &crate::NullLog,
        )
        .unwrap();
        for (ring, h) in [(0usize, 15.0f32), (1, 15.0), (2, 15.0)] {
            let key = crate::geo::tile_for(lat, lon, 16 - ring as u8);
            let a = build_tile(key, ring, frame, &Payload::Synthetic, &def).unwrap();
            let b = build_tile(key, ring, frame, &Payload::Synthetic, &over(ring, h as f64)).unwrap();
            let c = build_tile(key, ring, frame, &Payload::Synthetic, &all).unwrap();
            assert!(b.triangle_count() > 0, "r{ring}: something taller than {h} m remains");
            assert!(b.triangle_count() < a.triangle_count(), "r{ring}: the filter dropped nothing");
            assert!(b.mesh.colors.iter().all(|c| c[3] * 400.0 >= h - 1e-3), "r{ring}: a building below {h} m was meshed");
            assert_eq!(b.mesh, c.mesh, "r{ring}: the other bands' keys must not matter");
            // Mesh only.
            assert_eq!(b.lights, a.lights, "r{ring}: lights must not depend on the filter");
            assert_eq!(b.ground, a.ground, "r{ring}: ground must not depend on the filter");
            assert_eq!((b.building_count, b.max_height), (a.building_count, a.max_height));
            // A filter on another band leaves this one byte-identical.
            let other = build_tile(key, ring, frame, &Payload::Synthetic, &over((ring + 1) % 3, 100.0)).unwrap();
            assert_eq!(other.mesh, a.mesh, "r{ring}: another band's key changed this band");
            eprintln!("r{ring} {key}: {} -> {} triangles with min_height_m {h}", a.triangle_count(), b.triangle_count());
        }
        // R2 with real lights: identical single and clustered lights.
        let k14 = crate::geo::tile_for(lat, lon, 14);
        let l = build_tile(k14, 2, frame, &Payload::Synthetic, &all).unwrap().lights;
        assert!(!l.is_empty() && !l.clustered_positions.is_empty());
        // R3: no building mesh and no key; output identical.
        let k13 = crate::geo::tile_for(lat, lon, 13);
        let a3 = build_tile(k13, 3, frame, &Payload::Synthetic, &def).unwrap();
        let b3 = build_tile(k13, 3, frame, &Payload::Synthetic, &all).unwrap();
        assert!(b3.mesh.is_empty());
        assert_eq!((a3.lights, a3.ground), (b3.lights, b3.ground));
        assert!(all.lod(3).is_none());
    }
}

#[derive(Debug)]
pub enum Payload {
    Bytes(Bytes),
    Synthetic,
}

#[derive(Debug)]
pub enum Job {
    Fetch { key: TileKey, ring: usize, job: u64 },
    Decode { key: TileKey, ring: usize, job: u64, payload: Payload },
    /// Terrain layer (api 1.5): build the TIN of a terrain tile at `detail` times the band's
    /// `max_error_m`. No fetch stage (the terrain is a function).
    Terrain { key: TileKey, ring: usize, job: u64, detail: f64 },
    /// Tiles v2 (T2a): fetch the raw bytes of data tile `key` (I/O runtime, disk cache first).
    FetchData { key: TileKey, job: u64 },
    /// Tiles v2: decode data tile `key` of `level` into an `Arc<DataTile>` (CPU pool); the output
    /// goes to the dependent [`Job::MeshChunk`]s and back to the main thread
    /// ([`WorkerMsg::DataDecoded`]). The synthetic city (no tile source) generates it instead
    /// (`bytes` empty, step T2c).
    DecodeData { key: TileKey, job: u64, bytes: Bytes, level: DataLevel },
    /// Tiles v2: cut render chunk `key` (band `ring`) from its data tile, with the landmarks of
    /// `lm` folded in (T2b, no landmark layer). Replies like a [`Job::Decode`] (`Decoded` or
    /// `Dropped`).
    MeshChunk { key: TileKey, ring: usize, job: u64, data: DataRef, lm: LmRef },
    /// Landmark layer (api 1.11): mesh the landmarks of overview data tile `key` (the `l/` key).
    /// Replies [`WorkerMsg::LandmarksBuilt`] or `Dropped { layer: Landmarks }`.
    MeshLandmarks { key: TileKey, job: u64, data: DataRef },
}

/// Which landmarks a [`Job::MeshChunk`] draws.
#[derive(Debug, Clone)]
pub enum LmRef {
    /// None (the landmark layer draws them, or the source has none).
    None,
    /// Those of the chunk's own data tile (an overview chunk).
    Same,
    /// Those of another data tile (the overview tile of a detail chunk).
    Other(DataRef),
}

/// The data tile a [`Job::MeshChunk`] is cut from.
#[derive(Debug, Clone)]
pub enum DataRef {
    /// Already decoded (resident on the main thread).
    Ready(Arc<DataTile>),
    /// The output of the pending [`Job::DecodeData`] with this job id (a pool dependency: a
    /// failed or cancelled decode cancels the chunk job).
    After(u64),
}

impl Job {
    /// Tile key of the job (the data tile for `FetchData` / `DecodeData`).
    pub fn key(&self) -> TileKey {
        match *self {
            Job::Fetch { key, .. }
            | Job::Decode { key, .. }
            | Job::Terrain { key, .. }
            | Job::FetchData { key, .. }
            | Job::DecodeData { key, .. }
            | Job::MeshChunk { key, .. }
            | Job::MeshLandmarks { key, .. } => key,
        }
    }
    /// A data-tile job (`FetchData` / `DecodeData`) rather than a job of a tracked tile.
    pub fn is_data(&self) -> bool {
        matches!(self, Job::FetchData { .. } | Job::DecodeData { .. })
    }
}

#[derive(Debug)]
pub enum WorkerMsg {
    Fetched { key: TileKey, ring: usize, job: u64, result: Result<(Bytes, Source), FetchError> },
    Decoded { key: TileKey, ring: usize, job: u64, result: Result<TileData, String> },
    /// Result of a [`Job::Terrain`] (routed to the terrain layer's scheduler).
    TerrainBuilt { key: TileKey, ring: usize, job: u64, result: Result<TileData, String> },
    /// A `Decode` / `MeshChunk` (`layer` World) or `Terrain` job that was cancelled before or
    /// while it ran (a stale generation, a cancelled dependency, or shutdown): frees its slot,
    /// carries no result.
    Dropped { layer: LayerKind, key: TileKey, ring: usize, job: u64 },
    /// Result of a [`Job::FetchData`].
    DataFetched { key: TileKey, job: u64, result: Result<(Bytes, Source), FetchError> },
    /// Result of a [`Job::DecodeData`] (sent before the dependent chunk jobs run).
    DataDecoded { key: TileKey, job: u64, result: Result<Arc<DataTile>, String> },
    /// A [`Job::DecodeData`] that was cancelled before or while it ran.
    DataDropped { key: TileKey, job: u64 },
    /// Result of a [`Job::MeshLandmarks`] (an empty `TileData` = the tile has no landmarks).
    LandmarksBuilt { key: TileKey, job: u64, result: Result<TileData, String> },
    /// The archive opened and passed the tileset check with these data levels (or, offline,
    /// the levels a warm disk cache recorded). Sent once per worker.
    Levels(DataLevels),
    /// Opening the archive failed while its levels are still unknown (status).
    OpenFailed(FetchError),
    Info(String),
    /// Logged as a (rate-limited) warning on the main thread.
    Warn(String),
    /// The disk cache was disabled for the session: logged as a warning, status degraded.
    CacheDisabled(String),
    /// A configuration problem (logged as an error on the main thread).
    Error(String),
}

/// CPU stage: payload -> buildings, ground areas (if `want_ground`), roads (if `want_roads`) and projector
/// (shared by real and synthetic tiles). Flat terrain; see [`decode_payload_on`].
pub fn decode_payload(
    key: TileKey,
    frame: Frame,
    payload: &Payload,
    want_ground: bool,
    want_roads: bool,
    profile: &Profile,
) -> Result<DecodedTile, String> {
    decode_payload_on(key, frame, &Terrain::flat(), payload, want_ground, want_roads, profile)
}

/// [`decode_payload`] on `terrain` (the projector of the result carries it).
pub fn decode_payload_on(
    key: TileKey,
    frame: Frame,
    terrain: &Terrain,
    payload: &Payload,
    want_ground: bool,
    want_roads: bool,
    profile: &Profile,
) -> Result<DecodedTile, String> {
    match payload {
        Payload::Synthetic => {
            let mut ground =
                if want_ground || want_roads { synthetic::generate_ground(key, frame, profile) } else { GroundFeatures::default() };
            if !want_ground {
                ground.areas.clear();
            }
            if !want_roads {
                ground.roads.clear();
            }
            Ok(DecodedTile {
                buildings: synthetic::generate_on(key, frame, terrain),
                ground,
                proj: TileProjector::new(key, frame, 4096.0).with_terrain(terrain.clone()),
            })
        }
        Payload::Bytes(b) => decode_tile_on(b, key, frame, terrain, want_ground, want_roads, profile),
    }
}

/// CPU stage: payload -> buildings + projector.
pub fn tile_buildings_proj(
    key: TileKey,
    frame: Frame,
    payload: &Payload,
    profile: &Profile,
) -> Result<(Vec<Building>, TileProjector), String> {
    decode_payload(key, frame, payload, false, false, profile).map(|t| (t.buildings, t.proj))
}

/// CPU stage: payload -> buildings.
pub fn tile_buildings(key: TileKey, frame: Frame, payload: &Payload, profile: &Profile) -> Result<Vec<Building>, String> {
    tile_buildings_proj(key, frame, payload, profile).map(|(b, _)| b)
}

/// CPU stage: payload -> flat per-tile output (building + ground meshes per ring, lights). The
/// decoded structs are dropped here, on the worker; only flat buffers cross to the main thread.
/// Flat terrain; see [`build_tile_on`].
pub fn build_tile(key: TileKey, ring: usize, frame: Frame, payload: &Payload, profile: &Profile) -> Result<TileData, String> {
    build_tile_on(key, ring, frame, &Terrain::flat(), payload, profile)
}

/// [`build_tile`] on `terrain`: building bases (+ foundations), draped ground and light heights
/// all sample it through the tile's projector. This is what the worker pool runs.
pub fn build_tile_on(
    key: TileKey,
    ring: usize,
    frame: Frame,
    terrain: &Terrain,
    payload: &Payload,
    profile: &Profile,
) -> Result<TileData, String> {
    build_tile_stoppable(key, ring, frame, terrain, payload, profile, &never).map(|d| d.expect("never stopped"))
}

fn never() -> bool {
    false
}

/// [`build_tile_on`] with a cancel probe checked between stages: `Ok(None)` = stopped. The
/// output of a build that is not stopped is identical.
fn build_tile_stoppable(
    key: TileKey,
    ring: usize,
    frame: Frame,
    terrain: &Terrain,
    payload: &Payload,
    profile: &Profile,
    stop: &dyn Fn() -> bool,
) -> Result<Option<TileData>, String> {
    let t0 = std::time::Instant::now();
    let want_ground = profile.ground_params(ring).is_some();
    let want_streets = ring >= profile.lights.street.min_ring as usize;
    if stop() {
        return Ok(None);
    }
    let DecodedTile { buildings, ground: gf, proj } =
        decode_payload_on(key, frame, terrain, payload, want_ground, want_ground || want_streets, profile)?;
    Ok(finish_tile(buildings, gf, &proj, ring, profile, &[], t0, stop))
}

/// The seed generator's tile (api 1.7, `docs/spec-world-seed.md` section 7): generated in tile
/// pixels from `(key, seed)` alone, then projected, meshed and lit exactly like archive data,
/// plus the airport lights (`airport_lights`).
pub fn build_seed_tile_on(
    key: TileKey,
    ring: usize,
    frame: Frame,
    terrain: &Terrain,
    seed: &SeedWorld,
    profile: &Profile,
) -> Result<TileData, String> {
    build_seed_tile_stoppable(key, ring, frame, terrain, seed, profile, &never).map(|d| d.expect("never stopped"))
}

/// [`build_seed_tile_on`] with a cancel probe (see [`build_tile_stoppable`]).
fn build_seed_tile_stoppable(
    key: TileKey,
    ring: usize,
    frame: Frame,
    terrain: &Terrain,
    seed: &SeedWorld,
    profile: &Profile,
    stop: &dyn Fn() -> bool,
) -> Result<Option<TileData>, String> {
    if stop() {
        return Ok(None);
    }
    let t0 = std::time::Instant::now();
    let want_ground = profile.ground_params(ring).is_some();
    let want_streets = ring >= profile.lights.street.min_ring as usize;
    let st = match crate::seed::world::route(key, seed) {
        crate::seed::world::TileSource::Seed => crate::seed::world::generate(key, seed),
    };
    let proj = TileProjector::new(key, frame, crate::seed::world::EXTENT).with_terrain(terrain.clone());
    let buildings: Vec<Building> = st.buildings.into_iter().filter_map(|b| proj.finish(b)).collect();
    let mut gf = st.ground;
    if !want_ground {
        gf.areas.clear();
    }
    if !(want_ground || want_streets) {
        gf.roads.clear();
    }
    Ok(finish_tile(buildings, gf, &proj, ring, profile, &st.airport_lights, t0, stop))
}

/// Mesh, ground and lights of a decoded or generated tile (shared by every source). `stop` is
/// checked between the stages (safe points); `None` = stopped.
#[allow(clippy::too_many_arguments)]
fn finish_tile(
    buildings: Vec<Building>,
    gf: GroundFeatures,
    proj: &TileProjector,
    ring: usize,
    profile: &Profile,
    airport_lights: &[crate::seed::world::AirportLightPx],
    t0: std::time::Instant,
    stop: &dyn Fn() -> bool,
) -> Option<TileData> {
    let want_streets = ring >= profile.lights.street.min_ring as usize;
    if stop() {
        return None;
    }
    let mesh = mesh_tile(&buildings, proj, ring, profile);
    if stop() {
        return None;
    }
    let ground = mesh_ground(&gf, proj, ring, profile);
    if stop() {
        return None;
    }
    let mut lights = if ring >= profile.lights.min_ring as usize { LightSet::from_buildings(&buildings, profile) } else { LightSet::default() };
    if want_streets {
        lights.set_streets(&gf.roads, proj, profile);
    }
    if ring >= profile.lights.beacons.min_ring as usize {
        lights.set_beacons(&buildings, proj, profile);
    }
    if !airport_lights.is_empty() {
        lights.set_airport(airport_lights, proj, ring, profile);
    }
    drop(gf);
    let building_count = buildings.len();
    let max_height = buildings.iter().map(|b| crate::mesher::resolve(b, profile).height).fold(0.0f32, f32::max);
    drop(buildings);
    Some(TileData {
        building_count,
        lights: Arc::new(lights),
        mesh,
        ground,
        max_height,
        cpu_us: t0.elapsed().as_micros() as u64,
        terrain: None,
    })
}

/// Terrain layer: the TIN of terrain tile `key` (ring = index in `terrain.bands`) as a
/// [`TileData`] with only `terrain` set (box height 0: the box is the terrain range).
pub fn build_terrain_data(
    key: TileKey,
    ring: usize,
    frame: Frame,
    terrain: &Terrain,
    profile: &Profile,
    detail: f64,
    seed: Option<&SeedWorld>,
) -> Result<TileData, String> {
    build_terrain_data_until(key, ring, frame, terrain, profile, detail, seed, &|| false).map(|d| d.expect("never stopped"))
}

/// [`build_terrain_data`] that stops at the TIN build's safe points when `stop()` (`Ok(None)`).
#[allow(clippy::too_many_arguments)]
pub fn build_terrain_data_until(
    key: TileKey,
    ring: usize,
    frame: Frame,
    terrain: &Terrain,
    profile: &Profile,
    detail: f64,
    seed: Option<&SeedWorld>,
    stop: &dyn Fn() -> bool,
) -> Result<Option<TileData>, String> {
    let t0 = std::time::Instant::now();
    // A seed world's terrain carries COLOR: r = ground class / 8, g = tint / 8 (api 1.7).
    let color = seed.map(|s| {
        move |lat: f64, lon: f64| {
            let (class, tint) = s.class_at(lat, lon);
            [class as f32 / 8.0, tint as f32 / 8.0, 0.0, 1.0]
        }
    });
    let color = color.as_ref().map(|c| c as &dyn Fn(f64, f64) -> [f32; 4]);
    let Some(tile) = build_terrain_tile_until(key, ring, frame, terrain, profile, detail, color, stop)? else { return Ok(None) };
    Ok(Some(TileData { terrain: Some(Box::new(tile)), cpu_us: t0.elapsed().as_micros() as u64, ..TileData::default() }))
}

fn panic_text(p: Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

/// The fetch map stays usable after a panic elsewhere (it only holds abort handles).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Pool job id of a layer's job (the two schedulers number their jobs independently).
fn pool_id(layer: LayerKind, job: u64) -> u64 {
    match layer {
        LayerKind::World | LayerKind::Landmarks => job & !TERRAIN_TAG,
        LayerKind::Terrain => job | TERRAIN_TAG,
    }
}

const TERRAIN_TAG: u64 = 1 << 63;

pub struct Worker {
    rt: Option<tokio::runtime::Runtime>,
    /// CPU work of both layers (decode / mesh / terrain), one priority queue.
    pool: JobPool,
    tx: Sender<WorkerMsg>,
    rx: Receiver<WorkerMsg>,
    fetcher: Option<Arc<Fetcher>>,
    frame: Frame,
    /// Terrain every decode job meshes on (cloned into the job).
    terrain: Terrain,
    /// Seed world (api 1.7): synthetic-payload tiles come from its generator, terrain tiles get
    /// its COLOR.
    seed: Option<Arc<SeedWorld>>,
    profile: Arc<Profile>,
    cache_bytes: Arc<AtomicU64>,
    /// A disk cache folder is in use (tile source + non-empty `cache_dir`).
    disk_cache: bool,
    /// Source and cache settings, to decide whether a re-configure may reuse the open cache.
    url: String,
    cache_dir: String,
    cache_cap: u64,
    /// Fetches in flight by job id, for [`Worker::cancel`] (each task removes its own entry).
    fetches: Arc<Mutex<AHashMap<u64, AbortHandle>>>,
    /// Ids of [`Worker::release`] jobs.
    release_seq: AtomicU64,
}

impl Worker {
    /// `url` empty = synthetic city (no runtime / fetcher needed).
    pub fn new(frame: Frame, url: &str, cache_dir: &str, profile: Arc<Profile>) -> Result<Self, String> {
        Self::with_cache(frame, url, cache_dir, profile, None)
    }

    /// The terrain decode jobs mesh on (flat unless set; set before the first job).
    pub fn set_terrain(&mut self, terrain: Terrain) {
        self.terrain = terrain;
    }

    /// The seed world tiles are generated from (set before the first job; `None` = the old
    /// synthetic city for an empty `tile_url`).
    pub fn set_seed(&mut self, seed: Option<Arc<SeedWorld>>) {
        self.seed = seed;
    }

    /// [`Worker::new`], reusing a disk cache that is already open (from
    /// [`Worker::shutdown`] with `keep_cache`; same source, folder and cap).
    pub fn with_cache(
        frame: Frame,
        url: &str,
        cache_dir: &str,
        profile: Arc<Profile>,
        reuse: Option<Arc<DiskCache>>,
    ) -> Result<Self, String> {
        let (tx, rx) = crossbeam_channel::unbounded();
        // Profile `streaming.worker_threads` (0 = cores - 2, clamped to 2..=8). iOS will set a
        // QoS class per thread through `PoolConfig::on_thread_start` (not built).
        let pool = JobPool::new(PoolConfig::new(worker_threads(profile.streaming.worker_threads), "simplane-cpu"))?;
        let cache_cap = FetchParams::from_profile(&profile).cache_cap_bytes;
        let cache_bytes = reuse.as_ref().map_or_else(|| Arc::new(AtomicU64::new(0)), |c| c.bytes_counter());
        let (rt, fetcher) = if url.is_empty() {
            (None, None)
        } else {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .max_blocking_threads(4)
                .thread_name("simplane-io")
                .enable_all()
                .build()
                .map_err(|e| format!("tokio runtime: {e}"))?;
            let _g = rt.enter();
            let info_tx = tx.clone();
            let f = Fetcher::new(
                url,
                cache_dir,
                cache_bytes.clone(),
                FetchParams::from_profile(&profile),
                Box::new(move |n| {
                    let _ = info_tx.send(match n {
                        Note::Info(s) => WorkerMsg::Info(s),
                        Note::Warn(s) => WorkerMsg::Warn(s),
                        Note::CacheDisabled(s) => WorkerMsg::CacheDisabled(s),
                        Note::Error(s) => WorkerMsg::Error(s),
                    });
                }),
            )?;
            let f = match reuse {
                Some(c) if !cache_dir.is_empty() => f.with_disk_cache(c),
                _ => f,
            };
            let f = Arc::new(f);
            Self::spawn_levels_probe(&rt, f.clone(), tx.clone());
            (Some(rt), Some(f))
        };
        let disk_cache = fetcher.is_some() && !cache_dir.is_empty();
        Ok(Self {
            rt,
            pool,
            tx,
            rx,
            fetcher,
            frame,
            terrain: Terrain::flat(),
            seed: None,
            profile,
            cache_bytes,
            disk_cache,
            url: url.to_string(),
            cache_dir: cache_dir.to_string(),
            cache_cap,
            fetches: Arc::new(Mutex::new(AHashMap::new())),
            release_seq: AtomicU64::new(0),
        })
    }

    pub fn is_synthetic(&self) -> bool {
        self.fetcher.is_none()
    }

    /// Whether this worker may write to a disk cache folder.
    pub fn uses_disk_cache(&self) -> bool {
        self.disk_cache
    }

    /// Whether a configuration with this source, cache folder and cap may take over this
    /// worker's open disk cache (the file stays open; no close / reopen).
    pub fn can_reuse_cache(&self, url: &str, cache_dir: &str, cap: u64) -> bool {
        self.disk_cache
            && self.url == url
            && self.cache_dir == cache_dir
            && self.cache_cap == cap
            && self.fetcher.as_ref().and_then(|f| f.disk_handle()).is_some_and(|d| !d.is_disabled())
    }

    /// Stop with a bounded wait (`wait` in total): the I/O runtime stops first (async tasks
    /// such as HTTP requests are cancelled at their next await; a cache open in progress
    /// finishes), then the disk cache either is handed back open (`keep_cache`, for a
    /// re-configure with the same source and folder) or is closed (pending writes flushed,
    /// WAL checkpointed, file closed). Returns whether everything finished within `wait`, and
    /// the kept cache. CPU jobs are cancelled (queued ones at once, running ones at their next
    /// safe point) but not waited for; they never touch the disk and their results are
    /// discarded. Dropping a `Worker` instead only signals the stop and the cache close,
    /// without waiting.
    pub fn shutdown(mut self, wait: std::time::Duration, keep_cache: bool) -> (bool, Option<Arc<DiskCache>>) {
        let t0 = std::time::Instant::now();
        self.pool.shutdown(std::time::Duration::ZERO);
        if let Some(rt) = self.rt.take() {
            rt.shutdown_timeout(wait);
        }
        let disk = self.fetcher.take().and_then(|f| f.disk_handle());
        let Some(disk) = disk else { return (t0.elapsed() < wait, None) };
        if keep_cache && !disk.is_disabled() {
            return (t0.elapsed() < wait, Some(disk));
        }
        let closed = disk.close(wait.saturating_sub(t0.elapsed()));
        (closed, None)
    }

    pub fn cache_bytes(&self) -> u64 {
        self.cache_bytes.load(Ordering::Relaxed)
    }

    /// Cancel these jobs of `layer` (generations the scheduler orphaned: the tile left the
    /// cover or its pending work became moot). A fetch is aborted and still gets its `Fetched`
    /// reply, with [`FetchErrorKind::Cancelled`]; a CPU job that has not finished replies
    /// [`WorkerMsg::Dropped`] (queued: at once, running: at its next safe point). Either way
    /// the in-flight slot is freed early; a job that already finished replied normally.
    /// Unknown ids are ignored.
    pub fn cancel(&self, layer: LayerKind, jobs: &[u64]) {
        if jobs.is_empty() {
            return;
        }
        if layer == LayerKind::World {
            let mut map = lock(&self.fetches);
            for j in jobs {
                if let Some(h) = map.remove(j) {
                    h.abort();
                }
            }
        }
        let ids: Vec<u64> = jobs.iter().map(|&j| pool_id(layer, j)).collect();
        self.pool.cancel(&ids);
    }

    /// CPU job counters (`debug_stats` `jobs_*`).
    pub fn job_stats(&self) -> JobStats {
        self.pool.stats()
    }

    /// Worker threads of the CPU pool.
    pub fn cpu_threads(&self) -> usize {
        self.pool.threads()
    }

    /// Tests: occupy every CPU pool thread with a job that ignores cancellation and blocks until
    /// the returned sender is dropped; returns once all of them run. Jobs submitted meanwhile
    /// stay queued, so "jobs in flight" is a fact, not a race.
    #[cfg(test)]
    pub(crate) fn hold_cpu_threads(&self) -> crossbeam_channel::Sender<()> {
        let (gate_tx, gate_rx) = crossbeam_channel::unbounded::<()>();
        let (started_tx, started_rx) = crossbeam_channel::unbounded::<()>();
        for i in 0..self.pool.threads() {
            let (gate, started) = (gate_rx.clone(), started_tx.clone());
            // Ids far above the scheduler's generation numbers.
            self.pool.submit(
                JobSpec::new((1 << 62) + i as u64, Priority::on_demand(f64::NEG_INFINITY)),
                move |_| {
                    let _ = started.send(());
                    let _ = gate.recv();
                    JobEnd::Done(None)
                },
                || {},
            );
        }
        for _ in 0..self.pool.threads() {
            started_rx.recv_timeout(std::time::Duration::from_secs(5)).expect("hold job started");
        }
        gate_tx
    }

    pub fn try_recv(&self) -> Option<WorkerMsg> {
        self.rx.try_recv().ok()
    }

    /// Start a job. CPU jobs (`Decode`, `Terrain`) queue by `prio` in the shared pool; fetches
    /// go to the I/O runtime (`prio` unused).
    pub fn submit(&self, job: Job, prio: Priority) {
        match job {
            Job::Fetch { key, ring, job } => {
                self.spawn_fetch(key, job, move |result| WorkerMsg::Fetched { key, ring, job, result });
            }
            Job::FetchData { key, job } => {
                self.spawn_fetch(key, job, move |result| WorkerMsg::DataFetched { key, job, result });
            }
            Job::DecodeData { key, job, bytes, level } => {
                let tx = self.tx.clone();
                let frame = self.frame;
                let terrain = self.terrain.clone();
                let profile = self.profile.clone();
                let synthetic = self.fetcher.is_none();
                let tx_cancel = self.tx.clone();
                let spec = JobSpec::new(pool_id(LayerKind::World, job), prio);
                self.pool.submit(
                    spec,
                    move |ctx| {
                        if ctx.is_cancelled() {
                            return JobEnd::Cancelled;
                        }
                        let result = catch_unwind(AssertUnwindSafe(|| {
                            if synthetic {
                                Ok(synthetic_data_tile(key, level, frame, &terrain, &profile))
                            } else {
                                decode_level(key, level, frame, &terrain, &bytes, &profile)
                            }
                        }))
                            .unwrap_or_else(|p| Err(format!("data decode panic: {}", panic_text(p))));
                        if result.is_ok() && ctx.is_cancelled() {
                            return JobEnd::Cancelled;
                        }
                        match result {
                            Ok(d) => {
                                let d = Arc::new(d);
                                // Main thread first (resident tile), then the dependants.
                                let _ = tx.send(WorkerMsg::DataDecoded { key, job, result: Ok(d.clone()) });
                                JobEnd::Done(Some(d as Output))
                            }
                            Err(e) => {
                                let _ = tx.send(WorkerMsg::DataDecoded { key, job, result: Err(e) });
                                JobEnd::Failed
                            }
                        }
                    },
                    move || {
                        let _ = tx_cancel.send(WorkerMsg::DataDropped { key, job });
                    },
                );
            }
            Job::MeshChunk { key, ring, job, data, lm } => {
                let tx = self.tx.clone();
                let profile = self.profile.clone();
                let on_cancel = self.dropped_reply(LayerKind::World, key, ring, job);
                // Ready tiles go in the closure; pending ones become dependencies (in order).
                let mut deps = Vec::new();
                let mut dep = |r: DataRef| match r {
                    DataRef::Ready(d) => Some(d),
                    DataRef::After(j) => {
                        deps.push(pool_id(LayerKind::World, j));
                        None
                    }
                };
                let ready = dep(data);
                let (same, lm_ready, lm_wait) = match lm {
                    LmRef::None => (false, None, false),
                    LmRef::Same => (true, None, false),
                    LmRef::Other(r) => {
                        let d = dep(r);
                        let wait = d.is_none();
                        (false, d, wait)
                    }
                };
                let spec = JobSpec::new(pool_id(LayerKind::World, job), prio).with_deps(deps);
                self.pool.submit(
                    spec,
                    move |ctx| {
                        let mut outs = ctx.deps().iter().cloned();
                        let mut next = || outs.next().and_then(|o| o.downcast::<DataTile>().ok());
                        let Some(data) = ready.or_else(&mut next) else { return JobEnd::Cancelled };
                        let other = if lm_wait {
                            match next() {
                                Some(d) => Some(d),
                                None => return JobEnd::Cancelled,
                            }
                        } else {
                            lm_ready
                        };
                        let lm: Option<&DataTile> = if same { Some(&data) } else { other.as_deref() };
                        let stop = || ctx.is_cancelled();
                        let result = catch_unwind(AssertUnwindSafe(|| build_chunk_with(&data, lm, key, ring, &profile, &stop)))
                            .unwrap_or_else(|p| Err(format!("chunk panic: {}", panic_text(p))));
                        let result = match result {
                            Ok(None) => return JobEnd::Cancelled,
                            Ok(Some(_)) if ctx.is_cancelled() => return JobEnd::Cancelled,
                            Ok(Some(d)) => Ok(d),
                            Err(e) => Err(e),
                        };
                        let failed = result.is_err();
                        let _ = tx.send(WorkerMsg::Decoded { key, ring, job, result });
                        if failed {
                            JobEnd::Failed
                        } else {
                            JobEnd::Done(None)
                        }
                    },
                    on_cancel,
                );
            }
            Job::MeshLandmarks { key, job, data } => {
                let tx = self.tx.clone();
                let profile = self.profile.clone();
                let on_cancel = self.dropped_reply(LayerKind::Landmarks, key, 0, job);
                let (ready, deps) = match data {
                    DataRef::Ready(d) => (Some(d), Vec::new()),
                    DataRef::After(j) => (None, vec![pool_id(LayerKind::World, j)]),
                };
                let spec = JobSpec::new(pool_id(LayerKind::Landmarks, job), prio).with_deps(deps);
                self.pool.submit(
                    spec,
                    move |ctx| {
                        let data = match ready {
                            Some(d) => d,
                            None => match ctx.deps().first().cloned().and_then(|o| o.downcast::<DataTile>().ok()) {
                                Some(d) => d,
                                None => return JobEnd::Cancelled,
                            },
                        };
                        let stop = || ctx.is_cancelled();
                        let result = catch_unwind(AssertUnwindSafe(|| build_landmark_tile(&data, &profile, &stop)))
                            .unwrap_or_else(|p| Err(format!("landmark panic: {}", panic_text(p))));
                        let result = match result {
                            Ok(None) => return JobEnd::Cancelled,
                            Ok(Some(_)) if ctx.is_cancelled() => return JobEnd::Cancelled,
                            Ok(Some(d)) => Ok(d),
                            Err(e) => Err(e),
                        };
                        let failed = result.is_err();
                        let _ = tx.send(WorkerMsg::LandmarksBuilt { key, job, result });
                        if failed {
                            JobEnd::Failed
                        } else {
                            JobEnd::Done(None)
                        }
                    },
                    on_cancel,
                );
            }
            Job::Decode { key, ring, job, payload } => {
                let tx = self.tx.clone();
                let frame = self.frame;
                let terrain = self.terrain.clone();
                let profile = self.profile.clone();
                let seed = self.seed.clone();
                let on_cancel = self.dropped_reply(LayerKind::World, key, ring, job);
                let spec = JobSpec::new(pool_id(LayerKind::World, job), prio);
                self.pool.submit(
                    spec,
                    move |ctx| {
                        let stop = || ctx.is_cancelled();
                        let result = catch_unwind(AssertUnwindSafe(|| match (&seed, &payload) {
                            (Some(s), Payload::Synthetic) => build_seed_tile_stoppable(key, ring, frame, &terrain, s, &profile, &stop),
                            _ => build_tile_stoppable(key, ring, frame, &terrain, &payload, &profile, &stop),
                        }))
                        .unwrap_or_else(|p| Err(format!("decode panic: {}", panic_text(p))));
                        // Last safe point: a stale result is dropped here, not sent.
                        let result = match result {
                            Ok(None) => return JobEnd::Cancelled,
                            Ok(Some(_)) if ctx.is_cancelled() => return JobEnd::Cancelled,
                            Ok(Some(d)) => Ok(d),
                            Err(e) => Err(e),
                        };
                        let failed = result.is_err();
                        let _ = tx.send(WorkerMsg::Decoded { key, ring, job, result });
                        if failed {
                            JobEnd::Failed
                        } else {
                            JobEnd::Done(None)
                        }
                    },
                    on_cancel,
                );
            }
            Job::Terrain { key, ring, job, detail } => {
                let tx = self.tx.clone();
                let frame = self.frame;
                let terrain = self.terrain.clone();
                let profile = self.profile.clone();
                let seed = self.seed.clone();
                let on_cancel = self.dropped_reply(LayerKind::Terrain, key, ring, job);
                let spec = JobSpec::new(pool_id(LayerKind::Terrain, job), prio);
                self.pool.submit(
                    spec,
                    move |ctx| {
                        // The TIN build stops at its safe points (`build_terrain_tile_until`).
                        let stop = || ctx.is_cancelled();
                        let result = catch_unwind(AssertUnwindSafe(|| {
                            build_terrain_data_until(key, ring, frame, &terrain, &profile, detail, seed.as_deref(), &stop)
                        }))
                        .unwrap_or_else(|p| Err(format!("terrain panic: {}", panic_text(p))));
                        let result = match result {
                            Ok(None) => return JobEnd::Cancelled,
                            Ok(Some(_)) if ctx.is_cancelled() => return JobEnd::Cancelled,
                            Ok(Some(d)) => Ok(d),
                            Err(e) => Err(e),
                        };
                        let failed = result.is_err();
                        let _ = tx.send(WorkerMsg::TerrainBuilt { key, ring, job, result });
                        if failed {
                            JobEnd::Failed
                        } else {
                            JobEnd::Done(None)
                        }
                    },
                    on_cancel,
                );
            }
        }
    }
}

impl Worker {
    /// Fetch `key` on the I/O runtime; `reply` turns the result into the message sent back
    /// (exactly one per job, also on cancel or panic). Registered for [`Worker::cancel`] by `job`.
    fn spawn_fetch(&self, key: TileKey, job: u64, reply: impl FnOnce(Result<(Bytes, Source), FetchError>) -> WorkerMsg + Send + 'static) {
        let tx = self.tx.clone();
        let (Some(rt), Some(f)) = (&self.rt, &self.fetcher) else {
            let _ = tx.send(reply(Err("no tile source".into())));
            return;
        };
        let f = f.clone();
        // Inner task so a panic or a cancel becomes a JoinError instead of a lost reply
        // (the scheduler's in-flight counters need exactly one reply per job).
        let inner = rt.spawn(async move { f.fetch(key).await });
        let fetches = self.fetches.clone();
        // Registered before the outer task exists, so its removal always comes after.
        lock(&fetches).insert(job, inner.abort_handle());
        rt.spawn(async move {
            let result = match inner.await {
                Ok(r) => r,
                Err(e) if e.is_cancelled() => Err(FetchError::new(FetchErrorKind::Cancelled, format!("{key}: cancelled"))),
                Err(e) => Err(format!("fetch task: {e}").into()),
            };
            lock(&fetches).remove(&job);
            let _ = tx.send(reply(result));
        });
    }

    /// The `Dropped` reply of a cancelled CPU job (sent at most once, by the pool).
    fn dropped_reply(&self, layer: LayerKind, key: TileKey, ring: usize, job: u64) -> impl FnOnce() + Send + 'static {
        let tx = self.tx.clone();
        move || {
            let _ = tx.send(WorkerMsg::Dropped { layer, key, ring, job });
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Never block the main thread on in-flight HTTP requests (`shutdown` is the bounded,
        // waiting stop).
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
        // Signal only: the cache thread flushes and closes the file on its own.
        if let Some(d) = self.fetcher.take().and_then(|f| f.disk_handle()) {
            d.signal_close();
        }
    }
}
