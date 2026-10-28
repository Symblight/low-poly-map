//! Generated airports (`docs/spec-world-seed.md` section 5.1), built once per airport at load.
//!
//! Geometry is data: runways come from the airport file (after `airports.overrides`) at their
//! surveyed ends; only the cosmetic parts (hangar count, apron side tie-break) use
//! `H("airport", ident)`, which is independent of `world_seed`. Everything is laid out in a local
//! equirectangular plane at the airport reference point ([`LocalFrame`], metres, x east, y north;
//! only `+ - * / sqrt`, `libm` trigonometry), then handed out as lat/lon rings.
//!
//! * **Runway** rectangles (sub-kind 1 paved, 2 grass), the **strip** (no buildings; flat in the
//!   terrain), a parallel **taxiway** with connectors (sub-kind 3, medium / large), an **apron**
//!   (sub-kind 4), a **terminal** (kind `transportation` -> civic), **hangars** (`hangar` ->
//!   industrial), a **control tower** (`office`, tagged height; medium / large) and the **fence**
//!   (oriented rectangle around everything plus `fence_margin_m`; ground sub-kind 0 = airfield
//!   grass).
//! * **Lights** for `lighted` runways: edge (white), threshold (green), end (red), approach row
//!   (paved runways >= `approach_min_runway_m`) and taxiway edge lights (blue, medium / large).
//! * **Strip rule** ([`StripIndex`]): the terrain levels each runway inside the height function
//!   (section 5.1 step 9): along the centreline linear between the heights at the two landing
//!   thresholds, constant across; the rest of the fence takes the height of the nearest point of
//!   the nearest strip; outside the fence it blends into the surroundings over `shoulder_m`.
//!   Strips that overlap (or come within 1 m) share the profile of their group's longest runway,
//!   so the height never steps on a runway (S1 fix; before, the first key won the overlap).

use super::files::{q7, q_cm, Airport, Runway};
use super::geom::{m_per_deg_lon, M_PER_DEG};
use super::hash::{airport_seed, unit, Seed};
use crate::profile::SeedProfile;
use crate::terrain::noise::smoothstep;

/// Feet -> metres.
pub const FT: f64 = 0.3048;

/// Ground sub-kinds of class 7 `aeroway` (ground COLOR `b` = sub-kind / 8).
pub const SUB_FIELD: u8 = 0;
pub const SUB_RUNWAY_PAVED: u8 = 1;
pub const SUB_RUNWAY_GRASS: u8 = 2;
pub const SUB_TAXIWAY: u8 = 3;
pub const SUB_APRON: u8 = 4;

/// Ground stacking levels of the airport areas (S1 fix, contract section 9 "Airports"): the
/// fence is cut out of every other area of the tile (countryside and water: "airports cut
/// water"; `GroundFeatures::cutouts`), so inside it only these three stack, directly on the
/// strip height (no drape lift inside a fence): the runway lies `3 x lift_step_m` above
/// `ground_height_at` (R0 0.06 m).
pub const LEVEL_FIELD: u8 = 1;
pub const LEVEL_PAVEMENT: u8 = 2;
pub const LEVEL_RUNWAY: u8 = 3;
/// Stacking level the approach lights stand on: they run out of the fence over countryside or
/// water, whose highest area level is 7 (water).
pub const LEVEL_APPROACH: u8 = 7;

/// Local equirectangular plane at a reference point (metres; x east, y north).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocalFrame {
    pub lat0: f64,
    pub lon0: f64,
    pub kx: f64,
}

impl LocalFrame {
    pub fn new(lat0: f64, lon0: f64) -> Self {
        Self { lat0, lon0, kx: m_per_deg_lon(lat0) }
    }
    #[inline]
    pub fn xy(&self, lat: f64, lon: f64) -> [f64; 2] {
        [(lon - self.lon0) * self.kx, (lat - self.lat0) * M_PER_DEG]
    }
    /// (lat, lon) of a plane point.
    #[inline]
    pub fn ll(&self, p: [f64; 2]) -> (f64, f64) {
        (self.lat0 + p[1] / M_PER_DEG, self.lon0 + p[0] / self.kx)
    }
}

/// Oriented rectangle in a local plane: centre, unit axes `u` / `v`, half extents.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Obb {
    pub c: [f64; 2],
    pub u: [f64; 2],
    pub half: [f64; 2],
}

impl Obb {
    #[inline]
    pub fn v(&self) -> [f64; 2] {
        [-self.u[1], self.u[0]]
    }
    /// Distance from `p` to the rectangle (0 inside).
    #[inline]
    pub fn distance(&self, p: [f64; 2]) -> f64 {
        let d = [p[0] - self.c[0], p[1] - self.c[1]];
        let v = self.v();
        let a = (d[0] * self.u[0] + d[1] * self.u[1]).abs() - self.half[0];
        let b = (d[0] * v[0] + d[1] * v[1]).abs() - self.half[1];
        let (a, b) = (a.max(0.0), b.max(0.0));
        if a == 0.0 {
            b
        } else if b == 0.0 {
            a
        } else {
            (a * a + b * b).sqrt()
        }
    }
    /// Corner ring (counter-clockwise in x east / y north).
    pub fn ring(&self) -> Vec<[f64; 2]> {
        let v = self.v();
        let at = |su: f64, sv: f64| {
            [
                self.c[0] + self.u[0] * su * self.half[0] + v[0] * sv * self.half[1],
                self.c[1] + self.u[1] * su * self.half[0] + v[1] * sv * self.half[1],
            ]
        };
        vec![at(-1.0, -1.0), at(1.0, -1.0), at(1.0, 1.0), at(-1.0, 1.0)]
    }
}

