//! The memoized presence rules equal the plain recursive ones of the contract, frame by frame,
//! on flights of both layers (one memo shared by every question of a frame, as `best_ready`
//! shares it).

use super::super::tests::{terrain_harness, terrain_setup, Harness};
use super::super::Scheduler;
use super::Presence;
use crate::geo::TileKey;
use crate::rings::children;

/// The contract's rules, written out without a memo (the code before item 5).
fn present_plain(s: &Scheduler, k: TileKey) -> bool {
    s.held(k) || (k.z < s.layer.leaf_zoom && children(k).iter().all(|&c| present_plain(s, c)))
}

fn deliverable_plain(s: &Scheduler, k: TileKey) -> bool {
    let effective = |k: TileKey| {
        let mut k = k;
        loop {
            if !present_plain(s, k) {
                return false;
            }
            let Some(p) = s.layer.parent(k) else { return true };
            if !s.layer.siblings(k).iter().all(|&x| present_plain(s, x)) {
                return false;
            }
            k = p;
        }
    };
    k.z == s.layer.root_zoom || s.layer.parent(k).is_some_and(|p| effective(p) && !s.in_fade(p))
}

/// Every tracked tile and its children against the plain rules; returns the questions asked.
fn compare(s: &Scheduler, ctx: &str) -> usize {
    let mut memo = Presence::new();
    let mut keys: Vec<TileKey> = s.entries().map(|e| e.key).collect();
    keys.sort();
    let mut n = 0;
    for k in keys {
        for q in std::iter::once(k).chain(if k.z < s.layer.leaf_zoom { children(k).to_vec() } else { Vec::new() }) {
            assert_eq!(s.present_in(q, &mut memo), present_plain(s, q), "{ctx}: present({q})");
            assert_eq!(s.deliverable_in(q, &mut memo), deliverable_plain(s, q), "{ctx}: deliverable({q})");
            n += 1;
        }
    }
    n
}

fn fly(h: &mut Harness, name: &str) -> usize {
    let mut asked = 0;
    let dt = 1.0 / 30.0;
    let (mut y, mut alt) = (0.0f64, 300.0f64);
    for f in 0..900 {
        let vel = if f < 600 { [0.0, 250.0, 0.0] } else { [0.0, -120.0, 8.0] };
        y += vel[1] * dt;
        alt += vel[2] * dt;
        h.frame([0.0, y, alt], vel);
        asked += compare(&h.s, name);
    }
    asked
}

#[test]
fn memoized_presence_equals_the_plain_rules() {
    let mut v = Harness::new();
    let asked = fly(&mut v, "vector");
    let (p, t) = terrain_setup(|_| {});
    let mut tr = terrain_harness(p, t);
    let asked_t = fly(&mut tr, "terrain");
    assert!(asked > 10_000 && asked_t > 10_000, "{asked} / {asked_t} questions");
}

/// Cost of the deliverable checks of `best_ready` (ignored; release:
/// `cargo test --release -p simworld-core perf_presence -- --ignored --nocapture`): a cold fill at
/// 300 m and a flight on both layers; every frame, every ready tile is checked as `best_ready`
/// does, once with the plain rules and once through one shared memo. Prints the totals.
#[test]
#[ignore]
fn perf_presence_best_ready_checks() {
    use std::time::Instant;
    let run = |h: &mut Harness, name: &str| {
        let (mut plain, mut memo, mut checks, mut worst) = (0.0f64, 0.0f64, 0usize, (0.0f64, 0.0f64));
        let dt = 1.0 / 30.0;
        let mut y = 0.0;
        for f in 0..1200 {
            let vel = if f < 300 { [0.0; 3] } else { [0.0, 250.0, 0.0] };
            y += vel[1] * dt;
            h.frame([0.0, y, 300.0], vel);
            let s = &h.s;
            let ready: Vec<TileKey> = s.ready.clone();
            let t = Instant::now();
            let a = ready.iter().filter(|&&k| deliverable_plain(s, k)).count();
            let tp = t.elapsed().as_secs_f64() * 1e3;
            let t = Instant::now();
            let mut m = Presence::new();
            let b = ready.iter().filter(|&&k| s.deliverable_in(k, &mut m)).count();
            let tm = t.elapsed().as_secs_f64() * 1e3;
            assert_eq!(a, b);
            (plain, memo, checks) = (plain + tp, memo + tm, checks + ready.len());
            worst = (worst.0.max(tp), worst.1.max(tm));
        }
        eprintln!(
            "{name}: {checks} ready-tile checks over 1200 frames: plain {plain:.1} ms (worst frame {:.3} ms), memoized {memo:.1} ms (worst frame {:.3} ms)",
            worst.0, worst.1
        );
    };
    run(&mut Harness::new(), "vector");
    let (p, t) = terrain_setup(|_| {});
    run(&mut terrain_harness(p, t), "terrain");
}
