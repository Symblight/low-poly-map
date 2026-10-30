//! `seed preview`: a 2D SVG plan of one city (docs/spec-world-seed.md section 3.2, layers after
//! docs/spec-seed-city-pipeline.md section 9): the author's iteration loop without starting
//! Godot. Generator 3 layers: `footprint` (cells, city outline, airport fences), `zones` (cells
//! coloured by district type; water and airport cells grey-blue / grey), `major` (arterials,
//! radials and airport access roads), `local` (district streets) and `buildings` (footprints).
//! Without `--layers` the plan is today's view (zones and major roads, `--detail` adds local
//! streets and buildings). `--extent` crops to a square and builds only the district plans in it.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use simworld_core::seed::city::{CellKind, CityPlan, RoadKind};
use simworld_core::seed::files::DistrictType;
use simworld_core::seed::world::SeedReader;
use simworld_core::seed::SeedWorld;
use simworld_core::{NullLog, Profile, Terrain};

use super::validate::Resolver;

/// Profile overrides every seed world needs (terrain on, no flat zone).
pub fn seed_profile_json() -> serde_json::Value {
    serde_json::json!({ "terrain": { "enabled": true, "flat_radius_m": 0.0, "flat_blend_m": 0.0 } })
}

/// A loaded seed world with what the mesher needs: the profile and the terrain with the seed rules.
pub struct Loaded {
    pub profile: Profile,
    pub terrain: Terrain,
    pub world: Arc<SeedWorld>,
}

/// Load a world through `read` the way the library does, with profile overrides `ov`.
pub fn load_with(label: &str, read: &SeedReader, ov: &serde_json::Value) -> Result<Loaded, String> {
    let profile = simworld_core::profile::resolve(None, Some(ov), &NullLog)?;
    // Lake levels and runway heights read the field around the first city; any origin works (the
    // flat zone is off).
    let field = Terrain::from_profile(&profile, 54.0, 24.0);
    let world = SeedWorld::load(label, read, &profile, &field, None)?;
    let terrain = field.with_seed(world.terrain.clone());
    Ok(Loaded { profile, terrain, world })
}

/// Load the world file `manifest` (default profile with the terrain on, as every seed world
/// requires).
pub fn load_world(manifest: &Path, res_root: Option<PathBuf>) -> Result<Loaded, String> {
    let r = Resolver::new(manifest, res_root);
    let label = manifest.display().to_string();
    // `SeedWorld::load` already resolves paths relative to the manifest; only `res://` needs the
    // project root.
    let read = move |p: &str| -> Result<String, String> {
        let path = if p.starts_with("res://") || p.starts_with("user://") { r.resolve(p)? } else { PathBuf::from(p) };
        std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))
    };
    load_with(&label, &read, &seed_profile_json())
}

/// Load the world and return it with the index of `city`.
pub fn load(manifest: &Path, res_root: Option<PathBuf>, city: &str) -> Result<(Arc<SeedWorld>, usize), String> {
    let w = load_world(manifest, res_root)?.world;
    let i = w.cities().iter().position(|c| c.id == city).ok_or_else(|| format!("no city {city} in {}", manifest.display()))?;
    Ok((w, i))
}

/// Which stage outputs to draw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Layers {
    /// Cell polygons and airport fences.
    pub cells: bool,
    /// The city outline (outer edge of the kept cells).
    pub outline: bool,
    /// Cells filled by district type.
    pub zones: bool,
    pub major: bool,
    pub local: bool,
    pub buildings: bool,
}

impl Layers {
    pub const NAMES: &'static str = "footprint,major,zones,local,buildings";

    /// Today's view: zones and major roads; `detail` adds local streets and buildings.
    pub fn legacy(detail: bool) -> Self {
        Self { cells: true, outline: false, zones: true, major: true, local: detail, buildings: detail }
    }

    /// Comma list of layer names.
    pub fn parse(s: &str) -> Result<Self, String> {
        let mut l = Self::default();
        for n in s.split(',').map(str::trim).filter(|n| !n.is_empty()) {
            match n {
                "footprint" => {
                    l.cells = true;
                    l.outline = true;
                }
                "zones" => {
                    l.cells = true;
                    l.zones = true;
                }
                "major" => l.major = true,
                "local" => l.local = true,
                "buildings" => l.buildings = true,
                _ => return Err(format!("--layers: unknown layer {n} (generator 3 has {})", Self::NAMES)),
            }
        }
        if l == Self::default() {
            return Err(format!("--layers: name at least one of {}", Self::NAMES));
        }
        Ok(l)
    }
}

/// The plane box [min x, min y, max x, max y] of a square `km` wide centred on (lat, lon).
pub fn extent_box(plan: &CityPlan, lon: f64, lat: f64, km: f64) -> [f64; 4] {
    let c = plan.frame.xy(lat, lon);
    let h = km * 500.0;
    [c[0] - h, c[1] - h, c[0] + h, c[1] + h]
}

fn fill(kind: DistrictType) -> &'static str {
    match kind {
        DistrictType::OldTown => "#c8875a",
        DistrictType::Business => "#5a7fc8",
        DistrictType::Residential => "#e3d6a8",
        DistrictType::Industrial => "#a58fb5",
        DistrictType::Port => "#7aa3a8",
        DistrictType::Park => "#8cc07a",
    }
}

#[inline]
fn overlap(a: [f64; 4], b: [f64; 4]) -> bool {
    a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3]
}

/// Writes plane points as SVG pixel pairs (1 px = `m` metres, north up, box `b`).
struct Pen {
    b: [f64; 4],
    m: f64,
}

impl Pen {
    fn pt(&self, s: &mut String, p: [f64; 2]) {
        let _ = write!(s, "{:.1},{:.1}", (p[0] - self.b[0]) / self.m, (self.b[3] - p[1]) / self.m);
    }

