//! The loaded seed world (`docs/spec-world-seed.md`, phase S1) and its tile generator.
//!
//! [`SeedWorld::load`] reads the manifest and every file it names through a host reader (Godot:
//! `res://`, `user://`; Rust hosts and tests: the file system), validates them (section 4.6),
//! builds the z11 water index (lake levels from the terrain field), every airport (section 5.1)
//! and the terrain modifiers ([`SeedTerrain`]: water rule and runway strips).
//!
//! [`generate`] is the seed generator of section 7: `(key, &SeedWorld) -> SeedTile`, no frame,
//! no floating origin. A tile at any zoom 13..16 is routed by its z13 ancestor; in S1 every z13
//! tile goes to the generator (real city sources come with S5). Output is in tile pixel space
//! like decoded MVT, so the mesher, the ground mesher, the lights and the sibling swap work
//! unchanged:
//!
//! * cities (S2-S4, [`super::city`]): district areas, arterials, radials, local streets and
//!   buildings;
//! * ground in **all four bands**: countryside patches (forest / farmland / meadow + tint),
//!   water areas (sea, lakes, rivers) from the water file's polygons, the airports' fence field,
//!   runways, taxiways and aprons (class 7 `aeroway`, or `seed.aeroway_class`);
//! * the airports' buildings (terminal, hangars, tower), emitted into every tile they overlap,
//!   owned by the tile holding the centroid;
//! * airport lights inside the tile rectangle.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use super::airport::{build_airport, AirportGeo, ApproachIndex, StripIndex};
use super::city::cache::PlanCache;
use super::city::CellPlan;
use super::city::hex::Ax;
use super::city::{CityKinds, CityPlan};
use super::countryside::{z15_to_px, Countryside, PATCH_ZOOM};
use super::files::{load_world, City, SeedFiles};
use super::geom::{m_per_deg_lon, M_PER_DEG};
use super::hash::{fnv1a64, world_root, Seed};
use super::terrain::SeedTerrain;
use super::water::{WaterHit, WaterIndex, WaterParams};
use crate::geo::{lat_to_tile_y, lon_to_tile_x, tile_x_to_lon, tile_y_to_lat, TileKey};
use crate::mvt::{BuildingTags, GroundArea, GroundFeatures, OsmType, RawBuilding, GROUND_WATER};
use crate::profile::{Profile, SeedProfile};
use crate::terrain::Terrain;

/// Version of the generator's output: changes with any intended output change (section 7).
/// 2: S1 fixes (no drape lift inside airport fences, fences cut out of countryside and water,
/// airport levels 1 / 2 / 3, coast-resolved terrain COLOR). 3: cities (S2-S4: districts,
/// arterials, radials, streets, buildings; countryside at stacking level 0 under them; terrain
/// COLOR of the district class inside city footprints).
pub const SEED_GEN_VERSION: u32 = 3;

/// Phases of `docs/spec-world-seed.md` section 11 this library has built (`seed_world_info()`).
pub const SEED_PHASES: &[&str] = &["S0", "S1", "S2", "S3", "S4"];

/// Courtesy attribution of the airport data (public domain).
pub const OURAIRPORTS_ATTRIBUTION: &str = "Airport data: OurAirports";

/// Stacking level of the countryside patches: the bottom, so city district areas (residential
/// 2 ... grass 5) draw over them and water (7) over both.
pub const COUNTRYSIDE_LEVEL: u8 = 0;

/// Tile pixel extent of generated tiles (as the archive's MVT).
pub const EXTENT: f64 = 4096.0;

/// Reads a resolved path (`res://...`, `user://...` or an OS path) -> text.
pub type SeedReader = dyn Fn(&str) -> Result<String, String> + Send + Sync;

