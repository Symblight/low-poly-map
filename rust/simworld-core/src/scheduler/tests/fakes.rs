//! Fake tiles and the fake worker of the scheduler harness.

use super::*;

/// The default world profile (the spike's constants).
pub(super) fn dp() -> Arc<Profile> {
    Arc::new(crate::profile::test_default().clone())
}
pub(super) fn bands() -> BandParams {
    *crate::profile::test_default().bands()
}
pub(super) fn layer() -> Layer {
    Layer::world(&bands(), 4, 2)
}
pub(super) fn fade_cap() -> Duration {
    Duration::from_secs_f64(crate::profile::test_default().fades.cap_s)
}

/// Box height of fake tiles: deterministic 20..110 m by key (exercises per-tile heights).
pub(super) fn fake_height(k: TileKey) -> f32 {
    20.0 + (crate::mvt::splitmix64(((k.z as u64) << 48) ^ ((k.x as u64) << 24) ^ k.y as u64) % 90) as f32
}

pub(super) fn fake_data(k: TileKey, ring: usize) -> TileData {
    let lights = if ring >= 2 {
        LightSet {
            positions: vec![[0.0, 10.0, 0.0], [5.0, 10.0, 5.0]],
            colors: vec![[1.0; 4]; 2],
            clustered_positions: vec![[2.5, 10.0, 2.5]],
            clustered_colors: vec![[1.0; 4]],
            ..LightSet::default()
        }
    } else {
        LightSet::default()
    };
    TileData {
        building_count: 0,
        lights: Arc::new(lights),
        mesh: Arc::default(),
        // One triangle, a different count per band, so re-deliveries can be checked.
        ground: Arc::new(MeshData {
            positions: vec![[0.0; 3]; 3 + ring],
            indices: vec![0, 1, 2],
            ..MeshData::default()
        }),
        max_height: fake_height(k),
        cpu_us: 0,
        terrain: None,
    }
}

/// Stand-in terrain tile: 2 triangles, `4 + ring` vertices (re-deliveries can be checked).
pub(super) fn fake_terrain(ring: usize) -> TileData {
    let mesh = MeshData { positions: vec![[0.0; 3]; 4 + ring], indices: vec![0, 1, 2, 0, 2, 3], ..MeshData::default() };
    TileData {
        terrain: Some(Arc::new(TerrainTile { mesh, surface_triangles: 2, ..TerrainTile::default() })),
        ..TileData::default()
    }
}

/// Worker stand-in: jobs finish after a fixed number of frames (fetch 3, decode 2).
pub(super) struct FakeWorker {
    pub(super) pending: VecDeque<(u64, WorkerMsg)>,
    pub(super) frame: u64,
    pub(super) real: Option<Frame>,
    /// Profile of the real builds (`real`); None = the test default.
    pub(super) profile: Option<Arc<Profile>>,
    /// Terrain jobs: real TINs on this terrain (with `real` and `profile`), else stand-ins.
    pub(super) terrain: Option<Terrain>,
    /// Data tiles (Tiles v2): the archive's bytes per data tile key (absent = empty tile).
    pub(super) data_src: AHashMap<TileKey, Bytes>,
    /// Decoded data tiles by decode job (decoded at submit, so a dependent chunk job finds it).
    pub(super) data_out: AHashMap<u64, Arc<crate::data::DataTile>>,
    /// Data jobs seen: fetches, decodes.
    pub(super) data_jobs: (u32, u32),
    /// Zoom of every per-tile `Fetch`.
    pub(super) fetch_zooms: Vec<u8>,
    /// Last building mesh built per chunk cut from a data tile.
    pub(super) chunk_meshes: std::collections::BTreeMap<TileKey, MeshData>,
    /// Whether each chunk job folded landmarks in (T2b).
    pub(super) chunk_lm: std::collections::BTreeMap<TileKey, bool>,
    /// Data tiles are generated (the synthetic city, T2c) instead of decoded.
    pub(super) synthetic_data: bool,
}

