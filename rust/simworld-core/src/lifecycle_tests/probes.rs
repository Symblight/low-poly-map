//! First-fill delivery and churn probes.

use super::*;

// ---- First-fill delivery probe ------------------------------------------------------------

/// One first-fill run (static camera at local 0, 300 m AGL, 2 deliveries per frame, ~60 fps
/// wall clock) with the vector world and, optionally, the full default terrain layer.
struct FillProbe {
    /// Every fresh delivery: (frame, key, horizontal distance of the tile from the camera).
    deliveries: Vec<(usize, LayerKey, f64)>,
    /// First frame each key appeared in `poll_fades` with a value < 1 (fading in or out).
    faded: std::collections::BTreeMap<LayerKey, (usize, f32)>,
    /// (frame, key) of the first delivery of the tile under the camera, per layer and zoom.
    under: Vec<(usize, LayerKey)>,
    frames: usize,
}

fn fill_probe(terrain: bool, frames: usize) -> FillProbe {
    let mut w = world();
    let opts = if terrain {
        WorldOptions {
            profile_overrides: Some(serde_json::json!({ "terrain": { "enabled": true } })),
            layers: Some(vec!["world".into(), "terrain".into()]),
            ..synthetic(VILNIUS.0, VILNIUS.1)
        }
    } else {
        synthetic(VILNIUS.0, VILNIUS.1)
    };
    assert!(w.configure_world(opts).ok);
    let mut p = FillProbe { deliveries: Vec::new(), faded: Default::default(), under: Vec::new(), frames };
    let half_w = |z: u8| 40_075_016.7 * VILNIUS.0.to_radians().cos() / f64::from(1u32 << z) / 2.0;
    for f in 0..frames {
        let t0 = Instant::now();
        w.update_camera([0.0, 300.0, 0.0], [0.0; 3], 300.0);
        let (dl, under) = (&mut p.deliveries, &mut p.under);
        w.pop_ready(2, |t| {
            if t.layer == crate::rings::LayerKind::Terrain && t.terrain.is_empty() {
                return;
            }
            let k = t.layer_key();
            let d = t.origin[0].hypot(t.origin[2]);
            if !dl.iter().any(|x| x.1 == k) {
                dl.push((f, k, d));
                let hw = half_w(t.key.z);
                if t.origin[0].abs() <= hw && t.origin[2].abs() <= hw {
                    under.push((f, k));
                }
            }
        });
        for &(k, v) in w.drain_fades() {
            if v < 1.0 {
                p.faded.entry(k).or_insert((f, v));
            }
        }
        let _ = w.drain_unload();
        std::thread::sleep(Duration::from_millis(16).saturating_sub(t0.elapsed()));
    }
    p
}

impl FillProbe {
    fn first(&self, layer: crate::rings::LayerKind, z: u8) -> Option<usize> {
        self.deliveries.iter().find(|d| d.1.layer == layer && d.1.key.z == z).map(|d| d.0)
    }
    fn under_at(&self, layer: crate::rings::LayerKind, z: u8) -> Option<usize> {
        self.under.iter().find(|d| d.1.layer == layer && d.1.key.z == z).map(|d| d.0)
    }
    /// Longest run of frames without a terrain delivery between the first terrain delivery and
    /// the 60th one.
    fn terrain_gap(&self) -> usize {
        let t: Vec<usize> =
            self.deliveries.iter().filter(|d| d.1.layer == crate::rings::LayerKind::Terrain).map(|d| d.0).take(60).collect();
        t.windows(2).map(|w| w[1] - w[0]).max().unwrap_or(0)
    }
    fn summary(&self) -> String {
        use crate::rings::LayerKind::{Terrain, World};
        let mut s = String::new();
        for z in 13..=16 {
            s += &format!("world z{z}: first {:?} under-cam {:?}\n", self.first(World, z), self.under_at(World, z));
        }
        for z in 10..=14 {
            s += &format!("terrain z{z}: first {:?} under-cam {:?}\n", self.first(Terrain, z), self.under_at(Terrain, z));
        }
        let fresh_faded_in = self
            .deliveries
            .iter()
            .filter(|d| self.faded.get(&d.1).is_some_and(|&(f, v)| f <= d.0 + 1 && v == 0.0))
            .count();
        s += &format!(
            "deliveries {} in {} frames, terrain max gap (first 60) {} frames, fresh tiles that faded in from 0: {}\n",
            self.deliveries.len(),
            self.frames,
            self.terrain_gap(),
            fresh_faded_in
        );
        for d in self.deliveries.iter().filter(|d| std::env::var("PROBE_TERRAIN").is_err() || d.1.layer == crate::rings::LayerKind::Terrain).take(48) {
            s += &format!("  f{:>3} {:<16} {:>7.0} m\n", d.0, d.1.to_string(), d.2);
        }
        s
    }
}

