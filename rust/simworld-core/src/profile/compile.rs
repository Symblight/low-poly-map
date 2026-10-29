//! Validation of a [`WorldProfile`] (cross-key rules) and compilation into the lookup tables of
//! [`Profile`].

use ahash::AHashMap;

use crate::ground::GroundParams;
use crate::mesher::{
    LodParams, PROFILE_CIVIC, PROFILE_COUNT, PROFILE_DARK, PROFILE_INDUSTRIAL, PROFILE_LANDMARK, PROFILE_OFFICE,
    PROFILE_RESIDENTIAL, PROFILE_RETAIL, PROFILE_UNKNOWN,
};
use crate::mvt::{BuildingTags, Landuse, OsmType};
use crate::rings::{BandParams, RING_COUNT};

use super::ranges::{check_ranges, fmt_num};
use super::*;

#[derive(Clone, Debug, PartialEq)]
pub(super) enum StateRule {
    /// `historic` / `historic=value`.
    Historic(Option<String>),
    /// `man_made` / `man_made=value`.
    ManMade(Option<String>),
    Ruins,
    Disused,
    Abandoned,
}

impl StateRule {
    fn parse(s: &str) -> Option<Self> {
        let (k, v) = match s.split_once('=') {
            Some((k, v)) => (k.trim(), Some(v.trim())),
            None => (s.trim(), None),
        };
        if v.is_some_and(str::is_empty) {
            return None;
        }
        let flag = |r: Self| match v {
            None | Some("yes") => Some(r),
            _ => None,
        };
        match k {
            "historic" => Some(Self::Historic(v.map(str::to_string))),
            "man_made" => Some(Self::ManMade(v.map(str::to_string))),
            "ruins" => flag(Self::Ruins),
            "disused" => flag(Self::Disused),
            "abandoned" => flag(Self::Abandoned),
            _ => None,
        }
    }
    pub(super) fn matches(&self, t: &BuildingTags) -> bool {
        let text = |have: &Option<String>, want: &Option<String>| match (have, want) {
            (Some(h), Some(w)) => h == w,
            (Some(_), None) => true,
            (None, _) => false,
        };
        match self {
            Self::Historic(v) => text(&t.historic, v),
            Self::ManMade(v) => text(&t.man_made, v),
            Self::Ruins => t.ruins,
            Self::Disused => t.disused,
            Self::Abandoned => t.abandoned,
        }
    }
}

/// `n123` / `w123` / `r123` -> (OSM type tag, id).
fn parse_osm_ref(s: &str) -> Option<(u64, u64)> {
    let mut c = s.chars();
    let t = match c.next()? {
        'n' => OsmType::Node,
        'w' => OsmType::Way,
        'r' => OsmType::Relation,
        _ => return None,
    };
    let digits = c.as_str();
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((t.tag(), digits.parse().ok()?))
}

/// Hours checked by the lit-share rise warning: 18 h .. 23 h, 0 h .. 3 h.
const EVENING_HOURS: [usize; 10] = [18, 19, 20, 21, 22, 23, 0, 1, 2, 3];

pub(super) fn landuse_index(l: Option<Landuse>) -> usize {
    match l {
        Some(Landuse::Residential) => 0,
        Some(Landuse::Commercial) => 1,
        Some(Landuse::Retail) => 2,
        Some(Landuse::Industrial) => 3,
        Some(Landuse::Civic) => 4,
        Some(Landuse::Other) => 5,
        None => 6,
    }
}

