//! `seed gallery` (docs/spec-seed-city-pipeline.md section 9, phase C0): the generator's
//! inspection fixture. For every city of a world (by default a built-in flat world without water
//! or airports) it writes the Fig. 2 analogue (full layout, streets only, buildings only, plus one
//! sheet of all cities x views) and the Table 2 analogue (`gallery.json`, `gallery.md`): build
//! time per level and per district-plan stage, plan bytes, tile assembly time, and per band the
//! triangles, mesh bytes and build time of every tile over the city's bounding box (meshed
//! through the library's own seed tile pipeline), plus counts.
//!
//! Timing: every district plan of a city is built (cached) before anything is timed; each timed
//! item runs [`REPS`] times and reports the median, so the figures are warm and repeatable.
//! Max RSS and user CPU time are sampled after the measurements, before the figures.
//!
//! Generator 3 has three levels: the city plan, the district plans (one per land cell; stage a =
//! local network + lots, stage b = buildings) and the tile assembly. Generator 4 (phase C3)
//! replaces the fixture cases with organic / grid / radial in `default` and `dense`; the case
//! list is data for that reason.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::hint::black_box;
use std::path::Path;
use std::time::Instant;

use serde_json::{json, Value};
use simworld_core::geo::{lat_to_tile_y, lon_to_tile_x, Frame, TileKey};
use simworld_core::seed::city::{buildings as stage_buildings, lots as stage_lots, network::local as stage_local, CellInput, CityPlan, RoadKind};
use simworld_core::seed::files::DistrictType;
use simworld_core::seed::geom::signed_area;
use simworld_core::seed::world::generate;
use simworld_core::seed::SEED_GEN_VERSION;

use super::preview::{self, Layers, Loaded};

/// One city of the built-in fixture world.
pub struct Case {
    pub id: &'static str,
    pub lat: f64,
    pub lon: f64,
    pub radius_km: f64,
    pub cell_size_m: f64,
    /// The city file's `districts` array (JSON).
    pub districts: &'static str,
}

const DISTRICTS: &str = r#"[
    { "type": "old_town", "cells": 1, "max_height_m": 25, "landmarks": 2 },
    { "type": "business", "share": 0.1, "max_height_m": 80, "density": 0.75 },
    { "type": "residential", "style": "microdistrict", "share": 0.3, "max_height_m": 45 },
    { "type": "park", "share": 0.08 }
]"#;

/// Three small `old_center_hex` cities with different radii and cell sizes, about 13 km apart
/// on flat land.
pub const FIXTURE: [Case; 3] = [
    Case { id: "hex-s", lat: 55.0, lon: 23.0, radius_km: 0.8, cell_size_m: 300.0, districts: DISTRICTS },
    Case { id: "hex-m", lat: 55.0, lon: 23.2, radius_km: 1.2, cell_size_m: 450.0, districts: DISTRICTS },
    Case { id: "hex-l", lat: 55.0, lon: 23.4, radius_km: 1.6, cell_size_m: 600.0, districts: DISTRICTS },
];

const FIXTURE_DIR: &str = "gallery-fixture";

/// The files of the fixture world: (path as the loader asks for it, text).
fn fixture_files(cases: &[Case]) -> BTreeMap<String, String> {
    let mut f = BTreeMap::new();
    let cities: Vec<String> = cases.iter().map(|c| format!("cities/{}.json", c.id)).collect();
    let manifest = json!({
        "format": 1, "world_version": 1, "world_seed": "gallery-fixture", "water": "water.json",
        "airports": { "file": "airports.json" }, "cities": cities,
    });
    f.insert(format!("{FIXTURE_DIR}/world.json"), manifest.to_string());
    let (mut w, mut s, mut e, mut n) = (180.0f64, 90.0f64, -180.0f64, -90.0f64);
    for c in cases {
        (w, s, e, n) = (w.min(c.lon - 0.5), s.min(c.lat - 0.5), e.max(c.lon + 0.5), n.max(c.lat + 0.5));
    }
    let water = json!({ "format": 1, "source": "gallery fixture", "bbox": [w, s, e, n], "outside": "land", "tolerance_m": 50.0, "polygons": [] });
    f.insert(format!("{FIXTURE_DIR}/water.json"), water.to_string());
    f.insert(format!("{FIXTURE_DIR}/airports.json"), json!({ "format": 1, "source": "gallery fixture", "source_date": "2026-10-30", "airports": [] }).to_string());
    for c in cases {
        let districts: Value = serde_json::from_str(c.districts).expect("fixture districts");
        let city = json!({
            "id": c.id, "name": c.id, "version": 1, "center": { "lat": c.lat, "lon": c.lon }, "radius_km": c.radius_km,
            "layout": { "type": "old_center_hex", "cell_size_m": c.cell_size_m }, "max_height_m": 60, "density": 0.6,
            "districts": districts, "fill": { "type": "residential", "style": "houses", "max_height_m": 10, "density": 0.35 },
        });
        f.insert(format!("{FIXTURE_DIR}/cities/{}.json", c.id), city.to_string());
    }
    f
}

