//! LOD bands as a quadtree cover by 3D slant distance (design doc "Tile grid and 3D LOD").
//!
//! Pure functions, no Godot types, no allocation beyond caller-provided buffers. The tree
//! state itself (which node is split) lives in `scheduler`; this module answers "should this
//! node be split / this root exist" for one frame.
//!
//! Distances are measured in Web Mercator world units scaled to meters at the view point's
//! latitude (one `cos(lat)` per frame, no trig per tile). The scale error grows with the
//! north-south offset: ~0.6 % at 25 km at Vilnius latitude, irrelevant for band selection.
//!
//! Bands (ring = 16 - zoom). Since api 1.10 (Tiles and LOD v2 step T3a) the edges of R0..R2
//! come from the screen-space error: a tile of ring r > 0 refines into its 4 children when
//! `SSE = e_r * H / (2 d tan(fov_y / 2)) > tau`, i.e. when its slant distance is below the
//! refine distance [`sse_refine_distance`]`(e_r, view, tau)` (`e_r` = profile `lod.error_m.r{r}`,
//! `H` / `fov_y` from the host's `set_view`, `tau` = `lod.tau_px` x the quality scale). At the
//! default view (1080 px, 60 deg, tau 4) the defaults give the old 0.6 / 2.5 / 7 km edges. R3's
//! edge is the outer limit `bands.max_distance_m` (25 km). Distances are measured from the
//! finer band's look-ahead point. Hysteresis (default 15 %): a split tile merges again only once
//! its distance exceeds the edge by that much (SSE < tau / 1.15); a z13 root is added below
//! 25 km and dropped beyond 28.75 km. The tuning values come in as [`BandParams`]
//! (`profile::Profile::bands`); only the structural constants (ring count, zooms) live here.

use crate::geo::{lat_to_tile_y, lon_to_tile_x, TileKey, MERCATOR_CIRCUMFERENCE};

pub use crate::cover_budget::estimate_cover_tiles;

/// Structural (tileset contract, not tuning): 4 bands over zooms 13..16.
pub const RING_COUNT: usize = 4;
/// Coarsest zoom (R3); tiles of this zoom are the roots of the cover.
pub const ROOT_ZOOM: u8 = 13;
/// Finest zoom (R0).
pub const LEAF_ZOOM: u8 = 16;
/// Upper bound for any building (m; the vertex COLOR `a` encoding range), used only to bound
/// the root candidate search (on top of the highest terrain when the terrain is active).
pub const MAX_BOX_HEIGHT_M: f64 = 400.0;
/// Most rings any layer can have (the terrain layer spans zooms 8..16 at most).
pub const MAX_RINGS: usize = 9;
/// Earth radius of the horizon cull (m; real-size Earth, `docs/spec-terrain.md` 5.1).
pub const HORIZON_EARTH_RADIUS_M: f64 = 6_371_000.0;

/// Which quadtree a tile belongs to (contract api 1.5: `layer` of a ready tile).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LayerKind {
    /// The vector world (buildings, ground, lights): zooms 13..16, keys `"z/x/y"`.
    World,
    /// The terrain mesh: zooms `terrain.far_zoom..=terrain.near_zoom`, keys `"t/z/x/y"`.
    Terrain,
    /// Landmarks (api 1.11, Tiles and LOD v2 step T2b): one tile per overview data tile, keys
    /// `"l/z/x/y"` at the overview zoom (root = leaf zoom), additive refinement (never faded).
    Landmarks,
}

impl LayerKind {
    /// Contract name (`layer` key of a ready tile / `debug_tiles` entry).
    pub fn name(self) -> &'static str {
        match self {
            LayerKind::World => "world",
            LayerKind::Terrain => "terrain",
            LayerKind::Landmarks => "landmarks",
        }
    }
    /// Key prefix (`"t/"` for terrain, none for the vector world).
    pub fn prefix(self) -> &'static str {
        match self {
            LayerKind::World => "",
            LayerKind::Terrain => "t/",
            LayerKind::Landmarks => "l/",
        }
    }
}

/// A tile key with its layer: `"z/x/y"` (world) or `"t/z/x/y"` (terrain). The two layers never
/// share a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LayerKey {
    pub layer: LayerKind,
    pub key: TileKey,
}

