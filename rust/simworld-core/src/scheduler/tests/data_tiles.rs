//! Tiles v2: chunks cut from data tiles (T2a) and the two-level source with the landmark layer
//! (T2b).

use super::*;

// ---------------------------------------------------------------------------------------------
// Tiles v2, step T2a: R0 / R1 cut from detail data tiles

/// A harness whose vector scheduler cuts R0 / R1 from z14 data tiles (the compatibility levels);
/// the z14 tile under the origin holds the `data::fixture` area, every other data tile is empty.
/// R2 / R3 stay fake per-tile jobs. `budget`: the data tiles' memory budget in bytes.
fn data_harness(budget: u64) -> Harness {
    let mut h = Harness::new();
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    h.s = Scheduler::new(frame, false, dp()).with_data_levels(crate::data::DataLevels::COMPAT);
    h.w.real = Some(frame);
    let k14 = crate::data::fixture::key14();
    h.w.data_src.insert(k14, crate::data::fixture::tile_bytes(k14).into());
    h.mem = Some(crate::budget::MemoryBudget::new(budget));
    h
}

/// The detail data tiles of every chunk the cover ever wanted.
fn data_tiles_of(h: &Harness) -> HashSet<TileKey> {
    h.ever_requested.iter().filter_map(|k| crate::data::DataLevels::COMPAT.detail_of(*k)).collect()
}

/// One fetch and one decode per data tile, however many R0 / R1 chunks are cut from it; no
/// per-tile fetch of a z15 / z16 key; every requested chunk is delivered (guarantee 2); the
/// chunks under the camera carry the fixture's buildings; the decoded tiles are in the memory
/// budget and pinned only while chunk jobs run.
#[test]
fn data_tiles_are_fetched_and_decoded_once_for_all_their_chunks() {
    let mut h = data_harness(512 << 20);
    let cam = [0.0, 0.0, 300.0];
    let frames = h.settle(cam, 20_000);
    let tiles = data_tiles_of(&h);
    let chunks = h.ever_requested.iter().filter(|k| k.z >= 15).count();
    assert!(tiles.len() >= 2 && chunks >= 4 * tiles.len(), "{chunks} chunks from {} data tiles", tiles.len());
    let n = tiles.len() as u32;
    assert_eq!(h.w.data_jobs, (n, n), "one fetch and one decode per data tile");
    let st = h.s.data_stats();
    assert_eq!((st.decoded, st.fetched, st.loaded, st.fetching), (n as u64, n as u64, n, 0));
    assert!(h.w.fetch_zooms.iter().all(|&z| z <= 14), "a chunk was fetched on its own: {:?}", h.w.fetch_zooms);
    assert_eq!(h.ever_requested, h.ever_delivered);
    let under = crate::geo::tile_for(VILNIUS.0, VILNIUS.1, 16);
    assert!(h.w.chunk_meshes.get(&under).is_some_and(|m| !m.is_empty()), "R0 under the camera has the fixture's buildings");
    let mem = h.mem.as_ref().unwrap().stats();
    let pool = mem.pool(crate::budget::Pool::DataTiles);
    assert_eq!((pool.items, pool.pinned_items, pool.evictions), (n, 0, 0));
    assert!(pool.bytes > 20_000, "{}", pool.bytes);
    eprintln!("settled in {frames} frames: {chunks} chunks cut from {n} data tiles ({} KB)", pool.bytes / 1024);
}

/// Under a budget too small for one data tile, every decoded tile is evicted as soon as no chunk
/// job pins it, and the chunks still waiting fetch and decode it again: the cover completes with
/// the same chunk meshes as without eviction, and nothing pinned stays over the budget.
#[test]
fn evicted_data_tiles_are_fetched_again_and_give_the_same_chunks() {
    let cam = [0.0, 0.0, 300.0];
    let mut a = data_harness(512 << 20);
    a.settle(cam, 20_000);
    let mut b = data_harness(1);
    b.settle(cam, 40_000);
    assert_eq!(b.ever_requested, b.ever_delivered);
    let n = data_tiles_of(&b).len() as u64;
    let st = b.s.data_stats();
    assert!(st.decoded > n && st.fetched == st.decoded, "re-fetched after eviction: {st:?} for {n} tiles");
    assert_eq!(st.loaded, 0, "nothing stays resident over the budget");
    assert_eq!(a.w.chunk_meshes, b.w.chunk_meshes, "same chunks with and without eviction");
    let mem = b.mem.as_ref().unwrap().stats();
    let pool = mem.pool(crate::budget::Pool::DataTiles);
    assert_eq!((pool.items, pool.pinned_items, mem.over_budget_bytes), (0, 0, 0));
    assert!(pool.evictions > n, "{pool:?}");
    eprintln!("budget 1 byte: {} decodes for {n} data tiles, {} evictions", st.decoded, pool.evictions);
}

