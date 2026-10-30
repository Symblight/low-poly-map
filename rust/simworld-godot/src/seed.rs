//! Seed-world results as Godot types (api 1.7): `airports()` entries and `seed_world_info()`.

use godot::prelude::*;

use simworld_core::{AirportInfo, RunwayEndInfo, SeedInfo};

use crate::ready::{as_col, v3};

fn end_dict(e: &RunwayEndInfo) -> VarDictionary {
    let mut d = VarDictionary::new();
    d.set("ident", &GString::from(e.ident.as_str()));
    d.set("lat", e.lat);
    d.set("lon", e.lon);
    d.set("position", v3(e.position));
    d.set("threshold_lat", e.threshold_lat);
    d.set("threshold_lon", e.threshold_lon);
    d.set("threshold_position", v3(e.threshold_position));
    d.set("displaced_threshold_m", e.displaced_threshold_m);
    d.set("touchdown_position", v3(e.touchdown_position));
    d.set("heading_deg", e.heading_deg);
    d
}

/// `airports()` entry (api 1.7).
pub(crate) fn airport_dict(a: &AirportInfo) -> VarDictionary {
    let mut d = VarDictionary::new();
    d.set("ident", &GString::from(a.ident.as_str()));
    d.set("name", &GString::from(a.name.as_str()));
    d.set("type", &GString::from(a.kind.as_str()));
    d.set("lat", a.lat);
    d.set("lon", a.lon);
    d.set("position", v3(a.position));
    match a.elevation_ft {
        Some(e) => d.set("elevation_ft", e),
        None => d.set("elevation_ft", &Variant::nil()),
    }
    let mut runways: Array<VarDictionary> = Array::new();
    for r in &a.runways {
        let mut rd = VarDictionary::new();
        rd.set("designator", &GString::from(r.designator.as_str()));
        rd.set("le", &end_dict(&r.le));
        rd.set("he", &end_dict(&r.he));
        rd.set("length_m", r.length_m);
        rd.set("width_m", r.width_m);
        rd.set("surface", &GString::from(r.surface.as_str()));
        rd.set("paved", r.paved);
        rd.set("lighted", r.lighted);
        rd.set("approximate", r.approximate);
        let lights: Vec<Vector3> = r.lights.iter().map(|p| v3(*p)).collect();
        rd.set("lights", &PackedVector3Array::from(&lights[..]));
        rd.set("light_colors", &PackedColorArray::from(as_col(&r.light_colors)));
        runways.push(&rd);
    }
    d.set("runways", &runways);
    d
}

/// `seed_world_info()` (api 1.7).
pub(crate) fn seed_info_dict(i: &SeedInfo) -> VarDictionary {
    let mut d = VarDictionary::new();
    d.set("manifest", &GString::from(i.manifest.as_str()));
    d.set("generator_version", i.generator_version as i64);
    let phases: PackedStringArray = i.phases.iter().map(|s| GString::from(s.as_str())).collect();
    d.set("phases", &phases);
    d.set("world_version", i.world_version);
    d.set("world_seed", &GString::from(i.world_seed.as_str()));
    let mut cities: Array<VarDictionary> = Array::new();
    for c in &i.cities {
        let mut cd = VarDictionary::new();
        cd.set("id", &GString::from(c.id.as_str()));
        cd.set("name", &GString::from(c.name.as_str()));
        cd.set("lat", c.lat);
        cd.set("lon", c.lon);
        cd.set("radius_km", c.radius_km);
        cd.set("version", c.version);
        cd.set("source", &GString::from(c.source.as_str()));
        let ap: PackedStringArray = c.airports.iter().map(|s| GString::from(s.as_str())).collect();
        cd.set("airports", &ap);
        // api 1.12: the city plan.
        let st = &c.plan;
        let mut pd = VarDictionary::new();
        pd.set("land_cells", st.land_cells as i64);
        pd.set("water_cells", st.water_cells as i64);
        pd.set("airport_cells", st.airport_cells as i64);
        pd.set("districts", st.districts as i64);
        let mut by_type = VarDictionary::new();
        for (k, v) in &st.cells_by_type {
            by_type.set(k.as_str(), *v as i64);
        }
        pd.set("cells_by_type", &by_type);
        pd.set("arterial_edges", st.arterial_edges as i64);
        pd.set("arterial_components", st.arterial_components as i64);
        pd.set("arterial_components_before", st.arterial_components_before as i64);
        pd.set("bridges", st.bridges as i64);
        pd.set("radials", st.radials as i64);
        pd.set("airport_access", st.airport_access as i64);
        pd.set("build_ms", st.build_ms);
        pd.set("kb", c.plan_bytes as f64 / 1024.0);
        let pw: PackedStringArray = c.plan_warnings.iter().map(|s| GString::from(s.as_str())).collect();
        pd.set("warnings", &pw);
        cd.set("plan", &pd);
        cities.push(&cd);
    }
    d.set("cities", &cities);
    d.set("airports", i.airports as i64);
    d.set("water_polygons", i.water_polygons as i64);
    let warnings: PackedStringArray = i.warnings.iter().map(|s| GString::from(s.as_str())).collect();
    d.set("warnings", &warnings);
    let mb = |b: usize| b as f64 / 1_048_576.0;
    let mut m = VarDictionary::new();
    m.set("water_index_mb", mb(i.stats.water_index_bytes));
    m.set("water_raster_mb", mb(i.stats.water_raster_bytes));
    m.set("strip_index_kb", i.stats.strip_index_bytes as f64 / 1024.0);
    m.set("airports_kb", i.stats.airport_bytes as f64 / 1024.0);
    m.set("load_ms", i.stats.build_ms);
    m.set("city_plans_kb", i.cities.iter().map(|c| c.plan_bytes).sum::<usize>() as f64 / 1024.0);
    m.set("district_plans", i.district_plans.held as i64);
    m.set("district_plans_mb", mb(i.district_plans.bytes));
    m.set("district_plans_built", i.district_plans.built as i64);
    m.set("district_plan_ms", i.district_plans.build_ms);
    d.set("memory", &m);
    d
}
