//! Dithered LOD fades: the fade math, refine / coarsen swaps, hovering, the safety cap,
//! reversal and the cap on concurrent swaps.

use super::*;

// ---- Dithered LOD fades -------------------------------------------------------------

#[test]
fn fade_math_refine_coarsen_reverse_and_cap() {
    let t0 = Instant::now();
    let gap = fade_gap_m(&layer(), 1); // 90 m (R0 edge 600 m)
    assert!((gap - 90.0).abs() < 1e-9);
    assert!((fade_gap_m(&layer(), 3) - 1050.0).abs() < 1e-9);
    // Refine: approaching by 45 m = half way; hovering keeps it; receding never undoes it.
    let mut f = Fade::new(true, 580.0, t0);
    assert_eq!((f.reported(), f.t), (0.0, 0.0));
    assert!(!f.advance(535.0, gap, t0, fade_cap()));
    assert!((f.t - 0.5).abs() < 1e-12);
    assert!(!f.advance(535.0, gap, t0 + Duration::from_millis(500), fade_cap()));
    assert!(!f.advance(560.0, gap, t0 + Duration::from_millis(600), fade_cap()));
    assert!((f.t - 0.5).abs() < 1e-12, "receding must not undo a refine fade");
    // Reverse at 560 m: coarsen continues from 0.5; 18 m farther = 0.2 down.
    f.retarget(false, 560.0);
    assert!(!f.advance(578.0, gap, t0 + Duration::from_millis(700), fade_cap()));
    assert!((f.t - 0.3).abs() < 1e-12);
    // Done when the target is reached.
    assert!(f.advance(700.0, gap, t0 + Duration::from_millis(800), fade_cap()));
    assert_eq!(f.t, 0.0);
    // Coarsen from scratch: children 1 -> 0 with distance, parent the complement.
    let mut c = Fade::new(false, 700.0, t0);
    assert_eq!(c.reported(), 1.0);
    assert!(!c.advance(745.0, gap, t0, fade_cap()));
    assert!((c.reported() - 0.5).abs() < 1e-6);
    // Safety cap: a frozen fade completes after the cap (fades.cap_s), not before.
    let mut h = Fade::new(true, 580.0, t0);
    assert!(!h.advance(580.0, gap, t0 + fade_cap() - Duration::from_millis(1), fade_cap()));
    assert!(h.advance(580.0, gap, t0 + fade_cap(), fade_cap()));
    assert_eq!(h.t, 0.0, "the cap completes without inventing progress");
    // While active the outgoing side never reads 0; values are exact complements in f32.
    let mut a = Fade::new(true, 600.0, t0);
    a.advance(600.0 - 0.9999 * gap, gap, t0, fade_cap());
    let r = a.reported();
    assert!(r < 1.0 && (1.0 - r) > 0.0 && r + (1.0 - r) == 1.0);
    let mut b = Fade::new(false, 600.0, t0);
    b.advance(600.0 + 0.9999 * gap, gap, t0, fade_cap());
    assert!(b.reported() > 0.0);
}

/// Per-frame view of one swap: parent `p` and its 4 children.
#[derive(Debug, Clone)]
struct Obs {
    p_fade: Option<f32>,
    kid_fades: [Option<f32>; 4],
    p_up: bool,
    kids_up: [bool; 4],
    p_unloaded: bool,
    kids_unloaded: [bool; 4],
    active: Option<bool>, // Some(refine) while Rust has a fade for p
}

fn observe(h: &Harness, p: TileKey, un: &[TileKey]) -> Obs {
    let kids = children(p);
    Obs {
        p_fade: h.fade(p),
        kid_fades: kids.map(|c| h.fade(c)),
        p_up: h.g.uploaded.contains(&p),
        kids_up: kids.map(|c| h.g.uploaded.contains(&c)),
        p_unloaded: un.contains(&p),
        kids_unloaded: kids.map(|c| un.contains(&c)),
        active: h.s.fades().find(|(k, _)| *k == p).map(|(_, f)| f.refine),
    }
}

