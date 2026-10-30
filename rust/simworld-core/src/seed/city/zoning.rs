//! Stage 3a, zoning (`docs/spec-seed-city-pipeline.md` section 5.4; generator 3:
//! `docs/spec-world-seed.md` section 5.3 steps 3 and 4). Generator 3 zones on the hex cells
//! before the major network is built (the arterials follow the district boundaries).
//!
//! * **Placement:** per request in file order, candidates are the unassigned land cells within
//!   `rings` and `near`; the score is the ring distance to the preferred ring + 0.5 x the hex
//!   distance to the request's cells so far (region growing) + 0.25 x a hash + the approach
//!   penalty for tall districts; ties break on (q, r). The rest of the land is the fill.
//! * **Merge:** a hex edge between two land cells of different requests, or on a ring boundary
//!   (rings 0|1, 2|3, ...), is an arterial candidate. Same-request cells merge (union-find in
//!   edge-key order) while the merged area stays within `max_district_m2`; an edge whose merge
//!   would cross the cap is a candidate too. Each merged group is one [`District`], its street
//!   lattice along the first candidate edge of its first cell.

use std::collections::BTreeMap;

use super::context::CityContext;
use super::hex::{add, corner_key, dist, edge_key, Ax, Dsu, DIRS};
use super::lines::len;
use super::{style_index, style_params, Cell, CellKind, District, StageHash};
use crate::seed::files::{City, DistrictType, NearFeature, Style};
use crate::seed::hash::{district_seed, unit, Seed};
use crate::seed::world::SeedWorld;

/// Output of stage 3a: the districts, the district of every land cell and the arterial candidate
/// edges the major network starts from.
#[derive(Clone, Debug)]
pub struct Zoning {
    /// Request index of every land cell (`districts.len()` of the city file = the fill).
    pub request: BTreeMap<Ax, usize>,
    pub districts: Vec<District>,
    /// District of every land cell, in cell order.
    pub cell_district: Vec<(Ax, u32)>,
    /// Edges between land cells in key order: (a, b, arterial candidate).
    pub edges: Vec<(Ax, Ax, bool)>,
    /// Candidates made by the merge cap (never dropped).
    pub capped: Vec<(Ax, Ax)>,
    /// Placement shortfalls.
    pub warnings: Vec<String>,
}

impl Zoning {
    /// Stage hash: requests, districts, cell districts, edges and warnings.
    pub fn hash(&self) -> u64 {
        let mut h = StageHash::new("zoning");
        for (a, r) in &self.request {
            h.ax(*a).u(*r as u64);
        }
        for d in &self.districts {
            h.u(d.id.0).u(d.kind as u64).u(d.style as u64).u(d.style_index as u64).u(d.request as u64);
            h.f(d.max_height_m).f(d.density).f(d.lit).u(d.landmarks as u64).u(d.cells.len() as u64);
            for a in &d.cells {
                h.ax(*a);
            }
            h.pts(&[d.origin, d.dir]);
        }
        for (a, g) in &self.cell_district {
            h.ax(*a).u(*g as u64);
        }
        for (a, b, c) in &self.edges {
            h.ax(*a).ax(*b).u(*c as u64);
        }
        for (a, b) in &self.capped {
            h.ax(*a).ax(*b);
        }
        for s in &self.warnings {
            h.s(s);
        }
        h.finish()
    }
}

/// Stage 3a of `city` on its context.
pub fn build(ctx: &CityContext, city: &City, w: &SeedWorld) -> Zoning {
    let mut warnings = Vec::new();
    let request = place(ctx, city, w, &mut warnings);
    let mut z = Zoning { request, districts: Vec::new(), cell_district: Vec::new(), edges: Vec::new(), capped: Vec::new(), warnings };
    merge(&mut z, ctx, city, w);
    z
}

