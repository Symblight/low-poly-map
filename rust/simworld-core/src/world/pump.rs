//! The worker pump: apply results, order, cancel and submit jobs.

use super::*;

impl World {

    /// Apply worker results (cheap: moves boxes around). Bounded per call.
    pub(super) fn drain_worker(&mut self) {
        let (Some(worker), Some(sched)) = (&self.worker, &mut self.scheduler) else { return };
        let mut tsched = self.terrain_sched.as_mut();
        let now = Instant::now();
        let mut warns: Vec<String> = Vec::new();
        for _ in 0..256 {
            let Some(msg) = worker.try_recv() else { break };
            match msg {
                WorkerMsg::Info(s) => self.log.info(&format!("WorldStreamer: {s}")),
                WorkerMsg::Warn(s) => warns.push(format!("WorldStreamer: {s}")),
                WorkerMsg::CacheDisabled(s) => {
                    warns.push(format!("WorldStreamer: {s}"));
                    self.health.set_cache_disabled(&s);
                }
                WorkerMsg::Error(s) => {
                    self.log.error(&format!("WorldStreamer: {s}"));
                    self.health.set_fatal(&s);
                }
                WorkerMsg::Levels(l) => {
                    if sched.set_data_levels(l) {
                        let what = if l.two_level {
                            format!(
                                "two-level archive (detail z{}, overview z{}{})",
                                l.detail,
                                l.overview,
                                match (l.landmarks, sched.landmark_layer_active()) {
                                    (false, _) => "",
                                    (true, true) => ", landmark layer",
                                    (true, false) => ", landmarks folded into the chunks",
                                }
                            )
                        } else {
                            "archive without simworld_levels (zooms 13..16, compatibility levels)".to_string()
                        };
                        self.log.info(&format!("WorldStreamer: data levels: {what}"));
                    }
                }
                WorkerMsg::OpenFailed(e) => self.health.record_fetch(&Err(e), now),
                m @ (WorkerMsg::TerrainBuilt { .. } | WorkerMsg::Dropped { layer: LayerKind::Terrain, .. }) => {
                    if let WorkerMsg::TerrainBuilt { result: Err(e), .. } = &m {
                        self.health.record_decode_failure(e, now);
                    }
                    if let Some(t) = tsched.as_deref_mut() {
                        warns.extend(t.handle(m, now));
                    }
                }
                m => {
                    match &m {
                        WorkerMsg::Fetched { result, .. } | WorkerMsg::DataFetched { result, .. } => self.health.record_fetch(result, now),
                        WorkerMsg::Decoded { result: Err(e), .. } | WorkerMsg::DataDecoded { result: Err(e), .. } => {
                            self.health.record_decode_failure(e, now)
                        }
                        _ => {}
                    }
                    warns.extend(sched.handle(m, now));
                }
            }
        }
        sched.sync_memory(&mut self.memory);
        for w in warns {
            self.warn(&w);
        }
    }

    /// Both layers' new jobs in one order for the shared worker pool (FIFO): nearest to the
    /// camera first, so the terrain's jobs do not queue behind the vector world's backlog.
    /// Each layer already lists its jobs that way; this merges the two lists (stable).
    pub(super) fn order_jobs(&mut self) {
        let (Some(w), Some(t)) = (&self.scheduler, &self.terrain_sched) else { return };
        if self.jobs.len() < 2 {
            return;
        }
        self.jobs.sort_by(|a, b| {
            let d = |j: &Job| match *j {
                Job::Terrain { key, .. } => t.camera_distance(key),
                Job::FetchData { key, .. } | Job::DecodeData { key, .. } => w.data_distance(key),
                Job::MeshLandmarks { key, .. } => w.landmark_distance(key),
                Job::Fetch { key, .. } | Job::Decode { key, .. } | Job::MeshChunk { key, .. } => w.camera_distance(key),
            };
            d(a).total_cmp(&d(b))
        });
    }

    /// Cancel the jobs (fetches and mesh jobs) both layers' schedulers orphaned.
    pub(super) fn cancel_orphans(&mut self) {
        let Some(w) = &self.worker else { return };
        let mut ids = std::mem::take(&mut self.cancel_buf);
        if let Some(s) = &mut self.scheduler {
            w.release(s.take_released_data()); // decoded data tiles are freed off the main thread
            w.release(s.take_released_tiles());
            s.drain_cancelled(&mut ids);
            w.cancel(LayerKind::World, &ids);
            ids.clear();
        }
        if let Some(t) = &mut self.terrain_sched {
            w.release(t.take_released_tiles());
            t.drain_cancelled(&mut ids);
            w.cancel(LayerKind::Terrain, &ids);
            ids.clear();
        }
        self.cancel_buf = ids;
    }

    /// Hand both layers' cold tile-box probes to the CPU pool (`scheduler::boxes`).
    pub(super) fn submit_probes(&mut self) {
        let Some(w) = &self.worker else { return };
        for s in [self.scheduler.as_mut(), self.terrain_sched.as_mut()].into_iter().flatten() {
            if let Some(b) = s.take_probe_batch() {
                w.probe(b);
            }
        }
    }

    /// Hand the new jobs to the worker; CPU jobs queue by [`Scheduler::job_priority`] (fresh
    /// before re-deliveries, then nearest group first, both layers in one heap).
    pub(super) fn submit_jobs(&mut self) {
        if let Some(w) = &self.worker {
            for j in self.jobs.drain(..) {
                let prio = match &j {
                    Job::Terrain { key, .. } => self.terrain_sched.as_ref().map(|t| t.job_priority(*key)),
                    Job::FetchData { key, .. } | Job::DecodeData { key, .. } => self.scheduler.as_ref().map(|s| s.data_job_priority(*key)),
                    Job::MeshLandmarks { key, .. } => self.scheduler.as_ref().map(|s| Priority::on_demand(s.landmark_distance(*key))),
                    Job::Fetch { key, .. } | Job::Decode { key, .. } | Job::MeshChunk { key, .. } => {
                        self.scheduler.as_ref().map(|s| s.job_priority(*key))
                    }
                };
                w.submit(j, prio.unwrap_or(Priority::prefetch(f64::INFINITY)));
            }
        } else {
            self.jobs.clear();
        }
    }

}