impl Profile {
    /// Validate (ranges and cross-key rules) and compile. Errors name the key, the value and
    /// the allowed range; several problems are reported together.
    pub fn new(world: WorldProfile) -> Result<Self, String> {
        let mut errs = Vec::new();
        let json = serde_json::to_value(&world).map_err(|e| format!("profile: {e}"))?;
        check_ranges(&json, "", &mut errs);
        let b = &world.bands;
        let lod = &world.lod;
        let errors = [lod.error_m.r1, lod.error_m.r2, lod.error_m.r3];
        for r in 1..errors.len() {
            if errors[r] <= errors[r - 1] {
                errs.push(format!(
                    "lod.error_m.r{} = {} must be greater than lod.error_m.r{} = {}",
                    r + 1,
                    fmt_num(errors[r]),
                    r,
                    fmt_num(errors[r - 1])
                ));
            }
        }
        let a = &lod.adaptive;
        if !(a.min_scale <= 1.0 && 1.0 <= a.max_scale) {
            errs.push(format!(
                "lod.adaptive.min_scale = {} <= 1 <= lod.adaptive.max_scale = {} does not hold",
                fmt_num(a.min_scale),
                fmt_num(a.max_scale)
            ));
        }
        let l = &world.lights;
        if l.cluster_off_agl_m > l.cluster_on_agl_m {
            errs.push(format!(
                "lights.cluster_off_agl_m = {} must not exceed lights.cluster_on_agl_m = {}",
                fmt_num(l.cluster_off_agl_m),
                fmt_num(l.cluster_on_agl_m)
            ));
        }
        let t = &world.terrain;
        if t.near_zoom < t.far_zoom {
            errs.push(format!("terrain.near_zoom = {} must not be below terrain.far_zoom = {}", t.near_zoom, t.far_zoom));
        } else if t.bands.len() != (t.near_zoom - t.far_zoom + 1) as usize {
            errs.push(format!(
                "terrain.bands has {} entries, needs {} (one per zoom from terrain.near_zoom = {} down to terrain.far_zoom = {})",
                t.bands.len(),
                t.near_zoom - t.far_zoom + 1,
                t.near_zoom,
                t.far_zoom
            ));
        }
        for (i, tb) in t.bands.iter().enumerate() {
            if i > 0 && tb.edge_m <= t.bands[i - 1].edge_m {
                errs.push(format!(
                    "terrain.bands[{i}].edge_m = {} must be greater than terrain.bands[{}].edge_m = {}",
                    fmt_num(tb.edge_m),
                    i - 1,
                    fmt_num(t.bands[i - 1].edge_m)
                ));
            }
            if tb.patch_cells != 0 && (tb.patch_cells < 16 || !tb.patch_cells.is_power_of_two()) {
                errs.push(format!("terrain.bands[{i}].patch_cells = {} must be 0 (by zoom) or a power of two from 16 to 512", tb.patch_cells));
            }
            if tb.max_edge_m != 0.0 && tb.max_edge_m < 500.0 {
                errs.push(format!(
                    "terrain.bands[{i}].max_edge_m = {} must be 0 (corners only) or at least 500",
                    fmt_num(tb.max_edge_m)
                ));
            }
        }
        let s = &world.streaming;
        if s.retry_max_s < s.retry_base_s {
            errs.push(format!(
                "streaming.retry_max_s = {} must not be below streaming.retry_base_s = {}",
                fmt_num(s.retry_max_s),
                fmt_num(s.retry_base_s)
            ));
        }

        // Light profiles: kind -> id (a kind in two profiles is ambiguous). One table for
        // `building`, `historic` and `man_made` values.
        let mut kind_profile = AHashMap::new();
        let lp = &l.profiles;
        let named = [
            ("residential", &lp.residential, PROFILE_RESIDENTIAL),
            ("office", &lp.office, PROFILE_OFFICE),
            ("retail", &lp.retail, PROFILE_RETAIL),
            ("industrial", &lp.industrial, PROFILE_INDUSTRIAL),
            ("civic", &lp.civic, PROFILE_CIVIC),
            ("landmark", &lp.landmark, PROFILE_LANDMARK),
            ("dark", &lp.dark, PROFILE_DARK),
        ];
        let mut profile_colors = [[1.0f32; 4]; PROFILE_COUNT];
        let mut window_width = [None; PROFILE_COUNT];
        let mut warnings = Vec::new();
        for (name, p, id) in named {
            profile_colors[id as usize] = [p.color[0] as f32, p.color[1] as f32, p.color[2] as f32, 1.0];
            for k in &p.kinds {
                if let Some(prev) = kind_profile.insert(k.clone(), id) {
                    if prev != id {
                        errs.push(format!("lights.profiles.{name}.kinds: \"{k}\" is already in another profile"));
                    }
                }
            }
            match (&p.windows, id) {
                (Some(_), PROFILE_INDUSTRIAL | PROFILE_LANDMARK | PROFILE_DARK) => errs.push(format!(
                    "lights.profiles.{name}.windows: the {name} profile has no windows (a `windows=yes` building borrows its landuse fallback profile's)"
                )),
                (Some(w), _) => {
                    window_width[id as usize] = Some(w.width_m as f32);
                    if w.lit_share.len() != 24 {
                        errs.push(format!(
                            "lights.profiles.{name}.windows.lit_share has {} values, needs 24 (one per local hour 0..23)",
                            w.lit_share.len()
                        ));
                    } else if let Some(i) =
                        (1..EVENING_HOURS.len()).find(|&i| w.lit_share[EVENING_HOURS[i]] > w.lit_share[EVENING_HOURS[i - 1]])
                    {
                        let (a, b) = (EVENING_HOURS[i - 1], EVENING_HOURS[i]);
                        warnings.push(format!(
                            "lights.profiles.{name}.windows.lit_share rises from {a} h ({}) to {b} h ({}): between 18 h and 3 h it should only fall, or windows switch on again while the evening goes on",
                            fmt_num(w.lit_share[a]),
                            fmt_num(w.lit_share[b])
                        ));
                    }
                }
                (None, _) => {}
            }
            match (&p.floodlight, id) {
                (None, PROFILE_LANDMARK) => errs.push("lights.profiles.landmark.floodlight is required".into()),
                (Some(_), i) if i != PROFILE_LANDMARK => {
                    errs.push(format!("lights.profiles.{name}.floodlight: only the landmark profile has a floodlight"))
                }
                _ => {}
            }
        }
        // Id 5 ("unknown") never reaches the output; give it residential's color.
        profile_colors[PROFILE_UNKNOWN as usize] = profile_colors[PROFILE_RESIDENTIAL as usize];

        let bw = &world.buildings;
        let mut overrides = AHashMap::new();
        for (k, v) in &bw.overrides {
            match parse_osm_ref(k) {
                Some(r) => {
                    overrides.insert(r, v.id());
                }
                None => errs.push(format!(
                    "buildings.overrides: \"{k}\" is not an OSM id (n, w or r followed by digits, e.g. w123456)"
                )),
            }
        }
        let mut dark_states = Vec::new();
        for d in &bw.dark_states {
            match StateRule::parse(d) {
                Some(r) => dark_states.push(r),
                None => errs.push(format!(
                    "buildings.dark_states: \"{d}\" is not `key` or `key=value` over historic, man_made, ruins, disused, abandoned (the flags take no value but `yes`)"
                )),
            }
        }

        let f = &l.landuse_fallback;
        let landuse_profile =
            [f.residential.id(), f.commercial.id(), f.retail.id(), f.industrial.id(), f.civic.id(), f.other.id(), f.none.id()];

        // Ground areas: kind -> (class, level).
        let g = &world.ground;
        let mut area_kinds = AHashMap::new();
        for (name, a) in &g.areas {
            for k in &a.kinds {
                if area_kinds.insert(k.clone(), (a.class.id(), a.level)).is_some() {
                    errs.push(format!("ground.areas.{name}.kinds: \"{k}\" is already in another area group"));
                }
            }
        }
        // Roads: highway -> class.
        let rd = &g.roads;
        let mut road_kinds = AHashMap::new();
        let mut road_widths = [rd.other_width_m; 7];
        for (name, grp, class) in [
            ("motorway", &rd.motorway, RoadClass::Motorway),
            ("primary", &rd.primary, RoadClass::Primary),
            ("secondary", &rd.secondary, RoadClass::Secondary),
            ("tertiary", &rd.tertiary, RoadClass::Tertiary),
            ("residential", &rd.residential, RoadClass::Residential),
        ] {
            road_widths[class.id() as usize] = grp.width_m;
            for h in &grp.highways {
                if h.ends_with("_link") {
                    errs.push(format!(
                        "ground.roads.{name}.highways: \"{h}\": list base values only, `_link` variants are included"
                    ));
                }
                if road_kinds.insert(h.clone(), class.id()).is_some() {
                    errs.push(format!("ground.roads.{name}.highways: \"{h}\" is already in another road class"));
                }
            }
        }
        if !errs.is_empty() {
            return Err(errs.join("; "));
        }

        // R2 walls never carry windows (a band rule, not a profile key).
        let lod_of = |b: &BuildingLod, windows: bool| LodParams { dp_m: b.dp_m, min_area_m2: b.min_area_m2, min_height_m: b.min_height_m, windows };
        let ground_of = |p: &GroundLod| GroundParams {
            area_dp_m: p.area_dp_m,
            min_area_m2: p.min_area_m2,
            road_dp_m: p.road_dp_m,
            max_road_class: p.max_road_class.id(),
            minor_paths: p.minor_paths,
            step_m: p.lift_step_m as f32,
        };
        Ok(Self {
            band_params: {
                let mut bp = BandParams {
                    edge_m: [b.max_distance_m; RING_COUNT],
                    error_m: errors,
                    tau_px: lod.tau_px,
                    max_distance_m: b.max_distance_m,
                    hysteresis: b.hysteresis,
                    look_ahead_s: b.look_ahead_s,
                    look_ahead_clamp: b.look_ahead_clamp,
                    default_tile_height_m: b.default_tile_height_m,
                    max_tiles: lod.max_tiles as f64,
                };
                bp.edge_m = bp.edges_for(&crate::rings::LodView::default(), 1.0);
                bp
            },
            building_lod: [Some(lod_of(&bw.r0, true)), Some(lod_of(&bw.r1, true)), Some(lod_of(&bw.r2, false)), None],
            ground_lod: [Some(ground_of(&g.r0)), Some(ground_of(&g.r1)), Some(ground_of(&g.r2)), Some(ground_of(&g.r3))],
            kind_profile,
            landuse_profile,
            profile_colors,
            window_width,
            overrides,
            dark_states,
            slender_ratio: bw.slender_tower.ratio as f32,
            slender_max_area_m2: bw.slender_tower.max_area_m2 as f32,
            min_window_wall_m: bw.min_window_wall_m as f32,
            warnings,
            area_kinds,
            other_area_level: g.other_area_level,
            road_kinds,
            road_widths,
            minor_paths: rd.minor_paths.iter().cloned().collect(),
            lit_values: rd.lit_values.iter().cloned().collect(),
            cluster_cell_m: l.cluster_cell_m as f32,
            world,
        })
    }

}
