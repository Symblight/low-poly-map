//! Runway geometry and the airport layout: strip, taxiway, apron, buildings, fence.

use super::{
    designator_heading, heading_dir, is_paved, AirportArea, AirportBuilding, AirportGeo, HeightFn, LocalFrame, Obb, RunwayGeo, FT,
    LEVEL_FIELD, LEVEL_PAVEMENT, LEVEL_RUNWAY, SUB_APRON, SUB_FIELD, SUB_RUNWAY_GRASS, SUB_RUNWAY_PAVED, SUB_TAXIWAY,
};
use crate::profile::SeedProfile;
use crate::seed::files::{q7, q_cm, Runway};
use crate::seed::hash::unit;

pub(super) fn runway_geo(r: &Runway, f: &LocalFrame, kind: &str, p: &SeedProfile, height: &HeightFn<'_>) -> RunwayGeo {
    let ap = &p.airport;
    let width_m = r.width_ft.filter(|w| *w > 0.0).map_or(ap.runway_width_m.get(kind), |w| w * FT);
    let (a, b, approximate) = match (r.le_lat, r.le_lon, r.he_lat, r.he_lon) {
        (Some(la), Some(lo), Some(hla), Some(hlo)) if (la, lo) != (hla, hlo) => (f.xy(la, lo), f.xy(hla, hlo), false),
        _ => {
            // Centred on the reference point along the true heading (else the designator).
            let hdg = r.le_heading_deg_t.or_else(|| designator_heading(&r.le)).unwrap_or(0.0);
            let d = heading_dir(hdg);
            let len = r.length_ft.filter(|l| *l > 0.0).map_or(ap.default_length_m, |l| l * FT);
            ([-d[0] * len * 0.5, -d[1] * len * 0.5], [d[0] * len * 0.5, d[1] * len * 0.5], true)
        }
    };
    // Quantize the ends like any input (1e-7 degrees), then work from the quantized points.
    let (la, lo) = f.ll(a);
    let (hla, hlo) = f.ll(b);
    let le_ll = (q7(la), q7(lo));
    let he_ll = (q7(hla), q7(hlo));
    let a = f.xy(le_ll.0, le_ll.1);
    let b = f.xy(he_ll.0, he_ll.1);
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let length_m = (dx * dx + dy * dy).sqrt().max(1.0);
    let dir = [dx / length_m, dy / length_m];
    let le_d = r.le_displaced_threshold_ft.unwrap_or(0.0).max(0.0) * FT;
    let he_d = r.he_displaced_threshold_ft.unwrap_or(0.0).max(0.0) * FT;
    let (le_thr_s, he_thr_s) = if le_d + he_d < length_m - 100.0 { (le_d, length_m - he_d) } else { (0.0, length_m) };
    let thr = |s: f64| {
        let q = [a[0] + dir[0] * s, a[1] + dir[1] * s];
        let (lat, lon) = f.ll(q);
        q_cm(height(lat, lon))
    };
    // Threshold heights from the terrain before any strip, the slope capped at `max_slope`
    // around their mean.
    let (mut le_thr_h, mut he_thr_h) = (thr(le_thr_s), thr(he_thr_s));
    let cap = ap.max_slope * (he_thr_s - le_thr_s);
    if (he_thr_h - le_thr_h).abs() > cap {
        let mean = (le_thr_h + he_thr_h) * 0.5;
        let half = if he_thr_h > le_thr_h { cap * 0.5 } else { -cap * 0.5 };
        le_thr_h = q_cm(mean - half);
        he_thr_h = q_cm(mean + half);
    }
    let mut heading_deg = crate::math::atan2(dir[0], dir[1]).to_degrees();
    if heading_deg < 0.0 {
        heading_deg += 360.0;
    }
    RunwayGeo {
        key: r.key(),
        le: r.le.clone(),
        he: r.he.clone(),
        le_ll,
        he_ll,
        a,
        b,
        dir,
        length_m,
        width_m,
        strip_half_m: ap.strip_half_m.get(kind).max(width_m * 0.5),
        paved: is_paved(&r.surface),
        lighted: r.lighted,
        approximate,
        le_thr_s,
        he_thr_s,
        le_thr_h,
        he_thr_h,
        heading_deg,
        surface: r.surface.clone(),
    }
}

