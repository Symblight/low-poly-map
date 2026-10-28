//! S1 fixes (2026-10-28): the drawn runway equals `ground_height_at` (no drape lift inside
//! airport fences, contract section 9 "Airports"), and the terrain COLOR of a seed world does
//! not bleed across the coast (contract 9a, `docs/spec-world-seed.md` 5.5).

use std::collections::HashMap;

use super::airport::{AirportGeo, LEVEL_RUNWAY};
use super::world::test_world;
use crate::geo::{tile_for, Frame, TileKey};
use crate::mesher::MeshData;
use crate::terrain::tin::{build_terrain_tile, WATER_COLOR_R};
use crate::worker::{build_seed_tile_on, build_terrain_data};

const VILNIUS: (f64, f64) = (54.6872, 25.2797);

/// Top surface height (y) of `m` (positions relative to `center`, Godot axes) at local
/// (east, north) `p`: the highest triangle containing the point, `None` = no ground there.
fn surface_at(m: &MeshData, center: [f64; 2], p: [f64; 2], tris: Option<usize>) -> Option<f64> {
    let (x, z) = (p[0] - center[0], -(p[1] - center[1]));
    let n = tris.unwrap_or(m.indices.len() / 3);
    let mut best: Option<f64> = None;
    for t in m.indices[..3 * n].chunks_exact(3) {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| m.positions[i as usize].map(|v| v as f64));
        let d = (b[0] - a[0]) * (c[2] - a[2]) - (c[0] - a[0]) * (b[2] - a[2]);
        if d.abs() < 1e-9 {
            continue;
        }
        let u = ((x - a[0]) * (c[2] - a[2]) - (c[0] - a[0]) * (z - a[2])) / d;
        let v = ((b[0] - a[0]) * (z - a[2]) - (x - a[0]) * (b[2] - a[2])) / d;
        let eps = 1e-6;
        if u < -eps || v < -eps || u + v > 1.0 + eps {
            continue;
        }
        let y = a[1] + u * (b[1] - a[1]) + v * (c[1] - a[1]);
        best = Some(best.map_or(y, |b: f64| b.max(y)));
    }
    best
}

/// Points of a runway: 20 along the centreline (end to end) and 9 across the strip at 5
/// stations; (lat, lon, on the runway pavement, at least 30 m from both physical ends).
fn runway_points(g: &AirportGeo, ri: usize) -> Vec<(f64, f64, bool, bool)> {
    let r = &g.runways[ri];
    let mut v = Vec::new();
    let interior = |s: f64| s >= 30.0 && s <= r.length_m - 30.0;
    for k in 0..20 {
        let s = r.length_m * k as f64 / 19.0;
        let (lat, lon) = g.frame.ll(r.at(s, 0.0));
        v.push((lat, lon, true, interior(s)));
    }
    for k in 0..5 {
        let s = r.length_m * (0.1 + 0.2 * k as f64);
        for j in 0..9 {
            let t = r.strip_half_m * (-1.0 + 2.0 * j as f64 / 8.0) * 0.999;
            let (lat, lon) = g.frame.ll(r.at(s, t));
            v.push((lat, lon, t.abs() <= r.width_m * 0.5, interior(s)));
        }
    }
    v
}

/// Terrain mesh tolerance inside a fence (1 cm, `LIFT_FIELD_MIN_ERROR_M`) plus f32 rounding.
const TOL_M: f64 = 0.012;
/// Extra allowance within 30 m of a runway's physical end: the strip height is clamped past the
/// ends, so the fence plane kinks there (slope change <= `seed.airport.max_slope`, 1 %), and a
/// drape edge of up to `drape_step_m` (25 m) bridges the kink: <= 1 % x 25 m / 4.
const KINK_M: f64 = 0.0625;

