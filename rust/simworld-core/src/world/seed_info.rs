//! Seed world views for the host: the file reader option, `airports()` and `seed_world_info()`
//! (api 1.7).

use super::*;

/// Host file reader for the seed world's files (see [`WorldOptions::seed_reader`]).
#[derive(Clone)]
pub struct SeedFileReader(pub Arc<crate::seed::world::SeedReader>);

impl std::fmt::Debug for SeedFileReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SeedFileReader")
    }
}

impl PartialEq for SeedFileReader {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// One runway end of [`AirportInfo`] (contract `airports()`, api 1.7).
#[derive(Debug, Clone, PartialEq)]
pub struct RunwayEndInfo {
    /// Designator of this end (`"08"`, `"26L"`).
    pub ident: String,
    /// Physical end (WGS84) and its local position (`y` = `ground_height_at` there).
    pub lat: f64,
    pub lon: f64,
    pub position: [f64; 3],
    /// Landing threshold (the end moved inward by the displaced threshold).
    pub threshold_lat: f64,
    pub threshold_lon: f64,
    pub threshold_position: [f64; 3],
    pub displaced_threshold_m: f64,
    /// Aiming point of the touchdown zone: 300 m past the threshold (a quarter of the landing
    /// distance on shorter runways).
    pub touchdown_position: [f64; 3],
    /// True heading of a landing on this end (degrees, 0 = north, clockwise).
    pub heading_deg: f64,
}

/// One runway of [`AirportInfo`].
#[derive(Debug, Clone, PartialEq)]
pub struct RunwayInfo {
    /// `"<le>/<he>"`.
    pub designator: String,
    pub le: RunwayEndInfo,
    pub he: RunwayEndInfo,
    pub length_m: f64,
    pub width_m: f64,
    /// Surface as in the airport file, and whether the generator reads it as paved.
    pub surface: String,
    pub paved: bool,
    pub lighted: bool,
    /// An end was missing in the file: placed at the reference point along the heading.
    pub approximate: bool,
    /// Lights of this runway (local; `y` = ground + `seed.airport.lights.height_m`) and their
    /// colors (rgb, a = intensity): the points of the tiles' `airport_lights`.
    pub lights: Vec<[f64; 3]>,
    pub light_colors: Vec<[f32; 4]>,
}

/// One airport of a seed world (contract `airports()`, api 1.7).
#[derive(Debug, Clone, PartialEq)]
pub struct AirportInfo {
    pub ident: String,
    pub name: String,
    /// OurAirports type (`large_airport`, `medium_airport`, `small_airport`).
    pub kind: String,
    /// Reference point (WGS84) and its local position (`y` = `ground_height_at`).
    pub lat: f64,
    pub lon: f64,
    pub position: [f64; 3],
    /// From the airport file; informational (the terrain decides every height).
    pub elevation_ft: Option<f64>,
    pub runways: Vec<RunwayInfo>,
}

/// One city of [`SeedInfo`].
#[derive(Debug, Clone, PartialEq)]
pub struct SeedCityInfo {
    pub id: String,
    pub name: String,
    pub lat: f64,
    pub lon: f64,
    pub radius_km: f64,
    pub version: i64,
    /// `"synthetic"` (S1 has no real sources; S5 adds `"real:<url>"`, `"synthetic (missing)"`).
    pub source: String,
    pub airports: Vec<String>,
    /// The city plan (api 1.12; built by this call when no tile needed it yet).
    pub plan: crate::seed::city::CityStats,
    /// Plan warnings (district requests that got fewer cells than asked).
    pub plan_warnings: Vec<String>,
    /// Approximate memory of the city plan (bytes).
    pub plan_bytes: usize,
}

/// District plan cache counters (api 1.12, `seed_world_info()["memory"]`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DistrictPlanStats {
    pub held: usize,
    pub bytes: usize,
    pub built: u64,
    pub build_ms: f64,
}

/// Contract `seed_world_info()` (api 1.7; city plans and district plan counters since 1.12).
#[derive(Debug, Clone, PartialEq)]
pub struct SeedInfo {
    pub manifest: String,
    pub generator_version: u32,
    pub phases: Vec<String>,
    pub world_version: i64,
    pub world_seed: String,
    pub cities: Vec<SeedCityInfo>,
    pub airports: usize,
    pub water_polygons: usize,
    pub warnings: Vec<String>,
    pub stats: crate::seed::world::SeedStats,
    pub district_plans: DistrictPlanStats,
}