    fn path(&self, s: &mut String, pts: impl Iterator<Item = [f64; 2]>) {
        for (i, p) in pts.enumerate() {
            if i > 0 {
                s.push(' ');
            }
            self.pt(s, p);
        }
    }
}

/// The SVG of a city plan (1 px = `m_per_px` metres, north up): `layers` of the whole plan, or
/// of the plane box `extent` only (district plans built only for the cells it touches).
pub fn svg(w: &SeedWorld, plan: &CityPlan, layers: Layers, extent: Option<[f64; 4]>, m_per_px: f64) -> String {
    let b = extent.unwrap_or_else(|| {
        let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for c in plan.cells.values() {
            b = [b[0].min(c.bbox[0]), b[1].min(c.bbox[1]), b[2].max(c.bbox[2]), b[3].max(c.bbox[3])];
        }
        for r in &plan.roads {
            b = [b[0].min(r.bbox[0]), b[1].min(r.bbox[1]), b[2].max(r.bbox[2]), b[3].max(r.bbox[3])];
        }
        b
    });
    let pen = Pen { b, m: m_per_px };
    let (wpx, hpx) = ((b[2] - b[0]) / m_per_px, (b[3] - b[1]) / m_per_px);
    let cells = || plan.cells.values().filter(|c| extent.is_none_or(|e| overlap(c.bbox, e)));
    let mut s = String::new();
    let _ = writeln!(s, r#"<svg xmlns="http://www.w3.org/2000/svg" width="{wpx:.0}" height="{hpx:.0}" viewBox="0 0 {wpx:.0} {hpx:.0}">"#);
    let _ = writeln!(s, r##"<rect width="100%" height="100%" fill="#f4f1e8"/>"##);
    if layers.outline {
        // Every cell with a wide dark stroke under the cells: only the outer edge stays visible.
        for c in cells() {
            s.push_str(r#"<polygon points=""#);
            pen.path(&mut s, c.poly.iter().copied());
            s.push_str("\" fill=\"none\" stroke=\"#333333\" stroke-width=\"4\"/>\n");
        }
    }
    if layers.cells {
        for c in cells() {
            let colour = match (c.kind, c.district, layers.zones) {
                (_, _, false) => "#f4f1e8",
                (CellKind::Water, _, _) => "#9fc0d8",
                (CellKind::Airport, _, _) => "#b0b0b0",
                (_, Some(d), _) => fill(plan.districts[d as usize].kind),
                _ => "#ffffff",
            };
            let stroke = if layers.zones { "#ffffff" } else { "#bbbbbb" };
            s.push_str(r#"<polygon points=""#);
            pen.path(&mut s, c.poly.iter().copied());
            let _ = writeln!(s, r#"" fill="{colour}" stroke="{stroke}" stroke-width="0.5"/>"#);
        }
        for g in &w.airports {
            let ring: Vec<[f64; 2]> = g.fence.ring().iter().map(|p| {
                let (lat, lon) = g.frame.ll(*p);
                plan.frame.xy(lat, lon)
            }).collect();
            if ring.iter().any(|p| p[0] >= b[0] && p[0] <= b[2] && p[1] >= b[1] && p[1] <= b[3]) {
                s.push_str(r#"<polygon points=""#);
                pen.path(&mut s, ring.iter().copied());
                let (x, y) = ((ring[0][0] - b[0]) / m_per_px, (b[3] - ring[0][1]) / m_per_px);
                let _ = writeln!(s, r##"" fill="none" stroke="#555555" stroke-dasharray="4 3"/><text x="{x:.1}" y="{y:.1}" font-size="12">{}</text>"##, g.ident);
            }
        }
    }
    if layers.local || layers.buildings {
        for c in cells().filter(|c| c.district.is_some()) {
            let cp = w.cell_plan(plan, c.ax);
            if layers.local {
                for st in &cp.streets {
                    s.push_str(r#"<polyline points=""#);
                    pen.path(&mut s, st.pts.iter().map(|p| cp.abs(*p)));
                    let (colour, width) = match (layers.zones, st.minor) {
                        (true, minor) => ("#ffffff", if minor { 0.4 } else { 0.8 }),
                        (false, minor) => ("#555555", if minor { 0.4 } else { 0.8 }),
                    };
                    let _ = writeln!(s, r#"" fill="none" stroke="{colour}" stroke-width="{width}"/>"#);
                }
            }
            if layers.buildings {
                for bl in &cp.buildings {
                    s.push_str(r#"<polygon points=""#);
                    pen.path(&mut s, bl.ring.iter().map(|p| cp.abs(*p)));
                    s.push_str("\" fill=\"#6b5d52\"/>\n");
                }
            }
        }
    }
    if layers.major {
        for r in plan.roads.iter().filter(|r| extent.is_none_or(|e| overlap(r.bbox, e))) {
            let (colour, width) = if r.kind == RoadKind::Arterial { ("#d08a2c", 1.5) } else { ("#c0392b", 2.5) };
            s.push_str(r#"<polyline points=""#);
            pen.path(&mut s, r.pts.iter().copied());
            let _ = writeln!(s, r#"" fill="none" stroke="{colour}" stroke-width="{width}"/>"#);
        }
    }
    let st = &plan.stats;
    let _ = writeln!(
        s,
        r#"<text x="8" y="18" font-size="14" font-family="sans-serif">{}: {} land cells, {} districts {:?}, {} arterial edges, {} radials, {} bridges</text>"#,
        plan.id, st.land_cells, st.districts, st.cells_by_type, st.arterial_edges, st.radials, st.bridges
    );
    s.push_str("</svg>\n");
    s
}
