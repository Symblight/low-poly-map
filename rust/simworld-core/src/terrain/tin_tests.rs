//! TIN tests (docs/spec-terrain.md section 11): Delatin on analytic fields, triangle cap,
//! skirts, winding, determinism.

use super::*;
use crate::geo::{tile_for, Frame, TileKey};
use crate::geo::MERCATOR_CIRCUMFERENCE;
use crate::mesher::MeshData;
use crate::profile::{Profile, TerrainProfile};
use crate::terrain::sample::lattice_lat_lon;
use crate::terrain::Terrain;

const VILNIUS: (f64, f64) = (54.6872, 25.2797);

fn grid(n: usize, f: impl Fn(f64, f64) -> f64) -> Vec<f32> {
    let mut v = Vec::with_capacity(n * n);
    for y in 0..n {
        for x in 0..n {
            v.push(f(x as f64, y as f64) as f32);
        }
    }
    v
}

/// Largest |TIN - grid| over every grid point, by barycentric interpolation in each triangle
/// (independent of Delatin's own rasterizer). Also checks orientation and that the triangles
/// tile the grid rectangle exactly (area sum).
fn true_error(h: &[f32], n: usize, d: &Delatin<'_>) -> f64 {
    let c = d.coords();
    let mut area2 = 0i64;
    let mut worst = 0.0f64;
    for t in d.triangles().as_chunks::<3>().0 {
        let (a, b, p) = (c[t[0] as usize], c[t[1] as usize], c[t[2] as usize]);
        let o = orient(a, b, p);
        assert!(o > 0, "triangle {t:?} not positively oriented");
        area2 += o;
        let z = |q: [i64; 2]| h[q[1] as usize * n + q[0] as usize] as f64;
        let (x0, x1) = (a[0].min(b[0]).min(p[0]), a[0].max(b[0]).max(p[0]));
        let (y0, y1) = (a[1].min(b[1]).min(p[1]), a[1].max(b[1]).max(p[1]));
        for y in y0..=y1 {
            for x in x0..=x1 {
                let q = [x, y];
                let (w0, w1, w2) = (orient(b, p, q), orient(p, a, q), orient(a, b, q));
                if w0 < 0 || w1 < 0 || w2 < 0 {
                    continue;
                }
                let zi = (z(a) * w0 as f64 + z(b) * w1 as f64 + z(p) * w2 as f64) / o as f64;
                worst = worst.max((zi - z(q)).abs());
            }
        }
    }
    assert_eq!(area2, 2 * ((n - 1) * (n - 1)) as i64, "triangles do not tile the grid");
    worst
}

#[test]
fn plane_is_two_triangles() {
    let n = 65;
    let h = grid(n, |x, y| 12.0 + 0.3 * x - 0.7 * y);
    let mut d = Delatin::new(&h, n, n, &[0, n - 1], &[0, n - 1]);
    let e = d.run(0.01, 10_000);
    assert_eq!(d.triangle_count(), 2);
    assert_eq!(d.coords().len(), 4);
    assert!(e < 1e-4, "{e}");
    assert!(true_error(&h, n, &d) < 1e-4);
}

#[test]
fn plane_with_seeds_is_exactly_the_seed_grid() {
    let n = 129;
    let h = grid(n, |x, y| -4.0 + 0.05 * x + 0.02 * y);
    for s in [3usize, 5, 9] {
        let seeds = seed_line(s, n - 1);
        let mut d = Delatin::new(&h, n, n, &seeds, &seeds);
        d.run(0.01, 100_000);
        assert_eq!(d.triangle_count(), 2 * (s - 1) * (s - 1), "seeds {s}");
        assert_eq!(d.coords().len(), s * s, "seeds {s}: nothing but the seeds");
        assert!(true_error(&h, n, &d) < 1e-4);
    }
    // Seed counts from tile width and max_edge_m; the cap bounds them.
    assert_eq!(seed_count(22_600.0, 5000.0, 256, 8192), 6);
    assert_eq!(seed_count(22_600.0, 0.0, 256, 8192), 2);
    assert_eq!(seed_count(22_600.0, 500.0, 256, 100), 8, "2 * 7^2 = 98 <= 100");
    assert_eq!(seed_count(1000.0, 500.0, 2, 8192), 3, "never more seeds than samples");
}

