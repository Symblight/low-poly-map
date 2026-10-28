//! Data tiles (Tiles and LOD v2, step T2a): the unit of download and decode is no longer the
//! render chunk. A data tile is fetched and decoded once ([`DataTile`]: buildings, roads,
//! landuse and the light inputs as plain Rust structs); the render chunks z/x/y the host sees are
//! cut from it in memory ([`build_chunk`]), as many as need it, without another fetch or decode.
//!
//! Coordinates: a data tile keeps its geometry in its own pixel space (exact Web Mercator, y
//! down, one common extent for every layer, f64). That space is tile independent in the sense
//! that matters here: a chunk is an axis-aligned rectangle in it (a power-of-two fraction of the
//! extent), so clipping a building to a chunk is exact and the 4 children of a chunk partition it
//! bit for bit, as the per-tile clipping did before. Projection to ENU happens per chunk, relative
//! to the chunk center ([`crate::mvt::TileProjector::cut`]).
//!
//! Ownership: geometry (buildings, ground areas, roads) is clipped exactly to the chunk rectangle
//! as today; per-building outputs (the light point, beacons) belong to the one chunk whose
//! half-open rectangle contains the building's centroid, so the 4 siblings never double a light;
//! street lamps belong to the chunk containing the lamp.
//!
//! Levels: a two-level archive (metadata `simworld_levels`, built by `simworld-cli bake`) is
//! read in step T2b. An archive without that key (today's z13..z16 `vilnius.pmtiles`) is read
//! through the compatibility levels [`DataLevels::COMPAT`]: detail = z14 feeds R0 (z16) and R1
//! (z15); R2 and R3 keep their own z14 / z13 tiles exactly as before.

use std::sync::Arc;
use std::time::Instant;

use crate::geo::{Frame, TileKey};
use crate::lights::LightSet;
use crate::mesher::mesh_building_iter;
use crate::ground::mesh_ground;
use crate::mvt::{decode_tile_on, Building, GroundFeatures, TileProjector};
use crate::profile::Profile;
use crate::terrain::Terrain;
use crate::worker::TileData;

/// The data levels of a tile source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DataLevels {
    /// Zoom of the detail data tiles (every footprint); chunks finer than it are cut from it.
    pub detail: u8,
    /// Zoom of the overview data tiles (T2b: R2 / R3; unused by the compatibility path).
    pub overview: u8,
}

impl DataLevels {
    /// An archive without `simworld_levels` metadata (zooms 13..16): detail z14, overview z13.
    pub const COMPAT: Self = Self { detail: 14, overview: 13 };

    /// The detail data tile a chunk is cut from, or `None` when the chunk keeps its own tile
    /// (step T2a: chunks finer than the detail zoom, i.e. R0 z16 and R1 z15).
    pub fn detail_of(&self, chunk: TileKey) -> Option<TileKey> {
        if chunk.z <= self.detail {
            return None;
        }
        let dz = chunk.z - self.detail;
        Some(TileKey::new(self.detail, chunk.x >> dz, chunk.y >> dz))
    }
}

/// One decoded data tile, shared (`Arc`) by every chunk job that is cut from it.
#[derive(Debug)]
pub struct DataTile {
    pub key: TileKey,
    /// Every building of the tile (incl. the encoder's buffer copies), decode order (sorted).
    /// `centroid` / `owned` refer to the whole data tile; chunks use
    /// [`TileProjector::place`] instead.
    pub buildings: Vec<Building>,
    /// Pixel bounding box `[x0, y0, x1, y1]` of each building's outer rings (chunk candidates).
    pub bboxes: Vec<[f64; 4]>,
    /// Ground areas and roads (always decoded: every band that is cut from the tile may need them).
    pub ground: GroundFeatures,
    /// Projector of the whole data tile (pixel space, terrain).
    pub proj: TileProjector,
    /// Estimated heap bytes (the memory budget's item size).
    pub bytes: u64,
    /// Worker CPU time of the decode (microseconds).
    pub cpu_us: u64,
}

