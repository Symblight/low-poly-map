//! Threads: a small tokio runtime for I/O (fetch; the disk cache has its own thread, owned by
//! [`crate::fetch::DiskCache`]) and a dedicated rayon pool for
//! CPU work (MVT decode / synthetic generation / meshing / lights). Results go back to
//! the main thread over one crossbeam channel. Workers only produce plain Rust structs; they
//! never call Godot APIs, and panics are caught and turned into error messages.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use crossbeam_channel::{Receiver, Sender};

use crate::fetch::{DiskCache, FetchError, FetchParams, Fetcher, Note, Source};
use crate::geo::{Frame, TileKey};
use crate::lights::LightSet;
use crate::mesher::{mesh_tile, MeshData};
use crate::ground::mesh_ground;
use crate::mvt::{decode_tile, Building, DecodedTile, GroundFeatures, TileProjector};
use crate::profile::Profile;
use crate::synthetic;

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
    /// tile. Empty for R3 and for tiles without ground features.
    pub ground: MeshData,
    /// Highest roof top (m above ground) of any building decoded in the tile, including
    /// buffer copies owned by neighbours; 0 for an empty tile. Height of the slant-distance box.
    pub max_height: f32,
    /// Worker-side CPU time (decode + mesh + lights), microseconds.
    pub cpu_us: u64,
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
        // Lights only for R2/R3.
        assert!(d.lights.is_empty() && !d.mesh.is_empty());
        let k14 = crate::geo::tile_for(54.6872, 25.2797, 14);
        let d2 = build_tile(k14, 2, frame, &Payload::Synthetic, &p).unwrap();
        assert!(!d2.lights.is_empty() && !d2.mesh.is_empty());
        assert!(d2.lights.clustered_positions.len() < d2.lights.positions.len());
        let k13 = crate::geo::tile_for(54.6872, 25.2797, 13);
        let d3 = build_tile(k13, 3, frame, &Payload::Synthetic, &p).unwrap();
        assert!(!d3.lights.is_empty() && d3.mesh.is_empty());
        // Ground for R0-R2 only (synthetic city has streets and districts everywhere).
        assert!(!d.ground.is_empty() && !d2.ground.is_empty() && d3.ground.is_empty());
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
            let p = Profile::new(w).unwrap();
            build_tile(key, ring, frame, &Payload::Synthetic, &p).unwrap().lights
        };
        let reference = lit(2, 2);
        assert!(!reference.is_empty());
        assert!(lit(2, 1).is_empty() && lit(2, 0).is_empty(), "default: R0/R1 unlit");
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
}

#[derive(Debug)]
pub enum WorkerMsg {
    Fetched { key: TileKey, ring: usize, job: u64, result: Result<(Bytes, Source), FetchError> },
    Decoded { key: TileKey, ring: usize, job: u64, result: Result<TileData, String> },
    Info(String),
    /// Logged as a (rate-limited) warning on the main thread.
    Warn(String),
    /// The disk cache was disabled for the session: logged as a warning, status degraded.
    CacheDisabled(String),
    /// A configuration problem (logged as an error on the main thread).
    Error(String),
}

/// CPU stage: payload -> buildings, ground features (if `want_ground`) and projector
/// (shared by real and synthetic tiles).
pub fn decode_payload(
    key: TileKey,
    frame: Frame,
    payload: &Payload,
    want_ground: bool,
    profile: &Profile,
) -> Result<DecodedTile, String> {
    match payload {
        Payload::Synthetic => Ok(DecodedTile {
            buildings: synthetic::generate(key, frame),
            ground: if want_ground { synthetic::generate_ground(key, frame, profile) } else { GroundFeatures::default() },
            proj: TileProjector::new(key, frame, 4096.0),
        }),
        Payload::Bytes(b) => decode_tile(b, key, frame, want_ground, profile),
    }
}

/// CPU stage: payload -> buildings + projector.
pub fn tile_buildings_proj(
    key: TileKey,
    frame: Frame,
    payload: &Payload,
    profile: &Profile,
) -> Result<(Vec<Building>, TileProjector), String> {
    decode_payload(key, frame, payload, false, profile).map(|t| (t.buildings, t.proj))
}

/// CPU stage: payload -> buildings.
pub fn tile_buildings(key: TileKey, frame: Frame, payload: &Payload, profile: &Profile) -> Result<Vec<Building>, String> {
    tile_buildings_proj(key, frame, payload, profile).map(|(b, _)| b)
}