/// Whether runway `ri`'s strip overlaps another strip of the same airport (crossing or close
/// parallel runways, or approximate runways all centred on the reference point): there the strip
/// rule's "first key wins" makes the height function step across the pavement
/// (`docs/spec-world-seed.md` 11c (8)), which no drape can follow.
fn strip_overlaps(g: &AirportGeo, ri: usize) -> bool {
    let r = &g.runways[ri];
    let inside = |o: &super::airport::RunwayGeo, p: [f64; 2]| {
        let q = [p[0] - o.a[0], p[1] - o.a[1]];
        let s = q[0] * o.dir[0] + q[1] * o.dir[1];
        let t = -q[0] * o.dir[1] + q[1] * o.dir[0];
        s >= 0.0 && s <= o.length_m && t.abs() <= o.strip_half_m
    };
    g.runways.iter().enumerate().filter(|(j, _)| *j != ri).any(|(_, o)| {
        (0..=20).any(|i| (0..=8).any(|j| inside(o, r.at(r.length_m * i as f64 / 20.0, r.strip_half_m * (-1.0 + j as f64 / 4.0)))))
    })
}

/// Whether the height function steps within 30 m of a point (the strips of two runways overlap
/// and the first key wins, `docs/spec-world-seed.md` 11c (8)): along 8 rays at 1 m spacing a
/// neighbour differs by more than 5 cm (the 1 % slope cap allows 1 cm per metre).
fn near_step(t: &crate::terrain::Terrain, g: &AirportGeo, lat: f64, lon: f64) -> bool {
    let c = g.frame.xy(lat, lon);
    const R: f64 = std::f64::consts::FRAC_1_SQRT_2;
    [(1.0, 0.0), (R, R), (0.0, 1.0), (-R, R), (-1.0, 0.0), (-R, -R), (0.0, -1.0), (R, -R)].iter().any(|&(dx, dy)| {
        let mut prev = t.height(lat, lon);
        (1..=30).any(|k| {
            let (la, lo) = g.frame.ll([c[0] + dx * k as f64, c[1] + dy * k as f64]);
            let h = t.height(la, lo);
            let jump = (h - prev).abs() > 0.05;
            prev = h;
            jump
        })
    })
}

