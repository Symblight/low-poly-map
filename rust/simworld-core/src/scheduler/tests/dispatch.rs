//! Jobs: the synthetic city through the scheduler, failures and backoff, cancelled and stale
//! work, delivery order under parents, subtree unloads and the ground draw order.

use super::*;

#[test]
fn synthetic_city_streams_real_meshes_and_lights() {
    let lights = synthetic_city_settles(dp());
    assert!(lights[0] == 0 && lights[1] > 0 && lights[2] > 0 && lights[3] > 0, "{lights:?}");
}

/// `lights.min_ring` = 2 (the old default, profile_version 3): R2/R3 only.
#[test]
fn lights_min_ring_2_lights_r2_r3_only() {
    let mut w = crate::profile::WorldProfile::default();
    w.lights.min_ring = 2;
    let lights = synthetic_city_settles(Arc::new(Profile::new(w).unwrap()));
    assert!(lights[0] == 0 && lights[1] == 0 && lights[2] > 0 && lights[3] > 0, "{lights:?}");
}

/// `lights.min_ring` = 0: R0 tiles carry lights through the whole pipeline too, and a settled cover has no
/// cover errors (the lights do not touch the visibility rule).
#[test]
fn lights_min_ring_0_lights_r0_through_the_scheduler() {
    let mut w = crate::profile::WorldProfile::default();
    w.lights.min_ring = 0;
    let lights = synthetic_city_settles(Arc::new(Profile::new(w).unwrap()));
    assert!(lights[0] > 0 && lights[1] > 0 && lights[2] > 0 && lights[3] > 0, "{lights:?}");
}

/// Streams the synthetic city around 300 m AGL until settled; returns the light counts per ring.
fn synthetic_city_settles(profile: Arc<Profile>) -> [usize; RING_COUNT] {
    let mut h = Harness::new();
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    h.s = Scheduler::new(frame, true, profile.clone());
    h.w.real = Some(frame);
    h.w.profile = Some(profile);
    let mut tris = [0usize; RING_COUNT];
    let mut ground = [0usize; RING_COUNT];
    let mut lights = [0usize; RING_COUNT];
    for i in 0..20_000 {
        h.now += Duration::from_secs_f64(DT);
        h.w.step(&mut h.s, h.now);
        h.s.update([0.0, 0.0, 300.0], [0.0; 3], h.now, &mut h.jobs);
        h.w.submit(&mut h.jobs);
        while let Some(t) = h.s.pop_ready(true) {
            tris[t.ring] += t.mesh.triangle_count();
            ground[t.ring] += t.ground.triangle_count();
            lights[t.ring] += t.light_arrays().0.len();
            h.g.upload(t.key);
        }
        h.fades.clear();
        h.s.drain_fades(&mut h.fades);
        h.g.set_fades(&h.fades);
        let mut un = Vec::new();
        h.s.drain_unload(&mut un);
        for k in un {
            h.g.unload(k);
        }
        assert!(h.g.check().is_empty());
        if h.s.settled() {
            eprintln!("synthetic settled after {i} frames: tris {tris:?} ground tris {ground:?} lights {lights:?}");
            break;
        }
    }
    assert!(h.s.settled());
    assert!(tris[0] > 0 && tris[1] > 0 && tris[2] > 0 && tris[3] == 0);
    // Every band has ground since api 1.3 (R3: the coarse `ground.r3`).
    assert!(ground.iter().all(|&g| g > 0));
    lights
}

