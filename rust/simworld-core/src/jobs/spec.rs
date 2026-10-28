//! The job pool's public types: ids, priorities, specs, job ends and context, configuration
//! and counters.

use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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
    pub(super) id: JobId,
    pub(super) version: u64,
    pub(super) cancel: Arc<AtomicBool>,
    pub(super) deps: Vec<Output>,
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

pub(super) type Run = Box<dyn FnOnce(&JobCtx) -> JobEnd + Send>;
pub(super) type OnCancel = Box<dyn FnOnce() + Send>;
/// Thread setup hook: called on each worker thread before its first job, with its index.
pub type ThreadStart = Arc<dyn Fn(usize) + Send + Sync>;

/// Pool configuration.
#[derive(Clone)]
pub struct PoolConfig {
    pub threads: usize,
    pub name: String,
    /// Platform thread setup (iOS QoS later). `None` = nothing.
    pub on_thread_start: Option<ThreadStart>,
}

impl PoolConfig {
    pub fn new(threads: usize, name: &str) -> Self {
        Self { threads: threads.max(1), name: name.to_string(), on_thread_start: None }
    }
}

/// Most worker threads the automatic count picks (`streaming.worker_threads` = 0).
pub const AUTO_THREADS_MAX: usize = 8;

/// Worker count for profile `streaming.worker_threads`: the value itself, or with 0 (auto)
/// cores - 2, at least 2 and at most [`AUTO_THREADS_MAX`] (desktop; a mobile host passes its
/// own count). Perf review 2 item 2: the old clamp to 4 left 6 of a 10-core machine idle while
/// R2 / R3 chunks waited 0.4-1.9 s for a build slot.
pub fn worker_threads(configured: u32) -> usize {
    if configured > 0 {
        return configured as usize;
    }
    auto_threads(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4))
}

/// The automatic worker count for `cores` logical cores.
pub fn auto_threads(cores: usize) -> usize {
    cores.saturating_sub(2).clamp(2, AUTO_THREADS_MAX)
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