fn bbox(b: &Building) -> [f64; 4] {
    let mut r = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for p in b.polygons.iter().filter_map(|p| p.first()).flatten() {
        r[0] = r[0].min(p[0]);
        r[1] = r[1].min(p[1]);
        r[2] = r[2].max(p[0]);
        r[3] = r[3].max(p[1]);
    }
    r
}

/// Heap estimate of a decoded tile (points dominate; a fixed overhead per feature).
fn estimate_bytes(buildings: &[Building], ground: &GroundFeatures) -> u64 {
    const PT: u64 = 16;
    const FEATURE: u64 = 160;
    let b: u64 = buildings
        .iter()
        .map(|b| FEATURE + 32 + b.kind.len() as u64 + b.polygons.iter().flatten().map(|r| r.len() as u64 * PT + 24).sum::<u64>())
        .sum();
    let a: u64 = ground.areas.iter().map(|a| FEATURE + a.rings.iter().map(|r| r.len() as u64 * PT + 24).sum::<u64>()).sum();
    let r: u64 = ground.roads.iter().map(|r| FEATURE + r.lines.iter().map(|l| l.len() as u64 * PT + 24).sum::<u64>()).sum();
    256 + b + a + r
}

/// Decode a data tile (gzip or raw MVT; empty bytes = an empty tile) on `terrain`: buildings,
/// ground areas and roads, every layer in one pixel space (per-layer extents are scaled to the
/// largest, [`crate::mvt::decode_tile_on`]).
pub fn decode_data_tile(key: TileKey, frame: Frame, terrain: &Terrain, bytes: &[u8], profile: &Profile) -> Result<DataTile, String> {
    let t0 = Instant::now();
    let d = decode_tile_on(bytes, key, frame, terrain, true, true, profile)?;
    let bboxes = d.buildings.iter().map(bbox).collect();
    let bytes = estimate_bytes(&d.buildings, &d.ground);
    Ok(DataTile { key, buildings: d.buildings, bboxes, ground: d.ground, proj: d.proj, bytes, cpu_us: t0.elapsed().as_micros() as u64 })
}

/// The render chunk `chunk` (band `ring`) cut from data tile `dt`: building mesh, ground mesh and
/// lights exactly as [`crate::worker::build_tile`] makes them for a tile of its own, with the
/// geometry clipped to the chunk rectangle and per-building lights owned by centroid (module
/// docs). `stop` is checked between the stages; `Ok(None)` = stopped.
pub fn build_chunk(dt: &DataTile, chunk: TileKey, ring: usize, profile: &Profile, stop: &dyn Fn() -> bool) -> Result<Option<TileData>, String> {
    let t0 = Instant::now();
    let proj = dt.proj.cut(chunk).ok_or_else(|| format!("chunk {chunk} is not inside data tile {}", dt.key))?;
    let r = proj.rect();
    let sel: Vec<&Building> = dt
        .buildings
        .iter()
        .zip(&dt.bboxes)
        .filter(|(_, bb)| !(bb[2] <= r.x0 || bb[0] >= r.x1 || bb[3] <= r.y0 || bb[1] >= r.y1))
        .map(|(b, _)| b)
        .collect();
    if stop() {
        return Ok(None);
    }
    let mesh = match profile.lod(ring) {
        Some(p) => mesh_building_iter(sel.iter().copied(), &proj, r, p, profile),
        None => Default::default(),
    };
    if stop() {
        return Ok(None);
    }
    let ground = mesh_ground(&dt.ground, &proj, ring, profile);
    if stop() {
        return Ok(None);
    }
    let mut lights =
        if ring >= profile.lights.min_ring as usize { LightSet::from_buildings_in(dt.buildings.iter(), &proj, profile) } else { LightSet::default() };
    if ring >= profile.lights.street.min_ring as usize {
        lights.set_streets(&dt.ground.roads, &proj, profile);
    }
    if ring >= profile.lights.beacons.min_ring as usize {
        lights.set_beacons_in(dt.buildings.iter(), &proj, profile);
    }
    let max_height = sel.iter().map(|b| crate::mesher::resolve(b, profile).height).fold(0.0f32, f32::max);
    Ok(Some(TileData {
        building_count: sel.len(),
        lights: Arc::new(lights),
        mesh,
        ground,
        max_height,
        cpu_us: t0.elapsed().as_micros() as u64,
        terrain: None,
    }))
}

