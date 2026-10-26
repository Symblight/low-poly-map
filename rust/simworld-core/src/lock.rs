//! Exclusive `cache_dir` folder lock (contract lifecycle rules, api 0.7).
//!
//! One world per cache folder: `World::configure_world` takes an exclusive lock on
//! `{cache_dir}/.simworld.lock` synchronously, before any asynchronous archive or cache open.
//! The lock is an OS file lock (`std::fs::File::try_lock`: `flock` on macOS/iOS/Linux,
//! `LockFileEx` on Windows). It is held per open file handle, so two worlds in one process
//! exclude each other just like two processes do, and the OS drops it if the process dies.
//! No dependency, and independent of the cache implementation behind it.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

/// Name of the lock file inside `cache_dir`.
pub const LOCK_FILE_NAME: &str = ".simworld.lock";

#[derive(Debug)]
pub enum LockError {
    /// Another world (this process or another one) holds the folder.
    InUse,
    /// The folder or lock file could not be created / locked (read-only volume, no locking
    /// support, ...).
    Io(io::Error),
}

/// A held folder lock; released on drop.
#[derive(Debug)]
pub struct FolderLock {
    file: File,
    /// Canonical path of the locked folder (compared to decide whether a re-configure keeps it).
    dir: PathBuf,
}

/// Create `dir` if missing and return its canonical path (the identity of a cache folder).
pub fn canonical_dir(dir: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    dir.canonicalize()
}

impl FolderLock {
    /// Try to lock `dir` (canonical, existing) without blocking.
    pub fn try_acquire(dir: &Path) -> Result<Self, LockError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(LOCK_FILE_NAME))
            .map_err(LockError::Io)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { file, dir: dir.to_path_buf() }),
            Err(TryLockError::WouldBlock) => Err(LockError::InUse),
            Err(TryLockError::Error(e)) => Err(LockError::Io(e)),
        }
    }

    /// Canonical path of the locked folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Drop for FolderLock {
    fn drop(&mut self) {
        // Closing the handle releases it too; unlock explicitly so the release does not depend
        // on when the OS closes the descriptor.
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lock_on_the_same_folder_fails_until_released() {
        let dir = std::env::temp_dir().join(format!("simworld-lock-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let d = canonical_dir(&dir).unwrap();
        let a = FolderLock::try_acquire(&d).unwrap();
        assert!(matches!(FolderLock::try_acquire(&d), Err(LockError::InUse)));
        assert!(d.join(LOCK_FILE_NAME).exists());
        drop(a);
        let b = FolderLock::try_acquire(&d).unwrap();
        assert_eq!(b.dir(), d);
        drop(b);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
