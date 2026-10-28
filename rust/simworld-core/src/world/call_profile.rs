//! `diagnostics.profile` lines for slow main-thread calls (perf review 3 item 4).
//!
//! Every host call the profile watches logs one line when it took more than [`SLOW_CALL_US`] of
//! wall time, with the calling thread's CPU time next to it: wall time well above CPU time means
//! the thread was preempted or waited (the OS, a lock), CPU time close to wall time means work
//! inside the call. `update_camera` and `poll_ready` add their phase breakdown; the binding
//! wraps `poll_fades`, `poll_unload`, `debug_stats` and `lod_state` (type conversion included)
//! with [`World::call_timer`] / [`World::end_call`].

use std::time::Instant;

use super::World;
use crate::diag::thread_cpu_us;

/// Calls slower than this (wall, µs) are logged.
pub const SLOW_CALL_US: u128 = 1000;

/// Start of a profiled call: wall clock and thread CPU time.
#[derive(Clone, Copy, Debug)]
pub struct CallTimer {
    start: Instant,
    cpu_us: u64,
}

impl CallTimer {
    pub fn start() -> Self {
        Self { start: Instant::now(), cpu_us: thread_cpu_us() }
    }

    /// (wall µs, thread CPU µs) since the start.
    pub fn elapsed_us(&self) -> (u128, u64) {
        (self.start.elapsed().as_micros(), thread_cpu_us().saturating_sub(self.cpu_us))
    }

    /// Wall µs from the start to `t`.
    pub fn us_until(&self, t: Instant) -> u128 {
        t.duration_since(self.start).as_micros()
    }
}

impl World {
    /// A timer for a host call when `diagnostics.profile` is on (`None` otherwise: no clock
    /// reads).
    pub fn call_timer(&self) -> Option<CallTimer> {
        self.diag.profile.then(CallTimer::start)
    }

    /// End of the call `name` started with [`World::call_timer`]: one profile line if it was
    /// slow (`detail` appended, e.g. a phase breakdown).
    pub fn end_call_with(&self, name: &str, t: Option<CallTimer>, detail: impl FnOnce() -> String) {
        let Some(t) = t else { return };
        let (wall, cpu) = t.elapsed_us();
        if wall > SLOW_CALL_US {
            let d = detail();
            let sep = if d.is_empty() { "" } else { " " };
            self.log.info(&format!("WorldStreamer profile: {name} {wall}us wall, {cpu}us thread CPU{sep}{d}"));
        }
    }

    /// [`World::end_call_with`] without a breakdown.
    pub fn end_call(&self, name: &str, t: Option<CallTimer>) {
        self.end_call_with(name, t, String::new);
    }
}
