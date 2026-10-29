use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use bytes::Bytes;

use crate::geo::TileKey;

use super::disk::FLUSH_INTERVAL;
use super::sqlite::sidecar;
use super::*;

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("simworld-cache-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn db_in(dir: &Path) -> PathBuf {
    dir.join(cache_file_name("http://127.0.0.1:8787/vilnius.pmtiles"))
}

fn open(path: &Path, cap: u64) -> DiskCache {
    DiskCache::open(path.to_path_buf(), cap, Arc::new(AtomicU64::new(0))).unwrap()
}

/// Run a future to completion (the cache's async calls never need a runtime's I/O driver).
fn block<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
}

fn get(c: &DiskCache, k: TileKey) -> Option<Vec<u8>> {
    block(c.get(k))
}

fn file_bytes(path: &Path) -> u64 {
    [path.to_path_buf(), sidecar(path, "-wal")].iter().filter_map(|p| fs::metadata(p).ok()).map(|m| m.len()).sum()
}

const WAIT: Duration = Duration::from_secs(5);

#[test]
fn put_get_survives_reopen() {
    let dir = tmpdir("reopen");
    let db = db_in(&dir);
    let ctr = Arc::new(AtomicU64::new(0));
    let k = TileKey::new(16, 37370, 20827);
    let absent = TileKey::new(16, 1, 1);
    {
        let c = DiskCache::open(db.clone(), 1 << 20, ctr.clone()).unwrap();
        assert!(get(&c, k).is_none());
        c.put(k, Bytes::from_static(b"hello"));
        c.put(absent, Bytes::new()); // known-absent tile
        assert!(c.flush(WAIT));
        assert_eq!(get(&c, k).as_deref(), Some(&b"hello"[..]));
        assert_eq!(ctr.load(Ordering::Relaxed), 5);
        assert_eq!(c.len(), 2);
        assert!(c.close(WAIT));
    }
    assert!(db.exists() && !sidecar(&db, "-wal").exists(), "close checkpoints and removes the WAL");
    let c = DiskCache::open(db.clone(), 1 << 20, ctr.clone()).unwrap();
    assert_eq!((c.len(), c.total_bytes()), (2, 5));
    assert_eq!(get(&c, k).as_deref(), Some(&b"hello"[..]));
    assert_eq!(get(&c, absent).as_deref(), Some(&b""[..]));
    assert!(get(&c, TileKey::new(16, 1, 2)).is_none());
    assert!(c.close(WAIT));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn pending_put_visible_to_get() {
    let dir = tmpdir("pending");
    let c = open(&db_in(&dir), 1 << 20);
    let k = TileKey::new(15, 100, 200);
    c.put(k, Bytes::from_static(b"fresh"));
    // Not committed yet (250 ms batch), but answered from the pending buffer.
    assert_eq!(get(&c, k).as_deref(), Some(&b"fresh"[..]));
    assert_eq!(c.commit_count(), 0);
    assert_eq!(c.len(), 0, "len counts committed tiles");
    // A newer put of the same key wins.
    c.put(k, Bytes::from_static(b"newer"));
    assert_eq!(get(&c, k).as_deref(), Some(&b"newer"[..]));
    assert!(c.flush(WAIT));
    assert_eq!((c.len(), c.total_bytes()), (1, 5));
    assert!(c.close(WAIT));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn evicts_oldest_by_bytes() {
    let dir = tmpdir("evict");
    let cap = 64 * 1024;
    let c = open(&db_in(&dir), cap);
    let key = |i: u32| TileKey::new(16, 1000 + i, 2000);
    let one_kb = Bytes::from(vec![7u8; 1024]);
    // One second per 10 tiles written; after each 10, tiles 0..10 are read again (kept
    // hot). Written-only tiles age; the hot ones were written first but read last.
    for i in 0..100u32 {
        c.shared.test_clock_s.store(1_000_000 + i as i64 / 10, Ordering::Relaxed);
        c.put(key(i), one_kb.clone());
        if i % 10 == 9 {
            assert!(c.flush(WAIT));
            for h in 0..10 {
                assert!(get(&c, key(h)).is_some(), "hot tile {h} gone after {i} puts");
            }
            assert!(c.flush(WAIT));
            assert!(c.total_bytes() <= cap, "{} > {cap}", c.total_bytes());
        }
    }
    assert!(c.total_bytes() <= cap, "{} > {cap}", c.total_bytes());
    assert!(c.total_bytes() >= cap * 8 / 10, "evicts to 90 %, not more: {}", c.total_bytes());
    assert_eq!(c.len() as u64 * 1024, c.total_bytes());
    // The recently read tiles survived although they were written first; the written-only
    // old ones went first.
    for i in 0..10 {
        assert!(get(&c, key(i)).is_some(), "recently read tile {i} was evicted");
    }
    for i in 10..40 {
        assert!(get(&c, key(i)).is_none(), "old unread tile {i} survived");
    }
    for i in 60..100 {
        assert!(get(&c, key(i)).is_some(), "recent tile {i} was evicted");
    }
    assert!(get(&c, key(99)).is_some());
    assert!(c.close(WAIT));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn file_stays_near_cap_with_random_order_puts() {
    // Tiles in random key order leave B-tree pages partly empty, so the file outgrows the
    // tile bytes; the cache also bounds the pages in use (FILE_OVER_CAP + FILE_SLACK).
    let dir = tmpdir("filebound");
    let db = db_in(&dir);
    let cap = 4u64 << 20;
    // Literal (not the constants), so the test notices if the bound is lost.
    let limit = cap + cap / 20 + (1 << 20);
    let c = open(&db, cap);
    let mut s = 0x2545_f491_4f6c_dd1du64;
    for i in 0..12_000u32 {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        let len = 900 + (s % 1800) as usize;
        c.put(TileKey::new(16, (s >> 20) as u32 & 0xffff, (s >> 40) as u32 & 0xffff), Bytes::from(vec![(i % 251) as u8; len]));
        if i % 1000 == 999 {
            assert!(c.flush(WAIT));
            assert!(c.total_bytes() <= cap, "{} > {cap}", c.total_bytes());
        }
    }
    assert!(c.close(WAIT));
    let file = file_bytes(&db);
    assert!(file <= limit, "file {file} > bound {limit}");
    let c = open(&db, cap);
    assert!(c.total_bytes() >= cap / 2, "bound evicted far too much: {}", c.total_bytes());
    assert!(c.close(WAIT));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn file_shrinks_after_eviction() {
    let dir = tmpdir("shrink");
    let db = db_in(&dir);
    let c = open(&db, 64 << 20);
    for i in 0..400u32 {
        c.put(TileKey::new(16, i, 7), Bytes::from(vec![(i % 251) as u8; 8 * 1024]));
    }
    assert!(c.close(WAIT));
    let full = file_bytes(&db);
    assert!(full >= 400 * 8 * 1024, "{full}");
    // Reopen with half the data's size as cap: evicts to 90 % of it and vacuums.
    let cap = 200 * 8 * 1024;
    let c = open(&db, cap);
    assert!(c.total_bytes() <= cap);
    assert!(c.close(WAIT));
    let after = file_bytes(&db);
    assert!(after < full * 6 / 10, "file did not shrink: {full} -> {after}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn fingerprint_change_wipes() {
    let dir = tmpdir("fingerprint");
    let db = db_in(&dir);
    let ctr = Arc::new(AtomicU64::new(0));
    let k = TileKey::new(16, 5, 6);
    let c = DiskCache::open(db.clone(), 1 << 20, ctr.clone()).unwrap();
    assert!(!block(c.sync_fingerprint("a")).unwrap(), "empty cache: nothing wiped");
    c.put(k, Bytes::from_static(b"old"));
    assert!(c.flush(WAIT));
    assert!(!block(c.sync_fingerprint("a")).unwrap(), "same fingerprint");
    assert!(block(c.sync_fingerprint("b")).unwrap(), "rebuilt archive wipes");
    assert!(get(&c, k).is_none() && c.is_empty());
    assert_eq!(ctr.load(Ordering::Relaxed), 0);
    assert!(!block(c.sync_fingerprint("b")).unwrap());
    // Pending (uncommitted) tiles are wiped too.
    c.put(k, Bytes::from_static(b"x"));
    assert!(block(c.sync_fingerprint("c")).unwrap());
    assert!(get(&c, k).is_none());
    // The fingerprint survives a restart.
    c.put(k, Bytes::from_static(b"new"));
    assert!(c.close(WAIT));
    drop(c);
    let c = open(&db, 1 << 20);
    assert!(!block(c.sync_fingerprint("c")).unwrap());
    assert_eq!(get(&c, k).as_deref(), Some(&b"new"[..]));
    assert!(c.close(WAIT));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn schema_version_change_wipes() {
    let dir = tmpdir("schema");
    let db = db_in(&dir);
    let k = TileKey::new(14, 9, 9);
    let c = open(&db, 1 << 20);
    c.put(k, Bytes::from_static(b"tile"));
    assert!(block(c.sync_fingerprint("fp")).unwrap());
    c.put(k, Bytes::from_static(b"tile"));
    assert!(c.close(WAIT));
    drop(c);
    // Same version: kept.
    let c = open(&db, 1 << 20);
    assert_eq!(c.len(), 1);
    assert!(c.close(WAIT));
    drop(c);
    // Written by the previous version.
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute(
            "UPDATE meta SET v = ?1 WHERE k = 'schema_version'",
            [(CACHE_SCHEMA_VERSION - 1).to_string()],
        )
        .unwrap();
    }
    let c = open(&db, 1 << 20);
    assert!(c.is_empty() && c.total_bytes() == 0);
    assert!(get(&c, k).is_none());
    // The fingerprint went with the tiles; the next sync records it without a wipe.
    assert!(!block(c.sync_fingerprint("fp")).unwrap());
    assert!(c.close(WAIT));
    let conn = rusqlite::Connection::open(&db).unwrap();
    let v: String = conn.query_row("SELECT v FROM meta WHERE k = 'schema_version'", [], |r| r.get(0)).unwrap();
    assert_eq!(v, CACHE_SCHEMA_VERSION.to_string());
    drop(conn);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn corrupt_file_is_recreated() {
    let dir = tmpdir("corrupt");
    let db = db_in(&dir);
    fs::write(&db, vec![0xA5u8; 8192]).unwrap();
    let notes = Arc::new(Mutex::new(Vec::<Note>::new()));
    let n2 = notes.clone();
    let notify: Notify = Arc::new(move |n| n2.lock().unwrap().push(n));
    let c = DiskCache::open_with(db.clone(), 1 << 20, Arc::new(AtomicU64::new(0)), Some(notify)).unwrap();
    assert!(c.is_empty());
    let k = TileKey::new(16, 3, 4);
    c.put(k, Bytes::from_static(b"ok"));
    assert!(c.flush(WAIT));
    assert_eq!(get(&c, k).as_deref(), Some(&b"ok"[..]));
    let notes = notes.lock().unwrap().clone();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(matches!(&notes[0], Note::Info(m) if m.starts_with(MSG_CACHE_RECREATED)), "{notes:?}");
    assert!(c.close(WAIT));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn second_open_is_rejected() {
    let dir = tmpdir("busy");
    let url = "http://127.0.0.1:9/none.pmtiles";
    let db = dir.join(cache_file_name(url));
    let first = open(&db, 1 << 20);
    let k = TileKey::new(16, 1, 2);
    first.put(k, Bytes::from_static(b"held"));
    assert!(first.flush(WAIT));
    let e = DiskCache::open(db.clone(), 1 << 20, Arc::new(AtomicU64::new(0))).err().expect("second open must fail");
    assert_eq!(e.kind(), io::ErrorKind::ResourceBusy, "{e}");
    assert!(e.to_string().contains("in use"), "{e}");
    // A fetcher on the same file runs without a disk cache and warns once.
    let notes = Arc::new(Mutex::new(Vec::<Note>::new()));
    let n2 = notes.clone();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let f = {
        let _g = rt.enter();
        Fetcher::new(url, dir.to_str().unwrap(), Arc::new(AtomicU64::new(0)), FetchParams::default(), Box::new(move |n| n2.lock().unwrap().push(n)))
            .unwrap()
    };
    assert!(rt.block_on(f.disk()).is_none());
    let notes = notes.lock().unwrap().clone();
    assert!(notes.iter().any(|n| matches!(n, Note::Warn(m) if m.contains("in use") && m.contains("without a disk cache"))), "{notes:?}");
    // The first handle is unaffected; once it is closed the file opens again.
    assert_eq!(get(&first, k).as_deref(), Some(&b"held"[..]));
    assert!(first.close(WAIT));
    let again = open(&db, 1 << 20);
    assert_eq!(get(&again, k).as_deref(), Some(&b"held"[..]));
    assert!(again.close(WAIT));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn legacy_folders_removed() {
    let dir = tmpdir("legacy");
    let url = "http://127.0.0.1:8787/vilnius.pmtiles";
    let hash = source_hash(url);
    let db = dir.join(cache_file_name(url));
    let old2 = dir.join(format!("pmtiles-v2-{hash}"));
    let old1 = dir.join(format!("pmtiles-v1-{hash}"));
    let old0 = dir.join(format!("pmtiles-{hash}"));
    let other = dir.join("pmtiles-v2-0123456789abcdef");
    let other0 = dir.join("pmtiles-0123456789abcdef");
    for d in [&old0, &old1, &old2, &other, &other0] {
        fs::create_dir_all(d.join("16/1")).unwrap();
        fs::write(d.join("16/1/2.mvt"), b"x").unwrap();
    }
    let c = open(&db, 1 << 20);
    assert!(!old0.exists() && !old1.exists() && !old2.exists(), "same-source folders are removed");
    assert!(other.exists() && other0.exists(), "other sources are left alone");
    assert!(db.exists());
    assert!(c.close(WAIT));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn reads_do_not_write() {
    let dir = tmpdir("reads");
    let c = open(&db_in(&dir), 1 << 20);
    let keys: Vec<TileKey> = (0..32).map(|i| TileKey::new(16, i, 1)).collect();
    for &k in &keys {
        c.put(k, Bytes::from_static(b"warm"));
    }
    assert!(c.flush(WAIT));
    let commits = c.commit_count();
    assert_eq!(commits, 1);
    // Later reads record a newer access time (in memory only).
    c.shared.test_clock_s.store(i64::from(u32::MAX), Ordering::Relaxed);
    let t0 = Instant::now();
    for _ in 0..4 {
        for &k in &keys {
            assert!(get(&c, k).is_some());
        }
    }
    let burst = t0.elapsed();
    if burst < Duration::from_millis(200) {
        assert_eq!(c.commit_count(), commits, "a read burst committed before the flush interval");
    }
    // The access times go out with the next timed batch: exactly one commit.
    std::thread::sleep(FLUSH_INTERVAL + Duration::from_millis(250));
    assert_eq!(c.commit_count(), commits + 1);
    // Reads at the same access time record nothing new.
    for &k in &keys {
        assert!(get(&c, k).is_some());
    }
    assert!(c.flush(WAIT));
    assert_eq!(c.commit_count(), commits + 1);
    assert!(c.close(WAIT));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn repeated_write_failures_disable_the_cache() {
    let dir = tmpdir("fail");
    let notes = Arc::new(Mutex::new(Vec::<Note>::new()));
    let n2 = notes.clone();
    let notify: Notify = Arc::new(move |n| n2.lock().unwrap().push(n));
    let c = DiskCache::open_with(db_in(&dir), 1 << 20, Arc::new(AtomicU64::new(0)), Some(notify)).unwrap();
    let k = TileKey::new(16, 8, 8);
    c.put(k, Bytes::from_static(b"kept"));
    assert!(c.flush(WAIT));
    c.shared.fail_writes.store(true, Ordering::Relaxed);
    for i in 0..2 {
        c.put(TileKey::new(16, 9, i), Bytes::from_static(b"lost"));
        assert!(c.flush(WAIT));
        assert!(!c.is_disabled());
        assert!(get(&c, TileKey::new(16, 9, i)).is_none(), "a failed batch is dropped");
    }
    // A success resets the count.
    c.shared.fail_writes.store(false, Ordering::Relaxed);
    c.put(TileKey::new(16, 9, 5), Bytes::from_static(b"ok"));
    assert!(c.flush(WAIT));
    c.shared.fail_writes.store(true, Ordering::Relaxed);
    for i in 0..3 {
        assert!(!c.is_disabled(), "disabled after {i} consecutive failures");
        c.put(TileKey::new(16, 10, i), Bytes::from_static(b"lost"));
        assert!(c.flush(WAIT));
    }
    assert!(c.is_disabled());
    assert!(get(&c, k).is_none(), "a disabled cache misses");
    let notes = notes.lock().unwrap().clone();
    assert_eq!(notes.iter().filter(|n| matches!(n, Note::CacheDisabled(_))).count(), 1, "{notes:?}");
    assert!(c.close(WAIT));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn cache_files_differ_per_url_and_keys_pack() {
    assert_ne!(cache_file_name("http://a/x.pmtiles"), cache_file_name("http://a/y.pmtiles"));
    assert_eq!(cache_file_name("u"), cache_file_name("u"));
    assert!(cache_file_name("u").starts_with("tiles-") && cache_file_name("u").ends_with(".sqlite"));
    let a = pack_key(TileKey::new(16, 37370, 20827));
    assert_eq!(a, (16i64 << 58) | (37370i64 << 29) | 20827);
    assert_ne!(pack_key(TileKey::new(15, 1, 2)), pack_key(TileKey::new(16, 1, 2)));
    assert_ne!(pack_key(TileKey::new(16, 2, 1)), pack_key(TileKey::new(16, 1, 2)));
    assert!(pack_key(TileKey::new(29, (1 << 29) - 1, (1 << 29) - 1)) > 0);
}

#[test]
fn tileset_contract_check() {
    let meta = r#"{"vector_layers":[{"id":"buildings"},{"id":"landuse"},{"id":"roads"}],"name":"x"}"#;
    assert_eq!(check_tileset(13, 16, meta), Ok(DataLevels::COMPAT));
    assert!(check_tileset(0, 18, meta).is_ok());
    let e = check_tileset(14, 16, meta).unwrap_err();
    assert!(e.contains("z14-16") && e.contains("z13-16"), "{e}");
    assert!(check_tileset(13, 15, meta).is_err());
    let e = check_tileset(13, 16, r#"{"vector_layers":[{"id":"buildings"},{"id":"water"}]}"#).unwrap_err();
    assert!(e.contains("landuse, roads") && e.contains("has: buildings, water"), "{e}");
    assert!(check_tileset(13, 16, "{}").unwrap_err().contains("vector_layers"));
    assert!(check_tileset(13, 16, "not json").is_err());
    // Two-level archives (Tiles and LOD v2): exactly the two zooms, overview layers.
    let v2 = |levels: &str, extra: &str| {
        format!(
            r#"{{"simworld_levels":{levels},"vector_layers":[{{"id":"buildings"}},{{"id":"landuse"}},{{"id":"roads"}},{{"id":"blocks"}},{{"id":"lights"}}{extra}]}}"#
        )
    };
    let full = v2(r#"{"detail":14,"overview":11}"#, r#",{"id":"landmarks"}"#);
    assert_eq!(check_tileset(11, 14, &full), Ok(DataLevels::two_level(14, 11, true)));
    assert_eq!(check_tileset(11, 14, &v2(r#"{"detail":14,"overview":11}"#, "")), Ok(DataLevels::two_level(14, 11, false)));
    assert_eq!(check_tileset(12, 14, &v2(r#"{"detail":14,"overview":12}"#, "")), Ok(DataLevels::two_level(14, 12, false)));
    let e = check_tileset(11, 16, &full).unwrap_err();
    assert!(e.contains("z11-16") && e.contains("exactly z11 and z14"), "{e}");
    assert!(check_tileset(13, 16, &full).is_err(), "levels win over the compat zooms");
    let e = check_tileset(11, 15, &v2(r#"{"detail":15,"overview":11}"#, "")).unwrap_err();
    assert!(e.contains("detail z15"), "{e}");
    let e = check_tileset(11, 14, &v2(r#"{"detail":"x"}"#, "")).unwrap_err();
    assert!(e.contains("simworld_levels must be"), "{e}");
    let e = check_tileset(11, 14, r#"{"simworld_levels":{"detail":14,"overview":11},"vector_layers":[{"id":"buildings"},{"id":"landuse"},{"id":"roads"}]}"#)
        .unwrap_err();
    assert!(e.contains("blocks, lights"), "{e}");
    for l in [DataLevels::COMPAT, DataLevels::two_level(14, 11, true), DataLevels::two_level(14, 12, false)] {
        assert_eq!(parse_levels(&levels_text(l)), Some(l));
    }
    assert_eq!(parse_levels("14,11"), None);
    assert_eq!(FetchParams::default().cache_cap_bytes, 300 * 1024 * 1024);
    assert_eq!(FetchParams::default().open_retry, Duration::from_millis(1500));
}