#[test]
fn sphere_cap_respects_the_error_bound() {
    let n = 129;
    let r = 150.0;
    let cap = |x: f64, y: f64| {
        let (dx, dy) = (x - 64.0, y - 64.0);
        let q = r * r - dx * dx - dy * dy;
        if q > 0.0 {
            q.sqrt() - 100.0
        } else {
            0.0
        }
        .max(0.0)
    };
    let h = grid(n, cap);
    for max_error in [0.25, 1.0, 4.0] {
        let mut d = Delatin::new(&h, n, n, &[0, n - 1], &[0, n - 1]);
        let left = d.run(max_error, 1_000_000);
        let worst = true_error(&h, n, &d);
        assert!(left <= max_error && worst <= max_error + 1e-3, "bound {max_error}: reported {left}, true {worst}");
        eprintln!("sphere cap, error {max_error}: {} triangles, true error {worst:.3}", d.triangle_count());
    }
    // Tighter bound, more triangles.
    let tri = |e: f64| {
        let mut d = Delatin::new(&h, n, n, &[0, n - 1], &[0, n - 1]);
        d.run(e, 1_000_000);
        d.triangle_count()
    };
    assert!(tri(0.25) > tri(1.0) && tri(1.0) > tri(4.0));
}

#[test]
fn triangle_cap_is_respected() {
    let n = 129;
    let h = grid(n, |x, y| 40.0 * ((x * 0.37).sin() * (y * 0.23).cos()) + 7.0 * (x * y * 0.01).sin());
    for cap in [2usize, 3, 50, 501, 4096] {
        let mut d = Delatin::new(&h, n, n, &[0, n - 1], &[0, n - 1]);
        let left = d.run(0.05, cap);
        assert!(d.triangle_count() <= cap, "cap {cap}: {}", d.triangle_count());
        assert!(d.triangle_count() + 2 > cap || left <= 0.05, "cap {cap}: stopped early");
        true_error(&h, n, &d);
    }
}

fn terrain_profile(t: TerrainProfile) -> (Profile, Terrain) {
    let w = crate::profile::WorldProfile { terrain: TerrainProfile { enabled: true, ..t }, ..Default::default() };
    let p = Profile::new(w).unwrap();
    let terrain = Terrain::from_profile(&p, VILNIUS.0, VILNIUS.1);
    (p, terrain)
}

fn hilly() -> (Profile, Terrain) {
    terrain_profile(TerrainProfile { amplitude_m: 160.0, flat_radius_m: 0.0, flat_blend_m: 0.0, ..TerrainProfile::default() })
}

/// The z13 tile 2 tiles east of the origin's (relief, no flat zone).
fn sample_keys() -> Vec<(TileKey, usize)> {
    (10u8..=14)
        .map(|z| {
            let k = tile_for(VILNIUS.0, VILNIUS.1, z);
            (TileKey::new(z, k.x + 1, k.y), (14 - z) as usize)
        })
        .collect()
}

#[test]
fn terrain_tiles_wind_clockwise_and_stay_inside_their_tile() {
    let (p, t) = hilly();
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    for (k, ring) in sample_keys() {
        let tile = build_terrain_tile(k, ring, frame, &t, &p, 1.0).unwrap();
        let m = &tile.mesh;
        assert!(m.normals.is_empty() && m.colors.is_empty() && m.uvs.is_empty(), "flat shading: VERTEX + INDEX only");
        assert!(tile.surface_triangles >= 2 && tile.surface_triangles as usize <= p.terrain.bands[ring].max_triangles as usize);
        // Surface triangles face up (Godot front face clockwise from above).
        for tri in m.indices[..tile.surface_triangles as usize * 3].as_chunks::<3>().0 {
            let (a, b, c) = (m.positions[tri[0] as usize], m.positions[tri[1] as usize], m.positions[tri[2] as usize]);
            let ny = (c[2] - a[2]) as f64 * (b[0] - a[0]) as f64 - (c[0] - a[0]) as f64 * (b[2] - a[2]) as f64;
            assert!(ny > 0.0, "{k}: surface triangle faces down");
        }
        // Every vertex inside the tile rectangle (ENU, relative to the centre).
        let r = frame.tile_rect(k);
        let c = frame.tile_center(k);
        for v in &m.positions {
            let (e, n) = (v[0] as f64 + c[0], -v[2] as f64 + c[1]);
            assert!(e >= r.min_e - 0.05 && e <= r.max_e + 0.05 && n >= r.min_n - 0.05 && n <= r.max_n + 0.05, "{k}: vertex outside");
        }
        assert!(tile.h_min <= tile.h_max);
        eprintln!(
            "{k} ring {ring}: {} surface + {} skirt triangles, {} vertices, error left {:.2} m, skirt {:.1} m, h {:.0}..{:.0}",
            tile.surface_triangles,
            m.triangle_count() - tile.surface_triangles as usize,
            m.vertex_count(),
            tile.max_error,
            tile.skirt_m,
            tile.h_min,
            tile.h_max
        );
    }
}

