//! Job submission: fetches on the I/O runtime, CPU jobs in the shared pool, one reply per job.

use super::build::{build_seed_tile_stoppable, build_tile_stoppable, panic_text};
use super::*;

impl Worker {

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
                let packed = self.engine_surfaces;
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
                            Ok(Some(d)) => Ok(surfaces::pack(d, packed)),
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
                let packed = self.engine_surfaces;
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
                            Ok(Some(d)) => Ok(surfaces::pack(d, packed)),
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
                let packed = self.engine_surfaces;
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
                            Ok(Some(d)) => Ok(surfaces::pack(d, packed)),
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
                let packed = self.engine_surfaces;
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
                            Ok(Some(d)) => Ok(surfaces::pack(d, packed)),
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
