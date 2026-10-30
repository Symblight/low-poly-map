//! Stage 1, context (`docs/spec-seed-city-pipeline.md` section 2; generator 3:
//! `docs/spec-world-seed.md` section 5.3 steps 1-2): the hex grid, the footprint and the cell
//! classification.
//!
//! Every cell within `ring_count` whose centre lies within `radius x (1 + 0.15 x (2u - 1))`
//! (`u = unit(H(city, "footprint", q, r))`) is kept, so the footprint edge is irregular. Each cell
//! is sampled at 19 points: more than half in water = water, centre in a fence or more than 30 %
//! = airport, else land; the share inside approach surfaces and the `near` features within two
//! cells are recorded.

use std::collections::BTreeMap;

use super::hex::{ring, Ax, HexGrid, S3H};
use super::{Cell, CellKind, Near, StageHash};
use crate::seed::airport::LocalFrame;
use crate::seed::files::{City, WaterKind};
use crate::seed::hash::{city_seed, unit, Seed};
use crate::seed::water::WaterHit;
use crate::seed::world::SeedWorld;

/// Output of stage 1: the city's frame and grid and its kept cells (no districts yet).
#[derive(Clone, Debug)]
pub struct CityContext {
    pub seed: Seed,
    pub frame: LocalFrame,
    pub grid: HexGrid,
    pub cells: BTreeMap<Ax, Cell>,
}

impl CityContext {
    /// The kept cell whose plain hexagon holds a plane point.
    pub fn cell_at(&self, p: [f64; 2]) -> Option<&Cell> {
        self.cells.get(&self.grid.cell_at(p))
    }

    /// Stage hash: seed, grid and every cell.
    pub fn hash(&self) -> u64 {
        let mut h = StageHash::new("context");
        h.u(self.seed.0).f(self.grid.cell_m).pts(&[self.grid.center((1, 0)), self.grid.center((0, 1))]);
        for c in self.cells.values() {
            h.ax(c.ax).u(c.ring as u64).u(c.kind as u64).u(c.district.map_or(u64::MAX, |d| d as u64));
            h.pts(&c.poly).pts(&[[c.bbox[0], c.bbox[1]], [c.bbox[2], c.bbox[3]]]).f(c.approach);
            h.u(c.near.water as u64 | (c.near.coast as u64) << 1 | (c.near.river as u64) << 2 | (c.near.airport as u64) << 3);
        }
        h.finish()
    }
}

/// Unit vectors every 30 degrees from east.
const DIR12: [[f64; 2]; 12] = [
    [1.0, 0.0],
    [S3H, 0.5],
    [0.5, S3H],
    [0.0, 1.0],
    [-0.5, S3H],
    [-S3H, 0.5],
    [-1.0, 0.0],
    [-S3H, -0.5],
    [-0.5, -S3H],
    [0.0, -1.0],
    [0.5, -S3H],
    [S3H, -0.5],
];

/// The 19 sample offsets of a cell (unrotated hex frame, cell size 1): centre, six points at half
/// the circumradius, six near the corners, six near the edge midpoints.
fn samples() -> [[f64; 2]; 19] {
    let rc = 1.0 / (3.0f64).sqrt();
    let mut s = [[0.0; 2]; 19];
    for k in 0..6 {
        let e = DIR12[2 * k];
        let c = DIR12[2 * k + 1];
        s[1 + k] = [e[0] * 0.5 * rc, e[1] * 0.5 * rc];
        s[7 + k] = [c[0] * 0.85 * rc, c[1] * 0.85 * rc];
        s[13 + k] = [e[0] * 0.4, e[1] * 0.4];
    }
    s
}

fn water_kind(w: &SeedWorld, hit: WaterHit) -> Option<WaterKind> {
    match hit {
        WaterHit::Land => None,
        WaterHit::OutsideSea => Some(WaterKind::Sea),
        WaterHit::Body(i) => Some(w.water.bodies()[i as usize].kind),
    }
}

