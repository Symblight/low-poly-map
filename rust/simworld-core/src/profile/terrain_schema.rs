//! The `terrain` section of the profile data tree (profile_version 5).

use serde::{Deserialize, Serialize};

/// Procedural terrain (profile_version 5). Heights come from a seeded function
/// (`terrain::field_height`), no data. The `bands` table, `skirt_m`, `shading`, `horizon_*` and
/// `max_fetch` / `max_mesh` belong to the terrain-mesh layer (api 1.5, `terrain::tin`, the
/// terrain `Layer` of the scheduler).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TerrainProfile {
    /// Terrain on. Ignored (with one warning) unless the host passes `layers: ["terrain", ...]`.
    pub enabled: bool,
    /// Seed of the height function (same seed and keys = same world).
    pub seed: u32,
    /// Relief scale (m): heights are about `amplitude_m * (0.5 + s)`, `s` mostly in [-1, 1].
    pub amplitude_m: f64,
    /// Size of the largest hills (m).
    pub feature_size_m: f64,
    /// Noise octaves (detail levels, each half the size of the previous).
    pub octaves: u32,
    /// Strength of ridged mountain chains (inside a low-frequency mountain mask).
    pub ridge_share: f64,
    /// Depth of the valleys cut along meandering lines (1 = down to the valley floor).
    pub valley_share: f64,
    /// Share of the land raised into flat-topped plateaus.
    pub plateau_share: f64,
    /// Nothing is lower than this (flat sea, lakes and river plains), m.
    pub sea_level_m: f64,
    /// Radius of the flat zone around the configure origin (m; 0 with blend 0 = none).
    pub flat_radius_m: f64,
    /// Width of the smooth blend from the flat zone into the relief (m).
    pub flat_blend_m: f64,
    /// Vertical scale of every height (terrain, building bases, lights, `ground_height_at`).
    pub exaggeration: f64,
    /// Added to every height (m).
    pub height_offset_m: f64,
    /// Water vertices take the minimum height within this radius (m) and water is not subdivided;
    /// 0 (default) = water drapes like every other area (plain height, subdivided). For terrain
    /// whose lakes are flat in the height function itself (a DEM); the procedural terrain has none.
    pub water_snap_m: f64,
    /// Max edge length of draped ground geometry where the surface is not flat (m).
    pub drape_step_m: f64,
    /// Upper bound of the runtime terrain detail scale (`set_terrain_detail`, since G2 a
    /// multiplier of `tau_px`: 2 = coarser cover).
    pub detail_scale_max: f64,
    /// Screen-space error threshold (px) of the terrain layer's cover (Terrain v2 G2): a terrain
    /// tile refines when its error proxy (`terrain::height::ErrorProxy`) would show more than
    /// `tau_px` x the `set_terrain_detail` scale pixels at its slant distance in the host's view
    /// (`set_view`), and coarsens again at that / `bands.hysteresis`. The terrain bands'
    /// `edge_m` no longer refine (the last one stays the layer's outer limit; every edge still
    /// sizes the ring's look-ahead clamp).
    pub tau_px: f64,
    /// Tile budget of the terrain cover (leaves; 0 = no cap; review M1): over it the plan
    /// skips the splits with the smallest screen error (the foundation guard's never, the 2:1
    /// balance may add a few on top).
    pub max_tiles: u32,
    /// Skirt depth of terrain tiles (m; 0 = automatic: `2 * max_error_m * detail + 1`, at most 60).
    pub skirt_m: f64,
    /// Terrain mesh shading (smooth adds a NORMAL array).
    pub shading: TerrainShading,
    /// Drop terrain tiles below the horizon of a real-size Earth (roots and whole sibling groups).
    pub horizon_cull: bool,
    /// Extra distance past the horizon before a terrain tile is culled (m).
    pub horizon_margin_m: f64,
    /// Terrain dispatch limit per band (the terrain has no fetch stage, so `max_mesh` bounds it).
    pub max_fetch: u32,
    /// Terrain mesh jobs in flight per band.
    pub max_mesh: u32,
    /// Finest zoom of the terrain layer (ring 0).
    pub near_zoom: u32,
    /// Coarsest zoom of the terrain layer (the roots).
    pub far_zoom: u32,
    /// One entry per terrain zoom, `near_zoom` first (`near_zoom - far_zoom + 1` entries).
    pub bands: Vec<TerrainBand>,
}

/// One terrain band (ring i = zoom `near_zoom - i`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TerrainBand {
    /// Outer slant-distance edge (m); strictly increasing over the array. Since api 1.10 (G2)
    /// only the last one acts on the cover (the terrain layer's outer limit); every edge still
    /// sizes its ring's look-ahead clamp. The terrain cover refines by `terrain.tau_px`.
    pub edge_m: f64,
    /// Max vertical error of the terrain mesh (m): the low-poly knob.
    pub max_error_m: f64,
    /// Seed grid spacing so the visual earth curvature bends flat tiles (m; 0 = corners only, else >= 500).
    pub max_edge_m: f64,
    /// Hard triangle cap per tile.
    pub max_triangles: u32,
    /// Cells per tile side of the height patch the TIN is built from (a power of two, 16..512);
    /// 0 = by zoom (z14 64, z13 128, coarser 256: about 22 m at z12..z14). Since perf review 2
    /// the coarse bands default to 128 (z12 44 m, z11 88 m, z10 176 m at 55 N): a quarter of the
    /// height evaluations, the drawn surface still within the vector drape lift above it there.
    pub patch_cells: u32,
}

/// Terrain mesh shading.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerrainShading {
    /// Normals from screen derivatives in the shader (no NORMAL array).
    Flat,
    /// Area-weighted vertex normals (a NORMAL array).
    Smooth,
}

impl Default for TerrainBand {
    fn default() -> Self {
        Self { edge_m: 2500.0, max_error_m: 0.5, max_edge_m: 0.0, max_triangles: 4096, patch_cells: 0 }
    }
}
