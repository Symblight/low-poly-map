use super::*;
use crate::geo::{tile_for, Frame};
use crate::mvt::{Landuse, OsmType};

mod clip_tests;
mod wall_tests;

const VILNIUS: (f64, f64) = (54.6872, 25.2797);

fn dp() -> Profile {
    Profile::default()
}
fn lod0() -> LodParams {
    dp().lod(0).unwrap()
}
fn lod1() -> LodParams {
    dp().lod(1).unwrap()
}

fn bld(kind: &str, height: Option<f32>, levels: Option<f32>, polys: Vec<Vec<Vec<[f64; 2]>>>) -> Building {
    Building {
        osm_type: OsmType::Way,
        id: 4242,
        kind: kind.into(),
        height,
        levels,
        min_height: 0.0,
        landuse: None,
        tags: Default::default(),
        polygons: polys,
        clipped: false,
        footprint_m2: 0.0,
        centroid_px: [0.0; 2],
        centroid: [0.0; 2],
        base_m: 0.0,
        owned: true,
    }
}

/// Square ring in pixels, clockwise in screen space (y down) like MVT exteriors.
fn sq(x0: f64, y0: f64, s: f64) -> Vec<[f64; 2]> {
    vec![[x0, y0], [x0 + s, y0], [x0 + s, y0 + s], [x0, y0 + s]]
}

fn setup() -> (TileProjector, f64) {
    let key = tile_for(VILNIUS.0, VILNIUS.1, 16);
    let (lat, lon) = key.center_lat_lon();
    let p = TileProjector::new(key, Frame::new(lat, lon), 4096.0);
    let m = p.m_per_px();
    (p, m)
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Every triangle is clockwise seen from the side its normal points to (Godot front face),
/// and its geometric normal agrees with the vertex normal.
fn assert_winding(m: &MeshData) {
    for t in m.indices.as_chunks::<3>().0 {
        let [a, b, c] = [t[0] as usize, t[1] as usize, t[2] as usize];
        let g = cross(sub(m.positions[c], m.positions[a]), sub(m.positions[b], m.positions[a]));
        let l = dot(g, g).sqrt();
        assert!(l > 0.0);
        assert!(dot(g, m.normals[a]) / l > 0.99, "triangle {t:?} faces away from its normal");
    }
}

fn mesh_centroid(m: &MeshData) -> [f32; 3] {
    let n = m.positions.len() as f32;
    let mut c = [0.0; 3];
    for p in &m.positions {
        for k in 0..3 {
            c[k] += p[k] / n;
        }
    }
    c
}

#[test]
fn box_triangle_count_normals_outward_and_uvs() {
    let (p, mpp) = setup();
    let b = bld("office", Some(30.0), None, vec![vec![sq(1000.0, 1000.0, 200.0)]]);
    let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), lod0(), &dp());
    // 4 walls x 2 + roof 2 = 10 triangles; 4 x 4 + 4 vertices.
    assert_eq!(m.triangle_count(), 10);
    assert_eq!(m.vertex_count(), 20);
    assert_winding(&m);
    // Wall normals point away from the footprint center.
    let c = mesh_centroid(&m);
    for (i, n) in m.normals.iter().enumerate() {
        if n[1] == 0.0 {
            let d = sub(m.positions[i], c);
            assert!(d[0] * n[0] + d[2] * n[2] > 0.0, "wall normal points inward");
            assert!((dot(*n, *n) - 1.0).abs() < 1e-5);
        } else {
            assert_eq!(*n, [0.0, 1.0, 0.0]);
        }
    }
    // UV: window columns (office: 2 m) around the ring and floors up (30 m / 3 m = 10).
    let side = 200.0 * mpp;
    let max_u = m.uvs[..16].iter().map(|uv| uv[0]).fold(0.0f32, f32::max);
    let max_v = m.uvs[..16].iter().map(|uv| uv[1]).fold(0.0f32, f32::max);
    assert_eq!(max_v, 10.0);
    assert_eq!(max_u, 4.0 * (side / 2.0).round() as f32, "{max_u} vs side {side}");
    // Roof at 30 m with x/z meter UVs.
    for i in 16..20 {
        assert_eq!(m.positions[i][1], 30.0);
        assert_eq!(m.uvs[i], [m.positions[i][0], m.positions[i][2]]);
    }
    // Vertex color encoding.
    let col = m.colors[0];
    assert_eq!(col[0], 1.0 / 8.0);
    assert!((col[2] - 0.3).abs() < 1e-6);
    assert!((col[3] - 30.0 / 400.0).abs() < 1e-6);
}

