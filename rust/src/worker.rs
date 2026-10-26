//! Threads: a small tokio runtime for I/O (fetch + disk cache) and a dedicated rayon pool for
//! CPU work (MVT decode / synthetic generation / meshing / lights). Results go back to
//! the main thread over one crossbeam channel. Workers only produce plain Rust structs; they
//! never call Godot APIs, and panics are caught and turned into error messages.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use crossbeam_channel::{Receiver, Sender};

use crate::fetch::{Fetcher, Source};
use crate::geo::{Frame, TileKey};
use crate::lights::LightSet;
use crate::mesher::{mesh_tile, MeshData};
use crate::ground::mesh_ground;
use crate::mvt::{decode_tile, Building, DecodedTile, GroundFeatures, TileProjector};
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
        let d = build_tile(key, 0, frame, &Payload::Synthetic).unwrap();
        let expect = synthetic::generate(key, frame)
            .iter()
            .map(|b| crate::mesher::resolve(b).height)
            .fold(0.0f32, f32::max);
        assert!(d.max_height > 10.0);
        assert_eq!(d.max_height, expect);
        assert_eq!(build_tile(key, 0, frame, &Payload::Bytes(Bytes::new())).unwrap().max_height, 0.0);
        // Lights only for R2/R3.
        assert!(d.lights.is_empty() && !d.mesh.is_empty());
        let k14 = crate::geo::tile_for(54.6872, 25.2797, 14);
        let d2 = build_tile(k14, 2, frame, &Payload::Synthetic).unwrap();
        assert!(!d2.lights.is_empty() && !d2.mesh.is_empty());
        assert!(d2.lights.clustered_positions.len() < d2.lights.positions.len());
        let k13 = crate::geo::tile_for(54.6872, 25.2797, 13);
        let d3 = build_tile(k13, 3, frame, &Payload::Synthetic).unwrap();
        assert!(!d3.lights.is_empty() && d3.mesh.is_empty());
        // Ground for R0-R2 only (synthetic city has streets and districts everywhere).
        assert!(!d.ground.is_empty() && !d2.ground.is_empty() && d3.ground.is_empty());
        assert!(d.ground.uvs.is_empty());
        assert_eq!(d.ground, build_tile(key, 0, frame, &Payload::Synthetic).unwrap().ground);
        // Deterministic.
        assert_eq!(d3.lights, build_tile(k13, 3, frame, &Payload::Synthetic).unwrap().lights);
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
    Fetched { key: TileKey, ring: usize, job: u64, result: Result<(Bytes, Source), String> },
    Decoded { key: TileKey, ring: usize, job: u64, result: Result<TileData, String> },
    Info(String),
}

/// CPU stage: payload -> buildings, ground features (if `want_ground`) and projector
/// (shared by real and synthetic tiles).
pub fn decode_payload(key: TileKey, frame: Frame, payload: &Payload, want_ground: bool) -> Result<DecodedTile, String> {
    match payload {
        Payload::Synthetic => Ok(DecodedTile {
            buildings: synthetic::generate(key, frame),
            ground: if want_ground { synthetic::generate_ground(key, frame) } else { GroundFeatures::default() },
            proj: TileProjector::new(key, frame, 4096.0),
        }),
        Payload::Bytes(b) => decode_tile(b, key, frame, want_ground),
    }
}

/// CPU stage: payload -> buildings + projector.
pub fn tile_buildings_proj(key: TileKey, frame: Frame, payload: &Payload) -> Result<(Vec<Building>, TileProjector), String> {
    decode_payload(key, frame, payload, false).map(|t| (t.buildings, t.proj))
}

/// CPU stage: payload -> buildings.
pub fn tile_buildings(key: TileKey, frame: Frame, payload: &Payload) -> Result<Vec<Building>, String> {
    tile_buildings_proj(key, frame, payload).map(|(b, _)| b)
}

/// CPU stage: payload -> flat per-tile output (building + ground meshes per ring, lights). The
/// decoded structs are dropped here, on the worker; only flat buffers cross to the main thread.
pub fn build_tile(key: TileKey, ring: usize, frame: Frame, payload: &Payload) -> Result<TileData, String> {
    let t0 = std::time::Instant::now();
    let want_ground = crate::ground::ground_params(ring).is_some();
    let DecodedTile { buildings, ground: gf, proj } = decode_payload(key, frame, payload, want_ground)?;
    let mesh = mesh_tile(&buildings, &proj, ring);
    let ground = mesh_ground(&gf, &proj, ring);
    drop(gf);
    let lights = if ring >= 2 { LightSet::from_buildings(&buildings) } else { LightSet::default() };
    let building_count = buildings.len();
    let max_height = buildings.iter().map(|b| crate::mesher::resolve(b).height).fold(0.0f32, f32::max);
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
    cache_bytes: Arc<AtomicU64>,
}

impl Worker {
    /// `url` empty = synthetic city (no runtime / fetcher needed).
    pub fn new(frame: Frame, url: &str, cache_dir: &str) -> Result<Self, String> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(cpus.saturating_sub(2).clamp(2, 4))
            .thread_name(|i| format!("simplane-cpu-{i}"))
            .panic_handler(|_| {}) // jobs catch their own panics; never abort the host
            .build()
            .map_err(|e| format!("rayon pool: {e}"))?;
        let cache_bytes = Arc::new(AtomicU64::new(0));
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
                Box::new(move |s| {
                    let _ = info_tx.send(WorkerMsg::Info(s));
                }),
            )?;
            (Some(rt), Some(Arc::new(f)))
        };
        Ok(Self { rt, pool, tx, rx, fetcher, frame, cache_bytes })
    }

    pub fn is_synthetic(&self) -> bool {
        self.fetcher.is_none()
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
                        Err(e) => Err(format!("fetch task: {e}")),
                    };
                    let _ = tx.send(WorkerMsg::Fetched { key, ring, job, result });
                });
            }
            Job::Decode { key, ring, job, payload } => {
                let tx = self.tx.clone();
                let frame = self.frame;
                self.pool.spawn(move || {
                    let result = catch_unwind(AssertUnwindSafe(|| build_tile(key, ring, frame, &payload)))
                        .unwrap_or_else(|p| Err(format!("decode panic: {}", panic_text(p))));
                    let _ = tx.send(WorkerMsg::Decoded { key, ring, job, result });
                });
            }
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Never block the main thread on in-flight HTTP requests.
        if let Some(rt) = self.rt.take() {
            rt.shutdown_background();
        }
    }
}
