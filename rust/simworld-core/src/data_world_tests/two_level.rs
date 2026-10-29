//! Tiles v2, step T2b: two-level archives end to end.

use super::*;

/// Run the camera at 300 m AGL over the origin until the vector layer and the landmark layer are
/// idle. Returns (vector deliveries by key, landmark deliveries in order).
fn settle_v2(w: &mut World) -> (BTreeMap<TileKey, u64>, Vec<TileKey>) {
    let mut got = BTreeMap::new();
    let mut lms = Vec::new();
    let mut n = 0;
    let t0 = Instant::now();
    loop {
        w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
        n += w.pop_ready(64, |t| match t.layer {
            LayerKind::Landmarks => lms.push(t.key),
            _ => {
                got.insert(t.key, mesh_hash(t.mesh));
            }
        });
        let _ = w.drain_fades();
        let _ = w.drain_unload();
        let s = w.stats();
        let busy: i32 = s.queued.iter().chain(s.fetching.iter()).chain(s.meshing.iter()).sum();
        if n > 0 && busy == 0 && s.landmarks.pending == 0 && s.data_levels.is_some() {
            return (got, lms);
        }
        assert!(t0.elapsed() < Duration::from_secs(60), "did not settle: {s:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// A served two-level archive: the levels come from its metadata (`stats().data_levels`), every
/// band streams from the two data levels (no request for a z13 / z15 / z16 tile; one fetch and
/// one decode per data tile), R2 over the fixture carries its blocks. With `layers` containing
/// `"landmarks"` the one overview tile with landmarks arrives once as `l/11/x/y` (layer
/// `Landmarks`, `debug_tiles` lists it); without it no `l/` key exists anywhere. An archive whose
/// zoom range does not match its `simworld_levels` is refused (status error).
#[test]
fn two_level_archive_streams_every_band_and_the_landmark_layer() {
    use crate::data::fixture::key11;
    let (url, requests) = serve(archive_v2((11, 14)));
    for landmarks in [true, false] {
        let mut w = world();
        let layers = if landmarks { vec!["world".to_string(), "landmarks".to_string()] } else { vec!["world".to_string()] };
        let r = w.configure_world(WorldOptions { layers: Some(layers), ..opts(&url) });
        assert!(r.ok, "{}", r.error);
        let req0 = requests.load(Ordering::Relaxed);
        let (got, lms) = settle_v2(&mut w);
        let s = w.stats();
        assert_eq!(s.data_levels, Some(crate::data::DataLevels::two_level(14, 11, true)));
        assert_eq!(w.landmark_layer_active(), landmarks);
        assert!(got.keys().any(|k| k.z == 13) && got.keys().any(|k| k.z == 16), "every band delivered");
        assert_ne!(got[&key14()], mesh_hash(&Default::default()), "R2 over the fixture has its blocks");
        assert_eq!(s.data_tiles_fetched, s.data_tiles_decoded, "one decode per fetch");
        let mut tiles = 0;
        let mut l_keys = Vec::new();
        w.debug_tiles(|k, _, _, _| match k.layer {
            LayerKind::Landmarks => l_keys.push(k.to_string()),
            _ => tiles += 1,
        });
        if landmarks {
            assert_eq!(lms, vec![key11()], "one landmark tile, delivered once");
            assert!(l_keys.contains(&format!("l/{}", key11())), "{l_keys:?}");
            assert_eq!(s.landmarks.loaded, 1);
        } else {
            assert!(lms.is_empty() && l_keys.is_empty());
        }
        let req = requests.load(Ordering::Relaxed) - req0;
        eprintln!(
            "two-level archive, landmark layer {landmarks}: {} chunks, {} data tiles fetched, {req} HTTP requests, {tiles} tracked",
            got.len(),
            s.data_tiles_fetched
        );
        // Teleport: the landmark tile goes with the rest.
        let r = w.configure_world(WorldOptions { layers: None, ..opts(&url) });
        assert!(r.ok);
        let un: Vec<String> = w.drain_unload().iter().map(|k| k.to_string()).collect();
        assert_eq!(un.iter().any(|k| k.starts_with("l/")), landmarks);
        w.shutdown();
    }
    // Zooms that do not match `simworld_levels`: refused.
    let (bad, _) = serve(archive_v2((11, 16)));
    let mut w = world();
    assert!(w.configure_world(opts(&bad)).ok);
    let t0 = Instant::now();
    while w.status().state != crate::status::StatusState::Error {
        w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
        assert!(t0.elapsed() < Duration::from_secs(20), "{:?}", w.status());
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(w.status().last_error.contains("simworld_levels"), "{:?}", w.status());
    assert_eq!(w.stats().data_levels, None);
}

/// `layers` accepts `"landmarks"`; unknown names are still refused.
#[test]
fn layers_option_knows_landmarks() {
    let mut w = world();
    let r = w.configure_world(WorldOptions { layers: Some(vec!["world".into(), "landmarks".into()]), ..opts("") });
    assert!(r.ok, "{}", r.error);
    let r = w.configure_world(WorldOptions { layers: Some(vec!["landmark".into()]), ..opts("") });
    assert!(!r.ok && r.error.contains("known: world, terrain, landmarks"), "{}", r.error);
}

/// The synthetic city with the landmark layer: `l/11/x/y` tiles (one per overview tile with
/// landmarks in range), each delivered once; without the layer, none.
#[test]
fn synthetic_city_streams_landmark_tiles_on_request() {
    for landmarks in [true, false] {
        let mut w = world();
        let layers = if landmarks { Some(vec!["world".into(), "landmarks".into()]) } else { None };
        assert!(w.configure_world(WorldOptions { layers, ..opts("") }).ok);
        let (got, lms) = settle_v2(&mut w);
        assert_eq!(w.stats().data_levels, Some(crate::data::DataLevels::SYNTHETIC));
        let distinct: std::collections::BTreeSet<TileKey> = lms.iter().copied().collect();
        assert_eq!(distinct.len(), lms.len(), "a landmark tile delivered twice");
        assert_eq!(!lms.is_empty(), landmarks, "{lms:?}");
        assert!(lms.iter().all(|k| k.z == 11));
        eprintln!("synthetic, landmark layer {landmarks}: {} chunks, {} landmark tiles", got.len(), lms.len());
    }
}
