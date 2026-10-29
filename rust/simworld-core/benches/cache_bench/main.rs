//! Disk tile cache benchmark: the SQLite `DiskCache` (one file per source) against the old
//! file-per-tile cache it replaced (docs/spec-tile-cache.md, "Benchmark and acceptance").
//!
//! Run from `rust/` (release build, needs `data/out/vilnius.pmtiles`, see `data/README.md`):
//!
//! ```text
//! cargo bench -p simworld-core --bench cache_bench
//! ```
//!
//! Environment: `SIMWORLD_BENCH_PMTILES` (archive path, default `../data/out/vilnius.pmtiles`
//! relative to the workspace), `SIMWORLD_BENCH_DIR` (scratch folder, default the system temp
//! folder; it needs about 2 GB free for the 300 MB file-per-tile fill), `SIMWORLD_BENCH_SKIP_300=1`
//! (skip the slow 300 MB fills), `SIMWORLD_BENCH_KEEP=1` (keep the scratch folder).
//!
//! The tile bytes are the archive's own (gzip MVT), read once into memory. `OldCache` is a
//! minimal copy of the file-per-tile `DiskCache` of `simworld-core` 0.5.0 (git `41f414d`,
//! `fetch.rs`): `{root}/{z}/{x}/{y}.mvt`, tmp + rename writes, mtime touched on a hit, LRU index
//! rebuilt by a directory walk on open, eviction by unlink. Caller-side timings use the same
//! call path as `Fetcher::fetch` then and now: the old cache through `spawn_blocking`, the new
//! one through its async `get` / fire-and-forget `put`.

mod old_cache;
mod runs;
mod tiles;
mod util;

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use simworld_core::fetch::cache_file_name;
use simworld_core::geo::TileKey;