/// Stage 1 of `city`: the seed, the rotated grid and the kept cells with their class, polygon,
/// approach share and near flags.
pub fn build(city: &City, w: &SeedWorld) -> CityContext {
    let seed = city_seed(w.root, &city.id, city.variant);
    let frame = LocalFrame::new(city.center.lat, city.center.lon);
    let rot = match city.layout.rotation_deg {
        Some(d) => d.to_radians(),
        None => (unit(crate::seed_hash!(seed, "rotation")) * 60.0).to_radians(),
    };
    let grid = HexGrid::new(city.layout.cell_size_m, rot);
    let mut ctx = CityContext { seed, frame, grid, cells: BTreeMap::new() };
    cells(&mut ctx, city, w);
    ctx
}

fn cells(ctx: &mut CityContext, city: &City, w: &SeedWorld) {
    let n = city.ring_count() as i32;
    let radius = city.radius_km * 1000.0;
    let cell_m = ctx.grid.cell_m;
    let warp = w.params.city.edge_warp;
    let mut kept: Vec<Ax> = Vec::new();
    for q in -n..=n {
        for r in -n..=n {
            let a = (q, r);
            if ring(a) > n as u32 {
                continue;
            }
            let c = ctx.grid.center(a);
            let noise = unit(crate::seed_hash!(ctx.seed, "footprint", q, r)) * 2.0 - 1.0;
            let reach = radius * (1.0 + crate::seed::files::FOOTPRINT_NOISE * noise);
            if (c[0] * c[0] + c[1] * c[1]).sqrt() < reach || a == (0, 0) {
                kept.push(a);
            }
        }
    }
    let mut curves: BTreeMap<(Ax, Ax), Vec<[f64; 2]>> = BTreeMap::new();
    let samples = samples();
    let approach = w.approach.within(grow_ll(&ctx.frame, radius + 2.0 * cell_m));
    for a in kept {
        let pts: Vec<(f64, f64)> = samples.iter().map(|s| ctx.frame.ll(ctx.grid.offset(a, [s[0] * cell_m, s[1] * cell_m]))).collect();
        let wet = pts.iter().filter(|(la, lo)| w.water.water_at(*la, *lo) != WaterHit::Land).count();
        let fence = pts.iter().filter(|(la, lo)| w.terrain.strips.in_fence(*la, *lo)).count();
        let appr = pts.iter().filter(|(la, lo)| crate::seed::airport::limit_of(approach.iter().copied(), *la, *lo).is_some()).count();
        let kind = if wet * 2 > pts.len() {
            CellKind::Water
        } else if w.terrain.strips.in_fence(pts[0].0, pts[0].1) || fence as f64 > 0.3 * pts.len() as f64 {
            CellKind::Airport
        } else {
            CellKind::Land
        };
        let mut near = Near::default();
        let mut probe = |lat: f64, lon: f64| {
            if let Some(k) = water_kind(w, w.water.water_at(lat, lon)) {
                near.water = true;
                near.coast |= k == WaterKind::Sea;
                near.river |= k == WaterKind::River;
            }
        };
        for (la, lo) in &pts {
            probe(*la, *lo);
        }
        for d in [cell_m, 2.0 * cell_m] {
            for u in DIR12 {
                let c = ctx.grid.center(a);
                let (la, lo) = ctx.frame.ll([c[0] + u[0] * d, c[1] + u[1] * d]);
                probe(la, lo);
            }
        }
        let (cla, clo) = pts[0];
        near.airport = w.airports.iter().any(|g| g.fence.distance(g.frame.xy(cla, clo)) <= 2.0 * cell_m);
        let (grid, seed) = (ctx.grid, ctx.seed);
        let poly = grid.cell_ring(a, &mut |x, y| curves.entry(super::hex::edge_key(x, y)).or_insert_with(|| grid.edge_curve(seed, x, y, warp)).clone());
        let bbox = super::lines::bbox(&poly);
        ctx.cells.insert(a, Cell { ax: a, ring: ring(a), kind, district: None, poly, bbox, approach: appr as f64 / pts.len() as f64, near });
    }
}

/// lat/lon box around the city centre with a radius in metres.
fn grow_ll(frame: &LocalFrame, r: f64) -> [f64; 4] {
    let (s, w) = frame.ll([-r, -r]);
    let (n, e) = frame.ll([r, r]);
    [w, s, e, n]
}
