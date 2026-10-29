//! Helpers: deterministic shuffle, disk use, percentiles and the stats line.

use std::fs;
use std::path::Path;
use std::time::Duration;

pub(super) struct Rng(pub(super) u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub(super) fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            v.swap(i, (self.next() % (i as u64 + 1)) as usize);
        }
    }
}

/// (apparent bytes, allocated bytes, files) of everything under `p`.
pub(super) fn disk_use(p: &Path) -> (u64, u64, u64) {
    let Ok(md) = fs::symlink_metadata(p) else { return (0, 0, 0) };
    #[cfg(unix)]
    let alloc = {
        use std::os::unix::fs::MetadataExt;
        md.blocks() * 512
    };
    #[cfg(not(unix))]
    let alloc = md.len();
    if md.is_dir() {
        let mut t = (0, alloc, 0);
        for e in fs::read_dir(p).into_iter().flatten().flatten() {
            let (a, b, c) = disk_use(&e.path());
            t = (t.0 + a, t.1 + b, t.2 + c);
        }
        t
    } else {
        (md.len(), alloc, 1)
    }
}

/// Database + `-wal` + `-shm`.
pub(super) fn db_use(db: &Path) -> (u64, u64, u64) {
    let mut t = (0, 0, 0);
    for suffix in ["", "-wal", "-shm"] {
        let mut s = db.as_os_str().to_owned();
        s.push(suffix);
        let (a, b, c) = disk_use(Path::new(&s));
        t = (t.0 + a, t.1 + b, t.2 + c);
    }
    t
}

pub(super) fn mb(b: u64) -> f64 {
    b as f64 / 1_048_576.0
}

fn pct(v: &mut [Duration], p: f64) -> Duration {
    v.sort();
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

pub(super) fn stats(name: &str, mut v: Vec<Duration>) {
    let total: Duration = v.iter().sum();
    println!(
        "    {name:<34} n={:<6} p50={:>7.1} us  p99={:>7.1} us  max={:>8.1} us  total={:>8.1} ms",
        v.len(),
        pct(&mut v, 0.5).as_secs_f64() * 1e6,
        pct(&mut v, 0.99).as_secs_f64() * 1e6,
        pct(&mut v, 1.0).as_secs_f64() * 1e6,
        total.as_secs_f64() * 1e3
    );
}

pub(super) fn fresh(dir: &Path) {
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir).unwrap();
}
