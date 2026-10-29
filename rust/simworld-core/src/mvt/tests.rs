use super::test_mvt::*;
use super::*;
use crate::geo::{Frame, TileKey};
use crate::profile::Profile;

fn square(x0: i32, y0: i32, s: i32) -> Vec<(i32, i32)> {
    // Clockwise in screen space (y down) = MVT exterior.
    vec![(x0, y0), (x0 + s, y0), (x0 + s, y0 + s), (x0, y0 + s)]
}

fn buildings_layer() -> Vec<u8> {
    encode_layer(
        "buildings",
        &[
            Feat {
                id: 1234567892,
                rings: vec![square(1000, 1000, 400)],
                props: vec![
                    ("id", Val::D(123456789.0)),
                    ("osm_type", Val::S("way")),
                    ("kind", Val::S("apartments")),
                    ("levels", Val::D(2.5)),
                    ("min_height", Val::D(0.0)),
                ],
            },
            Feat {
                id: 77,
                // Straddles the east edge: centroid outside -> not owned.
                rings: vec![square(4000, 2000, 300)],
                props: vec![
                    ("id", Val::D(7.0)),
                    ("osm_type", Val::S("relation")),
                    ("kind", Val::S("office")),
                    ("height", Val::D(42.0)),
                ],
            },
            Feat {
                id: 99,
                // kind=yes inside the industrial landuse below.
                rings: vec![square(3000, 3000, 100)],
                props: vec![("id", Val::D(9.0)), ("osm_type", Val::S("way")), ("kind", Val::S("yes"))],
            },
        ],
    )
}

fn fixture() -> Vec<u8> {
    let mut t = buildings_layer();
    t.extend(encode_layer(
        "landuse",
        &[
            Feat { id: 10, rings: vec![square(-64, -64, 4224)], props: vec![("kind", Val::S("residential"))] },
            Feat { id: 20, rings: vec![square(2800, 2800, 600)], props: vec![("kind", Val::S("industrial"))] },
        ],
    ));
    t
}

#[test]
fn decodes_buildings_layer_properties_ownership_and_landuse() {
    let key = TileKey::new(16, 37370, 20827);
    let (lat, lon) = key.center_lat_lon();
    let frame = Frame::new(lat, lon);
    let bs = decode_buildings(&fixture(), key, frame, &Profile::default()).unwrap();
    assert_eq!(bs.len(), 3);
    let a = bs.iter().find(|b| b.id == 123456789).unwrap();
    assert_eq!(a.osm_type, OsmType::Way);
    assert_eq!(a.kind, "apartments");
    assert_eq!(a.levels, Some(2.5));
    assert_eq!(a.height, None);
    assert!((crate::mesher::resolve(a, &Profile::default()).height - 7.5).abs() < 1e-6);
    assert!(a.owned);
    assert_eq!(a.polygons[0][0].len(), 4);
    assert_eq!(a.centroid_px, [1200.0, 1200.0]);
    // Centroid of square at px (1200,1200): west and north of the tile center.
    assert!(a.centroid[0] < 0.0 && a.centroid[1] > 0.0);
    // Smallest containing landuse wins.
    assert_eq!(a.landuse, Some(Landuse::Residential));
    let y = bs.iter().find(|b| b.id == 9).unwrap();
    assert_eq!(y.landuse, Some(Landuse::Industrial));
    assert_eq!(crate::mesher::resolve(y, &Profile::default()).profile, crate::mesher::PROFILE_INDUSTRIAL);

    let b = bs.iter().find(|b| b.id == 7).unwrap();
    assert_eq!(b.osm_type, OsmType::Relation);
    assert_eq!(crate::mesher::resolve(b, &Profile::default()).height, 42.0);
    assert!(!b.owned);
}

