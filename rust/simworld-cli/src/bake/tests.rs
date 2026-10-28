//! End-to-end bake on a small synthetic source archive in Vilnius (z13 / z14 / z16 tiles encoded
//! like planetiler: buffer 4/256, pieces clipped at the buffer line).

use std::collections::BTreeMap;

use super::archive::{self, Archive, Key};
use super::geom::{m_per_g, tile_span_g, MPoly, Pt, Ring};
use super::input::{self, prop, FGeom};
use super::levels::{self, Params};
use super::mvt_enc::{Layer, Val};
use super::verify;

/// z14 tile in central Vilnius (the largest detail tile of the real bake).
const Z14: (u32, u32) = (9342, 5207);

fn origin() -> Pt {
    [Z14.0 as f64 * tile_span_g(14), Z14.1 as f64 * tile_span_g(14)]
}

/// Rectangle `w x h` m with its top-left corner `x, y` m east / south of the z14 tile's corner.
fn rect_m(x: f64, y: f64, w: f64, h: f64) -> MPoly {
    let o = origin();
    let s = m_per_g(o[1]);
    let p = |a: f64, b: f64| [o[0] + a / s, o[1] + b / s];
    vec![vec![vec![p(x, y), p(x + w, y), p(x + w, y + h), p(x, y + h)]]]
}

fn line_m(pts: &[(f64, f64)]) -> Vec<Ring> {
    let o = origin();
    let s = m_per_g(o[1]);
    vec![pts.iter().map(|(a, b)| [o[0] + a / s, o[1] + b / s]).collect()]
}

fn s(v: &str) -> Val {
    Val::Str(v.into())
}

struct Src {
    tiles: BTreeMap<Key, Vec<(String, Layer)>>,
}

impl Src {
    fn layer(&mut self, k: Key, name: &str) -> &mut Layer {
        let v = self.tiles.entry(k).or_default();
        if let Some(i) = v.iter().position(|l| l.0 == name) {
            return &mut v[i].1;
        }
        v.push((name.to_string(), Layer::new(name, 4096)));
        &mut v.last_mut().unwrap().1
    }
    fn poly(&mut self, z: u8, name: &str, m: &MPoly, fid: Option<u64>, props: &[(&str, Val)]) {
        let buf = tile_span_g(z) * 4.0 / 256.0;
        let mut out = Vec::new();
        levels::polys_into_tiles(m, z, 4096, buf, |k, g| out.push((k, g)));
        for (k, g) in out {
            self.layer(k, name).add(fid, props, &g);
        }
    }
    fn line(&mut self, z: u8, l: &[Ring], props: &[(&str, Val)]) {
        let buf = tile_span_g(z) * 4.0 / 256.0;
        let mut out = Vec::new();
        levels::lines_into_tiles(l, z, 4096, buf, |k, g| out.push((k, g)));
        for (k, g) in out {
            self.layer(k, "roads").add(None, props, &g);
        }
    }
    fn building(&mut self, id: u64, osm_type: &str, m: &MPoly, extra: &[(&str, Val)]) {
        let mut props = vec![("id", Val::F64(id as f64)), ("osm_type", s(osm_type)), ("kind", s("yes")), ("min_height", Val::F64(0.0))];
        props.extend(extra.iter().cloned());
        let fid = id * 10 + if osm_type == "way" { 2 } else { 3 };
        self.poly(16, "buildings", m, Some(fid), &props);
    }
}

