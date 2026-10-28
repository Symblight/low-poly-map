//! One CPU job scheduler for every layer (Terrain v2 spec section 5 "Jobs and threads",
//! Tiles v2 spec sections 2 and 5): a priority queue in front of a fixed pool of worker threads.
//! It replaced the rayon pool of [`crate::worker`] (rayon has no priorities and no cancel).
//!
//! - **Priority:** ready jobs sit in one binary heap keyed by ([`Urgency`], `order`, submission
//!   sequence): every on-demand job before any prefetch job, then the smaller `order` first
//!   (the caller passes today's delivery order, the slant distance of the tile's sibling group),
//!   then FIFO. The vector and terrain layers share the heap.
//! - **Generations / cancellation:** a job id is the generation number of the tile's request (the
//!   scheduler gives a tile a new id whenever an old result would be stale). [`JobPool::cancel`]
//!   marks ids stale: a queued or waiting job is removed at once, a running job sees
//!   [`JobCtx::is_cancelled`] at its next safe point and ends with [`JobEnd::Cancelled`] (the
//!   worker checks before it starts and between build stages). A stale result is never handed
//!   on by the pool; it is counted in [`JobStats::cancelled`].
//! - **Exactly one end per job:** either the job's `run` returns `Done` / `Failed` (the job
//!   delivered its own reply), or its `on_cancel` runs (cancelled while queued, waiting or
//!   running, a dependency failed or was cancelled, the pool shut down, or `run` panicked). The
//!   tile scheduler's in-flight counters rely on that.
//! - **Dependencies:** a job may wait on the outputs of other jobs (Tiles v2: chunk jobs wait on
//!   their DataTile decode). It becomes ready when every dependency ended `Done`; their outputs
//!   are in [`JobCtx::deps`] in the order given. A dependency that fails or is cancelled cancels
//!   its dependants (recursively). A dependency must be pending (queued, waiting or running) when
//!   the dependant is submitted, or have ended `Done` recently with an output that is still
//!   alive (a weak reference to the last [`RECENT_OUTPUTS`] outputs is kept: a chunk submitted
//!   just after its data decode finished takes the decoded tile instead of being cancelled and
//!   redone, review L1); any other unknown id cancels the dependant at once.
//! - **Version:** every job carries `version` (a hash of its inputs), reserved for the marker
//!   reload (a new version is built beside the old one); the pool only passes it through.
//! - **Threads:** [`default_threads`] (cores - 2, clamped to 2..=4 as before). `on_thread_start`
//!   is the hook for a platform thread setup (iOS: QoS class of the thread group; not built).
//!
//! Pure Rust, no Godot, no global state: every `World` owns its pool. Results do not depend on
//! the scheduling order (jobs are pure functions of their inputs).

use std::any::Any;
use std::cmp::Ordering as CmpOrdering;
use std::collections::BinaryHeap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use ahash::AHashMap;

mod spec;
pub use spec::*;
use spec::{OnCancel, Run};

struct Pending {
    spec: JobSpec,
    seq: u64,
    run: Run,
    on_cancel: OnCancel,
    /// Dependencies not yet done; outputs collected by index.
    missing: usize,
    outputs: Vec<Option<Output>>,
}

struct Running {
    cancel: Arc<AtomicBool>,
    on_cancel: Option<OnCancel>,
    /// Dependants waiting on this job: (dependant id, dependency index).
    waiters: Vec<(JobId, usize)>,
}

#[derive(PartialEq)]
struct HeapItem {
    urgency: Urgency,
    order: f64,
    seq: u64,
    id: JobId,
}

impl Eq for HeapItem {}

impl Ord for HeapItem {
    /// Max-heap: the "greatest" item pops first, so the comparison is reversed.
    fn cmp(&self, o: &Self) -> CmpOrdering {
        let ord = |x: f64| if x.is_nan() { f64::INFINITY } else { x };
        o.urgency
            .cmp(&self.urgency)
            .then(ord(o.order).total_cmp(&ord(self.order)))
            .then(o.seq.cmp(&self.seq))
    }
}

