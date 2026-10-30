//! Generated cities (`docs/spec-world-seed.md` sections 5.3 and 6; phases S2, S3, S4), split into
//! the stages of the city pipeline (`docs/spec-seed-city-pipeline.md` section 1, phase C1) with
//! generator 3 behind them. Each stage is a pure function of the city file, the world's water,
//! airports and terrain, the profile's `seed` section and the previous stage's output (a typed
//! hand-off), so a later phase replaces one stage at a time.
//!
//! The **city plan** ([`CityPlan`], built once per city on first use; [`CityPlan::stages`]):
//!
//! 1. [`context`] -> [`CityContext`]: hex cells around the centre with an irregular footprint
//!    edge, classified water / airport / land, approach shares and `near` features;
//! 2. [`zoning`] -> [`Zoning`]: districts placed automatically by the file's requests,
//!    same-district cells merged up to `seed.city.max_district_m2`, the arterial candidate edges;
//! 3. [`network`] -> [`MajorNetwork`]: curved **arterials** on the cell edges (spanning tree kept,
//!    a share of the rest dropped, water crossings bridged sparingly), **radials** from the
//!    centre to the footprint edge and on to each linked airport.
//!
//! Generator 3 zones before it builds the major network (the arterials follow the district
//! boundaries); the pipeline's order (network, then zoning on its faces) comes with C3.
//!
//! The **district plan** of each land cell ([`CellPlan`], cached, `seed.city.plan_cache`;
//! [`CellPlan::build`]) from its [`CellInput`]:
//!
//! 4. [`network::local`] -> [`LocalNetwork`]: a warped street lattice in its district's frame
//!    (continuous across the merged cells of a district), its blocks and the service loops;
//! 5. [`lots`] -> [`Lots`]: lots along each block's long sides and the civic lots;
//! 6. [`buildings`] -> [`PlanBuilding`]s: one building per lot, heights capped by the approach
//!    surfaces, the old town's landmarks.
//!
//! The **tile assembly** ([`tile::emit_city`]): district ground areas, arterials, radials, local
//! streets and buildings in tile pixels, each building in every tile it overlaps (owned by the
//! tile holding its centroid, like decoded MVT).
//!
//! Every stage output has a content hash ([`StageHash`], pipeline section 8), computed on demand
//! and pinned per stage by `golden_seed_city_stages`.
//!
//! Every random choice is a counter-based hash of the city seed (`H(root, "city", id, variant)`)
//! or a district seed and stable ids (axial cells, lattice indices), so output depends on no
//! order, thread or other city. Plan geometry is in the city plane ([`LocalFrame`] at the
//! centre: metres, x east, y north), never the world's floating origin.

pub mod buildings;
pub mod cache;
pub mod context;
pub mod hex;
pub mod lines;
pub mod lots;
pub mod network;
pub mod tile;
pub mod zoning;

use std::collections::BTreeMap;

use crate::mvt::{ROAD_PRIMARY, ROAD_SECONDARY};
use crate::profile::{Profile, SeedStyle, SeedStyles};
use crate::seed::airport::LocalFrame;
use crate::seed::files::{City, DistrictType, Style};
use crate::seed::geom::point_in_ring;
use crate::seed::hash::{Hasher, Part, Seed};
use crate::seed::water::WaterHit;
use crate::seed::world::SeedWorld;
pub use buildings::PlanBuilding;
pub use context::CityContext;
use hex::{Ax, HexGrid};
pub use lots::Lots;
pub use network::local::{LocalNetwork, PlanStreet};
pub use network::MajorNetwork;
pub use zoning::Zoning;

/// What a cell of the city grid holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellKind {
    Land,
    /// More than half of its area in water.
    Water,
    /// Centre inside an airport fence or more than 30 % of it.
    Airport,
}

/// Features within two cells of a cell (district requests' `near`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Near {
    pub water: bool,
    pub coast: bool,
    pub river: bool,
    pub airport: bool,
}

/// One kept cell of the footprint.
#[derive(Clone, Debug)]
pub struct Cell {
    pub ax: Ax,
    pub ring: u32,
    pub kind: CellKind,
    /// Index into [`CityPlan::districts`] (land cells only; `None` in the [`CityContext`], set
    /// from the [`Zoning`] when the plan is assembled).
    pub district: Option<u32>,
    /// Counter-clockwise ring of the six edge curves (plane metres).
    pub poly: Vec<[f64; 2]>,
    pub bbox: [f64; 4],
    /// Share of the cell's sample points inside an approach surface.
    pub approach: f64,
    pub near: Near,
}