impl LayerKey {
    pub const fn world(key: TileKey) -> Self {
        Self { layer: LayerKind::World, key }
    }
    pub const fn terrain(key: TileKey) -> Self {
        Self { layer: LayerKind::Terrain, key }
    }
    pub const fn landmarks(key: TileKey) -> Self {
        Self { layer: LayerKind::Landmarks, key }
    }
    /// Parse `"z/x/y"`, `"t/z/x/y"` or `"l/z/x/y"`.
    pub fn parse(s: &str) -> Option<Self> {
        if let Some(r) = s.strip_prefix("t/") {
            return TileKey::parse(r).map(Self::terrain);
        }
        if let Some(r) = s.strip_prefix("l/") {
            return TileKey::parse(r).map(Self::landmarks);
        }
        TileKey::parse(s).map(Self::world)
    }
}

impl std::fmt::Display for LayerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{}", self.layer.prefix(), self.key)
    }
}

/// One quadtree layer of the cover (`docs/spec-terrain.md` 5.1): zoom range, band edges (ring =
/// `leaf_zoom - z`), the shared hysteresis / look-ahead tuning, in-flight limits and the
/// optional horizon cull. The vector layer is `{13, 16, bands.*}` exactly as before; the terrain
/// layer is `{terrain.far_zoom, terrain.near_zoom, terrain.bands[*].edge_m}`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layer {
    pub kind: LayerKind,
    /// Coarsest zoom (the roots; ring `rings() - 1`).
    pub root_zoom: u8,
    /// Finest zoom (ring 0).
    pub leaf_zoom: u8,
    /// Outer slant-distance edge per ring (m); entries past `rings()` are unused.
    pub edge_m: [f64; MAX_RINGS],
    pub hysteresis: f64,
    pub look_ahead_s: f64,
    pub look_ahead_clamp: f64,
    /// Box height (m) above the terrain of a tile whose own height is still unknown (buildings;
    /// 0 for the terrain layer, whose box is the terrain range alone).
    pub default_tile_height_m: f64,
    /// Jobs in flight per ring: fetches and mesh jobs.
    pub max_fetch: u32,
    pub max_mesh: u32,
    /// Horizon cull margin (m); `None` = no cull (the vector layer, or `terrain.horizon_cull`
    /// off).
    pub horizon_margin_m: Option<f64>,
}

impl Layer {
    /// The vector layer at the default view ([`LodView::default`], quality 1): zooms 13..16,
    /// the profile's `bands.*` / `lod.*` and `streaming.*` limits.
    pub fn world(b: &BandParams, max_fetch: u32, max_mesh: u32) -> Self {
        Self::world_view(b, &LodView::default(), 1.0, max_fetch, max_mesh)
    }

    /// The vector layer for a view and quality scale: the edges of R0..R2 are the SSE refine
    /// distances ([`BandParams::edges_for`]), R3's is `bands.max_distance_m`.
    pub fn world_view(b: &BandParams, view: &LodView, quality: f64, max_fetch: u32, max_mesh: u32) -> Self {
        let mut edge_m = [0.0; MAX_RINGS];
        edge_m[..RING_COUNT].copy_from_slice(&b.edges_for(view, quality));
        Self {
            kind: LayerKind::World,
            root_zoom: ROOT_ZOOM,
            leaf_zoom: LEAF_ZOOM,
            edge_m,
            hysteresis: b.hysteresis,
            look_ahead_s: b.look_ahead_s,
            look_ahead_clamp: b.look_ahead_clamp,
            default_tile_height_m: b.default_tile_height_m,
            max_fetch,
            max_mesh,
            horizon_margin_m: None,
        }
    }

    /// The terrain layer of a (validated) profile: zooms `far_zoom..=near_zoom`, edges from
    /// `terrain.bands`, hysteresis and look-ahead from `bands.*`.
    pub fn terrain(t: &crate::profile::TerrainProfile, b: &BandParams) -> Self {
        let mut edge_m = [0.0; MAX_RINGS];
        for (i, band) in t.bands.iter().take(MAX_RINGS).enumerate() {
            edge_m[i] = band.edge_m;
        }
        Self {
            kind: LayerKind::Terrain,
            root_zoom: t.far_zoom as u8,
            leaf_zoom: t.near_zoom as u8,
            edge_m,
            hysteresis: b.hysteresis,
            look_ahead_s: b.look_ahead_s,
            look_ahead_clamp: b.look_ahead_clamp,
            default_tile_height_m: 0.0,
            max_fetch: t.max_fetch,
            max_mesh: t.max_mesh,
            horizon_margin_m: t.horizon_cull.then_some(t.horizon_margin_m),
        }
    }

