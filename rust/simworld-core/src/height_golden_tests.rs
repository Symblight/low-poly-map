//! Height golden (Tiles v2 + Terrain v2, step P0 "freeze"): pins the exact f64 bits of the terrain
//! height function, `Terrain::height` and `World::ground_height_at`, before any refactor of the
//! terrain or tile pipeline. Nothing is quantized: the height function is built to be
//! bit-identical on every thread and platform (`libm` through `math.rs`, fixed operation order,
//! no fused multiply-add), and `ground_height_at`, building bases and the drape must keep sampling
//! exactly this function (user decision for Terrain v2), so any change of a single bit shows here.
//!
//! Two worlds:
//! * (a) the plain procedural terrain (default profile + `terrain.enabled`, flat zone at the
//!   Vilnius origin): random points around Vilnius and over the whole globe, rings across the flat
//!   zone's edges (radius 1500 m, blend 2000 m), the same rings around a second flat-zone centre
//!   (the `terrain_center` option), and `World::ground_height_at` of a world configured with an
//!   origin away from its `terrain_center` (the local -> geodetic path).
//! * (b) the shared seed world `low-poly-map/world/two-cities/` (`seed::world::test_world`): random
//!   points over Lithuania and the Baltic, offshore points, lake points near Kaunas, and per runway
//!   of every airport points along the centreline, across and beyond the strip, past the ends, at
//!   the fence corners and out through the shoulder.
//!
//! Every point set is hashed (FNV-1a 64 over the bits of lat, lon and height) per group, and about
//! 20 labelled points are pinned as literal values that the failure message prints side by side.
//! Points come from the seed hash (`seed_hash!`, counter-based), never from a stateful RNG.
//!
//! Regenerate only for an intended change of the height function:
//! `SIMWORLD_UPDATE_GOLDEN=1 cargo test -p simworld-core golden -- --nocapture`
//! prints `HEIGHT_GOLDEN_*` and `HEIGHT_PINNED_*` instead of failing; paste them here and say why.

use crate::geo::Frame;
use crate::profile::Profile;
use crate::seed::hash::unit;
use crate::terrain::Terrain;

const VILNIUS: (f64, f64) = (54.6872, 25.2797);
/// Second flat-zone centre (the `terrain_center` option), ~13 km south-west of the origin.
const CENTER2: (f64, f64) = (54.60, 25.10);

/// Bearings x distances (m) of the flat-zone rings: inside, at and across the radius (1500 m)
/// and the end of the blend (1500 + 2000 m), and beyond.
const RING_BEARINGS: usize = 24;
const RING_DIST_M: [f64; 12] = [0.0, 750.0, 1499.0, 1500.0, 1501.0, 2000.0, 2500.0, 3000.0, 3499.0, 3500.0, 3501.0, 5000.0];

/// One sample: label (pinned points only), lat, lon, height.
type Sample = (f64, f64, f64);

fn fnv(samples: &[Sample]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut word = |w: u64| {
        for b in w.to_le_bytes() {
            h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
        }
    };
    word(samples.len() as u64);
    for &(lat, lon, v) in samples {
        word(lat.to_bits());
        word(lon.to_bits());
        word(v.to_bits());
    }
    h
}

/// Uniform point `i` of group `tag` in the box [lat0, lat1] x [lon0, lon1].
fn random_point(tag: &str, i: usize, lat: (f64, f64), lon: (f64, f64)) -> (f64, f64) {
    let a = unit(crate::seed_hash!("height-golden", tag, i as u64, 0u64));
    let b = unit(crate::seed_hash!("height-golden", tag, i as u64, 1u64));
    (lat.0 + (lat.1 - lat.0) * a, lon.0 + (lon.1 - lon.0) * b)
}

/// Rings around `center` (lat, lon) through the flat zone's edges.
fn ring_points(center: (f64, f64)) -> Vec<(f64, f64)> {
    let f = Frame::new(center.0, center.1);
    let mut v = Vec::new();
    for k in 0..RING_BEARINGS {
        let a = (k as f64 / RING_BEARINGS as f64) * std::f64::consts::TAU;
        for &d in &RING_DIST_M {
            v.push(f.lat_lon(d * a.sin(), d * a.cos()));
        }
    }
    v
}

