//! The old file-per-tile cache (minimal copy; `lru::LruCache` replaced by a seq-ordered map).

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;

use simworld_core::geo::TileKey;

#[derive(Default)]
struct OldInner {
    /// key -> (seq, len); `order` is seq -> key (oldest first).
    map: HashMap<TileKey, (u64, u64)>,
    order: BTreeMap<u64, TileKey>,
    seq: u64,
    total: u64,
}

impl OldInner {
    fn touch(&mut self, k: TileKey, len: u64) -> Option<u64> {
        self.seq += 1;
        let old = self.map.insert(k, (self.seq, len));
        if let Some((s, _)) = old {
            self.order.remove(&s);
        }
        self.order.insert(self.seq, k);
        old.map(|(_, l)| l)
    }
}

pub(super) struct OldCache {
    root: PathBuf,
    cap: u64,
    inner: Mutex<OldInner>,
    tmp_counter: AtomicU64,
}

impl OldCache {
    pub(super) fn open(root: PathBuf, cap: u64) -> io::Result<Self> {
        fs::create_dir_all(&root)?;
        let mut found: Vec<(SystemTime, TileKey, u64)> = Vec::new();
        for zd in fs::read_dir(&root)?.flatten() {
            let Some(z) = zd.file_name().to_str().and_then(|s| s.parse::<u8>().ok()) else { continue };
            for xd in fs::read_dir(zd.path()).into_iter().flatten().flatten() {
                let Some(x) = xd.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
                for fe in fs::read_dir(xd.path()).into_iter().flatten().flatten() {
                    let name = fe.file_name();
                    let Some(name) = name.to_str() else { continue };
                    if let Some(y) = name.strip_suffix(".mvt").and_then(|s| s.parse::<u32>().ok()) {
                        if let Ok(md) = fe.metadata() {
                            let t = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                            found.push((t, TileKey::new(z, x, y), md.len()));
                        }
                    } else if name.contains(".tmp") {
                        let _ = fs::remove_file(fe.path());
                    }
                }
            }
        }
        found.sort();
        let mut inner = OldInner::default();
        for (_, k, len) in found {
            inner.touch(k, len);
            inner.total += len;
        }
        let c = Self { root, cap, inner: Mutex::new(inner), tmp_counter: AtomicU64::new(0) };
        c.evict();
        Ok(c)
    }

    pub(super) fn len(&self) -> usize {
        self.inner.lock().unwrap().map.len()
    }

    pub(super) fn total_bytes(&self) -> u64 {
        self.inner.lock().unwrap().total
    }

    fn path(&self, k: TileKey) -> PathBuf {
        self.root.join(k.z.to_string()).join(k.x.to_string()).join(format!("{}.mvt", k.y))
    }

    pub(super) fn get(&self, k: TileKey) -> Option<Vec<u8>> {
        {
            let mut i = self.inner.lock().ok()?;
            let len = i.map.get(&k)?.1;
            i.touch(k, len);
        }
        let path = self.path(k);
        match fs::read(&path) {
            Ok(b) => {
                if let Ok(f) = fs::File::options().write(true).open(&path) {
                    let _ = f.set_modified(SystemTime::now());
                }
                Some(b)
            }
            Err(_) => None,
        }
    }

    pub(super) fn put(&self, k: TileKey, data: &[u8]) -> io::Result<()> {
        let path = self.path(k);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let n = self.tmp_counter.fetch_add(1, Ordering::Relaxed);
        let tmp = path.with_extension(format!("mvt.tmp{n}"));
        fs::write(&tmp, data)?;
        fs::rename(&tmp, &path)?;
        if let Ok(mut i) = self.inner.lock() {
            if let Some(old) = i.touch(k, data.len() as u64) {
                i.total -= old;
            }
            i.total += data.len() as u64;
        }
        self.evict();
        Ok(())
    }

    fn evict(&self) {
        let Ok(mut i) = self.inner.lock() else { return };
        while i.total > self.cap {
            let Some((&s, &k)) = i.order.iter().next() else { break };
            i.order.remove(&s);
            if let Some((_, len)) = i.map.remove(&k) {
                i.total -= len;
            }
            let _ = fs::remove_file(self.path(k));
        }
    }
}