/// One runway in the airport's plane.
#[derive(Clone, Debug, PartialEq)]
pub struct RunwayGeo {
    /// `"<le>/<he>"`.
    pub key: String,
    pub le: String,
    pub he: String,
    /// Physical ends (lat, lon; quantized) and plane positions.
    pub le_ll: (f64, f64),
    pub he_ll: (f64, f64),
    pub a: [f64; 2],
    pub b: [f64; 2],
    /// Unit direction le -> he and length (m).
    pub dir: [f64; 2],
    pub length_m: f64,
    pub width_m: f64,
    /// Half-width of the strip (m).
    pub strip_half_m: f64,
    pub paved: bool,
    pub lighted: bool,
    /// An end was missing in the file: placed at the reference point along the heading.
    pub approximate: bool,
    /// Landing thresholds as distances from the le end (displaced thresholds applied).
    pub le_thr_s: f64,
    pub he_thr_s: f64,
    /// Terrain height at the thresholds before any strip (m, quantized to 1 cm).
    pub le_thr_h: f64,
    pub he_thr_h: f64,
    /// True heading le -> he (degrees, 0 = north, clockwise).
    pub heading_deg: f64,
    pub surface: String,
}

impl RunwayGeo {
    /// Plane point at distance `s` from the le end along the centreline, `t` to the left.
    pub fn at(&self, s: f64, t: f64) -> [f64; 2] {
        let v = [-self.dir[1], self.dir[0]];
        [self.a[0] + self.dir[0] * s + v[0] * t, self.a[1] + self.dir[1] * s + v[1] * t]
    }
    /// Strip height at distance `s` from the le end (clamped to the physical runway): linear
    /// through the two threshold heights.
    pub fn height_at(&self, s: f64) -> f64 {
        let s = s.clamp(0.0, self.length_m);
        let span = self.he_thr_s - self.le_thr_s;
        if span <= 1.0 {
            return (self.le_thr_h + self.he_thr_h) * 0.5;
        }
        self.le_thr_h + (self.he_thr_h - self.le_thr_h) * ((s - self.le_thr_s) / span)
    }
}

/// A generated building of an airport (plane ring, counter-clockwise).
#[derive(Clone, Debug, PartialEq)]
pub struct AirportBuilding {
    pub id: u64,
    pub kind: &'static str,
    pub height_m: f32,
    pub levels: f32,
    pub ring: Vec<[f64; 2]>,
}

/// A ground area of an airport (plane ring).
#[derive(Clone, Debug, PartialEq)]
pub struct AirportArea {
    pub sub: u8,
    pub level: u8,
    pub ring: Vec<[f64; 2]>,
}

/// One airport light (plane point; color rgba with a = intensity).
#[derive(Clone, Debug, PartialEq)]
pub struct AirportLight {
    pub p: [f64; 2],
    pub color: [f32; 4],
    /// Stacking level of the pavement it stands on (the light sits on the drawn ground).
    pub level: u8,
    /// Index of the runway it belongs to (`usize::MAX` = taxiway).
    pub runway: usize,
}

/// A built airport.
#[derive(Clone, Debug, PartialEq)]
pub struct AirportGeo {
    pub ident: String,
    pub name: String,
    pub kind: String,
    pub lat: f64,
    pub lon: f64,
    pub elevation_ft: Option<f64>,
    pub frame: LocalFrame,
    pub seed: Seed,
    pub runways: Vec<RunwayGeo>,
    /// Index of the main (longest) runway.
    pub main: usize,
    pub fence: Obb,
    pub areas: Vec<AirportArea>,
    pub buildings: Vec<AirportBuilding>,
    pub lights: Vec<AirportLight>,
    /// lat/lon box [w, s, e, n] of everything the airport draws (fence, approach lights).
    pub bbox: [f64; 4],
    /// lat/lon box of the terrain influence (fence + shoulder).
    pub terrain_bbox: [f64; 4],
}

/// Designator heading: the numeric part of `"09L"` x 10 degrees (magnetic; an approximation).
fn designator_heading(d: &str) -> Option<f64> {
    let n: String = d.chars().take_while(|c| c.is_ascii_digit()).collect();
    n.parse::<f64>().ok().filter(|v| (1.0..=36.0).contains(v)).map(|v| v * 10.0)
}

/// Paved unless the surface reads as grass / earth / sand / gravel.
pub fn is_paved(surface: &str) -> bool {
    let s = surface.trim().to_ascii_uppercase();
    !["GR", "TURF", "DIRT", "EARTH", "SAND", "SOIL", "CLAY", "SNOW", "WATER"].iter().any(|p| s.starts_with(p))
}

/// Unit vector of a true heading (degrees from north, clockwise) in x east / y north.
fn heading_dir(deg: f64) -> [f64; 2] {
    let (s, c) = crate::math::sin_cos(deg.to_radians());
    [s, c]
}