/// `p` as written in the manifest -> the path to read: relative paths are relative to the
/// manifest's folder; `res://`, `user://`, `file://` and absolute OS paths are kept.
pub fn resolve_path(manifest: &str, p: &str) -> String {
    let absolute = p.contains("://") || p.starts_with('/') || p.starts_with('\\') || p.as_bytes().get(1) == Some(&b':');
    if absolute {
        return p.to_string();
    }
    match manifest.rfind(['/', '\\']) {
        Some(i) => format!("{}/{}", &manifest[..i], p),
        None => p.to_string(),
    }
}

/// One airport light in tile pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct AirportLightPx {
    pub px: [f64; 2],
    /// Stacking level of the drawn pavement under it.
    pub level: u8,
    pub color: [f32; 4],
}

/// The generator's output for one tile (tile pixel space).
#[derive(Clone, Debug, Default)]
pub struct SeedTile {
    pub buildings: Vec<RawBuilding>,
    pub ground: GroundFeatures,
    pub airport_lights: Vec<AirportLightPx>,
}

/// Measured sizes of a loaded world (section 10a budgets).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SeedStats {
    pub water_index_bytes: usize,
    pub water_raster_bytes: usize,
    pub water_mixed_cells: usize,
    pub water_raster_cells: usize,
    pub strip_index_bytes: usize,
    pub airport_bytes: usize,
    pub build_ms: f64,
}

/// A loaded, validated and indexed seed world (immutable; shared as `Arc`).
#[derive(Debug)]
pub struct SeedWorld {
    /// Manifest path as passed in `seed_world`.
    pub manifest: String,
    pub files: SeedFiles,
    pub root: Seed,
    pub water: Arc<WaterIndex>,
    /// Selected airports, sorted by ident (only those with an open runway).
    pub airports: Vec<AirportGeo>,
    /// Water rule + runway strips for the terrain.
    pub terrain: Arc<SeedTerrain>,
    pub countryside: Countryside,
    pub params: SeedProfile,
    /// The configuration's terrain with the seed rules (approach caps read it).
    pub height: Terrain,
    /// Approach surfaces of every runway end (building caps).
    pub approach: ApproachIndex,
    /// Profile resolutions the city generator needs (road classes and widths, area classes).
    pub city_kinds: CityKinds,
    /// City plans, built on first use (index = manifest order).
    city_plans: Vec<OnceLock<Arc<CityPlan>>>,
    /// District (cell) plans, bounded by `seed.city.plan_cache`.
    cell_plans: PlanCache,
    /// Ground (class, level) of the countryside kinds and of water, from the profile.
    area_forest: (u8, u8),
    area_farmland: (u8, u8),
    area_meadow: (u8, u8),
    area_water: (u8, u8),
    /// Hash of every file's text, the profile's `seed` and `terrain` field keys: a reconfigure
    /// with the same key reuses this world.
    pub content_key: u64,
    pub attributions: Vec<String>,
    pub warnings: Vec<String>,
    pub stats: SeedStats,
}