#[test]
fn failures_back_off_and_retry_and_block_only_their_group() {
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let mut s = Scheduler::new(frame, false, dp());
    let t0 = Instant::now();
    let mut jobs = Vec::new();
    s.update([0.0, 0.0, 8000.0], [0.0; 3], t0, &mut jobs);
    // Only R3 roots at 8 km: 4 fetches in flight.
    assert_eq!(jobs.len(), 4);
    for j in jobs.drain(..) {
        let Job::Fetch { key, ring, job } = j else { panic!() };
        let w = s.handle(WorkerMsg::Fetched { key, ring, job, result: Err("boom".into()) }, t0);
        assert!(w.is_some());
    }
    assert_eq!(s.stats().failed[3], 4);
    s.update([0.0, 0.0, 8000.0], [0.0; 3], t0, &mut jobs);
    assert_eq!(jobs.len(), 4, "next tiles go out while the failed ones wait");
    jobs.clear();
    s.update([0.0, 0.0, 8000.0], [0.0; 3], t0 + dp().backoff(1), &mut jobs);
    assert_eq!(s.stats().failed[3], 0);
    assert_eq!(dp().backoff(1), Duration::from_millis(500));
    assert_eq!(dp().backoff(3), Duration::from_millis(2000));
    assert_eq!(dp().backoff(20), Duration::from_secs(30));
}

/// Step J1: mesh jobs are cancelled like fetches. A removed tile's running mesh job is reported
/// by `drain_cancelled`; its `Dropped` reply frees the slot without a failure; a late result of
/// the stale generation is ignored. Re-deliveries queue as prefetch, fresh tiles on demand.
#[test]
fn orphaned_mesh_jobs_are_cancelled_and_free_their_slots() {
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let mut s = Scheduler::new(frame, true, dp());
    let t0 = Instant::now();
    let mut jobs = Vec::new();
    s.update([0.0, 0.0, 8000.0], [0.0; 3], t0, &mut jobs);
    let decodes: Vec<(TileKey, usize, u64)> = jobs
        .drain(..)
        .map(|j| match j {
            Job::Decode { key, ring, job, .. } => (key, ring, job),
            _ => panic!("decode expected (synthetic)"),
        })
        .collect();
    assert!(!decodes.is_empty());
    for d in &decodes {
        let p = s.job_priority(d.0);
        assert_eq!(p.urgency, crate::jobs::Urgency::OnDemand, "fresh tiles are on demand");
    }
    let mut ids = Vec::new();
    s.drain_cancelled(&mut ids);
    assert!(ids.is_empty(), "nothing orphaned while the cover stands");
    let far = [200_000.0, 0.0, 8000.0];
    s.update(far, [0.0; 3], t0, &mut jobs);
    s.drain_cancelled(&mut ids);
    let mut want: Vec<u64> = decodes.iter().map(|d| d.2).collect();
    want.sort_unstable();
    ids.sort_unstable();
    assert_eq!(ids, want, "every running mesh job of a removed tile is reported");
    let (k0, r0, j0) = decodes[0];
    for &(key, ring, job) in &decodes[1..] {
        assert!(s.handle(WorkerMsg::Dropped { layer: LayerKind::World, key, ring, job }, t0).is_none());
    }
    // A result that slipped through before the cancel took effect is dropped by its id.
    assert!(s.handle(WorkerMsg::Decoded { key: k0, ring: r0, job: j0, result: Ok(fake_data(k0, r0)) }, t0).is_none());
    assert_eq!(s.inflight().1, [0; MAX_RINGS], "every slot is free again");
    assert!(s.pop_ready(true).is_none(), "nothing stale is ready");
    assert_eq!(s.stats().failed.iter().sum::<i32>(), 0);
}

