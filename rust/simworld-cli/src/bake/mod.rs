//! `simworld-cli bake`: the two-level tile bake of "Tiles and LOD v2" (step T1b).
//!
//! Reads today's archive (z13-z16, `data/out/vilnius.pmtiles`) and re-tiles it into two data
//! levels in one PMTiles v3 file (see `levels` for the content of each level):
//! detail z14 (extent 8192) and overview z11 (extent 4096, `landmarks` layer at 16384).
//! Every building is in exactly one of detail footprints / landmarks; blocks and light points are
//! derived data. Metadata: `simworld_levels` = {"detail":14,"overview":11}, the attribution, the
//! bake parameters (`simworld_bake`) and `vector_layers`.

pub mod archive;
pub mod geom;
pub mod input;
pub mod levels;
pub mod mvt_enc;
pub mod verify;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::time::Instant;

use rayon::prelude::*;

use archive::{Archive, Key};
use levels::{Counts, Params};

pub const USAGE: &str = "\
  simworld-cli bake <in.pmtiles> <out.pmtiles> [--block-min-area M2] [--block-gap M] [--landmark-m M]
      re-tile a z13-z16 archive into the two data levels (detail z14, overview z11 + landmarks),
      print sizes, then verify the result against the source
  simworld-cli bake --verify <out.pmtiles> [--input <in.pmtiles>]
      check a two-level archive (levels, metadata, landmarks, blocks, heights, light points)";

/// `bake ...` arguments (after `bake`).
pub fn run(args: &[String]) -> Result<bool, String> {
    let mut p = Params::default();
    let mut pos = Vec::new();
    let (mut verify_path, mut input) = (None, None);
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let mut val = |name: &str| -> Result<String, String> {
            if let Some(v) = a.strip_prefix(&format!("{name}=")) {
                return Ok(v.to_string());
            }
            i += 1;
            args.get(i).cloned().ok_or_else(|| format!("{name} needs a value"))
        };
        let numv = |s: String, name: &str| s.parse::<f64>().map_err(|_| format!("{name}: not a number: {s}"));
        if a == "--verify" || a.starts_with("--verify=") {
            verify_path = Some(val("--verify")?);
        } else if a == "--input" || a.starts_with("--input=") {
            input = Some(val("--input")?);
        } else if a == "--block-min-area" || a.starts_with("--block-min-area=") {
            p.block_min_area_m2 = numv(val("--block-min-area")?, a)?;
        } else if a == "--block-gap" || a.starts_with("--block-gap=") {
            p.block_gap_m = numv(val("--block-gap")?, a)?;
        } else if a == "--landmark-m" || a.starts_with("--landmark-m=") {
            p.landmark_m = numv(val("--landmark-m")?, a)?;
        } else if a.starts_with("--") {
            return Err(format!("bake: unknown option {a}\n{USAGE}"));
        } else {
            pos.push(a.to_string());
        }
        i += 1;
    }
    if let Some(out) = verify_path {
        let a = Archive::open(&out)?;
        let src = match &input {
            Some(path) => {
                let s = Archive::open(path)?;
                let lu = input::LanduseIndex::build(&s, p.detail_z)?;
                Some(input::buildings(&s, s.max_zoom, &lu)?)
            }
            None => None,
        };
        return Ok(report_verify(&a, src.as_deref()));
    }
    let [inp, out] = pos.as_slice() else { return Err(format!("bake: need <in.pmtiles> <out.pmtiles>\n{USAGE}")) };
    let (tiles, stats, buildings) = bake(inp, &p)?;
    let src = Archive::open(inp)?;
    let meta = metadata(&p, &src);
    archive::write(out, &tiles, &meta, src.bounds, 13)?;
    let written = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    print_stats(&stats, written, src.file_len() as u64, &p);
    let a = Archive::open(out)?;
    Ok(report_verify(&a, Some(&buildings)))
}

fn report_verify(a: &Archive, src: Option<&[input::Building]>) -> bool {
    match verify::verify(a, src) {
        Ok(s) => {
            println!(
                "verify OK: {} detail buildings, {} landmarks, {} blocks, {} light points{}",
                s.detail_ids,
                s.landmark_ids,
                s.blocks,
                s.lights,
                if src.is_some() { ", every source building in exactly one level" } else { "" }
            );
            true
        }
        Err(errs) => {
            for e in &errs {
                println!("verify FAIL: {e}");
            }
            false
        }
    }
}

pub struct Stats {
    pub counts: Counts,
    /// (level zoom) -> gzipped tile sizes with their keys.
    pub sizes: BTreeMap<u8, Vec<(usize, Key)>>,
    /// Overview: raw bytes of the `lights` layers, raw bytes of all layers, gz bytes without lights.
    pub lights_raw: usize,
    pub overview_raw: usize,
    pub overview_gz_without_lights: usize,
    pub seconds: f64,
}

