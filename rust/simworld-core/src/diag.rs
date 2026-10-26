//! Host-supplied diagnostics: the [`Log`] sink and the [`Diagnostics`] switches.
//!
//! The core never prints and never reads environment variables. Each host maps its own logging
//! (Godot: `godot_print!` / `godot_warn!` / `godot_error!`; CLI: stderr) onto [`Log`] and its own
//! switches (Godot: `SIMPLANE_PROFILE=1`; CLI: flags) onto [`Diagnostics`].

use std::time::Instant;

/// Log sink provided by the host. Called on the thread that drives [`crate::World`] (the host's
/// main thread); worker threads report through the result channel instead.
pub trait Log: Send + Sync {
    fn info(&self, msg: &str);
    fn warn(&self, msg: &str);
    fn error(&self, msg: &str);
}

/// Discards everything (tests, hosts that don't care).
#[derive(Debug, Default, Clone, Copy)]
pub struct NullLog;

impl Log for NullLog {
    fn info(&self, _: &str) {}
    fn warn(&self, _: &str) {}
    fn error(&self, _: &str) {}
}

/// Default for [`Diagnostics::warnings_per_second`].
pub const DEFAULT_WARNINGS_PER_SECOND: u32 = 5;

/// Diagnostic switches, fixed at construction of a [`crate::World`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostics {
    /// Log a phase breakdown (via [`Log::info`]) for main-thread calls slower than 1 ms.
    pub profile: bool,
    /// At most this many [`Log::warn`] per second; the rest are counted and summarized.
    pub warnings_per_second: u32,
}

impl Default for Diagnostics {
    fn default() -> Self {
        Self { profile: false, warnings_per_second: DEFAULT_WARNINGS_PER_SECOND }
    }
}

/// One-second window warning limiter.
#[derive(Debug, Default)]
pub(crate) struct WarnLimiter {
    window: Option<Instant>,
    in_window: u32,
    suppressed: u32,
}

impl WarnLimiter {
    pub(crate) fn warn(&mut self, log: &dyn Log, per_second: u32, msg: &str) {
        let now = Instant::now();
        match self.window {
            Some(t) if now.duration_since(t).as_secs_f32() < 1.0 => {}
            _ => {
                if self.suppressed > 0 {
                    log.warn(&format!("WorldStreamer: {} more warnings suppressed", self.suppressed));
                }
                self.window = Some(now);
                self.in_window = 0;
                self.suppressed = 0;
            }
        }
        if self.in_window < per_second {
            self.in_window += 1;
            log.warn(msg);
        } else {
            self.suppressed += 1;
        }
    }
}

/// CPU time consumed by the calling thread (µs); 0 where unavailable. Profiling only: wall
/// time above CPU time means the main thread was preempted, not working.
pub fn thread_cpu_us() -> u64 {
    #[cfg(unix)]
    {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: valid out-pointer; CLOCK_THREAD_CPUTIME_ID is supported on macOS/iOS/Linux.
        if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) } == 0 {
            return ts.tv_sec as u64 * 1_000_000 + ts.tv_nsec as u64 / 1000;
        }
    }
    0
}
