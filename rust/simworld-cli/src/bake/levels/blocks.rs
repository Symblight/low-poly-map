//! Overview blocks: buildings closer than `block_gap_m` merged into one footprint with an
//! area-weighted height.

use std::collections::{BTreeMap, HashMap};

use geo::algorithm::buffer::{Buffer, BufferStyle, LineJoin};
use rayon::prelude::*;

use super::{is_landmark, polys_into_tiles, round_step, Counts, Feat, LayerTiles, Params};
use crate::bake::geom::{self, m_per_g, MPoly, Rect};
use crate::bake::input::{Building, Props};
use crate::bake::mvt_enc::Val;

/// A merged block in global units.
pub struct Block {
    pub polys: MPoly,
    pub height_m: f64,
    pub kind: String,
    pub id: u64,
    pub members: usize,
}

struct Dsu(Vec<u32>);
impl Dsu {
    fn find(&mut self, mut i: u32) -> u32 {
        while self.0[i as usize] != i {
            self.0[i as usize] = self.0[self.0[i as usize] as usize];
            i = self.0[i as usize];
        }
        i
    }
    fn join(&mut self, a: u32, b: u32) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[a.max(b) as usize] = a.min(b);
        }
    }
}

/// Clusters of buildings whose footprints are within `block_gap_m` of each other.
pub fn clusters(cand: &[&Building], gap_m: f64) -> Vec<Vec<usize>> {
    const CELL: f64 = 2048.0;
    let gap_g: Vec<f64> = cand.iter().map(|b| gap_m / m_per_g(b.centroid[1])).collect();
    let rects: Vec<Rect> = cand.iter().zip(&gap_g).map(|(b, g)| Rect::of_mpoly(&b.polys).grow(g / 2.0)).collect();
    let mut cells: HashMap<(i64, i64), Vec<u32>> = HashMap::new();
    for (i, r) in rects.iter().enumerate() {
        for cy in (r.y0 / CELL).floor() as i64..=(r.y1 / CELL).floor() as i64 {
            for cx in (r.x0 / CELL).floor() as i64..=(r.x1 / CELL).floor() as i64 {
                cells.entry((cx, cy)).or_default().push(i as u32);
            }
        }
    }
    // Candidate pairs (bbox overlap), distance tested in parallel, joined in a fixed order.
    let mut keys: Vec<&(i64, i64)> = cells.keys().collect();
    keys.sort();
    let pairs: Vec<(u32, u32)> = keys
        .par_iter()
        .flat_map_iter(|k| {
            let v = &cells[*k];
            let mut out = Vec::new();
            for (ai, &a) in v.iter().enumerate() {
                for &b in &v[ai + 1..] {
                    if !rects[a as usize].intersects(&rects[b as usize]) {
                        continue;
                    }
                    // Test each pair once: in the cell holding the overlap's top-left corner.
                    let (ra, rb) = (rects[a as usize], rects[b as usize]);
                    let (ox, oy) = (ra.x0.max(rb.x0), ra.y0.max(rb.y0));
                    if ((ox / CELL).floor() as i64, (oy / CELL).floor() as i64) != **k {
                        continue;
                    }
                    let g = (gap_g[a as usize] + gap_g[b as usize]) / 2.0;
                    if geom::mpoly_distance(&cand[a as usize].polys, &cand[b as usize].polys) <= g {
                        out.push((a, b));
                    }
                }
            }
            out
        })
        .collect();
    let mut dsu = Dsu((0..cand.len() as u32).collect());
    for (a, b) in pairs {
        dsu.join(a, b);
    }
    let mut groups: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for i in 0..cand.len() {
        let r = dsu.find(i as u32);
        groups.entry(r).or_default().push(i);
    }
    groups.into_values().collect()
}