fn near_ok(c: &Cell, f: &[NearFeature]) -> bool {
    f.iter().any(|f| match f {
        NearFeature::Water => c.near.water,
        NearFeature::Coast => c.near.coast,
        NearFeature::River => c.near.river,
        NearFeature::Airport => c.near.airport,
    })
}

/// Placement: the request index of every land cell (`districts.len()` = the fill). Shortfalls
/// are plan warnings.
fn place(ctx: &CityContext, city: &City, w: &SeedWorld, warnings: &mut Vec<String>) -> BTreeMap<Ax, usize> {
    let land: Vec<Ax> = ctx.cells.values().filter(|c| c.kind == CellKind::Land).map(|c| c.ax).collect();
    let mut assigned: BTreeMap<Ax, usize> = BTreeMap::new();
    for (ri, req) in city.districts.iter().enumerate() {
        let want = match (req.cells, req.share) {
            (Some(n), _) => n as usize,
            (None, Some(s)) => ((s * land.len() as f64).round() as usize).max(1),
            _ => 0,
        };
        let near: Option<Vec<NearFeature>> = req.near.as_ref().map(|n| n.features());
        let max_h = req.max_height_m.or(city.max_height_m).unwrap_or(0.0);
        let tall = max_h > w.params.city.approach_tall_m;
        let lo = req.rings.map_or(0, |r| r[0]);
        let ring_pref = req.rings.is_some() || req.kind == DistrictType::OldTown;
        // (cell, base score, min hex distance to the request's cells so far)
        let mut cand: Vec<(Ax, f64, u32)> = land
            .iter()
            .filter(|a| !assigned.contains_key(a))
            .map(|a| &ctx.cells[a])
            .filter(|c| req.rings.is_none_or(|r| (r[0]..=r[1]).contains(&c.ring)))
            .filter(|c| near.as_ref().is_none_or(|f| near_ok(c, f)))
            .map(|c| {
                let ring_term = if ring_pref { (c.ring as f64 - lo as f64).abs() } else { 0.0 };
                let h = unit(crate::seed_hash!(ctx.seed, "place", ri, c.ax.0, c.ax.1)) * 0.25;
                let appr = if tall { w.params.city.approach_penalty * c.approach } else { 0.0 };
                (c.ax, ring_term + h + appr, u32::MAX)
            })
            .collect();
        let mut got = 0;
        while got < want && !cand.is_empty() {
            let score = |e: &(Ax, f64, u32)| e.1 + if e.2 == u32::MAX { 0.0 } else { 0.5 * e.2 as f64 };
            let best = (0..cand.len())
                .min_by(|&i, &j| score(&cand[i]).total_cmp(&score(&cand[j])).then(cand[i].0.cmp(&cand[j].0)))
                .expect("non-empty");
            let (a, _, _) = cand.swap_remove(best);
            assigned.insert(a, ri);
            got += 1;
            for e in cand.iter_mut() {
                e.2 = e.2.min(dist(e.0, a));
            }
        }
        if got < want {
            warnings.push(format!("city {}: district request {ri} ({}) got {got} of {want} cells", city.id, req.kind.name()));
        }
    }
    let fill = city.districts.len();
    for a in land {
        assigned.entry(a).or_insert(fill);
    }
    assigned
}

/// District parameters of request `ri` (`districts.len()` = the fill).
fn district_params(city: &City, ri: usize, w: &SeedWorld) -> (DistrictType, Style, f64, f64, f64, u32) {
    let (kind, style, mh, dens, lit, lm) = match city.districts.get(ri) {
        Some(r) => (r.kind, r.style, r.max_height_m, r.density, r.lit, r.landmarks),
        None => (city.fill.kind, city.fill.style, city.fill.max_height_m, city.fill.density, city.fill.lit, None),
    };
    let style = style.unwrap_or(kind.default_style());
    let s = style_params(&w.params.styles, style_index(kind, style));
    let landmarks = lm.unwrap_or(if kind == DistrictType::OldTown { 1 } else { 0 });
    (
        kind,
        style,
        mh.or(city.max_height_m).unwrap_or(s.max_height_m),
        dens.or(city.density).unwrap_or(s.density),
        lit.unwrap_or(s.street_lit),
        landmarks,
    )
}

