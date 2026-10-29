//! Areas: clipped, simplified, triangulated with `earcutr`, airport fences cut out, draped.

use ahash::AHashMap;

use super::drape::{draped_h, Drape};
use super::{bbox, push_v, tri_up, AreaCtx, Scratch};
use crate::mesher::{clip_ring, simplify_ring, MeshData};
use crate::mvt::ring_area2;

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_area(
    m: &mut MeshData,
    s: &mut Scratch,
    rings: &[Vec<[f64; 2]>],
    ctx: &AreaCtx<'_>,
    tol_px: f64,
    lift: f32,
    color: [f32; 4],
    water: bool,
    cut: bool,
) {
    let rect = ctx.rect;
    let bb = bbox(&rings[0]);
    if bb[2] <= rect.x0 || bb[0] >= rect.x1 || bb[3] <= rect.y0 || bb[1] >= rect.y1 {
        return;
    }
    let needs_clip = bb[0] < rect.x0 || bb[2] > rect.x1 || bb[1] < rect.y0 || bb[3] > rect.y1;
    if needs_clip && crate::polyclip::hole_crosses(rings, rect) {
        // A hole reaches the clip boundary: clip the polygon as a whole so the hole notches
        // the outer ring (per-ring clipping would cover it).
        for piece in crate::polyclip::clip_polygon(rings, rect) {
            emit_area_rings(m, s, &piece, ctx, tol_px, lift, color, water, cut, true);
        }
    } else {
        emit_area_rings(m, s, rings, ctx, tol_px, lift, color, water, cut, false);
    }
}

/// [`emit_area`] for one polygon; `pre_clipped`: the rings are already inside the rect
/// ([`crate::polyclip::clip_polygon`]), vertices on the rect boundary stay locked.
#[allow(clippy::too_many_arguments)]
fn emit_area_rings(
    m: &mut MeshData,
    s: &mut Scratch,
    rings: &[Vec<[f64; 2]>],
    ctx: &AreaCtx<'_>,
    tol_px: f64,
    lift: f32,
    color: [f32; 4],
    water: bool,
    cut: bool,
    pre_clipped: bool,
) {
    let (proj, rect, field) = (ctx.proj, ctx.rect, ctx.field);
    s.flat.clear();
    s.hs.clear();
    s.pxs.clear();
    s.holes.clear();
    for (ri, ring) in rings.iter().enumerate() {
        let rb = bbox(ring);
        if ri > 0 && (rb[2] <= rect.x0 || rb[0] >= rect.x1 || rb[3] <= rect.y0 || rb[1] >= rect.y1) {
            continue; // hole entirely outside the tile
        }
        let needs_clip = pre_clipped || rb[0] < rect.x0 || rb[2] > rect.x1 || rb[1] < rect.y0 || rb[3] > rect.y1;
        if needs_clip && !pre_clipped {
            clip_ring(ring, rect, &mut s.clip_a, &mut s.clip_b);
        } else {
            s.clip_a.clear();
            s.clip_a.extend_from_slice(ring);
        }
        s.dedup.clear();
        for &p in &s.clip_a {
            if s.dedup.last() != Some(&p) {
                s.dedup.push(p);
            }
        }
        while s.dedup.len() > 1 && s.dedup.first() == s.dedup.last() {
            s.dedup.pop();
        }
        if s.dedup.len() < 3 {
            if ri == 0 {
                return;
            }
            continue;
        }
        s.locked.clear();
        s.locked.extend(s.dedup.iter().map(|&p| needs_clip && rect.flags(p) != 0));
        simplify_ring(&s.dedup, &s.locked, tol_px, &mut s.keep, &mut s.kept);
        if s.kept.len() < 3 {
            if ri == 0 {
                return;
            }
            continue;
        }
        let start = s.flat.len();
        for &i in &s.kept {
            let px = s.dedup[i];
            let (en, h) = if water { proj.project_water(px) } else { proj.project_h(px) };
            s.flat.push(en[0]);
            s.flat.push(en[1]);
            s.hs.push(draped_h(proj, px, h, field));
            s.pxs.push(px);
        }
        let (ring_en, _) = s.flat[start..].as_chunks::<2>();
        if ring_area2(ring_en).abs() < 1e-3 {
            // Degenerate (e.g. a sliver along a clip line): drop it.
            s.flat.truncate(start);
            s.hs.truncate(start / 2);
            s.pxs.truncate(start / 2);
            if ri == 0 {
                return;
            }
            continue;
        }
        if ri > 0 {
            s.holes.push(start / 2);
        }
    }
    let Ok(mut tris) = earcutr::earcut(&s.flat, &s.holes, 2) else { return };
    if cut && !ctx.cutouts.is_empty() {
        tris = cut_triangles(s, &tris, ctx, water);
    }
    if tris.is_empty() {
        return;
    }
    // Water drapes like every other area unless `terrain.water_snap_m` asks for the DEM-era snap
    // (then its corners are snapped to the lake level and it is not subdivided).
    if proj.terrain().is_active() && (!water || proj.terrain().water_snap_m() <= 0.0) {
        // Drape: split long triangle edges where the terrain is not flat along them.
        let mut d = Drape::new(proj, &s.pxs, &s.flat, &s.hs, field, lift as f64, ctx.step);
        let mut out = Vec::with_capacity(tris.len());
        for t in tris.as_chunks::<3>().0 {
            d.triangle([t[0] as u32, t[1] as u32, t[2] as u32], 0, &mut out);
        }
        let base = m.positions.len() as i32;
        for k in 0..d.en.len() {
            push_v(m, d.en[k], d.h[k] + lift as f64, color);
        }
        for t in out {
            tri_up(m, base + t[0] as i32, base + t[1] as i32, base + t[2] as i32);
        }
        return;
    }
    let base = m.positions.len() as i32;
    for k in 0..s.flat.len() / 2 {
        push_v(m, [s.flat[2 * k], s.flat[2 * k + 1]], s.hs[k] + lift as f64, color);
    }
    for t in tris.as_chunks::<3>().0 {
        tri_up(m, base + t[0] as i32, base + t[1] as i32, base + t[2] as i32);
    }
}