impl SeedWorld {
    /// Load the world from `manifest` (read through `read`). `field` is the configuration's
    /// terrain **without** seed rules (lake levels, runway threshold heights). `cached` is the
    /// world of an earlier configuration: returned as is when every file and the relevant
    /// profile keys are unchanged. Errors name the file, the key and the value.
    pub fn load(manifest: &str, read: &SeedReader, profile: &Profile, field: &Terrain, cached: Option<&Arc<SeedWorld>>) -> Result<Arc<SeedWorld>, String> {
        let t0 = std::time::Instant::now();
        let fail = |e: String| format!("seed_world {manifest}: {e}");
        let mtext = read(manifest).map_err(|e| fail(format!("cannot read the manifest: {e}")))?;
        // Read every file once (the content key covers them all).
        let mut texts: BTreeMap<String, Result<String, String>> = BTreeMap::new();
        let m = super::files::parse_manifest(manifest, &mtext).map_err(|e| fail(e.join("; ")))?;
        let mut names: Vec<String> = vec![m.airports.file.clone(), m.water.clone()];
        names.extend(m.cities.iter().cloned());
        names.extend(m.roads.iter().cloned());
        for n in &names {
            texts.entry(n.clone()).or_insert_with(|| read(&resolve_path(manifest, n)));
        }
        let mut key = crate::seed::hash::Hasher::new();
        key.part(fnv1a64(mtext.as_bytes()).into());
        for (n, t) in &texts {
            key.part(n.as_str().into());
            key.part(fnv1a64(t.as_ref().map_or("", |s| s.as_str()).as_bytes()).into());
        }
        let prof = serde_json::to_string(&(&profile.seed, &profile.terrain)).unwrap_or_default();
        key.part(fnv1a64(prof.as_bytes()).into());
        key.part((SEED_GEN_VERSION as u64).into());
        let content_key = key.finish();
        if let Some(c) = cached.filter(|c| c.content_key == content_key && c.manifest == manifest) {
            return Ok(c.clone());
        }

        let mut reader = |p: &str| -> Result<(String, String), String> {
            match texts.get(p) {
                Some(Ok(t)) => Ok((resolve_path(manifest, p), t.clone())),
                Some(Err(e)) => Err(e.clone()),
                None => read(&resolve_path(manifest, p)).map(|t| (resolve_path(manifest, p), t)),
            }
        };
        let files = load_world((manifest, &mtext), &mut reader).map_err(|e| fail(e.errors.join("; ")))?;
        let sp = profile.seed.clone();
        let params = WaterParams {
            blend_m: sp.water.blend_m,
            raster_n: sp.water.raster_n as usize,
            sea_level_m: profile.terrain.sea_level_m,
            rivers_flat: false,
        };
        let fieldf = |lat: f64, lon: f64| field.field_only_height(lat, lon);
        let water = Arc::new(WaterIndex::build(&files.water, params, &fieldf));
        let before_strips = |lat: f64, lon: f64| water.apply_with(lat, lon, || field.field_only_height(lat, lon));
        let airports: Vec<AirportGeo> = files
            .airports
            .iter()
            .map(|a| {
                let cities: Vec<(f64, f64)> =
                    files.cities.iter().filter(|c| c.airports.contains(&a.ident)).map(|c| (c.center.lat, c.center.lon)).collect();
                build_airport(a, &cities, &sp, &before_strips)
            })
            .filter(|g| !g.runways.is_empty())
            .collect();
        let strips = StripIndex::new(&airports, sp.airport.shoulder_m);
        let terrain = Arc::new(SeedTerrain { water: water.clone(), strips });
        let height = field.clone().with_seed(terrain.clone());
        let approach = ApproachIndex::new(&airports, &sp.airport.approach, sp.airport.strip_end_m, &|lat, lon| height.height(lat, lon));
        let root = world_root(&files.manifest.world_seed);
        let countryside = Countryside::new(Seed(crate::seed_hash!(root, "countryside")), sp.countryside.clone());
        let mut warnings = files.warnings.clone();
        warnings.extend(water.city_warnings(&files.cities));
        let mut attributions = Vec::new();
        let osm = files.water.source.to_ascii_lowercase().contains("openstreetmap") || files.cities.iter().any(|c| c.real.is_some());
        if osm {
            attributions.push(crate::world::OSM_ATTRIBUTION.to_string());
        }
        if files.airport_source.0.to_ascii_lowercase().contains("ourairports") {
            attributions.push(OURAIRPORTS_ATTRIBUTION.to_string());
        }
        let ws = water.stats();
        let stats = SeedStats {
            water_index_bytes: ws.index_bytes,
            water_raster_bytes: ws.raster_bytes,
            water_mixed_cells: ws.mixed_cells,
            water_raster_cells: ws.raster_cells,
            strip_index_bytes: terrain.strips.bytes(),
            airport_bytes: airports
                .iter()
                .map(|g| 512 + g.runways.len() * 256 + g.areas.len() * 120 + g.buildings.len() * 140 + g.lights.len() * 48)
                .sum(),
            build_ms: t0.elapsed().as_secs_f64() * 1000.0,
        };
        Ok(Arc::new(SeedWorld {
            manifest: manifest.to_string(),
            root,
            water,
            airports,
            terrain,
            countryside,
            area_forest: profile.ground_area_class("forest"),
            area_farmland: profile.ground_area_class("farmland"),
            area_meadow: profile.ground_area_class("meadow"),
            area_water: profile.ground_area_class("water"),
            city_plans: (0..files.cities.len()).map(|_| OnceLock::new()).collect(),
            cell_plans: PlanCache::new(sp.city.plan_cache as usize),
            city_kinds: CityKinds::resolve(profile),
            height,
            approach,
            params: sp,
            content_key,
            attributions,
            warnings,
            stats,
            files,
        }))
    }