fn sample(t: &Terrain, pts: &[(f64, f64)]) -> Vec<Sample> {
    pts.iter().map(|&(lat, lon)| (lat, lon, t.height(lat, lon))).collect()
}

/// A group's result: name, sample count, hash.
type Group = (&'static str, usize, u64);

/// Compare groups and pinned values, or print the tables to paste with `SIMWORLD_UPDATE_GOLDEN=1`.
fn check(
    what: &str,
    groups: &[Group],
    want_groups: &[Group],
    pinned: &[(&'static str, Sample)],
    want_pinned: &[(&str, f64)],
) {
    let gtable: String = groups.iter().map(|(n, c, h)| format!("    (\"{n}\", {c}, 0x{h:016x}),\n")).collect();
    let ptable: String = pinned.iter().map(|(n, (_, _, v))| format!("    (\"{n}\", {v:?}),\n")).collect();
    if std::env::var("SIMWORLD_UPDATE_GOLDEN").is_ok_and(|v| v == "1") {
        println!("const HEIGHT_GOLDEN_{what}: &[(&str, usize, u64)] = &[\n{gtable}];");
        println!("const HEIGHT_PINNED_{what}: &[(&str, f64)] = &[\n{ptable}];");
        return;
    }
    // Readable side by side of the pinned points first (exact bits compared).
    let mut report = String::new();
    let mut pinned_ok = pinned.len() == want_pinned.len();
    for (i, (name, (lat, lon, got))) in pinned.iter().enumerate() {
        let want = want_pinned.get(i).filter(|(n, _)| n == name).map(|(_, v)| *v);
        let same = want.is_some_and(|w| w.to_bits() == got.to_bits());
        pinned_ok &= same;
        report.push_str(&format!(
            "  {} {name:<28} ({lat:.7}, {lon:.7})  want {:<24} got {got:?}\n",
            if same { "  " } else { "!!" },
            want.map_or("<missing>".to_string(), |w| format!("{w:?}"))
        ));
    }
    let groups_ok = groups == want_groups;
    assert!(
        pinned_ok && groups_ok,
        "height golden ({what}) changed: the height function, `ground_height_at` or a seed rule moved.\n\
         pinned points (!! = differs):\n{report}groups (name, samples, hash) now:\n{gtable}want:\n{}\n\
         If the change is intended, regenerate with SIMWORLD_UPDATE_GOLDEN=1 cargo test -p simworld-core golden -- --nocapture",
        want_groups.iter().map(|(n, c, h)| format!("    (\"{n}\", {c}, 0x{h:016x}),\n")).collect::<String>()
    );
}

// ---- (a) plain procedural terrain ------------------------------------------------------------

fn plain_profile() -> Profile {
    crate::profile::resolve(None, Some(&serde_json::json!({ "terrain": { "enabled": true } })), &crate::NullLog).unwrap()
}

const HEIGHT_GOLDEN_PLAIN: &[(&str, usize, u64)] = &[
    ("near", 6000, 0xda4500b9e73eebde),
    ("globe", 1000, 0x26ccc6bbed01affe),
    ("flat_rings", 288, 0x32e03d96882fbabf),
    ("center2_rings", 288, 0x65f3b7a3a855e7b5),
    ("world_ground_height_at", 1442, 0x4a98d41bf072c998),
];
const HEIGHT_PINNED_PLAIN: &[(&str, f64)] = &[
    ("origin", 146.05799710966886),
    ("flat_r1499_n", 146.05799710966886),
    ("flat_r1501_e", 146.05799710966886),
    ("blend_r2500_s", 246.81425728602719),
    ("blend_r3499_w", 275.8115311958645),
    ("relief_r5000_ne", 252.7123986232338),
    ("center2", 263.5959574824891),
    ("center2_r3501_s", 361.6132193779431),
    ("near_0", 513.2522563151811),
    ("near_first_sea", 0.0),
    ("near_highest", 664.2770767417192),
    ("globe_0", 9.408632684444228),
    ("world_local_0_0", 146.05799710966886),
    ("world_local_center2", 263.5959574824891),
];

#[test]
fn golden_height_plain_terrain() {
    use crate::world::{World, WorldOptions};
    use std::sync::Arc;
    let p = plain_profile();
    let (radius, blend, sea) = (p.terrain.flat_radius_m, p.terrain.flat_blend_m, p.terrain.sea_level_m);
    assert_eq!((radius, blend), (1500.0, 2000.0), "the ring distances assume the default flat zone");
    let t = Terrain::from_profile(&p, VILNIUS.0, VILNIUS.1);
    let t2 = Terrain::from_profile(&p, CENTER2.0, CENTER2.1);

    let near: Vec<(f64, f64)> = (0..6000).map(|i| random_point("plain-near", i, (53.5, 56.0), (23.0, 27.5))).collect();
    let globe: Vec<(f64, f64)> = (0..1000).map(|i| random_point("plain-globe", i, (-80.0, 80.0), (-180.0, 180.0))).collect();
    let s_near = sample(&t, &near);
    let s_globe = sample(&t, &globe);
    let s_ring = sample(&t, &ring_points(VILNIUS));
    let s_ring2 = sample(&t2, &ring_points(CENTER2));

    // World::ground_height_at: origin Vilnius, flat zone at CENTER2 (`terrain_center`), local
    // grid around CENTER2 plus random local points within 30 km. Each must equal the terrain
    // function at the frame's lat/lon of the point, bit for bit.
    let mut w = World::new(Arc::new(crate::NullLog), crate::Diagnostics::default(), "height-golden");
    let r = w.configure_world(WorldOptions {
        origin_lat: VILNIUS.0,
        origin_lon: VILNIUS.1,
        profile_overrides: Some(serde_json::json!({ "terrain": { "enabled": true } })),
        layers: Some(vec!["world".into(), "terrain".into()]),
        terrain_center: Some([CENTER2.0, CENTER2.1]),
        ..WorldOptions::default()
    });
    assert!(r.ok, "{}", r.error);
    assert!(w.terrain().is_active());
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let c2 = frame.enu(CENTER2.0, CENTER2.1);
    let mut local: Vec<[f64; 2]> = vec![[0.0, 0.0]];
    for j in 0..21 {
        for i in 0..21 {
            local.push([c2[0] + (i as f64 - 10.0) * 500.0, -(c2[1] + (j as f64 - 10.0) * 500.0)]);
        }
    }
    for i in 0..1000 {
        let (a, b) = random_point("plain-local", i, (-30_000.0, 30_000.0), (-30_000.0, 30_000.0));
        local.push([a, b]);
    }
    let s_world: Vec<Sample> = local
        .iter()
        .map(|&[x, z]| {
            let h = w.ground_height_at(x, z);
            let (lat, lon) = frame.lat_lon(x, -z);
            assert_eq!(h.to_bits(), t2.height(lat, lon).to_bits(), "ground_height_at({x}, {z}) = the terrain function");
            (x, z, h)
        })
        .collect();
    w.shutdown();

    // The sets exercise what they claim: the sea-level clamp, the flat zone, the relief.
    let at_sea = s_near.iter().filter(|s| s.2 == sea).count();
    let flat_h = t.height(VILNIUS.0, VILNIUS.1);
    let flat_in = s_ring.iter().filter(|s| s.2 == flat_h).count();
    let max = s_near.iter().map(|s| s.2).fold(f64::NEG_INFINITY, f64::max);
    println!(
        "plain: {} samples; near: {at_sea} at sea level, max {max:.1} m; flat zone {flat_h} m on {flat_in} ring points",
        s_near.len() + s_globe.len() + s_ring.len() + s_ring2.len() + s_world.len()
    );
    assert!(at_sea > 0, "some random points on the sea-level clamp");
    assert!(flat_in >= RING_BEARINGS * 5, "ring points inside the flat radius are flat: {flat_in}");

    let groups: Vec<Group> = vec![
        ("near", s_near.len(), fnv(&s_near)),
        ("globe", s_globe.len(), fnv(&s_globe)),
        ("flat_rings", s_ring.len(), fnv(&s_ring)),
        ("center2_rings", s_ring2.len(), fnv(&s_ring2)),
        ("world_ground_height_at", s_world.len(), fnv(&s_world)),
    ];
    let first_sea = *s_near.iter().find(|s| s.2 == sea).unwrap();
    let highest = *s_near.iter().max_by(|a, b| a.2.total_cmp(&b.2)).unwrap();
    let ring = |s: &[Sample], bearing: usize, d: usize| s[bearing * RING_DIST_M.len() + d];
    let pinned: Vec<(&'static str, Sample)> = vec![
        ("origin", ring(&s_ring, 0, 0)),
        ("flat_r1499_n", ring(&s_ring, 0, 2)),
        ("flat_r1501_e", ring(&s_ring, 6, 4)),
        ("blend_r2500_s", ring(&s_ring, 12, 6)),
        ("blend_r3499_w", ring(&s_ring, 18, 8)),
        ("relief_r5000_ne", ring(&s_ring, 3, 11)),
        ("center2", ring(&s_ring2, 0, 0)),
        ("center2_r3501_s", ring(&s_ring2, 12, 10)),
        ("near_0", s_near[0]),
        ("near_first_sea", first_sea),
        ("near_highest", highest),
        ("globe_0", s_globe[0]),
        // Local (x, z) in place of (lat, lon).
        ("world_local_0_0", s_world[0]),
        ("world_local_center2", s_world[1 + 10 * 21 + 10]),
    ];
    check("PLAIN", &groups, HEIGHT_GOLDEN_PLAIN, &pinned, HEIGHT_PINNED_PLAIN);
}

// ---- (b) seed world two-cities ---------------------------------------------------------------

/// Points in the Baltic west of the Lithuanian coast (inside the water file's bbox).
const OFFSHORE: [(f64, f64); 10] = [
    (55.40, 20.90),
    (55.50, 20.70),
    (55.60, 20.95),
    (55.70, 20.60),
    (55.80, 20.90),
    (55.90, 20.80),
    (56.00, 20.95),
    (55.45, 20.40),
    (55.65, 19.90),
    (56.20, 20.50),
];

const HEIGHT_GOLDEN_SEED: &[(&str, usize, u64)] = &[
    ("land", 6000, 0xd56924494dca8c6f),
    ("offshore", 10, 0xf697cd17a3bb49d0),
    ("lakes", 577, 0xe4537c673ec1841f),
    ("rivers", 85, 0x874734873d967d11),
    ("airports", 1483, 0xb7788e03f042de70),
];
const HEIGHT_PINNED_SEED: &[(&str, f64)] = &[
    ("land_0", 433.98363210506466),
    ("land_1", 72.80390080671557),
    ("land_highest", 660.657871061966),
    ("offshore_0", 0.0),
    ("lake_0", 393.69),
    ("lake_last", 61.48),
    ("river_0", 378.37577522700474),
    ("EYVI_main_mid", 328.6949999999994),
    ("EYVI_main_end0", 330.88),
    ("EYKA_main_mid", 269.5002158587185),
    ("EYKA_main_end0", 253.30902986786484),
    ("airport_shoulder_0", 267.04),
    ("airport_fence_corner_last", 314.10780020254333),
];

#[test]
fn golden_height_seed_world() {
    use crate::seed::files::WaterKind;
    use crate::seed::water::WaterHit;
    let (p, t, w) = crate::seed::world::test_world();
    let sea = p.terrain.sea_level_m;
    let shoulder = p.seed.airport.shoulder_m;

    let land: Vec<(f64, f64)> = (0..6000).map(|i| random_point("seed-land", i, (53.85, 56.5), (20.0, 27.0))).collect();
    let s_land = sample(t, &land);
    let s_off = sample(t, &OFFSHORE);
    // Lakes near Kaunas (the reservoir and smaller lakes): every lake hit of a 80 x 40 grid.
    let mut lakes = Vec::new();
    let mut rivers = Vec::new();
    for i in 0..80 {
        for j in 0..40 {
            let (lat, lon) = (54.84 + j as f64 * 0.002, 23.85 + i as f64 * 0.004);
            if let WaterHit::Body(b) = w.water.water_at(lat, lon) {
                match w.water.bodies()[b as usize].kind {
                    WaterKind::Lake => lakes.push((lat, lon)),
                    WaterKind::River => rivers.push((lat, lon)),
                    _ => {}
                }
            }
        }
    }
    let s_lake = sample(t, &lakes);
    let s_river = sample(t, &rivers);
    // Airports: centreline, across and beyond the strip, past the ends, fence corners, shoulder.
    let mut apt = Vec::new();
    let mut labelled: Vec<(String, usize)> = Vec::new();
    for g in &w.airports {
        for (ri, r) in g.runways.iter().enumerate() {
            if ri == g.main {
                labelled.push((format!("{}_mid", g.ident), apt.len() + 3));
                labelled.push((format!("{}_end0", g.ident), apt.len()));
            }
            for k in 0..7 {
                apt.push(g.frame.ll(r.at(r.length_m * k as f64 / 6.0, 0.0)));
            }
            for t_m in [-1.0, -0.5, 0.5, 1.0].map(|f| f * r.strip_half_m * 0.999) {
                apt.push(g.frame.ll(r.at(r.length_m * 0.5, t_m)));
            }
            for d in [1.0, 0.25 * shoulder, 0.5 * shoulder, shoulder, 1.5 * shoulder] {
                apt.push(g.frame.ll(r.at(r.length_m * 0.5, r.strip_half_m + d)));
                apt.push(g.frame.ll(r.at(r.length_m * 0.5, -(r.strip_half_m + d))));
                apt.push(g.frame.ll(r.at(-d, 0.0)));
                apt.push(g.frame.ll(r.at(r.length_m + d, 0.0)));
            }
        }
        let f = &g.fence;
        let v = f.v();
        for (su, sv) in [(1.0, 1.0), (1.0, -1.0), (-1.0, 1.0), (-1.0, -1.0)] {
            for grow in [0.0, 0.5 * shoulder, shoulder] {
                let (a, b) = (su * (f.half[0] + grow), sv * (f.half[1] + grow));
                apt.push(g.frame.ll([f.c[0] + a * f.u[0] + b * v[0], f.c[1] + a * f.u[1] + b * v[1]]));
            }
        }
    }
    let s_apt = sample(t, &apt);

    let at_sea = s_off.iter().filter(|s| s.2 == sea).count();
    println!(
        "seed: {} samples ({} airports, {} lake, {} river points); offshore at sea level {at_sea}/{}",
        s_land.len() + s_off.len() + s_lake.len() + s_river.len() + s_apt.len(),
        w.airports.len(),
        s_lake.len(),
        s_river.len(),
        s_off.len()
    );
    assert_eq!(at_sea, OFFSHORE.len(), "the sea is flat at sea level");
    assert!(s_lake.len() > 50 && !s_river.is_empty() && w.airports.len() >= 2);

    let groups: Vec<Group> = vec![
        ("land", s_land.len(), fnv(&s_land)),
        ("offshore", s_off.len(), fnv(&s_off)),
        ("lakes", s_lake.len(), fnv(&s_lake)),
        ("rivers", s_river.len(), fnv(&s_river)),
        ("airports", s_apt.len(), fnv(&s_apt)),
    ];
    let find = |name: &str| labelled.iter().find(|(n, _)| n == name).map(|(_, i)| s_apt[*i]).unwrap_or_else(|| panic!("{name}"));
    let highest = *s_land.iter().max_by(|a, b| a.2.total_cmp(&b.2)).unwrap();
    let pinned: Vec<(&'static str, Sample)> = vec![
        ("land_0", s_land[0]),
        ("land_1", s_land[1]),
        ("land_highest", highest),
        ("offshore_0", s_off[0]),
        ("lake_0", s_lake[0]),
        ("lake_last", *s_lake.last().unwrap()),
        ("river_0", s_river[0]),
        ("EYVI_main_mid", find("EYVI_mid")),
        ("EYVI_main_end0", find("EYVI_end0")),
        ("EYKA_main_mid", find("EYKA_mid")),
        ("EYKA_main_end0", find("EYKA_end0")),
        ("airport_shoulder_0", s_apt[7 + 4 + 3]),
        ("airport_fence_corner_last", *s_apt.last().unwrap()),
    ];
    check("SEED", &groups, HEIGHT_GOLDEN_SEED, &pinned, HEIGHT_PINNED_SEED);
}
