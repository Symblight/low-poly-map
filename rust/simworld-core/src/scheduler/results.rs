//! Worker results: fetched, decoded and failed tiles, backoff and parking.

use super::*;

impl Scheduler {

    /// Apply a worker result. Returns a warning to log (main thread) on failure.
    pub fn handle(&mut self, msg: WorkerMsg, now: Instant) -> Option<String> {
        match msg {
            WorkerMsg::Info(_) | WorkerMsg::Warn(_) | WorkerMsg::CacheDisabled(_) | WorkerMsg::Error(_) => None,
            WorkerMsg::DataFetched { key, job, result } => {
                let profile = self.profile.clone();
                self.data.as_mut().and_then(|d| d.fetched(key, job, result, now, &profile))
            }
            WorkerMsg::DataDecoded { key, job, result } => {
                let profile = self.profile.clone();
                self.data.as_mut().and_then(|d| d.decoded(key, job, result, now, &profile))
            }
            WorkerMsg::DataDropped { key, job } => {
                if let Some(d) = self.data.as_mut() {
                    d.dropped(key, job);
                }
                None
            }
            WorkerMsg::Fetched { key, ring, job, result } => {
                self.fetch_inflight[ring] = self.fetch_inflight[ring].saturating_sub(1);
                let e = self.entries.get_mut(&key).filter(|e| e.job == job)?;
                if !matches!(e.state, TileState::Fetching) {
                    return None;
                }
                match result {
                    Ok((bytes, source)) => {
                        e.times.fetched = Some(now);
                        e.times.source = Some(source);
                        e.state = TileState::Fetched(bytes);
                        None
                    }
                    // Only orphaned jobs are cancelled; should a current one be, ask again.
                    Err(err) if err.kind == FetchErrorKind::Cancelled => {
                        e.state = TileState::Queued;
                        None
                    }
                    Err(err) => Some(Self::fail(e, now, "fetch", &err.message, &self.profile)),
                }
            }
            WorkerMsg::LandmarksBuilt { key, job, result } => {
                self.landmark_job_done(key);
                let profile = self.profile.clone();
                let e = self.landmarks.entries.get_mut(&key).filter(|e| e.job == job)?;
                if !matches!(e.state, LmState::Building) {
                    return None;
                }
                match result {
                    Ok(d) if d.building_count == 0 => {
                        e.state = LmState::Empty;
                        None
                    }
                    Ok(d) => {
                        e.failures = 0;
                        e.height = d.max_height as f64;
                        e.state = LmState::Ready(Box::new(d));
                        None
                    }
                    Err(err) => {
                        e.failures += 1;
                        if e.failures >= DECODE_ATTEMPTS {
                            e.state = LmState::Broken;
                            return Some(format!("WorldStreamer: landmark tile l/{key} failed {} times, giving up: {err}", e.failures));
                        }
                        let wait = profile.backoff(e.failures);
                        e.state = LmState::Failed { retry_at: now + wait };
                        Some(format!("WorldStreamer: landmark tile l/{key} failed (attempt {}, retry in {:.1}s): {err}", e.failures, wait.as_secs_f32()))
                    }
                }
            }
            WorkerMsg::Dropped { layer: LayerKind::Landmarks, key, job, .. } => {
                self.landmark_job_done(key);
                let e = self.landmarks.entries.get_mut(&key).filter(|e| e.job == job)?;
                if matches!(e.state, LmState::Building) {
                    e.state = LmState::Wait;
                }
                None
            }
            WorkerMsg::Levels(_) | WorkerMsg::OpenFailed(_) => None,
            WorkerMsg::Dropped { key, ring, job, .. } => {
                // A cancelled mesh job: only orphaned generations are cancelled; should a
                // current one be (shutdown race, or its data tile's decode was cancelled or
                // finished before the job was queued), ask again.
                self.decode_inflight[ring] = self.decode_inflight[ring].saturating_sub(1);
                self.unpin_data(key);
                let e = self.entries.get_mut(&key).filter(|e| e.job == job)?;
                if matches!(e.state, TileState::Decoding) {
                    e.state = TileState::Queued;
                }
                None
            }
            WorkerMsg::Decoded { key, ring, job, result } | WorkerMsg::TerrainBuilt { key, ring, job, result } => {
                self.decode_inflight[ring] = self.decode_inflight[ring].saturating_sub(1);
                self.unpin_data(key);
                if let Ok(d) = &result {
                    self.cpu.record(ring, d.cpu_us);
                }
                let e = self.entries.get_mut(&key).filter(|e| e.job == job)?;
                if !matches!(e.state, TileState::Decoding) {
                    return None;
                }
                match result {
                    Ok(data) => {
                        e.attempts = 0;
                        e.decode_failures = 0;
                        e.height = Some(data.max_height);
                        // Terrain tiles keep their 9 x 9 box (G2): widening it by the delivered
                        // TIN (as until 1.7) made the cover depend on which tiles were built.
                        e.times.ready = Some(now);
                        e.times.cpu_us = data.cpu_us;
                        e.state = TileState::Ready(Box::new(data));
                        self.ready.push(key);
                        None
                    }
                    Err(err) => {
                        e.decode_failures += 1;
                        if e.decode_failures < DECODE_ATTEMPTS {
                            return Some(Self::fail(e, now, "decode", &err, &self.profile));
                        }
                        e.state = TileState::Broken;
                        Some(format!(
                            "WorldStreamer: decode {} failed {} times, giving up (the coarser tile stays): {err}",
                            e.key, e.decode_failures
                        ))
                    }
                }
            }
        }
    }

    /// A chunk job of `key` ended: release its pins on the data tiles (data-cut chunks only).
    fn unpin_data(&mut self, key: TileKey) {
        for dk in self.data_keys(key).into_iter().flatten() {
            if let Some(d) = self.data.as_mut() {
                d.unpin(dk);
            }
        }
    }

    fn fail(e: &mut Entry, now: Instant, stage: &str, err: &str, profile: &Profile) -> String {
        e.attempts += 1;
        let wait = profile.backoff(e.attempts);
        e.state = TileState::Failed { retry_at: now + wait };
        format!(
            "WorldStreamer: {stage} {} failed (attempt {}, retry in {:.1}s): {err}",
            e.key,
            e.attempts,
            wait.as_secs_f32()
        )
    }

    pub(super) fn held(&self, k: TileKey) -> bool {
        self.entries.get(&k).is_some_and(|e| e.held)
    }

    /// All 4 children are uploaded (held directly, not merely present).
    pub(super) fn children_held(&self, k: TileKey) -> bool {
        k.z < self.layer.leaf_zoom && children(k).iter().all(|&c| self.held(c))
    }

}