/// Skirts: one bottom copy per hull vertex, `skirt` metres below it; each hull edge closed by a
/// quad facing away from the tile centre; the bottom ring is closed (each bottom vertex in
/// exactly two quads).
#[test]
fn skirts_are_closed_and_face_outwards() {
    let (p, t) = hilly();
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    for (k, ring) in sample_keys() {
        let tile = build_terrain_tile(k, ring, frame, &t, &p, 1.0).unwrap();
        let m = &tile.mesh;
        let surf = tile.surface_triangles as usize;
        let skirt = &m.indices[surf * 3..];
        assert_eq!(skirt.len() % 6, 0);
        let quads = skirt.len() / 6;
        // Bottom vertices are the ones after the surface vertices.
        let first_bottom = m.indices[..surf * 3].iter().map(|&i| i as usize).max().unwrap() + 1;
        let bottoms = m.vertex_count() - first_bottom;
        assert_eq!(quads, bottoms, "{k}: a closed ring has as many edges as vertices");
        let mut uses = vec![0usize; bottoms];
        for q in skirt.as_chunks::<6>().0 {
            let mut seen = std::collections::BTreeSet::new();
            for &i in q {
                if (i as usize) >= first_bottom {
                    seen.insert(i as usize - first_bottom);
                }
            }
            assert_eq!(seen.len(), 2, "{k}: a quad has two bottom vertices");
            for s in seen {
                uses[s] += 1;
            }
            for tri in q.as_chunks::<3>().0 {
                let (a, b, c) = (m.positions[tri[0] as usize], m.positions[tri[1] as usize], m.positions[tri[2] as usize]);
                let u = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
                let v = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                let g = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
                let mid = [(a[0] + b[0] + c[0]) / 3.0, (a[2] + b[2] + c[2]) / 3.0];
                assert!(g[0] * mid[0] + g[2] * mid[1] > 0.0, "{k}: skirt faces inwards");
                assert!(g[1].abs() < 1e-3 * (g[0].abs() + g[2].abs()), "{k}: skirt not vertical");
            }
        }
        assert!(uses.iter().all(|&u| u == 2), "{k}: bottom ring not closed: {uses:?}");
        // Depth: automatic 2 * max_error + 1, sized for the next coarser band (api 1.7: the sunk
        // edges of a tile lie up to 2 x error under the height function).
        let bands = &p.terrain.bands;
        let e = bands[ring].max_error_m.max(bands.get(ring + 1).map_or(0.0, |b| b.max_error_m));
        let want = (2.0 * e + 1.0).min(SKIRT_MAX_M) as f32;
        assert_eq!(tile.skirt_m, want);
    }
    // An explicit depth wins.
    let (p2, t2) = terrain_profile(TerrainProfile { skirt_m: 33.0, ..TerrainProfile::default() });
    let (k, ring) = sample_keys()[2];
    assert_eq!(build_terrain_tile(k, ring, frame, &t2, &p2, 1.0).unwrap().skirt_m, 33.0);
}

#[test]
fn terrain_tiles_are_deterministic_and_follow_the_detail_scale() {
    let (p, t) = hilly();
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let (k, ring) = sample_keys()[1]; // z13
    let a = build_terrain_tile(k, ring, frame, &t, &p, 1.0).unwrap();
    let b = std::thread::spawn(move || build_terrain_tile(k, ring, frame, &t, &p, 1.0).unwrap()).join().unwrap();
    assert_eq!(a, b, "two builds (two threads) are identical");
    let (p, t) = hilly();
    let coarse = build_terrain_tile(k, ring, frame, &t, &p, 2.0).unwrap();
    let fine = build_terrain_tile(k, ring, frame, &t, &p, 0.25).unwrap();
    assert!(fine.surface_triangles > a.surface_triangles && a.surface_triangles > coarse.surface_triangles);
    assert!(coarse.max_error <= 2.0 * p.terrain.bands[ring].max_error_m as f32 + 1e-6);
}

/// Flat land: corners only without seeds, exactly the seed grid with `max_edge_m`.
#[test]
fn flat_land_is_corners_or_the_seed_grid() {
    let (p, t) = terrain_profile(TerrainProfile { amplitude_m: 0.0, ..TerrainProfile::default() });
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    for (k, ring) in sample_keys() {
        let tile = build_terrain_tile(k, ring, frame, &t, &p, 1.0).unwrap();
        let band = &p.terrain.bands[ring];
        let (lat, _) = k.center_lat_lon();
        let w = MERCATOR_CIRCUMFERENCE * lat.to_radians().cos() / (1u64 << k.z) as f64;
        let s = seed_count(w, band.max_edge_m, band_cells(k.z, band), band.max_triangles as usize);
        assert_eq!(tile.surface_triangles as usize, 2 * (s - 1) * (s - 1), "{k}");
        assert_eq!(tile.h_min, tile.h_max);
    }
}

