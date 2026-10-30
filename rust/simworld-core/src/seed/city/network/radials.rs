//! Radials and airport access roads (`docs/spec-world-seed.md` section 5.3 step 5).
//!
//! `n = clamp(round(radius_km / 1.5), 3, 8)` **primary** roads start at the centre cell, evenly
//! spaced with a hash jitter, and advance in `radial_step_m` steps with a heading random walk of
//! `± radial_turn_deg` (`H(city, "radial", i, step)`). A step into water or a fence is steered
//! (up to ±60° either way, and never more than 60° from the radial's start bearing); when no heading is free the radial crosses once in a straight line
//! (a bridge) if dry land lies within `bridge_max_m`, else it ends. A radial ends where it leaves
//! the footprint (the plain hexagons of the kept cells), on the bisected edge point. For each
//! linked airport the radial whose end is nearest the fence gate (the fence point nearest the
//! city centre) continues toward the gate, steered around water, for at most `access_max_km`.
//! Inter-city roads claiming radials come with phase S2b.

use std::f64::consts::PI;

use super::{road, MajorNetwork};
use crate::seed::city::context::CityContext;
use crate::seed::city::lines::{len, lerp};
use crate::seed::city::RoadKind;
use crate::math::{atan2, sin_cos};
use crate::seed::files::City;
use crate::seed::hash::unit;
use crate::seed::water::WaterHit;
use crate::seed::world::SeedWorld;

/// A radial never turns more than this from its start bearing (radians), so steering around
/// water cannot turn it back toward the centre.
const MAX_BEND: f64 = PI / 3.0;

/// Heading offsets tried when the next step is blocked (radians, in order).
fn offsets() -> [f64; 13] {
    let d = PI / 18.0;
    [0.0, d, -d, 2.0 * d, -2.0 * d, 3.0 * d, -3.0 * d, 4.0 * d, -4.0 * d, 5.0 * d, -5.0 * d, 6.0 * d, -6.0 * d]
}

#[inline]
fn step(p: [f64; 2], h: f64, s: f64) -> [f64; 2] {
    let (sn, cs) = sin_cos(h);
    [p[0] + cs * s, p[1] + sn * s]
}

pub(super) fn build(net: &mut MajorNetwork, ctx: &CityContext, city: &City, w: &SeedWorld) {
    let cp = &w.params.city;
    let n = ((city.radius_km / 1.5).round() as i64).clamp(3, 8) as usize;
    let frame = ctx.frame;
    let blocked = |p: [f64; 2]| {
        let (lat, lon) = frame.ll(p);
        w.water.water_at(lat, lon) != WaterHit::Land || w.terrain.strips.in_fence(lat, lon)
    };
    let wet = |p: [f64; 2]| {
        let (lat, lon) = frame.ll(p);
        w.water.water_at(lat, lon) != WaterHit::Land
    };
    let inside = |p: [f64; 2]| ctx.cell_at(p).is_some();
    let s = cp.radial_step_m;
    let turn = cp.radial_turn_deg.to_radians();
    let max_steps = ((city.radius_km * 2000.0) / s).ceil() as usize + 4;
    let mut paths: Vec<Vec<[f64; 2]>> = Vec::with_capacity(n);
    for i in 0..n {
        let jitter = (unit(crate::seed_hash!(ctx.seed, "radial", i)) - 0.5) * (2.0 * PI / n as f64) * 0.5;
        let h0 = 2.0 * PI * i as f64 / n as f64 + jitter;
        let mut h = h0;
        let mut cur = step([0.0, 0.0], h, 0.5 * ctx.grid.cell_m);
        let mut pts = vec![cur];
        let mut bridged = false;
        'walk: for k in 0..max_steps {
            h += (unit(crate::seed_hash!(ctx.seed, "radial", i, k)) * 2.0 - 1.0) * turn;
            h = h.clamp(h0 - MAX_BEND, h0 + MAX_BEND);
            for off in offsets().into_iter().filter(|o| (h + o - h0).abs() <= MAX_BEND) {
                let p = step(cur, h + off, s);
                if !inside(p) {
                    pts.push(edge_point(cur, p, &inside));
                    break 'walk;
                }
                if !blocked(p) {
                    h += off;
                    cur = p;
                    pts.push(p);
                    continue 'walk;
                }
            }
            // Every heading blocked: one straight crossing (a bridge) if dry land is near.
            if bridged {
                break;
            }
            let mut d = s;
            while d <= cp.bridge_max_m + s {
                let q = step(cur, h, d);
                if !inside(q) || !blocked(q) {
                    break;
                }
                d += 20.0;
            }
            let q = step(cur, h, d);
            if d > cp.bridge_max_m + s || !inside(q) || wet(q) {
                break;
            }
            bridged = true;
            cur = q;
            pts.push(q);
        }
        paths.push(pts);
    }
    // Access roads to the linked airports.
    for ident in &city.airports {
        let Some(g) = w.airport(ident) else { continue };
        let c = g.frame.xy(city.center.lat, city.center.lon);
        let (u, v) = (g.fence.u, g.fence.v());
        let d = [c[0] - g.fence.c[0], c[1] - g.fence.c[1]];
        let a = (d[0] * u[0] + d[1] * u[1]).clamp(-g.fence.half[0], g.fence.half[0]);
        let b = (d[0] * v[0] + d[1] * v[1]).clamp(-g.fence.half[1], g.fence.half[1]);
        let gate_a = [g.fence.c[0] + u[0] * a + v[0] * b, g.fence.c[1] + u[1] * a + v[1] * b];
        let (glat, glon) = g.frame.ll(gate_a);
        let gate = frame.xy(glat, glon);
        let Some(best) = (0..paths.len()).min_by(|&i, &j| {
            let (ei, ej) = (paths[i][paths[i].len() - 1], paths[j][paths[j].len() - 1]);
            len(ei, gate).total_cmp(&len(ej, gate)).then(i.cmp(&j))
        }) else {
            continue;
        };
        let path = &mut paths[best];
        let mut cur = path[path.len() - 1];
        let steps = (cp.access_max_km * 1000.0 / s).ceil() as usize;
        let mut reached = len(cur, gate) <= s;
        for _ in 0..steps {
            if reached {
                break;
            }
            let h = atan2(gate[1] - cur[1], gate[0] - cur[0]);
            let next = offsets().iter().map(|o| step(cur, h + o, s)).find(|p| !wet(*p));
            let Some(p) = next else { break };
            cur = p;
            path.push(p);
            reached = len(cur, gate) <= s;
        }
        if reached {
            path.push(gate);
            net.airport_access += 1;
        }
    }
    net.radials = n as u32;
    for (i, pts) in paths.into_iter().enumerate() {
        if pts.len() >= 2 {
            net.roads.push(road(crate::seed_hash!(ctx.seed, "radial-id", i) & 0xFFFF_FFFF, RoadKind::Radial, pts));
        }
    }
}

/// The footprint edge between `a` (inside) and `b` (outside), by bisection.
fn edge_point(a: [f64; 2], b: [f64; 2], inside: &dyn Fn([f64; 2]) -> bool) -> [f64; 2] {
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..16 {
        let m = (lo + hi) * 0.5;
        if inside(lerp(a, b, m)) {
            lo = m;
        } else {
            hi = m;
        }
    }
    lerp(a, b, lo)
}
