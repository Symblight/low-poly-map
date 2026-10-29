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

use super::files::Airport;
use super::geom::{m_per_deg_lon, M_PER_DEG};
use super::hash::{airport_seed, Seed};
use crate::profile::SeedProfile;

mod layout;
mod lights;
mod strips;

pub use strips::{runway_planes, StripIndex};

use layout::{layout, runway_geo};
use lights::lights;

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