#[cfg(test)]
pub(crate) mod fixture {
    //! A small synthetic archive area for the data-tile tests: one z14 tile with buildings
    //! (grid, chunk-edge crossers, a jagged outline, a hole, a tower), landuse areas and roads,
    //! in z14 pixels, and the same features re-expressed in the pixels of any tile inside it
    //! (what an archive's z15 / z16 tiles hold for the same area, unclipped).
    use crate::geo::{tile_for, TileKey};
    use crate::mvt::test_mvt::{encode_layer_ext, Feat, Val};

    pub const VILNIUS: (f64, f64) = (54.6872, 25.2797);

    /// The z14 tile under the Vilnius origin.
    pub fn key14() -> TileKey {
        tile_for(VILNIUS.0, VILNIUS.1, 14)
    }

    fn rect(x0: i32, y0: i32, w: i32, h: i32) -> Vec<(i32, i32)> {
        // Clockwise in screen space (y down) = MVT exterior.
        vec![(x0, y0), (x0 + w, y0), (x0 + w, y0 + h), (x0, y0 + h)]
    }

    const KINDS: [&str; 6] = ["apartments", "house", "office", "retail", "yes", "industrial"];

    /// (buildings, landuse, roads) in z14 pixels (extent 4096).
    fn features() -> (Vec<Feat>, Vec<Feat>, Vec<Feat>) {
        let mut b = Vec::new();
        let mut id = 1000u64;
        // Grid over the whole tile: every z16 chunk gets buildings.
        for i in 0..12 {
            for j in 0..12 {
                id += 1;
                let (x, y) = (60 + i * 335, 40 + j * 333);
                let h = 6.0 + ((i * 7 + j * 3) % 9) as f64 * 3.0;
                b.push(Feat {
                    id,
                    rings: vec![rect(x, y, 40 + (i % 3) * 12, 30 + (j % 4) * 9)],
                    props: vec![("id", Val::D(id as f64)), ("osm_type", Val::S("way")), ("kind", Val::S(KINDS[((i + j) % 6) as usize])), ("height", Val::D(h))],
                });
            }
        }
        // Crossers: the z16 edges (1024 multiples) and the z15 / z16 corner at 2048.
        for (k, (x, y, w, hh)) in [(1000, 500, 60, 40), (2030, 2030, 40, 40), (3060, 1200, 70, 50), (500, 3060, 50, 70), (1010, 1010, 30, 30)].into_iter().enumerate() {
            let id = 5000 + k as u64;
            b.push(Feat {
                id,
                rings: vec![rect(x, y, w, hh)],
                props: vec![("id", Val::D(id as f64)), ("osm_type", Val::S("way")), ("kind", Val::S("apartments")), ("levels", Val::D(5.0))],
            });
        }
        // A jagged outline (Douglas-Peucker drops the 1 px teeth at R0, keeps the 3 px ones).
        let mut jag = Vec::new();
        for t in 0..20 {
            jag.push((1500 + t * 6, 700 + if t % 2 == 0 { 0 } else if t % 4 == 1 { 1 } else { 3 }));
        }
        jag.extend([(1620, 700), (1620, 790), (1500, 790)]);
        b.push(Feat {
            id: 6001,
            rings: vec![jag],
            props: vec![("id", Val::D(6001.0)), ("osm_type", Val::S("way")), ("kind", Val::S("office")), ("height", Val::D(24.0))],
        });
        // A courtyard (hole) in a building crossing the z16 edge at x = 3072. The hole itself
        // stays off the edge: a hole cut by a clip edge is triangulated over (earcut sees a hole
        // touching the outer ring), the same in the per-tile path; see the T2a notes.
        let mut hole = rect(3085, 2600, 40, 50);
        hole.reverse();
        b.push(Feat {
            id: 6002,
            rings: vec![rect(3000, 2560, 140, 130), hole],
            props: vec![("id", Val::D(6002.0)), ("osm_type", Val::S("relation")), ("kind", Val::S("apartments")), ("height", Val::D(18.0))],
        });
        // A tower (beacons).
        b.push(Feat {
            id: 6003,
            rings: vec![rect(2200, 1500, 80, 80)],
            props: vec![("id", Val::D(6003.0)), ("osm_type", Val::S("way")), ("kind", Val::S("office")), ("height", Val::D(95.0))],
        });
        let mut whole = rect(-64, -64, 4224, 4224);
        let l = vec![
            Feat { id: 10, rings: vec![std::mem::take(&mut whole)], props: vec![("id", Val::D(10.0)), ("kind", Val::S("residential"))] },
            Feat { id: 11, rings: vec![rect(900, 900, 400, 300)], props: vec![("id", Val::D(11.0)), ("kind", Val::S("park"))] },
            Feat { id: 12, rings: vec![rect(1950, 3000, 300, 500), { let mut h = rect(2100, 3150, 80, 80); h.reverse(); h }], props: vec![("id", Val::D(12.0)), ("kind", Val::S("water"))] },
            Feat { id: 13, rings: vec![rect(2500, 300, 700, 600)], props: vec![("id", Val::D(13.0)), ("kind", Val::S("industrial"))] },
        ];
        let r = vec![
            Feat { id: 20, rings: vec![vec![(-64, 2000), (1500, 2100), (2600, 1980), (4160, 2050)]], props: vec![("id", Val::D(20.0)), ("kind", Val::S("primary")), ("lit", Val::S("yes"))] },
            Feat { id: 21, rings: vec![vec![(1200, -64), (1260, 1800), (1180, 4160)]], props: vec![("id", Val::D(21.0)), ("kind", Val::S("residential"))] },
            Feat { id: 22, rings: vec![vec![(300, 3000), (900, 3020), (1700, 2900)]], props: vec![("id", Val::D(22.0)), ("kind", Val::S("footway"))] },
        ];
        (b, l, r)
    }

