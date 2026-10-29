//! The terrain layer (api 1.5) through `World`.

use super::*;

/// A small terrain layer for World-level tests (zooms 13..14, 7 km reach) with real TINs.
fn small_terrain(enabled: bool) -> serde_json::Value {
    serde_json::json!({ "terrain": { "enabled": enabled, "near_zoom": 14, "far_zoom": 13, "bands": [
        { "edge_m": 2500.0, "max_error_m": 0.5, "max_edge_m": 0.0, "max_triangles": 4096 },
        { "edge_m": 7000.0, "max_error_m": 1.5, "max_edge_m": 0.0, "max_triangles": 4096 } ] } })
}

fn terrain_opts(lat: f64, lon: f64, layers: Option<&[&str]>, enabled: bool) -> WorldOptions {
    WorldOptions {
        profile_overrides: Some(small_terrain(enabled)),
        layers: layers.map(|l| l.iter().map(|s| s.to_string()).collect()),
        ..synthetic(lat, lon)
    }
}

/// Host side of one frame in contract order at a static camera (300 m AGL over the origin).
#[derive(Default)]
struct Host {
    held: BTreeSet<LayerKey>,
    /// Every delivery: (key, already held = re-delivery).
    deliveries: Vec<(LayerKey, bool)>,
    unloads: Vec<LayerKey>,
    fades: Vec<LayerKey>,
}

impl Host {
    fn frame(&mut self, w: &mut World) {
        w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
        let (held, deliveries) = (&mut self.held, &mut self.deliveries);
        w.pop_ready(4, |t| {
            let k = t.layer_key();
            assert_eq!(LayerKey::parse(&k.to_string()), Some(k));
            match t.layer {
                crate::rings::LayerKind::Terrain => {
                    assert!(k.to_string().starts_with("t/"), "{k}");
                    assert!(t.ring < 2 && t.key.z == 14 - t.ring as u8, "{k}: ring {}", t.ring);
                    assert!(!t.terrain.is_empty() && t.terrain.normals.is_empty());
                    assert!(t.mesh.is_empty() && t.ground.is_empty() && t.lights.is_empty() && t.street_lights.is_empty());
                    assert!(t.h_min <= t.h_max && t.origin[1] == 0.0);
                }
                crate::rings::LayerKind::World => {
                    assert!(t.terrain.is_empty() && !k.to_string().starts_with("t/"));
                }
                crate::rings::LayerKind::Landmarks => panic!("{k}: no landmark layer requested"),
            }
            deliveries.push((k, !held.insert(k)));
        });
        self.fades.extend(w.drain_fades().iter().map(|f| f.0));
        for k in w.drain_unload() {
            self.held.remove(k);
            self.unloads.push(*k);
        }
    }

    /// `t/` keys seen in deliveries, unloads and fades.
    fn terrain_keys(&self) -> usize {
        let t = |k: &LayerKey| k.layer == crate::rings::LayerKind::Terrain;
        self.deliveries.iter().filter(|(k, _)| t(k)).count()
            + self.unloads.iter().filter(|k| t(k)).count()
            + self.fades.iter().filter(|k| t(k)).count()
    }