/// A data fetch nobody needs any more (the camera left before it arrived) is cancelled, and its
/// late reply changes nothing.
#[test]
fn unneeded_data_fetches_are_cancelled() {
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let mut s = Scheduler::new(frame, false, dp()).with_data_levels(crate::data::DataLevels::COMPAT);
    let t0 = Instant::now();
    let mut jobs = Vec::new();
    s.update([0.0, 0.0, 300.0], [0.0; 3], t0, &mut jobs);
    let fetches: Vec<(TileKey, u64)> =
        jobs.iter().filter_map(|j| if let Job::FetchData { key, job } = j { Some((*key, *job)) } else { None }).collect();
    assert!(!fetches.is_empty() && fetches.iter().all(|(k, _)| k.z == 14));
    assert!(jobs.iter().all(|j| !matches!(j, Job::Fetch { key, .. } if key.z >= 15)));
    assert_eq!(s.data_stats().fetching, fetches.len() as u32);
    // Teleport 60 km east: no chunk of the old area is wanted.
    jobs.clear();
    s.update([60_000.0, 0.0, 300.0], [0.0; 3], t0 + Duration::from_millis(30), &mut jobs);
    let mut cancelled = Vec::new();
    s.drain_cancelled(&mut cancelled);
    for (_, job) in &fetches {
        assert!(cancelled.contains(job), "fetch {job} not cancelled");
    }
    // The cancelled replies free the slots; a late success is ignored (the job is stale).
    for (i, (key, job)) in fetches.iter().enumerate() {
        let result = if i == 0 { Ok((Bytes::new(), Source::Network)) } else { Err(FetchError::new(crate::fetch::FetchErrorKind::Cancelled, "x")) };
        assert!(s.handle(WorkerMsg::DataFetched { key: *key, job: *job, result }, t0).is_none());
    }
    assert!(s.data().unwrap().get(fetches[0].0).is_none_or(|e| !matches!(e.state, DataState::Fetched(_))));
}
// ---------------------------------------------------------------------------------------------
// Tiles v2, step T2b: a two-level source (detail z14 + overview z11) and the landmark layer

/// A harness on the two-level fixture (`data::fixture`): the detail z14 tile under the origin
/// and its overview z11 tile hold data, every other data tile is empty. `landmarks` = the host
/// draws the landmark layer.
fn v2_harness(landmarks: bool) -> Harness {
    use crate::data::fixture::{detail_v2_bytes, key11, key14, overview_bytes};
    let mut h = Harness::new();
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    h.s = Scheduler::new(frame, false, dp()).with_data_levels(crate::data::DataLevels::two_level(14, 11, true)).with_landmark_layer(landmarks);
    h.w.real = Some(frame);
    h.w.data_src.insert(key14(), detail_v2_bytes().into());
    h.w.data_src.insert(key11(), overview_bytes().into());
    h.mem = Some(crate::budget::MemoryBudget::new(512 << 20));
    h
}

/// Every band is cut from the two data levels: no per-tile fetch at all, one fetch and one
/// decode per data tile (detail for R0 / R1, overview for R2 / R3 and the landmarks), every
/// requested chunk delivered; R2 / R3 carry the overview's blocks and light points. Landmarks:
/// with the layer, one `l/` tile (the only overview tile with landmarks), delivered once, and no
/// chunk folds them in; without it, no `l/` key and every chunk with data folds them in.
#[test]
fn two_level_source_cuts_every_band_from_two_data_tile_levels() {
    use crate::data::fixture::{key11, key14};
    for landmarks in [true, false] {
        let mut h = v2_harness(landmarks);
        let cam = [0.0, 0.0, 300.0];
        h.settle(cam, 20_000);
        assert!(h.w.fetch_zooms.is_empty(), "a chunk was fetched on its own: {:?}", h.w.fetch_zooms);
        assert_eq!(h.ever_requested, h.ever_delivered);
        let levels = crate::data::DataLevels::two_level(14, 11, true);
        let mut tiles: HashSet<TileKey> = h.ever_requested.iter().filter_map(|k| levels.data_of(*k)).collect();
        if !landmarks {
            tiles.extend(h.ever_requested.iter().filter_map(|k| levels.landmarks_of(*k)));
        }
        if landmarks {
            tiles.extend(h.lm_held.iter().copied());
        }
        let n = tiles.len() as u32;
        assert!(tiles.iter().any(|k| k.z == 11) && tiles.iter().any(|k| k.z == 14), "{tiles:?}");
        assert_eq!(h.w.data_jobs, (n, n), "one fetch and one decode per data tile ({n})");
        let r2 = h.w.chunk_meshes.get(&key14()).expect("the R2 chunk over the fixture");
        assert!(!r2.is_empty(), "R2 from the overview blocks");
        if landmarks {
            assert_eq!(h.lm_held, [key11()].into_iter().collect(), "one landmark tile");
            assert_eq!(h.lm_delivered, 1);
            assert!(h.w.chunk_lm.values().all(|&f| !f), "the landmark layer draws them");
            let st = h.s.landmark_stats();
            assert_eq!((st.loaded, st.pending, st.delivered), (1, 0, 1), "{st:?}");
            assert!(st.empty > 0, "empty overview tiles are built but not delivered");
        } else {
            assert_eq!(h.lm_delivered, 0);
            assert!(h.w.chunk_lm.values().all(|&f| f), "every chunk folds the landmarks in");
            assert_eq!(h.s.landmark_stats(), Default::default());
        }
        eprintln!("landmark layer {landmarks}: {} chunks from {n} data tiles", h.ever_requested.len());
    }
}