impl PartialOrd for HeapItem {
    fn partial_cmp(&self, o: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(o))
    }
}

/// Outputs of jobs that ended `Done` remembered (weakly) for dependants submitted late.
pub const RECENT_OUTPUTS: usize = 64;

#[derive(Default)]
struct State {
    /// The last [`RECENT_OUTPUTS`] `Done` outputs, weak (they keep nothing alive).
    recent: VecDeque<(JobId, Weak<dyn Any + Send + Sync>)>,
    heap: BinaryHeap<HeapItem>,
    /// Ready jobs by id (heap items whose `seq` no longer matches are stale and skipped).
    ready: AHashMap<JobId, Pending>,
    /// Jobs waiting on dependencies.
    waiting: AHashMap<JobId, Pending>,
    /// Waiters registered on a pending dependency that is itself queued or waiting:
    /// dependency id -> (dependant id, index).
    waiters: AHashMap<JobId, Vec<(JobId, usize)>>,
    running: AHashMap<JobId, Running>,
    seq: u64,
    shutdown: bool,
    live_threads: usize,
}

struct Inner {
    state: Mutex<State>,
    /// Workers wait here for jobs; `shutdown` waits here for the threads to exit.
    cv: Condvar,
    exit_cv: Condvar,
    cancelled: AtomicU64,
}

fn lock(m: &Mutex<State>) -> MutexGuard<'_, State> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The job pool. Dropping it signals the threads to stop (queued jobs are cancelled; running
/// jobs see their cancel flag) without waiting; [`JobPool::shutdown`] waits, bounded.
pub struct JobPool {
    inner: Arc<Inner>,
    threads: usize,
}

impl JobPool {
    pub fn new(cfg: PoolConfig) -> Result<Self, String> {
        let inner = Arc::new(Inner {
            state: Mutex::new(State::default()),
            cv: Condvar::new(),
            exit_cv: Condvar::new(),
            cancelled: AtomicU64::new(0),
        });
        let n = cfg.threads.max(1);
        for i in 0..n {
            let inner2 = inner.clone();
            let hook = cfg.on_thread_start.clone();
            lock(&inner.state).live_threads += 1;
            let spawned = std::thread::Builder::new().name(format!("{}-{i}", cfg.name)).spawn(move || {
                if let Some(h) = hook {
                    h(i);
                }
                worker_loop(&inner2);
            });
            if let Err(e) = spawned {
                lock(&inner.state).live_threads -= 1;
                let pool = JobPool { inner, threads: i };
                pool.signal_stop();
                return Err(format!("job pool thread: {e}"));
            }
        }
        Ok(Self { inner, threads: n })
    }

    /// Worker thread count.
    pub fn threads(&self) -> usize {
        self.threads
    }