/// The z15 (R1) tile under the camera at the harness origin.
fn r1_under(h: &Harness, cam: [f64; 3]) -> TileKey {
    let v = h.view(cam);
    let n = (1u64 << 15) as f64;
    TileKey::new(15, (v.wx * n).floor() as u32, (v.wy * n).floor() as u32)
}

/// Fly from `from` to `to` altitude (velocity input 0: the look-ahead stays on the camera,
/// so the slant distance follows the position exactly), then hold until settled.
fn fly(h: &mut Harness, p: TileKey, from: f64, to: f64, step: f64) -> Vec<Obs> {
    let mut obs = Vec::new();
    let mut alt = from;
    for _ in 0..20_000 {
        if (to - alt).abs() > step {
            alt += step * (to - alt).signum();
        } else {
            alt = to;
        }
        let (_, un) = h.frame([0.0, 0.0, alt], [0.0; 3]);
        h.check_frame(&un, "fly");
        obs.push(observe(h, p, &un));
        if alt == to && h.s.settled() {
            return obs;
        }
    }
    panic!("did not settle");
}

/// First and completion frame of the fade of `p` (completion = the frame with finals).
fn fade_span(obs: &[Obs]) -> (usize, usize) {
    let s = obs.iter().position(|o| o.active.is_some()).expect("no fade started");
    let e = s + obs[s..].iter().position(|o| o.active.is_none()).expect("fade never completed");
    (s, e)
}

#[test]
fn refine_fade_keeps_parent_until_complete() {
    let mut h = Harness::new();
    h.settle([0.0, 0.0, 1000.0], 20_000);
    let p = r1_under(&h, [0.0, 0.0, 1000.0]);
    assert!(h.g.visible(p) && children(p).iter().all(|c| !h.g.uploaded.contains(c)));
    // Descend at 180 m/s (6 m per frame) to 300 m: fast enough that the distance-driven
    // fade completes well inside the safety cap (fades.cap_s, 0.75 s by default).
    let obs = fly(&mut h, p, 1000.0, 300.0, 6.0);
    let (s, e) = fade_span(&obs);
    // Start: the frame the 4th child arrives; children 0 (hidden), parent 1.
    assert_eq!(obs[s].active, Some(true));
    assert!(obs[s].kids_up.iter().all(|u| *u) && obs[s].p_up);
    assert!(obs[s - 1].kids_up.iter().any(|u| !*u), "fade starts with the last child");
    assert_eq!(obs[s].p_fade, Some(1.0));
    assert!(obs[s].kid_fades.iter().all(|f| *f == Some(0.0)));
    // Active: parent stays uploaded, values complementary and rising.
    let mut last = 0.0f32;
    for o in &obs[s..e] {
        assert!(o.p_up && !o.p_unloaded, "parent unloaded during its fade");
        let k = o.kid_fades[0].unwrap();
        assert!(o.kid_fades.iter().all(|f| *f == Some(k)));
        assert_eq!(o.p_fade.unwrap() + k, 1.0);
        assert!(k >= last && k < 1.0);
        last = k;
    }
    // Completion: finals (children 1.0, parent 0.0) and the parent's unload, same frame.
    assert_eq!(obs[e].p_fade, Some(0.0));
    assert!(obs[e].kid_fades.iter().all(|f| *f == Some(1.0)));
    assert!(obs[e].p_unloaded && !obs[e].p_up);
    assert!(obs[e + 1].p_fade.is_none() && obs[e + 1].kid_fades.iter().all(|f| f.is_none()));
    // Driven by distance (~90 m gap at 180 m/s = ~0.5 s), so before the cap fires.
    let frames = e - s;
    let cap_frames = (fade_cap().as_secs_f64() / DT).round() as usize;
    assert!((5..cap_frames).contains(&frames), "refine fade took {frames} frames (cap {cap_frames})");
    eprintln!("refine fade of {p}: {frames} frames");
    // Latency `swap` is sampled when the refine fade starts (4 children at 2 per frame: at
    // least one frame after the split), not in the frame the split is decided (0 ms).
    let sw = h.s.latency().summary(Stage::Swap, 1);
    assert!(sw.n >= 1 && sw.p50 >= (DT * 1000.0) as f32 - 0.1, "swap R1 p50 {} ms (n {})", sw.p50, sw.n);
}