    /// Number of rings (zoom levels).
    pub fn rings(&self) -> usize {
        (self.leaf_zoom - self.root_zoom) as usize + 1
    }

    /// Outer edge of the coarsest ring (roots exist below it).
    pub fn reach_m(&self) -> f64 {
        self.edge_m[self.rings() - 1]
    }

    pub fn ring_for_zoom(&self, z: u8) -> Option<usize> {
        (self.root_zoom..=self.leaf_zoom).contains(&z).then(|| (self.leaf_zoom - z) as usize)
    }

    pub fn zoom_for_ring(&self, ring: usize) -> u8 {
        self.leaf_zoom - ring as u8
    }

    /// Parent inside this layer (`None` at the root zoom).
    pub fn parent(&self, k: TileKey) -> Option<TileKey> {
        (k.z > self.root_zoom).then(|| TileKey::new(k.z - 1, k.x / 2, k.y / 2))
    }

    /// The tile's sibling group (its parent's children), including itself.
    pub fn siblings(&self, k: TileKey) -> [TileKey; 4] {
        match self.parent(k) {
            Some(p) => children(p),
            None => [k; 4],
        }
    }

    /// The look-ahead clamp of a ring (default: half the ring's radius).
    pub fn look_ahead_clamp_m(&self, ring: usize) -> f64 {
        self.edge_m[ring] * self.look_ahead_clamp
    }

    /// Band thresholds with hysteresis (as [`BandParams::inside`]).
    pub fn inside(&self, d: f64, edge: f64, current: bool) -> bool {
        if current {
            d <= edge * self.hysteresis
        } else {
            d < edge
        }
    }
}

/// Horizontal distance (m) past which a tile box `[lo, hi]` (m above MSL) is below the horizon
/// of a camera at `cam_alt` (MSL) over ground `cam_ground` on a real-size Earth:
/// `sqrt(2R max(cam_alt - ref, 0)) + sqrt(2R max(hi - ref, 0)) + margin`, `ref` = the lower of
/// the ground under the camera and the tile's minimum.
pub fn horizon_line_m(cam_alt: f64, cam_ground: f64, lo: f64, hi: f64, margin_m: f64) -> f64 {
    let r2 = 2.0 * HORIZON_EARTH_RADIUS_M;
    let h_ref = cam_ground.min(lo);
    (r2 * (cam_alt - h_ref).max(0.0)).sqrt() + (r2 * (hi - h_ref).max(0.0)).sqrt() + margin_m
}

/// Horizon test with the band hysteresis: a tile above the horizon now (`current`) drops out
/// only `hysteresis` past the line, a culled one comes back at the line.
pub fn above_horizon(horizontal_m: f64, line_m: f64, hysteresis: f64, current: bool) -> bool {
    if current {
        horizontal_m <= line_m * hysteresis
    } else {
        horizontal_m < line_m
    }
}

/// The host's view for the screen-space error (contract `set_view`, api 1.10): vertical field of
/// view, viewport height in physical pixels and the camera's forward direction (local axes,
/// unit length; stored for the rear-hemisphere relaxation of step T4, unused by the cover yet).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LodView {
    pub fov_y_deg: f64,
    pub viewport_h_px: f64,
    pub forward: [f64; 3],
}

impl Default for LodView {
    /// 60 deg, 1080 px, looking north (`-Z`): the view the default `lod.error_m` were sized for.
    fn default() -> Self {
        Self { fov_y_deg: 60.0, viewport_h_px: 1080.0, forward: [0.0, 0.0, -1.0] }
    }
}

impl LodView {
    /// Smallest / largest accepted vertical field of view (deg).
    pub const FOV_MIN_DEG: f64 = 1.0;
    pub const FOV_MAX_DEG: f64 = 179.0;
    /// Largest accepted viewport height (px).
    pub const MAX_VIEWPORT_PX: f64 = 65_536.0;

    /// A validated view: `fov_y_deg` in [1, 179], `viewport_h_px` in [1, 65536], `forward`
    /// finite and not zero (normalized here). `None` for anything else.
    pub fn new(fov_y_deg: f64, viewport_h_px: f64, forward: [f64; 3]) -> Option<Self> {
        let fov_ok = fov_y_deg.is_finite() && (Self::FOV_MIN_DEG..=Self::FOV_MAX_DEG).contains(&fov_y_deg);
        let h_ok = viewport_h_px.is_finite() && (1.0..=Self::MAX_VIEWPORT_PX).contains(&viewport_h_px);
        let len = (forward[0] * forward[0] + forward[1] * forward[1] + forward[2] * forward[2]).sqrt();
        let f_ok = forward.iter().all(|v| v.is_finite()) && len.is_finite() && len > 1e-9;
        (fov_ok && h_ok && f_ok).then(|| Self { fov_y_deg, viewport_h_px, forward: forward.map(|v| v / len) })
    }