#[test]
fn both_windings_give_outward_normals() {
    let (p, _) = setup();
    let mut ring = sq(1000.0, 1000.0, 200.0);
    ring.reverse();
    let m = mesh_buildings(&[bld("yes", Some(10.0), None, vec![vec![ring]])], &p, ClipRect::tile(4096.0), lod0(), &dp());
    assert_eq!(m.triangle_count(), 10);
    assert_winding(&m);
    let c = mesh_centroid(&m);
    for (i, n) in m.normals.iter().enumerate().filter(|(_, n)| n[1] == 0.0) {
        let d = sub(m.positions[i], c);
        assert!(d[0] * n[0] + d[2] * n[2] > 0.0);
    }
}

#[test]
fn earcut_with_hole_and_courtyard_walls_face_inward_to_the_yard() {
    let (p, _) = setup();
    let outer = sq(1000.0, 1000.0, 300.0);
    let mut hole = sq(1100.0, 1100.0, 100.0);
    hole.reverse();
    let m = mesh_buildings(&[bld("apartments", None, Some(5.0), vec![vec![outer, hole]])], &p, ClipRect::tile(4096.0), lod0(), &dp());
    // Walls: 8 edges x 2; roof of a square with a square hole: 8 vertices -> 8 triangles.
    assert_eq!(m.triangle_count(), 16 + 8);
    assert_winding(&m);
    // Roof area = outer - hole.
    let mut area = 0.0f32;
    for t in m.indices.as_chunks::<3>().0 {
        let [a, b, c] = [t[0] as usize, t[1] as usize, t[2] as usize];
        if m.normals[a][1] > 0.5 {
            let g = cross(sub(m.positions[c], m.positions[a]), sub(m.positions[b], m.positions[a]));
            area += dot(g, g).sqrt() / 2.0;
        }
    }
    let (_, mpp) = setup();
    let expect = ((300.0f64 * 300.0 - 100.0 * 100.0) * mpp * mpp) as f32;
    assert!((area - expect).abs() / expect < 0.005, "{area} vs {expect}");
    // Courtyard walls: normals point toward the yard center (away from building material).
    let yard = p.project([1150.0, 1150.0]);
    let yard = [yard[0], 0.0, -yard[1]];
    let mut yard_walls = 0;
    for (i, n) in m.normals.iter().enumerate().filter(|(_, n)| n[1] == 0.0) {
        let d = sub(m.positions[i], yard);
        if d[0].abs() < 8.0 && d[2].abs() < 8.0 {
            assert!(d[0] * n[0] + d[2] * n[2] < 0.0, "yard wall faces into the building");
            yard_walls += 1;
        }
    }
    assert_eq!(yard_walls, 16);
}

#[test]
fn multipolygon_parts_and_raised_part_gets_bottom() {
    let (p, _) = setup();
    let mut b = bld("yes", Some(20.0), None, vec![vec![sq(100.0, 100.0, 100.0)], vec![sq(400.0, 100.0, 100.0)]]);
    b.min_height = 8.0;
    let m = mesh_buildings(&[b], &p, ClipRect::tile(4096.0), lod0(), &dp());
    // Two boxes, each: 8 wall + 2 roof + 2 bottom.
    assert_eq!(m.triangle_count(), 24);
    assert_winding(&m);
    assert!(m.positions.iter().all(|v| v[1] >= 8.0));
    assert!(m.normals.iter().any(|n| n[1] < -0.5));
    // Wall v starts at 0 at min_height.
    assert!(m.uvs.iter().all(|uv| uv[1] <= 12.0 + 1e-4));
}