fn source(path: &std::path::Path) {
    let mut src = Src { tiles: BTreeMap::new() };
    // A: no height, in residential landuse -> 5 levels x 3 m.
    src.building(1, "way", &rect_m(100.0, 100.0, 20.0, 20.0), &[]);
    // B: crosses the z16 seam at ~353.5 m east: stitched from two clipped pieces.
    src.building(2, "way", &rect_m(330.0, 100.0, 40.0, 15.0), &[("height", Val::F64(9.0))]);
    // C + D: 3 m apart -> one block, height (10 + 12) / 2 = 11 -> 10.
    src.building(3, "way", &rect_m(100.0, 300.0, 12.0, 12.0), &[("height", Val::F64(10.0)), ("historic", s("yes"))]);
    src.building(4, "way", &rect_m(115.0, 300.0, 12.0, 12.0), &[("levels", Val::F64(4.0))]);
    // E: 9 m² shed -> no detail footprint, still a light.
    src.building(5, "way", &rect_m(200.0, 600.0, 3.0, 3.0), &[("height", Val::F64(2.0))]);
    // F: 80 m tower -> landmark only.
    src.building(6, "relation", &rect_m(600.0, 600.0, 15.0, 15.0), &[("height", Val::F64(80.0)), ("man_made", s("tower"))]);
    // G: isolated 100 m² house -> detail, no block (< 150 m²).
    src.building(7, "way", &rect_m(900.0, 900.0, 10.0, 10.0), &[("height", Val::F64(6.0))]);
    // z14 ground: residential landuse around A, a residential road.
    src.poly(14, "landuse", &rect_m(50.0, 50.0, 150.0, 150.0), None, &[("id", Val::F64(100.0)), ("kind", s("residential"))]);
    src.line(14, &line_m(&[(0.0, 250.0), (1400.0, 250.0)]), &[("id", Val::F64(200.0)), ("kind", s("residential"))]);
    // z13: a primary and a tertiary road; forest (kept), small park (dropped), pond (kept).
    src.line(13, &line_m(&[(-500.0, 260.0), (2000.0, 260.0)]), &[("id", Val::F64(201.0)), ("kind", s("primary"))]);
    src.line(13, &line_m(&[(0.0, 700.0), (1000.0, 700.0)]), &[("id", Val::F64(202.0)), ("kind", s("tertiary"))]);
    src.poly(13, "landuse", &rect_m(1000.0, 1000.0, 200.0, 200.0), None, &[("id", Val::F64(300.0)), ("kind", s("forest"))]);
    src.poly(13, "landuse", &rect_m(400.0, 1000.0, 50.0, 50.0), None, &[("id", Val::F64(301.0)), ("kind", s("park"))]);
    src.poly(13, "landuse", &rect_m(700.0, 1000.0, 60.0, 60.0), None, &[("id", Val::F64(302.0)), ("kind", s("water"))]);
    let tiles: BTreeMap<Key, Vec<u8>> = src
        .tiles
        .into_iter()
        .map(|(k, ls)| {
            let enc: Vec<Vec<u8>> = ls.iter().map(|l| l.1.encode()).collect();
            (k, archive::gzip(&super::mvt_enc::tile(&enc)))
        })
        .collect();
    let meta = serde_json::json!({"attribution": "© OpenStreetMap contributors"});
    archive::write(path.to_str().unwrap(), &tiles, &meta, [25.2, 54.6, 25.4, 54.8], 14).unwrap();
}

