//! `diagnostics.profile` summary of the per-tile main-thread cost of `poll_ready` (perf
//! review 2 item 4): the thread CPU time of each tile's callback (Packed arrays and the
//! dictionary in the Godot binding) and of dropping the tile, as p50 / p95 / max and triangles
//! per tile, logged every
//! `LATENCY_LOG_EVERY` with the latency table. Thread CPU, so a preempted frame on a busy
//! machine does not read as a slow conversion (the per-call wall-time line stays for those).

/// Samples since the last summary: (CPU microseconds, triangles).
#[derive(Debug, Default)]
pub(super) struct PollProfile {
    samples: Vec<(u32, u32)>,
}

impl PollProfile {
    pub fn record(&mut self, cpu_us: u64, tris: usize) {
        self.samples.push((cpu_us.min(u32::MAX as u64) as u32, tris.min(u32::MAX as usize) as u32));
    }

    /// The summary line of the samples so far (and start over); `None` without samples.
    pub fn take_summary(&mut self) -> Option<String> {
        if self.samples.is_empty() {
            return None;
        }
        let n = self.samples.len();
        let mut us: Vec<u32> = self.samples.iter().map(|s| s.0).collect();
        us.sort_unstable();
        let at = |q: f64| us[((n - 1) as f64 * q).round() as usize] as f64 / 1000.0;
        let tris = self.samples.iter().map(|s| s.1 as u64).sum::<u64>() / n as u64;
        self.samples.clear();
        Some(format!(
            "poll_ready host conversion per tile (thread CPU): p50 {:.3} / p95 {:.3} / max {:.3} ms over {n} tiles ({tris} triangles per tile on average)",
            at(0.5),
            at(0.95),
            at(1.0)
        ))
    }
}