    pub fn cities(&self) -> &[City] {
        &self.files.cities
    }

    /// Ground class written for airport areas (7 `aeroway`, or 6 with `seed.aeroway_class`).
    pub fn aeroway_class(&self) -> u8 {
        self.params.aeroway_class.clamp(6, 7) as u8
    }

    /// Countryside kind and tint of the patch holding a point.
    fn patch_class(&self, cx: i64, cy: i64) -> (super::countryside::PatchKind, u8) {
        let near = self.near_water(cx, cy);
        self.countryside.classify(cx, cy, near)
    }

    /// Whether patch `(cx, cy)` lies within `water_distance_m` of water (its centre and eight
    /// points on a circle of that radius).
    fn near_water(&self, cx: i64, cy: i64) -> bool {
        let (lat, lon) = z15_ll([cx as f64 + 0.5, cy as f64 + 0.5]);
        let d = self.params.countryside.water_distance_m;
        if self.water.water_at(lat, lon) != WaterHit::Land {
            return true;
        }
        if d <= 0.0 {
            return false;
        }
        let (ky, kx) = (d / M_PER_DEG, d / m_per_deg_lon(lat));
        const R: f64 = std::f64::consts::FRAC_1_SQRT_2;
        [(1.0, 0.0), (R, R), (0.0, 1.0), (-R, R), (-1.0, 0.0), (-R, -R), (0.0, -1.0), (R, -R)]
            .iter()
            .any(|&(a, b)| self.water.water_at(lat + b * ky, lon + a * kx) != WaterHit::Land)
    }

    /// Ground class and tint at a point (terrain COLOR, section 5.5): water (class 2) from the
    /// water test (rivers included), class 7 (`seed.aeroway_class`) inside an airport fence,
    /// else the countryside patch's class and tint.
    pub fn class_at(&self, lat: f64, lon: f64) -> (u8, u8) {
        if self.water.water_at(lat, lon) != WaterHit::Land {
            return (GROUND_WATER, 0);
        }
        if self.terrain.strips.in_fence(lat, lon) {
            return (self.aeroway_class(), 0);
        }
        for i in 0..self.city_plans.len() {
            let p = self.city_plan(i);
            let bb = p.bbox_ll;
            if lon < bb[0] || lon > bb[2] || lat < bb[1] || lat > bb[3] {
                continue;
            }
            if let Some(d) = p.district_at(lat, lon) {
                return (self.city_kinds.styles[d.style_index].area.0, 0);
            }
        }
        let g = [lon_to_tile_x(lon, PATCH_ZOOM), lat_to_tile_y(lat, PATCH_ZOOM)];
        let (cx, cy) = self.countryside.patch_at(g[0], g[1]);
        let (kind, tint) = self.patch_class(cx, cy);
        (self.area_of(kind).0, tint)
    }

    fn area_of(&self, kind: super::countryside::PatchKind) -> (u8, u8) {
        match kind {
            super::countryside::PatchKind::Forest => self.area_forest,
            super::countryside::PatchKind::Farmland => self.area_farmland,
            super::countryside::PatchKind::Meadow => self.area_meadow,
        }
    }

    /// Airport by ident.
    pub fn airport(&self, ident: &str) -> Option<&AirportGeo> {
        self.airports.iter().find(|a| a.ident == ident)
    }