#[test]
fn orphaned_fetches_are_cancelled_and_free_their_slots() {
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let mut s = Scheduler::new(frame, false, dp());
    let t0 = Instant::now();
    let mut jobs = Vec::new();
    s.update([0.0, 0.0, 8000.0], [0.0; 3], t0, &mut jobs);
    let fetches: Vec<(TileKey, usize, u64)> = jobs
        .drain(..)
        .map(|j| match j {
            Job::Fetch { key, ring, job } => (key, ring, job),
            _ => panic!("fetch expected"),
        })
        .collect();
    assert_eq!(fetches.len(), 4, "R3 roots at 8 km fill the band's fetch slots");
    let mut ids = Vec::new();
    s.drain_cancelled(&mut ids);
    assert!(ids.is_empty(), "nothing orphaned while the cover stands");
    // Teleport 200 km east: every root leaves the disk while its fetch runs.
    let far = [200_000.0, 0.0, 8000.0];
    s.update(far, [0.0; 3], t0, &mut jobs);
    s.drain_cancelled(&mut ids);
    let mut want: Vec<u64> = fetches.iter().map(|f| f.2).collect();
    want.sort_unstable();
    ids.sort_unstable();
    assert_eq!(ids, want, "every running fetch of a removed tile is reported");
    assert!(jobs.is_empty(), "the slots stay taken until the replies arrive");
    // The cancelled replies free the slots without a failure or a warning.
    for (key, ring, job) in fetches {
        let result = Err(FetchError::new(FetchErrorKind::Cancelled, "cancelled"));
        assert!(s.handle(WorkerMsg::Fetched { key, ring, job, result }, t0).is_none());
    }
    assert_eq!(s.inflight().0[3], 0);
    s.update(far, [0.0; 3], t0, &mut jobs);
    assert_eq!(jobs.len(), 4, "the new area gets the freed slots");
    assert_eq!(s.stats().failed[3], 0);
    ids.clear();
    s.drain_cancelled(&mut ids);
    assert!(ids.is_empty(), "nothing new orphaned");
}

#[test]
fn repeated_decode_failures_park_the_tile() {
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let mut s = Scheduler::new(frame, true, dp());
    let cam = [0.0, 0.0, 8000.0];
    let mut t = Instant::now();
    let mut jobs = Vec::new();
    s.update(cam, [0.0; 3], t, &mut jobs);
    // The nearest root goes first, so it also wins the free slot on every retry.
    let Job::Decode { key, ring, mut job, .. } = jobs.remove(0) else { panic!("decode expected") };
    jobs.clear();
    let decode_of = |jobs: &[Job]| {
        jobs.iter().find_map(|j| match j {
            Job::Decode { key: k, job, .. } if *k == key => Some(*job),
            _ => None,
        })
    };
    for attempt in 1..=DECODE_ATTEMPTS {
        let w = s.handle(WorkerMsg::Decoded { key, ring, job, result: Err("bad tile".into()) }, t).unwrap();
        if attempt < DECODE_ATTEMPTS {
            assert!(w.contains("retry in"), "{w}");
            t += Duration::from_secs(60);
            s.update(cam, [0.0; 3], t, &mut jobs);
            job = decode_of(&jobs).expect("retried after the backoff");
            jobs.clear();
        } else {
            assert!(w.contains("giving up"), "{w}");
        }
    }
    // Parked: counted as failed, never dispatched again while it stays in the cover.
    for _ in 0..3 {
        t += Duration::from_secs(60);
        s.update(cam, [0.0; 3], t, &mut jobs);
        assert!(decode_of(&jobs).is_none(), "a broken tile gets no more jobs");
        jobs.clear();
    }
    assert_eq!(s.stats().failed[ring], 1);
    assert!(matches!(s.entries.get(&key).map(|e| &e.state), Some(TileState::Broken)));
}

#[test]
fn stale_results_are_ignored() {
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let mut s = Scheduler::new(frame, false, dp());
    let now = Instant::now();
    let mut jobs = Vec::new();
    s.update([0.0, 0.0, 300.0], [0.0; 3], now, &mut jobs);
    let Job::Fetch { key, ring, job } = jobs[0] else { panic!() };
    // Fly far away: the entry is dropped while its fetch is in flight.
    let mut more = Vec::new();
    s.update([200_000.0, 0.0, 300.0], [0.0; 3], now, &mut more);
    assert!(s.handle(WorkerMsg::Fetched { key, ring, job, result: Ok((Bytes::new(), Source::Network)) }, now).is_none());
    assert!(s.pop_ready(true).is_none());
    assert_eq!(s.inflight().0[ring], dp().streaming.max_fetch_per_band - 1);
}

