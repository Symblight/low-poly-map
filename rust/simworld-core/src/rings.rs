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

use crate::geo::TileKey;

pub use crate::cover_budget::estimate_cover_tiles;

mod view;
#[cfg(test)]
mod tests;

pub use view::ViewPoint;

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
