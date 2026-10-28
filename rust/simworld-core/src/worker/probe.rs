//! Cold tile-box probes off the main thread (perf review 2, item 1; `scheduler::boxes`).
//!
//! A scheduler in async-box mode collects the tiles whose terrain box or error proxy is not
//! memoized and hands them out as a [`ProbeBatch`]; the world passes it here. The batch is cut
//! into pool jobs of [`KEYS_PER_JOB`] keys at the top priority (the cover waits for them, the
//! mesh jobs do not); each job replies with its probes. A job the pool drops (shutdown) replies
//! `None` per key, so the scheduler may ask again.

use std::sync::atomic::Ordering;

use super::Worker;
use crate::jobs::{JobEnd, JobSpec, Priority};
use crate::scheduler::ProbeBatch;
use crate::terrain::sample::probe;

/// Job ids of probe jobs: a tag no tile job id reaches (below the release tag), plus a counter.
const PROBE_TAG: u64 = 1 << 61;

/// Keys per pool job (about 0.1 ms each: a job of 1-2 ms, spread over the threads).
const KEYS_PER_JOB: usize = 12;

impl Worker {
    /// Probe `batch` on the CPU pool (see the module docs).
    pub fn probe(&self, batch: ProbeBatch) {
        for keys in batch.keys.chunks(KEYS_PER_JOB) {
            let id = PROBE_TAG | (self.release_seq.fetch_add(1, Ordering::Relaxed) & (PROBE_TAG - 1));
            let (keys, dropped) = (keys.to_vec(), keys.to_vec());
            let (reply, reply_dropped) = (batch.reply.clone(), batch.reply.clone());
            let terrain = self.terrain.clone();
            self.pool.submit(
                JobSpec::new(id, Priority::on_demand(f64::NEG_INFINITY)),
                move |_| {
                    let _ = reply.send(keys.iter().map(|&k| (k, Some(probe(&terrain, k)))).collect());
                    JobEnd::Done(None)
                },
                move || {
                    let _ = reply_dropped.send(dropped.into_iter().map(|k| (k, None)).collect());
                },
            );
        }
    }
}