/// The heights the strip rule levels to: `height(lat, lon)` = the terrain before any strip.
pub type HeightFn<'a> = dyn Fn(f64, f64) -> f64 + 'a;

/// Build one airport (section 5.1). `cities` = (lat, lon) of the city centres that link this
/// airport (apron side), `height` = the terrain before any strip.
pub fn build_airport(a: &Airport, cities: &[(f64, f64)], p: &SeedProfile, height: &HeightFn<'_>) -> AirportGeo {
    let ap = &p.airport;
    let kind = a.kind.as_str();
    let frame = LocalFrame::new(a.lat, a.lon);
    let seed = airport_seed(&a.ident);
    let mut runways: Vec<RunwayGeo> = a.runways.iter().filter(|r| !r.closed).map(|r| runway_geo(r, &frame, kind, p, height)).collect();
    runways.sort_by(|x, y| x.key.cmp(&y.key));
    // Main runway: the longest (ties: the first key).
    let main = runways
        .iter()
        .enumerate()
        .fold(0usize, |best, (i, r)| if r.length_m > runways[best].length_m + 1e-9 { i } else { best });
    let mut g = AirportGeo {
        ident: a.ident.clone(),
        name: a.name.clone(),
        kind: a.kind.clone(),
        lat: a.lat,
        lon: a.lon,
        elevation_ft: a.elevation_ft,
        frame,
        seed,
        runways,
        main,
        fence: Obb { c: [0.0; 2], u: [1.0, 0.0], half: [0.0; 2] },
        areas: Vec::new(),
        buildings: Vec::new(),
        lights: Vec::new(),
        bbox: [0.0; 4],
        terrain_bbox: [0.0; 4],
    };
    if g.runways.is_empty() {
        return g;
    }
    layout(&mut g, cities, p);
    lights(&mut g, p);
    // Boxes in lat/lon.
    let mut pts: Vec<[f64; 2]> = g.fence.ring();
    pts.extend(g.lights.iter().map(|l| l.p));
    g.bbox = ll_box(&g.frame, &pts, 0.0);
    let grown = Obb { half: [g.fence.half[0] + ap.shoulder_m, g.fence.half[1] + ap.shoulder_m], ..g.fence };
    g.terrain_bbox = ll_box(&g.frame, &grown.ring(), 1.0);
    g
}

fn ll_box(f: &LocalFrame, pts: &[[f64; 2]], pad_m: f64) -> [f64; 4] {
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for p in pts {
        let (lat, lon) = f.ll(*p);
        b = [b[0].min(lon), b[1].min(lat), b[2].max(lon), b[3].max(lat)];
    }
    let (dx, dy) = (pad_m / f.kx, pad_m / M_PER_DEG);
    [b[0] - dx, b[1] - dy, b[2] + dx, b[3] + dy]
}

fn runway_geo(r: &Runway, f: &LocalFrame, kind: &str, p: &SeedProfile, height: &HeightFn<'_>) -> RunwayGeo {
    let ap = &p.airport;
    let width_m = r.width_ft.filter(|w| *w > 0.0).map_or(ap.runway_width_m.get(kind), |w| w * FT);
    let (a, b, approximate) = match (r.le_lat, r.le_lon, r.he_lat, r.he_lon) {
        (Some(la), Some(lo), Some(hla), Some(hlo)) if (la, lo) != (hla, hlo) => (f.xy(la, lo), f.xy(hla, hlo), false),
        _ => {
            // Centred on the reference point along the true heading (else the designator).
            let hdg = r.le_heading_deg_t.or_else(|| designator_heading(&r.le)).unwrap_or(0.0);
            let d = heading_dir(hdg);
            let len = r.length_ft.filter(|l| *l > 0.0).map_or(ap.default_length_m, |l| l * FT);
            ([-d[0] * len * 0.5, -d[1] * len * 0.5], [d[0] * len * 0.5, d[1] * len * 0.5], true)
        }
    };
    // Quantize the ends like any input (1e-7 degrees), then work from the quantized points.
    let (la, lo) = f.ll(a);
    let (hla, hlo) = f.ll(b);
    let le_ll = (q7(la), q7(lo));
    let he_ll = (q7(hla), q7(hlo));
    let a = f.xy(le_ll.0, le_ll.1);
    let b = f.xy(he_ll.0, he_ll.1);
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let length_m = (dx * dx + dy * dy).sqrt().max(1.0);
    let dir = [dx / length_m, dy / length_m];
    let le_d = r.le_displaced_threshold_ft.unwrap_or(0.0).max(0.0) * FT;
    let he_d = r.he_displaced_threshold_ft.unwrap_or(0.0).max(0.0) * FT;
    let (le_thr_s, he_thr_s) = if le_d + he_d < length_m - 100.0 { (le_d, length_m - he_d) } else { (0.0, length_m) };
    let thr = |s: f64| {
        let q = [a[0] + dir[0] * s, a[1] + dir[1] * s];
        let (lat, lon) = f.ll(q);
        q_cm(height(lat, lon))
    };
    // Threshold heights from the terrain before any strip, the slope capped at `max_slope`
    // around their mean.
    let (mut le_thr_h, mut he_thr_h) = (thr(le_thr_s), thr(he_thr_s));
    let cap = ap.max_slope * (he_thr_s - le_thr_s);
    if (he_thr_h - le_thr_h).abs() > cap {
        let mean = (le_thr_h + he_thr_h) * 0.5;
        let half = if he_thr_h > le_thr_h { cap * 0.5 } else { -cap * 0.5 };
        le_thr_h = q_cm(mean - half);
        he_thr_h = q_cm(mean + half);
    }
    let mut heading_deg = crate::math::atan2(dir[0], dir[1]).to_degrees();
    if heading_deg < 0.0 {
        heading_deg += 360.0;
    }
    RunwayGeo {
        key: r.key(),
        le: r.le.clone(),
        he: r.he.clone(),
        le_ll,
        he_ll,
        a,
        b,
        dir,
        length_m,
        width_m,
        strip_half_m: ap.strip_half_m.get(kind).max(width_m * 0.5),
        paved: is_paved(&r.surface),
        lighted: r.lighted,
        approximate,
        le_thr_s,
        he_thr_s,
        le_thr_h,
        he_thr_h,
        heading_deg,
        surface: r.surface.clone(),
    }
}

