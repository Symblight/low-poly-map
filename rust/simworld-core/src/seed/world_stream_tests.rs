//! Seed world S1 through `World`: first tiles, configure errors, reconfigure reuse.

use super::*;

/// Stream the World over EYVI until every band delivered: first tile within 5 s, R3 ground,
/// airport lights in every band, terrain COLOR.
#[test]
fn first_tiles_arrive_with_r3_ground_and_airport_lights() {
    let (lat, lon) = {
        let (_, _, w) = test_world();
        let g = w.airport("EYVI").unwrap();
        g.frame.ll(g.runways[0].at(g.runways[0].length_m * 0.5, 0.0))
    };
    let mut world = new_world();
    let res = world.configure_world(seed_options(lat, lon));
    assert!(res.ok, "{}", res.error);
    assert!(world.status().terrain_active);
    let t0 = Instant::now();
    let mut first = None;
    let mut r3_ground = false;
    let mut lit = [false; 4];
    let mut terrain_color = false;
    while t0.elapsed() < Duration::from_secs(30) && !(r3_ground && lit.iter().all(|x| *x) && terrain_color) {
        world.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
        world.pop_ready(64, |t| {
            first.get_or_insert(t0.elapsed());
            if t.layer == crate::rings::LayerKind::World {
                if t.ring == 3 && !t.ground.is_empty() {
                    r3_ground = true;
                }
                if !t.airport_lights.is_empty() {
                    assert_eq!(t.airport_lights.len(), t.airport_light_colors.len());
                    lit[t.ring] = true;
                }
            } else if !t.terrain.colors.is_empty() {
                assert_eq!(t.terrain.colors.len(), t.terrain.positions.len());
                terrain_color = true;
            }
        });
        let _ = world.drain_fades();
        let _ = world.drain_unload();
        std::thread::sleep(Duration::from_millis(2));
    }
    let first = first.expect("no tile at all");
    eprintln!("EYVI: first tile after {:.0} ms, all checks after {:.0} ms", first.as_secs_f64() * 1000.0, t0.elapsed().as_secs_f64() * 1000.0);
    assert!(first < Duration::from_secs(5), "first tile within 5 s: {first:?}");
    assert!(r3_ground, "R3 ground present");
    assert_eq!(lit, [true; 4], "airport lights in every band over the runway");
    assert!(terrain_color, "terrain tiles carry COLOR in a seed world");
    assert_eq!(world.status().state, crate::StatusState::Ok);
    assert_eq!(world.tile_source(crate::rings::LayerKind::World), "seed");
}

#[test]
fn configure_errors_name_the_problem_and_change_nothing() {
    let mut world = new_world();
    assert!(world.configure_world(seed_options(VILNIUS.0, VILNIUS.1)).ok);
    let kept = world.seed_world().cloned().unwrap();
    let v = VILNIUS;
    let cases: Vec<(WorldOptions, &str)> = vec![
        (WorldOptions { tile_url: "http://127.0.0.1:9/x.pmtiles".into(), ..seed_options(v.0, v.1) }, "seed_world: tile_url must be empty"),
        (WorldOptions { layers: None, ..seed_options(v.0, v.1) }, "seed_world: needs the terrain layer"),
        (
            WorldOptions {
                profile_overrides: Some(serde_json::json!({ "terrain": { "enabled": false, "flat_radius_m": 0.0, "flat_blend_m": 0.0 } })),
                ..seed_options(v.0, v.1)
            },
            "seed_world: needs terrain.enabled = true",
        ),
        (
            WorldOptions { profile_overrides: Some(serde_json::json!({ "terrain": { "enabled": true } })), ..seed_options(v.0, v.1) },
            "seed_world: terrain.flat_radius_m = 1500 must be 0",
        ),
        (
            WorldOptions { seed_world: Some("/nonexistent/world.json".into()), ..seed_options(v.0, v.1) },
            "seed_world /nonexistent/world.json: cannot read the manifest",
        ),
    ];
    for (o, want) in cases {
        let r = world.configure_world(o);
        assert!(!r.ok && r.error.contains(want), "want `{want}`, got `{}`", r.error);
        assert!(Arc::ptr_eq(world.seed_world().unwrap(), &kept), "a failed configure keeps the running world");
    }
    // A broken file names the file, the key and the value.
    let dir = std::env::temp_dir().join(format!("simworld-seed-bad-{}", std::process::id()));
    let src = std::path::Path::new(TEST_WORLD).parent().unwrap();
    std::fs::create_dir_all(dir.join("cities")).unwrap();
    for f in ["world.json", "airports.json", "water.json", "roads.json", "cities/vilnius.json"] {
        std::fs::copy(src.join(f), dir.join(f)).unwrap();
    }
    let k = std::fs::read_to_string(src.join("cities/kaunas.json")).unwrap().replace("\"radius_km\": 6.0", "\"radius_km\": 45.0");
    std::fs::write(dir.join("cities/kaunas.json"), k).unwrap();
    let o = WorldOptions { seed_world: Some(dir.join("world.json").to_string_lossy().into_owned()), ..seed_options(v.0, v.1) };
    let r = world.configure_world(o);
    assert!(!r.ok && r.error.contains("cities/kaunas.json: radius_km: 45.0 is above the maximum 30"), "{}", r.error);
    let _ = std::fs::remove_dir_all(&dir);
    // A configure without a seed world drops it (and the API answers empty).
    assert!(world.configure_world(WorldOptions { origin_lat: v.0, origin_lon: v.1, ..WorldOptions::default() }).ok);
    assert!(world.seed_world().is_none() && world.airports().is_empty() && world.seed_world_info().is_none());
    assert_eq!(world.tile_source(crate::rings::LayerKind::World), "synthetic");
}

#[test]
fn a_reconfigure_with_the_same_files_reuses_the_loaded_world() {
    let mut world = new_world();
    assert!(world.configure_world(seed_options(VILNIUS.0, VILNIUS.1)).ok);
    let a = world.seed_world().cloned().unwrap();
    // A teleport (new origin) and a vector-band change keep the world; a seed key rebuilds it.
    let mut o = seed_options(54.9, 23.9);
    o.profile_overrides =
        Some(serde_json::json!({ "terrain": { "enabled": true, "flat_radius_m": 0.0, "flat_blend_m": 0.0 }, "lod": { "error_m": { "r3": 25.0 } } }));
    assert!(world.configure_world(o).ok);
    assert!(Arc::ptr_eq(world.seed_world().unwrap(), &a));
    let mut o = seed_options(54.9, 23.9);
    o.profile_overrides =
        Some(serde_json::json!({ "terrain": { "enabled": true, "flat_radius_m": 0.0, "flat_blend_m": 0.0 }, "seed": { "water": { "blend_m": 300.0 } } }));
    assert!(world.configure_world(o).ok);
    assert!(!Arc::ptr_eq(world.seed_world().unwrap(), &a));
}