#[test]
fn heights_and_profiles_fallbacks() {
    let r = |kind: &str, h, l, lu| {
        let mut b = bld(kind, h, l, vec![]);
        b.landuse = lu;
        resolve(&b, &dp())
    };
    // height wins; floor = height / levels clamped.
    let a = r("office", Some(40.0), Some(10.0), None);
    assert_eq!((a.profile, a.height, a.floor_h), (PROFILE_OFFICE, 40.0, 4.0));
    assert_eq!(r("yes", Some(40.0), Some(2.0), None).floor_h, 5.0);
    assert_eq!(r("yes", Some(10.0), Some(10.0), None).floor_h, 2.5);
    assert_eq!(r("yes", Some(12.0), None, None).floor_h, 3.0);
    // levels x 3.
    let b = r("apartments", None, Some(9.0), None);
    assert_eq!((b.profile, b.height, b.floor_h), (PROFILE_RESIDENTIAL, 27.0, 3.0));
    // landuse fallback levels.
    assert_eq!(r("yes", None, None, Some(Landuse::Residential)).height, 15.0);
    assert_eq!(r("yes", None, None, Some(Landuse::Commercial)).height, 12.0);
    assert_eq!(r("yes", None, None, Some(Landuse::Industrial)).height, 3.0);
    assert_eq!(r("yes", None, None, Some(Landuse::Other)).height, 6.0);
    assert_eq!(r("yes", None, None, None).height, 6.0);
    assert_eq!(r("garage", None, None, Some(Landuse::Residential)).height, 3.0);
    // Profiles.
    assert_eq!(r("house", None, None, Some(Landuse::Industrial)).profile, PROFILE_RESIDENTIAL);
    assert_eq!(r("supermarket", None, None, None).profile, PROFILE_RETAIL);
    assert_eq!(r("warehouse", None, None, None).profile, PROFILE_INDUSTRIAL);
    assert_eq!(r("cathedral", None, None, None).profile, PROFILE_LANDMARK);
    assert_eq!(r("school", None, None, None).profile, PROFILE_CIVIC);
    assert_eq!(r("yes", None, None, Some(Landuse::Industrial)).profile, PROFILE_INDUSTRIAL);
    assert_eq!(r("yes", None, None, Some(Landuse::Commercial)).profile, PROFILE_OFFICE);
    assert_eq!(r("yes", None, None, Some(Landuse::Retail)).profile, PROFILE_RETAIL);
    assert_eq!(r("yes", None, None, Some(Landuse::Civic)).profile, PROFILE_CIVIC);
    assert_eq!(r("yes", None, None, Some(Landuse::Other)).profile, PROFILE_RESIDENTIAL);
    assert_eq!(r("yes", None, None, None).profile, PROFILE_RESIDENTIAL);
    assert_eq!(r("weird_value", None, None, None).profile, PROFILE_RESIDENTIAL);
    // Raised part with levels only.
    let mut raised = bld("yes", None, Some(2.0), vec![]);
    raised.min_height = 9.0;
    assert_eq!(resolve(&raised, &dp()).height, 15.0);
}

