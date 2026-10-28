//! The reused terrain plan equals a fresh plan (`plan_memo`), on the scheduler test harness of
//! [`super::tests`].

use super::tests::{seed_terrain_harness, terrain_harness, terrain_setup, Harness};

/// Fly `h` through level flight north at 250 m/s, a climb to 3000 m, a turn and a hover, with a
/// detail and a fov change on the way; every reused plan is checked against a fresh one
/// (`verify_plan_reuse` asserts equality inside the scheduler) and the host rule every frame.
/// Returns (reused, planned).
fn fly(h: &mut Harness) -> (u64, u64) {
    h.s.verify_plan_reuse = true;
    h.settle([0.0, 0.0, 300.0], 20_000);
    let dt = 1.0 / 30.0;
    let (mut x, mut y, mut alt) = (0.0f64, 0.0f64, 300.0f64);
    for f in 0..1800 {
        let vel = match f {
            0..=599 => [0.0, 250.0, 0.0],
            600..=899 => [0.0, 150.0, 9.0],
            900..=1199 => [200.0, 50.0, 0.0],
            1200..=1499 => [0.0; 3],
            _ => [-120.0, -120.0, -9.0],
        };
        if f == 1000 {
            h.s.set_detail(2.0);
        }
        if f == 1300 {
            h.s.set_lod_view(&crate::rings::LodView::new(30.0, 1080.0, [0.0, 0.0, -1.0]).unwrap(), 1.0);
        }
        x += vel[0] * dt;
        y += vel[1] * dt;
        alt = (alt + vel[2] * dt).clamp(100.0, 3000.0);
        let (_, un) = h.frame([x, y, alt], vel);
        h.check_frame(&un, "plan reuse flight");
    }
    h.s.plan_reuse()
}

#[test]
fn plan_reuse_equals_a_fresh_plan() {
    let (p, t) = terrain_setup(|_| {});
    let mut h = terrain_harness(p, t);
    let (reused, planned) = fly(&mut h);
    eprintln!("terrain plan: {reused} reused, {planned} computed");
    assert!(reused > 0, "the plan was never reused");
    // The seed world: coast, lakes, airports (no flat zone).
    let mut h = seed_terrain_harness(55.85, 21.06, |_| {});
    let (reused, planned) = fly(&mut h);
    eprintln!("seed world plan: {reused} reused, {planned} computed");
    assert!(reused > 0, "the plan was never reused");
}
