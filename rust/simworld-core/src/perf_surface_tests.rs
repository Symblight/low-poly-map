//! Worker cost of engine surfaces (perf review 3 item 1, ignored; release, `--nocapture`):
//! `cargo test --release -p simworld-core perf_surface -- --ignored --nocapture --test-threads=1`.
//!
//! `perf_surface_encode`: per band, the thread CPU of building a chunk (decode excluded) and of
//! packing its building and ground meshes (`MeshData::pack_engine_surface`), best of 3, around the
//! Vilnius origin: R0 / R1 chunks of the detail z14 tile, R2 / R3 chunks of the overview z11
//! tile (`SIMWORLD_V2_ARCHIVE`, else the synthetic city), and the terrain tiles of every terrain
//! band (TIN build vs packing).

use std::sync::Arc;

use crate::data::{build_chunk, decode_data_tile, decode_overview_tile, synthetic_data_tile, DataLevel, DataTile};
use crate::diag::thread_cpu_us;
use crate::geo::{tile_for, Frame, TileKey};
use crate::mesh_data::MeshData;
use crate::profile::{Profile, WorldProfile};
use crate::terrain::Terrain;

const VILNIUS: (f64, f64) = (54.6872, 25.2797);

/// Best-of-3 thread CPU (ms) of `f`, and its last result.
fn best<T>(mut f: impl FnMut() -> T) -> (f64, T) {
    let mut out = None;
    let mut ms = f64::INFINITY;
    for _ in 0..3 {
        let c = thread_cpu_us();
        let r = f();
        ms = ms.min((thread_cpu_us() - c) as f64 / 1e3);
        out = Some(r);
    }
    (ms, out.expect("ran"))
}

fn pack_ms(m: &MeshData) -> f64 {
    best(|| {
        let mut c = m.clone();
        let c0 = thread_cpu_us();
        c.pack_engine_surface();
        (thread_cpu_us() - c0) as f64 / 1e3
    })
    .1
}

/// Per band: (tiles, vertices, build ms, pack ms) summed.
#[derive(Default)]
struct Row {
    tiles: usize,
    verts: usize,
    build: f64,
    pack: f64,
    pack_max: f64,
}

impl Row {
    fn line(&self, label: &str) -> String {
        let n = self.tiles.max(1) as f64;
        format!(
            "{label}: {} tiles, {:.0} vertices / tile, build {:.2} ms / tile, pack {:.3} ms / tile (max {:.3}), pack = {:.1} % of build",
            self.tiles,
            self.verts as f64 / n,
            self.build / n,
            self.pack / n,
            self.pack_max,
            100.0 * self.pack / self.build.max(1e-9)
        )
    }
}

fn data_tile(key: TileKey, level: DataLevel, frame: Frame, terrain: &Terrain, profile: &Profile, f: Option<&crate::fetch::Fetcher>, rt: &tokio::runtime::Runtime) -> DataTile {
    match f {
        Some(f) => {
            let (bytes, _) = rt.block_on(f.fetch(key)).expect("fetch");
            match level {
                DataLevel::Overview => decode_overview_tile(key, frame, terrain, &bytes, profile),
                _ => decode_data_tile(key, frame, terrain, &bytes, profile),
            }
            .expect("decode")
        }
        None => synthetic_data_tile(key, level, frame, terrain, profile),
    }
}

#[test]
#[ignore]
fn perf_surface_encode() {
    let mut w = WorldProfile::default();
    w.terrain.enabled = true;
    let profile = Profile::new(w).unwrap();
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let terrain = Terrain::from_profile(&profile, VILNIUS.0, VILNIUS.1);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let fetcher = std::env::var("SIMWORLD_V2_ARCHIVE").ok().map(|p| {
        let (url, _) = crate::data_world_tests::serve(std::fs::read(p).expect("v2 archive"));
        crate::fetch::Fetcher::new(&url, "", Arc::new(std::sync::atomic::AtomicU64::new(0)), crate::fetch::FetchParams::default(), Box::new(|_| {})).unwrap()
    });
    let name = if fetcher.is_some() { "v2" } else { "synthetic" };
    let detail = tile_for(VILNIUS.0, VILNIUS.1, 14);
    let overview = tile_for(VILNIUS.0, VILNIUS.1, 11);
    let dt = data_tile(detail, DataLevel::Detail, frame, &terrain, &profile, fetcher.as_ref(), &rt);
    let ot = data_tile(overview, DataLevel::Overview, frame, &terrain, &profile, fetcher.as_ref(), &rt);
    let mut rows: [Row; 4] = Default::default();
    for (ring, src, zoom) in [(0usize, &dt, 16u8), (1, &dt, 15), (2, &ot, 14), (3, &ot, 13)] {
        let shift = zoom - src.key.z;
        let (x0, y0) = (src.key.x << shift, src.key.y << shift);
        let side = 1u32 << shift;
        // At most 16 chunks per band, from the data tile's centre.
        let start = side.saturating_sub(4) / 2;
        for dy in 0..side.min(4) {
            for dx in 0..side.min(4) {
                let k = TileKey::new(zoom, x0 + start + dx, y0 + start + dy);
                let (ms, d) = best(|| build_chunk(src, k, ring, &profile, &|| false).unwrap().unwrap());
                let r = &mut rows[ring];
                r.tiles += 1;
                r.build += ms;
                r.verts += d.mesh.vertex_count() + d.ground.vertex_count();
                let p = pack_ms(&d.mesh) + pack_ms(&d.ground);
                r.pack += p;
                r.pack_max = r.pack_max.max(p);
            }
        }
    }
    for (ring, r) in rows.iter().enumerate() {
        eprintln!("[{name}] R{ring} {}", r.line("building + ground"));
    }
    let mut t = Row::default();
    for ring in 0..profile.terrain.bands.len() {
        let z = profile.terrain.near_zoom as u8 - ring as u8;
        let k = tile_for(VILNIUS.0 + 0.05, VILNIUS.1, z);
        let (ms, tile) = best(|| crate::terrain::tin::build_terrain_tile(k, ring, frame, &terrain, &profile, 1.0).unwrap());
        t.tiles += 1;
        t.build += ms;
        t.verts += tile.mesh.vertex_count();
        let p = pack_ms(&tile.mesh);
        t.pack += p;
        t.pack_max = t.pack_max.max(p);
    }
    eprintln!("[{name}] {}", t.line("terrain (all bands)"));
}
