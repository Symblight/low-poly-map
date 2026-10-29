//! Timed runs: fills and reads through the old and the new cache.

use std::path::Path;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use simworld_core::fetch::DiskCache;
use simworld_core::geo::TileKey;

use crate::old_cache::OldCache;

pub(super) const URL: &str = "http://127.0.0.1:8787/vilnius.pmtiles";

pub(super) fn new_open(db: &Path, cap: u64) -> DiskCache {
    DiskCache::open(db.to_path_buf(), cap, Arc::new(AtomicU64::new(0))).expect("open sqlite cache")
}

pub(super) fn old_put_all(rt: &tokio::runtime::Runtime, c: &Arc<OldCache>, tiles: &[(TileKey, Bytes)]) -> Vec<Duration> {
    rt.block_on(async {
        let mut lat = Vec::with_capacity(tiles.len());
        for (k, b) in tiles {
            let (c, k, b) = (c.clone(), *k, b.clone());
            let t = Instant::now();
            tokio::task::spawn_blocking(move || c.put(k, &b)).await.unwrap().unwrap();
            lat.push(t.elapsed());
        }
        lat
    })
}

pub(super) fn new_put_all(c: &DiskCache, tiles: &[(TileKey, Bytes)]) -> Vec<Duration> {
    let mut lat = Vec::with_capacity(tiles.len());
    for (k, b) in tiles {
        let t = Instant::now();
        c.put(*k, b.clone());
        lat.push(t.elapsed());
    }
    lat
}

pub(super) fn old_reads(rt: &tokio::runtime::Runtime, c: &Arc<OldCache>, keys: &[TileKey]) -> (Vec<Duration>, usize) {
    rt.block_on(async {
        let (mut lat, mut hits) = (Vec::with_capacity(keys.len()), 0);
        for k in keys {
            let (c, k) = (c.clone(), *k);
            let t = Instant::now();
            let r = tokio::task::spawn_blocking(move || c.get(k)).await.unwrap();
            lat.push(t.elapsed());
            hits += r.is_some() as usize;
        }
        (lat, hits)
    })
}

pub(super) fn new_reads(rt: &tokio::runtime::Runtime, c: &DiskCache, keys: &[TileKey]) -> (Vec<Duration>, usize) {
    rt.block_on(async {
        let (mut lat, mut hits) = (Vec::with_capacity(keys.len()), 0);
        for k in keys {
            let t = Instant::now();
            let r = c.get(*k).await;
            lat.push(t.elapsed());
            hits += r.is_some() as usize;
        }
        (lat, hits)
    })
}

/// The tile set repeated under shifted x until it holds at least `bytes` (real size mix).
pub(super) fn inflate(tiles: &[(TileKey, Bytes)], bytes: u64) -> Vec<(TileKey, Bytes)> {
    let mut out = Vec::new();
    let mut total = 0u64;
    let mut pass = 0u32;
    while total < bytes {
        for (k, b) in tiles {
            let shift = pass << (k.z - 6); // 1/64 of the world width per pass: no overlap
            out.push((TileKey::new(k.z, k.x + shift, k.y), b.clone()));
            total += b.len() as u64;
            if total >= bytes {
                break;
            }
        }
        pass += 1;
    }
    out
}