#[test]
fn smooth_shading_adds_unit_normals() {
    let (p, t) = terrain_profile(TerrainProfile {
        shading: crate::profile::TerrainShading::Smooth,
        flat_radius_m: 0.0,
        flat_blend_m: 0.0,
        ..TerrainProfile::default()
    });
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let (k, ring) = sample_keys()[1];
    let tile = build_terrain_tile(k, ring, frame, &t, &p, 1.0).unwrap();
    let m = &tile.mesh;
    assert_eq!(m.normals.len(), m.positions.len());
    for n in &m.normals {
        let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        assert!((l - 1.0).abs() < 1e-4 && n[1] > 0.0, "{n:?}");
    }
}

#[test]
fn patch_resolution_per_zoom() {
    assert_eq!([10u8, 11, 12, 13, 14, 15, 16].map(patch_cells), [256, 256, 256, 128, 64, 32, 16]);
    assert_eq!(patch_cells(8), 256);
}

/// Surface height of a terrain tile at tile-local (x, z) meters: barycentric in the surface
/// triangle that contains the point (a bucket grid over the surface triangles).
pub(crate) struct SurfaceIndex<'a> {
    m: &'a MeshData,
    tris: usize,
    lo: [f32; 2],
    cell: [f32; 2],
    buckets: Vec<Vec<u32>>,
}

pub(crate) const SURFACE_BUCKETS: usize = 48;

impl<'a> SurfaceIndex<'a> {
    pub(crate) fn new(tile: &'a TerrainTile) -> Self {
        let m = &tile.mesh;
        let tris = tile.surface_triangles as usize;
        let n = SURFACE_BUCKETS;
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for t in 0..tris {
            for k in 0..3 {
                let p = m.positions[m.indices[t * 3 + k] as usize];
                lo = [lo[0].min(p[0]), lo[1].min(p[2])];
                hi = [hi[0].max(p[0]), hi[1].max(p[2])];
            }
        }
        let cell = [(hi[0] - lo[0]).max(1e-3) / n as f32, (hi[1] - lo[1]).max(1e-3) / n as f32];
        let mut buckets = vec![Vec::new(); n * n];
        let b = |v: f32, l: f32, c: f32| (((v - l) / c) as usize).min(n - 1);
        for t in 0..tris {
            let ps: Vec<[f32; 3]> = (0..3).map(|k| m.positions[m.indices[t * 3 + k] as usize]).collect();
            let (x0, x1) = (ps.iter().map(|p| p[0]).fold(f32::MAX, f32::min), ps.iter().map(|p| p[0]).fold(f32::MIN, f32::max));
            let (z0, z1) = (ps.iter().map(|p| p[2]).fold(f32::MAX, f32::min), ps.iter().map(|p| p[2]).fold(f32::MIN, f32::max));
            for j in b(z0, lo[1], cell[1])..=b(z1, lo[1], cell[1]) {
                for i in b(x0, lo[0], cell[0])..=b(x1, lo[0], cell[0]) {
                    buckets[j * n + i].push(t as u32);
                }
            }
        }
        Self { m, tris, lo, cell, buckets }
    }

    pub(crate) fn height(&self, x: f64, z: f64) -> Option<f64> {
        let n = SURFACE_BUCKETS;
        let i = ((x as f32 - self.lo[0]) / self.cell[0]).floor();
        let j = ((z as f32 - self.lo[1]) / self.cell[1]).floor();
        if i < 0.0 || j < 0.0 || i as usize >= n || j as usize >= n {
            return None;
        }
        let m = self.m;
        for &t in &self.buckets[j as usize * n + i as usize] {
            let t = t as usize;
            debug_assert!(t < self.tris);
            let p: Vec<[f64; 3]> = (0..3).map(|k| m.positions[m.indices[t * 3 + k] as usize].map(f64::from)).collect();
            let d = (p[1][2] - p[2][2]) * (p[0][0] - p[2][0]) + (p[2][0] - p[1][0]) * (p[0][2] - p[2][2]);
            if d.abs() < 1e-12 {
                continue;
            }
            let l1 = ((p[1][2] - p[2][2]) * (x - p[2][0]) + (p[2][0] - p[1][0]) * (z - p[2][2])) / d;
            let l2 = ((p[2][2] - p[0][2]) * (x - p[2][0]) + (p[0][0] - p[2][0]) * (z - p[2][2])) / d;
            let l3 = 1.0 - l1 - l2;
            let eps = -1e-7;
            if l1 >= eps && l2 >= eps && l3 >= eps {
                return Some(l1 * p[0][1] + l2 * p[1][1] + l3 * p[2][1]);
            }
        }
        None
    }
}

