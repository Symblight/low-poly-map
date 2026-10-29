//! Attributes: light profile ids and heights per building ([`resolve`]), vertex COLOR.

use crate::mvt::{Building, Landuse};
use crate::profile::Profile;

pub const PROFILE_RESIDENTIAL: u8 = 0;
pub const PROFILE_OFFICE: u8 = 1;
pub const PROFILE_RETAIL: u8 = 2;
pub const PROFILE_INDUSTRIAL: u8 = 3;
pub const PROFILE_CIVIC: u8 = 4;
pub const PROFILE_UNKNOWN: u8 = 5;
/// Castles, towers, churches: no windows, floodlit facade (docs/spec-night-windows.md).
pub const PROFILE_LANDMARK: u8 = 6;
/// Ruins, garages, chimneys: no windows, no light point.
pub const PROFILE_DARK: u8 = 7;
/// Number of profile ids (0..=7; COLOR `r` = id / 8).
pub const PROFILE_COUNT: usize = 8;

pub const FLOOR_H_DEFAULT: f32 = 3.0;
pub const MAX_HEIGHT_M: f32 = 400.0;

// Kind -> profile and the landuse fallback come from the world profile (`lights.profiles.*.kinds`,
// `lights.landuse_fallback`; `Profile::profile_for_kind` / `Profile::landuse_profile`).

/// Fallback level count when neither `height` nor `levels` is tagged.
pub fn fallback_levels(kind: &str, l: Option<Landuse>) -> f32 {
    // Small single-purpose kinds never get landuse-sized heights (a garage in a residential
    // area is not a 5-storey block).
    match kind {
        "garage" | "garages" | "shed" | "kiosk" | "carport" | "roof" | "toilets" | "hut"
        | "service" | "transformer_tower" | "greenhouse" | "cabin" => return 1.0,
        "house" | "detached" | "semidetached_house" | "bungalow" | "farm" => return 2.0,
        _ => {}
    }
    match l {
        Some(Landuse::Residential) => 5.0,
        Some(Landuse::Commercial) => 4.0,
        Some(Landuse::Industrial) => 1.0,
        _ => 2.0,
    }
}

/// Resolved per-building attributes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Resolved {
    /// Light profile id: 0 residential .. 4 civic, 6 landmark, 7 dark (never 5).
    pub profile: u8,
    /// The building's walls may carry windows: its profile has a `windows` block and it is not
    /// tagged `windows=no` (a `windows=yes` building on a windowless profile switches to a
    /// windowed one, see [`resolve`]). Per-wall rules (wall height, R2) come on top.
    pub windows: bool,
    /// Roof height above ground (m).
    pub height: f32,
    /// Bottom of the extrusion (m).
    pub min_height: f32,
    /// Floor-to-floor height (m).
    pub floor_h: f32,
    pub seed: f32,
}

/// Heights, then the light profile (docs/spec-night-windows.md section 1), first match wins:
///
/// 1. `buildings.overrides` by OSM id;
/// 2. a state tag in `buildings.dark_states` (ruins, disused, abandoned, ...) -> dark;
/// 3. the kind table (`lights.profiles.*.kinds`) for `kind`, then, if `kind` is in no
///    profile (`yes`, ...), for `historic`, then `man_made`;
/// 4. generic kinds only, with a **tagged** `height` or `levels` and a footprint the encoder
///    did not clip: `buildings.slender_tower` -> landmark;
/// 5. the landuse fallback.
///
/// Then the `windows` tag: `no` keeps the profile without windows; `yes` on a windowless
/// profile (industrial, landmark, dark) switches to the landuse fallback profile if that one has
/// windows, else to residential if it has, else changes nothing. The switch also gives the
/// building that profile's light color (COLOR `r` is the one profile the shader sees).
pub fn resolve(b: &Building, p: &Profile) -> Resolved {
    let min_h = if b.min_height.is_finite() { b.min_height.clamp(0.0, MAX_HEIGHT_M) } else { 0.0 };
    let h = b.height.filter(|h| h.is_finite() && *h > 0.0);
    let l = b.levels.filter(|l| l.is_finite() && *l > 0.0);
    let (mut height, floor_h) = match (h, l) {
        (Some(h), Some(l)) => (h, (h / l).clamp(2.5, 5.0)),
        (Some(h), None) => (h, FLOOR_H_DEFAULT),
        (None, Some(l)) => (l * FLOOR_H_DEFAULT, FLOOR_H_DEFAULT),
        (None, None) => (fallback_levels(&b.kind, b.landuse) * FLOOR_H_DEFAULT, FLOOR_H_DEFAULT),
    };
    if h.is_none() && height <= min_h {
        // building:levels on a raised part counted from its bottom.
        height += min_h;
    }
    let height = height.min(MAX_HEIGHT_M);
    let tagged_height = h.is_some() || l.is_some();
    let profile = resolve_profile(b, p, height, tagged_height);
    let (profile, windows) = match b.tags.windows {
        Some(false) => (profile, false),
        Some(true) if p.window_width_m(profile).is_none() => {
            let fallback = p.landuse_profile(b.landuse);
            if p.window_width_m(fallback).is_some() {
                (fallback, true)
            } else if p.window_width_m(PROFILE_RESIDENTIAL).is_some() {
                (PROFILE_RESIDENTIAL, true)
            } else {
                (profile, false)
            }
        }
        _ => (profile, p.window_width_m(profile).is_some()),
    };
    Resolved { profile, windows, height, min_height: min_h, floor_h, seed: b.seed01() }
}

/// Steps 1-5 of [`resolve`] (the light profile before the `windows` tag).
fn resolve_profile(b: &Building, p: &Profile, height: f32, tagged_height: bool) -> u8 {
    if let Some(id) = p.override_profile(b.osm_type, b.id) {
        return id;
    }
    if p.is_dark_state(&b.tags) {
        return PROFILE_DARK;
    }
    let by_table = p
        .profile_for_kind(&b.kind)
        .or_else(|| b.tags.historic.as_deref().and_then(|v| p.profile_for_kind(v)))
        .or_else(|| b.tags.man_made.as_deref().and_then(|v| p.profile_for_kind(v)));
    if let Some(id) = by_table {
        return id;
    }
    // Fallback heights are not evidence of a tower (a 5 x 5 m `yes` in residential landuse gets
    // 15 m), and a clipped footprint's area differs per tile and zoom.
    if tagged_height && !b.clipped && p.is_slender_tower(height, b.footprint_m2) {
        return PROFILE_LANDMARK;
    }
    p.landuse_profile(b.landuse)
}

/// Vertex COLOR per contract: r = profile id / 8 (0-4, 6, 7), g = seed, b = floor height / 10 m,
/// a = building height / 400 m.
pub fn vertex_color(r: &Resolved) -> [f32; 4] {
    [
        r.profile as f32 / 8.0,
        r.seed,
        (r.floor_h / 10.0).clamp(0.0, 1.0),
        (r.height / MAX_HEIGHT_M).clamp(0.0, 1.0),
    ]
}