/// Rectangle in the main runway's (s, t) frame -> plane ring (counter-clockwise).
fn rect_st(r: &RunwayGeo, s0: f64, s1: f64, t0: f64, t1: f64) -> Vec<[f64; 2]> {
    let (s0, s1) = (s0.min(s1), s0.max(s1));
    let (t0, t1) = (t0.min(t1), t0.max(t1));
    // (s, t) is right-handed like (x, y) (t to the left of the direction), so this order is CCW.
    vec![r.at(s0, t0), r.at(s1, t0), r.at(s1, t1), r.at(s0, t1)]
}

fn building(g: &AirportGeo, i: u64, kind: &'static str, height_m: f32, levels: f32, ring: Vec<[f64; 2]>) -> AirportBuilding {
    // OSM id space of generated buildings: 2^40 + 40 bits of hash (never a real OSM id).
    let h = crate::seed_hash!(g.seed, "building", i);
    AirportBuilding { id: (1u64 << 40) | (h >> 24), kind, height_m, levels, ring }
}

fn layout(g: &mut AirportGeo, cities: &[(f64, f64)], p: &SeedProfile) {
    let ap = &p.airport;
    let kind = g.kind.clone();
    let large = kind == "large_airport";
    let medium = kind == "medium_airport";
    let m = g.runways[g.main].clone();
    let l = m.length_m;
    // Apron side (+1 = left of le -> he): toward the nearest linked city centre, else the seed.
    let side = {
        let near = cities
            .iter()
            .map(|&(lat, lon)| g.frame.xy(lat, lon))
            .min_by(|a, b| (a[0] * a[0] + a[1] * a[1]).total_cmp(&(b[0] * b[0] + b[1] * b[1])));
        let left = [-m.dir[1], m.dir[0]];
        match near {
            Some(c) => {
                let mid = m.at(l * 0.5, 0.0);
                let d = (c[0] - mid[0]) * left[0] + (c[1] - mid[1]) * left[1];
                if d.abs() > 1.0 {
                    d.signum()
                } else if g.seed.0 & 1 == 0 {
                    1.0
                } else {
                    -1.0
                }
            }
            None if g.seed.0 & 1 == 0 => 1.0,
            None => -1.0,
        }
    };
    let rw_half = m.width_m * 0.5;
    // Runways.
    for r in &g.runways {
        let sub = if r.paved { SUB_RUNWAY_PAVED } else { SUB_RUNWAY_GRASS };
        g.areas.push(AirportArea { sub, level: LEVEL_RUNWAY, ring: rect_st(r, 0.0, r.length_m, -r.width_m * 0.5, r.width_m * 0.5) });
    }
    // Parallel taxiway with connectors (main runway only).
    let taxi = ap.taxi_offset_m.get(&kind);
    let tw = ap.taxiway_width_m.get(&kind);
    let mut far_t = rw_half; // outer edge (|t|) of the pavement so far on the apron side
    if taxi > rw_half + tw && m.paved && (large || medium) {
        let t = side * taxi;
        let (s0, s1) = (30.0f64.min(l * 0.1), l - 30.0f64.min(l * 0.1));
        g.areas.push(AirportArea { sub: SUB_TAXIWAY, level: LEVEL_PAVEMENT, ring: rect_st(&m, s0, s1, t - tw * 0.5, t + tw * 0.5) });
        let n = ((l / 1000.0).round() as usize).max(1);
        for k in 0..=n {
            let s = s0 + tw * 0.5 + (s1 - s0 - tw) * (k as f64 / n as f64);
            g.areas.push(AirportArea {
                sub: SUB_TAXIWAY,
                level: LEVEL_PAVEMENT,
                ring: rect_st(&m, s - tw * 0.5, s + tw * 0.5, side * rw_half, side * (taxi - tw * 0.5)),
            });
        }
        far_t = taxi + tw * 0.5;
    }
    // Apron, terminal, hangars, tower: centred on the runway middle, on the apron side.
    let (apron_len, apron_depth, term_len, term_depth, term_h, term_levels, hangar, hangar_h, tower_h) = if large {
        (0.35 * l, 200.0, 0.22 * l, 60.0, 20.0, 3.0, [70.0, 60.0], 18.0, 45.0)
    } else if medium {
        (0.25 * l, 140.0, 0.14 * l, 40.0, 12.0, 2.0, [50.0, 40.0], 14.0, 30.0)
    } else {
        (60.0f64.min(0.3 * l), 40.0, 20.0f64.min(0.15 * l), 12.0, 5.0, 1.0, [25.0, 20.0], 7.0, 0.0)
    };
    let mid = l * 0.5;
    let apron_t0 = if far_t > rw_half { far_t } else { m.strip_half_m.min(rw_half + 60.0) };
    if far_t <= rw_half {
        // Small field: a connector from the runway edge to the apron.
        let w = tw.max(6.0);
        g.areas.push(AirportArea { sub: SUB_TAXIWAY, level: LEVEL_PAVEMENT, ring: rect_st(&m, mid - w * 0.5, mid + w * 0.5, side * rw_half, side * apron_t0) });
    }
    let apron_t1 = apron_t0 + apron_depth;
    g.areas.push(AirportArea {
        sub: SUB_APRON,
        level: LEVEL_PAVEMENT,
        ring: rect_st(&m, mid - apron_len * 0.5, mid + apron_len * 0.5, side * apron_t0, side * apron_t1),
    });
    // Terminal behind the apron; car park (apron pavement) behind it.
    let gap = 10.0;
    let tt0 = apron_t1 + gap;
    let tt1 = tt0 + term_depth;
    let mut bi = 0u64;
    let b = building(g, bi, "transportation", term_h, term_levels, rect_st(&m, mid - term_len * 0.5, mid + term_len * 0.5, side * tt0, side * tt1));
    g.buildings.push(b);
    bi += 1;
    if large || medium {
        g.areas.push(AirportArea {
            sub: SUB_APRON,
            level: LEVEL_PAVEMENT,
            ring: rect_st(&m, mid - term_len * 0.4, mid + term_len * 0.4, side * (tt1 + gap), side * (tt1 + gap + 80.0)),
        });
    }
    // Hangars along the apron edge beyond the terminal's far end (count from the airport seed).
    let n_hangars = if large { 2 + (unit(crate::seed_hash!(g.seed, "hangars")) * 3.0) as usize } else if medium { 1 + (unit(crate::seed_hash!(g.seed, "hangars")) * 3.0) as usize } else { 1 + (unit(crate::seed_hash!(g.seed, "hangars")) * 2.0) as usize };
    let mut s = mid + apron_len * 0.5 + 20.0;
    for _ in 0..n_hangars {
        if s + hangar[0] > l {
            break;
        }
        let r = rect_st(&m, s, s + hangar[0], side * apron_t0, side * (apron_t0 + hangar[1]));
        let b = building(g, bi, "hangar", hangar_h, 1.0, r);
        g.buildings.push(b);
        bi += 1;
        s += hangar[0] + 15.0;
    }
    // Control tower at the apron's near end.
    if tower_h > 0.0 {
        let s0 = mid - apron_len * 0.5 - 30.0;
        let r = rect_st(&m, s0 - 12.0, s0, side * (apron_t0 + 10.0), side * (apron_t0 + 22.0));
        let b = building(g, bi, "office", tower_h, (tower_h / 3.0).round(), r);
        g.buildings.push(b);
    }
    // Fence: around every strip (+ strip_end_m), pavement and building, in the main runway's frame.
    let u = m.dir;
    let v = [-u[1], u[0]];
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    let mut add = |q: [f64; 2]| {
        let d = [q[0] - m.a[0], q[1] - m.a[1]];
        let (su, sv) = (d[0] * u[0] + d[1] * u[1], d[0] * v[0] + d[1] * v[1]);
        lo = [lo[0].min(su), lo[1].min(sv)];
        hi = [hi[0].max(su), hi[1].max(sv)];
    };
    for r in &g.runways {
        for q in rect_st(r, -ap.strip_end_m, r.length_m + ap.strip_end_m, -r.strip_half_m, r.strip_half_m) {
            add(q);
        }
    }
    for q in g.areas.iter().flat_map(|a| a.ring.iter()).chain(g.buildings.iter().flat_map(|b| b.ring.iter())) {
        add(*q);
    }
    let margin = ap.fence_margin_m;
    let (cu, cv) = ((lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5);
    g.fence = Obb {
        c: [m.a[0] + u[0] * cu + v[0] * cv, m.a[1] + u[1] * cu + v[1] * cv],
        u,
        half: [(hi[0] - lo[0]) * 0.5 + margin, (hi[1] - lo[1]) * 0.5 + margin],
    };
    g.areas.insert(0, AirportArea { sub: SUB_FIELD, level: LEVEL_FIELD, ring: g.fence.ring() });
}

fn rgba(c: [f64; 3], a: f64) -> [f32; 4] {
    [c[0] as f32, c[1] as f32, c[2] as f32, a as f32]
}

fn lights(g: &mut AirportGeo, p: &SeedProfile) {
    let lp = &p.airport.lights;
    let a = lp.intensity;
    let (edge, thr, end, appr, taxi) =
        (rgba(lp.edge_color, a), rgba(lp.threshold_color, a), rgba(lp.end_color, a), rgba(lp.approach_color, a), rgba(lp.taxiway_color, a));
    let mut out = Vec::new();
    for (ri, r) in g.runways.iter().enumerate().filter(|(_, r)| r.lighted) {
        let half = r.width_m * 0.5 + 1.5;
        let l = r.length_m;
        // Edge lights: both edges, end to end, evenly spaced (both ends included).
        let n = ((l / lp.edge_spacing_m.max(1.0)).round() as usize).max(1);
        for k in 0..=n {
            let s = l * (k as f64 / n as f64);
            for t in [-half, half] {
                out.push(AirportLight { p: r.at(s, t), color: edge, level: LEVEL_RUNWAY, runway: ri });
            }
        }
        // Threshold (green, across at each landing threshold) and end lights (red, 3 m past each
        // physical end), every 3 m across the runway.
        let across = ((r.width_m / 3.0).floor() as usize).max(2);
        for k in 0..=across {
            let t = -r.width_m * 0.5 + r.width_m * (k as f64 / across as f64);
            out.push(AirportLight { p: r.at(r.le_thr_s, t), color: thr, level: LEVEL_RUNWAY, runway: ri });
            out.push(AirportLight { p: r.at(r.he_thr_s, t), color: thr, level: LEVEL_RUNWAY, runway: ri });
            out.push(AirportLight { p: r.at(-3.0, t), color: end, level: LEVEL_RUNWAY, runway: ri });
            out.push(AirportLight { p: r.at(l + 3.0, t), color: end, level: LEVEL_RUNWAY, runway: ri });
        }
        // Approach rows before both thresholds (centreline, outward).
        if r.paved && l >= lp.approach_min_runway_m && lp.approach_length_m > 0.0 {
            let n = (lp.approach_length_m / lp.approach_spacing_m.max(1.0)).floor() as usize;
            for k in 1..=n {
                let d = k as f64 * lp.approach_spacing_m;
                out.push(AirportLight { p: r.at(r.le_thr_s - d, 0.0), color: appr, level: LEVEL_APPROACH, runway: ri });
                out.push(AirportLight { p: r.at(r.he_thr_s + d, 0.0), color: appr, level: LEVEL_APPROACH, runway: ri });
            }
        }
    }
    // Taxiway edge lights along the parallel taxiway (blue), medium / large airports.
    let any_lit = g.runways.iter().any(|r| r.lighted);
    if any_lit && (g.kind == "large_airport" || g.kind == "medium_airport") {
        for area in g.areas.iter().filter(|a| a.sub == SUB_TAXIWAY) {
            // Long sides of the rectangle only.
            let r = &area.ring;
            let len = |i: usize| {
                let (a, b) = (r[i], r[(i + 1) % 4]);
                ((b[0] - a[0]) * (b[0] - a[0]) + (b[1] - a[1]) * (b[1] - a[1])).sqrt()
            };
            for i in 0..4 {
                if len(i) < len((i + 1) % 4) {
                    continue;
                }
                let (pa, pb) = (r[i], r[(i + 1) % 4]);
                let n = ((len(i) / p.airport.lights.taxiway_spacing_m.max(1.0)).round() as usize).max(1);
                for k in 0..=n {
                    let t = k as f64 / n as f64;
                    out.push(AirportLight {
                        p: [pa[0] + (pb[0] - pa[0]) * t, pa[1] + (pb[1] - pa[1]) * t],
                        color: taxi,
                        level: LEVEL_PAVEMENT,
                        runway: usize::MAX,
                    });
                }
            }
        }
    }
    g.lights = out;
}

// ---------------------------------------------------------------------------------------------
// The strip rule of the height function (section 5.1 step 9)

/// One airport's part of the strip rule.
#[derive(Clone, Debug, PartialEq)]
struct StripAirport {
    frame: LocalFrame,
    fence: Obb,
    runways: Vec<StripRunway>,
    /// lat/lon box [w, s, e, n] of the fence plus the shoulder (the strip rule's reach).
    bbox: [f64; 4],
}

#[derive(Clone, Debug, PartialEq)]
struct StripRunway {
    a: [f64; 2],
    dir: [f64; 2],
    length_m: f64,
    half_m: f64,
    le_thr_s: f64,
    he_thr_s: f64,
    le_thr_h: f64,
    he_thr_h: f64,
    /// Index (in the airport's runways) of the runway whose profile gives this strip its
    /// height: itself, or the longest runway of its group of overlapping strips.
    plane: usize,
}

/// Per runway of `g` (same order), the index of the runway whose centreline profile levels its
/// strip: itself, or the longest runway of its group of overlapping strips (see
/// [`StripIndex`]). Height there: `g.runways[i].height_at(along-axis distance on runway i)`.
pub fn runway_planes(g: &AirportGeo) -> Vec<usize> {
    let mut v: Vec<StripRunway> = g
        .runways
        .iter()
        .map(|r| StripRunway {
            a: r.a,
            dir: r.dir,
            length_m: r.length_m,
            half_m: r.strip_half_m,
            le_thr_s: r.le_thr_s,
            he_thr_s: r.he_thr_s,
            le_thr_h: r.le_thr_h,
            he_thr_h: r.he_thr_h,
            plane: 0,
        })
        .collect();
    group_planes(&mut v);
    v.iter().map(|r| r.plane).collect()
}

impl StripRunway {
    /// Along-axis distance of plane point `p` from the le end.
    #[inline]
    fn s_of(&self, p: [f64; 2]) -> f64 {
        let q = [p[0] - self.a[0], p[1] - self.a[1]];
        q[0] * self.dir[0] + q[1] * self.dir[1]
    }

    /// Strip rectangle corners (s in [0, length], |t| <= half), grown by `pad` metres.
    fn corners(&self, pad: f64) -> [[f64; 2]; 4] {
        let v = [-self.dir[1], self.dir[0]];
        let at = |s: f64, t: f64| [self.a[0] + self.dir[0] * s + v[0] * t, self.a[1] + self.dir[1] * s + v[1] * t];
        let (s0, s1, h) = (-pad, self.length_m + pad, self.half_m + pad);
        [at(s0, -h), at(s1, -h), at(s1, h), at(s0, h)]
    }
}

/// Whether two strip rectangles overlap or come within `pad` (separating-axis test).
fn strips_touch(x: &StripRunway, y: &StripRunway, pad: f64) -> bool {
    let (cx, cy) = (x.corners(pad * 0.5), y.corners(pad * 0.5));
    let axes = [x.dir, [-x.dir[1], x.dir[0]], y.dir, [-y.dir[1], y.dir[0]]];
    axes.iter().all(|ax| {
        let proj = |c: &[[f64; 2]; 4]| {
            c.iter().map(|p| p[0] * ax[0] + p[1] * ax[1]).fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| (lo.min(v), hi.max(v)))
        };
        let ((a0, a1), (b0, b1)) = (proj(&cx), proj(&cy));
        a0 <= b1 && b0 <= a1
    })
}