    /// `2 tan(fov_y / 2)`: the view's height at 1 m distance (m).
    pub fn view_height_at_1m(&self) -> f64 {
        2.0 * crate::math::tan(self.fov_y_deg.to_radians() * 0.5)
    }
}

/// Slant distance (m) below which a tile with geometric error `error_m` (m lost when it is not
/// refined) shows more than `tau` pixels of error in `view`: the solution of
/// `SSE = error_m * H / (2 d tan(fov_y / 2)) = tau` for `d`. Shared by every layer that refines
/// by screen-space error (the vector layer since api 1.10; the terrain layer later, step G2).
///
/// Evaluated as `error_m * (H / (2 tan(fov_y / 2) * tau))`: with this order the default
/// `lod.error_m` give exactly 600.0 / 2500.0 / 7000.0 m at the default view (the old edges, bit
/// for bit), so the default cover is the 1.7 cover even for boxes at an exact edge distance.
pub fn sse_refine_distance(error_m: f64, view: &LodView, tau: f64) -> f64 {
    error_m * (view.viewport_h_px / (view.view_height_at_1m() * tau))
}

/// Smallest refine distance the SSE edges are clamped to (m).
pub const MIN_REFINE_DISTANCE_M: f64 = 1.0;

/// Band tuning from the world profile (`bands.*`, `lod.*`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandParams {
    /// Outer slant-distance edge of each band (m), indexed by ring, at the default view
    /// ([`LodView::default`], quality 1): R0..R2 from `lod.error_m`, R3 = `max_distance_m`. The
    /// live cover uses [`BandParams::edges_for`] with the host's view instead.
    pub edge_m: [f64; RING_COUNT],
    /// Geometric error (m) of the R1, R2, R3 chunks against the next finer level
    /// (`lod.error_m.r1..r3`): index i = the error behind edge i.
    pub error_m: [f64; RING_COUNT - 1],
    /// SSE threshold (px) at quality 1 (`lod.tau_px`).
    pub tau_px: f64,
    /// Outer limit of the whole cover (m, `bands.max_distance_m`): R3's edge.
    pub max_distance_m: f64,
    /// A tile changes band only once its slant distance is this factor past the band edge.
    pub hysteresis: f64,
    /// Look-ahead horizon (s): center = camera + velocity * this (3D), clamped.
    pub look_ahead_s: f64,
    /// Look-ahead offset clamp as a fraction of the band's edge.
    pub look_ahead_clamp: f64,
    /// Box height (m) of a tile whose own and ancestors' heights are still unknown.
    pub default_tile_height_m: f64,
    /// Tile budget of the cover (`lod.max_tiles`; 0 = no cap): when the estimated cover of a
    /// view and quality ([`estimate_cover_tiles`]) exceeds it, tau rises until it fits
    /// ([`BandParams::capped_quality`]).
    pub max_tiles: f64,
}

impl BandParams {
    /// The band edges (m) for `view` and the quality scale `quality` (tau_eff = `tau_px` x
    /// `quality`): R0..R2 = [`sse_refine_distance`] of `error_m`, clamped to
    /// [[`MIN_REFINE_DISTANCE_M`], `max_distance_m`]; R3 = `max_distance_m`.
    pub fn edges_for(&self, view: &LodView, quality: f64) -> [f64; RING_COUNT] {
        let tau = self.tau_px * quality;
        let mut e = [self.max_distance_m; RING_COUNT];
        for (i, err) in self.error_m.iter().enumerate() {
            e[i] = sse_refine_distance(*err, view, tau).clamp(MIN_REFINE_DISTANCE_M, self.max_distance_m);
        }
        e
    }

    /// The look-ahead clamp of a band (default: half the band's radius).
    pub fn look_ahead_clamp_m(&self, ring: usize) -> f64 {
        self.edge_m[ring] * self.look_ahead_clamp
    }