    /// The plan of city `i` (manifest order), built on first use.
    pub fn city_plan(&self, i: usize) -> Arc<CityPlan> {
        self.city_plans[i].get_or_init(|| Arc::new(CityPlan::build(i, &self.files.cities[i], self))).clone()
    }

    /// The district plan of cell `a` of a city plan (cached).
    pub fn cell_plan(&self, plan: &CityPlan, a: Ax) -> Arc<CellPlan> {
        self.cell_plans.get(plan.index as u32, a, || CellPlan::build(plan, a, self))
    }

    /// City plans built so far (without building the others).
    pub fn built_city_plans(&self) -> Vec<Arc<CityPlan>> {
        self.city_plans.iter().filter_map(|p| p.get().cloned()).collect()
    }

    /// District plan cache: (plans held, bytes, plans built, total build ms).
    pub fn cell_plan_stats(&self) -> (usize, usize, u64, f64) {
        self.cell_plans.stats()
    }
}

/// z15 units -> (lat, lon).
fn z15_ll(g: [f64; 2]) -> (f64, f64) {
    (tile_y_to_lat(g[1], PATCH_ZOOM), tile_x_to_lon(g[0], PATCH_ZOOM))
}

/// (lat, lon) -> pixels of tile `key`.
#[inline]
pub fn px_of(key: TileKey, lat: f64, lon: f64) -> [f64; 2] {
    [(lon_to_tile_x(lon, key.z) - key.x as f64) * EXTENT, (lat_to_tile_y(lat, key.z) - key.y as f64) * EXTENT]
}

/// lat/lon box [w, s, e, n] of a tile.
pub fn tile_box(key: TileKey) -> [f64; 4] {
    let z = key.z;
    [tile_x_to_lon(key.x as f64, z), tile_y_to_lat((key.y + 1) as f64, z), tile_x_to_lon((key.x + 1) as f64, z), tile_y_to_lat(key.y as f64, z)]
}

fn overlaps(a: [f64; 4], b: [f64; 4]) -> bool {
    a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3]
}

/// Where a tile's data comes from (section 8: routed by its z13 ancestor). S1 has no real
/// sources, so every tile is generated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileSource {
    Seed,
}

/// Route a vector tile (zoom >= 13) by its z13 ancestor.
pub fn route(key: TileKey, _world: &SeedWorld) -> TileSource {
    let _z13 = if key.z >= 13 { TileKey::new(13, key.x >> (key.z - 13), key.y >> (key.z - 13)) } else { key };
    TileSource::Seed
}