/// The built-in flat fixture world (terrain amplitude 0, no water, no airports).
pub fn fixture_world() -> Result<Loaded, String> {
    let files = fixture_files(&FIXTURE);
    let read = move |p: &str| files.get(p).cloned().ok_or_else(|| format!("{p}: not in the fixture"));
    let ov = json!({ "terrain": { "enabled": true, "amplitude_m": 0.0, "flat_radius_m": 0.0, "flat_blend_m": 0.0 } });
    preview::load_with(&format!("{FIXTURE_DIR}/world.json"), &read, &ov)
}

/// What `run` reports back.
pub struct Summary {
    pub cities: usize,
}

fn key_at(lat: f64, lon: f64, z: u8) -> TileKey {
    TileKey::new(z, lon_to_tile_x(lon, z) as u32, lat_to_tile_y(lat, z) as u32)
}

/// Value at quantile `q` of a sorted slice.
fn q<T: Copy + Default>(v: &[T], q: f64) -> T {
    if v.is_empty() { T::default() } else { v[((v.len() - 1) as f64 * q).round() as usize] }
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn r2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Process resource use: (peak resident bytes, user CPU seconds), where the platform reports it.
#[derive(Clone, Copy, Default)]
struct Usage {
    max_rss_bytes: Option<u64>,
    user_cpu_s: Option<f64>,
}

#[cfg(unix)]
fn usage() -> Usage {
    // SAFETY: `getrusage` fills the zeroed struct it is given and touches nothing else.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) } != 0 {
        return Usage::default();
    }
    let v = ru.ru_maxrss.max(0) as u64;
    Usage {
        max_rss_bytes: Some(if cfg!(target_os = "macos") { v } else { v * 1024 }),
        user_cpu_s: Some(ru.ru_utime.tv_sec as f64 + ru.ru_utime.tv_usec as f64 * 1e-6),
    }
}

#[cfg(not(unix))]
fn usage() -> Usage {
    Usage::default()
}

/// Runs per timed item (the median is reported).
const REPS: usize = 5;

/// Median wall time (ms) of [`REPS`] runs of `f`, and the last run's output.
fn timed<T>(mut f: impl FnMut() -> T) -> (f64, T) {
    let mut t = [0.0f64; REPS];
    let mut last = None;
    for v in t.iter_mut() {
        let s = Instant::now();
        let out = f();
        *v = ms(s);
        last = Some(out);
    }
    t.sort_by(f64::total_cmp);
    (t[REPS / 2], last.expect("REPS > 0"))
}

/// p50 / p95 / max of a list (sorted here) as JSON.
fn stats_f(mut v: Vec<f64>) -> Value {
    v.sort_by(f64::total_cmp);
    json!({ "p50": r2(q(&v, 0.5)), "p95": r2(q(&v, 0.95)), "max": r2(q(&v, 1.0)) })
}

fn stats_u(mut v: Vec<usize>) -> Value {
    v.sort_unstable();
    json!({ "p50": q(&v, 0.5), "p95": q(&v, 0.95), "max": q(&v, 1.0) })
}