/// (a) At 20 points along every runway centreline and 45 across its strip, the drawn ground of
/// the generated tile (top surface) equals `ground_height_at` plus the airport's stacking level
/// x `lift_step_m` (no drape lift inside a fence), in every band: `3 x step` on the runway
/// (R0 0.06 m) within the drape tolerance (1 cm in R0, half a step in R1..R3). The terrain mesh of T0..T2 over the same points is within 1 cm of the height
/// function (it never pokes through the unlifted pavement).
#[test]
fn drawn_runway_equals_ground_height_at_in_every_band() {
    let (p, t, w) = test_world();
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let mut tiles: HashMap<(TileKey, usize), MeshData> = HashMap::new();
    let mut worst = [0.0f64; 4];
    let mut worst_runway = [0.0f64; 4];
    let mut best = [f64::INFINITY; 4];
    let mut best_runway = [f64::INFINITY; 4];
    let mut checked = 0usize;
    let mut skipped: Vec<String> = Vec::new();
    let mut overlapping: Vec<String> = Vec::new();
    for g in &w.airports {
        for ri in 0..g.runways.len() {
            if strip_overlaps(g, ri) {
                overlapping.push(format!("{} {}{}", g.ident, g.runways[ri].key, if g.runways[ri].approximate { " (approximate)" } else { "" }));
            }
            for (lat, lon, on_runway, interior) in runway_points(g, ri) {
                let h = t.height(lat, lon);
                let en = frame.enu(lat, lon);
                if near_step(t, g, lat, lon) {
                    skipped.push(format!("{} {}", g.ident, g.runways[ri].key));
                    continue;
                }
                for ring in 0..4usize {
                    let step = p.ground_params(ring).unwrap().step_m as f64;
                    let key = tile_for(lat, lon, 16 - ring as u8);
                    let m = tiles.entry((key, ring)).or_insert_with(|| build_seed_tile_on(key, ring, frame, t, w, p).unwrap().ground);
                    let Some(y) = surface_at(m, frame.tile_center(key), en, None) else {
                        panic!("{} runway {}: no ground at ({lat}, {lon}) in R{ring} ({key})", g.ident, g.runways[ri].key);
                    };
                    let d = y - h;
                    // Drape tolerance inside a fence: 1 cm in R0, half a step in coarser bands.
                    let base = (0.5 * step).max(0.01) + 0.002;
                    let tol = if interior { base } else { base + KINK_M };
                    let top = LEVEL_RUNWAY as f64 * step;
                    assert!(
                        d >= step - tol && d <= top + tol,
                        "{} runway {} R{ring}: drawn ground {y:.3} vs ground_height_at {h:.3} (residual {d:.3} m, allowed {step:.2}..{top:.2})",
                        g.ident,
                        g.runways[ri].key
                    );
                    if on_runway && interior {
                        worst_runway[ring] = worst_runway[ring].max(d);
                        best_runway[ring] = best_runway[ring].min(d);
                    }
                    worst[ring] = worst[ring].max(d);
                    best[ring] = best[ring].min(d);
                    checked += 1;
                }
            }
        }
    }
    eprintln!(
        "residual (drawn - ground_height_at) per ring R0..R3: runway pavement min {best_runway:.4?} max {worst_runway:.4?} m; anywhere in the strips min {best:.4?} max {worst:.4?} m ({checked} checks, {} tiles)",
        tiles.len()
    );
    skipped.dedup();
    eprintln!("points within 30 m of a step of the height function: {skipped:?}");
    eprintln!("runways whose strip overlaps another strip of the airport (one shared plane, checked too): {overlapping:?}");
    assert!(skipped.is_empty(), "the height function steps on a runway: {skipped:?}");
    assert!(!overlapping.is_empty(), "the test world exercises overlapping strips");
    // The terrain mesh over the levelled runways (T0..T2: the bands seen within 25 km).
    let mut terr: HashMap<(TileKey, usize), (MeshData, usize)> = HashMap::new();
    let mut tin_worst = 0.0f64;
    for ident in ["EYVI", "EYKA"] {
        let g = w.airport(ident).expect("airport");
        for (lat, lon, _, interior) in runway_points(g, g.main) {
            let h = t.height(lat, lon);
            let en = frame.enu(lat, lon);
            for ring in 0..3usize {
                let key = tile_for(lat, lon, 14 - ring as u8);
                let (m, n) = terr.entry((key, ring)).or_insert_with(|| {
                    let tt = build_terrain_tile(key, ring, frame, t, p, 1.0).unwrap();
                    let n = tt.surface_triangles as usize;
                    (tt.mesh, n)
                });
                let y = surface_at(m, frame.tile_center(key), en, Some(*n)).expect("terrain under the runway");
                // api 1.7: the terrain mesh is sunk by its own excess (at most its error bound,
                // 1 cm in a fence), so it lies in [h - 2 x bound, h].
                let tol = if interior { 2.0 * TOL_M } else { 2.0 * (TOL_M + KINK_M) };
                if interior {
                    tin_worst = tin_worst.max((y - h).abs());
                }
                assert!(y <= h + 1e-3 && h - y <= tol, "{ident} T{ring} {key}: terrain mesh {y:.3} vs ground_height_at {h:.3}");
            }
        }
    }
    eprintln!("terrain mesh vs ground_height_at on EYVI / EYKA runways (T0..T2): max {tin_worst:.4} m");
}