/// Merge: the candidate edges, the capped merges and the districts.
fn merge(z: &mut Zoning, ctx: &CityContext, city: &City, w: &SeedWorld) {
    let cp = &w.params.city;
    let req = &z.request;
    let land: Vec<Ax> = req.keys().copied().collect();
    let idx: BTreeMap<Ax, usize> = land.iter().enumerate().map(|(i, a)| (*a, i)).collect();
    // Edges between land cells, in key order: (a, b, candidate).
    let mut edges: Vec<(Ax, Ax, bool)> = Vec::new();
    for &a in &land {
        for d in DIRS {
            let b = add(a, d);
            if a < b && req.contains_key(&b) {
                let (ra, rb) = (ctx.cells[&a].ring, ctx.cells[&b].ring);
                let boundary = ra != rb && ra.min(rb) % 2 == 0;
                edges.push((a, b, req[&a] != req[&b] || boundary));
            }
        }
    }
    edges.sort_by_key(|e| (e.0, e.1));
    let cap_cells = ((cp.max_district_m2 / ctx.grid.cell_area()).floor() as usize).max(1);
    let mut dsu = Dsu::new(land.len());
    let mut capped = Vec::new();
    for e in edges.iter_mut().filter(|e| !e.2) {
        let (i, j) = (idx[&e.0], idx[&e.1]);
        let (ri, rj) = (dsu.find(i), dsu.find(j));
        if ri != rj && dsu.size[ri] + dsu.size[rj] > cap_cells {
            e.2 = true;
            capped.push((e.0, e.1));
        } else {
            dsu.union(i, j);
        }
    }
    // Districts: one per group, ordered by their first cell.
    let mut districts: Vec<District> = Vec::new();
    let mut cell_district = Vec::with_capacity(land.len());
    let mut group_of: BTreeMap<usize, u32> = BTreeMap::new();
    for (i, a) in land.iter().enumerate() {
        let root = dsu.find(i);
        let n = group_of.len() as u32;
        let g = *group_of.entry(root).or_insert(n);
        if g as usize == districts.len() {
            let (kind, style, max_height_m, density, lit, landmarks) = district_params(city, req[a], w);
            let id = district_seed(ctx.seed, a.0, a.1, 0);
            let dir = lattice_dir(ctx, *a, &edges, id);
            districts.push(District {
                id,
                kind,
                style,
                style_index: style_index(kind, style),
                request: req[a],
                max_height_m,
                density,
                lit,
                landmarks,
                cells: Vec::new(),
                origin: ctx.grid.center(*a),
                dir,
            });
        }
        districts[g as usize].cells.push(*a);
        cell_district.push((*a, g));
    }
    z.districts = districts;
    z.cell_district = cell_district;
    z.edges = edges;
    z.capped = capped;
}

/// Lattice direction of a district: the chord of the first candidate edge of its first cell (by
/// neighbour order), else an angle from the district seed.
fn lattice_dir(ctx: &CityContext, a: Ax, edges: &[(Ax, Ax, bool)], id: Seed) -> [f64; 2] {
    for (k, d) in DIRS.iter().enumerate() {
        let b = add(a, *d);
        let (lo, hi) = edge_key(a, b);
        if edges.binary_search_by_key(&(lo, hi), |e| (e.0, e.1)).is_ok_and(|i| edges[i].2) {
            let (p0, p1) = (ctx.grid.corner(corner_key(a, k + 5)), ctx.grid.corner(corner_key(a, k)));
            let l = len(p0, p1);
            return [(p1[0] - p0[0]) / l, (p1[1] - p0[1]) / l];
        }
    }
    let (s, c) = crate::math::sin_cos(unit(crate::seed_hash!(id, "dir")) * std::f64::consts::FRAC_PI_2);
    [c, s]
}