/// Resolution order of docs/spec-night-windows.md section 1 and the `windows` tag.
#[test]
fn resolve_light_profiles_tags_shape_and_overrides() {
    use crate::mvt::{BuildingTags, RawBuilding};
    let p = dp();
    let (proj, mpp) = setup();
    // A building through the real decode path (`TileProjector::finish` sets `clipped` and
    // `footprint_m2`): a `w_m` x `w_m` square at pixel (x0, y0).
    let mk = |kind: &str, height: Option<f32>, levels: Option<f32>, tags: BuildingTags, x0: f64, w_m: f64| {
        let s = w_m / mpp;
        proj.finish(RawBuilding {
            osm_type: OsmType::Way,
            id: 777,
            kind: kind.into(),
            height,
            levels,
            min_height: 0.0,
            landuse: Some(Landuse::Residential),
            tags,
            polygons: vec![vec![sq(x0, 1000.0, s)]],
        })
        .unwrap()
    };
    let t = |historic: Option<&str>, man_made: Option<&str>| BuildingTags {
        historic: historic.map(Into::into),
        man_made: man_made.map(Into::into),
        ..Default::default()
    };
    let prof = |b: &Building| resolve(b, &p).profile;
    // 3. Kind table, then historic, then man_made for generic kinds.
    assert_eq!(prof(&mk("castle", None, None, t(None, None), 1000.0, 30.0)), PROFILE_LANDMARK);
    assert_eq!(prof(&mk("yes", None, None, t(Some("castle"), None), 1000.0, 30.0)), PROFILE_LANDMARK);
    assert_eq!(prof(&mk("yes", None, None, t(None, Some("chimney")), 1000.0, 4.0)), PROFILE_DARK);
    assert_eq!(prof(&mk("yes", None, None, t(None, Some("tower")), 1000.0, 30.0)), PROFILE_LANDMARK);
    assert_eq!(prof(&mk("weird", None, None, t(Some("city_gate"), None), 1000.0, 30.0)), PROFILE_LANDMARK);
    // A specific kind wins over historic / man_made.
    assert_eq!(prof(&mk("apartments", None, None, t(Some("castle"), Some("tower")), 1000.0, 30.0)), PROFILE_RESIDENTIAL);
    // An unknown historic value falls through to landuse.
    assert_eq!(prof(&mk("yes", None, None, t(Some("manor"), None), 1000.0, 30.0)), PROFILE_RESIDENTIAL);
    // 2. State tags beat the kind table.
    assert_eq!(prof(&mk("house", None, None, t(Some("ruins"), None), 1000.0, 10.0)), PROFILE_DARK);
    let st = |f: fn(&mut BuildingTags)| {
        let mut b = BuildingTags::default();
        f(&mut b);
        b
    };
    assert_eq!(prof(&mk("apartments", None, Some(5.0), st(|b| b.disused = true), 1000.0, 20.0)), PROFILE_DARK);
    assert_eq!(prof(&mk("castle", None, None, st(|b| b.ruins = true), 1000.0, 20.0)), PROFILE_DARK);
    assert_eq!(prof(&mk("office", None, None, st(|b| b.abandoned = true), 1000.0, 20.0)), PROFILE_DARK);
    // 4. Slender shape: tagged height 30 m on 8 x 8 m (30 >= 3 x 8, 64 < 150 m2) -> landmark.
    let tower = mk("yes", Some(30.0), None, t(None, None), 1000.0, 8.0);
    assert!(!tower.clipped && (tower.footprint_m2 - 64.0).abs() < 1.0, "{}", tower.footprint_m2);
    assert_eq!(prof(&tower), PROFILE_LANDMARK);
    assert_eq!(prof(&mk("yes", None, Some(10.0), t(None, None), 1000.0, 8.0)), PROFILE_LANDMARK, "tagged levels count");
    assert_eq!(prof(&mk("yes", Some(20.0), None, t(None, None), 1000.0, 8.0)), PROFILE_RESIDENTIAL, "too low");
    assert_eq!(prof(&mk("yes", Some(60.0), None, t(None, None), 1000.0, 13.0)), PROFILE_RESIDENTIAL, "169 m2 too big");
    assert_eq!(prof(&mk("house", Some(30.0), None, t(None, None), 1000.0, 8.0)), PROFILE_RESIDENTIAL, "specific kind");
    // ... not with a fallback height: a 5 x 5 m `yes` in residential landuse gets 15 m.
    let small = mk("yes", None, None, t(None, None), 1000.0, 5.0);
    assert_eq!(resolve(&small, &p).height, 15.0);
    assert!(p.is_slender_tower(15.0, small.footprint_m2), "the shape alone would pass");
    assert_eq!(prof(&small), PROFILE_RESIDENTIAL);
    // ... and not when the encoder clipped the footprint (a vertex on the -64 px buffer line).
    let clipped = mk("yes", Some(30.0), None, t(None, None), -64.0, 8.0);
    assert!(clipped.clipped);
    assert_eq!(prof(&clipped), PROFILE_RESIDENTIAL);
    assert!(!mk("yes", Some(30.0), None, t(None, None), -40.0, 8.0).clipped, "inside the buffer, not on its line");
    // 1. Override by OSM id wins over kind and state.
    let po = crate::profile::resolve(
        None,
        Some(&serde_json::json!({"buildings": {"overrides": {"w777": "landmark"}}})),
        &crate::NullLog,
    )
    .unwrap();
    let ruin = mk("garage", None, None, st(|b| b.ruins = true), 1000.0, 10.0);
    assert_eq!(resolve(&ruin, &p).profile, PROFILE_DARK);
    assert_eq!(resolve(&ruin, &po).profile, PROFILE_LANDMARK);
    let mut rel = ruin.clone();
    rel.osm_type = OsmType::Relation;
    assert_eq!(resolve(&rel, &po).profile, PROFILE_DARK, "w777 is not r777");
    // Windows flag and the `windows` tag (applied after resolution).
    assert!(resolve(&mk("apartments", None, None, t(None, None), 1000.0, 20.0), &p).windows);
    assert!(!resolve(&mk("castle", None, None, t(None, None), 1000.0, 20.0), &p).windows);
    assert!(!resolve(&mk("warehouse", None, None, t(None, None), 1000.0, 20.0), &p).windows);
    let mut no = mk("office", None, None, t(None, None), 1000.0, 20.0);
    no.tags.windows = Some(false);
    let r = resolve(&no, &p);
    assert_eq!((r.profile, r.windows), (PROFILE_OFFICE, false), "windows=no keeps the profile");
    let mut castle = mk("castle", None, None, t(None, None), 1000.0, 30.0);
    castle.tags.windows = Some(true);
    castle.landuse = Some(Landuse::Commercial);
    let r = resolve(&castle, &p);
    assert_eq!((r.profile, r.windows), (PROFILE_OFFICE, true), "windows=yes borrows the landuse fallback profile");
    assert_eq!(p.window_width_m(r.profile), Some(2.0));
    castle.landuse = Some(Landuse::Industrial); // fallback industrial has no windows -> residential
    let r = resolve(&castle, &p);
    assert_eq!((r.profile, r.windows), (PROFILE_RESIDENTIAL, true));
    assert_eq!(p.window_width_m(r.profile), Some(3.0));
    let mut yes_office = mk("office", None, None, t(None, None), 1000.0, 20.0);
    yes_office.tags.windows = Some(true);
    assert_eq!(resolve(&yes_office, &p).profile, PROFILE_OFFICE, "windows=yes on a windowed profile changes nothing");
    // Ids 6 and 7 reach COLOR r.
    assert_eq!(vertex_color(&resolve(&tower, &p))[0], 6.0 / 8.0);
    assert_eq!(vertex_color(&resolve(&ruin, &p))[0], 7.0 / 8.0);
}

