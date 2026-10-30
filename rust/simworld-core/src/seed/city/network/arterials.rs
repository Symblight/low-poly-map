//! Arterials (`docs/spec-world-seed.md` section 5.3 step 6), from the zoning's candidate edges.
//!
//! * **Drop:** the spanning tree of the candidate graph (Kruskal over the hex corners, weight
//!   `unit(edge seed)`, ties by edge key) is kept whole; of the other candidates between
//!   different requests, a share `arterial_drop` (by hash) is dropped unless a cell would be left
//!   without an arterial. So the drop never splits the network.
//! * **Water:** an arterial crossing water is kept as a bridge when its crossing is at most
//!   `bridge_max_m` and no other bridge lies within `bridge_spacing_km` (shortest crossings
//!   first); otherwise only its dry runs remain. Runs inside an airport fence are cut.

use std::collections::BTreeMap;

use crate::seed::city::context::CityContext;
use crate::seed::city::hex::{add, corner_key, edge_key, Ax, Dsu, DIRS};
use crate::seed::city::lines::{densify, len, ok_runs};
use crate::seed::city::zoning::Zoning;
use crate::seed::city::RoadKind;
use super::{road, MajorNetwork};
use crate::seed::hash::{edge_seed, unit};
use crate::seed::water::WaterHit;
use crate::seed::world::SeedWorld;

/// A bridge candidate: (crossing length, edge, curve, midpoint).
type Bridge = (f64, (Ax, Ax), Vec<[f64; 2]>, [f64; 2]);

/// The arterial roads and their statistics.
pub(super) fn build(net: &mut MajorNetwork, ctx: &CityContext, zoning: &Zoning, w: &SeedWorld) {
    let cp = &w.params.city;
    let (req, edges, capped) = (&zoning.request, &zoning.edges, &zoning.capped);
    // Spanning tree over the corners, then the drop.
    let cand: Vec<(Ax, Ax)> = edges.iter().filter(|e| e.2).map(|e| (e.0, e.1)).collect();
    let corners = |a: Ax, b: Ax| {
        let k = DIRS.iter().position(|d| add(a, *d) == b).expect("neighbours");
        (corner_key(a, k + 5), corner_key(a, k))
    };
    let mut node: BTreeMap<[Ax; 3], usize> = BTreeMap::new();
    for &(a, b) in &cand {
        let (c0, c1) = corners(a, b);
        for c in [c0, c1] {
            let n = node.len();
            node.entry(c).or_insert(n);
        }
    }
    let mut order: Vec<usize> = (0..cand.len()).collect();
    let weight = |i: usize| unit(edge_seed(ctx.seed, cand[i].0, cand[i].1).0);
    order.sort_by(|&i, &j| weight(i).total_cmp(&weight(j)).then(cand[i].cmp(&cand[j])));
    let mut tree = Dsu::new(node.len());
    let mut in_tree = vec![false; cand.len()];
    for i in order {
        let (c0, c1) = corners(cand[i].0, cand[i].1);
        in_tree[i] = tree.union(node[&c0], node[&c1]);
    }
    let components = |keep: &dyn Fn(usize) -> bool| {
        let mut d = Dsu::new(node.len());
        let mut used = vec![false; node.len()];
        for &(a, b) in cand.iter().enumerate().filter(|(i, _)| keep(*i)).map(|(_, e)| e) {
            let (c0, c1) = corners(a, b);
            used[node[&c0]] = true;
            used[node[&c1]] = true;
            d.union(node[&c0], node[&c1]);
        }
        (0..node.len()).filter(|&n| used[n] && d.find(n) == n).count() as u32
    };
    net.arterial_components_before = components(&|_| true);
    let mut roads_of: BTreeMap<Ax, u32> = BTreeMap::new();
    for &(a, b) in &cand {
        *roads_of.entry(a).or_default() += 1;
        *roads_of.entry(b).or_default() += 1;
    }
    let mut keep = vec![true; cand.len()];
    for (i, &(a, b)) in cand.iter().enumerate() {
        let capped_edge = capped.contains(&(a, b));
        if in_tree[i] || capped_edge || req[&a] == req[&b] {
            continue;
        }
        let drop = unit(crate::seed_hash!(ctx.seed, "drop", a.0, a.1, b.0, b.1)) < cp.arterial_drop;
        if drop && roads_of[&a] > 1 && roads_of[&b] > 1 {
            keep[i] = false;
            *roads_of.get_mut(&a).expect("counted") -= 1;
            *roads_of.get_mut(&b).expect("counted") -= 1;
        }
    }
    net.arterial_components = components(&|i| keep[i]);
    net.arterial_edges = keep.iter().filter(|k| **k).count() as u32;

    // Water and fences.
    let wet = |p: [f64; 2]| {
        let (lat, lon) = ctx.frame.ll(p);
        w.water.water_at(lat, lon) != WaterHit::Land
    };
    let fenced = |p: [f64; 2]| {
        let (lat, lon) = ctx.frame.ll(p);
        w.terrain.strips.in_fence(lat, lon)
    };
    // (crossing length, edge, curve, midpoint)
    let mut bridges: Vec<Bridge> = Vec::new();
    let mut roads = Vec::new();
    for (i, &(a, b)) in cand.iter().enumerate().filter(|(i, _)| keep[*i]) {
        let curve = ctx.grid.edge_curve(ctx.seed, a, b, cp.edge_warp);
        let id = edge_seed(ctx.seed, a, b).0 & 0xFFFF_FFFF;
        let fine = densify(&curve, 20.0);
        let n_wet = fine.iter().filter(|p| wet(**p)).count();
        if n_wet == 0 && !fine.iter().any(|p| fenced(*p)) {
            roads.push(road(id, RoadKind::Arterial, curve));
            continue;
        }
        let crossing = n_wet as f64 * len(fine[0], fine[1]);
        if n_wet > 0 && crossing <= cp.bridge_max_m && !fine.iter().any(|p| fenced(*p)) {
            let mid = fine[fine.len() / 2];
            bridges.push((crossing, edge_key(cand[i].0, cand[i].1), curve, mid));
            continue;
        }
        for run in ok_runs(&fine, &|p| !wet(p) && !fenced(p), 30.0) {
            roads.push(road(id, RoadKind::Arterial, run));
        }
    }
    bridges.sort_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));
    let spacing = cp.bridge_spacing_km * 1000.0;
    let mut accepted: Vec<[f64; 2]> = Vec::new();
    for (_, (a, b), curve, mid) in bridges {
        let id = edge_seed(ctx.seed, a, b).0 & 0xFFFF_FFFF;
        if accepted.iter().all(|m| len(*m, mid) >= spacing) {
            accepted.push(mid);
            roads.push(road(id, RoadKind::Arterial, curve));
        } else {
            for run in ok_runs(&densify(&curve, 20.0), &|p| !wet(p), 30.0) {
                roads.push(road(id, RoadKind::Arterial, run));
            }
        }
    }
    net.bridges = accepted.len() as u32;
    roads.sort_by(|x, y| x.id.cmp(&y.id).then(x.pts[0][0].total_cmp(&y.pts[0][0])).then(x.pts[0][1].total_cmp(&y.pts[0][1])));
    net.roads.extend(roads);
}