/// Blocks of one cluster: buffer by gap/2, union, assign members, shrink by gap/2, simplify,
/// keep >= min area. Returns the blocks and how many members ended in a kept block.
fn cluster_blocks(members: &[&Building], p: &Params) -> (Vec<Block>, usize, f64) {
    let o = members[0].polys[0][0][0];
    let scale = m_per_g(members[0].centroid[1]); // g -> local metres
    let r = p.block_gap_m / 2.0;
    let style = |d: f64| BufferStyle::new(d).line_join(LineJoin::Miter(0.4));
    // Components in local metres with their member indices.
    let comps: Vec<(MPoly, Vec<usize>)> = if members.len() == 1 {
        vec![(geom::from_geo(&geom::to_geo(&members[0].polys, o, scale), [0.0, 0.0], 1.0), vec![0])]
    } else {
        let buffered: Vec<geo::Polygon<f64>> = members.iter().flat_map(|b| geom::to_geo(&b.polys, o, scale).buffer_with_style(style(r)).0).collect();
        let u = geom::from_geo(&geo::unary_union(&buffered), [0.0, 0.0], 1.0);
        let mut owner: Vec<Vec<usize>> = vec![Vec::new(); u.len()];
        for (i, b) in members.iter().enumerate() {
            let v = b.polys[0][0][0];
            let pt = [(v[0] - o[0]) * scale, (v[1] - o[1]) * scale];
            let c = u.iter().position(|poly| geom::point_in_poly(pt, poly)).unwrap_or_else(|| {
                // On a boundary by rounding: the nearest component.
                (0..u.len())
                    .min_by(|&a, &b| {
                        let d = |k: usize| geom::mpoly_distance(&vec![u[k].clone()], &vec![vec![vec![pt, [pt[0] + 1e-6, pt[1]], [pt[0], pt[1] + 1e-6]]]]);
                        d(a).total_cmp(&d(b))
                    })
                    .unwrap_or(0)
            });
            owner[c].push(i);
        }
        u.into_iter()
            .zip(owner)
            .filter(|(_, m)| !m.is_empty())
            .map(|(poly, m)| {
                let shrunk = geom::to_geo(&vec![poly], [0.0, 0.0], 1.0).buffer_with_style(style(-r));
                (geom::from_geo(&shrunk, [0.0, 0.0], 1.0), m)
            })
            .collect()
    };
    let mut out = Vec::new();
    let mut kept = 0;
    let mut kept_m2 = 0.0;
    for (m, idx) in comps {
        let s = geom::simplify_mpoly(&m, p.block_simplify_m);
        let area = geom::mpoly_area(&s);
        if area < p.block_min_area_m2 {
            continue;
        }
        let (mut sw, mut sh) = (0.0, 0.0);
        let mut kinds: BTreeMap<&str, f64> = BTreeMap::new();
        for &i in &idx {
            let b = members[i];
            sw += b.area_m2;
            sh += b.area_m2 * b.roof_m as f64;
            *kinds.entry(b.kind.as_str()).or_default() += b.area_m2;
        }
        let h = if sw > 0.0 { sh / sw } else { 0.0 };
        let kind = kinds.iter().max_by(|a, b| a.1.total_cmp(b.1).then(b.0.cmp(a.0))).map(|k| k.0.to_string()).unwrap_or_else(|| "yes".into());
        let id = idx.iter().map(|&i| members[i].key.1).min().unwrap_or(0);
        kept += idx.len();
        kept_m2 += sw;
        let polys = geom::from_geo(&geom::to_geo(&s, [0.0, 0.0], 1.0), o, scale);
        out.push(Block { polys, height_m: round_step(h, p.height_step_m), kind, id, members: idx.len() });
    }
    (out, kept, kept_m2)
}

pub fn blocks(bs: &[Building], p: &Params, counts: &mut Counts) -> (LayerTiles, Vec<Block>) {
    let cand: Vec<&Building> = bs.iter().filter(|b| !is_landmark(b, p)).collect();
    let cl = clusters(&cand, p.block_gap_m);
    counts.block_candidates = cand.len();
    counts.clusters = cl.len();
    counts.candidate_m2 = cand.iter().map(|b| b.area_m2).sum();
    let res: Vec<(Vec<Block>, usize, f64)> = cl
        .par_iter()
        .map(|idx| {
            let members: Vec<&Building> = idx.iter().map(|&i| cand[i]).collect();
            cluster_blocks(&members, p)
        })
        .collect();
    let mut all = Vec::new();
    for (b, k, m2) in res {
        counts.buildings_in_blocks += k;
        counts.in_blocks_m2 += m2;
        all.extend(b);
    }
    counts.blocks = all.len();
    let (z, ext, buf) = (p.overview_z, p.overview_extent, p.buffer_g(p.overview_z));
    let mut t = LayerTiles::new();
    for b in &all {
        let props: Props = vec![
            ("id".into(), Val::F64(b.id as f64)),
            ("kind".into(), Val::Str(b.kind.clone())),
            ("height".into(), Val::F64(b.height_m)),
            ("count".into(), Val::I64(b.members as i64)),
        ];
        polys_into_tiles(&b.polys, z, ext, buf, |k, g| t.entry(k).or_default().push(Feat { id: None, props: props.clone(), geom: g }));
    }
    (t, all)
}
