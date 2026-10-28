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
//!   the dependant is submitted; an unknown id cancels the dependant at once.
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
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use ahash::AHashMap;

/// Job id: the generation number of a request (unique among the pool's pending jobs).
pub type JobId = u64;

/// Output a job hands to its dependants.
pub type Output = Arc<dyn Any + Send + Sync>;

/// Every `OnDemand` job runs before any `Prefetch` job.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Urgency {
    /// Wanted for display now (a fresh tile of the cover).
    OnDemand = 0,
    /// Wanted soon or as a refresh (re-deliveries, look-ahead, prefetch).
    Prefetch = 1,
}

/// Queue order of a job: urgency, then `order` (smaller first; non-finite = last), then FIFO.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Priority {
    pub urgency: Urgency,
    pub order: f64,
}

impl Priority {
    pub fn new(urgency: Urgency, order: f64) -> Self {
        Self { urgency, order }
    }
    pub fn on_demand(order: f64) -> Self {
        Self::new(Urgency::OnDemand, order)
    }
    pub fn prefetch(order: f64) -> Self {
        Self::new(Urgency::Prefetch, order)
    }
}

/// What to run.
#[derive(Clone, Debug)]
pub struct JobSpec {
    pub id: JobId,
    pub priority: Priority,
    /// Hash of the job's inputs (reserved for the marker reload; passed through).
    pub version: u64,
    /// Jobs whose outputs this one needs (must be pending when this one is submitted).
    pub deps: Vec<JobId>,
}

impl JobSpec {
    pub fn new(id: JobId, priority: Priority) -> Self {
        Self { id, priority, version: 0, deps: Vec::new() }
    }
    pub fn with_deps(mut self, deps: Vec<JobId>) -> Self {
        self.deps = deps;
        self
    }
    pub fn with_version(mut self, version: u64) -> Self {
        self.version = version;
        self
    }
}

/// How a job's `run` ended.
pub enum JobEnd {
    /// Finished; the output (if any) goes to the dependants.
    Done(Option<Output>),
    /// Finished with an error the job already reported; dependants are cancelled.
    Failed,
    /// Saw the cancel flag and stopped: the pool runs `on_cancel`.
    Cancelled,
}

/// Handed to a running job.
pub struct JobCtx {
    id: JobId,
    version: u64,
    cancel: Arc<AtomicBool>,
    deps: Vec<Output>,
}

impl JobCtx {
    pub fn id(&self) -> JobId {
        self.id
    }
    pub fn version(&self) -> u64 {
        self.version
    }
    /// The job was cancelled (stale generation or shutdown): stop at the next safe point and
    /// return [`JobEnd::Cancelled`].
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    /// A cheap, cloneable cancel probe for code below the job (long loops).
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }
    /// Outputs of the dependencies, in [`JobSpec::deps`] order.
    pub fn deps(&self) -> &[Output] {
        &self.deps
    }
    pub fn dep<T: 'static>(&self, i: usize) -> Option<&T> {
        self.deps.get(i).and_then(|o| o.downcast_ref::<T>())
    }
}

type Run = Box<dyn FnOnce(&JobCtx) -> JobEnd + Send>;
type OnCancel = Box<dyn FnOnce() + Send>;
/// Thread setup hook: called on each worker thread before its first job, with its index.
pub type ThreadStart = Arc<dyn Fn(usize) + Send + Sync>;

/// Pool configuration.
#[derive(Clone)]
pub struct PoolConfig {
    pub threads: usize,
    pub name: String,
    /// Platform thread setup (iOS QoS later). `None` = nothing.
    pub on_thread_start: Option<ThreadStart>,
    /// How long an idle worker polls for a new job before it sleeps on the condition variable
    /// (a job arriving within it starts without a wake-up; rayon spins similarly). 0 = sleep at
    /// once (battery-sensitive platforms).
    pub idle_spin: Duration,
}

impl PoolConfig {
    pub fn new(threads: usize, name: &str) -> Self {
        Self { threads: threads.max(1), name: name.to_string(), on_thread_start: None, idle_spin: DEFAULT_IDLE_SPIN }
    }
}

/// Default [`PoolConfig::idle_spin`].
pub const DEFAULT_IDLE_SPIN: Duration = Duration::from_micros(100);

/// Default worker count: cores - 2, clamped to 2..=4 (desktop; the measured spike setting).
pub fn default_threads() -> usize {
    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    cpus.saturating_sub(2).clamp(2, 4)
}