    /// Band thresholds with hysteresis: `current` = the state the tile is in now.
    /// Returns whether the tile should be "in" (split / exist) given distance `d` to `edge`.
    pub fn inside(&self, d: f64, edge: f64, current: bool) -> bool {
        if current {
            d <= edge * self.hysteresis
        } else {
            d < edge
        }
    }
}

/// Ring index for a zoom level.
pub fn ring_for_zoom(z: u8) -> Option<usize> {
    (ROOT_ZOOM..=LEAF_ZOOM).contains(&z).then(|| (LEAF_ZOOM - z) as usize)
}

pub fn zoom_for_ring(ring: usize) -> u8 {
    LEAF_ZOOM - ring as u8
}

/// The 4 children of a tile in contract order NW, NE, SW, SE.
pub fn children(k: TileKey) -> [TileKey; 4] {
    let (z, x, y) = (k.z + 1, 2 * k.x, 2 * k.y);
    [TileKey::new(z, x, y), TileKey::new(z, x + 1, y), TileKey::new(z, x, y + 1), TileKey::new(z, x + 1, y + 1)]
}

pub fn parent(k: TileKey) -> Option<TileKey> {
    (k.z > ROOT_ZOOM).then(|| TileKey::new(k.z - 1, k.x / 2, k.y / 2))
}

/// The tile's sibling group (its parent's children), including itself.
pub fn siblings(k: TileKey) -> [TileKey; 4] {
    match parent(k) {
        Some(p) => children(p),
        None => [k; 4],
    }
}

/// 3D look-ahead (ENU e, n, altitude): camera + velocity * `horizon_s` (default 20 s), offset
/// clamped to `max_offset_m` (3D length), altitude clamped to the ground.
pub fn look_ahead(cam: [f64; 3], vel: [f64; 3], max_offset_m: f64, horizon_s: f64) -> [f64; 3] {
    let mut d = vel.map(|v| if v.is_finite() { v * horizon_s } else { 0.0 });
    let len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    if len > max_offset_m && len > 0.0 {
        let s = max_offset_m / len;
        d = d.map(|v| v * s);
    }
    [cam[0] + d[0], cam[1] + d[1], (cam[2] + d[2]).max(0.0)]
}

/// A view point (one band's look-ahead) in Mercator world units [0, 1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewPoint {
    pub wx: f64,
    pub wy: f64,
    /// Height (m above MSL): AGL + terrain under the view point (equal to AGL with the terrain
    /// off).
    pub alt: f64,
    /// Meters per world unit at the view point's latitude.
    pub m_per_unit: f64,
}

impl ViewPoint {
    pub fn new(lat: f64, lon: f64, alt: f64) -> Self {
        Self {
            wx: lon_to_tile_x(lon, 0),
            wy: lat_to_tile_y(lat, 0),
            alt,
            m_per_unit: MERCATOR_CIRCUMFERENCE * crate::math::cos(lat.to_radians()),
        }
    }

    /// Horizontal distance (m) from the view point to the tile's footprint (0 inside).
    pub fn horizontal_m(&self, k: TileKey) -> f64 {
        let s = 1.0 / (1u64 << k.z) as f64;
        let (x0, y0) = (k.x as f64 * s, k.y as f64 * s);
        let dx = (x0 - self.wx).max(self.wx - (x0 + s)).max(0.0);
        let dy = (y0 - self.wy).max(self.wy - (y0 + s)).max(0.0);
        crate::math::hypot(dx, dy) * self.m_per_unit
    }

    /// Slant distance (m) to the nearest point of the tile box footprint x [0, box_h] (flat
    /// terrain). Same as `slant_box_m(k, 0.0, box_h)`.
    pub fn slant_m(&self, k: TileKey, box_h: f64) -> f64 {
        self.slant_box_m(k, 0.0, 0.0 + box_h)
    }

    /// Slant distance (m) to the nearest point of the tile box footprint x [lo, hi] (heights in
    /// meters above MSL, the scale of `alt`; see `Scheduler::views`). The scheduler passes
    /// `lo` = the tile's terrain minimum and `hi` = its terrain maximum + building height, both
    /// from `Terrain::tile_range` (deterministic per key; `[0, 0]` with the terrain off).
    pub fn slant_box_m(&self, k: TileKey, lo: f64, hi: f64) -> f64 {
        let v = (self.alt - hi).max(lo - self.alt).max(0.0);
        crate::math::hypot(self.horizontal_m(k), v)
    }