    /// Queue a job. Exactly one of `run` (returning `Done` / `Failed`) or `on_cancel` is called,
    /// on a pool thread or (for a job cancelled before it ran) on the calling thread of
    /// `submit` / `cancel` / `shutdown`. Returns false (and calls `on_cancel`) if the id is
    /// already pending, a dependency is unknown or failed, or the pool is shut down.
    pub fn submit(
        &self,
        spec: JobSpec,
        run: impl FnOnce(&JobCtx) -> JobEnd + Send + 'static,
        on_cancel: impl FnOnce() + Send + 'static,
    ) -> bool {
        let mut st = lock(&self.inner.state);
        let id = spec.id;
        let dup = st.ready.contains_key(&id) || st.waiting.contains_key(&id) || st.running.contains_key(&id);
        // Per dependency: pending (`None`), or done with a live output (`Some`).
        let mut done: Vec<Option<Output>> = Vec::with_capacity(spec.deps.len());
        let mut deps_known = true;
        for d in &spec.deps {
            if *d == id {
                deps_known = false;
            } else if st.ready.contains_key(d) || st.waiting.contains_key(d) || st.running.contains_key(d) {
                done.push(None);
            } else if let Some(out) = st.recent.iter().rev().find(|(r, _)| r == d).and_then(|(_, w)| w.upgrade()) {
                done.push(Some(out));
            } else {
                deps_known = false;
            }
        }
        if st.shutdown || dup || !deps_known {
            drop(st);
            self.inner.cancelled.fetch_add(1, Ordering::Relaxed);
            on_cancel();
            return false;
        }
        st.seq += 1;
        let seq = st.seq;
        let deps = spec.deps.clone();
        let missing = done.iter().filter(|o| o.is_none()).count();
        let p = Pending { spec, seq, run: Box::new(run), on_cancel: Box::new(on_cancel), missing, outputs: done.clone() };
        if missing == 0 {
            push_ready(&mut st, p);
            drop(st);
            self.inner.cv.notify_one();
        } else {
            for (i, d) in deps.into_iter().enumerate() {
                if done[i].is_some() {
                    continue;
                }
                match st.running.get_mut(&d) {
                    Some(r) => r.waiters.push((id, i)),
                    None => st.waiters.entry(d).or_default().push((id, i)),
                }
            }
            st.waiting.insert(id, p);
        }
        true
    }

    /// Mark these generations stale: queued / waiting jobs end through `on_cancel` now (with
    /// their dependants), running jobs at their next safe point. Unknown ids are ignored.
    pub fn cancel(&self, ids: &[JobId]) {
        if ids.is_empty() {
            return;
        }
        let mut out = Vec::new();
        {
            let mut st = lock(&self.inner.state);
            for &id in ids {
                cancel_one(&mut st, id, &mut out);
            }
        }
        self.finish_cancelled(out);
    }

    pub fn stats(&self) -> JobStats {
        let st = lock(&self.inner.state);
        JobStats {
            queued: st.ready.len() + st.waiting.len(),
            running: st.running.len(),
            cancelled: self.inner.cancelled.load(Ordering::Relaxed),
        }
    }

    fn finish_cancelled(&self, out: Vec<OnCancel>) {
        self.inner.cancelled.fetch_add(out.len() as u64, Ordering::Relaxed);
        for f in out {
            let _ = catch_unwind(AssertUnwindSafe(f));
        }
    }

    /// Stop accepting jobs, cancel every queued and waiting job, flag the running ones.
    fn signal_stop(&self) {
        let mut out = Vec::new();
        {
            let mut st = lock(&self.inner.state);
            if !st.shutdown {
                st.shutdown = true;
                let ids: Vec<JobId> =
                    st.ready.keys().chain(st.waiting.keys()).chain(st.running.keys()).copied().collect();
                for id in ids {
                    cancel_one(&mut st, id, &mut out);
                }
            }
        }
        self.inner.cv.notify_all();
        self.finish_cancelled(out);
    }

    /// Stop and wait up to `wait` for the worker threads to exit (a running job finishes its
    /// current stage first). Returns whether they all exited in time; the rest exit on their own.
    pub fn shutdown(&self, wait: Duration) -> bool {
        self.signal_stop();
        let deadline = Instant::now() + wait;
        let mut st = lock(&self.inner.state);
        while st.live_threads > 0 {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            st = self.inner.exit_cv.wait_timeout(st, deadline - now).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
        }
        true
    }
}

impl Drop for JobPool {
    fn drop(&mut self) {
        self.signal_stop();
    }
}

fn push_ready(st: &mut State, p: Pending) {
    st.heap.push(HeapItem { urgency: p.spec.priority.urgency, order: p.spec.priority.order, seq: p.seq, id: p.spec.id });
    st.ready.insert(p.spec.id, p);
}