#[test]
fn fade_without_anchor_anchors_at_the_first_distance() {
    let t0 = Instant::now();
    let gap = fade_gap_m(&layer(), 1);
    for refine in [true, false] {
        let mut f = Fade::new(refine, f64::NAN, t0);
        let start = f.t;
        assert!(!f.advance(500.0, gap, t0, fade_cap()), "a NaN anchor must not complete the fade");
        assert_eq!(f.t, start);
        let d = if refine { 500.0 - gap / 2.0 } else { 500.0 + gap / 2.0 };
        assert!(!f.advance(d, gap, t0, fade_cap()));
        assert!((f.t - 0.5).abs() < 1e-12);
    }
}

#[test]
fn coarsen_fade_redelivers_parent_then_unloads_children() {
    let mut h = Harness::new();
    h.settle([0.0, 0.0, 300.0], 20_000);
    let p = r1_under(&h, [0.0, 0.0, 300.0]);
    assert!(!h.g.uploaded.contains(&p) && children(p).iter().all(|c| h.g.visible(*c)));
    // Climb at 180 m/s so the distance-driven fade completes inside the safety cap.
    let obs = fly(&mut h, p, 300.0, 1000.0, 6.0);
    let (s, e) = fade_span(&obs);
    assert_eq!(obs[s].active, Some(false));
    // Start: the parent's re-delivery frame; parent 0 (hidden), children 1.
    assert!(obs[s].p_up && !obs[s - 1].p_up, "fade starts with the parent's delivery");
    assert_eq!(obs[s].p_fade, Some(0.0));
    assert!(obs[s].kid_fades.iter().all(|f| *f == Some(1.0)));
    // No child leaves before completion.
    for o in &obs[..e] {
        assert!(o.kids_unloaded.iter().all(|u| !*u), "child unloaded before the fade completed");
    }
    let mut last = 0.0f32;
    for o in &obs[s..e] {
        assert!(o.kids_up.iter().all(|u| *u) && o.p_up);
        let pf = o.p_fade.unwrap();
        assert!(o.kid_fades.iter().all(|f| f.unwrap() + pf == 1.0));
        assert!(pf >= last && pf < 1.0);
        last = pf;
    }
    // Completion: parent 1.0, children 0.0 and unloaded in the same frame.
    assert_eq!(obs[e].p_fade, Some(1.0));
    assert!(obs[e].kid_fades.iter().all(|f| *f == Some(0.0)));
    assert!(obs[e].kids_unloaded.iter().all(|u| *u) && obs[e].p_up);
    assert!(obs[e + 1].p_fade.is_none());
    let frames = e - s;
    let cap_frames = (fade_cap().as_secs_f64() / DT).round() as usize;
    assert!((5..cap_frames).contains(&frames), "coarsen fade took {frames} frames (cap {cap_frames})");
    eprintln!("coarsen fade of {p}: {frames} frames");
}

