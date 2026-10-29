//! Airport lights: runway edge, threshold, end and approach lights, taxiway edge lights.

use super::{AirportGeo, AirportLight, LEVEL_APPROACH, LEVEL_PAVEMENT, LEVEL_RUNWAY, SUB_TAXIWAY};
use crate::profile::SeedProfile;

fn rgba(c: [f64; 3], a: f64) -> [f32; 4] {
    [c[0] as f32, c[1] as f32, c[2] as f32, a as f32]
}

pub(super) fn lights(g: &mut AirportGeo, p: &SeedProfile) {
    let lp = &p.airport.lights;
    let a = lp.intensity;
    let (edge, thr, end, appr, taxi) =
        (rgba(lp.edge_color, a), rgba(lp.threshold_color, a), rgba(lp.end_color, a), rgba(lp.approach_color, a), rgba(lp.taxiway_color, a));
    let mut out = Vec::new();
    for (ri, r) in g.runways.iter().enumerate().filter(|(_, r)| r.lighted) {
        let half = r.width_m * 0.5 + 1.5;
        let l = r.length_m;
        // Edge lights: both edges, end to end, evenly spaced (both ends included).
        let n = ((l / lp.edge_spacing_m.max(1.0)).round() as usize).max(1);
        for k in 0..=n {
            let s = l * (k as f64 / n as f64);
            for t in [-half, half] {
                out.push(AirportLight { p: r.at(s, t), color: edge, level: LEVEL_RUNWAY, runway: ri });
            }
        }
        // Threshold (green, across at each landing threshold) and end lights (red, 3 m past each
        // physical end), every 3 m across the runway.
        let across = ((r.width_m / 3.0).floor() as usize).max(2);
        for k in 0..=across {
            let t = -r.width_m * 0.5 + r.width_m * (k as f64 / across as f64);
            out.push(AirportLight { p: r.at(r.le_thr_s, t), color: thr, level: LEVEL_RUNWAY, runway: ri });
            out.push(AirportLight { p: r.at(r.he_thr_s, t), color: thr, level: LEVEL_RUNWAY, runway: ri });
            out.push(AirportLight { p: r.at(-3.0, t), color: end, level: LEVEL_RUNWAY, runway: ri });
            out.push(AirportLight { p: r.at(l + 3.0, t), color: end, level: LEVEL_RUNWAY, runway: ri });
        }
        // Approach rows before both thresholds (centreline, outward).
        if r.paved && l >= lp.approach_min_runway_m && lp.approach_length_m > 0.0 {
            let n = (lp.approach_length_m / lp.approach_spacing_m.max(1.0)).floor() as usize;
            for k in 1..=n {
                let d = k as f64 * lp.approach_spacing_m;
                out.push(AirportLight { p: r.at(r.le_thr_s - d, 0.0), color: appr, level: LEVEL_APPROACH, runway: ri });
                out.push(AirportLight { p: r.at(r.he_thr_s + d, 0.0), color: appr, level: LEVEL_APPROACH, runway: ri });
            }
        }
    }
    // Taxiway edge lights along the parallel taxiway (blue), medium / large airports.
    let any_lit = g.runways.iter().any(|r| r.lighted);
    if any_lit && (g.kind == "large_airport" || g.kind == "medium_airport") {
        for area in g.areas.iter().filter(|a| a.sub == SUB_TAXIWAY) {
            // Long sides of the rectangle only.
            let r = &area.ring;
            let len = |i: usize| {
                let (a, b) = (r[i], r[(i + 1) % 4]);
                ((b[0] - a[0]) * (b[0] - a[0]) + (b[1] - a[1]) * (b[1] - a[1])).sqrt()
            };
            for i in 0..4 {
                if len(i) < len((i + 1) % 4) {
                    continue;
                }
                let (pa, pb) = (r[i], r[(i + 1) % 4]);
                let n = ((len(i) / p.airport.lights.taxiway_spacing_m.max(1.0)).round() as usize).max(1);
                for k in 0..=n {
                    let t = k as f64 / n as f64;
                    out.push(AirportLight {
                        p: [pa[0] + (pb[0] - pa[0]) * t, pa[1] + (pb[1] - pa[1]) * t],
                        color: taxi,
                        level: LEVEL_PAVEMENT,
                        runway: usize::MAX,
                    });
                }
            }
        }
    }
    g.lights = out;
}