/// The seed generator (section 7): everything of tile `key` (zoom 13..16) in tile pixels.
/// Deterministic: a function of the key and the world's files and profile only.
pub fn generate(key: TileKey, w: &SeedWorld) -> SeedTile {
    let mut out = SeedTile::default();
    let tb = tile_box(key);
    // Countryside patches (skipped where the patch is all water).
    let ([x0, x1], [y0, y1]) = Countryside::cells_for_tile(key);
    for cy in y0..=y1 {
        for cx in x0..=x1 {
            let ring = w.countryside.patch_ring(cx, cy);
            let wet = |g: [f64; 2]| {
                let (lat, lon) = z15_ll(g);
                w.water.water_at(lat, lon) != WaterHit::Land
            };
            let corners = [ring[0], ring[3], ring[6], ring[9]];
            if wet([cx as f64 + 0.5, cy as f64 + 0.5]) && corners.iter().all(|c| wet(*c)) {
                continue;
            }
            let px: Vec<[f64; 2]> = ring.iter().map(|g| z15_to_px(key, *g, EXTENT)).collect();
            let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
            for p in &px {
                lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                hi = [hi[0].max(p[0]), hi[1].max(p[1])];
            }
            if hi[0] <= 0.0 || lo[0] >= EXTENT || hi[1] <= 0.0 || lo[1] >= EXTENT {
                continue;
            }
            let (kind, tint) = w.patch_class(cx, cy);
            // Level 0: the countryside is the underlay every city area stacks on.
            let class = w.area_of(kind).0;
            let id = crate::seed_hash!(w.countryside_id(), cx, cy) & 0xFFFF_FFFF;
            out.ground.areas.push(GroundArea { class, level: COUNTRYSIDE_LEVEL, id, rings: vec![px], tint, sub: 0, cut: true });
        }
    }
    // Water (sea, lakes, rivers): the water file's polygons, clipped by the ground mesher.
    for (i, (_kind, rings)) in w.water.tile_polygons(key).into_iter().enumerate() {
        let rings: Vec<Vec<[f64; 2]>> = rings.iter().map(|r| r.iter().map(|p| px_of(key, p[1], p[0])).collect()).collect();
        let (class, level) = w.area_water;
        out.ground.areas.push(GroundArea { class, level, id: 0x5EA0_0000 + i as u64, rings, tint: 0, sub: 0, cut: true });
    }
    // Cities (districts, roads, buildings).
    super::city::tile::emit_city(key, tb, w, &mut out);
    // Airports.
    let aeroway = w.aeroway_class();
    for g in w.airports.iter().filter(|g| overlaps(g.bbox, tb)) {
        let to_px = |p: [f64; 2]| {
            let (lat, lon) = g.frame.ll(p);
            px_of(key, lat, lon)
        };
        // The fence is cut out of the countryside and water below it (contract section 9
        // "Airports"): inside it only the airport's three levels stack on the strip height.
        let fence = g.fence.ring();
        out.ground.cutouts.push([to_px(fence[0]), to_px(fence[1]), to_px(fence[2]), to_px(fence[3])]);
        for (i, a) in g.areas.iter().enumerate() {
            let ring: Vec<[f64; 2]> = a.ring.iter().map(|p| to_px(*p)).collect();
            let id = crate::seed_hash!(g.seed, "area", i) & 0xFFFF_FFFF;
            out.ground.areas.push(GroundArea { class: aeroway, level: a.level, id, rings: vec![ring], tint: 0, sub: a.sub, cut: false });
        }
        let buf = EXTENT * crate::mvt::ENCODER_BUFFER_FRACTION;
        for b in &g.buildings {
            let ring: Vec<[f64; 2]> = b.ring.iter().map(|p| to_px(*p)).collect();
            let inside = ring.iter().any(|p| p[0] > -buf && p[0] < EXTENT + buf && p[1] > -buf && p[1] < EXTENT + buf)
                || {
                    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
                    for p in &ring {
                        lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                        hi = [hi[0].max(p[0]), hi[1].max(p[1])];
                    }
                    hi[0] > 0.0 && lo[0] < EXTENT && hi[1] > 0.0 && lo[1] < EXTENT
                };
            if !inside {
                continue;
            }
            out.buildings.push(RawBuilding {
                osm_type: OsmType::Way,
                id: b.id,
                kind: b.kind.to_string(),
                height: Some(b.height_m),
                levels: Some(b.levels),
                min_height: 0.0,
                landuse: None,
                tags: BuildingTags::default(),
                polygons: vec![vec![ring]],
            });
        }
        for l in &g.lights {
            let px = to_px(l.p);
            if (0.0..EXTENT).contains(&px[0]) && (0.0..EXTENT).contains(&px[1]) {
                out.airport_lights.push(AirportLightPx { px, level: l.level, color: l.color });
            }
        }
    }
    out.ground.sort();
    out.buildings.sort_by_key(|b| b.id);
    out
}

impl SeedWorld {
    fn countryside_id(&self) -> Seed {
        Seed(crate::seed_hash!(self.root, "patch-id"))
    }
}

#[cfg(test)]
#[path = "world_fixture.rs"]
mod fixture;
#[cfg(test)]
pub(crate) use fixture::{test_profile_json, test_world, TEST_WORLD};

#[cfg(test)]
#[path = "world_tests.rs"]
mod tests;