impl FakeWorker {
    pub(super) fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            frame: 0,
            real: None,
            profile: None,
            terrain: None,
            data_src: AHashMap::new(),
            data_out: AHashMap::new(),
            data_jobs: (0, 0),
            fetch_zooms: Vec::new(),
            chunk_meshes: Default::default(),
            chunk_lm: Default::default(),
            synthetic_data: false,
        }
    }
    pub(super) fn submit(&mut self, jobs: &mut Vec<Job>) {
        for j in jobs.drain(..) {
            let (delay, msg) = match j {
                Job::Fetch { key, ring, job } => {
                    self.fetch_zooms.push(key.z);
                    (3, WorkerMsg::Fetched { key, ring, job, result: Ok((Bytes::new(), Source::Network)) })
                }
                Job::Decode { key, ring, job, payload } => {
                    let result = match self.real {
                        Some(f) => build_tile(key, ring, f, &payload, self.profile.as_deref().unwrap_or(crate::profile::test_default())),
                        None => Ok(fake_data(key, ring)),
                    };
                    (2, WorkerMsg::Decoded { key, ring, job, result })
                }
                Job::FetchData { key, job } => {
                    self.data_jobs.0 += 1;
                    let bytes = self.data_src.get(&key).cloned().unwrap_or_default();
                    (3, WorkerMsg::DataFetched { key, job, result: Ok((bytes, Source::Network)) })
                }
                Job::DecodeData { key, job, bytes, level } => {
                    self.data_jobs.1 += 1;
                    let f = self.real.unwrap_or(Frame::new(VILNIUS.0, VILNIUS.1));
                    let p = self.profile.clone().unwrap_or_else(dp);
                    let result = if self.synthetic_data {
                        Ok(crate::data::synthetic_data_tile(key, level, f, &Terrain::flat(), &p))
                    } else {
                        crate::data::decode_level(key, level, f, &Terrain::flat(), &bytes, &p)
                    }
                    .map(Arc::new);
                    if let Ok(d) = &result {
                        self.data_out.insert(job, d.clone());
                    }
                    (2, WorkerMsg::DataDecoded { key, job, result })
                }
                Job::MeshChunk { key, ring, job, data, lm } => {
                    let get = |r: crate::worker::DataRef| match r {
                        crate::worker::DataRef::Ready(d) => Some(d),
                        crate::worker::DataRef::After(j) => self.data_out.get(&j).cloned(),
                    };
                    let d = get(data);
                    let (same, other, missing) = match lm {
                        crate::worker::LmRef::None => (false, None, false),
                        crate::worker::LmRef::Same => (true, None, false),
                        crate::worker::LmRef::Other(r) => {
                            let o = get(r);
                            let m = o.is_none();
                            (false, o, m)
                        }
                    };
                    let p = self.profile.clone().unwrap_or_else(dp);
                    match d.filter(|_| !missing) {
                        Some(d) => {
                            self.chunk_lm.insert(key, same || other.is_some());
                            let lmt: Option<&crate::data::DataTile> = if same { Some(&d) } else { other.as_deref() };
                            let result = crate::data::build_chunk_with(&d, lmt, key, ring, &p, &|| false).map(|o| o.expect("not stopped"));
                            if let Ok(t) = &result {
                                self.chunk_meshes.insert(key, (*t.mesh).clone());
                            }
                            (2, WorkerMsg::Decoded { key, ring, job, result })
                        }
                        None => (1, WorkerMsg::Dropped { layer: LayerKind::World, key, ring, job }),
                    }
                }
                Job::MeshLandmarks { key, job, data } => {
                    let d = match data {
                        crate::worker::DataRef::Ready(d) => Some(d),
                        crate::worker::DataRef::After(j) => self.data_out.get(&j).cloned(),
                    };
                    let p = self.profile.clone().unwrap_or_else(dp);
                    match d {
                        Some(d) => {
                            let result = crate::data::build_landmark_tile(&d, &p, &|| false).map(|o| o.expect("not stopped"));
                            (2, WorkerMsg::LandmarksBuilt { key, job, result })
                        }
                        None => (1, WorkerMsg::Dropped { layer: LayerKind::Landmarks, key, ring: 0, job }),
                    }
                }
                Job::Terrain { key, ring, job, detail } => {
                    let result = match (self.real, &self.profile, &self.terrain) {
                        (Some(f), Some(p), Some(t)) => crate::worker::build_terrain_data(key, ring, f, t, p, detail, None),
                        _ => Ok(fake_terrain(ring)),
                    };
                    (2, WorkerMsg::TerrainBuilt { key, ring, job, result })
                }
            };
            self.pending.push_back((self.frame + delay, msg));
        }
    }
    pub(super) fn step(&mut self, s: &mut Scheduler, now: Instant) {
        self.frame += 1;
        let mut keep = VecDeque::new();
        while let Some((t, m)) = self.pending.pop_front() {
            if t <= self.frame {
                let _ = s.handle(m, now);
            } else {
                keep.push_back((t, m));
            }
        }
        self.pending = keep;
    }
}