/// Rectangle in the main runway's (s, t) frame -> plane ring (counter-clockwise).
fn rect_st(r: &RunwayGeo, s0: f64, s1: f64, t0: f64, t1: f64) -> Vec<[f64; 2]> {
    let (s0, s1) = (s0.min(s1), s0.max(s1));
    let (t0, t1) = (t0.min(t1), t0.max(t1));
    // (s, t) is right-handed like (x, y) (t to the left of the direction), so this order is CCW.
    vec![r.at(s0, t0), r.at(s1, t0), r.at(s1, t1), r.at(s0, t1)]
}

fn building(g: &AirportGeo, i: u64, kind: &'static str, height_m: f32, levels: f32, ring: Vec<[f64; 2]>) -> AirportBuilding {
    // OSM id space of generated buildings: 2^40 + 40 bits of hash (never a real OSM id).
    let h = crate::seed_hash!(g.seed, "building", i);
    AirportBuilding { id: (1u64 << 40) | (h >> 24), kind, height_m, levels, ring }
}

pub(super) fn layout(g: &mut AirportGeo, cities: &[(f64, f64)], p: &SeedProfile) {
    let ap = &p.airport;
    let kind = g.kind.clone();
    let large = kind == "large_airport";
    let medium = kind == "medium_airport";
    let m = g.runways[g.main].clone();
    let l = m.length_m;
    // Apron side (+1 = left of le -> he): toward the nearest linked city centre, else the seed.
    let side = {
        let near = cities
            .iter()
            .map(|&(lat, lon)| g.frame.xy(lat, lon))
            .min_by(|a, b| (a[0] * a[0] + a[1] * a[1]).total_cmp(&(b[0] * b[0] + b[1] * b[1])));
        let left = [-m.dir[1], m.dir[0]];
        match near {
            Some(c) => {
                let mid = m.at(l * 0.5, 0.0);
                let d = (c[0] - mid[0]) * left[0] + (c[1] - mid[1]) * left[1];
                if d.abs() > 1.0 {
                    d.signum()
                } else if g.seed.0 & 1 == 0 {
                    1.0
                } else {
                    -1.0
                }
            }
            None if g.seed.0 & 1 == 0 => 1.0,
            None => -1.0,
        }
    };
    let rw_half = m.width_m * 0.5;
    // Runways.
    for r in &g.runways {
        let sub = if r.paved { SUB_RUNWAY_PAVED } else { SUB_RUNWAY_GRASS };
        g.areas.push(AirportArea { sub, level: LEVEL_RUNWAY, ring: rect_st(r, 0.0, r.length_m, -r.width_m * 0.5, r.width_m * 0.5) });
    }
    // Parallel taxiway with connectors (main runway only).
    let taxi = ap.taxi_offset_m.get(&kind);
    let tw = ap.taxiway_width_m.get(&kind);
    let mut far_t = rw_half; // outer edge (|t|) of the pavement so far on the apron side
    if taxi > rw_half + tw && m.paved && (large || medium) {
        let t = side * taxi;
        let (s0, s1) = (30.0f64.min(l * 0.1), l - 30.0f64.min(l * 0.1));
        g.areas.push(AirportArea { sub: SUB_TAXIWAY, level: LEVEL_PAVEMENT, ring: rect_st(&m, s0, s1, t - tw * 0.5, t + tw * 0.5) });
        let n = ((l / 1000.0).round() as usize).max(1);
        for k in 0..=n {
            let s = s0 + tw * 0.5 + (s1 - s0 - tw) * (k as f64 / n as f64);
            g.areas.push(AirportArea {
                sub: SUB_TAXIWAY,
                level: LEVEL_PAVEMENT,
                ring: rect_st(&m, s - tw * 0.5, s + tw * 0.5, side * rw_half, side * (taxi - tw * 0.5)),
            });
        }
        far_t = taxi + tw * 0.5;
    }
    // Apron, terminal, hangars, tower: centred on the runway middle, on the apron side.
    let (apron_len, apron_depth, term_len, term_depth, term_h, term_levels, hangar, hangar_h, tower_h) = if large {
        (0.35 * l, 200.0, 0.22 * l, 60.0, 20.0, 3.0, [70.0, 60.0], 18.0, 45.0)
    } else if medium {
        (0.25 * l, 140.0, 0.14 * l, 40.0, 12.0, 2.0, [50.0, 40.0], 14.0, 30.0)
    } else {
        (60.0f64.min(0.3 * l), 40.0, 20.0f64.min(0.15 * l), 12.0, 5.0, 1.0, [25.0, 20.0], 7.0, 0.0)
    };
    let mid = l * 0.5;
    let apron_t0 = if far_t > rw_half { far_t } else { m.strip_half_m.min(rw_half + 60.0) };
    if far_t <= rw_half {
        // Small field: a connector from the runway edge to the apron.
        let w = tw.max(6.0);
        g.areas.push(AirportArea { sub: SUB_TAXIWAY, level: LEVEL_PAVEMENT, ring: rect_st(&m, mid - w * 0.5, mid + w * 0.5, side * rw_half, side * apron_t0) });
    }
    let apron_t1 = apron_t0 + apron_depth;
    g.areas.push(AirportArea {
        sub: SUB_APRON,
        level: LEVEL_PAVEMENT,
        ring: rect_st(&m, mid - apron_len * 0.5, mid + apron_len * 0.5, side * apron_t0, side * apron_t1),
    });
    // Terminal behind the apron; car park (apron pavement) behind it.
    let gap = 10.0;
    let tt0 = apron_t1 + gap;
    let tt1 = tt0 + term_depth;
    let mut bi = 0u64;
    let b = building(g, bi, "transportation", term_h, term_levels, rect_st(&m, mid - term_len * 0.5, mid + term_len * 0.5, side * tt0, side * tt1));
    g.buildings.push(b);
    bi += 1;
    if large || medium {
        g.areas.push(AirportArea {
            sub: SUB_APRON,
            level: LEVEL_PAVEMENT,
            ring: rect_st(&m, mid - term_len * 0.4, mid + term_len * 0.4, side * (tt1 + gap), side * (tt1 + gap + 80.0)),
        });
    }
    // Hangars along the apron edge beyond the terminal's far end (count from the airport seed).
    let n_hangars = if large { 2 + (unit(crate::seed_hash!(g.seed, "hangars")) * 3.0) as usize } else if medium { 1 + (unit(crate::seed_hash!(g.seed, "hangars")) * 3.0) as usize } else { 1 + (unit(crate::seed_hash!(g.seed, "hangars")) * 2.0) as usize };
    let mut s = mid + apron_len * 0.5 + 20.0;
    for _ in 0..n_hangars {
        if s + hangar[0] > l {
            break;
        }
        let r = rect_st(&m, s, s + hangar[0], side * apron_t0, side * (apron_t0 + hangar[1]));
        let b = building(g, bi, "hangar", hangar_h, 1.0, r);
        g.buildings.push(b);
        bi += 1;
        s += hangar[0] + 15.0;
    }
    // Control tower at the apron's near end.
    if tower_h > 0.0 {
        let s0 = mid - apron_len * 0.5 - 30.0;
        let r = rect_st(&m, s0 - 12.0, s0, side * (apron_t0 + 10.0), side * (apron_t0 + 22.0));
        let b = building(g, bi, "office", tower_h, (tower_h / 3.0).round(), r);
        g.buildings.push(b);
    }
    // Fence: around every strip (+ strip_end_m), pavement and building, in the main runway's frame.
    let u = m.dir;
    let v = [-u[1], u[0]];
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    let mut add = |q: [f64; 2]| {
        let d = [q[0] - m.a[0], q[1] - m.a[1]];
        let (su, sv) = (d[0] * u[0] + d[1] * u[1], d[0] * v[0] + d[1] * v[1]);
        lo = [lo[0].min(su), lo[1].min(sv)];
        hi = [hi[0].max(su), hi[1].max(sv)];
    };
    for r in &g.runways {
        for q in rect_st(r, -ap.strip_end_m, r.length_m + ap.strip_end_m, -r.strip_half_m, r.strip_half_m) {
            add(q);
        }
    }
    for q in g.areas.iter().flat_map(|a| a.ring.iter()).chain(g.buildings.iter().flat_map(|b| b.ring.iter())) {
        add(*q);
    }
    let margin = ap.fence_margin_m;
    let (cu, cv) = ((lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5);
    g.fence = Obb {
        c: [m.a[0] + u[0] * cu + v[0] * cv, m.a[1] + u[1] * cu + v[1] * cv],
        u,
        half: [(hi[0] - lo[0]) * 0.5 + margin, (hi[1] - lo[1]) * 0.5 + margin],
    };
    g.areas.insert(0, AirportArea { sub: SUB_FIELD, level: LEVEL_FIELD, ring: g.fence.ring() });
}