/// Measure one city (no figures); returns its `gallery.json` entry.
fn city(l: &Loaded, ci: usize) -> Result<Value, String> {
    let w = &l.world;
    let c = &w.cities()[ci];
    let (lat, lon) = (c.center.lat, c.center.lon);
    let plan = w.city_plan(ci);
    let st = &plan.stats;

    // Warm-up: every district plan of the city, through the plan cache (the tiles below use
    // them), with the counts on the way. Nothing before this point is timed.
    let land: Vec<_> = plan.cells.values().filter(|c| c.district.is_some()).map(|c| c.ax).collect();
    let mut plan_bytes = Vec::with_capacity(land.len());
    let (mut streets, mut buildings, mut area_sum) = (0usize, 0usize, 0.0f64);
    let mut ring: Vec<[f64; 2]> = Vec::with_capacity(8);
    for &ax in &land {
        let cp = w.cell_plan(&plan, ax);
        plan_bytes.push(cp.bytes());
        streets += cp.streets.len();
        buildings += cp.buildings.len();
        for b in &cp.buildings {
            ring.clear();
            ring.extend(b.ring.iter().map(|p| [p[0] as f64, p[1] as f64]));
            area_sum += signed_area(&ring).abs();
        }
    }
    plan_bytes.sort_unstable();

    // Level 1: the city plan, rebuilt (uncached).
    let (city_ms, _) = timed(|| CityPlan::build(ci, c, w));

    // Level 2: every district plan, rebuilt by stage: a = local network + lots, b = buildings.
    let (mut a_ms, mut b_ms, mut ab_ms) = (Vec::with_capacity(land.len()), Vec::with_capacity(land.len()), Vec::with_capacity(land.len()));
    for &ax in &land {
        let Some(inp) = CellInput::new(&plan, ax, w) else { continue };
        let (mut ta, mut tb, mut tab) = ([0.0f64; REPS], [0.0f64; REPS], [0.0f64; REPS]);
        for i in 0..REPS {
            let t = Instant::now();
            let net = black_box(stage_local::build(&inp));
            let lots = black_box(stage_lots::build(&inp, &net));
            ta[i] = ms(t);
            let t = Instant::now();
            let bs = black_box(stage_buildings::build(&inp, &net, &lots));
            tb[i] = ms(t);
            tab[i] = ta[i] + tb[i];
            drop((net, lots, bs));
        }
        for (t, out) in [(&mut ta, &mut a_ms), (&mut tb, &mut b_ms), (&mut tab, &mut ab_ms)] {
            t.sort_by(f64::total_cmp);
            out.push(t[REPS / 2]);
        }
    }

    // Level 3: tile assembly of the centre tile per zoom (plans cached).
    let mut tile_ms = serde_json::Map::new();
    for z in [16u8, 15, 14, 13] {
        let (t, g) = timed(|| generate(key_at(lat, lon, z), w));
        tile_ms.insert(format!("z{z}"), json!({ "ms": r2(t), "buildings": g.buildings.len(), "roads": g.ground.roads.len(), "areas": g.ground.areas.len() }));
    }

    // Per band: every tile over the city's bounding box, assembly + meshing through the
    // library's seed tile pipeline (buildings + ground; no terrain, no lights).
    let frame = Frame::new(lat, lon);
    let bb = plan.bbox_ll;
    let mut bands = serde_json::Map::new();
    for ring in 0..4usize {
        let z = 16 - ring as u8;
        let (x0, x1) = (lon_to_tile_x(bb[0], z) as u32, lon_to_tile_x(bb[2], z) as u32);
        let (y0, y1) = (lat_to_tile_y(bb[3], z) as u32, lat_to_tile_y(bb[1], z) as u32);
        let (mut bt, mut gt, mut kb, mut tms) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut tiles = 0usize;
        for y in y0..=y1 {
            for x in x0..=x1 {
                tiles += 1;
                let key = TileKey::new(z, x, y);
                let (t, d) = timed(|| simworld_core::worker::build_seed_tile_on(key, ring, frame, &l.terrain, w, &l.profile));
                let d = d?;
                let (b, g) = (d.mesh.triangle_count(), d.ground.triangle_count());
                if b + g == 0 {
                    continue;
                }
                if b > 0 {
                    bt.push(b);
                }
                gt.push(g);
                kb.push((d.mesh.bytes() + d.ground.bytes()) as f64 / 1024.0);
                tms.push(t);
            }
        }
        bands.insert(
            format!("R{ring}"),
            json!({
                "zoom": z, "tiles": tiles, "tiles_nonempty": gt.len(), "tiles_with_buildings": bt.len(),
                "buildings_tris": stats_u(bt), "ground_tris": stats_u(gt), "mesh_kb": stats_f(kb), "tile_ms": stats_f(tms),
            }),
        );
    }

    let mut kinds: BTreeMap<DistrictType, u32> = BTreeMap::new();
    for d in &plan.districts {
        *kinds.entry(d.kind).or_default() += 1;
    }
    let by_type: serde_json::Map<String, Value> =
        kinds.iter().map(|(k, n)| (serde_json::to_value(k).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default(), json!(n))).collect();
    let city_bytes = plan.bytes();
    let district_bytes: usize = plan_bytes.iter().sum();
    Ok(json!({
        "id": plan.id,
        "radius_km": c.radius_km,
        "cell_size_m": c.layout.cell_size_m,
        "city_plan": { "ms": r2(city_ms), "bytes": city_bytes },
        "district_plans": {
            "count": ab_ms.len(), "ms": stats_f(ab_ms), "stage_a_ms": stats_f(a_ms), "stage_b_ms": stats_f(b_ms),
            "bytes_p50": q(&plan_bytes, 0.5), "bytes_max": q(&plan_bytes, 1.0), "bytes_sum": district_bytes,
        },
        "tile_assembly": tile_ms,
        "bands": bands,
        "counts": {
            "cells_land": st.land_cells, "cells_water": st.water_cells, "cells_airport": st.airport_cells,
            "cells_by_type": st.cells_by_type, "districts": plan.districts.len(), "districts_by_type": by_type,
            "arterials": plan.roads.iter().filter(|r| r.kind == RoadKind::Arterial).count(), "arterial_edges": st.arterial_edges,
            "radials": plan.roads.iter().filter(|r| r.kind == RoadKind::Radial).count(), "airport_access": st.airport_access,
            "local_streets": streets, "buildings": buildings,
        },
        "mean_building_m2": if buildings > 0 { (area_sum / buildings as f64).round() } else { 0.0 },
        "memory": { "plan_bytes_sum": city_bytes + district_bytes },
    }))
}