    /// Run until the terrain layer is idle (nothing queued or meshing, no terrain fade and no
    /// terrain delivery for 40 frames) or 30 s.
    fn settle_terrain(&mut self, w: &mut World) {
        let t0 = Instant::now();
        let mut quiet = 0;
        while quiet < 40 && t0.elapsed() < Duration::from_secs(30) {
            let (nd, nf) = (self.deliveries.len(), self.fades.len());
            self.frame(w);
            let st = w.stats();
            let busy = st.terrain_queued.iter().chain(&st.terrain_meshing).any(|&n| n > 0)
                || self.deliveries[nd..].iter().any(|(k, _)| k.layer == crate::rings::LayerKind::Terrain)
                || self.fades[nf..].iter().any(|k| k.layer == crate::rings::LayerKind::Terrain);
            quiet = if busy { 0 } else { quiet + 1 };
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(quiet >= 40, "terrain layer did not settle");
    }
}

/// Opt-in: without the host layer (or with the profile off) no `t/` key appears anywhere; with
/// both, terrain tiles stream with `t/` keys next to the vector world, and a teleport unloads
/// both layers.
#[test]
fn terrain_layer_needs_the_opt_in_and_teleport_unloads_both_layers() {
    for (layers, enabled) in [(None, true), (Some(&["world", "terrain"][..]), false), (Some(&["world"][..]), true)] {
        let mut w = world();
        assert!(w.configure_world(terrain_opts(VILNIUS.0, VILNIUS.1, layers, enabled)).ok);
        let mut h = Host::default();
        for _ in 0..150 {
            h.frame(&mut w);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!h.deliveries.is_empty(), "the vector world streams");
        assert_eq!(h.terrain_keys(), 0, "{layers:?} / enabled {enabled}: a t/ key reached the host");
        let mut n = 0;
        w.debug_tiles(|k, _, _, _| n += (k.layer == crate::rings::LayerKind::Terrain) as usize);
        assert_eq!(n, 0);
        assert!(!w.terrain_layer_active() && w.stats().terrain_rings == 0 && w.terrain_cpu().is_none());
    }
    let mut w = world();
    assert!(w.configure_world(terrain_opts(VILNIUS.0, VILNIUS.1, Some(&["world", "terrain"]), true)).ok);
    assert!(w.terrain_layer_active());
    let mut h = Host::default();
    let t0 = Instant::now();
    // The vector world's first tiles wait for the generated overview tile (T2c, ~1-2 s in debug).
    while (h.held.iter().filter(|k| k.layer == crate::rings::LayerKind::Terrain).count() < 20
        || !h.held.iter().any(|k| k.layer == crate::rings::LayerKind::World))
        && t0.elapsed() < Duration::from_secs(30)
    {
        h.frame(&mut w);
        std::thread::sleep(Duration::from_millis(2));
    }
    let held_terrain = h.held.iter().filter(|k| k.layer == crate::rings::LayerKind::Terrain).count();
    assert!(held_terrain >= 20, "only {held_terrain} terrain tiles");
    assert!(h.held.iter().any(|k| k.layer == crate::rings::LayerKind::World));
    let st = w.stats();
    assert_eq!(st.terrain_rings, 2);
    assert!(st.terrain_loaded[..2].iter().sum::<i32>() as usize == held_terrain && st.terrain_tris > 0);
    let mut dbg = 0;
    w.debug_tiles(|k, ring, _, _| {
        if k.layer == crate::rings::LayerKind::Terrain {
            dbg += 1;
            assert!(ring < 2);
        }
    });
    assert!(dbg >= held_terrain);
    let cpu = w.terrain_cpu().unwrap();
    assert!(cpu.n[..2].iter().sum::<u32>() as usize >= held_terrain);
    // Teleport: every key of both layers comes back once.
    let held = h.held.clone();
    assert!(w.configure_world(terrain_opts(54.8985, 23.9036, Some(&["world", "terrain"]), true)).ok);
    let unloaded: BTreeSet<LayerKey> = w.drain_unload().iter().copied().collect();
    assert_eq!(unloaded, held, "teleport unloads both layers");
    assert!(w.drain_unload().is_empty());
}

/// api 1.6: `terrain_center` keeps the relief (and its flat zone) where it was across a
/// teleport; without it the flat zone follows the new origin. `status().terrain_active`.
#[test]
fn terrain_center_keeps_the_relief_across_a_reconfigure_and_status_reports_the_layer() {
    let layers = Some(&["world", "terrain"][..]);
    let height_at = |w: &World, lat: f64, lon: f64| {
        let p = w.geodetic_to_local(lat, lon, 0.0).unwrap();
        w.ground_height_at(p[0], p[2])
    };
    let (a, b) = ((VILNIUS.0, VILNIUS.1), (VILNIUS.0 + 0.045, VILNIUS.1 + 0.07)); // ~5 km / 4.5 km away
    let mut w = world();
    assert!(!w.status().terrain_active, "not configured");
    assert!(w.configure_world(terrain_opts(a.0, a.1, layers, true)).ok);
    assert!(w.status().terrain_active);
    let probe = [(a.0, a.1), (a.0 + 0.004, a.1 + 0.003), (b.0, b.1)];
    let first: Vec<f64> = probe.iter().map(|p| height_at(&w, p.0, p.1)).collect();
    // Same centre, new origin: every probe height is the same (up to the local-frame round trip).
    let mut o = terrain_opts(b.0, b.1, layers, true);
    o.terrain_center = Some([a.0, a.1]);
    assert!(w.configure_world(o).ok);
    for (p, h) in probe.iter().zip(&first) {
        assert!((height_at(&w, p.0, p.1) - h).abs() < 1e-6, "height at {p:?} moved");
    }
    // No centre: the flat zone moves to the new origin, so a spot that was inside the old flat
    // zone (probe 1, ~550 m from its centre) gets relief.
    assert!(w.configure_world(terrain_opts(b.0, b.1, layers, true)).ok);
    assert!((height_at(&w, probe[1].0, probe[1].1) - first[1]).abs() > 1.0, "the flat zone should have moved");
    // Out of range or not requested.
    let mut bad = terrain_opts(a.0, a.1, layers, true);
    bad.terrain_center = Some([91.0, 0.0]);
    let r = w.configure_world(bad);
    assert!(!r.ok && r.error.contains("terrain_center"), "{}", r.error);
    assert!(w.status().terrain_active, "a rejected configure changes nothing");
    assert!(w.configure_world(terrain_opts(a.0, a.1, Some(&["world"]), true)).ok);
    assert!(!w.status().terrain_active, "layer not requested: flat");
    w.shutdown();
    assert!(!w.status().terrain_active);
}

/// `set_terrain_detail` (api 1.10, Terrain v2 G2): clamped to [0.25, detail_scale_max]; a
/// tau scale of the terrain cover. 2 coarsens it through ordinary swaps: fewer terrain tiles,
/// no re-delivery of a held key (nothing is re-meshed), terrain unloads; the vector world is
/// untouched. Back to 1 restores the finer cover.
#[test]
fn set_terrain_detail_scales_the_terrain_cover() {
    let mut w = world();
    assert_eq!(w.set_terrain_detail(0.1), 0.25);
    assert_eq!(w.set_terrain_detail(10.0), 2.0, "the default profile's detail_scale_max");
    assert_eq!(w.set_terrain_detail(f64::NAN), 2.0, "ignored");
    assert_eq!(w.set_terrain_detail(1.0), 1.0);
    assert!(w.configure_world(terrain_opts(VILNIUS.0, VILNIUS.1, Some(&["world", "terrain"]), true)).ok);
    assert_eq!(w.set_terrain_detail(5.0), 2.0, "terrain.detail_scale_max");
    assert_eq!(w.set_terrain_detail(1.0), 1.0);
    let mut h = Host::default();
    h.settle_terrain(&mut w);
    let terrain = |h: &Host| -> BTreeSet<LayerKey> { h.held.iter().filter(|k| k.layer == crate::rings::LayerKind::Terrain).copied().collect() };
    let held = terrain(&h);
    assert!(held.len() >= 20);
    let (nd, nu) = (h.deliveries.len(), h.unloads.len());
    assert_eq!(w.set_terrain_detail(2.0), 2.0);
    h.settle_terrain(&mut w);
    let coarse = terrain(&h);
    let new: Vec<(LayerKey, bool)> =
        h.deliveries[nd..].iter().filter(|(k, _)| k.layer == crate::rings::LayerKind::Terrain).copied().collect();
    assert!(new.iter().all(|(_, again)| !*again), "no re-deliveries");
    assert!(new.iter().all(|(k, _)| !held.contains(k)), "no held key delivered again");
    assert!(h.unloads[nu..].iter().any(|k| k.layer == crate::rings::LayerKind::Terrain), "children unloaded");
    assert!(coarse.len() < held.len(), "detail 2 holds fewer terrain tiles: {} vs {}", coarse.len(), held.len());
    assert!(h.deliveries[nd..].iter().filter(|(k, _)| k.layer == crate::rings::LayerKind::World).all(|(_, again)| !*again));
    assert_eq!(w.terrain_detail(), 2.0);
    assert_eq!(w.set_terrain_detail(1.0), 1.0);
    h.settle_terrain(&mut w);
    assert_eq!(terrain(&h), held, "scale 1 again: the same cover as before");
}

#[test]
fn cross_layer_delivery_order() {
    use crate::world::terrain_first;
    assert!(!terrain_first(None, None) && !terrain_first(Some((false, 1.0)), None));
    assert!(terrain_first(None, Some((true, 9e9))));
    // Fresh before re-deliveries, whatever the distance.
    assert!(terrain_first(Some((true, 10.0)), Some((false, 1e6))));
    assert!(!terrain_first(Some((false, 1e6)), Some((true, 10.0))));
    // Then strictly by distance from the camera (no layer weight): 999 m terrain beats 1000 m
    // world, 1001 m does not.
    assert!(terrain_first(Some((false, 1000.0)), Some((false, 999.0))));
    assert!(!terrain_first(Some((false, 1000.0)), Some((false, 1001.0))));
    assert!(!terrain_first(Some((false, 1000.0)), Some((false, 1000.0))), "ties go to the vector world");
}