/// A district: one or more merged land cells of the same request.
#[derive(Clone, Debug)]
pub struct District {
    /// `H(city, "district", q, r, 0)` of its first cell.
    pub id: Seed,
    pub kind: DistrictType,
    pub style: Style,
    /// Index of its style in [`SeedStyles`] order (see [`style_index`]).
    pub style_index: usize,
    /// Request index in the city file (`districts.len()` = the fill).
    pub request: usize,
    pub max_height_m: f64,
    pub density: f64,
    pub lit: f64,
    pub landmarks: u32,
    pub cells: Vec<Ax>,
    /// Origin and unit direction of its street lattice (plane).
    pub origin: [f64; 2],
    pub dir: [f64; 2],
}

/// An arterial (secondary) or radial (primary) road of the city plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoadKind {
    Arterial,
    Radial,
}

#[derive(Clone, Debug)]
pub struct RoadPath {
    pub id: u64,
    pub kind: RoadKind,
    pub pts: Vec<[f64; 2]>,
    pub bbox: [f64; 4],
}

/// Counts and timings of a city plan (`seed_world_info()`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CityStats {
    pub land_cells: u32,
    pub water_cells: u32,
    pub airport_cells: u32,
    /// Land cells per district type name.
    pub cells_by_type: BTreeMap<String, u32>,
    pub districts: u32,
    /// Arterial hex edges kept after the drop (before water) and their connected components
    /// before and after the drop (the drop never splits the network).
    pub arterial_edges: u32,
    pub arterial_components_before: u32,
    pub arterial_components: u32,
    pub bridges: u32,
    pub radials: u32,
    /// Radials that reach a linked airport's fence.
    pub airport_access: u32,
    pub build_ms: f64,
}

/// The plan of one city (section 5.3): the three city-level stages assembled.
#[derive(Clone, Debug)]
pub struct CityPlan {
    pub index: usize,
    pub id: String,
    pub seed: Seed,
    pub frame: LocalFrame,
    pub grid: HexGrid,
    pub cells: BTreeMap<Ax, Cell>,
    pub districts: Vec<District>,
    pub roads: Vec<RoadPath>,
    /// lat/lon box [w, s, e, n] of every cell.
    pub bbox_ll: [f64; 4],
    pub stats: CityStats,
    pub warnings: Vec<String>,
}

impl CityPlan {
    /// Build the plan of `city` (index `index` in the manifest).
    pub fn build(index: usize, city: &City, w: &SeedWorld) -> Self {
        let t0 = std::time::Instant::now();
        let (ctx, zoning, network) = Self::stages(city, w);
        let mut plan = Self::assemble(index, city, ctx, zoning, network);
        plan.stats.build_ms = t0.elapsed().as_secs_f64() * 1000.0;
        plan
    }

    /// The city-level stage outputs: context -> zoning -> major network.
    pub fn stages(city: &City, w: &SeedWorld) -> (CityContext, Zoning, MajorNetwork) {
        let ctx = context::build(city, w);
        let zoning = zoning::build(&ctx, city, w);
        let network = network::build(&ctx, &zoning, city, w);
        (ctx, zoning, network)
    }

    /// The plan from its stage outputs: cells take their districts, roads are arterials then
    /// radials, plus the counts and the lat/lon box.
    fn assemble(index: usize, city: &City, ctx: CityContext, zoning: Zoning, network: MajorNetwork) -> Self {
        let CityContext { seed, frame, grid, mut cells } = ctx;
        for &(a, g) in &zoning.cell_district {
            cells.get_mut(&a).expect("land cell").district = Some(g);
        }
        // Water and airport cells never hold a district.
        debug_assert!(cells.values().all(|c| c.kind == CellKind::Land || c.district.is_none()));
        let mut stats = CityStats::default();
        for c in cells.values() {
            match c.kind {
                CellKind::Land => stats.land_cells += 1,
                CellKind::Water => stats.water_cells += 1,
                CellKind::Airport => stats.airport_cells += 1,
            }
        }
        stats.districts = zoning.districts.len() as u32;
        for d in &zoning.districts {
            *stats.cells_by_type.entry(d.kind.name().to_string()).or_default() += d.cells.len() as u32;
        }
        let n = &network;
        stats.arterial_edges = n.arterial_edges;
        stats.arterial_components_before = n.arterial_components_before;
        stats.arterial_components = n.arterial_components;
        stats.bridges = n.bridges;
        stats.radials = n.radials;
        stats.airport_access = n.airport_access;
        let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for c in cells.values() {
            for p in [[c.bbox[0], c.bbox[1]], [c.bbox[2], c.bbox[3]]] {
                let (lat, lon) = frame.ll(p);
                b = [b[0].min(lon), b[1].min(lat), b[2].max(lon), b[3].max(lat)];
            }
        }
        for r in &network.roads {
            for p in [[r.bbox[0], r.bbox[1]], [r.bbox[2], r.bbox[3]]] {
                let (lat, lon) = frame.ll(p);
                b = [b[0].min(lon), b[1].min(lat), b[2].max(lon), b[3].max(lat)];
            }
        }
        CityPlan {
            index,
            id: city.id.clone(),
            seed,
            frame,
            grid,
            cells,
            districts: zoning.districts,
            roads: network.roads,
            bbox_ll: b,
            stats,
            warnings: zoning.warnings,
        }
    }

