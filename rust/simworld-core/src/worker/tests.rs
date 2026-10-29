use super::*;

#[test]
fn cancel_aborts_a_hanging_fetch_and_still_replies() {
    // A server that accepts connections and never answers: the fetch hangs (request
    // timeout 20 s by default) until it is cancelled.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/hang.pmtiles", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let _held: Vec<_> = listener.incoming().collect();
    });
    let w = Worker::new(Frame::new(54.6872, 25.2797), &url, "", Arc::new(Profile::default())).unwrap();
    let key = TileKey::new(16, 37370, 20827);
    let fetched = |w: &Worker| {
        while let Some(m) = w.try_recv() {
            if let WorkerMsg::Fetched { job, result, .. } = m {
                return Some((job, result));
            }
        }
        None
    };
    w.submit(Job::Fetch { key, ring: 0, job: 7 }, Priority::on_demand(0.0));
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(fetched(&w).is_none(), "the server never answers");
    w.cancel(LayerKind::World, &[7, 99]); // unknown ids are ignored
    let t0 = std::time::Instant::now();
    let (job, result) = loop {
        if let Some(r) = fetched(&w) {
            break r;
        }
        assert!(t0.elapsed() < std::time::Duration::from_secs(5), "no reply after cancel");
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert_eq!(job, 7);
    assert_eq!(result.unwrap_err().kind, FetchErrorKind::Cancelled);
    assert!(lock(&w.fetches).is_empty(), "the task removed its abort handle");
}

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
    // Building lights from R1 (default lights.min_ring), street lamps on every band.
    assert!(d.lights.positions.is_empty() && !d.lights.street_positions.is_empty() && !d.mesh.is_empty());
    let k14 = crate::geo::tile_for(54.6872, 25.2797, 14);
    let d2 = build_tile(k14, 2, frame, &Payload::Synthetic, &p).unwrap();
    assert!(!d2.lights.is_empty() && !d2.mesh.is_empty());
    assert!(d2.lights.clustered_positions.len() < d2.lights.positions.len());
    let k13 = crate::geo::tile_for(54.6872, 25.2797, 13);
    let d3 = build_tile(k13, 3, frame, &Payload::Synthetic, &p).unwrap();
    assert!(!d3.lights.positions.is_empty() && !d3.lights.street_positions.is_empty() && d3.mesh.is_empty());
    // Ground on every band (synthetic city has streets and districts everywhere); R3 is the coarsest.
    assert!(!d.ground.is_empty() && !d2.ground.is_empty() && !d3.ground.is_empty());
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
        w.lights.street.min_ring = 4; // building lights only
        w.lights.beacons.min_ring = 4;
        let p = Profile::new(w).unwrap();
        build_tile(key, ring, frame, &Payload::Synthetic, &p).unwrap().lights
    };
    let reference = lit(2, 2);
    assert!(!reference.is_empty());
    assert!(lit(2, 1).is_empty() && lit(2, 0).is_empty(), "min_ring 2: R0/R1 unlit");
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