/// Group the strips of one airport that overlap (or come within 1 m): each strip takes the
/// profile of its group's longest runway (ties: the first key), so the height function is
/// continuous over crossing / close parallel runways and approximate runways stacked on the
/// reference point (S1 fix; before, the first key won and the height stepped across a runway).
fn group_planes(runways: &mut [StripRunway]) {
    let n = runways.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while p[r] != r {
            r = p[r];
        }
        p[i] = r;
        r
    }
    for i in 0..n {
        for j in i + 1..n {
            if strips_touch(&runways[i], &runways[j], 1.0) {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                if a != b {
                    parent[a.max(b)] = a.min(b);
                }
            }
        }
    }
    for i in 0..n {
        let root = find(&mut parent, i);
        // Longest member of the group (ties: the lowest index = first key).
        let mut best = root;
        for j in 0..n {
            if find(&mut parent, j) == root && runways[j].length_m > runways[best].length_m + 1e-9 {
                best = j;
            }
        }
        runways[i].plane = best;
    }
}

impl StripRunway {
    fn height_at(&self, s: f64) -> f64 {
        let s = s.clamp(0.0, self.length_m);
        let span = self.he_thr_s - self.le_thr_s;
        if span <= 1.0 {
            return (self.le_thr_h + self.he_thr_h) * 0.5;
        }
        self.le_thr_h + (self.he_thr_h - self.le_thr_h) * ((s - self.le_thr_s) / span)
    }
}