/// The tiles' airport lights stand on the drawn ground: `seed.airport.lights.height_m` above
/// `ground_height_at` + the pavement level x step inside the fence (no drape margin), + the
/// weighted margin outside it (approach rows), in every band.
#[test]
fn airport_lights_stand_on_the_drawn_runway() {
    let (p, t, w) = test_world();
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let lh = p.seed.airport.lights.height_m;
    for ident in ["EYVI", "EYKA"] {
        let g = w.airport(ident).unwrap();
        let r = &g.runways[g.main];
        let mut keys = std::collections::BTreeSet::new();
        for k in 0..=10 {
            let (lat, lon) = g.frame.ll(r.at(r.length_m * k as f64 / 10.0, 0.0));
            for ring in 0..4usize {
                keys.insert((tile_for(lat, lon, 16 - ring as u8), ring));
            }
        }
        let mut n_in = 0;
        for (key, ring) in keys {
            let step = p.ground_params(ring).unwrap().step_m as f64;
            let lift = t.drape_margin_m(ring) as f64;
            let d = build_seed_tile_on(key, ring, frame, t, w, p).unwrap();
            let c = frame.tile_center(key);
            for q in &d.lights.airport_positions {
                let (lat, lon) = frame.lat_lon(q[0] as f64 + c[0], -(q[2] as f64) + c[1]);
                let above = q[1] as f64 - t.height(lat, lon) - lh;
                let wgt = t.lift_weight(lat, lon);
                let top = if wgt == 0.0 { super::airport::LEVEL_APPROACH as f64 * step } else { 7.0 * step + wgt * lift };
                assert!(above >= -0.012 && above <= top + 0.012, "{ident} {key} R{ring}: light {above:.3} m above ground + {lh} (weight {wgt:.2})");
                if wgt == 0.0 {
                    n_in += 1;
                }
            }
        }
        assert!(n_in > 0, "{ident}: lights inside the fence");
    }
}

/// Outside the fence the drape margin blends back over the shoulder (continuous, no step):
/// along a line from the EYVI fence outward the drawn ground is never under `ground_height_at`
/// (api 1.7: no drape lift, the terrain mesh is sunk instead), at most the weighted margin + its
/// drape slack + the top area level above it, and never jumps between points 5 m apart.
#[test]
fn drape_margin_blends_over_the_shoulder() {
    let (p, t, w) = test_world();
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let g = w.airport("EYVI").unwrap();
    let shoulder = p.seed.airport.shoulder_m;
    // From the fence centre across the long side (+v), past the shoulder.
    let v = g.fence.v();
    for ring in 0..3usize {
        let lift = t.drape_margin_m(ring) as f64;
        let mut prev: Option<(f64, f64)> = None;
        let mut tiles: HashMap<TileKey, MeshData> = HashMap::new();
        let mut d = -50.0;
        while d <= shoulder + 50.0 {
            let o = g.fence.half[1] + d;
            let pt = [g.fence.c[0] + v[0] * o, g.fence.c[1] + v[1] * o];
            let (lat, lon) = g.frame.ll(pt);
            let key = tile_for(lat, lon, 16 - ring as u8);
            let m = tiles.entry(key).or_insert_with(|| build_seed_tile_on(key, ring, frame, t, w, p).unwrap().ground);
            let y = surface_at(m, frame.tile_center(key), frame.enu(lat, lon), None).expect("ground");
            let lifted = y - t.height(lat, lon);
            let wgt = t.lift_weight(lat, lon);
            let step = p.ground_params(ring).unwrap().step_m as f64;
            assert!(lifted >= -0.012, "R{ring} d={d}: the drawn ground {lifted:.3} m under ground_height_at");
            // Vertex offset w x margin + level, plus the drape slack (2 x margin + half the level).
            assert!(lifted <= 3.0 * lift + 10.5 * step + 0.012, "R{ring} d={d}: margin {lifted:.3} too high (weight {wgt:.2})");
            if let Some((pd, pl)) = prev {
                // smoothstep slope <= 1.5 / shoulder, plus the drape slack and the stacking step change at the fence edge.
                let max_jump = lift * 1.5 / shoulder * (d - pd) + 3.0 * lift + 10.5 * step;
                assert!((lifted - pl).abs() <= max_jump, "R{ring}: margin jumps {pl:.3} -> {lifted:.3} between d={pd} and d={d}");
            }
            prev = Some((d, lifted));
            d += 5.0;
        }
    }
}

