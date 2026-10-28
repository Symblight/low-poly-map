//! Streaming latency per band (contract `debug_stats()["latency"]`, api 1.3): how long each
//! stage of a tile's way from "wanted" to "delivered" took, over the last [`WINDOW`] tiles of
//! each band. Main thread only; fixed-size buffers (about 15 KB), nothing allocates per tile.
//!
//! Stages of a fresh delivery (re-deliveries of a held tile are not sampled):
//! `queue` wanted -> job dispatched (waiting for a fetch slot, or a mesh slot for the synthetic
//! city), `fetch_disk` / `fetch_net` fetch job (disk cache hit / HTTP range request), `mesh_wait`
//! fetched -> mesh job dispatched, `mesh` mesh job wall time (rayon queue + build), `cpu` the
//! worker's own build time, `deliver` ready -> handed to the host (waiting for the parent to be
//! effective, for the host's per-frame poll budget, finer bands first), `total` wanted ->
//! delivered. `swap` is sampled on the coarse tile: split decided -> refine fade started (all 4
//! children uploaded), i.e. how long the coarse tile stood in for its children.

use std::time::{Duration, Instant};

use crate::rings::RING_COUNT;
use crate::fetch::Source;

/// Samples kept per stage and band (the newest win).
pub const WINDOW: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Queue = 0,
    FetchDisk,
    FetchNet,
    MeshWait,
    Mesh,
    Cpu,
    Deliver,
    Total,
    Swap,
}

pub const STAGES: [Stage; 9] = [
    Stage::Queue,
    Stage::FetchDisk,
    Stage::FetchNet,
    Stage::MeshWait,
    Stage::Mesh,
    Stage::Cpu,
    Stage::Deliver,
    Stage::Total,
    Stage::Swap,
];

impl Stage {
    pub fn name(self) -> &'static str {
        match self {
            Stage::Queue => "queue",
            Stage::FetchDisk => "fetch_disk",
            Stage::FetchNet => "fetch_net",
            Stage::MeshWait => "mesh_wait",
            Stage::Mesh => "mesh",
            Stage::Cpu => "cpu",
            Stage::Deliver => "deliver",
            Stage::Total => "total",
            Stage::Swap => "swap",
        }
    }
}

/// Timestamps of one tile on its way to the host (reset when it is wanted again).
#[derive(Clone, Copy, Debug, Default)]
pub struct TileTimes {
    pub queued: Option<Instant>,
    pub fetch_start: Option<Instant>,
    pub fetched: Option<Instant>,
    pub source: Option<Source>,
    pub decode_start: Option<Instant>,
    pub ready: Option<Instant>,
    pub cpu_us: u64,
}

impl TileTimes {
    pub fn wanted(now: Instant) -> Self {
        Self { queued: Some(now), ..Self::default() }
    }
}

/// p50 / p95 / max (ms) and the sample count of one stage and band.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Summary {
    pub p50: f32,
    pub p95: f32,
    pub max: f32,
    pub n: u32,
}

#[derive(Clone, Copy)]
struct Window {
    ms: [f32; WINDOW],
    len: usize,
    next: usize,
}

impl Default for Window {
    fn default() -> Self {
        Self { ms: [0.0; WINDOW], len: 0, next: 0 }
    }
}

impl Window {
    fn push(&mut self, ms: f32) {
        self.ms[self.next] = ms;
        self.next = (self.next + 1) % WINDOW;
        self.len = (self.len + 1).min(WINDOW);
    }

    fn summary(&self) -> Summary {
        if self.len == 0 {
            return Summary::default();
        }
        let mut v = self.ms;
        let s = &mut v[..self.len];
        s.sort_unstable_by(f32::total_cmp);
        let at = |q: f32| s[(((s.len() - 1) as f32) * q).round() as usize];
        Summary { p50: at(0.5), p95: at(0.95), max: s[s.len() - 1], n: self.len as u32 }
    }
}

#[derive(Default)]
pub struct Latency {
    win: [[Window; RING_COUNT]; STAGES.len()],
}

fn ms(d: Duration) -> f32 {
    d.as_secs_f32() * 1000.0
}

fn between(a: Option<Instant>, b: Option<Instant>) -> Option<f32> {
    Some(ms(b?.saturating_duration_since(a?)))
}

impl Latency {
    pub fn record(&mut self, stage: Stage, ring: usize, ms: f32) {
        if ring < RING_COUNT && ms.is_finite() {
            self.win[stage as usize][ring].push(ms.max(0.0));
        }
    }