/// Grid cell size of the strip index (degrees).
const CELL_DEG: f64 = 0.02;

/// Every airport's runway strips with a coarse lat/lon grid, so a sample outside airports costs a
/// bounds check and one lookup.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StripIndex {
    airports: Vec<StripAirport>,
    shoulder_m: f64,
    /// Grid origin (west, south) and size.
    w: f64,
    s: f64,
    nx: usize,
    ny: usize,
    /// CSR: per cell the range of `items` (airport indices, ascending).
    start: Vec<u32>,
    items: Vec<u32>,
}

impl StripIndex {
    pub fn new(airports: &[AirportGeo], shoulder_m: f64) -> Self {
        let list: Vec<(&AirportGeo, StripAirport)> = airports
            .iter()
            .filter(|g| !g.runways.is_empty())
            .map(|g| {
                let mut runways: Vec<StripRunway> = g
                    .runways
                    .iter()
                    .map(|r| StripRunway {
                        a: r.a,
                        dir: r.dir,
                        length_m: r.length_m,
                        half_m: r.strip_half_m,
                        le_thr_s: r.le_thr_s,
                        he_thr_s: r.he_thr_s,
                        le_thr_h: r.le_thr_h,
                        he_thr_h: r.he_thr_h,
                        plane: 0,
                    })
                    .collect();
                group_planes(&mut runways);
                (g, StripAirport { frame: g.frame, fence: g.fence, runways, bbox: g.terrain_bbox })
            })
            .collect();
        if list.is_empty() {
            return Self { shoulder_m, ..Default::default() };
        }
        let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for (g, _) in &list {
            let t = g.terrain_bbox;
            b = [b[0].min(t[0]), b[1].min(t[1]), b[2].max(t[2]), b[3].max(t[3])];
        }
        let w = (b[0] / CELL_DEG).floor() * CELL_DEG;
        let s = (b[1] / CELL_DEG).floor() * CELL_DEG;
        let nx = (((b[2] - w) / CELL_DEG).floor() as usize) + 1;
        let ny = (((b[3] - s) / CELL_DEG).floor() as usize) + 1;
        let mut per: Vec<Vec<u32>> = vec![Vec::new(); nx * ny];
        for (i, (g, _)) in list.iter().enumerate() {
            let t = g.terrain_bbox;
            let (x0, x1) = (((t[0] - w) / CELL_DEG).floor() as usize, (((t[2] - w) / CELL_DEG).floor() as usize).min(nx - 1));
            let (y0, y1) = (((t[1] - s) / CELL_DEG).floor() as usize, (((t[3] - s) / CELL_DEG).floor() as usize).min(ny - 1));
            for y in y0..=y1 {
                for x in x0..=x1 {
                    per[y * nx + x].push(i as u32);
                }
            }
        }
        let mut start = Vec::with_capacity(nx * ny + 1);
        let mut items = Vec::new();
        start.push(0);
        for v in per {
            items.extend(v);
            start.push(items.len() as u32);
        }
        Self { airports: list.into_iter().map(|(_, a)| a).collect(), shoulder_m, w, s, nx, ny, start, items }
    }