/// Remove the tile's cutouts (convex quads, a seed world's airport fences) from an area's
/// triangles (`tris` index `s.flat` / `s.hs` / `s.pxs`; new vertices are appended there). Each
/// triangle minus a convex quad is the union of convex pieces (outside edge 0; inside 0 and
/// outside 1; ...), fan-triangulated. New vertices lie on a quad edge; they are computed from the
/// triangle edge's endpoints in a canonical order, snapped to 2^-20 px and shared through a map,
/// so two triangles that share an edge cut it at the same vertex (no T-junction for the drape).
fn cut_triangles(s: &mut Scratch, tris: &[usize], ctx: &AreaCtx<'_>, water: bool) -> Vec<usize> {
    const Q: f64 = (1u64 << 20) as f64;
    let quads: Vec<([[f64; 2]; 4], [f64; 4], f64)> = ctx
        .cutouts
        .iter()
        .map(|q| {
            let area2: f64 = (0..4).map(|i| q[i][0] * q[(i + 1) % 4][1] - q[(i + 1) % 4][0] * q[i][1]).sum();
            (*q, bbox(q), area2.signum())
        })
        .collect();
    let mut index: AHashMap<(u64, u64), usize> = AHashMap::new();
    let mut out = Vec::with_capacity(tris.len());
    // Polygon vertex: (tile pixel, index into the vertex arrays or usize::MAX = new).
    type V = ([f64; 2], usize);
    let mut work: Vec<Vec<V>> = Vec::new();
    let mut next: Vec<Vec<V>> = Vec::new();
    for t in tris.as_chunks::<3>().0 {
        let tri: Vec<V> = t.iter().map(|&i| (s.pxs[i], i)).collect();
        let tb = bbox(&[tri[0].0, tri[1].0, tri[2].0]);
        work.clear();
        work.push(tri);
        for (q, qb, sign) in &quads {
            if tb[2] <= qb[0] || tb[0] >= qb[2] || tb[3] <= qb[1] || tb[1] >= qb[3] {
                continue;
            }
            next.clear();
            for poly in work.drain(..) {
                // Signed side of edge i: > 0 inside the quad.
                let mut rest = poly;
                for i in 0..4 {
                    let (a, b) = (q[i], q[(i + 1) % 4]);
                    let f = |p: [f64; 2]| sign * ((b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]));
                    let outside = clip_half(&rest, |p| -f(p));
                    if outside.len() >= 3 {
                        next.push(outside);
                    }
                    rest = clip_half(&rest, f);
                    if rest.len() < 3 {
                        break;
                    }
                }
            }
            std::mem::swap(&mut work, &mut next);
        }
        for poly in &work {
            let ids: Vec<usize> = poly
                .iter()
                .map(|&(p, i)| {
                    if i != usize::MAX {
                        return i;
                    }
                    let p = [(p[0] * Q).round() / Q, (p[1] * Q).round() / Q];
                    *index.entry((p[0].to_bits(), p[1].to_bits())).or_insert_with(|| {
                        let (en, h) = if water { ctx.proj.project_water(p) } else { ctx.proj.project_h(p) };
                        s.flat.push(en[0]);
                        s.flat.push(en[1]);
                        s.hs.push(draped_h(ctx.proj, p, h, ctx.field));
                        s.pxs.push(p);
                        s.hs.len() - 1
                    })
                })
                .collect();
            for k in 1..ids.len() - 1 {
                out.extend_from_slice(&[ids[0], ids[k], ids[k + 1]]);
            }
        }
    }
    out
}

/// Sutherland-Hodgman against one half-plane: keeps the part where `f >= 0`. Crossing points are
/// computed from the edge's endpoints in lexicographic order, so both triangles of a shared
/// edge get the same bits.
fn clip_half(poly: &[([f64; 2], usize)], f: impl Fn([f64; 2]) -> f64) -> Vec<([f64; 2], usize)> {
    let n = poly.len();
    let mut out = Vec::with_capacity(n + 2);
    for k in 0..n {
        let (p, q) = (poly[k], poly[(k + 1) % n]);
        let (fp, fq) = (f(p.0), f(q.0));
        if fp >= 0.0 {
            out.push(p);
        }
        if (fp > 0.0 && fq < 0.0) || (fp < 0.0 && fq > 0.0) {
            let (a, b, fa, fb) = if (p.0[0], p.0[1]) <= (q.0[0], q.0[1]) { (p.0, q.0, fp, fq) } else { (q.0, p.0, fq, fp) };
            let t = fa / (fa - fb);
            out.push(([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t], usize::MAX));
        }
    }
    out
}