/// Runs the bake; returns gzipped tiles, stats and the source buildings (for verification).
pub fn bake(input_path: &str, p: &Params) -> Result<(BTreeMap<Key, Vec<u8>>, Stats, Vec<input::Building>), String> {
    let t0 = Instant::now();
    let src = Archive::open(input_path)?;
    if src.max_zoom < 16 || src.min_zoom > p.detail_z || src.keys_at(16).is_empty() {
        return Err(format!("{input_path}: expected a z13-z16 source archive (zooms {}..{})", src.min_zoom, src.max_zoom));
    }
    let lu = input::LanduseIndex::build(&src, p.detail_z)?;
    let buildings = input::buildings(&src, 16, &lu)?;
    eprintln!("bake: {} buildings stitched from z16 in {:.1} s", buildings.len(), t0.elapsed().as_secs_f64());
    let mut counts = Counts { buildings: buildings.len(), ..Counts::default() };

    let d_b = levels::detail_buildings(&buildings, p, &mut counts);
    let d_lu = levels::detail_ground(&src, p, "landuse")?;
    let d_rd = levels::detail_ground(&src, p, "roads")?;
    let ext = p.detail_extent;
    let detail = levels::encode(vec![("buildings", ext, d_b), ("landuse", ext, d_lu), ("roads", ext, d_rd)]);
    eprintln!("bake: detail level encoded ({} tiles) at {:.1} s", detail.len(), t0.elapsed().as_secs_f64());

    let (o_blocks, _) = levels::blocks(&buildings, p, &mut counts);
    eprintln!("bake: {} blocks from {} clusters at {:.1} s", counts.blocks, counts.clusters, t0.elapsed().as_secs_f64());
    let o_land = levels::landmarks(&buildings, p, &mut counts);
    let o_lights = levels::lights(&buildings, p, &mut counts);
    let low = src.min_zoom;
    let o_roads = levels::overview_roads(&input::exact_pieces(&src, low, "roads")?, p);
    let o_lu = levels::overview_landuse(&input::exact_pieces(&src, low, "landuse")?, p);
    let oe = p.overview_extent;
    let overview = levels::encode(vec![
        ("blocks", oe, o_blocks),
        ("landmarks", p.landmark_extent, o_land),
        ("lights", oe, o_lights),
        ("roads", oe, o_roads),
        ("landuse", oe, o_lu),
    ]);
    eprintln!("bake: overview level encoded ({} tiles) at {:.1} s", overview.len(), t0.elapsed().as_secs_f64());

    let all: Vec<(Key, &Vec<(String, Vec<u8>)>)> = detail.iter().chain(overview.iter()).map(|(k, v)| (*k, v)).collect();
    let gz: Vec<(Key, Vec<u8>, usize, usize, usize)> = all
        .par_iter()
        .map(|(k, layers)| {
            let bytes = levels::tile_bytes(layers);
            let (mut lights_raw, mut without) = (0, 0);
            if k.0 == p.overview_z {
                lights_raw = layers.iter().filter(|l| l.0 == "lights").map(|l| l.1.len()).sum();
                let rest: Vec<(String, Vec<u8>)> = layers.iter().filter(|l| l.0 != "lights").cloned().collect();
                without = if rest.is_empty() { 0 } else { archive::gzip(&levels::tile_bytes(&rest)).len() };
            }
            (*k, archive::gzip(&bytes), bytes.len(), lights_raw, without)
        })
        .collect();
    let mut stats = Stats { counts, sizes: BTreeMap::new(), lights_raw: 0, overview_raw: 0, overview_gz_without_lights: 0, seconds: 0.0 };
    let mut tiles = BTreeMap::new();
    for (k, g, raw, lr, wo) in gz {
        stats.sizes.entry(k.0).or_default().push((g.len(), k));
        if k.0 == p.overview_z {
            stats.lights_raw += lr;
            stats.overview_raw += raw;
            stats.overview_gz_without_lights += wo;
        }
        tiles.insert(k, g);
    }
    stats.seconds = t0.elapsed().as_secs_f64();
    Ok((tiles, stats, buildings))
}