    /// Samples every stage of a fresh delivery at `delivered`.
    pub fn record_delivery(&mut self, ring: usize, t: &TileTimes, delivered: Instant) {
        // The synthetic city has no fetch: its queue ends at the mesh dispatch.
        let dispatched = t.fetch_start.or(t.decode_start);
        if let Some(v) = between(t.queued, dispatched) {
            self.record(Stage::Queue, ring, v);
        }
        if let Some(v) = between(t.fetch_start, t.fetched) {
            let stage = if matches!(t.source, Some(Source::Network)) { Stage::FetchNet } else { Stage::FetchDisk };
            self.record(stage, ring, v);
        }
        if let Some(v) = between(t.fetched, t.decode_start) {
            self.record(Stage::MeshWait, ring, v);
        }
        if let Some(v) = between(t.decode_start, t.ready) {
            self.record(Stage::Mesh, ring, v);
            self.record(Stage::Cpu, ring, t.cpu_us as f32 / 1000.0);
        }
        if let Some(v) = between(t.ready, Some(delivered)) {
            self.record(Stage::Deliver, ring, v);
        }
        if let Some(v) = between(t.queued, Some(delivered)) {
            self.record(Stage::Total, ring, v);
        }
    }

    pub fn summary(&self, stage: Stage, ring: usize) -> Summary {
        self.win[stage as usize][ring].summary()
    }

    /// One line per stage with samples: `total p50/p95/max ms R0 .. R3`, for logs and the CLI.
    pub fn report(&self) -> String {
        let mut out = String::from("latency ms p50/p95/max (n) by band:");
        for st in STAGES {
            if (0..RING_COUNT).all(|r| self.win[st as usize][r].len == 0) {
                continue;
            }
            out.push_str(&format!("\n  {:<10}", st.name()));
            for r in 0..RING_COUNT {
                let s = self.summary(st, r);
                if s.n == 0 {
                    out.push_str(&format!(" | R{r} -"));
                } else {
                    out.push_str(&format!(" | R{r} {:.0}/{:.0}/{:.0} ({})", s.p50, s.p95, s.max, s.n));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_percentiles_and_wrap() {
        let mut l = Latency::default();
        assert_eq!(l.summary(Stage::Total, 0), Summary::default());
        for i in 1..=100 {
            l.record(Stage::Total, 2, i as f32);
        }
        let s = l.summary(Stage::Total, 2);
        assert_eq!((s.p50, s.p95, s.max, s.n), (51.0, 95.0, 100.0, 100));
        // Older samples drop out once the window is full.
        for _ in 0..WINDOW {
            l.record(Stage::Total, 2, 7.0);
        }
        let s = l.summary(Stage::Total, 2);
        assert_eq!((s.p50, s.max, s.n), (7.0, 7.0, WINDOW as u32));
        l.record(Stage::Total, 9, 1.0); // out-of-range band ignored
        l.record(Stage::Total, 1, f32::NAN); // non-finite ignored
        assert_eq!(l.summary(Stage::Total, 1).n, 0);
    }

    #[test]
    fn delivery_stages() {
        let t0 = Instant::now();
        let at = |ms: u64| Some(t0 + Duration::from_millis(ms));
        let t = TileTimes {
            queued: at(0),
            fetch_start: at(10),
            fetched: at(30),
            source: Some(Source::Network),
            decode_start: at(35),
            ready: at(85),
            cpu_us: 40_000,
        };
        let mut l = Latency::default();
        l.record_delivery(3, &t, t0 + Duration::from_millis(100));
        let p = |st| l.summary(st, 3).p50.round();
        assert_eq!(
            [p(Stage::Queue), p(Stage::FetchNet), p(Stage::MeshWait), p(Stage::Mesh), p(Stage::Cpu), p(Stage::Deliver), p(Stage::Total)],
            [10.0, 20.0, 5.0, 50.0, 40.0, 15.0, 100.0]
        );
        assert_eq!(l.summary(Stage::FetchDisk, 3).n, 0);
        // Synthetic: no fetch, the queue ends at the mesh dispatch.
        let s = TileTimes { queued: at(0), decode_start: at(25), ready: at(30), ..TileTimes::default() };
        let mut l = Latency::default();
        l.record_delivery(0, &s, t0 + Duration::from_millis(40));
        assert_eq!(l.summary(Stage::Queue, 0).p50.round(), 25.0);
        assert_eq!(l.summary(Stage::MeshWait, 0).n, 0);
        assert!(l.report().contains("total"));
    }
}