/// Terrain tiles of the bench's seed spots (coast north of Klaipeda, Kauno marios, EYVI, EYKA)
/// at every terrain zoom 14..10: (name, key, ring).
fn spot_terrain_keys() -> Vec<(&'static str, TileKey, usize)> {
    let spots = [("coast", 55.85, 21.06), ("lake", 54.855, 24.17), ("eyvi", 54.6341, 25.2858), ("eyka", 54.9639, 24.0848)];
    let mut v = Vec::new();
    for (name, lat, lon) in spots {
        for ring in 0..5usize {
            v.push((name, tile_for(lat, lon, 14 - ring as u8), ring));
        }
    }
    v
}

/// (b) No terrain triangle blends water and land: at the Baltic coast and Kauno marios (every
/// terrain band), each surface triangle's three vertex colours are all water or all land, a
/// triangle that had both takes the side of the water test at its centroid, and the fix adds no
/// triangle (only duplicated vertices).
#[test]
fn terrain_colour_does_not_blend_across_the_coast() {
    let (p, t, w) = test_world();
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let mut resolved_total = 0usize;
    for (name, key, ring) in spot_terrain_keys().into_iter().filter(|(n, _, _)| *n == "coast" || *n == "lake") {
        let tile = build_terrain_data(key, ring, frame, t, p, 1.0, Some(w)).unwrap().terrain.unwrap();
        let plain = build_terrain_tile(key, ring, frame, t, p, 1.0).unwrap();
        assert_eq!(tile.surface_triangles, plain.surface_triangles, "{name} {key}: the colour fix adds no triangle");
        let m = &tile.mesh;
        let mut wet_tris = 0;
        for tri in m.indices[..3 * tile.surface_triangles as usize].chunks_exact(3) {
            let wet = [0, 1, 2].map(|k| m.colors[tri[k] as usize][0] == WATER_COLOR_R);
            assert!(wet[0] == wet[1] && wet[1] == wet[2], "{name} {key}: a terrain triangle blends water and land");
            wet_tris += wet[0] as usize;
        }
        // Duplicated vertices = the resolved coast triangles' corners.
        let extra = m.positions.len() - plain.mesh.positions.len();
        eprintln!("{name} {key} T{ring}: {} surface triangles, {wet_tris} water, {extra} duplicated vertices", tile.surface_triangles);
        resolved_total += extra;
        assert!(wet_tris > 0, "{name} {key}: the fixture has water");
    }
    assert!(resolved_total > 0, "the coast fixtures exercise the fix (some vertices were duplicated)");
}

/// Measurement (not a check): surface triangles, vertices and build time of the spot tiles.
/// `cargo test -p simworld-core --release seed_terrain_spot_budget -- --ignored --nocapture`
#[test]
#[ignore]
fn seed_terrain_spot_budget() {
    let (p, t, w) = test_world();
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    eprintln!("spot  key            ring  tris  verts  max_err  ms(min of 3)");
    for (name, key, ring) in spot_terrain_keys() {
        let mut best = f64::INFINITY;
        let mut tile = None;
        for _ in 0..3 {
            let t0 = std::time::Instant::now();
            let d = build_terrain_data(key, ring, frame, t, p, 1.0, Some(w)).unwrap();
            best = best.min(t0.elapsed().as_secs_f64() * 1e3);
            tile = d.terrain;
        }
        let tile = tile.unwrap();
        eprintln!(
            "{name:5} {:14} {ring}  {:6} {:6} {:7.3} {:7.2}",
            key.to_string(),
            tile.surface_triangles,
            tile.mesh.positions.len(),
            tile.max_error,
            best
        );
    }
    let eyvi = (54.6341, 25.2858);
    for ring in 0..4usize {
        let key = tile_for(eyvi.0, eyvi.1, 16 - ring as u8);
        let mut best = f64::INFINITY;
        let mut d = None;
        for _ in 0..3 {
            let t0 = std::time::Instant::now();
            d = Some(build_seed_tile_on(key, ring, frame, t, w, p).unwrap());
            best = best.min(t0.elapsed().as_secs_f64() * 1e3);
        }
        eprintln!("eyvi vector {key} R{ring}: {best:.2} ms, ground {} triangles", d.unwrap().ground.triangle_count());
    }
}


