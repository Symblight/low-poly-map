//! One prepared polygon -> walls ([`super::walls`]) plus earcut roof and bottom caps.

use super::walls::{emit_walls_flat, emit_walls_on, gv, GroundWalls, WallLayout};
use super::{MeshData, Resolved, Scratch};

/// Wall UV (contract section 7, docs/spec-night-windows.md section 2):
/// - windowed walls: `u` in window columns (each run of a ring between corners holds
///   `round(len / width)` whole columns; the count continues across the rings of the polygon,
///   outer ring first, then holes), `v` in floors (`max(1, round(wall_h / floor_h))` whole
///   floors stretched to the wall height; top = the floor count exactly);
/// - windowless walls (no window width, wall lower than `min_window_wall_m`, a run too short
///   for one column): `u = -1` on all four vertices, `v` in meters up from the extrusion bottom.
pub(super) fn emit_polygon(m: &mut MeshData, s: &mut Scratch, res: &Resolved, color: [f32; 4], base: f32, walls: WallLayout, ground: Option<GroundWalls<'_>>) {
    // Heights stand on the terrain under the building (`Building::base_m`, the highest ground
    // under the footprint; 0 while flat).
    let y0 = base + res.min_height;
    let y1 = base + res.height;
    // The extrusion height (= wall height, roofs are flat), independent of the terrain base.
    let wall_h = res.height - res.min_height;
    let window_w = walls.window_w.filter(|w| *w > 0.0 && wall_h >= walls.min_window_wall_m);
    let floors = (wall_h / res.floor_h.max(0.1)).round().max(1.0);
    // Terrain on and the part stands on the ground: every wall reaches down to the exact ground
    // at its vertices (per-vertex bottoms, long edges subdivided over valleys), the windows keep
    // hanging from the roof (v below the nominal bottom goes negative on the downhill side), and
    // a windowless foundation reaches `foundation_m` further down.
    if let Some(g) = ground.filter(|_| res.min_height <= 0.25) {
        emit_walls_on(m, s, color, y0, y1, wall_h, floors, window_w, walls.foundation_m, g);
    } else {
        emit_walls_flat(m, s, color, y0, y1, wall_h, floors, window_w);
    }
    // Roof (and bottom for raised parts) via earcut with holes.
    s.flat.clear();
    s.holes.clear();
    for (i, ring) in s.rings.iter().enumerate() {
        if i > 0 {
            s.holes.push(s.flat.len() / 2);
        }
        for p in ring {
            s.flat.push(p[0]);
            s.flat.push(p[1]);
        }
    }
    let Ok(tris) = earcutr::earcut(&s.flat, &s.holes, 2) else { return };
    if tris.is_empty() {
        return;
    }
    let caps: &[(f32, f32)] = if res.min_height > 0.25 { &[(1.0, y1), (-1.0, y0)] } else { &[(1.0, y1)] };
    for &(dir, y) in caps {
        let base = m.positions.len() as i32;
        for k in 0..s.flat.len() / 2 {
            let p = [s.flat[2 * k], s.flat[2 * k + 1]];
            let v = gv(p, y);
            m.positions.push(v);
            m.normals.push([0.0, dir, 0.0]);
            m.colors.push(color);
            m.uvs.push([v[0], v[2]]);
        }
        for t in tris.as_chunks::<3>().0 {
            let (a, b, c) = (t[0], t[1], t[2]);
            // ((c - a) x (b - a)).y == cross_EN(c - a, b - a), so a front face toward +y needs
            // (a, b, c) clockwise in the EN plane; toward -y counter-clockwise.
            let pa = [s.flat[2 * a], s.flat[2 * a + 1]];
            let pb = [s.flat[2 * b], s.flat[2 * b + 1]];
            let pc = [s.flat[2 * c], s.flat[2 * c + 1]];
            let cr = (pb[0] - pa[0]) * (pc[1] - pa[1]) - (pb[1] - pa[1]) * (pc[0] - pa[0]);
            if cr.abs() < 2e-3 {
                continue; // < 0.001 m2 sliver (collinear clip-line vertices); f32 can't orient it
            }
            let ccw = cr > 0.0;
            let (b, c) = if ccw != (dir > 0.0) { (b, c) } else { (c, b) };
            m.indices.extend_from_slice(&[base + a as i32, base + b as i32, base + c as i32]);
        }
    }
}
