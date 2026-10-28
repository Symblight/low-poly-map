//! `seed validate`: load a seed world exactly like the library does (`simworld_core::seed::files::
//! load_world`), then report (docs/spec-world-seed.md sections 3.2, 4.6): errors (exit 1),
//! warnings, and a summary of cities, airports, water (with the measured water index) and roads.

use std::path::{Path, PathBuf};
use std::time::Instant;

use simworld_core::seed::files::{load_world, SeedFiles};
use simworld_core::seed::hash::world_root;
use simworld_core::seed::water::{WaterIndex, WaterParams};

/// Path resolution for the CLI: `res://` -> the Godot project root (the first folder above the
/// manifest holding `project.godot`, or `--res-root`), relative -> the manifest's folder.
pub struct Resolver {
    pub manifest_dir: PathBuf,
    pub res_root: Option<PathBuf>,
}

impl Resolver {
    pub fn new(manifest: &Path, res_root: Option<PathBuf>) -> Self {
        let manifest_dir = manifest.parent().map(Path::to_path_buf).unwrap_or_default();
        let res_root = res_root.or_else(|| {
            let mut d = std::fs::canonicalize(&manifest_dir).ok()?;
            loop {
                if d.join("project.godot").is_file() {
                    return Some(d);
                }
                if !d.pop() {
                    return None;
                }
            }
        });
        Self { manifest_dir, res_root }
    }

    pub fn resolve(&self, p: &str) -> Result<PathBuf, String> {
        if let Some(rest) = p.strip_prefix("res://") {
            let root = self.res_root.as_ref().ok_or("res:// path but no project.godot above the manifest (use --res-root)")?;
            return Ok(root.join(rest));
        }
        if p.starts_with("user://") {
            return Err("user:// paths are resolved by the game, not by the CLI".into());
        }
        let path = Path::new(p);
        Ok(if path.is_absolute() { path.to_path_buf() } else { self.manifest_dir.join(path) })
    }
}

/// Run the validation; returns the report text and success.
pub fn run(manifest: &Path, res_root: Option<PathBuf>, params: WaterParams) -> (String, bool) {
    let text = match std::fs::read_to_string(manifest) {
        Ok(t) => t,
        Err(e) => return (format!("error: {}: {e}\n", manifest.display()), false),
    };
    let r = Resolver::new(manifest, res_root);
    let mut read = |p: &str| -> Result<(String, String), String> {
        let path = r.resolve(p)?;
        let t = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok((p.to_string(), t))
    };
    let label = manifest.display().to_string();
    let mut out = String::new();
    match load_world((&label, &text), &mut read) {
        Err(e) => {
            for x in &e.errors {
                out.push_str(&format!("error: {x}\n"));
            }
            for x in &e.warnings {
                out.push_str(&format!("warning: {x}\n"));
            }
            out.push_str(&format!("FAILED: {} error(s), {} warning(s)\n", e.errors.len(), e.warnings.len()));
            (out, false)
        }
        Ok(w) => {
            let mut warnings = w.warnings.clone();
            let summary = summary(&w, params, &mut warnings);
            for x in &warnings {
                out.push_str(&format!("warning: {x}\n"));
            }
            out.push_str(&summary);
            out.push_str(&format!("OK: 0 errors, {} warning(s)\n", warnings.len()));
            (out, true)
        }
    }
}