#[test]
fn hovering_freezes_a_fade_until_the_safety_cap() {
    let mut h = Harness::new();
    h.settle([0.0, 0.0, 1000.0], 20_000);
    let p = r1_under(&h, [0.0, 0.0, 1000.0]);
    // Descend until p's refine fade is roughly a quarter through, then hover.
    let mut alt = 1000.0;
    let mut start = None;
    let mut frame = 0usize;
    let frozen = loop {
        alt -= 3.0;
        let (_, un) = h.frame([0.0, 0.0, alt], [0.0; 3]);
        h.check_frame(&un, "descend");
        frame += 1;
        let o = observe(&h, p, &un);
        if o.active.is_some() && start.is_none() {
            start = Some(frame);
        }
        if let Some(k) = o.kid_fades[0].filter(|_| o.active.is_some()) {
            if k >= 0.2 {
                break k;
            }
        }
        assert!(alt > 300.0, "no fade");
    };
    let start = start.unwrap();
    let mut done = None;
    for i in 0..200 {
        // Jittery velocity input moves the look-ahead point by up to 300 m; the fade is
        // driven by the camera position only, so it must stay frozen anyway.
        let vel = if i % 2 == 0 { [0.0, 0.0, -60.0] } else { [0.0; 3] };
        let (_, un) = h.frame([0.0, 0.0, alt], vel);
        h.check_frame(&un, "hover");
        frame += 1;
        let o = observe(&h, p, &un);
        if o.active.is_some() {
            assert_eq!(o.kid_fades[0], Some(frozen), "hovering must freeze the fade");
            assert!(o.p_up);
        } else {
            // Force-completed in its direction: children 1.0, parent unloaded.
            assert!(o.kid_fades.iter().all(|f| *f == Some(1.0)));
            assert!(o.p_unloaded);
            done = Some(frame);
            break;
        }
    }
    let took = done.expect("cap never fired") - start;
    let cap_frames = (fade_cap().as_secs_f64() / DT).round() as usize;
    assert!((cap_frames..=cap_frames + 1).contains(&took), "cap after {took} frames");
}

#[test]
fn safety_cap_completes_fades_started_while_hovering() {
    // A cold start at 450 m is a first fill: R3 -> R0 with no fade at all (each stand-in
    // swaps out as soon as its 4 children are in).
    let mut h = Harness::new();
    let cam = [0.0, 0.0, 450.0];
    for _ in 0..20_000 {
        let (_, un) = h.frame(cam, [0.0; 3]);
        h.check_frame(&un, "cold start");
        assert_eq!(h.s.fades().count(), 0, "a first fill has no fades");
        if h.s.settled() {
            break;
        }
    }
    assert!(h.s.settled() && h.first_fill_swaps > 0 && h.refine_fades == 0);
    // Settled at 3000 m (leaves at every band), then a jump down to 450 m and hovering:
    // the leaves that now split fade (detail change), each sitting at 0 until the cap.
    let mut h = Harness::new();
    h.settle([0.0, 0.0, 3000.0], 20_000);
    let mut started: AHashMap<TileKey, usize> = AHashMap::new();
    let mut durations = Vec::new();
    for i in 0..20_000 {
        let (_, un) = h.frame(cam, [0.0; 3]);
        h.check_frame(&un, "hover start");
        let active: AHashMap<TileKey, f64> = h.s.fades().map(|(k, f)| (k, f.t)).collect();
        for (k, t) in &active {
            assert_eq!(*t, 0.0, "{k}: a hovering fade must not progress");
            started.entry(*k).or_insert(i);
        }
        started.retain(|k, s| {
            if active.contains_key(k) {
                return true;
            }
            durations.push(i - *s);
            false
        });
        if h.s.settled() {
            break;
        }
    }
    assert!(h.s.settled() && !durations.is_empty());
    let cap_frames = (fade_cap().as_secs_f64() / DT).round() as usize;
    assert!(durations.iter().all(|d| (cap_frames..=cap_frames + 1).contains(d)), "{durations:?}");
    eprintln!("hover start: {} fades, all completed by the cap", durations.len());
}