/// Night-window attributes (W2 tile schema): decoded when present, absent otherwise (old
/// archives); `clipped` / `footprint_m2` from the received geometry.
#[test]
fn decodes_night_window_attributes_and_clipping() {
    let t = encode_layer(
        "buildings",
        &[
            Feat {
                id: 10,
                rings: vec![square(1000, 1000, 100)],
                props: vec![
                    ("id", Val::D(1.0)),
                    ("osm_type", Val::S("way")),
                    ("kind", Val::S("yes")),
                    ("historic", Val::S("castle")),
                    ("man_made", Val::S("tower")),
                    ("windows", Val::B(false)),
                    ("ruins", Val::B(true)),
                    ("disused", Val::B(true)),
                    ("abandoned", Val::S("yes")),
                ],
            },
            // Old archive: none of the attributes. Cut by the encoder at the -64 buffer line.
            Feat { id: 20, rings: vec![square(-64, 2000, 200)], props: vec![("id", Val::D(2.0)), ("kind", Val::S("yes"))] },
            Feat { id: 30, rings: vec![square(3000, 3000, 100)], props: vec![("id", Val::D(3.0)), ("windows", Val::B(true))] },
        ],
    );
    let key = TileKey::new(16, 37370, 20827);
    let frame = Frame::new(54.6872, 25.2797);
    let bs = decode_buildings(&t, key, frame, &Profile::default()).unwrap();
    let get = |id| bs.iter().find(|b| b.id == id).unwrap();
    let a = get(1);
    assert_eq!(
        a.tags,
        BuildingTags {
            historic: Some("castle".into()),
            man_made: Some("tower".into()),
            windows: Some(false),
            ruins: true,
            disused: true,
            abandoned: true
        }
    );
    assert!(!a.clipped);
    let mpp = TileProjector::new(key, frame, 4096.0).m_per_px();
    assert!(((a.footprint_m2 as f64) - 100.0 * 100.0 * mpp * mpp).abs() < 0.01, "{}", a.footprint_m2);
    let b = get(2);
    assert_eq!(b.tags, BuildingTags::default());
    assert!(b.clipped);
    assert_eq!(get(3).tags.windows, Some(true));
    assert!(!get(3).clipped);
    // Ruins + disused: dark whatever the historic / man_made values say.
    assert_eq!(crate::mesher::resolve(a, &Profile::default()).profile, crate::mesher::PROFILE_DARK);
}

#[test]
fn gzip_and_determinism() {
    use std::io::Write;
    let raw = fixture();
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(&raw).unwrap();
    let gz = enc.finish().unwrap();
    let key = TileKey::new(16, 37370, 20827);
    let frame = Frame::new(54.6872, 25.2797);
    let a = decode_buildings(&raw, key, frame, &Profile::default()).unwrap();
    let b = decode_buildings(&gz, key, frame, &Profile::default()).unwrap();
    assert_eq!(a, b);
    assert_eq!(a[0].seed01(), b[0].seed01());
    assert!(decode_buildings(&[], key, frame, &Profile::default()).unwrap().is_empty());
    assert!(decode_buildings(b"garbage!", key, frame, &Profile::default()).is_err());
    // No landuse layer -> None.
    let nolu = decode_buildings(&buildings_layer(), key, frame, &Profile::default()).unwrap();
    assert!(nolu.iter().all(|b| b.landuse.is_none()));
}

#[test]
fn missing_layer_is_empty_and_seed_distinguishes_type() {
    let t = encode_layer("roads", &[]);
    let frame = Frame::new(54.6872, 25.2797);
    assert!(decode_buildings(&t, TileKey::new(16, 1, 1), frame, &Profile::default()).unwrap().is_empty());
    let mk = |t| Building {
        osm_type: t,
        id: 42,
        kind: String::new(),
        height: None,
        levels: None,
        min_height: 0.0,
        landuse: None,
        tags: BuildingTags::default(),
        polygons: vec![],
        clipped: false,
        footprint_m2: 0.0,
        centroid_px: [0.0; 2],
        centroid: [0.0; 2],
        base_m: 0.0,
        owned: true,
    };
    assert_ne!(mk(OsmType::Way).seed01(), mk(OsmType::Relation).seed01());
    let s = mk(OsmType::Way).seed01();
    assert!((0.0..1.0).contains(&s));
}

