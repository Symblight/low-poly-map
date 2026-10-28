//! Async tile boxes (`boxes`): the cover with probes answered a frame later settles to the
//! inline cover, keeps the host rule every frame, and the first update probes nothing inline.

use super::super::tests::{terrain_harness, terrain_setup, Harness, VILNIUS};
use super::super::Scheduler;
use crate::geo::{Frame, TileKey};
use crate::terrain::sample::probe;

/// Answer the scheduler's probe batch (what `Worker::probe` does on the pool); no-op inline.
pub(in crate::scheduler) fn answer_probes(s: &mut Scheduler) {
    if let Some(b) = s.take_probe_batch() {
        let out = b.keys.iter().map(|&k| (k, Some(probe(&s.terrain, k)))).collect();
        b.reply.send(out).unwrap();
    }
}

/// The cover as (key, split, held), sorted.
fn signature(h: &Harness) -> Vec<(TileKey, bool, bool)> {
    let mut v: Vec<_> = h.s.entries().map(|e| (e.key, e.split, e.held)).collect();
    v.sort();
    v
}

/// Settle at 300 m, fly north at 250 m/s (host rule checked every frame), hover; the cover after
/// each settle.
fn run(h: &mut Harness) -> [Vec<(TileKey, bool, bool)>; 2] {
    h.settle([0.0, 0.0, 300.0], 20_000);
    let a = signature(h);
    let dt = 1.0 / 30.0;
    let mut y = 0.0;
    for _ in 0..450 {
        y += 250.0 * dt;
        let (_, un) = h.frame([0.0, y, 300.0], [0.0, 250.0, 0.0]);
        h.check_frame(&un, "async boxes flight");
    }
    h.settle([0.0, y, 1500.0], 20_000);
    [a, signature(h)]
}

fn vector_harness(asynchronous: bool) -> Harness {
    let (p, t) = terrain_setup(|_| {});
    let mut h = Harness::new();
    h.s = Scheduler::new(Frame::new(VILNIUS.0, VILNIUS.1), false, p).with_terrain(t);
    if asynchronous {
        h.s.enable_async_boxes();
    }
    h
}

fn terrain_layer(asynchronous: bool) -> Harness {
    let (p, t) = terrain_setup(|_| {});
    let mut h = terrain_harness(p, t);
    if asynchronous {
        h.s.enable_async_boxes();
    }
    h
}

#[test]
fn async_boxes_settle_to_the_inline_cover() {
    for (name, make) in [("vector", vector_harness as fn(bool) -> Harness), ("terrain", terrain_layer)] {
        let inline = run(&mut make(false));
        let mut h = make(true);
        let asynchronous = run(&mut h);
        assert!(h.s.box_requests.as_ref().unwrap().requested > 0, "{name}: nothing was probed asynchronously");
        assert_eq!(inline[0], asynchronous[0], "{name}: settled cover at the start");
        assert_eq!(inline[1], asynchronous[1], "{name}: settled cover after the flight");
    }
}

#[test]
fn first_update_probes_nothing_inline() {
    for make in [vector_harness as fn(bool) -> Harness, terrain_layer] {
        let mut h = make(true);
        let mut jobs = Vec::new();
        h.s.update([0.0, 0.0, 300.0], [0.0; 3], std::time::Instant::now(), &mut jobs);
        assert_eq!(h.s.terrain_cache.len(), 0, "a box was probed on the main thread");
        assert!(h.s.proxy.as_ref().is_none_or(|p| p.is_empty()), "an error proxy was probed on the main thread");
        assert!(h.s.entries.is_empty() && jobs.is_empty());
        assert!(h.s.probes_pending() > 0);
    }
}

/// A terrain plan that waited on a probe is reused until the next probe batch arrives (perf
/// review 3 item 2), and every reuse equals a fresh plan (`verify_plan_reuse`). Probe answers
/// are held back for 4 frames, so plans wait across frames; the host rule holds every frame and
/// the cover settles to the inline one.
#[test]
fn a_waiting_plan_is_reused_until_a_probe_arrives() {
    let inline = run(&mut terrain_layer(false));
    let mut h = terrain_layer(true);
    h.s.verify_plan_reuse = true;
    let mut held: Vec<Vec<super::ProbeResult>> = Vec::new();
    let (mut frame, mut reused_waiting) = (0u32, 0u64);
    let mut step = |h: &mut Harness, cam: [f64; 3], vel: [f64; 3]| {
        let before = h.s.plan_reuse().0;
        let waiting = h.s.probes_pending() > 0;
        let (_, un) = h.frame(cam, vel);
        h.check_frame(&un, "held probes");
        if waiting && h.s.plan_reuse().0 > before {
            reused_waiting += 1;
        }
        // Take this frame's answers back out of the channel; release them every 4th frame.
        let r = h.s.box_requests.as_ref().unwrap();
        held.extend(r.results.try_iter());
        frame += 1;
        if frame % 4 == 0 {
            for b in held.drain(..) {
                r.reply.send(b).unwrap();
            }
        }
    };
    let mut y = 0.0f64;
    for _ in 0..600 {
        step(&mut h, [0.0, 0.0, 300.0], [0.0; 3]);
    }
    for _ in 0..450 {
        y += 250.0 / 30.0;
        step(&mut h, [0.0, y, 300.0], [0.0, 250.0, 0.0]);
    }
    for _ in 0..1200 {
        step(&mut h, [0.0, y, 1500.0], [0.0; 3]);
    }
    let (reused, planned) = h.s.plan_reuse();
    eprintln!("held probes: {reused} plans reused ({reused_waiting} while probes were pending), {planned} computed");
    assert!(reused_waiting > 0, "a plan waiting on a probe was never reused");
    assert_eq!(signature(&h), inline[1], "settled cover after the flight");
}