#[test]
fn reversed_fade_continues_from_its_value() {
    let mut h = Harness::new();
    h.settle([0.0, 0.0, 1000.0], 20_000);
    let p = r1_under(&h, [0.0, 0.0, 1000.0]);
    let mut alt = 1000.0;
    let mut obs = Vec::new();
    // Down until the refine fade is a third through ...
    loop {
        alt -= 3.0;
        let (_, un) = h.frame([0.0, 0.0, alt], [0.0; 3]);
        h.check_frame(&un, "down");
        let o = observe(&h, p, &un);
        let k = o.kid_fades[0].filter(|_| o.active == Some(true));
        obs.push(o);
        if k.is_some_and(|k| k >= 0.33) {
            break;
        }
        assert!(alt > 300.0);
    }
    let at_turn = obs.last().unwrap().kid_fades[0].unwrap();
    // ... then straight back up at 450 m/s (past the 15 % hysteresis within the cap).
    let mut turned = None;
    for _ in 0..2000 {
        alt = (alt + 15.0).min(1000.0);
        let (_, un) = h.frame([0.0, 0.0, alt], [0.0; 3]);
        h.check_frame(&un, "up");
        let o = observe(&h, p, &un);
        if o.active == Some(false) && turned.is_none() {
            turned = Some(o.kid_fades[0].unwrap());
        }
        let end = o.active.is_none() && o.p_fade == Some(1.0);
        obs.push(o);
        if end {
            break;
        }
    }
    let turned = turned.expect("the fade never reversed");
    assert!(turned <= at_turn && turned > 0.0, "reversal jumped: {at_turn} -> {turned}");
    let o = obs.last().unwrap();
    assert!(o.p_up && o.kids_unloaded.iter().all(|u| *u), "coarsened back to the parent");
    assert!(o.kid_fades.iter().all(|f| *f == Some(0.0)));
    // The parent was never unloaded (and so never re-fetched) on the way.
    assert!(obs.iter().all(|o| o.p_up && !o.p_unloaded));
}
/// `fades.max_active`: never more swaps fading at once than the cap; a swap past it completes
/// at once (no fade entry, no hole); 0 turns fades off. The other contract checks hold as usual.
#[test]
fn fade_cap_limits_concurrent_swaps() {
    let mut runs = Vec::new();
    for cap in [0u32, 1, 4, 256] {
        let mut w = crate::profile::WorldProfile::default();
        w.fades.max_active = cap;
        let mut h = Harness::new();
        h.s = Scheduler::new(Frame::new(VILNIUS.0, VILNIUS.1), false, Arc::new(Profile::new(w).unwrap()));
        let dt = 1.0 / 30.0;
        let mut cam = [0.0, 0.0, 300.0];
        h.settle(cam, 20_000);
        let mut camera_holes = 0usize;
        let steps: [([f64; 3], usize, &str); 3] = [
            ([250.0, 0.0, 0.0], (30.0 / dt) as usize, "250 m/s"),
            ([0.0, 0.0, 60.0], (3000.0 / 60.0 / dt) as usize, "climb"),
            ([-250.0, 0.0, -60.0], (3000.0 / 60.0 / dt) as usize, "descent"),
        ];
        for (vel, n, name) in steps {
            for _ in 0..n {
                for a in 0..3 {
                    cam[a] += vel[a] * dt;
                }
                let (_, un) = h.frame(cam, vel);
                h.check_frame(&un, name);
                let v = h.view(cam);
                let under = h.g.tile_at(v.wx, v.wy);
                let fading_pair = under.len() == 2 && under.iter().all(|k| h.g.fades.contains_key(k));
                if under.len() != 1 && !fading_pair {
                    camera_holes += 1;
                }
            }
        }
        h.settle(cam, 20_000);
        assert!(h.g.check().is_empty());
        assert_eq!(camera_holes, 0, "cap {cap}: point under the camera uncovered");
        assert!(h.max_fades_seen <= cap as usize, "cap {cap}: {} fades at once", h.max_fades_seen);
        assert_eq!(h.s.capped_total() as usize, h.capped_seen, "cap {cap}: every capped swap seen");
        match cap {
            0 => assert_eq!(h.refine_fades + h.coarsen_fades, 0, "cap 0: no fades"),
            256 => assert_eq!(h.capped_seen, 0, "cap 256 is never reached here"),
            _ => assert!(h.max_fades_seen == cap as usize && h.capped_seen > 0, "cap {cap} is reached"),
        }
        runs.push((cap, h.refine_fades + h.coarsen_fades, h.capped_seen, h.max_fades_seen));
    }
    for (cap, faded, capped, peak) in runs {
        eprintln!("fade cap {cap}: {faded} swaps faded, {capped} without a fade, peak {peak} at once");
    }
}