    /// Append every z13 tile that could be a root (slant < R3 edge * hysteresis) to `out`, with
    /// box tops at most [`MAX_BOX_HEIGHT_M`] (flat terrain).
    pub fn root_candidates(&self, b: &BandParams, out: &mut Vec<TileKey>) {
        self.root_candidates_below(b, MAX_BOX_HEIGHT_M, out)
    }

    /// [`ViewPoint::root_candidates`] for boxes whose tops are at most `max_top_m` above MSL
    /// (highest terrain + [`MAX_BOX_HEIGHT_M`] with the terrain active).
    pub fn root_candidates_below(&self, b: &BandParams, max_top_m: f64, out: &mut Vec<TileKey>) {
        self.root_candidates_at(b.edge_m[RING_COUNT - 1] * b.hysteresis, ROOT_ZOOM, max_top_m, out)
    }

    /// Root candidates of a layer: every `root_zoom` tile whose box (top at most `max_top_m`)
    /// can be closer than the layer's reach times the hysteresis.
    pub fn root_candidates_layer(&self, l: &Layer, max_top_m: f64, out: &mut Vec<TileKey>) {
        self.root_candidates_at(l.reach_m() * l.hysteresis, l.root_zoom, max_top_m, out)
    }

    pub(crate) fn root_candidates_at(&self, reach: f64, root_zoom: u8, max_top_m: f64, out: &mut Vec<TileKey>) {
        let v = (self.alt - max_top_m).max(0.0);
        if v >= reach {
            return;
        }
        let r_units = (reach * reach - v * v).sqrt() / self.m_per_unit;
        let n = 1i64 << root_zoom;
        let to_tile = |w: f64| (w * n as f64).floor() as i64;
        let (x0, x1) = (to_tile(self.wx - r_units).max(0), to_tile(self.wx + r_units).min(n - 1));
        let (y0, y1) = (to_tile(self.wy - r_units).max(0), to_tile(self.wy + r_units).min(n - 1));
        for y in y0..=y1 {
            for x in x0..=x1 {
                out.push(TileKey::new(root_zoom, x as u32, y as u32));
            }
        }
    }
}

/// Whether a z13 root should exist (`existing` = it does now).
pub fn root_wanted(b: &BandParams, view: &ViewPoint, k: TileKey, box_h: f64, existing: bool) -> bool {
    b.inside(view.slant_m(k, box_h), b.edge_m[RING_COUNT - 1], existing)
}

/// Whether a tile of ring `ring` > 0 should be split into its children (`split` = it is now).
/// `finer_view` is the look-ahead point of band `ring - 1`.
pub fn split_wanted(b: &BandParams, finer_view: &ViewPoint, k: TileKey, ring: usize, box_h: f64, split: bool) -> bool {
    ring > 0 && b.inside(finer_view.slant_m(k, box_h), b.edge_m[ring - 1], split)
}