/// Probe for the delivery order at a first fill (prints only): `cargo test -p simworld-core
/// fill_probe_print -- --ignored --nocapture`.
#[test]
#[ignore]
fn fill_probe_print() {
    println!("== vector only ==\n{}", fill_probe(false, 240).summary());
    println!("== vector + terrain ==\n{}", fill_probe(true, 360).summary());
}

/// Cover churn of one run: deliveries, unloads, keys delivered again after an unload (`again`),
/// of those within 180 frames (`soon`), how many of the re-deliveries were coarsen swaps (the
/// 4 children unloaded within 90 frames after), and examples of the others.
#[derive(Debug)]
struct Churn {
    deliveries: usize,
    unloads: usize,
    again: usize,
    soon: usize,
    coarsen_swaps: usize,
    other: Vec<String>,
}

fn churn_probe(terrain: bool, frames: usize, cam: impl Fn(usize) -> ([f64; 3], [f64; 3], f64)) -> Churn {
    let mut w = world();
    let opts = WorldOptions {
        profile_overrides: terrain.then(|| serde_json::json!({ "terrain": { "enabled": true } })),
        layers: terrain.then(|| vec!["world".into(), "terrain".into()]),
        ..synthetic(VILNIUS.0, VILNIUS.1)
    };
    assert!(w.configure_world(opts).ok);
    let mut unloads: Vec<(usize, LayerKey)> = Vec::new();
    let mut last_unload: std::collections::HashMap<LayerKey, usize> = Default::default();
    let mut again: Vec<(usize, usize, LayerKey, usize, [f64; 2], f64)> = Vec::new();
    let mut c = Churn { deliveries: 0, unloads: 0, again: 0, soon: 0, coarsen_swaps: 0, other: Vec::new() };
    let mut shift = [0.0f64; 3];
    for f in 0..frames {
        let t0 = Instant::now();
        let (p, v, agl) = cam(f);
        let local = [p[0] - shift[0], p[1], p[2] - shift[2]];
        if let Some(s) = w.update_camera(local, v, agl) {
            shift = [shift[0] + s[0], 0.0, shift[2] + s[2]];
        }
        let mut got = Vec::new();
        w.pop_ready(2, |t| got.push((t.layer_key(), t.ring)));
        for (k, ring) in got {
            c.deliveries += 1;
            if let Some(&u) = last_unload.get(&k) {
                again.push((u, f, k, ring, [p[0].round(), p[2].round()], agl));
            }
        }
        let _ = w.drain_fades();
        for k in w.drain_unload() {
            c.unloads += 1;
            last_unload.insert(*k, f);
            unloads.push((f, *k));
        }
        std::thread::sleep(Duration::from_millis(16).saturating_sub(t0.elapsed()));
    }
    for (u, f, k, ring, p, agl) in again {
        c.again += 1;
        if f - u <= 180 {
            c.soon += 1;
        }
        let kids = crate::rings::children(k.key);
        let swap = kids.iter().all(|ch| unloads.iter().any(|&(uf, uk)| uk.layer == k.layer && uk.key == *ch && uf >= f && uf <= f + 90));
        if swap {
            c.coarsen_swaps += 1;
        } else if c.other.len() < 12 {
            c.other.push(format!("{k} ring {ring}: unloaded f{u}, delivered again f{f} (cam {p:?} agl {agl:.0})"));
        }
    }
    c
}

/// Churn probe (prints only): `cargo test -p simworld-core churn_probe_print -- --ignored --nocapture`.
#[test]
#[ignore]
fn churn_probe_print() {
    for terrain in [false, true] {
        let r = churn_probe(terrain, 900, |_| ([0.0, 300.0, 0.0], [0.0; 3], 300.0));
        println!("terrain {terrain} static 300 m: {r:#?}");
        for speed in [80.0, 250.0] {
            let r = churn_probe(terrain, 900, |f| ([0.0, 300.0, -speed * f as f64 / 60.0], [0.0, 0.0, -speed], 300.0));
            println!("terrain {terrain} level north {speed} m/s: {r:#?}");
            let r = churn_probe(terrain, 900, |f| {
                let t = f as f64 / 60.0;
                let agl = (300.0 + 180.0 * t).min(3000.0);
                ([0.0, agl, -speed * t], [0.0, if agl < 3000.0 { 180.0 } else { 0.0 }, -speed], agl)
            });
            println!("terrain {terrain} north {speed} m/s climbing: {r:#?}");
        }
    }
}