/// Fig. 2 analogue of one city: full layout, streets only, buildings only.
fn figures(l: &Loaded, ci: usize, out: &Path) -> Result<(), String> {
    let w = &l.world;
    let plan = w.city_plan(ci);
    let span = {
        let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for c in plan.cells.values() {
            b = [b[0].min(c.bbox[0]), b[1].min(c.bbox[1]), b[2].max(c.bbox[2]), b[3].max(c.bbox[3])];
        }
        (b[2] - b[0]).max(b[3] - b[1])
    };
    let m_per_px = (span / 1600.0).max(0.5);
    let all = Layers { cells: true, outline: true, zones: true, major: true, local: true, buildings: true };
    let views = [
        ("full", all),
        ("streets", Layers { major: true, local: true, ..Layers::default() }),
        ("buildings", Layers { cells: true, outline: true, buildings: true, ..Layers::default() }),
    ];
    for (name, layers) in views {
        let p = out.join(format!("{}-{name}.svg", plan.id));
        std::fs::write(&p, preview::svg(w, &plan, layers, None, m_per_px)).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    Ok(())
}

/// One sheet: a row per city, a column per view (the three SVGs nested, scaled to a cell).
fn sheet(ids: &[String], out: &Path) -> Result<(), String> {
    const CELL: f64 = 600.0;
    let views = ["full", "streets", "buildings"];
    let (wpx, hpx) = (CELL * views.len() as f64, (CELL + 24.0) * ids.len() as f64);
    let mut s = String::new();
    let _ = writeln!(s, r#"<svg xmlns="http://www.w3.org/2000/svg" width="{wpx:.0}" height="{hpx:.0}" viewBox="0 0 {wpx:.0} {hpx:.0}">"#);
    for (row, id) in ids.iter().enumerate() {
        let y = row as f64 * (CELL + 24.0);
        let _ = writeln!(s, r#"<text x="8" y="{:.0}" font-size="16" font-family="sans-serif">{id}: {}</text>"#, y + 18.0, views.join(" / "));
        for (col, v) in views.iter().enumerate() {
            let p = out.join(format!("{id}-{v}.svg"));
            let one = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            // Our own header: swap its size for the cell's place (the viewBox scales it).
            let Some(rest) = one.split_once("\" viewBox=").map(|(_, r)| r) else { continue };
            let _ = write!(s, r#"<svg x="{:.0}" y="{:.0}" width="{CELL:.0}" height="{CELL:.0}" viewBox="#, col as f64 * CELL, y + 24.0);
            s.push_str(rest);
        }
    }
    s.push_str("</svg>\n");
    let p = out.join("sheet.svg");
    std::fs::write(&p, s).map_err(|e| format!("{}: {e}", p.display()))
}

/// The table: one row per metric, one column per city.
fn markdown(label: &str, cities: &[Value], u: Usage) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# seed gallery: {label} (generator {SEED_GEN_VERSION})\n");
    let _ = writeln!(
        s,
        "Release numbers are meaningful only from a release build. Every district plan is built before anything is timed; \
         each timed item is the median of {REPS} runs. p50 / p95 / max are over the items of the row: with n items p95 is \
         item round(0.95 (n - 1)) of the sorted list, so with about 27 items (the built-in fixture's district plans) p95 is the second-largest value.\n"
    );
    let _ = write!(s, "| metric |");
    for c in cities {
        let _ = write!(s, " {} |", c["id"].as_str().unwrap_or("?"));
    }
    let _ = write!(s, "\n|---|");
    for _ in cities {
        s.push_str("---|");
    }
    s.push('\n');
    let mut row = |name: &str, f: &dyn Fn(&Value) -> String| {
        let _ = write!(s, "| {name} |");
        for c in cities {
            let _ = write!(s, " {} |", f(c));
        }
        s.push('\n');
    };
    let v = |c: &Value, p: &str| c.pointer(p).map_or_else(|| "-".to_string(), |x| x.to_string());
    let ppm = |c: &Value, p: &str| format!("{} / {} / {}", v(c, &format!("{p}/p50")), v(c, &format!("{p}/p95")), v(c, &format!("{p}/max")));
    row("radius km / cell m", &|c| format!("{} / {}", v(c, "/radius_km"), v(c, "/cell_size_m")));
    row("city plan ms / KB", &|c| format!("{} / {:.1}", v(c, "/city_plan/ms"), c["city_plan"]["bytes"].as_f64().unwrap_or(0.0) / 1024.0));
    row("district plans", &|c| v(c, "/district_plans/count"));
    row("district plan ms p50 / p95 / max", &|c| ppm(c, "/district_plans/ms"));
    row("stage a (local network + lots) ms p50 / p95 / max", &|c| ppm(c, "/district_plans/stage_a_ms"));
    row("stage b (buildings) ms p50 / p95 / max", &|c| ppm(c, "/district_plans/stage_b_ms"));
    row("district plan KB p50 / max", &|c| {
        let k = |p: &str| c.pointer(p).and_then(Value::as_f64).unwrap_or(0.0) / 1024.0;
        format!("{:.1} / {:.1}", k("/district_plans/bytes_p50"), k("/district_plans/bytes_max"))
    });
    for z in [13, 14, 15, 16] {
        row(&format!("centre tile z{z} assembly ms"), &|c| v(c, &format!("/tile_assembly/z{z}/ms")));
    }
    for r in 0..4 {
        row(&format!("R{r} tiles over the bbox / non-empty / with buildings"), &|c| {
            format!("{} / {} / {}", v(c, &format!("/bands/R{r}/tiles")), v(c, &format!("/bands/R{r}/tiles_nonempty")), v(c, &format!("/bands/R{r}/tiles_with_buildings")))
        });
        row(&format!("R{r} tris (buildings + ground, no terrain, no lights): buildings p50 / p95 / max"), &|c| ppm(c, &format!("/bands/R{r}/buildings_tris")));
        row(&format!("R{r} tris (buildings + ground, no terrain, no lights): ground p50 / p95 / max"), &|c| ppm(c, &format!("/bands/R{r}/ground_tris")));
        row(&format!("R{r} mesh KB p50 / p95 / max"), &|c| ppm(c, &format!("/bands/R{r}/mesh_kb")));
        row(&format!("R{r} tile ms p50 / p95 / max (assembly + meshing)"), &|c| ppm(c, &format!("/bands/R{r}/tile_ms")));
    }
    row("cells land / water / airport", &|c| format!("{} / {} / {}", v(c, "/counts/cells_land"), v(c, "/counts/cells_water"), v(c, "/counts/cells_airport")));
    row("districts (by type)", &|c| format!("{} {}", v(c, "/counts/districts"), v(c, "/counts/districts_by_type").replace('"', "")));
    row("arterials (edges) / radials (to airports)", &|c| {
        format!("{} ({}) / {} ({})", v(c, "/counts/arterials"), v(c, "/counts/arterial_edges"), v(c, "/counts/radials"), v(c, "/counts/airport_access"))
    });
    row("local streets / buildings", &|c| format!("{} / {}", v(c, "/counts/local_streets"), v(c, "/counts/buildings")));
    row("mean building footprint m2", &|c| v(c, "/mean_building_m2"));
    row("plan bytes sum KB", &|c| format!("{:.0}", c.pointer("/memory/plan_bytes_sum").and_then(Value::as_f64).unwrap_or(0.0) / 1024.0));
    s.push_str(
        "\nTile rows: every tile at the band's zoom over the city's bounding box; triangles, KB and ms over the non-empty tiles \
         (building triangles over the tiles with buildings). The countryside ground makes every tile of a seed world non-empty.\n",
    );
    match (u.max_rss_bytes, u.user_cpu_s) {
        (Some(b), Some(cpu)) => {
            let _ = writeln!(s, "\nProcess max RSS {:.1} MB, user CPU {cpu:.1} s (after the measurements, before the figures).", b as f64 / 1e6);
        }
        _ => s.push_str("\nProcess max RSS / user CPU: not reported on this platform.\n"),
    }
    s
}

/// Run the gallery on every city of `l` into `out` (created if missing).
pub fn run(l: &Loaded, label: &str, out: &Path) -> Result<Summary, String> {
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let t = Instant::now();
    let mut cities = Vec::with_capacity(l.world.cities().len());
    for ci in 0..l.world.cities().len() {
        cities.push(city(l, ci)?);
    }
    let u = usage();
    for ci in 0..l.world.cities().len() {
        figures(l, ci, out)?;
    }
    let ids: Vec<String> = l.world.cities().iter().map(|c| c.id.clone()).collect();
    sheet(&ids, out)?;
    let (held, cache_bytes, built, _) = l.world.cell_plan_stats();
    let doc = json!({
        "world": label,
        "generator": SEED_GEN_VERSION,
        "reps": REPS,
        "cities": cities,
        "plan_cache": { "held": held, "bytes": cache_bytes, "built": built },
        "max_rss_bytes": u.max_rss_bytes,
        "user_cpu_s": u.user_cpu_s.map(r2),
        "total_s": r2(t.elapsed().as_secs_f64()),
    });
    let write = |name: &str, text: String| {
        let p = out.join(name);
        std::fs::write(&p, text).map_err(|e| format!("{}: {e}", p.display()))
    };
    write("gallery.json", serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())? + "\n")?;
    write("gallery.md", markdown(label, &cities, u))?;
    Ok(Summary { cities: ids.len() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gallery_on_the_fixture_writes_the_table_and_figures() {
        let dir = std::env::temp_dir().join(format!("simworld-seed-gallery-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let args: Vec<String> = ["gallery", "-o", dir.to_str().unwrap()].iter().map(|s| s.to_string()).collect();
        assert!(super::super::run(&args).unwrap());
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("gallery.json")).unwrap()).unwrap();
        let cities = doc["cities"].as_array().unwrap();
        assert_eq!(cities.len(), FIXTURE.len());
        for c in cities {
            for p in [
                "/city_plan/ms", "/city_plan/bytes", "/district_plans/count", "/district_plans/ms/p50", "/district_plans/ms/p95",
                "/district_plans/stage_a_ms/max", "/district_plans/stage_b_ms/p95", "/district_plans/bytes_p50", "/district_plans/bytes_max",
                "/tile_assembly/z13/ms", "/tile_assembly/z16/ms", "/bands/R0/buildings_tris/p95", "/bands/R0/ground_tris/max",
                "/bands/R2/buildings_tris/max", "/bands/R2/ground_tris/p50", "/bands/R0/mesh_kb/p95", "/bands/R1/tile_ms/max",
                "/counts/cells_land", "/counts/districts_by_type", "/counts/arterials", "/counts/radials",
                "/counts/local_streets", "/counts/buildings", "/mean_building_m2", "/memory/plan_bytes_sum",
            ] {
                assert!(c.pointer(p).is_some(), "{} misses {p}", c["id"]);
            }
            assert!(c["counts"]["buildings"].as_u64().unwrap() > 0 && c["bands"]["R0"]["buildings_tris"]["max"].as_u64().unwrap() > 0, "{c}");
            let id = c["id"].as_str().unwrap();
            for v in ["full", "streets", "buildings"] {
                assert!(std::fs::read_to_string(dir.join(format!("{id}-{v}.svg"))).unwrap().starts_with("<svg"));
            }
        }
        assert!(doc.get("max_rss_bytes").is_some() && doc.get("user_cpu_s").is_some());
        assert!(std::fs::read_to_string(dir.join("gallery.md")).unwrap().contains("| district plans |"));
        assert!(std::fs::read_to_string(dir.join("sheet.svg")).unwrap().matches("<svg").count() == 1 + 3 * FIXTURE.len());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