#[test]
fn dropped_parent_forgets_pending_work() {
    for pending in [TileState::Queued, TileState::Failed { retry_at: Instant::now() }, TileState::Fetched(Bytes::new())] {
        let mut s = Scheduler::new(Frame::new(VILNIUS.0, VILNIUS.1), false, dp());
        let k = TileKey::new(14, 9000, 5000);
        let e = s.touch(k, 2);
        e.held = true;
        e.split = true;
        e.state = pending;
        let job = e.job;
        for c in children(k) {
            s.touch(c, 3).held = true;
        }
        s.try_drop(k);
        let e = &s.entries[&k];
        assert!(!e.held);
        assert!(matches!(e.state, TileState::Idle), "pending work survived the drop: {:?}", e.state);
        assert!(e.job > job, "in-flight results must be orphaned");
        let mut jobs = Vec::new();
        s.dispatch(&mut jobs);
        assert!(jobs.iter().all(|j| !matches!(j, Job::Fetch { key, .. } | Job::Decode { key, .. } if *key == k)));
    }
}

#[test]
fn children_wait_for_an_effective_parent() {
    let mut h = Harness::new();
    // Let everything decode before delivering anything: deliveries must still come
    // root-first per branch, and never under a parent that is part of a fade.
    let cam = [0.0, 0.0, 300.0];
    for _ in 0..3000 {
        h.w.step(&mut h.s, h.now);
        h.s.update(cam, [0.0; 3], h.now, &mut h.jobs);
        h.w.submit(&mut h.jobs);
    }
    let mut order = Vec::new();
    for _ in 0..20_000 {
        // Drain everything deliverable, then one update (fades advance only there).
        loop {
            let fading: HashSet<TileKey> = h.s.fades().map(|(k, _)| k).collect();
            let in_fade = |k: TileKey| fading.contains(&k) || parent(k).is_some_and(|p| fading.contains(&p));
            let Some(t) = h.s.pop_ready(true) else { break };
            if let Some(p) = parent(t.key) {
                assert!(h.g.present(p), "{} before its parent", t.key);
                assert!(!in_fade(p), "{} delivered under fading {p}", t.key);
                for s in siblings(p) {
                    if p.z > ROOT_ZOOM {
                        assert!(h.g.present(s), "{} before parent's sibling {s}", t.key);
                    }
                }
            }
            h.g.upload(t.key);
            order.push(t.key);
        }
        h.fades.clear();
        h.s.drain_fades(&mut h.fades);
        h.g.set_fades(&h.fades);
        let mut un = Vec::new();
        h.s.drain_unload(&mut un);
        for k in un {
            h.g.unload(k);
        }
        assert!(h.g.check().is_empty(), "{:?}", h.g.check());
        if h.s.settled() {
            break;
        }
        h.now += Duration::from_secs_f64(DT);
        h.w.step(&mut h.s, h.now);
        h.s.update(cam, [0.0; 3], h.now, &mut h.jobs);
        h.w.submit(&mut h.jobs);
    }
    assert!(!order.is_empty());
    assert!(h.s.settled());
}

#[test]
fn far_flight_unloads_everything_in_subtree_batches() {
    let mut h = Harness::new();
    h.settle([0.0, 0.0, 300.0], 20_000);
    let n = h.g.uploaded.len();
    let (_, un) = h.frame([200_000.0, 0.0, 300.0], [0.0; 3]);
    h.check_frame(&un, "teleport");
    assert_eq!(un.len(), n);
    assert!(h.g.uploaded.is_empty());
}
#[test]
fn ground_draw_order_is_bottom_level_first() {
    // Godot's Mobile renderer uses GREATER_OR_EQUAL (reversed Z): on a depth tie the later
    // triangle wins, so the ground mesh must be emitted bottom stacking level first.
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let k = crate::geo::tile_for(VILNIUS.0, VILNIUS.1, 14);
    let data = build_tile(k, 2, frame, &Payload::Synthetic, &Profile::default()).unwrap();
    let m = &data.ground;
    assert!(m.triangle_count() > 0);
    let mut last = f32::NEG_INFINITY;
    for tri in m.indices.chunks_exact(3) {
        let y = m.positions[tri[0] as usize][1];
        assert!(y >= last - 1e-4, "ground level {y} after {last}");
        last = last.max(y);
    }
}