/// CPU stage: payload -> flat per-tile output (building + ground meshes per ring, lights). The
/// decoded structs are dropped here, on the worker; only flat buffers cross to the main thread.
pub fn build_tile(key: TileKey, ring: usize, frame: Frame, payload: &Payload, profile: &Profile) -> Result<TileData, String> {
    let t0 = std::time::Instant::now();
    let want_ground = profile.ground_params(ring).is_some();
    let DecodedTile { buildings, ground: gf, proj } = decode_payload(key, frame, payload, want_ground, profile)?;
    let mesh = mesh_tile(&buildings, &proj, ring, profile);
    let ground = mesh_ground(&gf, &proj, ring, profile);
    drop(gf);
    let lights = if ring >= profile.lights.min_ring as usize { LightSet::from_buildings(&buildings, profile) } else { LightSet::default() };
    let building_count = buildings.len();
    let max_height = buildings.iter().map(|b| crate::mesher::resolve(b, profile).height).fold(0.0f32, f32::max);
    drop(buildings);
    Ok(TileData {
        building_count,
        lights: Arc::new(lights),
        mesh,
        ground,
        max_height,
        cpu_us: t0.elapsed().as_micros() as u64,
    })
}

fn panic_text(p: Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

pub struct Worker {
    rt: Option<tokio::runtime::Runtime>,
    pool: rayon::ThreadPool,
    tx: Sender<WorkerMsg>,
    rx: Receiver<WorkerMsg>,
    fetcher: Option<Arc<Fetcher>>,
    frame: Frame,
    profile: Arc<Profile>,
    cache_bytes: Arc<AtomicU64>,
    /// A disk cache folder is in use (tile source + non-empty `cache_dir`).
    disk_cache: bool,
    /// Source and cache settings, to decide whether a re-configure may reuse the open cache.
    url: String,
    cache_dir: String,
    cache_cap: u64,
}

impl Worker {
    /// `url` empty = synthetic city (no runtime / fetcher needed).
    pub fn new(frame: Frame, url: &str, cache_dir: &str, profile: Arc<Profile>) -> Result<Self, String> {
        Self::with_cache(frame, url, cache_dir, profile, None)
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
        let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(cpus.saturating_sub(2).clamp(2, 4))
            .thread_name(|i| format!("simplane-cpu-{i}"))
            .panic_handler(|_| {}) // jobs catch their own panics; never abort the host
            .build()
            .map_err(|e| format!("rayon pool: {e}"))?;
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
            (Some(rt), Some(Arc::new(f)))
        };
        let disk_cache = fetcher.is_some() && !cache_dir.is_empty();
        Ok(Self {
            rt,
            pool,
            tx,
            rx,
            fetcher,
            frame,
            profile,
            cache_bytes,
            disk_cache,
            url: url.to_string(),
            cache_dir: cache_dir.to_string(),
            cache_cap,
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
    /// the kept cache. CPU jobs on the rayon pool are not waited for; they never touch the
    /// disk and their results are discarded. Dropping a `Worker` instead only signals the
    /// stop and the cache close, without waiting.
    pub fn shutdown(mut self, wait: std::time::Duration, keep_cache: bool) -> (bool, Option<Arc<DiskCache>>) {
        let t0 = std::time::Instant::now();
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

    pub fn try_recv(&self) -> Option<WorkerMsg> {
        self.rx.try_recv().ok()
    }

    pub fn submit(&self, job: Job) {
        match job {
            Job::Fetch { key, ring, job } => {
                let tx = self.tx.clone();
                let (Some(rt), Some(f)) = (&self.rt, &self.fetcher) else {
                    let _ = tx.send(WorkerMsg::Fetched {
                        key,
                        ring,
                        job,
                        result: Err("no tile source".into()),
                    });
                    return;
                };
                let f = f.clone();
                rt.spawn(async move {
                    // Inner task so a panic becomes a JoinError instead of a lost reply.
                    let result = match tokio::spawn(async move { f.fetch(key).await }).await {
                        Ok(r) => r,
                        Err(e) => Err(format!("fetch task: {e}").into()),
                    };
                    let _ = tx.send(WorkerMsg::Fetched { key, ring, job, result });
                });
            }
            Job::Decode { key, ring, job, payload } => {
                let tx = self.tx.clone();
                let frame = self.frame;
                let profile = self.profile.clone();
                self.pool.spawn(move || {
                    let result = catch_unwind(AssertUnwindSafe(|| build_tile(key, ring, frame, &payload, &profile)))
                        .unwrap_or_else(|p| Err(format!("decode panic: {}", panic_text(p))));
                    let _ = tx.send(WorkerMsg::Decoded { key, ring, job, result });
                });
            }
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