    pub fn is_empty(&self) -> bool {
        self.airports.is_empty()
    }

    /// Bytes of the index (section 10a budget).
    pub fn bytes(&self) -> usize {
        self.start.len() * 4 + self.items.len() * 4 + self.airports.iter().map(|a| 128 + a.runways.len() * 80).sum::<usize>()
    }

    /// Airport indices whose terrain box covers the point's grid cell.
    #[inline]
    fn candidates(&self, lat: f64, lon: f64) -> &[u32] {
        if self.nx == 0 {
            return &[];
        }
        let fx = (lon - self.w) / CELL_DEG;
        let fy = (lat - self.s) / CELL_DEG;
        if !(fx >= 0.0 && fy >= 0.0 && fx < self.nx as f64 && fy < self.ny as f64) {
            return &[];
        }
        let c = fy as usize * self.nx + fx as usize;
        &self.items[self.start[c] as usize..self.start[c + 1] as usize]
    }

    /// The strip rule at a point: `Some((strip height, k))` with `k` the weight of the
    /// surrounding terrain (0 inside a fence, smoothstep to 1 at `shoulder_m` outside it);
    /// `None` = no airport influence. Several airports: the nearest fence (ties: the first by
    /// ident).
    #[inline]
    pub fn at(&self, lat: f64, lon: f64) -> Option<(f64, f64)> {
        let c = self.candidates(lat, lon);
        if c.is_empty() {
            return None;
        }
        let mut best: Option<(f64, f64)> = None;
        let mut best_d = f64::INFINITY;
        for &i in c {
            let ap = &self.airports[i as usize];
            let p = ap.frame.xy(lat, lon);
            let d = ap.fence.distance(p);
            if d >= self.shoulder_m && !(d == 0.0 && self.shoulder_m == 0.0) || d >= best_d {
                continue;
            }
            // Nearest strip (distance to its rectangle; ties: the earlier = lower key).
            let (mut h, mut sd) = (0.0, f64::INFINITY);
            for r in &ap.runways {
                let q = [p[0] - r.a[0], p[1] - r.a[1]];
                let s = q[0] * r.dir[0] + q[1] * r.dir[1];
                let t = -q[0] * r.dir[1] + q[1] * r.dir[0];
                let ds = (-s).max(s - r.length_m).max(0.0);
                let dt = (t.abs() - r.half_m).max(0.0);
                let dd = ds * ds + dt * dt;
                if dd < sd {
                    sd = dd;
                    let pl = &ap.runways[r.plane];
                    h = if std::ptr::eq(pl, r) { r.height_at(s) } else { pl.height_at(pl.s_of(p)) };
                }
            }
            best_d = d;
            best = Some((h, if d <= 0.0 { 0.0 } else { smoothstep(0.0, self.shoulder_m, d) }));
        }
        best
    }

