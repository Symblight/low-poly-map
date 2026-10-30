//! Tile assembly of the cities (`docs/spec-world-seed.md` sections 6.3 step 4 and 6.6): what a
//! city puts into one vector tile, in tile pixels (plan metres -> lat/lon -> pixels, never the
//! floating origin):
//!
//! * one ground area per land cell with its district's landuse kind (`cut`, so airport fences
//!   cut it), above the countryside underlay;
//! * the arterials (secondary) and radials (primary), lit;
//! * the local streets of every overlapping cell (not in R3, whose ground keeps secondary roads
//!   and more important only);
//! * every building overlapping the tile (plus the encoder buffer), owned by the tile holding its
//!   centroid; R3 tiles (no building mesh, lights only) get the owned ones only.

use super::lines::{bbox, boxes_overlap, grow};
use super::{CellKind, RoadKind};
use crate::geo::TileKey;
use crate::mvt::{centroid_px, BuildingTags, GroundArea, OsmType, RawBuilding, RoadLine, ENCODER_BUFFER_FRACTION};
use crate::seed::world::{px_of, SeedTile, SeedWorld, EXTENT};

/// Emit every city's content of tile `key` (lat/lon box `tb` = [w, s, e, n]) into `out`.
pub fn emit_city(key: TileKey, tb: [f64; 4], w: &SeedWorld, out: &mut SeedTile) {
    for ci in 0..w.cities().len() {
        let plan = w.city_plan(ci);
        if !(plan.bbox_ll[0] <= tb[2] && tb[0] <= plan.bbox_ll[2] && plan.bbox_ll[1] <= tb[3] && tb[1] <= plan.bbox_ll[3]) {
            continue;
        }
        let px = |p: [f64; 2]| {
            let (lat, lon) = plan.frame.ll(p);
            px_of(key, lat, lon)
        };
        // The tile in the city plane (with the encoder buffer).
        let corners = [plan.frame.xy(tb[1], tb[0]), plan.frame.xy(tb[3], tb[2])];
        let span = corners[1][0] - corners[0][0];
        let tbox = grow([corners[0][0], corners[0][1], corners[1][0], corners[1][1]], span * ENCODER_BUFFER_FRACTION + 1.0);
        for r in plan.roads.iter().filter(|r| boxes_overlap(r.bbox, tbox)) {
            let k = if r.kind == RoadKind::Arterial { w.city_kinds.arterial } else { w.city_kinds.radial };
            out.ground.roads.push(RoadLine { class: k.class, link: false, minor: false, lit: true, id: r.id, lines: vec![r.pts.iter().map(|p| px(*p)).collect()] });
        }
        for cell in plan.cells.values().filter(|c| c.kind == CellKind::Land && boxes_overlap(c.bbox, tbox)) {
            let Some(di) = cell.district else { continue };
            let d = &plan.districts[di as usize];
            let (class, level) = w.city_kinds.styles[d.style_index].area;
            let id = crate::seed_hash!(d.id, "area", cell.ax.0, cell.ax.1) & 0xFFFF_FFFF;
            out.ground.areas.push(GroundArea { class, level, id, rings: vec![cell.poly.iter().map(|p| px(*p)).collect()], tint: 0, sub: 0, cut: true });
            let cp = w.cell_plan(&plan, cell.ax);
            if key.z > 13 {
                for s in &cp.streets {
                    let pts: Vec<[f64; 2]> = s.pts.iter().map(|p| cp.abs(*p)).collect();
                    if boxes_overlap(bbox(&pts), tbox) {
                        out.ground.roads.push(RoadLine { class: s.class, link: false, minor: s.minor, lit: s.lit, id: s.id, lines: vec![pts.iter().map(|p| px(*p)).collect()] });
                    }
                }
            }
            for b in &cp.buildings {
                let ring: Vec<[f64; 2]> = b.ring.iter().map(|p| cp.abs(*p)).collect();
                if !boxes_overlap(bbox(&ring), tbox) {
                    continue;
                }
                let polygons = vec![vec![ring.iter().map(|p| px(*p)).collect::<Vec<_>>()]];
                if key.z == 13 && !centroid_px(&polygons).is_some_and(|c| (0.0..EXTENT).contains(&c[0]) && (0.0..EXTENT).contains(&c[1])) {
                    continue;
                }
                out.buildings.push(RawBuilding {
                    osm_type: OsmType::Way,
                    id: b.id,
                    kind: b.kind.clone(),
                    height: Some(b.height_m),
                    levels: Some(b.levels),
                    min_height: 0.0,
                    landuse: None,
                    tags: BuildingTags::default(),
                    polygons,
                });
            }
        }
    }
}
