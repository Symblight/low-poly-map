//! Freeing big buffers off the main thread (perf review item 1, `update_camera` spikes; perf
//! review 2 item 4, `poll_ready`).
//!
//! A decoded data tile is thousands of small allocations (footprints, rings, roads); the data
//! store drops one when it leaves the cover or the memory budget evicts it, inside
//! `update_camera`, and freeing it there cost 1-4 ms in single frames. The recent-tile cache
//! (perf review 2 item 6) drops kept tile outputs (meshes of up to megabytes) inside
//! `update_camera` too. Both go to [`Worker::release`] instead: one low-priority pool job per
//! call drops them (whatever else still holds an `Arc` keeps it).

use std::sync::atomic::Ordering;

use super::Worker;
use crate::jobs::{JobEnd, JobSpec, Priority};

/// Job ids of release jobs: a tag no tile job id reaches (they count up from 1), plus a counter.
const RELEASE_TAG: u64 = 1 << 62;

impl Worker {
    /// Drop `items` on a pool thread (lowest priority; nothing to do for an empty list). After
    /// shutdown the pool rejects the job and they are dropped here.
    pub fn release<T: Send + 'static>(&self, items: Vec<T>) {
        if items.is_empty() {
            return;
        }
        let tiles = items;
        let id = RELEASE_TAG | (self.release_seq.fetch_add(1, Ordering::Relaxed) & (RELEASE_TAG - 1));
        self.pool.submit(
            JobSpec::new(id, Priority::prefetch(f64::INFINITY)),
            move |_| {
                drop(tiles);
                JobEnd::Done(None)
            },
            || {},
        );
    }
}
