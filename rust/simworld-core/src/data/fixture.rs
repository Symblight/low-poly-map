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
    // stays off the edge (per-ring clip path, pinned by the goldens); a hole reaching a clip
    // edge is clipped with its outer ring as a whole since `polyclip` (3c751bd).
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

// ---- Two-level archive (T2b): the same area as a baked detail z14 + overview z11 pair.

/// The overview (z11) tile holding [`key14`].
pub fn key11() -> TileKey {
    let k = key14();
    TileKey::new(11, k.x >> 3, k.y >> 3)
}

/// The fixture's landmarks (roof >= 60 m): the 95 m tower.
pub const LANDMARK_IDS: [u64; 1] = [6003];

fn is_landmark(f: &Feat) -> bool {
    LANDMARK_IDS.contains(&f.id)
}

/// z14 pixels (extent 4096) -> overview z11 pixels at extent `ext` (rounded).
fn to11(f: Vec<Feat>, ext: i32) -> Vec<Feat> {
    let (z, o) = (key14(), key11());
    let (ox, oy) = (((z.x - o.x * 8) * 4096) as f64, ((z.y - o.y * 8) * 4096) as f64);
    let m = |p: (i32, i32)| {
        let k = ext as f64 / (4096.0 * 8.0);
        (((p.0 as f64 + ox) * k).round() as i32, ((p.1 as f64 + oy) * k).round() as i32)
    };
    f.into_iter().map(|mut x| {
        x.rings = x.rings.into_iter().map(|r| r.into_iter().map(m).collect()).collect();
        x
    }).collect()
}

fn num(f: &Feat, k: &str) -> Option<f64> {
    f.props.iter().find(|(n, _)| *n == k).and_then(|(_, v)| match v {
        Val::D(d) => Some(*d),
        _ => None,
    })
}

fn text(f: &Feat, k: &str) -> Option<&'static str> {
    f.props.iter().find(|(n, _)| *n == k).and_then(|(_, v)| match v {
        Val::S(s) => Some(*s),
        _ => None,
    })
}

/// The bake's tagged roof: height, else levels x 3 m.
fn roof(f: &Feat) -> Option<f64> {
    num(f, "height").or(num(f, "levels").map(|l| l * 3.0))
}

fn step5(v: f64) -> f64 {
    ((v / 5.0).round() * 5.0).max(5.0)
}

/// The detail z14 tile of a two-level archive: every layer at extent 8192, landmarks left out.
pub fn detail_v2_bytes() -> Vec<u8> {
    let k = key14();
    let (mut b, l, r) = features();
    b.retain(|f| !is_landmark(f));
    let mut t = encode_layer_ext("buildings", &map(b, k, 8192), false, 8192);
    t.extend(encode_layer_ext("landuse", &map(l, k, 8192), false, 8192));
    t.extend(encode_layer_ext("roads", &map(r, k, 8192), true, 8192));
    t
}

/// Ids of the fixture's buildings (all, landmarks included).
pub fn building_ids() -> Vec<u64> {
    features().0.iter().map(|f| f.id).collect()
}

/// The overview z11 tile of a two-level archive: `blocks` (one per non-landmark building,
/// its bounding box, height rounded to 5 m; extent 4096), `lights` (one point per building at
/// its bounding-box centre, OSM id / type in the feature id; 4096), `landmarks` (full
/// geometry, all attributes; 16384), the primary road and the landuse areas (4096).
pub fn overview_bytes() -> Vec<u8> {
    let (b, l, r) = features();
    let mut blocks = Vec::new();
    let mut lights = Vec::new();
    let mut lms = Vec::new();
    for f in b {
        let ring = &f.rings[0];
        let (x0, x1) = (ring.iter().map(|p| p.0).min().unwrap(), ring.iter().map(|p| p.0).max().unwrap());
        let (y0, y1) = (ring.iter().map(|p| p.1).min().unwrap(), ring.iter().map(|p| p.1).max().unwrap());
        let kind = text(&f, "kind").unwrap_or("yes");
        let relation = text(&f, "osm_type") == Some("relation");
        let tagged = roof(&f);
        let mut props = vec![("kind", Val::S(kind))];
        if let Some(h) = tagged {
            props.push(("height", Val::D(step5(h))));
            // m² at ~2.4 m per z14 pixel near Vilnius (only the slender-tower rule reads it).
            props.push(("area", Val::D(((x1 - x0) * (y1 - y0)) as f64 * 0.37 * 0.37)));
        }
        lights.push(Feat { id: f.id * 10 + if relation { 3 } else { 2 }, rings: vec![vec![((x0 + x1) / 2, (y0 + y1) / 2)]], props });
        if is_landmark(&f) {
            lms.push(f);
            continue;
        }
        let h = step5(tagged.unwrap_or(9.0));
        blocks.push(Feat {
            id: 0,
            rings: vec![vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]],
            props: vec![("id", Val::D(f.id as f64)), ("kind", Val::S(kind)), ("height", Val::D(h)), ("count", Val::D(1.0))],
        });
    }
    let roads: Vec<Feat> = r.into_iter().filter(|f| text(f, "kind") == Some("primary")).collect();
    let mut t = encode_layer_ext("blocks", &to11(blocks, 4096), false, 4096);
    t.extend(encode_layer_ext("lights", &to11(lights, 4096), false, 4096));
    t.extend(encode_layer_ext("landmarks", &to11(lms, 16384), false, 16384));
    t.extend(encode_layer_ext("landuse", &to11(l, 4096), false, 4096));
    t.extend(encode_layer_ext("roads", &to11(roads, 4096), true, 4096));
    t
}