/// Cancel `id` (queued, waiting or running) and every job waiting on it; collects the
/// `on_cancel`s to run outside the lock.
fn cancel_one(st: &mut State, id: JobId, out: &mut Vec<OnCancel>) {
    let mut stack = vec![id];
    while let Some(id) = stack.pop() {
        if let Some(p) = st.ready.remove(&id).or_else(|| st.waiting.remove(&id)) {
            out.push(p.on_cancel);
        } else if let Some(r) = st.running.get_mut(&id) {
            // Stops at its next safe point (`JobEnd::Cancelled` -> `on_cancel`); a `Done` that
            // slipped through was already replied and is dropped by the receiver's id check.
            r.cancel.store(true, Ordering::Relaxed);
            stack.extend(r.waiters.drain(..).map(|(w, _)| w));
            continue;
        } else {
            continue;
        }
        if let Some(ws) = st.waiters.remove(&id) {
            stack.extend(ws.into_iter().map(|(w, _)| w));
        }
    }
}

fn worker_loop(inner: &Inner) {
    let mut st = lock(&inner.state);
    loop {
        if st.shutdown {
            break;
        }
        let Some(item) = st.heap.pop() else {
            // Idle: block on the condition variable (no polling; `submit` notifies one worker
            // per ready job, `shutdown` all).
            st = inner.cv.wait(st).unwrap_or_else(|e| e.into_inner());
            continue;
        };
        if st.ready.get(&item.id).is_none_or(|p| p.seq != item.seq) {
            continue; // cancelled while queued (stale heap entry)
        }
        let p = st.ready.remove(&item.id).expect("checked");
        let id = p.spec.id;
        let cancel = Arc::new(AtomicBool::new(false));
        let waiters = st.waiters.remove(&id).unwrap_or_default();
        st.running.insert(id, Running { cancel: cancel.clone(), on_cancel: Some(p.on_cancel), waiters });
        drop(st);

        let ctx = JobCtx { id, version: p.spec.version, cancel, deps: p.outputs.into_iter().flatten().collect() };
        let run = p.run;
        let end = catch_unwind(AssertUnwindSafe(|| run(&ctx))).unwrap_or(JobEnd::Cancelled);
        let stale = ctx.is_cancelled();
        drop(ctx);

        st = lock(&inner.state);
        let mut r = st.running.remove(&id).expect("running");
        let mut cancelled = Vec::new();
        match end {
            JobEnd::Done(out) if !stale => {
                if let Some(o) = &out {
                    if st.recent.len() >= RECENT_OUTPUTS {
                        st.recent.pop_front();
                    }
                    st.recent.push_back((id, Arc::downgrade(o)));
                }
                for (w, i) in r.waiters.drain(..) {
                    let Some(p) = st.waiting.get_mut(&w) else { continue };
                    p.outputs[i] = out.clone();
                    p.missing -= 1;
                    if p.missing == 0 {
                        let p = st.waiting.remove(&w).expect("present");
                        push_ready(&mut st, p);
                        inner.cv.notify_one();
                    }
                }
            }
            // `Done` from a job cancelled meanwhile: it already replied (the receiver drops
            // the stale generation); its dependants are cancelled.
            JobEnd::Done(_) | JobEnd::Failed => {
                for (w, _) in r.waiters.drain(..) {
                    cancel_one(&mut st, w, &mut cancelled);
                }
            }
            JobEnd::Cancelled => {
                for (w, _) in r.waiters.drain(..) {
                    cancel_one(&mut st, w, &mut cancelled);
                }
                cancelled.extend(r.on_cancel.take());
            }
        }
        if !cancelled.is_empty() {
            drop(st);
            inner.cancelled.fetch_add(cancelled.len() as u64, Ordering::Relaxed);
            for f in cancelled {
                let _ = catch_unwind(AssertUnwindSafe(f));
            }
            st = lock(&inner.state);
        }
    }
    st.live_threads -= 1;
    drop(st);
    inner.exit_cv.notify_all();
}

#[cfg(test)]
mod tests;