fn summary(w: &SeedFiles, params: WaterParams, warnings: &mut Vec<String>) -> String {
    let m = &w.manifest;
    let mut s = String::new();
    s.push_str(&format!("world: seed \"{}\" (root {:#018x}), world_version {}\n", m.world_seed, world_root(&m.world_seed).0, m.world_version));
    s.push_str(&format!("cities: {}\n", w.cities.len()));
    for c in &w.cities {
        let mode = match &c.real {
            Some(r) if r.url.is_some() => "synthetic, real url available",
            Some(_) => "synthetic, real path available",
            None => "synthetic only",
        };
        s.push_str(&format!(
            "  {} \"{}\" v{} variant {}: radius {} km, {:?} cell {} m, rings 0..{}, ~{} cells (at most {}), {mode}\n",
            c.id,
            c.name,
            c.version,
            c.variant,
            c.radius_km,
            c.layout.kind,
            c.layout.cell_size_m,
            c.ring_count() - 1,
            c.cell_count(1.0),
            c.cell_count(1.0 + simworld_core::seed::files::FOOTPRINT_NOISE),
        ));
        let nominal = c.cell_count(1.0) as f64;
        for d in &c.districts {
            let n = d.cells.map(|n| n as f64).unwrap_or_else(|| (d.share.unwrap_or(0.0) * nominal).round().max(1.0));
            s.push_str(&format!(
                "    {:<12} {:<14} ~{n:.0} cells{}{}\n",
                d.kind.name(),
                format!("{:?}", d.style.unwrap_or(d.kind.default_style())).to_lowercase(),
                d.rings.map(|r| format!(", rings {}..{}", r[0], r[1])).unwrap_or_default(),
                d.near.as_ref().map(|n| format!(", near {:?}", n.features()).to_lowercase()).unwrap_or_default()
            ));
        }
        s.push_str(&format!("    fill {} ({:?}); airports: {}\n", c.fill.kind.name(), c.fill.style.unwrap_or(c.fill.kind.default_style()), c.airports.join(", ")));
    }
    let mut by_type = std::collections::BTreeMap::<&str, usize>::new();
    let mut approx = 0;
    for a in &w.airports {
        *by_type.entry(a.kind.as_str()).or_default() += 1;
        approx += a.runways.iter().filter(|r| !r.has_both_ends()).count();
    }
    s.push_str(&format!(
        "airports: {} selected of {} in the file ({}; source {} {}), {} runway(s) approximate\n",
        w.airports.len(),
        w.all_airports.len(),
        by_type.iter().map(|(k, v)| format!("{v} {k}")).collect::<Vec<_>>().join(", "),
        w.airport_source.0,
        w.airport_source.1,
        approx
    ));
    let mut kinds = [0usize; 3];
    let mut pts = 0usize;
    for p in &w.water.polygons {
        kinds[p.kind as usize] += 1;
        pts += p.rings.iter().map(Vec::len).sum::<usize>();
    }
    s.push_str(&format!(
        "water: {} sea, {} lake, {} river polygon(s), {pts} points, bbox {:?}, outside {:?}, tolerance {} m\n",
        kinds[0], kinds[1], kinds[2], w.water.bbox, w.water.outside, w.water.tolerance_m
    ));
    // The index the library builds at load (lake levels from the default procedural terrain).
    let tp = simworld_core::profile::TerrainProfile { enabled: true, flat_radius_m: 0.0, flat_blend_m: 0.0, ..Default::default() };
    let terrain = simworld_core::Terrain::new(&tp, simworld_core::Profile::default().bands(), 0.0, 0.0);
    let field = |lat: f64, lon: f64| terrain.height(lat, lon);
    let t = Instant::now();
    let idx = WaterIndex::build(&w.water, params, &field);
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    let st = idx.stats();
    s.push_str(&format!(
        "water index (z11, raster {0}x{0}, blend {1} m): built in {ms:.0} ms; {2} cells: {3} water, {4} mixed; {5} rasters; index {6:.2} MiB, rasters {7:.2} MiB\n",
        params.raster_n,
        params.blend_m,
        st.grid_cells,
        st.water_cells,
        st.mixed_cells,
        st.raster_cells,
        st.index_bytes as f64 / 1048576.0,
        st.raster_bytes as f64 / 1048576.0
    ));
    warnings.extend(idx.city_warnings(&w.cities));
    if let Some(r) = &w.roads {
        let mut km = 0.0;
        for road in &r.roads {
            for p in road.points.windows(2) {
                km += simworld_core::seed::files::approx_distance_m(
                    simworld_core::seed::files::LatLon { lat: p[0][1], lon: p[0][0] },
                    simworld_core::seed::files::LatLon { lat: p[1][1], lon: p[1][0] },
                ) / 1000.0;
            }
        }
        s.push_str(&format!("roads: {} ({km:.0} km between author points)\n", r.roads.len()));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../low-poly-map/world/two-cities/world.json")
    }

    #[test]
    fn validates_the_two_cities_fixture() {
        let (out, ok) = run(&fixture(), None, WaterParams::default());
        assert!(ok, "{out}");
        assert!(out.contains("cities: 2"), "{out}");
        assert!(out.contains("airports: 28 selected of 77"), "{out}");
        assert!(out.contains("water index (z11"), "{out}");
        assert!(out.contains("EYPR: runway 04/22 has an end missing"), "{out}");
        assert!(out.contains("OK: 0 errors"), "{out}");
    }

    #[test]
    fn reports_errors_and_fails() {
        let dir = std::env::temp_dir().join(format!("simworld-validate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("cities")).unwrap();
        let src = fixture().parent().unwrap().to_path_buf();
        for f in ["world.json", "airports.json", "water.json", "roads.json", "cities/vilnius.json", "cities/kaunas.json"] {
            std::fs::copy(src.join(f), dir.join(f)).unwrap();
        }
        let k = std::fs::read_to_string(dir.join("cities/kaunas.json")).unwrap().replace("\"id\": \"kaunas\"", "\"id\": \"vilnius\"");
        std::fs::write(dir.join("cities/kaunas.json"), k).unwrap();
        let (out, ok) = run(&dir.join("world.json"), None, WaterParams::default());
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!ok);
        assert!(out.contains("error:") && out.contains("duplicate city id \"vilnius\"") && out.contains("FAILED"), "{out}");
    }

    #[test]
    fn res_paths_resolve_against_the_project() {
        let r = Resolver { manifest_dir: PathBuf::from("/p/world"), res_root: Some(PathBuf::from("/p")) };
        assert_eq!(r.resolve("res://world/a.json").unwrap(), PathBuf::from("/p/world/a.json"));
        assert_eq!(r.resolve("cities/k.json").unwrap(), PathBuf::from("/p/world/cities/k.json"));
        assert!(r.resolve("user://x.json").is_err());
    }
}