use old_cache::OldCache;
use runs::{inflate, new_open, new_put_all, new_reads, old_put_all, old_reads, URL};
use tiles::{flight_order, load_tiles};
use util::{db_use, disk_use, fresh, mb, stats, Rng};

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let archive = std::env::var_os("SIMWORLD_BENCH_PMTILES")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../data/out/vilnius.pmtiles"));
    let scratch = std::env::var_os("SIMWORLD_BENCH_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("simworld-cache-bench-{}", std::process::id()));
    let skip_300 = std::env::var_os("SIMWORLD_BENCH_SKIP_300").is_some();
    // `cargo bench` passes `--bench`; ignore arguments.
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).build().unwrap();

    let tiles = load_tiles(&rt, &archive);
    let total: u64 = tiles.iter().map(|t| t.1.len() as u64).sum();
    let mut by_z = [0usize; 32];
    for (k, _) in &tiles {
        by_z[k.z as usize] += 1;
    }
    println!("tile set {}: {} tiles, {:.1} MB of tile data", archive.display(), tiles.len(), mb(total));
    for (z, n) in by_z.iter().enumerate().filter(|(_, n)| **n > 0) {
        println!("    z{z}: {n}");
    }
    println!("scratch {}", scratch.display());
    let index: HashMap<TileKey, usize> = tiles.iter().enumerate().map(|(i, t)| (t.0, i)).collect();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut fill_order = tiles.clone();
    rng.shuffle(&mut fill_order);
    let mut random_keys: Vec<TileKey> = tiles.iter().map(|t| t.0).collect();
    rng.shuffle(&mut random_keys);
    let flight = flight_order(&index);
    println!("flight order: {} keys (north from the bench origin, 100 m steps)", flight.len());

    let cap300 = 300 * 1_048_576u64;
    let old_dir = scratch.join("old");
    let new_dir = scratch.join("new");
    let db = new_dir.join(cache_file_name(URL));

    // 1. Cold fill, 300 MB cap (no eviction).
    println!("\n[1] cold fill: all tiles, random order, cap 300 MB");
    fresh(&old_dir);
    let t = Instant::now();
    let oc = Arc::new(OldCache::open(old_dir.clone(), cap300).unwrap());
    let lat = old_put_all(&rt, &oc, &fill_order);
    let old_fill = t.elapsed();
    println!("  old: {:.2} s total ({:.1} us/tile)", old_fill.as_secs_f64(), old_fill.as_secs_f64() * 1e6 / tiles.len() as f64);
    stats("old put (spawn_blocking, awaited)", lat);
    fresh(&new_dir);
    let t = Instant::now();
    let nc = new_open(&db, cap300);
    let lat = new_put_all(&nc, &fill_order);
    assert!(nc.flush(Duration::from_secs(120)), "flush");
    let new_fill = t.elapsed();
    println!(
        "  new: {:.2} s total incl. final flush ({:.1} us/tile), {} commits",
        new_fill.as_secs_f64(),
        new_fill.as_secs_f64() * 1e6 / tiles.len() as f64,
        nc.commit_count()
    );
    stats("new put (fire and forget)", lat);
    let (a, b, f) = disk_use(&old_dir);
    println!("  disk, full tile set: old {:.1} MB apparent / {:.1} MB allocated in {f} files", mb(a), mb(b));
    let (a, b, _) = db_use(&db);
    println!("  disk, full tile set: new {:.1} MB apparent / {:.1} MB allocated (db + wal + shm, open)", mb(a), mb(b));

    // 2. Warm random reads (page cache warm for both: the files / db were just written).
    println!("\n[2] warm random reads: every tile once, shuffled");
    let (lat, hits) = old_reads(&rt, &oc, &random_keys);
    println!("  old hits {hits}");
    stats("old get (spawn_blocking)", lat);
    let (lat, hits) = new_reads(&rt, &nc, &random_keys);
    println!("  new hits {hits}");
    stats("new get (async, cache thread)", lat);
    // Direct (no thread hop) for the old cache, as a floor.
    let mut lat = Vec::new();
    for k in &random_keys {
        let t = Instant::now();
        let _ = oc.get(*k);
        lat.push(t.elapsed());
    }
    stats("old get (direct call, no hop)", lat);

    // 3. Warm reads in flight order.
    println!("\n[3] warm reads in flight order");
    let (lat, _) = old_reads(&rt, &oc, &flight);
    stats("old get (spawn_blocking)", lat);
    let (lat, _) = new_reads(&rt, &nc, &flight);
    stats("new get (async, cache thread)", lat);

    // 5. Open time with the full tile set.
    println!("\n[5] open time, full tile set ({} tiles)", tiles.len());
    drop(oc);
    assert!(nc.close(Duration::from_secs(30)));
    drop(nc);
    for run in 0..3 {
        let t = Instant::now();
        let oc = OldCache::open(old_dir.clone(), cap300).unwrap();
        let ot = t.elapsed();
        let t = Instant::now();
        let nc = new_open(&db, cap300);
        let nt = t.elapsed();
        println!(
            "  run {run}: old {:.1} ms ({} tiles), new {:.1} ms ({} tiles)",
            ot.as_secs_f64() * 1e3,
            oc.len(),
            nt.as_secs_f64() * 1e3,
            nc.len()
        );
        nc.close(Duration::from_secs(30));
    }
    let (a, b, _) = db_use(&db);
    println!("  disk after close: new {:.1} MB apparent / {:.1} MB allocated", mb(a), mb(b));

    // 4. Eviction under a 50 MB cap.
    let cap50 = 50 * 1_048_576u64;
    println!("\n[4] eviction: cold fill of all tiles (random order) with a 50 MB cap");
    fresh(&old_dir);
    let oc = Arc::new(OldCache::open(old_dir.clone(), cap50).unwrap());
    let t = Instant::now();
    let lat = old_put_all(&rt, &oc, &fill_order);
    let el = t.elapsed();
    let (a, b, f) = disk_use(&old_dir);
    println!(
        "  old: {:.2} s, {} tiles, {:.1} MB data; disk {:.1} MB apparent / {:.1} MB allocated, {f} files",
        el.as_secs_f64(),
        oc.len(),
        mb(oc.total_bytes()),
        mb(a),
        mb(b)
    );
    stats("old put", lat);
    drop(oc);
    fresh(&new_dir);
    let nc = new_open(&db, cap50);
    let t = Instant::now();
    let lat = new_put_all(&nc, &fill_order);
    assert!(nc.flush(Duration::from_secs(120)));
    let el = t.elapsed();
    let (a, b, _) = db_use(&db);
    println!(
        "  new: {:.2} s incl. flush, {} tiles, {:.1} MB data; disk {:.1} MB apparent / {:.1} MB allocated (open)",
        el.as_secs_f64(),
        nc.len(),
        mb(nc.total_bytes()),
        mb(a),
        mb(b)
    );
    stats("new put", lat);
    // Eviction while reading: random reads of the evicting cache keep working.
    let (lat, hits) = new_reads(&rt, &nc, &random_keys[..5000]);
    println!("  new: {hits}/5000 hits after eviction");
    stats("new get (evicted cache)", lat);
    nc.close(Duration::from_secs(30));
    let (a, b, _) = db_use(&db);
    println!("  new after close: {:.1} MB apparent / {:.1} MB allocated", mb(a), mb(b));

    // 6. A full 300 MB cache: disk use and open time (tile set repeated under shifted x).
    if skip_300 {
        println!("\n[6] skipped (SIMWORLD_BENCH_SKIP_300)");
    } else {
        let big = inflate(&fill_order, cap300 * 5 / 2);
        let n_full = {
            let mut acc = 0u64;
            big.iter().take_while(|t| {
                acc += t.1.len() as u64;
                acc <= cap300 + cap300 / 20
            })
            .count()
        };
        println!(
            "\n[6] full 300 MB cache: old gets the first {} tiles (315 MB written), new all {} tiles ({:.0} MB written, steady-state churn); cap 300 MB",
            n_full,
            big.len(),
            mb(big.iter().map(|t| t.1.len() as u64).sum())
        );
        fresh(&old_dir);
        let oc = Arc::new(OldCache::open(old_dir.clone(), cap300).unwrap());
        let t = Instant::now();
        let _ = old_put_all(&rt, &oc, &big[..n_full]);
        println!("  old fill {:.1} s: {} tiles, {:.1} MB data", t.elapsed().as_secs_f64(), oc.len(), mb(oc.total_bytes()));
        let (a, b, f) = disk_use(&old_dir);
        println!("  old disk: {:.1} MB apparent / {:.1} MB allocated, {f} files + dirs", mb(a), mb(b));
        drop(oc);
        fresh(&new_dir);
        let nc = new_open(&db, cap300);
        let t = Instant::now();
        let (mut max_file, mut max_data) = (0u64, 0u64);
        let mut written = 0u64;
        for chunk in big.chunks(4096) {
            let _ = new_put_all(&nc, chunk);
            assert!(nc.flush(Duration::from_secs(300)));
            written += chunk.iter().map(|t| t.1.len() as u64).sum::<u64>();
            let (a, _, _) = db_use(&db);
            max_file = max_file.max(a);
            max_data = max_data.max(nc.total_bytes());
            if written >= cap300 * 9 / 10 {
                println!(
                    "    after {:>6.1} MB written: data {:>6.1} MB ({} tiles), file + wal {:>6.1} MB",
                    mb(written),
                    mb(nc.total_bytes()),
                    nc.len(),
                    mb(a)
                );
            }
        }
        println!(
            "  new fill {:.1} s; peak data {:.1} MB, peak file + wal {:.1} MB",
            t.elapsed().as_secs_f64(),
            mb(max_data),
            mb(max_file)
        );
        let (a, b, _) = db_use(&db);
        println!("  new disk (open, after flush): {:.1} MB apparent / {:.1} MB allocated (db + wal + shm)", mb(a), mb(b));
        nc.close(Duration::from_secs(60));
        let (a, b, _) = db_use(&db);
        println!("  new disk (closed): {:.1} MB apparent / {:.1} MB allocated", mb(a), mb(b));
        for run in 0..3 {
            let t = Instant::now();
            let oc = OldCache::open(old_dir.clone(), cap300).unwrap();
            let ot = t.elapsed();
            let t = Instant::now();
            let nc = new_open(&db, cap300);
            let nt = t.elapsed();
            println!(
                "  open run {run}: old {:.1} ms ({} tiles), new {:.1} ms ({} tiles)",
                ot.as_secs_f64() * 1e3,
                oc.len(),
                nt.as_secs_f64() * 1e3,
                nc.len()
            );
            nc.close(Duration::from_secs(30));
        }
    }

    if std::env::var_os("SIMWORLD_BENCH_KEEP").is_none() {
        let _ = fs::remove_dir_all(&scratch);
    }
    println!("\ndone");
}