    /// The kept cell whose plain hexagon holds a plane point.
    pub fn cell_at(&self, p: [f64; 2]) -> Option<&Cell> {
        self.cells.get(&self.grid.cell_at(p))
    }

    /// The district at a lat/lon point (plain hexagons; for the terrain colour).
    pub fn district_at(&self, lat: f64, lon: f64) -> Option<&District> {
        let c = self.cell_at(self.frame.xy(lat, lon))?;
        c.district.map(|d| &self.districts[d as usize])
    }

    /// Approximate memory of the plan (bytes).
    pub fn bytes(&self) -> usize {
        let cells: usize = self.cells.values().map(|c| std::mem::size_of::<Cell>() + c.poly.len() * 16).sum();
        let roads: usize = self.roads.iter().map(|r| std::mem::size_of::<RoadPath>() + r.pts.len() * 16).sum();
        cells + roads + self.districts.iter().map(|d| std::mem::size_of::<District>() + d.cells.len() * 8).sum::<usize>()
    }
}

/// What the district plan of one land cell reads: its cell and district in the city plan, the
/// district's style and the world (water, fences, approach surfaces, heights).
pub struct CellInput<'a> {
    pub plan: &'a CityPlan,
    pub w: &'a SeedWorld,
    pub a: Ax,
    pub cell: &'a Cell,
    /// Plane position of the cell centre (plan offsets are relative to it).
    pub center: [f64; 2],
    /// Index of [`Self::d`] in [`CityPlan::districts`].
    pub district: u32,
    pub d: &'a District,
    pub style: &'a SeedStyle,
    pub kinds: &'a StyleKinds,
}

impl<'a> CellInput<'a> {
    /// The input of cell `a` of `plan`; `None` when the cell holds no district.
    pub fn new(plan: &'a CityPlan, a: Ax, w: &'a SeedWorld) -> Option<Self> {
        let cell = &plan.cells[&a];
        let di = cell.district?;
        let d = &plan.districts[di as usize];
        Some(Self {
            plan,
            w,
            a,
            cell,
            center: plan.grid.center(a),
            district: di,
            d,
            style: style_params(&w.params.styles, d.style_index),
            kinds: &w.city_kinds.styles[d.style_index],
        })
    }

    /// Water or inside an airport fence.
    #[inline]
    pub fn wet(&self, p: [f64; 2]) -> bool {
        let (la, lo) = self.plan.frame.ll(p);
        self.w.water.water_at(la, lo) != WaterHit::Land || self.w.terrain.strips.in_fence(la, lo)
    }

    #[inline]
    pub fn in_cell(&self, p: [f64; 2]) -> bool {
        point_in_ring(p, &self.cell.poly)
    }

    /// In the cell and dry.
    #[inline]
    pub fn ok(&self, p: [f64; 2]) -> bool {
        self.in_cell(p) && !self.wet(p)
    }

    /// Plane point -> offset from the cell centre.
    #[inline]
    pub fn rel(&self, p: [f64; 2]) -> [f32; 2] {
        [(p[0] - self.center[0]) as f32, (p[1] - self.center[1]) as f32]
    }
}

/// The district plan of one cell: what the tile assembly reads (the cached level).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CellPlan {
    /// Plane position of the cell centre (all offsets are relative to it).
    pub center: [f64; 2],
    pub streets: Vec<PlanStreet>,
    pub buildings: Vec<PlanBuilding>,
}

impl CellPlan {
    /// Build the district plan of cell `a` (a cell of `plan`): local network -> lots -> buildings.
    pub fn build(plan: &CityPlan, a: Ax, w: &SeedWorld) -> Self {
        let Some(input) = CellInput::new(plan, a, w) else {
            return CellPlan { center: plan.grid.center(a), ..Default::default() };
        };
        let net = network::local::build(&input);
        let lots = lots::build(&input, &net);
        let buildings = buildings::build(&input, &net, &lots);
        CellPlan { center: input.center, streets: net.streets, buildings }
    }