/// Priority ordering key: finer band first, then slant distance to the band's look-ahead
/// point, then key (total order, deterministic).
pub fn priority_cmp(a: (usize, f64, TileKey), b: (usize, f64, TileKey)) -> std::cmp::Ordering {
    a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.cmp(&b.2))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::{tile_for, Frame};

    const VILNIUS: (f64, f64) = (54.6872, 25.2797);

    fn bands() -> BandParams {
        *crate::profile::Profile::default().bands()
    }

    #[test]
    fn ring_zoom_mapping_and_family() {
        assert_eq!(ring_for_zoom(16), Some(0));
        assert_eq!(ring_for_zoom(13), Some(3));
        assert_eq!(ring_for_zoom(12), None);
        assert_eq!(ring_for_zoom(17), None);
        assert_eq!(zoom_for_ring(2), 14);
        let k = TileKey::new(14, 9342, 5207);
        let c = children(k);
        assert_eq!(c[0], TileKey::new(15, 18684, 10414));
        assert_eq!(c[3], TileKey::new(15, 18685, 10415));
        for ch in c {
            assert_eq!(parent(ch), Some(k));
            assert_eq!(siblings(ch), c);
        }
        assert_eq!(parent(TileKey::new(13, 1, 1)), None);
    }

    #[test]
    fn slant_distance_matches_enu_geometry() {
        let f = Frame::new(VILNIUS.0, VILNIUS.1);
        let v = ViewPoint::new(VILNIUS.0, VILNIUS.1, 300.0);
        // Containing tile: horizontal 0; slant = altitude above the box top.
        let k = tile_for(VILNIUS.0, VILNIUS.1, 16);
        assert_eq!(v.horizontal_m(k), 0.0);
        assert!((v.slant_m(k, 60.0) - 240.0).abs() < 1e-9);
        assert!((v.slant_m(k, 500.0)).abs() < 1e-9, "camera inside the box height");
        // Far tiles: compare against the exact ENU rectangle distance (< 1 % error).
        for (dx, dy) in [(5i64, 0i64), (0, 7), (-6, -3), (2, 4)] {
            let k13 = tile_for(VILNIUS.0, VILNIUS.1, 13);
            let t = TileKey::new(13, (k13.x as i64 + dx) as u32, (k13.y as i64 + dy) as u32);
            let r = f.tile_rect(t);
            let ex = r.min_e.max(-r.max_e).max(0.0);
            let ey = r.min_n.max(-r.max_n).max(0.0);
            let exact = ex.hypot(ey);
            let approx = v.horizontal_m(t);
            assert!((approx - exact).abs() / exact < 0.01, "{t}: {approx} vs {exact}");
        }
    }

    #[test]
    fn look_ahead_includes_vertical_and_clamps_3d() {
        // 250 m/s east * 20 s = 5 km, clamped to 300 m (half of R0's 600 m).
        let b = bands();
        let p = look_ahead([10.0, 20.0, 300.0], [250.0, 0.0, 0.0], b.look_ahead_clamp_m(0), b.look_ahead_s);
        assert!((p[0] - 310.0).abs() < 1e-9 && (p[1] - 20.0).abs() < 1e-9 && p[2] == 300.0);
        // Climbing 10 m/s: 200 m up, not clamped for R1.
        let p = look_ahead([0.0, 0.0, 300.0], [0.0, 0.0, 10.0], b.look_ahead_clamp_m(1), b.look_ahead_s);
        assert!((p[2] - 500.0).abs() < 1e-9);
        // Diagonal: the 3D length is clamped, direction kept.
        let p = look_ahead([0.0, 0.0, 1000.0], [30.0, 0.0, -40.0], 250.0, b.look_ahead_s);
        assert!((p[0] - 150.0).abs() < 1e-9 && (p[2] - 800.0).abs() < 1e-9);
        // Diving below ground clamps to 0.
        let p = look_ahead([0.0, 0.0, 50.0], [0.0, 0.0, -100.0], 1000.0, b.look_ahead_s);
        assert_eq!(p[2], 0.0);
    }

    #[test]
    fn hysteresis_band() {
        let b = bands();
        assert!(b.inside(599.0, 600.0, false));
        assert!(!b.inside(600.0, 600.0, false));
        assert!(b.inside(680.0, 600.0, true));
        assert!(!b.inside(691.0, 600.0, true));
    }

    #[test]
    fn split_rules_by_altitude_directly_below() {
        let c13 = tile_for(VILNIUS.0, VILNIUS.1, 13);
        let c14 = tile_for(VILNIUS.0, VILNIUS.1, 14);
        let c15 = tile_for(VILNIUS.0, VILNIUS.1, 15);
        let at = |alt| ViewPoint::new(VILNIUS.0, VILNIUS.1, alt);
        let b = bands();
        let h = b.default_tile_height_m;
        // 300 m: everything below splits down to z16.
        assert!(split_wanted(&b, &at(300.0), c15, 1, h, false));
        // 1000 m: no R0 (slant >= 940 m), R1 yes.
        assert!(!split_wanted(&b, &at(1000.0), c15, 1, h, false));
        assert!(split_wanted(&b, &at(1000.0), c14, 2, h, false));
        // 3000 m: R2 below, no R1.
        assert!(!split_wanted(&b, &at(3000.0), c14, 2, h, false));
        assert!(split_wanted(&b, &at(3000.0), c13, 3, h, false));
        // 8000 m: lights only.
        assert!(!split_wanted(&b, &at(8000.0), c13, 3, h, false));
        // R0 never splits.
        assert!(!split_wanted(&b, &at(0.0), tile_for(VILNIUS.0, VILNIUS.1, 16), 0, h, true));
        // Roots: 20 km up still in, 26 km out.
        assert!(root_wanted(&b, &at(20_000.0), c13, h, false));
        assert!(!root_wanted(&b, &at(26_000.0), c13, h, false));
        assert!(root_wanted(&b, &at(26_000.0), c13, h, true));
    }

    #[test]
    fn root_candidates_cover_the_slant_disk() {
        for alt in [300.0, 7000.0, 20_000.0] {
            let v = ViewPoint::new(VILNIUS.0, VILNIUS.1, alt);
            let mut c = Vec::new();
            v.root_candidates(&bands(), &mut c);
            let set: std::collections::HashSet<_> = c.iter().copied().collect();
            // Every z13 tile within reach whose slant distance is <= 28.75 km is a candidate.
            let ct = tile_for(VILNIUS.0, VILNIUS.1, 13);
            for dy in -12i64..=12 {
                for dx in -12i64..=12 {
                    let k = TileKey::new(13, (ct.x as i64 + dx) as u32, (ct.y as i64 + dy) as u32);
                    if v.slant_m(k, 0.0) <= bands().edge_m[3] * bands().hysteresis {
                        assert!(set.contains(&k), "alt {alt}: {k} missing");
                    }
                }
            }
        }
        let mut c = Vec::new();
        ViewPoint::new(VILNIUS.0, VILNIUS.1, 30_000.0).root_candidates(&bands(), &mut c);
        assert!(c.is_empty());
    }

    /// api 1.10: the SSE refine distances. The default view and profile give the old fixed
    /// edges; fov 30 deg pushes them out by tan 30 / tan 15, H 540 and quality 2 halve them; the
    /// outer limit stays and clamps.
    #[test]
    fn sse_refine_distances_follow_view_and_tau() {
        let b = bands();
        let def = LodView::default();
        let e = b.edges_for(&def, 1.0);
        assert_eq!(e, [600.0, 2500.0, 7000.0, 25_000.0], "bit for bit the old edges");
        assert_eq!(e, b.edge_m, "BandParams::edge_m = the default view");
        // The formula itself: SSE at the refine distance is exactly tau.
        let d = sse_refine_distance(10.0, &def, 4.0);
        let sse = 10.0 * 1080.0 / (2.0 * d * (30f64).to_radians().tan());
        assert!((sse - 4.0).abs() < 1e-9);
        let narrow = LodView::new(30.0, 1080.0, [0.0, 0.0, -1.0]).unwrap();
        let k = (30f64).to_radians().tan() / (15f64).to_radians().tan();
        let en = b.edges_for(&narrow, 1.0);
        for r in 0..3 {
            let ratio = en[r] / e[r];
            assert!((ratio - k).abs() / k < 0.01, "ring {r}: {ratio} vs {k}");
        }
        assert_eq!(en[3], 25_000.0, "outer limit unchanged");
        let small = LodView::new(60.0, 540.0, [0.0, 0.0, -1.0]).unwrap();
        let q2 = b.edges_for(&def, 2.0);
        for (r, eh) in b.edges_for(&small, 1.0).iter().enumerate().take(3) {
            assert!((eh / e[r] - 0.5).abs() < 1e-12, "H 540 halves ring {r}");
            assert!((q2[r] / e[r] - 0.5).abs() < 1e-12, "quality 2 halves ring {r}");
        }
        // Clamped to the outer limit (a 5 deg zoom would put R2's edge at ~85 km).
        let zoom = LodView::new(5.0, 1080.0, [1.0, 0.0, 0.0]).unwrap();
        assert_eq!(b.edges_for(&zoom, 1.0)[2], 25_000.0);
        // Validation and normalization.
        assert!(LodView::new(0.5, 1080.0, [0.0, 0.0, -1.0]).is_none());
        assert!(LodView::new(180.0, 1080.0, [0.0, 0.0, -1.0]).is_none());
        assert!(LodView::new(f64::NAN, 1080.0, [0.0, 0.0, -1.0]).is_none());
        assert!(LodView::new(60.0, 0.0, [0.0, 0.0, -1.0]).is_none());
        assert!(LodView::new(60.0, 1080.0, [0.0, 0.0, 0.0]).is_none());
        assert!(LodView::new(60.0, 1080.0, [f64::INFINITY, 0.0, 0.0]).is_none());
        assert_eq!(LodView::new(60.0, 1080.0, [0.0, 3.0, 4.0]).unwrap().forward, [0.0, 0.6, 0.8]);
    }

    #[test]
    fn priority_finer_band_first_then_distance() {
        let a = (0usize, 900.0, TileKey::new(16, 1, 1));
        let b = (1usize, 10.0, TileKey::new(15, 1, 1));
        let c = (0usize, 100.0, TileKey::new(16, 2, 2));
        let mut v = vec![a, b, c];
        v.sort_by(|x, y| priority_cmp(*x, *y));
        assert_eq!(v, vec![c, a, b]);
    }
}