#[test]
fn synthetic_children_match_parent_quarters_and_meshing_is_byte_identical() {
    let frame = Frame::new(VILNIUS.0, VILNIUS.1);
    let k15 = tile_for(VILNIUS.0, VILNIUS.1, 15);
    let p15 = TileProjector::new(k15, frame, 4096.0);
    let b15 = crate::synthetic::generate(k15, frame);
    let c15 = frame.tile_center(k15);
    let whole = mesh_tile(&b15, &p15, 1, &dp());
    let mut sum = 0;
    for (i, child) in crate::rings::children(k15).into_iter().enumerate() {
        // The parent's quarter i, clipped exactly, equals the child tile meshed alone.
        let q = mesh_buildings(&b15, &p15, ClipRect::quadrant(4096.0, i), lod1(), &dp());
        let p16 = TileProjector::new(child, frame, 4096.0);
        let b16 = crate::synthetic::generate(child, frame);
        let t16 = mesh_tile(&b16, &p16, 0, &dp());
        // Same buildings, same triangles (synthetic footprints are rectangles: DP 0.5 and
        // 2 m agree, nothing < 20 m2).
        assert_eq!(q.triangle_count(), t16.triangle_count(), "quarter {i} vs {child}");
        assert!(!q.is_empty());
        sum += q.triangle_count();
        // ... at the same world positions (tile-root offsets differ).
        let c16 = frame.tile_center(child);
        let (dx, dz) = ((c16[0] - c15[0]) as f32, -(c16[1] - c15[1]) as f32);
        let a = mesh_centroid(&q);
        let b = mesh_centroid(&t16);
        assert!((a[0] - (b[0] + dx)).abs() < 0.05 && (a[2] - (b[2] + dz)).abs() < 0.05);
        // Centroid ownership: every z16-owned building lies in quarter i of the parent.
        for bb in b15.iter().filter(|bb| bb.owned) {
            let qi = quadrant_of(4096.0, bb.centroid_px);
            let in_child = b16.iter().any(|x| x.id == bb.id && x.owned);
            assert_eq!(qi == i, in_child, "building {} quadrant {qi}", bb.id);
        }
    }
    assert_eq!(sum, whole.triangle_count());
    // Determinism: byte-identical buffers.
    let bits = |m: &MeshData| -> Vec<u32> {
        m.positions
            .iter()
            .flatten()
            .chain(m.normals.iter().flatten())
            .chain(m.colors.iter().flatten())
            .chain(m.uvs.iter().flatten())
            .map(|f| f.to_bits())
            .chain(m.indices.iter().map(|&i| i as u32))
            .collect()
    };
    let again = mesh_tile(&crate::synthetic::generate(k15, frame), &p15, 1, &dp());
    assert_eq!(bits(&whole), bits(&again));
    // R2 meshes (coarser, still non-empty); R3 has no mesh.
    let k14 = tile_for(VILNIUS.0, VILNIUS.1, 14);
    let p14 = TileProjector::new(k14, frame, 4096.0);
    let b14 = crate::synthetic::generate(k14, frame);
    let r2 = mesh_tile(&b14, &p14, 2, &dp());
    assert!(!r2.is_empty());
    assert_winding(&r2);
    assert_eq!(bits(&r2), bits(&mesh_tile(&b14, &p14, 2, &dp())));
    assert!(mesh_tile(&b14, &p14, 3, &dp()).is_empty());
}