pub fn metadata(p: &Params, src: &Archive) -> serde_json::Value {
    let fields_b = serde_json::json!({"id": "Number", "osm_type": "String", "kind": "String", "height": "Number", "levels": "Number",
        "min_height": "Number", "colour": "String", "historic": "String", "man_made": "String", "windows": "Boolean",
        "ruins": "Boolean", "disused": "Boolean", "abandoned": "Boolean"});
    let (d, o) = (p.detail_z, p.overview_z);
    serde_json::json!({
        "name": "simworld two-level tiles",
        "format": "pbf",
        "type": "overlay",
        "attribution": "<a href=\"https://www.openstreetmap.org/copyright\" target=\"_blank\">© OpenStreetMap contributors</a>",
        "description": "SimWorld data levels (Tiles and LOD v2): detail z14 footprints, overview z11 blocks, lights and landmarks",
        "simworld_levels": {"detail": d, "overview": o},
        "simworld_bake": p.to_json(),
        "source_attribution": src.metadata.get("attribution").cloned().unwrap_or(serde_json::Value::Null),
        "vector_layers": [
            {"id": "buildings", "minzoom": d, "maxzoom": d, "fields": fields_b},
            {"id": "landuse", "minzoom": o, "maxzoom": d, "fields": {"id": "Number", "kind": "String"}},
            {"id": "roads", "minzoom": o, "maxzoom": d, "fields": {"id": "Number", "kind": "String", "lit": "String"}},
            {"id": "blocks", "minzoom": o, "maxzoom": o, "fields": {"id": "Number", "kind": "String", "height": "Number", "count": "Number"}},
            {"id": "landmarks", "minzoom": o, "maxzoom": o, "fields": fields_b},
            {"id": "lights", "minzoom": o, "maxzoom": o, "fields": {"id": "Number", "osm_type": "String", "kind": "String", "height": "Number",
                "area": "Number", "landuse": "String", "historic": "String", "man_made": "String", "windows": "Boolean",
                "ruins": "Boolean", "disused": "Boolean", "abandoned": "Boolean"}}
        ]
    })
}

fn pct(v: &[usize], q: f64) -> usize {
    if v.is_empty() {
        return 0;
    }
    v[((v.len() - 1) as f64 * q).round() as usize]
}

fn print_stats(s: &Stats, written: u64, src_len: u64, p: &Params) {
    let c = &s.counts;
    println!("bake done in {:.1} s", s.seconds);
    println!(
        "buildings: {} source, {} detail, {} landmarks (roof >= {} m), {} dropped from detail (< {} m², still lit)",
        c.buildings, c.detail_buildings, c.landmarks, p.landmark_m, c.dropped_small, p.detail_min_area_m2
    );
    println!(
        "blocks: {} from {} clusters of {} buildings (gap {} m, min {} m²); {} buildings ({:.1} % by count, {:.1} % by footprint area) inside a kept block; {} light points",
        c.blocks,
        c.clusters,
        c.block_candidates,
        p.block_gap_m,
        p.block_min_area_m2,
        c.buildings_in_blocks,
        100.0 * c.buildings_in_blocks as f64 / c.block_candidates.max(1) as f64,
        100.0 * c.in_blocks_m2 / c.candidate_m2.max(1.0),
        c.lights
    );
    println!("level  zoom  tiles   total gz      p50       p95       max  (largest)");
    for (z, v) in &s.sizes {
        let mut b: Vec<usize> = v.iter().map(|x| x.0).collect();
        b.sort_unstable();
        let tot: usize = b.iter().sum();
        let big = v.iter().max_by_key(|x| x.0).map(|x| x.1).unwrap_or_default();
        let name = if *z == p.detail_z { "detail" } else { "overview" };
        println!(
            "{name:<8} z{z:<3} {:>6} {:>9.2} MB {:>7} B {:>7} B {:>7} B  {}/{}/{}",
            b.len(),
            tot as f64 / 1e6,
            pct(&b, 0.5),
            pct(&b, 0.95),
            b.last().copied().unwrap_or(0),
            big.0,
            big.1,
            big.2
        );
    }
    let ov_gz: usize = s.sizes.get(&p.overview_z).map(|v| v.iter().map(|x| x.0).sum()).unwrap_or(0);
    println!(
        "lights layer: {:.1} % of overview raw bytes, {:.1} % of overview gz bytes ({:.2} MB of {:.2} MB)",
        100.0 * s.lights_raw as f64 / s.overview_raw.max(1) as f64,
        100.0 * (ov_gz.saturating_sub(s.overview_gz_without_lights)) as f64 / ov_gz.max(1) as f64,
        (ov_gz.saturating_sub(s.overview_gz_without_lights)) as f64 / 1e6,
        ov_gz as f64 / 1e6
    );
    println!(
        "archive: {:.2} MB vs source {:.2} MB ({:.1} %)",
        written as f64 / 1e6,
        src_len as f64 / 1e6,
        100.0 * written as f64 / src_len.max(1) as f64
    );
}