/// The landmark tile is held while its box is within `bands.max_distance_m` and unloaded once the
/// camera is past it (with the hysteresis); flying back delivers it again, once. Never in
/// `poll_fades` (checked every frame by the harness).
#[test]
fn landmark_tile_unloads_beyond_range_and_comes_back_once() {
    let mut h = v2_harness(true);
    h.settle([0.0, 0.0, 300.0], 20_000);
    assert_eq!((h.lm_delivered, h.lm_unloaded, h.lm_held.len()), (1, 0, 1));
    // 60 km east: the z11 tile (~11 km wide) is beyond 25 km * 1.15.
    h.settle([60_000.0, 0.0, 300.0], 40_000);
    assert_eq!((h.lm_delivered, h.lm_unloaded, h.lm_held.len()), (1, 1, 0));
    assert_eq!(h.s.delivered_landmark_keys(), Vec::<TileKey>::new());
    h.settle([0.0, 0.0, 300.0], 40_000);
    assert_eq!((h.lm_delivered, h.lm_unloaded, h.lm_held.len()), (2, 1, 1));
}

/// Light-mode switch on overview chunks: the held R2 / R3 tiles are re-delivered with the other
/// light version, R2 re-meshed from the resident overview tile: no data fetch, no decode.
#[test]
fn relight_remeshes_from_the_resident_overview_tile_without_fetching() {
    let mut h = v2_harness(true);
    h.settle([0.0, 0.0, 2000.0], 20_000);
    let jobs = h.w.data_jobs;
    let re = h.redelivered_total;
    // Above `lights.cluster_on_agl_m` (3300 m): clustered lights.
    h.settle([0.0, 0.0, 3600.0], 20_000);
    assert!(h.s.clustered());
    assert!(h.redelivered_total > re, "lit tiles re-delivered");
    let held_overview = h.s.delivered_keys().iter().filter(|k| k.z <= 14).count();
    assert!(held_overview > 0);
    // New overview / detail tiles may enter the cover at the new height; the re-lit ones were
    // all resident already.
    let st = h.s.data_stats();
    assert_eq!(st.fetched, h.w.data_jobs.0 as u64);
    assert_eq!(h.w.data_jobs.0 - jobs.0, h.w.data_jobs.1 - jobs.1, "every fetch decoded once");
    eprintln!("relight: {} re-deliveries, data jobs {:?} -> {:?}", h.redelivered_total - re, jobs, h.w.data_jobs);
    // Pure re-light at the same height band: go back down (unclustered), nothing new to fetch.
    let jobs2 = h.w.data_jobs;
    h.settle([0.0, 0.0, 2000.0], 20_000);
    assert!(!h.s.clustered());
    assert_eq!(h.w.data_jobs.0, jobs2.0, "re-light fetched a data tile");
}

/// Archive source before it opened: nothing is dispatched until the levels are known.
#[test]
fn pending_levels_dispatch_nothing_until_the_archive_opened() {
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let mut s = Scheduler::new(frame, false, dp()).with_data_pending();
    let mut jobs = Vec::new();
    let t0 = Instant::now();
    for i in 0..5 {
        s.update([0.0, 0.0, 300.0], [0.0; 3], t0 + Duration::from_millis(i * 30), &mut jobs);
    }
    assert!(jobs.is_empty() && s.data_pending() && s.entries().count() > 0);
    assert!(s.set_data_levels(crate::data::DataLevels::COMPAT));
    assert!(!s.set_data_levels(crate::data::DataLevels::two_level(14, 11, true)), "set once");
    s.update([0.0, 0.0, 300.0], [0.0; 3], t0 + Duration::from_millis(200), &mut jobs);
    assert!(jobs.iter().any(|j| matches!(j, Job::Fetch { .. })) && jobs.iter().any(|j| matches!(j, Job::FetchData { .. })));
}