/// The sunk terrain mesh (api 1.7, contract 9a) never lies above the height function: at every
/// lattice sample of the tile (the TIN's own input) by more than the projection slack, at every
/// terrain band and detail, and never more than twice the error bound under it (a sink is
/// at most the TIN's error, `Delatin::vertex_sinks`); between the samples (random interior
/// points) only by the sub-lattice curvature of the function (reported, not asserted). Reports
/// the largest excess and deficit per band (default relief, amplitude 450 m, no flat zone).
#[test]
fn sunk_tin_is_never_above_the_height_function() {
    let (p, t) = terrain_profile(TerrainProfile { flat_radius_m: 0.0, flat_blend_m: 0.0, ..TerrainProfile::default() });
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let mut rng = 0x9E37_79B9_7F4A_7C15u64;
    let mut rnd = || {
        rng = crate::mvt::splitmix64(rng);
        (rng >> 11) as f64 / (1u64 << 53) as f64
    };
    for detail in [1.0, 2.0] {
        for ring in 0..p.terrain.bands.len() {
            let z = p.terrain.near_zoom as u8 - ring as u8;
            let c = tile_for(VILNIUS.0 + 0.1, VILNIUS.1 + 0.15, z);
            let (mut lat_ex, mut lat_def, mut sub_ex, mut sub_def) = (f64::MIN, 0.0f64, f64::MIN, 0.0f64);
            for key in [TileKey::new(z, c.x, c.y), TileKey::new(z, c.x + 1, c.y)] {
                let tile = build_terrain_tile(key, ring, frame, &t, &p, detail).unwrap();
                let idx = SurfaceIndex::new(&tile);
                let ctr = frame.tile_center(key);
                let cells = band_cells(z, &p.terrain.bands[ring]);
                let step = (cells / 64).max(1);
                for j in (0..=cells).step_by(step) {
                    for i in (0..=cells).step_by(step) {
                        let (lat, lon) = lattice_lat_lon(key, cells as u32, i as i64, j as i64);
                        let en = frame.enu(lat, lon);
                        if let Some(y) = idx.height(en[0] - ctr[0], -(en[1] - ctr[1])) {
                            let d = y - t.height(lat, lon);
                            lat_ex = lat_ex.max(d);
                            lat_def = lat_def.min(d);
                        }
                    }
                }
                for _ in 0..4000 {
                    let (lat, lon) = key.frac_to_lat_lon(rnd(), rnd());
                    let en = frame.enu(lat, lon);
                    if let Some(y) = idx.height(en[0] - ctr[0], -(en[1] - ctr[1])) {
                        let d = y - t.height(lat, lon);
                        sub_ex = sub_ex.max(d);
                        sub_def = sub_def.min(d);
                    }
                }
            }
            let bound = p.terrain.bands[ring].max_error_m * detail;
            let width_m = crate::geo::MERCATOR_CIRCUMFERENCE * VILNIUS.0.to_radians().cos() / (1u64 << z) as f64;
            // The sink holds in lattice (Web Mercator) space; the mesh is linear in ENU metres, where
            // a sample sits at slightly other barycentric weights (the projection bends over a
            // triangle of several km, plus f32 x / z): measured 20.3 mm at z10 (128 cells, larger
            // triangles; 7.6 mm with 256), 2.8 mm at z12. Slack: 2 mm + 1.2 ppm of the tile width
            // (29 mm at z10, 9 mm at z12).
            let f32_m = 2e-3 + 1.2e-6 * width_m;
            eprintln!(
                "T{ring} z{z} detail {detail}: lattice samples excess max {lat_ex:+.4} m, deficit {lat_def:.3} m; random interior points excess max {sub_ex:+.4} m, deficit {sub_def:.3} m (error bound {bound} m)"
            );
            assert!(lat_ex <= f32_m, "T{ring}: the sunk TIN is {lat_ex} m above a lattice sample");
            assert!(lat_def >= -2.0 * bound - f32_m, "T{ring}: the sunk TIN is {lat_def} m below a lattice sample");
        }
    }
}

#[path = "tin_perf_tests.rs"]
mod perf;