    /// `f` expressed in the pixels (extent `ext`) of tile `k` inside the z14 tile.
    fn map(f: Vec<Feat>, k: TileKey, ext: i32) -> Vec<Feat> {
        let z = key14();
        let dz = k.z - z.z;
        let n = 1i32 << dz;
        let (ox, oy) = (((k.x - (z.x << dz)) as i32) * 4096 / n, ((k.y - (z.y << dz)) as i32) * 4096 / n);
        // z14 px -> k px: (p - o) * n * ext / 4096 (exact for the power-of-two cases used).
        let m = |p: (i32, i32)| (((p.0 - ox) as i64 * n as i64 * ext as i64 / 4096) as i32, ((p.1 - oy) as i64 * n as i64 * ext as i64 / 4096) as i32);
        f.into_iter().map(|mut x| {
            x.rings = x.rings.into_iter().map(|r| r.into_iter().map(m).collect()).collect();
            x
        }).collect()
    }

    /// MVT bytes of tile `k` (the z14 tile or a tile inside it) with these per-layer extents
    /// (buildings, landuse, roads).
    pub fn tile_bytes_ext(k: TileKey, ext: [u32; 3]) -> Vec<u8> {
        let (b, l, r) = features();
        let mut t = encode_layer_ext("buildings", &map(b, k, ext[0] as i32), false, ext[0]);
        t.extend(encode_layer_ext("landuse", &map(l, k, ext[1] as i32), false, ext[1]));
        t.extend(encode_layer_ext("roads", &map(r, k, ext[2] as i32), true, ext[2]));
        t
    }

    /// MVT bytes of tile `k` at extent 4096 (every layer).
    pub fn tile_bytes(k: TileKey) -> Vec<u8> {
        tile_bytes_ext(k, [4096; 3])
    }
}

#[cfg(test)]
mod tests;
