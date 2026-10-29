//! Configuration and lifecycle: `configure`, `configure_world` (validate, lock, swap the
//! worker), `shutdown`.

use super::*;

impl World {

    /// Set world origin and data source with the default profile (contract `configure`): a
    /// wrapper over [`World::configure_world`]. Errors are logged.
    pub fn configure(&mut self, origin_lat: f64, origin_lon: f64, tile_url: &str, cache_dir: &str) {
        self.configure_world(WorldOptions {
            origin_lat,
            origin_lon,
            tile_url: tile_url.to_string(),
            cache_dir: cache_dir.to_string(),
            ..WorldOptions::default()
        });
    }

    /// Check options, resolve the profile and load the seed world (if any) without touching the
    /// running world.
    fn prepare(&self, o: &WorldOptions) -> Result<(Profile, Option<Arc<crate::seed::SeedWorld>>), String> {
        if !(o.origin_lat.is_finite() && o.origin_lat.abs() <= MERCATOR_MAX_LAT) {
            return Err(format!("origin_lat = {} is out of range [-{MERCATOR_MAX_LAT:.4}, {MERCATOR_MAX_LAT:.4}]", o.origin_lat));
        }
        if !(o.origin_lon.is_finite() && o.origin_lon.abs() <= 180.0) {
            return Err(format!("origin_lon = {} is out of range [-180, 180]", o.origin_lon));
        }
        let url = o.tile_url.trim();
        let lower = url.to_ascii_lowercase();
        if !url.is_empty() && !(lower.starts_with("http://") || lower.starts_with("https://")) {
            return Err(format!("tile_url `{url}` must be an http:// or https:// URL of a .pmtiles archive (or empty for the synthetic city)"));
        }
        if let Some(d) = &o.diagnostics {
            if d.warnings_per_second == 0 {
                return Err("diagnostics.warnings_per_second = 0 is out of range [1, 1000]".into());
            }
        }
        let file_text;
        let file = match (&o.profile_json, &o.profile_path) {
            (Some((label, text)), _) => Some((label.as_str(), text.as_str())),
            (None, Some(path)) if !path.is_empty() => {
                file_text = std::fs::read_to_string(path).map_err(|e| format!("profile_path {path}: {e}"))?;
                Some((path.as_str(), file_text.as_str()))
            }
            _ => None,
        };
        if let Some([lat, lon]) = o.terrain_center {
            if !(lat.is_finite() && lat.abs() <= MERCATOR_MAX_LAT && lon.is_finite() && lon.abs() <= 180.0) {
                return Err(format!(
                    "terrain_center = ({lat}, {lon}) is out of range (latitude within +-{MERCATOR_MAX_LAT:.4}, longitude within +-180)"
                ));
            }
        }
        if let Some(layers) = &o.layers {
            if let Some(bad) = layers.iter().find(|l| !KNOWN_LAYERS.contains(&l.as_str())) {
                return Err(format!("layers: unknown layer \"{bad}\" (known: {})", KNOWN_LAYERS.join(", ")));
            }
        }
        let profile = crate::profile::resolve(file, o.profile_overrides.as_ref(), &*self.log)?;
        let seed = match o.seed_world.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            None => None,
            Some(path) => Some(self.prepare_seed(o, path, &profile)?),
        };
        Ok((profile, seed))
    }

    /// The seed world rules of `configure_world` (contract 3.1, `docs/spec-world-seed.md` 9.1),
    /// then the load (or the cached world when nothing changed).
    fn prepare_seed(&self, o: &WorldOptions, path: &str, profile: &Profile) -> Result<Arc<crate::seed::SeedWorld>, String> {
        let url = o.tile_url.trim();
        if !url.is_empty() {
            return Err(format!("seed_world: tile_url must be empty (a seed world generates every tile), got `{url}`"));
        }
        if !o.layers.as_ref().is_some_and(|l| l.iter().any(|x| x == "terrain")) {
            return Err("seed_world: needs the terrain layer: pass layers [\"world\", \"terrain\"]".into());
        }
        let t = &profile.terrain;
        if !t.enabled {
            return Err("seed_world: needs terrain.enabled = true in the world profile".into());
        }
        for (k, v) in [("flat_radius_m", t.flat_radius_m), ("flat_blend_m", t.flat_blend_m)] {
            if v != 0.0 {
                return Err(format!(
                    "seed_world: terrain.{k} = {v} must be 0 (the flat zone follows the configure origin; a seed world levels only its runways)"
                ));
            }
        }
        let field = Terrain::from_profile(profile, o.origin_lat, o.origin_lon);
        let fs_reader = |p: &str| -> Result<String, String> {
            if p.starts_with("res://") || p.starts_with("user://") {
                return Err(format!("{p}: res:// and user:// paths need the host's file reader"));
            }
            std::fs::read_to_string(p).map_err(|e| e.to_string())
        };
        let reader: &crate::seed::world::SeedReader = match &o.seed_reader {
            Some(r) => &*r.0,
            None => &fs_reader,
        };
        crate::seed::SeedWorld::load(path, reader, profile, &field, self.seed_cache.as_ref())
    }

    /// Set world origin, data source, world profile and diagnostics (contract
    /// `configure_world`). Everything is validated first: on an error nothing changes (the
    /// previous configuration keeps running) and the error is returned and logged. On success
    /// the old workers are dropped and tiles delivered before are reported by the next
    /// `drain_unload` (teleport); the disk cache is kept (the open file is reused when the tile
    /// source, `cache_dir` and cap are unchanged, else closed with a bounded wait first).
    pub fn configure_world(&mut self, o: WorldOptions) -> ConfigureResult {
        let fail = |w: &Self, e: String| {
            w.log.error(&format!("WorldStreamer: configure_world failed: {e}"));
            ConfigureResult { ok: false, error: e, attributions: w.attributions.clone() }
        };
        let (profile, seed) = match self.prepare(&o) {
            Ok(p) => p,
            Err(e) => return fail(self, e),
        };
        // Cache folder lock, synchronously and before anything changes or opens. A seed world
        // takes it when a real city source could become active (a city file with `real.url`).
        let seed_urls = seed.as_ref().is_some_and(|s| s.cities().iter().any(|c| c.real.as_ref().is_some_and(|r| r.url.is_some())));
        let lock = match self.plan_lock(&o, seed_urls) {
            Ok(l) => l,
            Err(e) => return fail(self, e),
        };
        let custom = profile != Profile::default();
        self.profile = Arc::new(profile);
        self.memory.set_budget_bytes(mb_to_bytes(self.profile.memory.budget_mb));
        if let Some(d) = o.diagnostics.clone() {
            self.diag = d;
        }
        let (origin_lat, origin_lon, cache_dir) = (o.origin_lat, o.origin_lon, o.cache_dir.as_str());
        let url = o.tile_url.trim().to_string();
        // Old workers stop before the new ones start (and before an old lock is released).
        let cap = FetchParams::from_profile(&self.profile).cache_cap_bytes;
        let (_, reuse) = self.stop_running(Some((&url, cache_dir, cap)));
        match lock {
            LockPlan::Keep => {}
            LockPlan::New(l) => self.folder_lock = Some(l),
            LockPlan::None => self.folder_lock = None,
        }

        self.origin_lat = origin_lat;
        self.origin_lon = origin_lon;
        let frame = Frame::new(origin_lat, origin_lon);
        self.frame = Some(frame);
        self.local = LocalSpace::default();
        self.pending_shift = [0.0; 3];
        let wants_terrain = o.layers.as_ref().is_some_and(|l| l.iter().any(|x| x == "terrain"));
        let wants_landmarks = o.layers.as_ref().is_some_and(|l| l.iter().any(|x| x == "landmarks"));
        // Validation warnings once per load (a reused world logged them already).
        let fresh_seed = seed.as_ref().is_some_and(|s| !self.seed_cache.as_ref().is_some_and(|c| Arc::ptr_eq(c, s)));
        if let Some(s) = &seed {
            self.seed_cache = Some(s.clone());
        }
        self.seed = seed;
        self.terrain = if wants_terrain && self.profile.terrain.enabled {
            let [c_lat, c_lon] = o.terrain_center.unwrap_or([origin_lat, origin_lon]);
            let t = Terrain::from_profile(&self.profile, c_lat, c_lon);
            match &self.seed {
                Some(s) => t.with_seed(s.terrain.clone()),
                None => t,
            }
        } else {
            if self.profile.terrain.enabled {
                self.log.warn(&format!("WorldStreamer: {TERRAIN_IGNORED_WARNING}"));
            }
            Terrain::flat()
        };

        self.attributions = match &self.seed {
            Some(s) => s.attributions.clone(),
            None if url.is_empty() => Vec::new(),
            None => vec![OSM_ATTRIBUTION.to_string()],
        };
        let hold = Duration::from_secs_f64(self.profile.streaming.retry_max_s + 5.0);
        self.health = Health::running(hold);
        let mut error = String::new();
        match Worker::with_cache(frame, &url, cache_dir, self.profile.clone(), reuse) {
            Ok(mut w) => {
                w.set_terrain(self.terrain.clone());
                w.set_seed(self.seed.clone());
                w.set_engine_surfaces(o.engine_surfaces);
                let mut s =
                    Scheduler::new(frame, w.is_synthetic(), self.profile.clone()).with_terrain(self.terrain.clone()).with_async_boxes();
                s.set_lod_view(&self.lod_view, self.cover_quality());
                // Tiles v2: chunks are cut from data tiles. An archive's levels are known once it
                // opened (`WorkerMsg::Levels`: two-level, or the compatibility levels of an archive
                // without `simworld_levels`); the synthetic city is a two-level source generated
                // in memory (T2c); a seed world keeps its per-tile generator.
                if self.seed.is_none() {
                    s = if w.is_synthetic() {
                        s.with_data_levels(crate::data::DataLevels::SYNTHETIC)
                    } else if self.per_tile_archive {
                        s
                    } else {
                        s.with_data_pending()
                    };
                    s = s.with_landmark_layer(wants_landmarks);
                }
                self.scheduler = Some(s);
                if self.terrain.is_active() {
                    self.terrain_detail = self.clamp_detail(self.terrain_detail);
                    let mut t = Scheduler::new_terrain(frame, self.profile.clone(), self.terrain.clone()).with_async_boxes();
                    t.set_detail(self.terrain_detail);
                    t.set_lod_view(&self.lod_view, self.cover_quality());
                    self.terrain_sched = Some(t);
                }
                self.worker = Some(w);
            }
            Err(e) => {
                error = format!("cannot start workers: {e}");
                self.log.error(&format!("WorldStreamer: {error}"));
                self.health.set_fatal(&error);
                self.folder_lock = None;
                // A fatal world has no relief: `ground_height_at` answers 0 like a world without terrain.
                self.terrain = Terrain::flat();
                self.seed = None;
            }
        }
        let source = match &self.seed {
            Some(s) => format!(
                "seed world {} ({} cities, {} airports, {} water polygons, loaded in {:.0} ms)",
                s.manifest,
                s.cities().len(),
                s.airports.len(),
                s.files.water.polygons.len(),
                s.stats.build_ms
            ),
            None if url.is_empty() => "synthetic city".to_string(),
            None => url,
        };
        if let Some(s) = self.seed.as_ref().filter(|s| fresh_seed && !s.warnings.is_empty()) {
            // At most a few lines: the full list is in `seed_world_info()["warnings"]`.
            const SHOWN: usize = 5;
            for w in s.warnings.iter().take(SHOWN) {
                self.log.warn(&format!("WorldStreamer: seed world: {w}"));
            }
            if s.warnings.len() > SHOWN {
                self.log.warn(&format!(
                    "WorldStreamer: seed world: {} more warnings (seed_world_info().warnings lists all {})",
                    s.warnings.len() - SHOWN,
                    s.warnings.len()
                ));
            }
        }
        self.log.info(&format!(
            "WorldStreamer configured origin=({:.6}, {:.6}) source={} cache_dir={} [{}]",
            origin_lat, origin_lon, source, cache_dir, self.version
        ));
        if custom || o.profile_json.is_some() || o.profile_path.is_some() {
            let label = match (&o.profile_json, &o.profile_path) {
                (Some((l, _)), _) => l.clone(),
                (None, Some(p)) => p.clone(),
                _ => "defaults".to_string(),
            };
            let ov = if o.profile_overrides.as_ref().is_some_and(|v| v.as_object().is_none_or(|m| !m.is_empty())) {
                " + profile_overrides"
            } else {
                ""
            };
            self.log.info(&format!(
                "WorldStreamer world profile: {label}{ov}{}",
                if custom { "" } else { " (equal to the defaults)" }
            ));
        }
        ConfigureResult { ok: error.is_empty(), error, attributions: self.attributions.clone() }
    }

    /// Decide the cache folder lock for `o` without changing anything: keep the held lock (same
    /// folder), take a new one, or none (synthetic city / no `cache_dir`). Errors when another
    /// world or process holds the folder. Other I/O problems (read-only volume, no lock support)
    /// are logged and the world runs without a lock (the cache itself will likely be disabled).
    fn plan_lock(&self, o: &WorldOptions, seed_urls: bool) -> Result<LockPlan, String> {
        if (o.tile_url.trim().is_empty() && !seed_urls) || o.cache_dir.is_empty() {
            return Ok(LockPlan::None);
        }
        let dir = match canonical_dir(std::path::Path::new(&o.cache_dir)) {
            Ok(d) => d,
            Err(e) => {
                self.log.warn(&format!("WorldStreamer: cache_dir {}: {e}; running without the folder lock", o.cache_dir));
                return Ok(LockPlan::None);
            }
        };
        if self.folder_lock.as_ref().is_some_and(|l| l.dir() == dir) {
            return Ok(LockPlan::Keep);
        }
        match FolderLock::try_acquire(&dir) {
            Ok(l) => Ok(LockPlan::New(l)),
            Err(LockError::InUse) => Err(format!("cache_dir in use: {}", o.cache_dir)),
            Err(LockError::Io(e)) => {
                self.log.warn(&format!(
                    "WorldStreamer: cannot lock cache_dir {}: {e}; running without the folder lock",
                    o.cache_dir
                ));
                Ok(LockPlan::None)
            }
        }
    }

    /// Stop the running configuration: delivered tiles go to the next `drain_unload`, the
    /// worker stops (bounded wait if it uses a disk cache, else in the background: nothing to
    /// flush, and a blocked DNS lookup must not stall the frame). The disk cache is closed
    /// (bounded wait) unless `next` (url, cache_dir, cap of the configuration that follows)
    /// can take it over open; then it is returned. Keeps the folder lock (the caller decides).
    /// Returns false if the bounded wait timed out.
    fn stop_running(&mut self, next: Option<(&str, &str, u64)>) -> (bool, Option<Arc<DiskCache>>) {
        // The host keeps whatever we handed out before: unload it with the next drain_unload.
        if let Some(s) = &mut self.scheduler {
            self.stale_unloads.extend(s.delivered_keys().into_iter().map(LayerKey::world));
            self.stale_unloads.extend(s.delivered_landmark_keys().into_iter().map(LayerKey::landmarks));
            // The decoded data tiles go with the scheduler: out of the memory budget.
            s.release_memory(&mut self.memory);
        }
        if let Some(t) = &self.terrain_sched {
            self.stale_unloads.extend(t.delivered_keys().into_iter().map(LayerKey::terrain));
        }
        self.scheduler = None;
        self.terrain_sched = None;
        self.jobs.clear();
        self.pending_shift = [0.0; 3];
        let Some(w) = self.worker.take() else { return (true, None) };
        if !w.uses_disk_cache() {
            drop(w);
            return (true, None);
        }
        let keep = next.is_some_and(|(url, dir, cap)| w.can_reuse_cache(url, dir, cap));
        let (done, kept) = w.shutdown(STOP_WAIT, keep);
        if !done {
            self.log.warn(&format!(
                "WorldStreamer: disk cache still closing after {:.1} s; the next open may run without a disk cache",
                STOP_WAIT.as_secs_f32()
            ));
        }
        (done, kept)
    }

    /// Stop the workers, wait (at most 1 s) until the disk cache has flushed its pending writes
    /// and closed its file, release the cache folder lock. Afterwards the world is inert (every call is a no-op returning
    /// empty values) until the next `configure` / `configure_world`. The tiles delivered
    /// before are still reported once by the next `drain_unload`. Returns false if the wait
    /// timed out. Dropping a `World` instead stops in the background without waiting.
    pub fn shutdown(&mut self) -> bool {
        let (done, _) = self.stop_running(None);
        self.folder_lock = None;
        self.frame = None;
        self.terrain = Terrain::flat();
        self.seed = None;
        self.seed_cache = None;
        self.memory.clear();
        self.local = LocalSpace::default();
        self.origin_lat = 0.0;
        self.origin_lon = 0.0;
        self.attributions.clear();
        self.health = Health::idle(MSG_SHUT_DOWN);
        done
    }
}

/// What `configure_world` does with the cache folder lock.
enum LockPlan {
    /// Same folder as the held lock: keep the handle (no self-lockout).
    Keep,
    /// A newly taken lock (replaces the held one after the old workers stopped).
    New(FolderLock),
    /// No lock (synthetic city, no `cache_dir`, or locking unavailable).
    None,
}