#[test]
fn r2_drops_small_footprints_and_simplifies_harder() {
    let (p, m) = setup();
    // 6 x 6 m shed (36 m2): kept at R1, dropped at R2.
    let side = 6.0 / m;
    let shed = bld("yes", Some(4.0), None, vec![vec![sq(100.0, 100.0, side)]]);
    assert_eq!(mesh_tile(std::slice::from_ref(&shed), &p, 1, &dp()).triangle_count(), 10);
    assert!(mesh_tile(std::slice::from_ref(&shed), &p, 2, &dp()).is_empty());
    // A 3 m bump on a 40 m wall: kept at R1 (DP 2 m), removed at R2 (DP 4 m).
    let s = 40.0 / m;
    let bump = 3.0 / m;
    let ring = vec![[0.0, 0.0], [s * 0.5, -bump], [s, 0.0], [s, s], [0.0, s]];
    let b = bld("yes", Some(10.0), None, vec![vec![ring.into_iter().map(|q| [q[0] + 500.0, q[1] + 500.0]).collect()]]);
    let r1 = mesh_tile(std::slice::from_ref(&b), &p, 1, &dp()).triangle_count();
    let r2 = mesh_tile(std::slice::from_ref(&b), &p, 2, &dp()).triangle_count();
    assert_eq!(r1, 13); // 5 walls + 3 roof triangles
    assert_eq!(r2, 10); // box
}