#[test]
fn bakes_two_levels_with_every_building_once() {
    let dir = std::env::temp_dir().join(format!("simworld-bake-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (inp, out) = (dir.join("in.pmtiles"), dir.join("out.pmtiles"));
    source(&inp);
    let p = Params::default();
    let (tiles, stats, buildings) = super::bake(inp.to_str().unwrap(), &p).unwrap();
    let src = Archive::open(inp.to_str().unwrap()).unwrap();
    archive::write(out.to_str().unwrap(), &tiles, &super::metadata(&p, &src), src.bounds, 13).unwrap();
    let a = Archive::open(out.to_str().unwrap()).unwrap();

    // Structural verification, against the source buildings.
    let sum = verify::verify(&a, Some(&buildings)).unwrap_or_else(|e| panic!("{e:#?}"));
    assert_eq!((sum.detail_ids, sum.landmark_ids, sum.blocks, sum.lights), (5, 1, 3, 7), "{sum:?}");
    assert_eq!(a.metadata["simworld_levels"], serde_json::json!({"detail": 14, "overview": 11}));
    assert!(a.keys().all(|k| k.0 == 14 || k.0 == 11));
    let c = &stats.counts;
    assert_eq!((c.buildings, c.detail_buildings, c.landmarks, c.dropped_small), (7, 5, 1, 1));

    // Stitching: B came in two clipped pieces and is whole again.
    let b = buildings.iter().find(|b| b.key == (1, 2)).unwrap();
    assert!((b.area_m2 - 600.0).abs() < 600.0 * 0.01, "B area {}", b.area_m2);
    // Height resolution with the landuse fallback.
    let a1 = buildings.iter().find(|b| b.key == (1, 1)).unwrap();
    assert_eq!((a1.landuse.as_deref(), a1.roof_m, a1.tagged_roof_m), (Some("residential"), 15.0, None));

    // The runtime's decoder reads a detail tile (extent 8192 everywhere) as it reads today's tiles.
    let dk = (14u8, Z14.0, Z14.1);
    let bytes = a.tile(dk).unwrap().unwrap();
    let frame = simworld_core::geo::Frame::new(54.687, 25.28);
    let dec = simworld_core::mvt::decode_tile(&bytes, simworld_core::geo::TileKey::new(14, Z14.0, Z14.1), frame, true, true, &simworld_core::Profile::default()).unwrap();
    let ids: Vec<u64> = dec.buildings.iter().map(|b| b.id).collect();
    assert_eq!(ids, vec![1, 2, 3, 4, 7]);
    let bb = dec.buildings.iter().find(|b| b.id == 2).unwrap();
    assert!(!bb.clipped && (bb.footprint_m2 - 600.0).abs() < 12.0, "B decoded {} m²", bb.footprint_m2);
    assert_eq!(dec.buildings[0].landuse, Some(simworld_core::mvt::Landuse::Residential));
    assert_eq!(dec.ground.roads.len(), 1);

    // Overview content.
    let ok = (11u8, Z14.0 >> 3, Z14.1 >> 3);
    let ov = a.tile(ok).unwrap().unwrap();
    let layer = |name: &str| input::decode_layer(&ov, ok, name).unwrap().map(|l| l.1).unwrap_or_default();
    let blocks = layer("blocks");
    let cd = blocks.iter().find(|f| prop(&f.props, "count").and_then(Val::as_f64) == Some(2.0)).expect("C + D block");
    assert_eq!(prop(&cd.props, "height").and_then(Val::as_f64), Some(10.0));
    // A (400 m²) and B (600 m²) are blocks of their own; E (9 m²) and G (100 m²) are below 150 m².
    assert_eq!(blocks.len(), 3);
    let lm = layer("landmarks");
    assert_eq!(lm.len(), 1);
    assert_eq!(input::osm_id(&lm[0].props, lm[0].fid), 6);
    let lights = layer("lights");
    assert_eq!(lights.len(), 7);
    let tower = lights.iter().find(|f| input::osm_id(&f.props, f.fid) == 6).unwrap();
    assert_eq!(input::osm_type_code(&tower.props, tower.fid), 2);
    assert_eq!(prop(&tower.props, "height").and_then(Val::as_f64), Some(80.0));
    assert_eq!(prop(&tower.props, "man_made").and_then(Val::as_str), Some("tower"));
    let a_light = lights.iter().find(|f| input::osm_id(&f.props, f.fid) == 1).unwrap();
    assert_eq!(prop(&a_light.props, "height"), None);
    assert_eq!(prop(&a_light.props, "landuse").and_then(Val::as_str), Some("residential"));
    let roads: Vec<String> = layer("roads").iter().filter_map(|f| prop(&f.props, "kind").and_then(Val::as_str).map(String::from)).collect();
    assert_eq!(roads, vec!["primary"]);
    let mut lu: Vec<String> = layer("landuse").iter().filter_map(|f| prop(&f.props, "kind").and_then(Val::as_str).map(String::from)).collect();
    lu.sort();
    assert_eq!(lu, vec!["forest", "water"]);
    assert!(layer("lights").iter().all(|f| matches!(f.geom, FGeom::Points(_))));
    std::fs::remove_dir_all(&dir).ok();
}

/// The real archive (`SIMWORLD_BAKE_INPUT`, default `data/out/vilnius.pmtiles`; skipped when
/// absent): every building in exactly one level, and stitched footprints equal the sum of their
/// pieces cut exactly at the z16 tile edges (away from the archive's edge).
#[test]
#[ignore = "needs data/out/vilnius.pmtiles; run with --ignored"]
fn bakes_the_vilnius_archive() {
    let inp = std::env::var("SIMWORLD_BAKE_INPUT").unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/out/vilnius.pmtiles").to_string());
    if !std::path::Path::new(&inp).exists() {
        eprintln!("skipped: {inp} missing");
        return;
    }
    let p = Params::default();
    let (tiles, _, buildings) = super::bake(&inp, &p).unwrap();
    let src = Archive::open(&inp).unwrap();
    let out = std::env::temp_dir().join(format!("simworld-bake-vilnius-{}.pmtiles", std::process::id()));
    archive::write(out.to_str().unwrap(), &tiles, &super::metadata(&p, &src), src.bounds, 13).unwrap();
    let a = Archive::open(out.to_str().unwrap()).unwrap();
    verify::verify(&a, Some(&buildings)).unwrap_or_else(|e| panic!("{e:#?}"));
    std::fs::remove_file(&out).ok();

    let mut pieces: std::collections::HashMap<(u8, u64), f64> = std::collections::HashMap::new();
    for k in src.keys_at(16) {
        let bytes = src.tile(k).unwrap().unwrap();
        let Some((_, feats)) = input::decode_layer(&bytes, k, "buildings").unwrap() else { continue };
        let r = super::geom::Rect::tile(16, k.1, k.2, 0.0);
        for f in feats {
            if let FGeom::Polys(m) = f.geom {
                let key = (input::osm_type_code(&f.props, f.fid), input::osm_id(&f.props, f.fid));
                *pieces.entry(key).or_default() += super::geom::mpoly_area(&super::geom::clip_mpoly(&m, &r));
            }
        }
    }
    let present: std::collections::HashSet<Key> = src.keys_at(16).into_iter().collect();
    let mut bad = Vec::new();
    for b in &buildings {
        // At the archive's edge the tiles beyond are missing: only the buffered pieces exist.
        if !super::geom::Rect::of_mpoly(&b.polys).tiles(16, 0.0).all(|(x, y)| present.contains(&(16, x, y))) {
            continue;
        }
        let s = m_per_g(b.centroid[1]);
        let want = pieces.get(&b.key).copied().unwrap_or(0.0) * s * s;
        if b.area_m2 > 50.0 && (b.area_m2 - want).abs() > 0.01 * want {
            bad.push((b.key, b.area_m2, want, b.centroid[0] / super::geom::G_WORLD * 360.0 - 180.0, super::geom::lat_of(b.centroid[1])));
        }
    }
    // Source quirk: relation 11618806 comes as one polygon in one z16 tile and as two nearly
    // identical overlapping polygons in its neighbour; the piece sum counts it twice. Allow such
    // cases at 1 in 10 000.
    eprintln!("stitched != pieces: {bad:?}");
    assert!(bad.len() <= buildings.len() / 10_000, "{} stitched footprints differ from their pieces, e.g. {:?}", bad.len(), &bad[..bad.len().min(10)]);
}