    #[inline]
    pub fn abs(&self, p: [f32; 2]) -> [f64; 2] {
        [self.center[0] + p[0] as f64, self.center[1] + p[1] as f64]
    }

    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.streets.iter().map(|s| std::mem::size_of::<PlanStreet>() + s.pts.len() * 8).sum::<usize>()
            + self.buildings.iter().map(|b| std::mem::size_of::<PlanBuilding>() + b.ring.len() * 8 + b.kind.len()).sum::<usize>()
    }
}

/// Content hash of a stage output (pipeline section 8): the seed [`Hasher`] over the exact bits
/// of what the stage produced (floats by their bit pattern), tagged with the stage name. Computed
/// on demand (goldens, tools), never inside a build.
#[derive(Clone, Copy, Debug)]
pub struct StageHash(Hasher);

impl StageHash {
    pub fn new(stage: &str) -> Self {
        let mut h = Hasher::new();
        h.part(Part::S(stage));
        Self(h)
    }

    #[inline]
    pub fn u(&mut self, v: u64) -> &mut Self {
        self.0.part(Part::U(v));
        self
    }

    #[inline]
    pub fn i(&mut self, v: i64) -> &mut Self {
        self.0.part(Part::I(v));
        self
    }

    #[inline]
    pub fn f(&mut self, v: f64) -> &mut Self {
        self.u(v.to_bits())
    }

    #[inline]
    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.u(v.to_bits() as u64)
    }

    pub fn s(&mut self, v: &str) -> &mut Self {
        self.0.part(Part::S(v));
        self
    }

    #[inline]
    pub fn ax(&mut self, a: Ax) -> &mut Self {
        self.i(a.0 as i64).i(a.1 as i64)
    }

    pub fn pts(&mut self, p: &[[f64; 2]]) -> &mut Self {
        self.u(p.len() as u64);
        for q in p {
            self.f(q[0]).f(q[1]);
        }
        self
    }

    pub fn pts32(&mut self, p: &[[f32; 2]]) -> &mut Self {
        self.u(p.len() as u64);
        for q in p {
            self.f32(q[0]).f32(q[1]);
        }
        self
    }

    pub fn finish(&self) -> u64 {
        self.0.finish()
    }
}

/// The style a district type / style pair uses: index in [`SeedStyles`] field order.
pub fn style_index(kind: DistrictType, style: Style) -> usize {
    match (kind, style) {
        (DistrictType::OldTown, _) => 0,
        (DistrictType::Business, _) => 1,
        (DistrictType::Residential, Style::Houses) => 3,
        (DistrictType::Residential, _) => 2,
        (DistrictType::Industrial, _) => 4,
        (DistrictType::Port, _) => 5,
        (DistrictType::Park, _) => 6,
    }
}

/// Style parameters by [`style_index`].
pub fn style_params(s: &SeedStyles, i: usize) -> &SeedStyle {
    [&s.old_town, &s.business, &s.microdistrict, &s.houses, &s.industrial, &s.port, &s.park][i]
}

/// A road class resolved through the profile: (class, minor path, width m).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoadKindParams {
    pub class: u8,
    pub minor: bool,
    pub width_m: f64,
}

/// Profile resolutions of one style.
#[derive(Clone, Debug, PartialEq)]
pub struct StyleKinds {
    pub street: RoadKindParams,
    pub loop_street: Option<RoadKindParams>,
    /// Ground (class, level) of the district area.
    pub area: (u8, u8),
}

/// Everything the city generator needs from the profile outside `seed`, resolved at load.
#[derive(Clone, Debug, PartialEq)]
pub struct CityKinds {
    pub styles: Vec<StyleKinds>,
    pub arterial: RoadKindParams,
    pub radial: RoadKindParams,
}

impl CityKinds {
    pub fn resolve(p: &Profile) -> Self {
        let road = |h: &str| {
            let (class, link, minor) = p.road_class(h);
            RoadKindParams { class, minor, width_m: p.road_width_m(class, link) }
        };
        let fixed = |class: u8| RoadKindParams { class, minor: false, width_m: p.road_width_m(class, false) };
        let styles = (0..7)
            .map(|i| {
                let s = style_params(&p.seed.styles, i);
                StyleKinds {
                    street: road(&s.street),
                    loop_street: (!s.loop_street.is_empty()).then(|| road(&s.loop_street)),
                    area: p.ground_area_class(&s.landuse),
                }
            })
            .collect();
        Self { styles, arterial: fixed(ROAD_SECONDARY), radial: fixed(ROAD_PRIMARY) }
    }
}