impl World {
    /// Contract `airports()` (api 1.7): every airport of the seed world with its runways, in the
    /// current local frame (call again after an origin shift). Empty without a seed world.
    pub fn airports(&self) -> Vec<AirportInfo> {
        let Some(s) = &self.seed else { return Vec::new() };
        let h_light = s.params.airport.lights.height_m;
        let local = |lat: f64, lon: f64, up: f64| -> [f64; 3] {
            let h = self.terrain.height(lat, lon) + up;
            self.geodetic_to_local(lat, lon, h).unwrap_or([0.0; 3])
        };
        s.airports
            .iter()
            .map(|g| {
                let runways = g
                    .runways
                    .iter()
                    .enumerate()
                    .map(|(ri, r)| {
                        let end = |ident: &str, ll: (f64, f64), thr_s: f64, toward: f64, disp: f64, heading: f64| {
                            let (tlat, tlon) = g.frame.ll(r.at(thr_s, 0.0));
                            let land = (r.he_thr_s - r.le_thr_s).abs();
                            let td = 300.0f64.min(land * 0.25);
                            let (dlat, dlon) = g.frame.ll(r.at(thr_s + toward * td, 0.0));
                            RunwayEndInfo {
                                ident: ident.to_string(),
                                lat: ll.0,
                                lon: ll.1,
                                position: local(ll.0, ll.1, 0.0),
                                threshold_lat: tlat,
                                threshold_lon: tlon,
                                threshold_position: local(tlat, tlon, 0.0),
                                displaced_threshold_m: disp,
                                touchdown_position: local(dlat, dlon, 0.0),
                                heading_deg: heading,
                            }
                        };
                        let back = (r.heading_deg + 180.0) % 360.0;
                        let lights: Vec<&crate::seed::airport::AirportLight> = g.lights.iter().filter(|l| l.runway == ri).collect();
                        RunwayInfo {
                            designator: r.key.clone(),
                            le: end(&r.le, r.le_ll, r.le_thr_s, 1.0, r.le_thr_s, r.heading_deg),
                            he: end(&r.he, r.he_ll, r.he_thr_s, -1.0, r.length_m - r.he_thr_s, back),
                            length_m: r.length_m,
                            width_m: r.width_m,
                            surface: r.surface.clone(),
                            paved: r.paved,
                            lighted: r.lighted,
                            approximate: r.approximate,
                            lights: lights
                                .iter()
                                .map(|l| {
                                    let (lat, lon) = g.frame.ll(l.p);
                                    local(lat, lon, h_light)
                                })
                                .collect(),
                            light_colors: lights.iter().map(|l| l.color).collect(),
                        }
                    })
                    .collect();
                AirportInfo {
                    ident: g.ident.clone(),
                    name: g.name.clone(),
                    kind: g.kind.clone(),
                    lat: g.lat,
                    lon: g.lon,
                    position: local(g.lat, g.lon, 0.0),
                    elevation_ft: g.elevation_ft,
                    runways,
                }
            })
            .collect()
    }

    /// Contract `seed_world_info()` (api 1.7); `None` without a seed world.
    pub fn seed_world_info(&self) -> Option<SeedInfo> {
        let s = self.seed.as_ref()?;
        Some(SeedInfo {
            manifest: s.manifest.clone(),
            generator_version: crate::seed::SEED_GEN_VERSION,
            phases: crate::seed::SEED_PHASES.iter().map(|p| p.to_string()).collect(),
            world_version: s.files.manifest.world_version,
            world_seed: s.files.manifest.world_seed.clone(),
            cities: s
                .cities()
                .iter()
                .enumerate()
                .map(|(i, c)| (c, s.city_plan(i)))
                .map(|(c, plan)| SeedCityInfo {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    lat: c.center.lat,
                    lon: c.center.lon,
                    radius_km: c.radius_km,
                    version: c.version,
                    source: "synthetic".into(),
                    airports: c.airports.clone(),
                    plan: plan.stats.clone(),
                    plan_warnings: plan.warnings.clone(),
                    plan_bytes: plan.bytes(),
                })
                .collect(),
            airports: s.airports.len(),
            water_polygons: s.files.water.polygons.len(),
            warnings: s.warnings.clone(),
            stats: s.stats,
            district_plans: {
                let (held, bytes, built, build_ms) = s.cell_plan_stats();
                DistrictPlanStats { held, bytes, built, build_ms }
            },
        })
    }
}