/// Counters for `debug_stats`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct JobStats {
    /// Ready (in the heap) plus waiting on dependencies.
    pub queued: usize,
    pub running: usize,
    /// Jobs that ended through `on_cancel` since the pool started.
    pub cancelled: u64,
}

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

#[derive(Default)]
struct State {
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
    /// Bumped whenever a job enters the heap (idle workers poll it before they sleep).
    pushes: AtomicU64,
    idle_spin: Duration,
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
            pushes: AtomicU64::new(0),
            idle_spin: cfg.idle_spin,
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
        let deps_known = spec
            .deps
            .iter()
            .all(|d| *d != id && (st.ready.contains_key(d) || st.waiting.contains_key(d) || st.running.contains_key(d)));
        if st.shutdown || dup || !deps_known {
            drop(st);
            self.inner.cancelled.fetch_add(1, Ordering::Relaxed);
            on_cancel();
            return false;
        }
        st.seq += 1;
        let seq = st.seq;
        let ndeps = spec.deps.len();
        let deps = spec.deps.clone();
        let p = Pending { spec, seq, run: Box::new(run), on_cancel: Box::new(on_cancel), missing: ndeps, outputs: vec![None; ndeps] };
        if ndeps == 0 {
            push_ready(&self.inner, &mut st, p);
            drop(st);
            self.inner.cv.notify_one();
        } else {
            for (i, d) in deps.into_iter().enumerate() {
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

fn push_ready(inner: &Inner, st: &mut State, p: Pending) {
    inner.pushes.fetch_add(1, Ordering::Release);
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
            if !inner.idle_spin.is_zero() {
                // Poll briefly without the lock: a job submitted right away starts at once.
                let seen = inner.pushes.load(Ordering::Acquire);
                drop(st);
                let t0 = Instant::now();
                while inner.pushes.load(Ordering::Acquire) == seen && t0.elapsed() < inner.idle_spin {
                    std::thread::yield_now();
                }
                st = lock(&inner.state);
                if !st.heap.is_empty() || st.shutdown {
                    continue;
                }
            }
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
                for (w, i) in r.waiters.drain(..) {
                    let Some(p) = st.waiting.get_mut(&w) else { continue };
                    p.outputs[i] = out.clone();
                    p.missing -= 1;
                    if p.missing == 0 {
                        let p = st.waiting.remove(&w).expect("present");
                        push_ready(inner, &mut st, p);
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
mod tests {
    use super::*;
    use crossbeam_channel::{unbounded, Receiver};

    fn pool(n: usize) -> JobPool {
        JobPool::new(PoolConfig::new(n, "test-jobs")).unwrap()
    }

    /// A job that blocks its worker until `gate` is dropped / sent to.
    fn blocker(p: &JobPool, id: JobId) -> crossbeam_channel::Sender<()> {
        let (tx, rx) = unbounded::<()>();
        let (started_tx, started_rx) = unbounded::<()>();
        p.submit(
            JobSpec::new(id, Priority::on_demand(0.0)),
            move |_| {
                let _ = started_tx.send(());
                let _ = rx.recv();
                JobEnd::Done(None)
            },
            || {},
        );
        started_rx.recv_timeout(Duration::from_secs(5)).expect("blocker started");
        tx
    }

    fn wait_for(rx: &Receiver<(JobId, &'static str)>, n: usize) -> Vec<(JobId, &'static str)> {
        (0..n).map(|_| rx.recv_timeout(Duration::from_secs(5)).expect("job end")).collect()
    }

    fn record(p: &JobPool, spec: JobSpec, tx: &crossbeam_channel::Sender<(JobId, &'static str)>) {
        let id = spec.id;
        let (a, b) = (tx.clone(), tx.clone());
        p.submit(
            spec,
            move |_| {
                let _ = a.send((id, "run"));
                JobEnd::Done(None)
            },
            move || {
                let _ = b.send((id, "cancel"));
            },
        );
    }

    #[test]
    fn pool_size_is_the_configured_thread_count() {
        assert_eq!(pool(3).threads(), 3);
        assert_eq!(JobPool::new(PoolConfig::new(0, "z")).unwrap().threads(), 1);
        let d = default_threads();
        assert!((2..=4).contains(&d), "cores - 2 clamped to 2..=4: {d}");
        // The thread start hook runs once per thread.
        let n = Arc::new(AtomicU64::new(0));
        let n2 = n.clone();
        let mut cfg = PoolConfig::new(4, "hook");
        cfg.on_thread_start = Some(Arc::new(move |_| {
            n2.fetch_add(1, Ordering::SeqCst);
        }));
        let p = JobPool::new(cfg).unwrap();
        assert!(p.shutdown(Duration::from_secs(2)));
        assert_eq!(n.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn on_demand_overtakes_prefetch_then_order_then_fifo() {
        let p = pool(1);
        let gate = blocker(&p, 1000);
        let (tx, rx) = unbounded();
        record(&p, JobSpec::new(1, Priority::prefetch(0.0)), &tx);
        record(&p, JobSpec::new(2, Priority::prefetch(-5.0)), &tx);
        record(&p, JobSpec::new(3, Priority::on_demand(900.0)), &tx);
        record(&p, JobSpec::new(4, Priority::on_demand(10.0)), &tx);
        record(&p, JobSpec::new(5, Priority::on_demand(10.0)), &tx);
        record(&p, JobSpec::new(6, Priority::on_demand(f64::NAN)), &tx);
        assert_eq!(p.stats().queued, 6);
        assert_eq!(p.stats().running, 1);
        drop(gate);
        let order: Vec<JobId> = wait_for(&rx, 6).into_iter().map(|(id, _)| id).collect();
        assert_eq!(order, vec![4, 5, 3, 6, 2, 1]);
    }

    #[test]
    fn cancel_removes_queued_jobs_and_stops_running_ones() {
        let p = pool(1);
        let (tx, rx) = unbounded();
        // A running job that loops until cancelled (a safe point per iteration).
        let (started_tx, started_rx) = unbounded::<()>();
        let (a, b) = (tx.clone(), tx.clone());
        p.submit(
            JobSpec::new(1, Priority::on_demand(0.0)),
            move |ctx| {
                let _ = started_tx.send(());
                let t0 = Instant::now();
                while !ctx.is_cancelled() {
                    assert!(t0.elapsed() < Duration::from_secs(5));
                    std::thread::yield_now();
                }
                let _ = a.send((1, "run-stopped"));
                JobEnd::Cancelled
            },
            move || {
                let _ = b.send((1, "cancel"));
            },
        );
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        record(&p, JobSpec::new(2, Priority::on_demand(0.0)), &tx);
        record(&p, JobSpec::new(3, Priority::on_demand(0.0)), &tx);
        p.cancel(&[2, 99]);
        assert_eq!(wait_for(&rx, 1), vec![(2, "cancel")], "queued job ends at once");
        p.cancel(&[1]);
        let ends = wait_for(&rx, 3);
        assert_eq!(ends, vec![(1, "run-stopped"), (1, "cancel"), (3, "run")]);
        assert_eq!(p.stats(), JobStats { queued: 0, running: 0, cancelled: 2 });
        // The same id can be submitted again after its end (a new request of the tile).
        record(&p, JobSpec::new(2, Priority::on_demand(0.0)), &tx);
        assert_eq!(wait_for(&rx, 1), vec![(2, "run")]);
    }

    #[test]
    fn dependent_runs_after_its_dependency_with_its_output() {
        let p = pool(3);
        let (tx, rx) = unbounded::<(JobId, &'static str)>();
        let gate = blocker(&p, 500);
        let (dep_go_tx, dep_go_rx) = unbounded::<()>();
        let (started_tx, started_rx) = unbounded::<()>();
        let a = tx.clone();
        p.submit(
            JobSpec::new(1, Priority::prefetch(0.0)),
            move |_| {
                let _ = started_tx.send(());
                let _ = dep_go_rx.recv();
                let _ = a.send((1, "run"));
                JobEnd::Done(Some(Arc::new(41u32) as Output))
            },
            || {},
        );
        let (b, c) = (tx.clone(), tx.clone());
        p.submit(
            JobSpec::new(2, Priority::on_demand(0.0)).with_deps(vec![1]).with_version(7),
            move |ctx| {
                assert_eq!(ctx.version(), 7);
                assert_eq!(ctx.dep::<u32>(0), Some(&41));
                let _ = b.send((2, "run"));
                JobEnd::Done(None)
            },
            move || {
                let _ = c.send((2, "cancel"));
            },
        );
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(p.stats(), JobStats { queued: 1, running: 2, cancelled: 0 }, "the dependant waits");
        std::thread::sleep(Duration::from_millis(50));
        assert!(rx.try_recv().is_err(), "nothing ran before the dependency");
        dep_go_tx.send(()).unwrap();
        assert_eq!(wait_for(&rx, 2), vec![(1, "run"), (2, "run")]);
        drop(gate);
        // Unknown dependency: cancelled at once.
        record(&p, JobSpec::new(3, Priority::on_demand(0.0)).with_deps(vec![12345]), &tx);
        assert_eq!(wait_for(&rx, 1), vec![(3, "cancel")]);
    }

    #[test]
    fn cancelling_a_dependency_cancels_its_dependants() {
        let p = pool(1);
        let (tx, rx) = unbounded();
        let gate = blocker(&p, 500);
        record(&p, JobSpec::new(1, Priority::on_demand(0.0)), &tx);
        record(&p, JobSpec::new(2, Priority::on_demand(0.0)).with_deps(vec![1]), &tx);
        record(&p, JobSpec::new(3, Priority::on_demand(0.0)).with_deps(vec![2]), &tx);
        record(&p, JobSpec::new(4, Priority::on_demand(0.0)), &tx);
        p.cancel(&[1]);
        let mut ends = wait_for(&rx, 3);
        ends.sort();
        assert_eq!(ends, vec![(1, "cancel"), (2, "cancel"), (3, "cancel")]);
        drop(gate);
        assert_eq!(wait_for(&rx, 1), vec![(4, "run")]);
        assert_eq!(p.stats().cancelled, 3);
        // A dependency that is running when cancelled: the dependant ends at once, the
        // dependency at its next safe point.
        let (go_tx, go_rx) = unbounded::<()>();
        let (started_tx, started_rx) = unbounded::<()>();
        let a = tx.clone();
        p.submit(
            JobSpec::new(10, Priority::on_demand(0.0)),
            move |ctx| {
                let _ = started_tx.send(());
                let _ = go_rx.recv();
                if ctx.is_cancelled() {
                    return JobEnd::Cancelled;
                }
                let _ = a.send((10, "run"));
                JobEnd::Done(None)
            },
            {
                let b = tx.clone();
                move || {
                    let _ = b.send((10, "cancel"));
                }
            },
        );
        record(&p, JobSpec::new(11, Priority::on_demand(0.0)).with_deps(vec![10]), &tx);
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        p.cancel(&[10]);
        assert_eq!(wait_for(&rx, 1), vec![(11, "cancel")]);
        go_tx.send(()).unwrap();
        assert_eq!(wait_for(&rx, 1), vec![(10, "cancel")]);
    }

    #[test]
    fn a_panicking_job_still_ends_once_and_fails_its_dependants() {
        let p = pool(1);
        let (tx, rx) = unbounded();
        let b = tx.clone();
        p.submit(
            JobSpec::new(1, Priority::on_demand(0.0)),
            |_| panic!("boom"),
            move || {
                let _ = b.send((1, "cancel"));
            },
        );
        record(&p, JobSpec::new(2, Priority::on_demand(0.0)).with_deps(vec![1]), &tx);
        let mut ends = wait_for(&rx, 2);
        ends.sort();
        assert_eq!(ends, vec![(1, "cancel"), (2, "cancel")]);
        record(&p, JobSpec::new(3, Priority::on_demand(0.0)), &tx);
        assert_eq!(wait_for(&rx, 1), vec![(3, "run")], "the thread survived");
    }

    #[test]
    fn shutdown_cancels_everything_and_never_deadlocks() {
        let p = pool(2);
        let (tx, rx) = unbounded();
        let g1 = blocker(&p, 100);
        let g2 = blocker(&p, 101);
        for i in 0..50 {
            record(&p, JobSpec::new(i, Priority::on_demand(i as f64)), &tx);
        }
        // Running jobs are stuck: the bounded wait gives up and reports it.
        assert!(!p.shutdown(Duration::from_millis(50)));
        let ends = wait_for(&rx, 50);
        assert!(ends.iter().all(|e| e.1 == "cancel"));
        // Submitting after shutdown cancels at once.
        record(&p, JobSpec::new(77, Priority::on_demand(0.0)), &tx);
        assert_eq!(wait_for(&rx, 1), vec![(77, "cancel")]);
        drop((g1, g2));
        assert!(p.shutdown(Duration::from_secs(2)), "threads exit once their jobs end");
        assert!(p.shutdown(Duration::from_millis(1)), "twice is fine");
        // Dropping a busy pool does not block.
        let q = pool(1);
        let _g = blocker(&q, 1);
        let t0 = Instant::now();
        drop(q);
        assert!(t0.elapsed() < Duration::from_millis(500));
    }
}