    /// Drape-lift weight at a point (contract section 9 "Airports"): the strip rule's terrain
    /// weight `k` (0 inside a fence, smoothstep to 1 at `shoulder_m` outside it), 1 beyond every
    /// airport's reach. The drawn ground and the terrain mesh tolerance both scale by it.
    #[inline]
    pub fn lift_weight(&self, lat: f64, lon: f64) -> f64 {
        self.at(lat, lon).map_or(1.0, |(_, k)| k)
    }

    /// Whether a lat/lon box [w, s, e, n] overlaps any airport's fence + shoulder box (outside,
    /// [`StripIndex::lift_weight`] is 1 everywhere and the plain drape applies).
    pub fn touches(&self, b: [f64; 4]) -> bool {
        self.airports.iter().any(|a| a.bbox[0] <= b[2] && b[0] <= a.bbox[2] && a.bbox[1] <= b[3] && b[1] <= a.bbox[3])
    }

    /// Whether the point is inside an airport fence (ground class 7 on the terrain).
    pub fn in_fence(&self, lat: f64, lon: f64) -> bool {
        self.candidates(lat, lon).iter().any(|&i| {
            let ap = &self.airports[i as usize];
            ap.fence.distance(ap.frame.xy(lat, lon)) <= 0.0
        })
    }
}