#[test]
fn ground_layers_decode_and_parks_do_not_change_building_fallback() {
    let mut hole = square(2600, 600, 200);
    hole.reverse();
    let mut t = encode_layer(
        "buildings",
        &[Feat {
            id: 50,
            rings: vec![square(1000, 1000, 100)],
            props: vec![("id", Val::D(5.0)), ("osm_type", Val::S("way")), ("kind", Val::S("yes"))],
        }],
    );
    t.extend(encode_layer(
        "landuse",
        &[
            Feat { id: 10, rings: vec![square(-64, -64, 4224)], props: vec![("kind", Val::S("residential"))] },
            // Smaller than the residential landuse and containing the building: before M5
            // the layer had no parks, so it must not take over the fallback.
            Feat { id: 11, rings: vec![square(900, 900, 400)], props: vec![("id", Val::D(11.0)), ("kind", Val::S("park"))] },
            Feat {
                id: 12,
                rings: vec![square(2400, 400, 600), hole],
                props: vec![("id", Val::D(12.0)), ("kind", Val::S("water"))],
            },
        ],
    ));
    t.extend(encode_layer_typed(
        "roads",
        &[
            Feat {
                id: 20,
                rings: vec![vec![(0, 2000), (2000, 2100), (4096, 2000)]],
                props: vec![("id", Val::D(20.0)), ("kind", Val::S("primary")), ("lit", Val::S("yes"))],
            },
            Feat { id: 21, rings: vec![vec![(100, 3000), (900, 3000)]], props: vec![("id", Val::D(21.0)), ("kind", Val::S("footway"))] },
        ],
        true,
    ));
    let key = TileKey::new(16, 37370, 20827);
    let frame = Frame::new(54.6872, 25.2797);
    let d = decode_tile(&t, key, frame, true, true, &Profile::default()).unwrap();
    assert_eq!(d.buildings.len(), 1);
    assert_eq!(d.buildings[0].landuse, Some(Landuse::Residential));
    assert_eq!(d.buildings[0].base_m, 0.0);
    let g = &d.ground;
    assert_eq!(g.areas.len(), 3);
    // Sorted bottom level first: residential, park, water.
    assert_eq!(g.areas.iter().map(|a| a.class).collect::<Vec<_>>(), vec![GROUND_RESIDENTIAL, GROUND_GRASS, GROUND_WATER]);
    assert_eq!(g.areas[2].rings.len(), 2, "water keeps its hole");
    assert_eq!(g.roads.len(), 2);
    // Roads sorted minor class first.
    assert_eq!((g.roads[0].class, g.roads[0].minor, g.roads[0].lit), (ROAD_OTHER, true, false));
    assert_eq!((g.roads[1].class, g.roads[1].lit), (ROAD_PRIMARY, true));
    assert_eq!(g.roads[1].lines[0].len(), 3);
    // Without ground: same buildings, no ground.
    let nb = decode_tile(&t, key, frame, false, false, &Profile::default()).unwrap();
    assert!(nb.ground.is_empty());
    assert_eq!(nb.buildings, d.buildings);
    // Meshes: R0 has the footway, R1 not.
    let m0 = crate::ground::mesh_ground(g, &d.proj, 0, &Profile::default());
    let m1 = crate::ground::mesh_ground(g, &d.proj, 1, &Profile::default());
    assert!(m0.triangle_count() > m1.triangle_count() && !m1.is_empty());
    assert!(m0.colors.iter().any(|c| *c == crate::ground::road_color(ROAD_PRIMARY, true)));
}

#[test]
fn landuse_index_holes_and_grid() {
    let outer = vec![[0.0, 0.0], [1000.0, 0.0], [1000.0, 1000.0], [0.0, 1000.0]];
    let hole = vec![[400.0, 400.0], [600.0, 400.0], [600.0, 600.0], [400.0, 600.0]];
    let idx = LanduseIndex::new(
        vec![
            (Landuse::Residential, 1, vec![outer, hole]),
            (Landuse::Retail, 2, vec![vec![[3000.0, 3000.0], [3500.0, 3000.0], [3500.0, 3500.0]]]),
        ],
        4096.0,
    );
    assert_eq!(idx.lookup([100.0, 100.0]), Some(Landuse::Residential));
    assert_eq!(idx.lookup([500.0, 500.0]), None);
    assert_eq!(idx.lookup([3400.0, 3100.0]), Some(Landuse::Retail));
    assert_eq!(idx.lookup([3100.0, 3400.0]), None);
    assert_eq!(idx.lookup([-5000.0, 9000.0]), None);
}
