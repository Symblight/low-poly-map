//! TIN build cost and accuracy per terrain ring (perf review 2, item 3; ignored, release):
//! `cargo test --release -p simworld-core perf_terrain_tin -- --ignored --nocapture`.
//! Six tiles per ring in the hills 35 km from the default origin (default relief, amplitude
//! 450 m): build thread-CPU time (mean / max), of which the height patch, surface triangles, and the drawn
//! surface against the exact height function at 4000 random interior points per tile (largest
//! excess above it, largest deficit below it; the band's `max_error_m` for reference).

use super::super::*;
use super::SurfaceIndex;
use crate::geo::{tile_for, Frame, TileKey};
use crate::profile::Profile;
use crate::terrain::Terrain;

const VILNIUS: (f64, f64) = (54.6872, 25.2797);

#[test]
#[ignore]
fn perf_terrain_tin_build() {
    let mut w = crate::profile::WorldProfile::default();
    w.terrain.enabled = true;
    let p = Profile::new(w).unwrap();
    let t = Terrain::from_profile(&p, VILNIUS.0, VILNIUS.1);
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let mut rng = 0x9E37_79B9_7F4A_7C15u64;
    let mut rnd = || {
        rng = crate::mvt::splitmix64(rng);
        (rng >> 11) as f64 / (1u64 << 53) as f64
    };
    for ring in 0..p.terrain.bands.len() {
        let z = p.terrain.near_zoom as u8 - ring as u8;
        let c = tile_for(VILNIUS.0 + 0.3, VILNIUS.1 + 0.4, z);
        let (mut sum, mut max, mut patch_sum, mut tris) = (0.0f64, 0.0f64, 0.0f64, 0usize);
        let (mut ex, mut def) = (f64::MIN, 0.0f64);
        let keys: Vec<TileKey> = (0..6).map(|i| TileKey::new(z, c.x + i % 3, c.y + i / 3)).collect();
        for &key in &keys {
            let c0 = crate::diag::thread_cpu_us();
            let tile = build_terrain_tile(key, ring, frame, &t, &p, 1.0).unwrap();
            let ms = (crate::diag::thread_cpu_us() - c0) as f64 / 1e3;
            sum += ms;
            max = max.max(ms);
            tris += tile.surface_triangles as usize;
            let c1 = crate::diag::thread_cpu_us();
            let _ = t.patch(key, band_cells(z, &p.terrain.bands[ring]) + 1);
            patch_sum += (crate::diag::thread_cpu_us() - c1) as f64 / 1e3;
            let idx = SurfaceIndex::new(&tile);
            let ctr = frame.tile_center(key);
            for _ in 0..4000 {
                let (lat, lon) = key.frac_to_lat_lon(rnd(), rnd());
                let en = frame.enu(lat, lon);
                if let Some(y) = idx.height(en[0] - ctr[0], -(en[1] - ctr[1])) {
                    let d = y - t.height(lat, lon);
                    ex = ex.max(d);
                    def = def.min(d);
                }
            }
        }
        let n = keys.len() as f64;
        eprintln!(
            "T{ring} z{z}: build mean {:.1} / max {max:.1} ms (height patch {:.1} ms), {:.0} surface triangles; \
             vs the height function: excess max {ex:+.3} m, deficit {def:.3} m (max_error_m {})",
            sum / n,
            patch_sum / n,
            tris as f64 / n,
            p.terrain.bands[ring].max_error_m
        );
    }
}
